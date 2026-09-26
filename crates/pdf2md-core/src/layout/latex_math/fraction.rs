// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! LaTeX math AST synthesis built directly from 2D glyph geometry.
//!
//! Modern PDF producers typeset math as positioned glyph runs rather than as a
//! semantic math tree: a built-up fraction is a smaller numerator run stacked
//! above a thin horizontal rule (the fraction bar) with a smaller denominator
//! run below it, and a simple super/subscript is a smaller run whose baseline
//! is raised/lowered relative to its base. Before the geometry engine exports
//! the page as Markdown it can now lift these visual patterns into a real
//! [`LatexExpr`] AST and serialize them as semantically correct LaTeX
//! (`$\frac{a}{b}$`, `$x^{2}$`, `$a_{i}$`) instead of emitting them as a flat
//! sequence of glyphs.
//!
//! The detector is intentionally geometric and conservative, and only runs when
//! the caller opts in via `detect_math` so the default extraction path stays
//! byte-identical.

use super::*;

pub(super) fn line_bbox(line: &[Span]) -> (f64, f64, f64, f64) {
    let x0 = line.iter().map(|s| s.x).fold(f64::INFINITY, f64::min);
    let x1 = line
        .iter()
        .map(|s| s.x + s.advance)
        .fold(f64::NEG_INFINITY, f64::max);
    let y = line.first().map(|s| s.y).unwrap_or(0.0);
    let size = line
        .iter()
        .map(|s| s.size)
        .fold(f64::NEG_INFINITY, f64::max)
        .max(0.1);
    (x0, y - 0.5 * size, x1, y + 0.5 * size)
}

pub(super) fn overlap(a0: f64, a1: f64, b0: f64, b1: f64) -> f64 {
    (a1.min(b1) - a0.max(b0)).max(0.0)
}

/// Check whether a thin horizontal rule between two stacked runs looks like a
/// fraction bar rather than a paragraph rule / table border / underline.
pub(super) fn is_fraction_bar(line: RuleSeg, num_line: &[Span], den_line: &[Span]) -> bool {
    let (bar_y, bx0, bx1) = line;
    let bar_w = (bx1 - bx0).abs();
    let (nx0, _, nx1, _) = line_bbox(num_line);
    let (dx0, _, dx1, _) = line_bbox(den_line);
    let num_size = num_line
        .iter()
        .map(|s| s.size)
        .fold(f64::NEG_INFINITY, f64::max)
        .max(0.1);

    // A fraction bar must be a short rule (not a full-width divider). The
    // comment above always said so, but nothing actually checked it — only
    // `bar_w <= 0.0` was rejected, so any rule of any length overlapping
    // >= 50% of the (possibly much narrower) numerator/denominator passed.
    // Measured against a real false-positive case (a decorative rule under
    // an invoice's "NOUS CONTACTER" heading, separating it from an unrelated
    // "N° client" line below — bar/content width ratio 1.68), bar-width
    // proportionality turned out not to discriminate at all: real
    // false-positive ratios there ranged 0.90-3.32, which fully overlaps a
    // legitimate single-digit fraction's ratio (this file's own
    // `detects_built_up_fraction_across_lines` test is 4.0). Width is kept
    // only as a generous backstop against a truly pathological case (a
    // page-spanning rule); the actual fix is the numerator/denominator
    // length cap below.
    let content_w = (nx1 - nx0).max(dx1 - dx0).max(0.1);
    if bar_w <= 0.0 || bar_w > 6.0 * content_w {
        return false;
    }
    // A real fraction's numerator/denominator is short (digits, a variable,
    // maybe an operator) — never a phrase or sentence. This is the actual
    // signal that separates a genuine fraction from two unrelated lines
    // that happen to have *some* rule between them (a section divider, a box
    // border): every false positive found on that same invoice had a
    // numerator or denominator of 14+ characters, often a full sentence.
    // `detect_stacked_fractions` (the bar-less sibling detector) already
    // enforces this; `detect_fractions` never did.
    const MAX_FRACTION_TEXT_CHARS: usize = 6;
    let num_len = render_cell_plain(num_line).trim().chars().count();
    let den_len = render_cell_plain(den_line).trim().chars().count();
    if num_len == 0 || den_len == 0 || num_len > MAX_FRACTION_TEXT_CHARS || den_len > MAX_FRACTION_TEXT_CHARS {
        return false;
    }
    // The bar must be horizontally near the numerator/denominator (centred),
    // and overlap them.
    let num_overlap = overlap(nx0, nx1, bx0, bx1);
    let den_overlap = overlap(dx0, dx1, bx0, bx1);
    if num_overlap < 0.5 * bar_w.min(nx1 - nx0) || den_overlap < 0.5 * bar_w.min(dx1 - dx0) {
        return false;
    }
    // Stacked tightly: numerator baseline above the bar, denominator below.
    let num_baseline = num_line.first().map(|s| s.y).unwrap_or(0.0);
    let den_baseline = den_line.first().map(|s| s.y).unwrap_or(0.0);
    if !(num_baseline > bar_y && den_baseline < bar_y) {
        return false;
    }
    let above = num_baseline - bar_y;
    let below = bar_y - den_baseline;
    // Tight vertical stack relative to the numerator size.
    if above > 2.2 * num_size || below > 2.2 * num_size {
        return false;
    }
    true
}

