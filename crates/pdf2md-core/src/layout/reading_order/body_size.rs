// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Reading order recovery, multi-column stream separation, and structured DocBlock generation.

use super::*;

/// Robustly estimate the body (regular prose) font size for a page.
///
/// The median is a poor anchor for heading detection: when a page carries many
/// mid-size headings (outlines, structured reports), the heading sizes pull the
/// median up to the heading size, so `size >= body * 1.25` no longer fires and
/// the headings hide themselves. Instead take the most frequent size class —
/// the mode — and, when several classes tie for frequency, the *smallest* of
/// them. Body text is the smallest regular size class and headings are larger,
/// so this keeps the heading threshold anchored on the body and never lets the
/// headings inflate it.
///
/// A nominal size never arrives as one exact number: glyph advances and the
/// producer's own text-matrix rounding make a single 10pt body emit as
/// 9.8/9.9/10.0/10.1/10.2. Quantizing to a tenth of a point (the previous
/// approach) left those in separate bins, so on a page whose small
/// table/caption text is perfectly uniform (every 7pt cell line identical) that
/// one bin could out-vote the spread-out prose and drag `body_size` down to the
/// small text's size. Every real body line then measured `>= 1.3x body` and was
/// mistaken for a heading. Cluster near-identical sizes first, then vote, so the
/// body and the genuinely smaller table text stay separate classes but the
/// body's own metric jitter does not split its vote.
pub(super) fn estimate_body_size(sizes: &[f64]) -> f64 {
    if sizes.is_empty() {
        return 10.0;
    }
    let mut sorted: Vec<f64> = sizes
        .iter()
        .copied()
        .filter(|s| s.is_finite() && *s > 0.0)
        .collect();
    if sorted.is_empty() {
        return 10.0;
    }
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    // Greedy 1-D clustering: a size joins the current cluster while it is within
    // `CLUSTER_TOL` of that cluster's anchor (its smallest member), so jitter
    // chains cannot drift the anchor upward past a genuinely distinct size. 5%
    // keeps a 7pt table separate from a 10pt body (30% apart) while merging the
    // ~2-4% jitter one nominal size carries.
    const CLUSTER_TOL: f64 = 0.05;
    let mut clusters: Vec<(f64, usize)> = Vec::new();
    for &s in &sorted {
        match clusters.last_mut() {
            Some((anchor, count)) if s <= *anchor * (1.0 + CLUSTER_TOL) => *count += 1,
            _ => clusters.push((s, 1)),
        }
    }

    // Body is the size class with the most lines. Clusters are ascending, so
    // keeping the first on an equal count preserves the old smallest-on-tie rule.
    let (mut best, mut best_count) = clusters[0];
    for &(anchor, count) in &clusters[1..] {
        if count > best_count {
            best = anchor;
            best_count = count;
        }
    }
    best.max(1.0)
}

/// Computes the body (regular prose) font size for a full page's lines — the
/// same anchor `build_doc_blocks` already uses to classify a line as a
/// title/heading, now also the anchor the render functions below use to
/// decide when to emit `#`/`##`/`###` instead of flat text.
pub(crate) fn body_size_for(lines: &[Vec<Span>]) -> f64 {
    let sizes: Vec<f64> = lines
        .iter()
        .map(|l| l.iter().map(|s| s.size).fold(0.0f64, f64::max))
        .filter(|s| *s > 0.0)
        .collect();
    estimate_body_size(&sizes).max(1.0)
}

// ---------------------------------------------------------------------------
// Structural Markdown emission (R7): headings (#, ##, ###) and lists (-, 1.)
// ---------------------------------------------------------------------------
//
// `build_doc_blocks` above already classifies each line into "title" /
// "heading" / "list" / "body" for the JSON `blocks` output, but nothing
// consulted that classification when building the actual Markdown string —
// every renderer in this module emitted flat text with only inline
// `**bold**`/`*italic*` emphasis, regardless of a line's role. A heading
// looked exactly like a paragraph that happened to be bold.
//
// This section is the shared classifier + formatter every render entry point
// (`render_cluster`, `render_human_order`, `render_math_stream` in
// latex_math.rs, `render_with_tables` in tables/mod.rs) now calls per line,
// so the emitted Markdown and the `blocks` JSON list are never a "heading"
// according to one and flat text according to the other.
//
// The heading-level thresholds mirror `layout::semantic::detect_heading`
// (a statistical 3-level H1/H2/H3 classifier that already existed, already
// tested, but lived only in the separate `ModernLayoutEngine`/`xy_cut`
// pipeline `convert_pdf_bytes_to_markdown` never calls) adapted onto the
// `Span`-based lines this live pipeline actually uses, anchored on
// `body_size_for`'s mode-based estimate rather than semantic.rs's median
// (see `estimate_body_size`'s own doc comment for why the mode is the safer
// anchor). List-item detection similarly mirrors
// `layout::semantic::detect_list_item`'s marker checks (bullets, checkboxes,
// ordered markers), adapted to slice the marker off a `Span` line instead of
// a `TextLine`'s first `TextWord`.
