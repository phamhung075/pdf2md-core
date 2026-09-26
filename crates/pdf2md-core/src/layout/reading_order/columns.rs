// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Reading order recovery, multi-column stream separation, and structured DocBlock generation.

use super::*;

/// Split one visual row at a clearly oversized intra-row gap (a column
/// gutter). Rows without such a gap are single-column rows -> None.
pub fn split_row_columns(spans: &[Span]) -> Option<(Vec<Span>, Vec<Span>)> {
    if spans.len() < 5 {
        return None;
    }
    let size = spans.iter().map(|s| s.size).fold(0.0f64, f64::max).max(0.1);
    let mut best: Option<(f64, usize)> = None; // (gap, split index)
    for i in 0..spans.len() - 1 {
        let a_end = spans[i].x + spans[i].advance;
        let b_start = spans[i + 1].x;
        let gap = (b_start - a_end).max(0.0);
        // A word space is ~0.25em; a true gutter is much wider. Requiring
        // > 1.2em keeps justified prose rows unsplit.
        if gap > 1.2 * size && best.map_or(true, |(g, _)| gap > g) {
            best = Some((gap, i));
        }
    }
    let (_, at) = best?;
    if at < 2 || at >= spans.len() - 3 {
        return None;
    }
    Some((spans[..=at].to_vec(), spans[at + 1..].to_vec()))
}

/// Split one visual row at an *already-established* column gutter `gx`, even
/// when the local gap is narrower than `split_row_columns`'s standalone
/// `1.2em` threshold.
///
/// A justified two-column layout can leave barely ~1em of whitespace between
/// its columns — below the threshold that distinguishes a genuine gutter from
/// ordinary (possibly stretched) word spacing on a single row, so
/// `split_row_columns` alone misses those rows. Once `detect_column_bands` has
/// confirmed a consistent gutter across several rows, the remaining rows of the
/// same block can be split reliably against it: the gap is taken only when its
/// midpoint sits at the known gutter, it is at least `0.6em`, and it is wider
/// than every other inter-word gap in the row, so ordinary prose never splits.
pub(super) fn split_row_at_gutter(spans: &[Span], gx: f64) -> Option<(Vec<Span>, Vec<Span>)> {
    if spans.len() < 5 {
        return None;
    }
    let size = spans.iter().map(|s| s.size).fold(0.0f64, f64::max).max(0.1);
    let mut at_gutter: Option<(f64, usize)> = None; // (gap, split index)
    let mut max_other_gap = 0.0f64;
    for i in 0..spans.len() - 1 {
        let a_end = spans[i].x + spans[i].advance;
        let b_start = spans[i + 1].x;
        let gap = (b_start - a_end).max(0.0);
        let mid = (a_end + b_start) / 2.0;
        if (mid - gx).abs() <= 0.5 * size + 6.0 {
            if at_gutter.map_or(true, |(g, _)| gap > g) {
                at_gutter = Some((gap, i));
            }
        } else if gap > max_other_gap {
            max_other_gap = gap;
        }
    }
    let (gap, at) = at_gutter?;
    if at < 2 || at >= spans.len() - 2 {
        return None;
    }
    if gap < 0.6 * size || gap <= max_other_gap {
        return None;
    }
    Some((spans[..=at].to_vec(), spans[at + 1..].to_vec()))
}

