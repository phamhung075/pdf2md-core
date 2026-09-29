// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Band repair: absorbs one-sided rows into a column band and merges column bands that share one gutter.

use super::*;

/// Maximum baseline gap between a column band and an adjacent full-width row
/// it may absorb, in em of the row's font size. Mirrors the `2.5 * size`
/// adjacency rule the running-gutter pass uses, so a row from an unrelated
/// block (a heading separated by a blank line, a footer) is never pulled into
/// a column.
const REPAIR_MAX_ADJACENT_GAP_EM: f64 = 2.5;

/// Relative tolerance when deciding two column bands share one gutter.
const REPAIR_GUTTER_REL_TOL: f64 = 0.15;

/// Absolute floor (pt) for [`REPAIR_GUTTER_REL_TOL`], so a tiny gutter still
/// has a workable tolerance.
const REPAIR_GUTTER_MIN_TOL_PT: f64 = 6.0;

/// One non-blank row's assignment against an established column gutter.
enum RowSide {
    /// Ink entirely to the left of the gutter.
    Left(Vec<Span>),
    /// Ink entirely to the right of the gutter.
    Right(Vec<Span>),
    /// Ink on both sides: a genuine crossing row, split into its two halves.
    Both(Vec<Span>, Vec<Span>),
}

/// Borrowed left/right row streams of a [`ColumnBand::Columns`] band.
type ColumnStreams<'a> = (&'a Vec<Vec<Span>>, &'a Vec<Vec<Span>>);

fn columns_of(band: &ColumnBand) -> Option<ColumnStreams<'_>> {
    match band {
        ColumnBand::Columns { left, right } => Some((left, right)),
        _ => None,
    }
}

/// Midpoint of the tightest corridor a column band's own rows leave: the
/// rightmost left-column ink and the leftmost right-column ink.
fn column_gutter(left: &[Vec<Span>], right: &[Vec<Span>]) -> Option<f64> {
    let left_end = left
        .iter()
        .flat_map(|r| r.iter())
        .filter(|s| !is_blank_span(s))
        .map(|s| s.x + s.advance)
        .fold(f64::NEG_INFINITY, f64::max);
    let right_start = right
        .iter()
        .flat_map(|r| r.iter())
        .filter(|s| !is_blank_span(s))
        .map(|s| s.x)
        .fold(f64::INFINITY, f64::min);
    if left_end.is_finite() && right_start.is_finite() && left_end < right_start {
        Some(0.5 * (left_end + right_start))
    } else {
        None
    }
}

/// Whether two gutters describe the same column boundary.
fn gutters_match(a: f64, b: f64) -> bool {
    (a - b).abs() <= (REPAIR_GUTTER_REL_TOL * b.abs()).max(REPAIR_GUTTER_MIN_TOL_PT)
}

/// The topmost / bottom-most baseline of a column band.
fn columns_top(band: &ColumnBand) -> Option<f64> {
    let (left, right) = columns_of(band)?;
    left.iter()
        .chain(right.iter())
        .filter_map(|r| r.first().map(|s| s.y))
        .reduce(f64::max)
}

fn columns_bottom(band: &ColumnBand) -> Option<f64> {
    let (left, right) = columns_of(band)?;
    left.iter()
        .chain(right.iter())
        .filter_map(|r| r.first().map(|s| s.y))
        .reduce(f64::min)
}

/// Assign one visual row against the established gutter `gx`.
///
/// `None` means the row carries ink across the gutter (a full-width heading or
/// figure) and cannot be placed in either column. Blank rows are the caller's
/// responsibility: they are skipped, not assigned.
fn assign_row(row: &[Span], gx: f64) -> Option<RowSide> {
    let mut left: Vec<Span> = Vec::new();
    let mut right: Vec<Span> = Vec::new();
    for s in row {
        if is_blank_span(s) {
            // A whitespace glyph is not ink; the renderer re-derives word gaps
            // from the geometry, so a space in the corridor is simply dropped.
            continue;
        }
        let end = s.x + s.advance;
        if end <= gx {
            left.push(s.clone());
        } else if s.x >= gx {
            right.push(s.clone());
        } else {
            // A non-space span straddles the gutter: not a crossing row.
            return None;
        }
    }
    match (left.is_empty(), right.is_empty()) {
        (false, true) => Some(RowSide::Left(left)),
        (true, false) => Some(RowSide::Right(right)),
        (false, false) => Some(RowSide::Both(left, right)),
        (true, true) => None,
    }
}

