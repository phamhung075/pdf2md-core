// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Reading order recovery, multi-column stream separation, and structured DocBlock generation.

use super::*;

/// Detect two columns via vertical projection gutter across the page (handles staggered
/// lines such as sidebar panels next to body text where baselines do not align).
pub fn detect_projection_two_columns(lines: &[Vec<Span>]) -> Option<PageColumns> {
    if lines.len() < 4 {
        return None;
    }

    struct LineBox {
        y: f64,
        x0: f64,
        x1: f64,
        size: f64,
        line: Vec<Span>,
    }
    let mut boxes = Vec::with_capacity(lines.len());
    let mut min_x = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    for l in lines {
        if l.is_empty() {
            continue;
        }
        let x0 = l.iter().map(|s| s.x).fold(f64::INFINITY, f64::min);
        let x1 = l.iter().map(|s| s.x + s.advance).fold(f64::NEG_INFINITY, f64::max);
        let size = l.iter().map(|s| s.size).fold(0.0f64, f64::max).max(0.1);
        min_x = min_x.min(x0);
        max_x = max_x.max(x1);
        boxes.push(LineBox {
            y: l[0].y,
            x0,
            x1,
            size,
            line: l.clone(),
        });
    }
    if max_x - min_x < 120.0 {
        return None;
    }

    let mut candidate_xs: Vec<f64> = Vec::new();
    for b in &boxes {
        if b.x1 > min_x + 50.0 && b.x1 < max_x - 50.0 {
            candidate_xs.push(b.x1 + 5.0);
        }
        for seg in split_line_segments(&b.line, None) {
            let seg_x1 = seg.iter().map(|s| s.x + s.advance).fold(f64::NEG_INFINITY, f64::max);
            if seg_x1 > min_x + 50.0 && seg_x1 < max_x - 50.0 {
                candidate_xs.push(seg_x1 + 5.0);
            }
        }
    }
    candidate_xs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    candidate_xs.dedup_by(|a, b| (*a - *b).abs() < 5.0);

    let mut best_gutter: Option<(f64, f64, f64)> = None;

    for &gx in &candidate_xs {
        let mut left_count = 0;
        let mut right_count = 0;
        let mut max_l_x = f64::NEG_INFINITY;
        let mut min_r_x = f64::INFINITY;
        let mut crossing_count = 0;

        for b in &boxes {
            if b.x1 <= gx {
                left_count += 1;
                max_l_x = max_l_x.max(b.x1);
            } else if b.x0 >= gx {
                right_count += 1;
                min_r_x = min_r_x.min(b.x0);
            } else {
                let left_spans: Vec<Span> = b.line.iter().filter(|s| s.x + s.advance <= gx).cloned().collect();
                let right_spans: Vec<Span> = b.line.iter().filter(|s| s.x >= gx).cloned().collect();
                if !left_spans.is_empty() && !right_spans.is_empty() {
                    let l_end = left_spans.iter().map(|s| s.x + s.advance).fold(f64::NEG_INFINITY, f64::max);
                    let r_start = right_spans.iter().map(|s| s.x).fold(f64::INFINITY, f64::min);
                    // A non-whitespace span that *straddles* the candidate
                    // gutter (`s.x < gx < s.x + advance`) satisfies neither the
                    // left (`end <= gx`) nor the right (`x >= gx`) filter, so it
                    // is absent from both and `r_start - l_end` reports a wide
                    // apparent white corridor that the straddling run actually
                    // fills. Producers routinely split one word across several
                    // `Tj` runs (`L|e statut et le r|égim|e`, `c|our|s`), so on a
                    // centered single-column line this manufactures a phantom
                    // page gutter: `detect_projection_two_columns` then reads
                    // the page as two columns and emits each line's tail after
                    // every line's head. Subtract the straddlers' ink and accept
                    // the gutter only when real whitespace of gutter width is
                    // left over. A short straddler genuinely sitting between two
                    // columns (the invoice `Z` regression) leaves enough gap to
                    // still count as a split.
                    let covered: f64 = b
                        .line
                        .iter()
                        .filter(|s| {
                            !s.text.trim().is_empty() && s.x < gx && s.x + s.advance > gx
                        })
                        .map(|s| {
                            let lo = s.x.max(l_end);
                            let hi = (s.x + s.advance).min(r_start);
                            (hi - lo).max(0.0)
                        })
                        .sum();
                    if r_start - l_end - covered >= 1.2 * b.size {
                        left_count += 1;
                        right_count += 1;
                        max_l_x = max_l_x.max(l_end);
                        min_r_x = min_r_x.min(r_start);
                        continue;
                    }
                }
                crossing_count += 1;
            }
        }

        let gutter_width = min_r_x - max_l_x;
        if gutter_width >= 15.0 && left_count >= 3 && right_count >= 3 {
            let score = gutter_width * (left_count.min(right_count) as f64) - (crossing_count as f64 * 100.0);
            if crossing_count <= 2 && best_gutter.map_or(true, |(_, _, s)| score > s) {
                best_gutter = Some((max_l_x, min_r_x, score));
            }
        }
    }

    let (gl, gr, _) = best_gutter?;
    let gx = (gl + gr) / 2.0;

    let mut col_top = f64::NEG_INFINITY;
    let mut col_bottom = f64::INFINITY;
    for b in &boxes {
        if b.x1 <= gx || b.x0 >= gx {
            col_top = col_top.max(b.y);
            col_bottom = col_bottom.min(b.y);
        }
    }

    let mut top_full: Vec<Vec<Span>> = Vec::new();
    let mut bottom_full: Vec<Vec<Span>> = Vec::new();
    let mut left: Vec<(f64, Vec<Span>)> = Vec::new();
    let mut right: Vec<(f64, Vec<Span>)> = Vec::new();

    for b in boxes {
        if b.x0 < gx && b.x1 > gx {
            // Partition the row's spans completely: `left` takes everything
            // that ends at or before the gutter and `right` takes the exact
            // complement. Two independent half-open filters (`x + advance <=
            // gx` and `x >= gx`) leave any span that *straddles* the gutter —
            // e.g. a glyph whose advance crosses it — in neither half, so that
            // span's text is silently deleted from BOTH columns (the row is
            // still pushed because each half is non-empty). On the FNFE
            // MINIMUM invoice this dropped the middle `0` of `120 000,00 €`
            // from the `blocks` channel while the Markdown, rendered through
            // the band path, kept it. The complement filter is total: every
            // span lands in exactly one side, and a straddler follows the side
            // its box leans to (it is kept whole instead of vanishing).
            let left_spans: Vec<Span> = b.line.iter().filter(|s| s.x + s.advance <= gx).cloned().collect();
            let right_spans: Vec<Span> = b.line.iter().filter(|s| s.x + s.advance > gx).cloned().collect();
            if !left_spans.is_empty() && !right_spans.is_empty() {
                left.push((b.y, left_spans));
                right.push((b.y, right_spans));
                continue;
            }
            if b.y >= col_top - 5.0 {
                top_full.push(b.line);
            } else if b.y <= col_bottom + 5.0 {
                bottom_full.push(b.line);
            } else {
                // A mid-block row that cannot be split into two non-empty
                // halves — e.g. the whole line is a *single* span (PDF
                // producers often draw `TVA Intracommunautaire : FR…` as one
                // TJ run) whose advance crosses the gutter, so the
                // complement partition puts it entirely on one side and the
                // other half comes back empty. The top/bottom fallback above
                // only accepts rows at the block's edge, so without this a
                // mid-block row vanished from BOTH reading-order streams (and
                // therefore from the `blocks` channel) even though the
                // Markdown, rendered through the band path, kept it. Keep it
                // whole on the side its box centre leans to.
                let cx = 0.5 * (b.x0 + b.x1);
                if cx <= gx {
                    left.push((b.y, b.line.clone()));
                } else {
                    right.push((b.y, b.line.clone()));
                }
            }
            continue;
        }

        if b.x1 <= gx {
            left.push((b.y, b.line));
        } else {
            right.push((b.y, b.line));
        }
    }

    left.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    right.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    top_full.sort_by(|a, b| b[0].y.partial_cmp(&a[0].y).unwrap_or(std::cmp::Ordering::Equal));
    bottom_full.sort_by(|a, b| b[0].y.partial_cmp(&a[0].y).unwrap_or(std::cmp::Ordering::Equal));

    Some(PageColumns {
        top_full,
        left: left.into_iter().map(|(_, v)| v).collect(),
        right: right.into_iter().map(|(_, v)| v).collect(),
        bottom_full,
    })
}

