// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Staggered side-by-side fallback for [`super::zones::side_by_side_zones`]:
//! two independent, unwrapped blocks whose columns do not share every baseline.

use super::zones::{
    partition_at, side_looks_like_cells, ZONE_GUTTER_DEDUP_EM, ZONE_MAX_EDGE_SPREAD_EM,
    ZONE_MAX_ROW_GAP_EM, ZONE_MIN_CORRIDOR_EM, ZONE_MIN_ROWS, ZONE_MIN_WORDS_PER_ROW,
};
use super::*;

/// Minimum lines on *both* sides for an unwrapped zone. A two-line side facing a
/// longer run is the shape of a repeated legal footer (a company line beside its
/// address) that the plain order already reads correctly; requiring three keeps
/// genuine short address blocks (which are at least three lines) while leaving
/// such footers alone.
const ZONE_MIN_UNWRAPPED_LINES: usize = 3;
/// Maximum lines on either side for an unwrapped zone: a longer unwrapped run is
/// a multi-column page the projection path owns.
const ZONE_MAX_UNWRAPPED_LINES: usize = 6;
/// Minimum rows where both sides carry content. A staggered pair must actually
/// face the gutter on at least this many rows.
const ZONE_MIN_BOTH_ROWS: usize = 2;
/// Maximum rows where both sides carry content for an unwrapped zone. A pair
/// facing the gutter on nearly every row is a form/table the plain order already
/// reads correctly; only a strongly staggered pair is re-banded.
const ZONE_MAX_UNWRAPPED_BOTH_ROWS: usize = 2;

/// Whether a row carries any visible (non-whitespace) text.
fn has_visible(row: &[Span]) -> bool {
    row.iter().any(|s| !s.text.trim().is_empty())
}

/// The `(left_end, right_start)` bounds a row leaves at `gx`, where a bound is
/// infinite when that side of the row is empty. `None` when a non-whitespace
/// span straddles `gx`.
fn corridor_bounds(row: &[Span], gx: f64) -> Option<(f64, f64)> {
    let mut left_end = f64::NEG_INFINITY;
    let mut right_start = f64::INFINITY;
    for s in row {
        if s.text.chars().all(|c| c == ' ') {
            continue;
        }
        let end = s.x + s.advance;
        if end <= gx {
            left_end = left_end.max(end);
        } else if s.x >= gx {
            right_start = right_start.min(s.x);
        } else {
            return None;
        }
    }
    Some((left_end, right_start))
}

/// Is `next` vertically adjacent to `prev` (a paragraph-like line pitch)?
fn zone_adjacent(prev: &[Span], next: &[Span]) -> bool {
    if prev.is_empty() || next.is_empty() {
        return false;
    }
    let gap = prev[0].y - next[0].y;
    let row_size = next[0].size.max(0.1);
    gap >= -1.0 && gap <= ZONE_MAX_ROW_GAP_EM * row_size
}

/// Grow a corridor run upward from `seed` while the rows stay adjacent and no
/// word crosses `gx`.
fn grow_zone_start(rows: &[Vec<Span>], seed: usize, gx: f64) -> usize {
    let mut start = seed;
    while start > 0
        && zone_adjacent(&rows[start - 1], &rows[start])
        && corridor_bounds(&rows[start - 1], gx).is_some()
    {
        start -= 1;
    }
    start
}

/// Grow a corridor run downward from `seed` while the rows stay adjacent and no
/// word crosses `gx`.
fn grow_zone_end(rows: &[Vec<Span>], seed: usize, gx: f64) -> usize {
    let mut end = seed;
    while end + 1 < rows.len()
        && zone_adjacent(&rows[end], &rows[end + 1])
        && corridor_bounds(&rows[end + 1], gx).is_some()
    {
        end += 1;
    }
    end
}

