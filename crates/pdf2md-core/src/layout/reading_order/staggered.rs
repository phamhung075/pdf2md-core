// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Reading order recovery, multi-column stream separation, and structured DocBlock generation.

use super::*;

/// Recover a *staggered* multi-column region: three or more spatially disjoint
/// columns whose rows keep their own baselines, so no single gutter runs across
/// the block and neither the running-gutter pass nor the vertical projection can
/// see it.
///
/// A bilingual e-ticket header is the motivating shape. The French heading and
/// its English translation sit one above the other in a narrow column, a
/// neighbouring heading pair does the same alongside it, and the columns'
/// baselines interleave (column 1 line 1, column 2 line 1, column 3 line 1,
/// column 1 line 2, ...). Merging the runs by baseline welds those into
/// spurious rows, and the plain top-to-bottom order then threads one column's
/// second line between a neighbour's pair. Split each row into its visual
/// segments, cluster the segments by horizontal extent, and when the clusters
/// form staggered columns emit each column top-to-bottom, left-to-right.
///
/// The window with the most columns wins (a bridging full-width row merges two
/// columns into one and lowers the count, so it is naturally excluded), then
/// the longest such window. Returns `(start, end, columns)`.
pub(super) fn staggered_columns_region(rows: &[Vec<Span>]) -> Option<(usize, usize, Vec<Vec<Vec<Span>>>)> {
    const MIN_STACK_ROWS: usize = 3;
    const MAX_STACK_WINDOW: usize = 8;
    if rows.len() < MIN_STACK_ROWS {
        return None;
    }
    let body = body_size_for(rows).max(1.0);
    let merge_gap = 0.6 * body;
    let max_spread = 2.5 * body;
    let segs: Vec<Vec<Vec<Span>>> = rows.iter().map(|r| split_line_segments(r)).collect();

    // Only a row that itself splits into >= 2 segments can anchor a staggered
    // block: the block's first row must carry at least two of its columns.
    let mut best: Option<(usize, usize, Vec<Vec<Vec<Span>>>)> = None;
    for s in 0..rows.len() {
        if segs[s].len() < 2 {
            continue;
        }
        let last = (s + MAX_STACK_WINDOW).min(rows.len() - 1);
        for e in (s + MIN_STACK_ROWS - 1)..=last {
            if let Some(cols) = staggered_window_columns(&segs[s..=e], merge_gap, max_spread) {
                let key = (cols.len(), e - s + 1);
                let better = best
                    .as_ref()
                    .map_or(true, |b| key > (b.2.len(), b.1 - b.0 + 1));
                if better {
                    best = Some((s, e, cols));
                }
            }
        }
    }
    best
}