/// Same gate as [`prose_beside_grid`], but additionally requires the non-clean
/// side to be a *real* grid — an internal gutter aligned across at least two
/// rows. The running-gutter pass has the row-split evidence that the projection
/// fallback lacks, so without this a coincidental gap inside prose could be
/// mistaken for a column and split a sentence in half.
pub(super) fn projection_prose_beside_grid(left: &[Vec<Span>], right: &[Vec<Span>]) -> bool {
    let (left_clean, right_clean) = (rows_are_clean(left), rows_are_clean(right));
    let (lw, rw) = (avg_words_per_row(left), avg_words_per_row(right));
    (left_clean
        && !right_clean
        && has_aligned_internal_gutter(right)
        && lw >= 4.0
        && wrapped_prose(left)
        && lw > rw)
        || (right_clean
            && !left_clean
            && has_aligned_internal_gutter(left)
            && rw >= 4.0
            && wrapped_prose(right)
            && rw > lw)
}

/// Recover a two-column block whose inter-column gutter is *narrower* than the
/// `1.2em` a standalone row split needs and which therefore never seeds the
/// running-gutter pass: a prose column facing a multi-column grid with only a
/// few points of white between them (common in tightly-typeset two-column
/// academic pages). Works from the vertical projection instead: find an x where
/// a contiguous run of rows has no span straddling it, then split the whole run
/// there. Returns `(start_index, end_index, left_rows, right_rows)`.
pub(super) fn projection_columns_region(
    lines: &[Vec<Span>],
) -> Option<(usize, usize, Vec<Vec<Span>>, Vec<Vec<Span>>)> {
    if lines.len() < 6 {
        return None;
    }

    /// A span straddles `gx` when it covers that x position — content on both
    /// sides of `gx` with no gutter there.
    fn straddles(line: &[Span], gx: f64) -> bool {
        line.iter().any(|s| s.x + 0.5 < gx && s.x + s.advance - 0.5 > gx)
    }

    // Candidate gutters: the midpoint of every inter-span gap at least 0.5em
    // wide (white narrower than a word space cannot separate columns), plus
    // each line's own left and right content edge — a column whose rows never
    // merged across the gutter shows it only as those facing edges.
    let mut cands: Vec<f64> = Vec::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let size = line.iter().map(|s| s.size).fold(0.0f64, f64::max).max(0.1);
        for w in line.windows(2) {
            let a_end = w[0].x + w[0].advance;
            let gap = w[1].x - a_end;
            if gap >= 0.5 * size {
                cands.push((a_end + w[1].x) / 2.0);
            }
        }
        cands.push(line.iter().map(|s| s.x).fold(f64::INFINITY, f64::min));
        cands.push(line.iter().map(|s| s.x + s.advance).fold(f64::NEG_INFINITY, f64::max));
    }
    cands.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    cands.dedup_by(|a, b| (*a - *b).abs() < 3.0);

    let mut best: Option<(usize, usize, Vec<Vec<Span>>, Vec<Vec<Span>>)> = None;
    for &gx in &cands {
        let mut i = 0;
        while i < lines.len() {
            if lines[i].is_empty() || straddles(&lines[i], gx) {
                i += 1;
                continue;
            }
            let start = i;
            while i < lines.len() && !lines[i].is_empty() && !straddles(&lines[i], gx) {
                i += 1;
            }
            let end = i - 1;
            // A real block spans several rows; a shorter "run" is usually an
            // accidental short alignment inside running prose.
            if end - start + 1 < 6 {
                continue;
            }
            // Snap the gutter to the centre of the white actually available
            // across the run (the candidate came from a single row, and column
            // edges are ragged).
            let (mut left_end, mut right_start) = (f64::NEG_INFINITY, f64::INFINITY);
            for line in &lines[start..=end] {
                for s in line {
                    if s.x >= gx {
                        right_start = right_start.min(s.x);
                    } else {
                        left_end = left_end.max(s.x + s.advance);
                    }
                }
            }
            if !(left_end < right_start) {
                continue;
            }
            let snap = 0.5 * (left_end + right_start);
            if lines[start..=end].iter().any(|l| straddles(l, snap)) {
                continue;
            }
            let mut left: Vec<Vec<Span>> = Vec::new();
            let mut right: Vec<Vec<Span>> = Vec::new();
            for line in &lines[start..=end] {
                let (l, r): (Vec<Span>, Vec<Span>) = line
                    .iter()
                    .cloned()
                    .partition(|s| s.x + s.advance <= snap + 0.5);
                if !l.is_empty() {
                    left.push(l);
                }
                if !r.is_empty() {
                    right.push(r);
                }
            }
            if left.len() < 3 || right.len() < 3 {
                continue;
            }
            // The running-gutter pass can also miss a *narrow* gutter between
            // two clean text columns: when the left column is justified flush
            // against the facing column (a body paragraph whose last word ends
            // exactly at the side column's left edge) no row ever shows the
            // standalone `1.2em` gap the pass needs, so both regions fall back
            // to one interleaved stream and the side column's text is woven into
            // the body prose line-by-line. The projection has already proved a
            // straddle-free corridor over several rows with text on both sides;
            // accept it when both halves are clean single columns that read as
            // text and the corridor left real white.
            //
            // "Multi-word" is measured per whitespace-separated word, not per
            // span: a bilingual block (French and English side by side) is
            // routinely drawn as one `Tj` run per line, so `avg_words_per_row`
            // reports 1.0 for a column that plainly carries a sentence. The
            // wrapped-continuation check is likewise required on *either* side
            // rather than both: the French column continues across lines
            // ("... (si" / "votre tarif ...") while each English list item
            // starts capitalized, and the corridor plus the clean halves are
            // what establish the split.
            //
            // Both halves must also be *columns* rather than fragments: each
            // side's rows have to share a starting edge (a few points of
            // raggedness at most). Cutting one full-width paragraph at a
            // recurring gap yields halves whose start x jumps with the
            // sentence — the `enedis_hp_hc` false positives spread 100-360pt,
            // against the bilingual e-ticket's 22pt.
            let two_clean_text_columns = {
                let gap = right_start - left_end;
                let size = left
                    .iter()
                    .chain(right.iter())
                    .flat_map(|r| r.iter())
                    .map(|s| s.size)
                    .fold(0.0f64, f64::max)
                    .max(0.1);
                rows_are_clean(&left)
                    && rows_are_clean(&right)
                    && gap >= 0.6 * size
                    && words_per_row(&left) >= 2.5
                    && words_per_row(&right) >= 2.5
                    && column_start_spread(&left) <= 4.0 * size
                    && column_start_spread(&right) <= 4.0 * size
                    && (wrapped_prose(&left) || wrapped_prose(&right))
            };
            // A span-count prose gate still admits the shape this fallback was
            // written for (a column of one-word spans); when it fails, the
            // word-based two-clean-columns gate is the second chance.
            if !two_clean_text_columns
                && (avg_words_per_row(&left) < 2.5 || avg_words_per_row(&right) < 2.5)
            {
                continue;
            }
            if !projection_prose_beside_grid(&left, &right) && !two_clean_text_columns {
                continue;
            }
            // Longest run wins; a tie keeps the earlier (leftmost) gutter, the
            // page gutter rather than an internal grid one.
            let score = end - start + 1;
            if best.as_ref().map_or(true, |b| score > b.1 - b.0 + 1) {
                best = Some((start, end, left, right));
            }
        }
    }
    best
}