/// Detect a genuine two-column page via simultaneous row-based gutters.
pub fn page_two_columns_rows(lines: &[Vec<Span>]) -> Option<PageColumns> {
    if lines.len() < 3 {
        return None;
    }
    struct Split {
        y: f64,
        left: Vec<Span>,
        right: Vec<Span>,
        /// Right edge of the left column's text on this row.
        l_end: f64,
        /// Left edge of the right column's text on this row.
        r_start: f64,
        gutter_x: f64,
    }
    let mut splits: Vec<Split> = Vec::new();
    for l in lines {
        if let Some((left, right)) = split_row_columns(l) {
            let l_end = left
                .iter()
                .map(|x| x.x + x.advance)
                .fold(f64::NEG_INFINITY, f64::max);
            let r_start = right.iter().map(|x| x.x).fold(f64::INFINITY, f64::min);
            splits.push(Split {
                y: l[0].y,
                left,
                right,
                l_end,
                r_start,
                gutter_x: (l_end + r_start) / 2.0,
            });
        }
    }
    if splits.len() < 3 {
        return None;
    }
    let mut gxs: Vec<f64> = splits.iter().map(|s| s.gutter_x).collect();
    gxs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let med = gxs[gxs.len() / 2];
    let tol = (0.15 * med.abs()).max(6.0);
    // The columns' facing *edges* are what stay fixed across a block; the
    // midpoint of the white between them recedes left as soon as a row's
    // facing text is short. The last line of a paragraph is the common case
    // and defeats the midpoint gate: on this document's receipt block the
    // left segment ends at x=125 instead of x=285, moving the midpoint from
    // 295 to 215 while the right column still begins at exactly x=305. Such a
    // row is still a crossing row of the same two columns, so accept a split
    // when its corridor overlaps the block's median corridor. Without this it
    // was filtered out of `keep`, `col_bottom` stopped one row above it, and
    // the row was flushed as `bottom_full` after the entire opposite column —
    // cutting both the French and the English sentence in half.
    let mut l_ends: Vec<f64> = splits.iter().map(|s| s.l_end).collect();
    let mut r_starts: Vec<f64> = splits.iter().map(|s| s.r_start).collect();
    let median = |v: &mut Vec<f64>| {
        v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        v[v.len() / 2]
    };
    let (med_l, med_r) = (median(&mut l_ends), median(&mut r_starts));
    let keep: Vec<Split> = splits
        .into_iter()
        .filter(|s| {
            (s.gutter_x - med).abs() <= tol
                || (s.l_end <= med_r + tol && s.r_start >= med_l - tol)
        })
        .collect();
    if keep.len() < 3 {
        return None;
    }
    let rows_with_text = lines.iter().filter(|l| l.len() >= 3).count().max(1);
    if keep.len() * 2 < rows_with_text {
        return None;
    }
    let col_top = keep.iter().map(|s| s.y).fold(f64::NEG_INFINITY, f64::max);
    let col_bottom = keep.iter().map(|s| s.y).fold(f64::INFINITY, f64::min);
    let mut left: Vec<(f64, Vec<Span>)> = Vec::new();
    let mut right: Vec<(f64, Vec<Span>)> = Vec::new();
    let mut top_full: Vec<Vec<Span>> = Vec::new();
    let mut bottom_full: Vec<Vec<Span>> = Vec::new();
    for l in lines {
        let y = l[0].y;
        let y_tol = 0.5 * l[0].size.max(0.1);
        if let Some(sp) = keep.iter().find(|s| (s.y - y).abs() < y_tol) {
            left.push((sp.y, sp.left.clone()));
            right.push((sp.y, sp.right.clone()));
            continue;
        }
        // A line that never paired with a facing line, yet sits *inside* the
        // column block's vertical span and lies entirely on one side of the
        // common gutter, still belongs to that column — e.g. the short final
        // line of a taller left column ("… pour les vols" / "intercontinentaux.")
        // whose baseline coincides with no right-column line. Testing only the
        // y extent against `col_top` sent every such line to `bottom_full`, so
        // it was rendered *after* the whole right column, jumping to the end of
        // the page instead of staying inside its own paragraph.
        if y <= col_top + y_tol && y >= col_bottom - y_tol {
            let x0 = l.iter().map(|s| s.x).fold(f64::INFINITY, f64::min);
            let x1 = l.iter().map(|s| s.x + s.advance).fold(f64::NEG_INFINITY, f64::max);
            if x1 <= med {
                left.push((y, l.clone()));
                continue;
            }
            if x0 >= med {
                right.push((y, l.clone()));
                continue;
            }
        }
        if y > col_top + y_tol {
            top_full.push(l.clone());
        } else {
            bottom_full.push(l.clone());
        }
    }
    top_full.sort_by(|a, b| {
        b[0].y
            .partial_cmp(&a[0].y)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    bottom_full.sort_by(|a, b| {
        b[0].y
            .partial_cmp(&a[0].y)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    // Prose gate: a true text column is made of multi-word lines on both
    // sides where the words are spaced like a sentence. Pipe-table rows have
    // short tokens and wide cell gutters inside the "line", so they must keep
    // flowing through the table detector.
    let wc = |rows: &[(f64, Vec<Span>)]| -> f64 {
        if rows.is_empty() {
            return 0.0;
        }
        rows.iter()
            .map(|(_, v)| v.iter().filter(|sp| !sp.text.trim().is_empty()).count() as f64)
            .sum::<f64>()
            / rows.len() as f64
    };
    if wc(&left) < 2.5 || wc(&right) < 2.5 {
        return None;
    }
    // No half may contain a column-wide gutter inside it (that would mean the
    // "column" still holds multiple table cells).
    let clean = |rows: &[(f64, Vec<Span>)]| -> bool {
        rows.iter().all(|(_, v)| {
            let size = v.iter().map(|x| x.size).fold(0.0f64, f64::max).max(0.1);
            v.windows(2).all(|p| {
                let a_end = p[0].x + p[0].advance;
                let gap = (p[1].x - a_end).max(0.0);
                gap <= 1.2 * size
            })
        })
    };
    if !clean(&left) || !clean(&right) {
        return None;
    }
    left.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    right.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    Some(PageColumns {
        top_full,
        left: left.into_iter().map(|(_, v)| v).collect(),
        right: right.into_iter().map(|(_, v)| v).collect(),
        bottom_full,
    })
}

/// Detect a genuine two-column page and produce reading-order streams:
/// full-width rows above the column block, left column top-to-bottom, right
/// column top-to-bottom, full-width rows below. Returns None when the page is
/// not convincingly two-column.
pub fn page_two_columns(lines: &[Vec<Span>]) -> Option<PageColumns> {
    if let Some(pc) = page_two_columns_rows(lines) {
        return Some(pc);
    }
    detect_projection_two_columns(lines)
}

/// Whether `pc` is safe to read as two independent prose columns.
///
/// `page_two_columns_rows` already requires both halves to be a single clean
/// column; the vertical-projection fallback has no such gate, because it must
/// also serve pages whose whole line is a single `Tj` run (one span per visual
/// line, so an average-word gate would reject every genuine column). That
/// leaves it free to mistake the gap in front of an invoice's right-aligned
/// value column for the page gutter: the "left half" of its split then still
/// holds several table cells (label, description, quantity...) separated by
/// column-wide internal gutters, and the "right half" holds the amounts.
/// Reading the page in two streams emits every amount after the entire left
/// stream, so the `blocks` channel jumps a row's total to the end of the page
/// instead of keeping it beside its label (the target fixture's `275,00` and
/// `Teilzahlung`). A half that is itself a grid of cells is not a prose column;
/// apply the very rule `page_two_columns_rows` uses.
///
/// This decision is deliberately kept out of `page_two_columns` itself: the
/// glyph-stream table filter uses a detected two-column *prose block* to drop
/// 2-column table hits that are really prose noise, and must keep doing so even
/// when the same page is read linearly here.
pub(super) fn columns_are_viable_prose(pc: &PageColumns) -> bool {
    rows_are_clean(&pc.left) && rows_are_clean(&pc.right)
}