/// Detect built-up fractions across a reading-order stream, using the thin
/// horizontal rules collected from the page content stream as fraction bars.
/// Returns the detected fractions in top-to-bottom order.
pub fn detect_fractions(stream: &[Vec<Span>], bars: &[RuleSeg]) -> Vec<FractionHit> {
    let mut hits = Vec::new();
    let mut used: Vec<bool> = vec![false; stream.len()];

    for (li, line) in stream.iter().enumerate() {
        if used[li] || line.is_empty() {
            continue;
        }
        let (lx0, _, lx1, _) = line_bbox(line);
        let line_size = line
            .iter()
            .map(|s| s.size)
            .fold(f64::NEG_INFINITY, f64::max)
            .max(0.1);
        // Find a bar that sits near this line's vertical belly and overlaps it
        // horizontally. We only try to match numerator (line above bar).
        for &(bar_y, bx0, bx1) in bars {
            let baseline = line.first().map(|s| s.y).unwrap_or(0.0);
            // The numerator must be above the bar and reasonably close.
            if baseline <= bar_y {
                continue;
            }
            if baseline - bar_y > 2.2 * line_size {
                continue;
            }
            if overlap(lx0, lx1, bx0, bx1) < 0.5 * line_size {
                continue;
            }
            // Search for a denominator line below the bar.
            let mut den_idx = None;
            for (di, dline) in stream.iter().enumerate() {
                if used[di] || di == li || dline.is_empty() {
                    continue;
                }
                let (dx0, _, dx1, _) = line_bbox(dline);
                let dbaseline = dline.first().map(|s| s.y).unwrap_or(0.0);
                if dbaseline >= bar_y {
                    continue;
                }
                if bar_y - dbaseline > 2.2 * line_size {
                    continue;
                }
                if overlap(dx0, dx1, bx0, bx1) < 0.5 * line_size {
                    continue;
                }
                den_idx = Some(di);
                break;
            }
            let Some(di) = den_idx else { continue };
            if used[di] {
                continue;
            }
            if !is_fraction_bar((bar_y, bx0, bx1), line, &stream[di]) {
                continue;
            }
            let num_expr = LatexExpr::text(render_cell_plain(line));
            let den_expr = LatexExpr::text(render_cell_plain(&stream[di]));
            hits.push(FractionHit {
                numerator_line: li,
                denominator_line: di,
                expr: LatexExpr::fraction(num_expr, den_expr),
            });
            used[li] = true;
            used[di] = true;
            break;
        }
    }
    // Top-to-bottom order.
    hits.sort_by(|a, b| a.numerator_line.cmp(&b.numerator_line));
    hits
}

// ---------------------------------------------------------------------------
// Page-level math-aware renderer
// ---------------------------------------------------------------------------

