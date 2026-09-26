// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Reading order recovery, multi-column stream separation, and structured DocBlock generation.

use super::*;

/// Recover a page region of 3+ narrow columns.
///
/// Every other reading-order detector models a *single* gutter
/// (`page_two_columns`, `detect_column_bands`) or a prose-vs-grid pair
/// (`projection_columns_region`). A magazine/newsletter page of three or more
/// prose columns separated by only ~0.7em of white defeats all of them: the
/// line builder fuses each row's columns into one visual line (word spaces are
/// ~0.25em, so a 0.7em gutter is far below any hard-break threshold) and the
/// whole page then weaves column-by-column row-by-row. This works from the
/// vertical projection instead: a gutter is an x covered by no span on any row
/// of the region, wide enough to be real white and narrow enough to sit between
/// columns. Rows above/below the region are left in place as full-width
/// streams, so a page header/footer keeps its order.
///
/// Deliberately conservative: it requires its own set of at least 2 gutters
/// (>=3 columns), each column to read as wrapped multi-word prose, and the
/// region to span several rows. A page it cannot read this way returns `None`
/// and falls back to the single linear stream exactly as before.
pub(super) fn multi_column_projection(lines: &[Vec<Span>]) -> Option<MultiColumnRegion> {
    const MIN_REGION_ROWS: usize = 8;
    const MIN_GAP_EM: f64 = 0.4;
    const MIN_COL_WIDTH_EM: f64 = 3.0;
    if lines.len() < MIN_REGION_ROWS {
        return None;
    }

    // Candidate gutters: the midpoint of every inter-span gap at least
    // `MIN_GAP_EM` wide. A word space (~0.25em) never seeds one.
    let mut cands: Vec<f64> = Vec::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        for w in line.windows(2) {
            let a_end = w[0].x + w[0].advance;
            let gap = w[1].x - a_end;
            let size = w[1].size.max(w[0].size).max(0.1);
            if gap >= MIN_GAP_EM * size {
                cands.push(0.5 * (a_end + w[1].x));
            }
        }
    }
    cands.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    cands.dedup_by(|a, b| (*a - *b).abs() < 3.0);
    if cands.len() < 2 {
        return None;
    }

    let n = lines.len();
    let c = cands.len();
    // For each candidate: is no span on this row *covering* it (so the row is
    // compatible with a column split there), and does a real gap sit on it.
    let mut compat: Vec<Vec<bool>> = vec![vec![false; n]; c];
    let mut has_gap: Vec<Vec<bool>> = vec![vec![false; n]; c];
    for (g, &gx) in cands.iter().enumerate() {
        for (i, line) in lines.iter().enumerate() {
            if line.is_empty() {
                compat[g][i] = true;
                continue;
            }
            let mut covered = false;
            let mut gap_here = false;
            for (k, s) in line.iter().enumerate() {
                if s.x + 0.5 < gx && s.x + s.advance - 0.5 > gx {
                    covered = true;
                    break;
                }
                if let Some(t) = line.get(k + 1) {
                    let a_end = s.x + s.advance;
                    let gap = t.x - a_end;
                    let size = t.size.max(s.size).max(0.1);
                    if gap >= MIN_GAP_EM * size && a_end < gx && t.x > gx {
                        gap_here = true;
                    }
                }
            }
            compat[g][i] = !covered;
            has_gap[g][i] = gap_here;
        }
    }

    // Longest contiguous window on which at least two *distinct* gutters are
    // compatible on every row and each has a real gap on most rows.
    let body = body_size_for(lines);
    let min_col_w = MIN_COL_WIDTH_EM * body;
    let mut best: Option<(usize, usize, Vec<usize>)> = None;
    for s in 0..n {
        let mut all_ok = vec![true; c];
        let mut gaps = vec![0usize; c];
        let mut e = s;
        while e < n {
            for g in 0..c {
                if all_ok[g] && !compat[g][e] {
                    all_ok[g] = false;
                }
                if has_gap[g][e] {
                    gaps[g] += 1;
                }
            }
            let len = e - s + 1;
            let active: Vec<usize> = (0..c)
                .filter(|&g| all_ok[g] && (gaps[g] as f64) >= 0.6 * len as f64)
                .collect();
            // The window must actually separate three or more columns: its
            // active gutters, collapsed against the minimum column width, must
            // leave at least two distinct x positions. A two-column region
            // (the territory of `page_two_columns` / `detect_column_bands`)
            // can otherwise contribute two near-coincident candidate x's that
            // collapse to a single physical gutter; counting them as a
            // "multi-column" hit made this pass claim a region it could not
            // split and then bail out, leaving the real 3-column region above
            // it interleaved.
            let mut distinct: Vec<f64> = active.iter().map(|&g| cands[g]).collect();
            distinct.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            distinct.dedup_by(|a, b| (*a - *b).abs() < min_col_w);
            if distinct.len() >= 2 && best.as_ref().map_or(true, |b| len > b.1 - b.0 + 1) {
                best = Some((s, e, active));
            }
            if all_ok.iter().filter(|x| **x).count() < 2 {
                break;
            }
            e += 1;
        }
    }
    let (start, end, active) = best?;
    if end - start + 1 < MIN_REGION_ROWS {
        return None;
    }

    let mut left_edge = f64::INFINITY;
    let mut right_edge = f64::NEG_INFINITY;
    for line in &lines[start..=end] {
        for sp in line {
            left_edge = left_edge.min(sp.x);
            right_edge = right_edge.max(sp.x + sp.advance);
        }
    }
    if !left_edge.is_finite() || right_edge - left_edge < 3.0 * min_col_w {
        return None;
    }

    // Keep the widest set of gutters that each leaves a real column on either
    // side and is separated from the next by at least one column width.
    let mut gutters: Vec<f64> = active.iter().map(|&g| cands[g]).collect();
    gutters.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    gutters.dedup_by(|a, b| (*a - *b).abs() < min_col_w);
    let mut selected: Vec<f64> = Vec::new();
    for gx in gutters {
        if gx - left_edge < min_col_w || right_edge - gx < min_col_w {
            continue;
        }
        if selected.last().map_or(true, |&last| gx - last >= min_col_w) {
            selected.push(gx);
        }
    }
    if selected.len() < 2 {
        return None;
    }

    // Bucket the region's rows by a gutter set.
    let bucket = |selected: &[f64]| -> Vec<Vec<Vec<Span>>> {
        let ncols = selected.len() + 1;
        let mut columns: Vec<Vec<Vec<Span>>> = vec![Vec::new(); ncols];
        for line in &lines[start..=end] {
            let mut buckets: Vec<Vec<Span>> = vec![Vec::new(); ncols];
            for sp in line {
                let center = sp.x + 0.5 * sp.advance;
                let mut col = 0usize;
                while col < selected.len() && center >= selected[col] {
                    col += 1;
                }
                buckets[col].push(sp.clone());
            }
            for (ci, bucket) in buckets.into_iter().enumerate() {
                if !bucket.is_empty() {
                    columns[ci].push(bucket);
                }
            }
        }
        columns
    };

    // A wide inter-column corridor contributes two candidate x's, one at each
    // edge of the white space. Both survive the minimum-column-width filter and
    // bracket an empty "column" of pure white (e.g. a 57pt corridor whose two
    // edges are 48pt apart — wider than `min_col_w`, so neither is dropped by
    // the separation rule). Real columns carry several rows of text, so drop
    // the boundary gutter of the emptiest failing column and re-bucket, until
    // every column reads as a text column or too few gutters remain (then fall
    // back exactly as before, leaving the page to the band pass).
    let mut columns = bucket(&selected);
    while selected.len() >= 2 {
        // `avg_words_per_row` counts non-empty *spans*, not words: a narrow
        // prose column whose lines are each drawn as one or two `Tj` runs
        // averages well under the 2.5 the two-column detectors use, even
        // though every row carries a clause of text. A phantom corridor column
        // is separated by `len < 3` instead (it holds a row or two at most),
        // so a lower span gate keeps genuine narrow columns without admitting
        // white corridors.
        let bad = columns
            .iter()
            .position(|c| c.len() < 3 || avg_words_per_row(c) < 2.0);
        let Some(ci) = bad else {
            break;
        };
        let drop_at = if ci == 0 { 0 } else { ci - 1 };
        selected.remove(drop_at);
        columns = bucket(&selected);
    }
    if selected.len() < 2 {
        return None;
    }
    if !columns.iter().any(|col| wrapped_prose(col)) {
        return None;
    }
    Some(MultiColumnRegion { start, end, columns })
}
