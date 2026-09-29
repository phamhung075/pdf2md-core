// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Reading order recovery, multi-column stream separation, and structured DocBlock generation.

use super::*;

/// Whether a span carries no visible ink (empty or whitespace-only).
pub(super) fn is_blank_span(s: &Span) -> bool {
    s.text.chars().all(|c| c == ' ')
}

/// Median gutter x (midpoint between left/right content) of the `Split`
/// rows accumulated so far in an in-progress column run, or `None` before
/// the run has any confirmed split.
pub(super) fn run_median_gutter(gutters: &[f64]) -> Option<f64> {
    if gutters.is_empty() {
        return None;
    }
    let mut gs = gutters.to_vec();
    gs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = gs.len();
    // True median: with an even count the two central gutters are averaged.
    // Picking the upper one skews the estimate high on the first two rows,
    // which then mis-classifies a short facing line that starts just left of
    // the inflated midpoint as neither side and breaks the column run.
    Some(if n % 2 == 1 {
        gs[n / 2]
    } else {
        0.5 * (gs[n / 2 - 1] + gs[n / 2])
    })
}

/// Average number of non-empty spans per row (0 for an empty slice). A cheap
/// "is this a text column rather than a grid of short cells" signal.
pub(super) fn avg_words_per_row(rows: &[Vec<Span>]) -> f64 {
    if rows.is_empty() {
        return 0.0;
    }
    rows.iter()
        .map(|v| v.iter().filter(|sp| !sp.text.trim().is_empty()).count() as f64)
        .sum::<f64>()
        / rows.len() as f64
}

/// Average number of *whitespace-separated words* per row (0 for an empty
/// slice).
///
/// [`avg_words_per_row`] counts non-empty spans, a good "single column versus
/// grid of short cells" signal when a producer emits one span per word. It is
/// the wrong signal when a producer draws each visual line as a single `Tj`
/// run: a genuine prose column then averages 1.0 span per row and falls below
/// every prose gate (verified on the Air France e-ticket's bilingual
/// FR/EN blocks, whose every line is one run). The projection recovery uses
/// this word count instead, so a column's wordiness is measured independently
/// of how the producer grouped its runs.
pub(super) fn words_per_row(rows: &[Vec<Span>]) -> f64 {
    if rows.is_empty() {
        return 0.0;
    }
    rows.iter()
        .map(|v| {
            v.iter()
                .map(|sp| sp.text.split_whitespace().count())
                .sum::<usize>() as f64
        })
        .sum::<f64>()
        / rows.len() as f64
}

/// Whether every row's internal span gaps stay below a column-gutter width —
/// i.e. the rows form a single column, not a multi-column table grid.
pub(super) fn rows_are_clean(rows: &[Vec<Span>]) -> bool {
    rows.iter().all(|v| {
        let size = v.iter().map(|x| x.size).fold(0.0f64, f64::max).max(0.1);
        v.windows(2).all(|p| {
            let a_end = p[0].x + p[0].advance;
            let gap = (p[1].x - a_end).max(0.0);
            gap <= 1.2 * size
        })
    })
}

/// Spread between the leftmost x of the rows' first spans — how ragged the
/// side's *starting edge* is.
///
/// A genuine column is left-aligned: every row starts at the column's margin,
/// so the spread is a few points. When the projection cuts a single full-width
/// paragraph at a recurring vertical gap, the "right column" is instead the
/// line continuations after that gap, and their start x jumps around with the
/// sentence (verified on `enedis_hp_hc`, whose false regions spread 100-360pt
/// against the bilingual e-ticket's 22pt). A bounded spread keeps the
/// projection from transposing fragments of one paragraph.
pub(super) fn column_start_spread(rows: &[Vec<Span>]) -> f64 {
    let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
    for r in rows {
        let start = r.iter().map(|s| s.x).fold(f64::INFINITY, f64::min);
        if start.is_finite() {
            lo = lo.min(start);
            hi = hi.max(start);
        }
    }
    if lo.is_finite() {
        hi - lo
    } else {
        0.0
    }
}

/// Whether `rows` is a *wrapped* text column: some consecutive pair where the
/// earlier row neither ends a sentence nor is a closed item and the later row
/// plainly continues it (starts lowercase), or the earlier row ends in a
/// line-break hyphen. A run of table cells never continues one row into the
/// next, so this is what separates real body text sitting beside a grid from an
/// ordinary label/value or multi-column table.
pub(super) fn wrapped_prose(rows: &[Vec<Span>]) -> bool {
    let plain: Vec<String> = rows
        .iter()
        .map(|r| r.iter().map(|s| s.text.as_str()).collect::<String>())
        .collect();
    plain.windows(2).any(|w| {
        let a = w[0].trim_end();
        let b = w[1].trim_start();
        let hyphen_break =
            a.ends_with('-') && a.chars().rev().nth(1).map_or(false, |c| c.is_alphabetic());
        let sentence_end = a.ends_with(['.', '!', '?', ':', ';', ')', ']', '€', '%']);
        let lower_next = b.chars().next().map_or(false, |c| c.is_lowercase());
        hyphen_break || (!sentence_end && lower_next)
    })
}

/// A flowing prose column facing a multi-column grid (or its mirror). The two
/// halves are independent regions even though one of them is not a single clean
/// column, so the column band must be kept rather than re-merged row by row —
/// which weaves the grid's cells into the prose and, when the grid uses smaller
/// type, renders them as fake LaTeX super/subscripts. Both halves unclean means
/// a single wide grid was cut in two, not a prose/table split.
pub(super) fn prose_beside_grid(left: &[Vec<Span>], right: &[Vec<Span>]) -> bool {
    let (left_clean, right_clean) = (rows_are_clean(left), rows_are_clean(right));
    let (lw, rw) = (avg_words_per_row(left), avg_words_per_row(right));
    (left_clean && !right_clean && lw >= 4.0 && wrapped_prose(left) && lw > rw)
        || (right_clean && !left_clean && rw >= 4.0 && wrapped_prose(right) && rw > lw)
}

/// Whether some internal gutter recurs at the same x across at least two of
/// `rows` — i.e. the side is itself a table grid (one or more aligned columns)
/// rather than a single column that merely happens to contain one wide gap.
/// The projection fallback uses this to tell a genuine grid from an
/// accidental short alignment inside running prose.
pub(super) fn has_aligned_internal_gutter(rows: &[Vec<Span>]) -> bool {
    let mut gutters: Vec<f64> = Vec::new();
    for v in rows {
        let size = v.iter().map(|s| s.size).fold(0.0f64, f64::max).max(0.1);
        for p in v.windows(2) {
            let gap = p[1].x - (p[0].x + p[0].advance);
            if gap >= 0.8 * size {
                gutters.push(0.5 * (p[0].x + p[0].advance + p[1].x));
            }
        }
    }
    gutters
        .iter()
        .any(|a| gutters.iter().filter(|b| (**b - *a).abs() <= 6.0).count() >= 2)
}