/// Cluster one candidate window's visual segments into columns by horizontal
/// extent, or `None` when the window is not a staggered multi-column block.
pub(super) fn staggered_window_columns(
    segs: &[Vec<Vec<Span>>],
    merge_gap: f64,
    max_spread: f64,
) -> Option<Vec<Vec<Vec<Span>>>> {
    // A staggered stack is a block of consecutive rows that *each* carry pieces
    // of two or more columns: the columns' baselines interleave, the line
    // builder welds a column's fragment to a neighbour's, and every resulting
    // row is still multi-column. A region that merely contains a few
    // multi-column rows among single-column rows is an ordinary multi-zone
    // layout (e.g. an invoice header with full-width fields between its zones)
    // whose logical lines the plain order already keeps together; re-banding it
    // would split them apart. Requiring every row of the window to split into
    // >= 2 segments separates the two.
    if segs.iter().any(|row| row.len() < 2) {
        return None;
    }
    const MAX_STACK_COLS: usize = 6;
    struct Item {
        x0: f64,
        x1: f64,
        y: f64,
        seg: Vec<Span>,
    }
    let mut items: Vec<Item> = Vec::new();
    for row in segs {
        for seg in row {
            let x0 = seg.iter().map(|p| p.x).fold(f64::INFINITY, f64::min);
            let x1 = seg.iter().map(|p| p.x + p.advance).fold(f64::NEG_INFINITY, f64::max);
            if x0.is_finite() && x1.is_finite() && x1 > x0 {
                items.push(Item { x0, x1, y: seg[0].y, seg: seg.clone() });
            }
        }
    }
    if items.len() < 4 {
        return None;
    }
    // 1-D interval cluster of the segments' x extents. Segments of one column
    // overlap (or nearly touch) and merge; a real inter-column gutter is wider
    // than `merge_gap`.
    let mut order: Vec<usize> = (0..items.len()).collect();
    order.sort_by(|&a, &b| {
        items[a].x0.partial_cmp(&items[b].x0).unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut bounds: Vec<(f64, f64)> = Vec::new();
    for &i in &order {
        match bounds.last_mut() {
            Some(c) if items[i].x0 - c.1 <= merge_gap => c.1 = c.1.max(items[i].x1),
            _ => bounds.push((items[i].x0, items[i].x1)),
        }
    }
    // Three or more disjoint x-clusters. A two-column arrangement is the
    // territory of the existing running-gutter / projection passes; requiring a
    // third column is what keeps a two-column prose block (one flowing run cut
    // by a recurring gap, or a genuine sidebar) from being re-banded here.
    if bounds.len() < 3 || bounds.len() > MAX_STACK_COLS {
        return None;
    }
    // Assign each segment to the column it overlaps most; every column must
    // carry at least two rows, or it is not a column but a stray.
    let mut cols: Vec<Vec<Vec<Span>>> = vec![Vec::new(); bounds.len()];
    let mut counts = vec![0usize; bounds.len()];
    let mut seq: Vec<(f64, usize)> = Vec::new();
    for it in &items {
        let mut best_ci = None;
        let mut best_overlap = 0.0f64;
        for (ci, &(c0, c1)) in bounds.iter().enumerate() {
            let overlap = (it.x1.min(c1) - it.x0.max(c0)).max(0.0);
            if overlap > best_overlap {
                best_overlap = overlap;
                best_ci = Some(ci);
            }
        }
        let ci = best_ci?;
        counts[ci] += 1;
        seq.push((it.y, ci));
        cols[ci].push(it.seg.clone());
    }
    // Every column is a *pair*: the French line and its English translation,
    // exactly two rows each. A three-or-more-row column is an invoice header
    // block or a prose fragment, not a heading pair, and re-banding it would
    // reorder content the plain order already had right.
    if counts.iter().any(|&n| n != 2) {
        return None;
    }
    // A real column's rows share a starting edge. When a recurring gap cuts one
    // flowing block in two, the fragments' start x jumps with the sentence
    // (verified 100-360pt on `enedis_hp_hc` against 0-21pt on the bilingual
    // e-ticket header), so a bounded spread keeps the re-banding from
    // transposing prose.
    if cols.iter().any(|c| column_start_spread(c) > max_spread) {
        return None;
    }
    // Columns must be *staggered*, not a table grid. A table's columns share
    // every row baseline (that is what makes it a grid); a bilingual stack's
    // columns keep their own baselines and interleave. For every pair, at most
    // half of the shorter column's rows may share a baseline — a grid fails
    // this on every pair, while the header's two neighbouring columns share at
    // most the one row the line builder merged across the gutter.
    let tol = 1.2;
    let ys: Vec<Vec<f64>> = cols
        .iter()
        .map(|c| c.iter().map(|s| s[0].y).collect())
        .collect();
    for a in 0..ys.len() {
        for b in (a + 1)..ys.len() {
            let shared = ys[a]
                .iter()
                .filter(|&&ya| ys[b].iter().any(|&yb| (ya - yb).abs() <= tol))
                .count();
            if shared * 2 > ys[a].len().min(ys[b].len()) {
                return None;
            }
        }
    }
    for c in &mut cols {
        c.sort_by(|a, b| b[0].y.partial_cmp(&a[0].y).unwrap_or(std::cmp::Ordering::Equal));
    }
    // Only a *staggered* block needs this re-banding. If the rows already fall
    // column-by-column in the plain top-to-bottom order (a two-column page
    // whose left column simply precedes the right), column-major equals the
    // plain order and splitting would only add spurious breaks.
    seq.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    let transitions = seq.windows(2).filter(|w| w[0].1 != w[1].1).count();
    if transitions < cols.len() {
        return None;
    }
    Some(cols)
}