/// Whether every span of `row` is whitespace (a blank spacer line).
fn row_is_blank(row: &[Span]) -> bool {
    row.iter().all(|s| s.text.trim().is_empty())
}

/// Size (pt) of the row at `index`, defaulted when the row is empty.
fn row_size(rows: &[Vec<Span>], index: usize) -> f64 {
    rows.get(index)
        .and_then(|r| r.first())
        .map(|s| s.size)
        .unwrap_or(10.0)
        .max(0.1)
}

/// Absorb the contiguous rows of `rows` nearest to a column band into that
/// band's streams.
///
/// `from_start` selects which edge is nearest: the band's prefix (a full-width
/// band *below* a column block) or its suffix (a full-width band *above* one).
/// `target_edge_y` is the facing baseline of the column band. Returns the
/// number of leading/trailing rows consumed, which the caller drops from
/// `rows`; a non-zero count means every consumed row was either placed or a
/// blank spacer.
fn absorb(
    target_left: &mut Vec<Vec<Span>>,
    target_right: &mut Vec<Vec<Span>>,
    rows: &[Vec<Span>],
    from_start: bool,
    gx: f64,
    target_edge_y: f64,
) -> usize {
    let order: Vec<usize> = if from_start {
        (0..rows.len()).collect()
    } else {
        (0..rows.len()).rev().collect()
    };
    let mut placed: Vec<(usize, RowSide)> = Vec::new();
    let mut prev_y = target_edge_y;
    let mut eaten = 0usize;
    for i in order {
        let row = &rows[i];
        let Some(first) = row.first() else { break };
        let gap = (prev_y - first.y).abs();
        if gap > REPAIR_MAX_ADJACENT_GAP_EM * row_size(rows, i) {
            break;
        }
        if row_is_blank(row) {
            prev_y = first.y;
            eaten += 1;
            continue;
        }
        match assign_row(row, gx) {
            Some(side) => {
                prev_y = first.y;
                eaten += 1;
                placed.push((i, side));
            }
            None => break,
        }
    }
    // Emit top-to-bottom (ascending row index) regardless of scan direction.
    placed.sort_by_key(|(i, _)| *i);
    for (_, side) in placed {
        match side {
            RowSide::Left(l) => target_left.push(l),
            RowSide::Right(r) => target_right.push(r),
            RowSide::Both(l, r) => {
                target_left.push(l);
                target_right.push(r);
            }
        }
    }
    eaten
}

/// Try to fold a `Full` band's near rows into the column band at `j`.
/// Returns the number of rows consumed.
fn absorb_into_full_target(bands: &mut [ColumnBand], full_index: usize, target: usize) -> usize {
    let into_next = target > full_index;
    let (gx, edge_y) = {
        let Some((left, right)) = columns_of(&bands[target]) else {
            return 0;
        };
        let Some(gx) = column_gutter(left, right) else {
            return 0;
        };
        let edge = if into_next {
            columns_top(&bands[target])
        } else {
            columns_bottom(&bands[target])
        };
        (gx, edge.unwrap_or(0.0))
    };
    let full = match &bands[full_index] {
        ColumnBand::Full(rows) => rows.clone(),
        _ => return 0,
    };
    if full.is_empty() {
        return 0;
    }
    let mut left_out: Vec<Vec<Span>> = Vec::new();
    let mut right_out: Vec<Vec<Span>> = Vec::new();
    let consumed = absorb(
        &mut left_out,
        &mut right_out,
        &full,
        !into_next,
        gx,
        edge_y,
    );
    if consumed == 0 {
        return 0;
    }
    if let ColumnBand::Columns { left, right } = &mut bands[target] {
        left.extend(left_out);
        right.extend(right_out);
        // Keep each stream top-to-bottom after splicing the absorbed rows in.
        let key = |r: &Vec<Span>| r.first().map(|s| s.y).unwrap_or(0.0);
        left.sort_by(|a, b| key(b).partial_cmp(&key(a)).unwrap_or(std::cmp::Ordering::Equal));
        right.sort_by(|a, b| key(b).partial_cmp(&key(a)).unwrap_or(std::cmp::Ordering::Equal));
    }
    consumed
}