/// Detect a built-up fraction WITHOUT an explicit rule, as a stacked pair of
/// short, similarly-sized glyph runs that are tightly stacked and horizontally
/// centred. Used when the producer draws no fraction bar (many simple formulas).
pub(super) fn detect_stacked_fractions(stream: &[Vec<Span>]) -> Vec<FractionHit> {
    let mut used = vec![false; stream.len()];
    let mut hits = Vec::new();
    for i in 0..stream.len().saturating_sub(1) {
        if used[i] || stream[i].is_empty() {
            continue;
        }
        let j = i + 1;
        if used[j] || stream[j].is_empty() {
            continue;
        }
        let num = &stream[i];
        let den = &stream[j];
        let np = line_profile(num);
        let dp = line_profile(den);
        // Numerator above numerator, denominator below (stream is top-to-bottom).
        if np.baseline <= dp.baseline {
            continue;
        }
        let gap = np.baseline - dp.baseline;
        let max_size = np.size.max(dp.size).max(0.1);
        // Tight vertical stack (a fraction, not two prose lines).
        if !(0.6 * max_size..=2.2 * max_size).contains(&gap) {
            continue;
        }
        if (np.size - dp.size).abs() > 0.2 * max_size {
            continue;
        }
        let n_text = render_cell_plain(num);
        let d_text = render_cell_plain(den);
        let n_len = n_text.trim().chars().count();
        let d_len = d_text.trim().chars().count();
        // A non-empty `Vec<Span>` line can still be entirely whitespace glyphs
        // (spacing/underline decoration); reject those too, not just
        // over-long ones, or a blank line pair renders as an empty `$\frac{}{}$`.
        if n_len == 0 || d_len == 0 || n_len > 6 || d_len > 6 {
            continue;
        }
        let (nx0, _, nx1, _) = line_bbox(num);
        let (dx0, _, dx1, _) = line_bbox(den);
        let n_center = (nx0 + nx1) / 2.0;
        let d_center = (dx0 + dx1) / 2.0;
        // A real built-up fraction centres its numerator and denominator on a
        // shared axis, so their box centres essentially coincide. A loose
        // half-em tolerance also lets two *right-aligned* table/list numbers in
        // the same column (`61,07` over `8,93`, `3,00` over `-11,21` in the
        // ZUGFeRD fixture) read as a fraction: both are short, similar-sized and
        // tightly stacked, but their centres differ by ~0.3-0.5 em because the
        // column is right-, not centre-, aligned. Require the tight centre
        // agreement a genuine fraction has (typesetting jitter is a fraction of
        // a point) and leave right-aligned numeric columns as separate lines.
        if (n_center - d_center).abs() > 0.25 * max_size {
            continue;
        }
        if overlap(nx0, nx1, dx0, dx1) < 0.4 * (nx1 - nx0).min(dx1 - dx0).max(0.1) {
            continue;
        }
        hits.push(FractionHit {
            numerator_line: i,
            denominator_line: j,
            expr: LatexExpr::fraction(LatexExpr::text(n_text), LatexExpr::text(d_text)),
        });
        used[i] = true;
        used[j] = true;
    }
    hits
}

/// Render a stream of visual lines with math synthesis, replacing the base
/// (or numerator) line with `$...$` and skipping the consumed script/denominator
/// line. Lines with no detected math fall back to `fallback(i, line)` so the
/// caller can keep the original text for plain content.
/// Discards fraction hits that are part of a dense run of 3+ closely-spaced
/// hits — the geometric signature of a vertical reference-number/barcode
/// strip (a French invoice's account or tracking number, printed as one
/// short line per character/digit-pair) rather than actual math. Each
/// adjacent pair in such a strip coincidentally satisfies every check a
/// real 2-line built-up fraction also has to pass — tight vertical spacing,
/// near-equal size, narrow width, horizontally centred — because the strip
/// itself *is* a tight vertical stack of narrow, centred, similar-size
/// lines. The one thing that tells them apart is repetition: a real
/// document essentially never has 3+ fractions stacked back-to-back with
/// only a line or two of gap between them; that pattern is the
/// reference-number artifact. Runs shorter than 3 (an isolated fraction, or
/// two unrelated fractions that happen to land near each other) are left
/// alone.
pub(super) fn reject_dense_fraction_clusters(mut hits: Vec<FractionHit>) -> Vec<FractionHit> {
    const CLUSTER_GAP: usize = 4;
    const MIN_CLUSTER_LEN: usize = 3;
    if hits.len() < MIN_CLUSTER_LEN {
        return hits;
    }

    hits.sort_by_key(|h| h.numerator_line);
    let mut keep = vec![true; hits.len()];
    let mut run_start = 0usize;
    for i in 1..hits.len() {
        let gap = hits[i]
            .numerator_line
            .saturating_sub(hits[i - 1].denominator_line);
        if gap > CLUSTER_GAP {
            if i - run_start >= MIN_CLUSTER_LEN {
                keep[run_start..i].iter_mut().for_each(|k| *k = false);
            }
            run_start = i;
        }
    }
    if hits.len() - run_start >= MIN_CLUSTER_LEN {
        keep[run_start..].iter_mut().for_each(|k| *k = false);
    }

    hits.into_iter()
        .zip(keep)
        .filter_map(|(h, k)| k.then_some(h))
        .collect()
}