/// Fallback for a *staggered* pair of independent, unwrapped blocks: the two
/// columns do not share every baseline, so the strict pass finds no run. Try
/// every span edge and every wide inter-span gap as a gutter, grow the maximal
/// run that no word crosses, and keep only a strongly staggered, short,
/// sentence-initial pair. A wrapping side belongs to the projection path.
pub(super) fn extended_side_by_side_zones(
    rows: &[Vec<Span>],
) -> Option<(usize, usize, Vec<Vec<Span>>, Vec<Vec<Span>>)> {
    if rows.len() < ZONE_MIN_ROWS {
        return None;
    }
    let body = body_size_for(rows).max(1.0);
    let min_corridor = ZONE_MIN_CORRIDOR_EM * body;

    let mut cands: Vec<f64> = Vec::new();
    for row in rows {
        for s in row {
            if !s.text.trim().is_empty() {
                cands.push(s.x);
                cands.push(s.x + s.advance);
            }
        }
        for w in row.windows(2) {
            let a_end = w[0].x + w[0].advance;
            let gap = w[1].x - a_end;
            let size = w[0].size.max(w[1].size).max(0.1);
            if gap >= ZONE_MIN_CORRIDOR_EM * size {
                cands.push(0.5 * (a_end + w[1].x));
            }
        }
    }
    cands.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    cands.dedup_by(|a, b| (*a - *b).abs() < ZONE_GUTTER_DEDUP_EM * body);

    let mut best: Option<(usize, usize, Vec<Vec<Span>>, Vec<Vec<Span>>)> = None;
    for &gx in &cands {
        let mut i = 0;
        while i < rows.len() {
            if corridor_bounds(&rows[i], gx).is_none() {
                i += 1;
                continue;
            }
            let start = grow_zone_start(rows, i, gx);
            let end = grow_zone_end(rows, i, gx);
            if let Some(region) = extended_zones_from_run(rows, start, end, gx, body, min_corridor) {
                let better = best
                    .as_ref()
                    .map_or(true, |b| region.1 - region.0 + 1 > b.1 - b.0 + 1);
                if better {
                    best = Some(region);
                }
            }
            i = end + 1;
        }
    }
    best
}

/// Validate one staggered run and split it into its two side streams. The run
/// may hold one-sided rows; whitespace-only rows are dropped.
fn extended_zones_from_run(
    rows: &[Vec<Span>],
    start: usize,
    end: usize,
    gx: f64,
    body: f64,
    min_corridor: f64,
) -> Option<(usize, usize, Vec<Vec<Span>>, Vec<Vec<Span>>)> {
    if end < start {
        return None;
    }
    let mut left: Vec<Vec<Span>> = Vec::new();
    let mut right: Vec<Vec<Span>> = Vec::new();
    let mut left_end_max = f64::NEG_INFINITY;
    let mut right_start_min = f64::INFINITY;
    let mut both_rows = 0usize;
    let mut one_sided = false;
    for row in &rows[start..=end] {
        let (le, rs) = corridor_bounds(row, gx)?;
        let (l, r) = partition_at(row, gx);
        let (lv, rv) = (has_visible(&l), has_visible(&r));
        if !lv && !rv {
            continue;
        }
        if lv != rv {
            one_sided = true;
        }
        if lv {
            left_end_max = left_end_max.max(le);
            left.push(l);
        }
        if rv {
            right_start_min = right_start_min.min(rs);
            right.push(r);
        }
        if lv && rv {
            both_rows += 1;
            if rs - le < min_corridor {
                return None;
            }
        }
    }
    if left.len() < ZONE_MIN_ROWS || right.len() < ZONE_MIN_ROWS {
        return None;
    }
    // This fallback exists for the staggered case only; a run the strict pass
    // could see is its business.
    if !one_sided || both_rows < ZONE_MIN_BOTH_ROWS || both_rows > ZONE_MAX_UNWRAPPED_BOTH_ROWS {
        return None;
    }
    if right_start_min - left_end_max < min_corridor {
        return None;
    }
    if column_start_spread(&left) > ZONE_MAX_EDGE_SPREAD_EM * body
        || column_start_spread(&right) > ZONE_MAX_EDGE_SPREAD_EM * body
    {
        return None;
    }
    if side_looks_like_cells(&left) || side_looks_like_cells(&right) {
        return None;
    }
    // Only unwrapped, unequal, short, sentence-initial blocks: a wrapping side,
    // a label/value pair (equal line counts) or a mid-sentence fragment is not
    // the staggered address pair this fallback targets.
    if wrapped_prose(&left) || wrapped_prose(&right) {
        return None;
    }
    if left.len() == right.len()
        || left.len() < ZONE_MIN_UNWRAPPED_LINES
        || right.len() < ZONE_MIN_UNWRAPPED_LINES
        || left.len() > ZONE_MAX_UNWRAPPED_LINES
        || right.len() > ZONE_MAX_UNWRAPPED_LINES
    {
        return None;
    }
    let first_left_starts_lower = left
        .first()
        .and_then(|r| {
            r.iter()
                .filter(|s| !s.text.trim().is_empty())
                .flat_map(|s| s.text.trim().chars())
                .next()
        })
        .map(|c| c.is_lowercase())
        .unwrap_or(true);
    if first_left_starts_lower {
        return None;
    }
    if words_per_row(&left) < ZONE_MIN_WORDS_PER_ROW || words_per_row(&right) < ZONE_MIN_WORDS_PER_ROW
    {
        return None;
    }
    Some((start, end, left, right))
}