/// Repair a band sequence whose two-column block the row-local running-gutter
/// pass fragmented.
///
/// The pass that builds `bands` can only absorb a one-sided row once its run
/// has a confirmed gutter, and it never merges the runs it flushes. A real
/// two-column block whose first row is one-sided (the right column's top line,
/// before any crossing row) or which carries a short centered heading whose
/// apparent gutter differs from the block's therefore comes out as several
/// `Columns` bands interleaved with small `Full` bands. Emitting those in
/// sequence reads the block's rows out of order: the leading right-column line
/// jumps ahead of the whole left column.
///
/// This pass re-joins the pieces: a `Full` band vertically adjacent to a
/// `Columns` band is absorbed row by row into the appropriate stream, and two
/// adjacent `Columns` bands sharing one gutter are merged. A row that carries
/// ink across the gutter (a full-width heading or figure) is never absorbed, so
/// an unrelated full-width row still separates the blocks.
pub(super) fn repair_column_bands(mut bands: Vec<ColumnBand>) -> Vec<ColumnBand> {
    let mut changed = true;
    while changed {
        changed = false;
        let mut i = 0;
        while i < bands.len() {
            if !matches!(bands[i], ColumnBand::Full(_)) {
                i += 1;
                continue;
            }
            let mut consumed = 0usize;
            let mut absorbed_suffix = false;
            if i + 1 < bands.len() && columns_of(&bands[i + 1]).is_some() {
                consumed = absorb_into_full_target(&mut bands, i, i + 1);
                absorbed_suffix = consumed > 0;
            }
            if consumed == 0 && i > 0 && columns_of(&bands[i - 1]).is_some() {
                consumed = absorb_into_full_target(&mut bands, i, i - 1);
            }
            if consumed == 0 {
                i += 1;
                continue;
            }
            let remaining = match &bands[i] {
                ColumnBand::Full(rows) => rows.len().saturating_sub(consumed),
                _ => 0,
            };
            if remaining == 0 {
                bands.remove(i);
            } else {
                if let ColumnBand::Full(rows) = &mut bands[i] {
                    // The absorbed rows are the contiguous near edge; keep the
                    // opposite edge as a smaller Full band.
                    if absorbed_suffix {
                        rows.truncate(rows.len() - consumed);
                    } else {
                        rows.drain(0..consumed);
                    }
                }
                i += 1;
            }
            changed = true;
        }

        let mut i = 0;
        while i + 1 < bands.len() {
            let joinable = match (&bands[i], &bands[i + 1]) {
                (
                    ColumnBand::Columns { left: l1, right: r1 },
                    ColumnBand::Columns { left: l2, right: r2 },
                ) => match (column_gutter(l1, r1), column_gutter(l2, r2)) {
                    (Some(a), Some(b)) if gutters_match(a, b) => {
                        let b_size = l2
                            .iter()
                            .chain(r2.iter())
                            .filter_map(|r| r.first().map(|s| s.size))
                            .fold(10.0f64, f64::max);
                        match (columns_bottom(&bands[i]), columns_top(&bands[i + 1])) {
                            (Some(bot), Some(top)) => {
                                (bot - top).abs() <= REPAIR_MAX_ADJACENT_GAP_EM * b_size.max(0.1)
                            }
                            _ => false,
                        }
                    }
                    _ => false,
                },
                _ => false,
            };
            if joinable {
                let next = bands.remove(i + 1);
                if let (
                    ColumnBand::Columns { left: l1, right: r1 },
                    ColumnBand::Columns { left: l2, right: r2 },
                ) = (&mut bands[i], next)
                {
                    l1.extend(l2);
                    r1.extend(r2);
                }
                changed = true;
            } else {
                i += 1;
            }
        }
    }
    bands
}
