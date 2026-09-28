// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Reading order recovery, multi-column stream separation, and structured DocBlock generation.

use super::*;

/// Minimum width of the white corridor between two side-by-side text zones, in
/// em of the region's body size.
///
/// A word space is ~0.25 em and a raised/justified gap can reach ~0.5 em, so a
/// corridor has to be several times a word space to be real column white. The
/// motivating block (a bilingual French/English footer, 8.2 pt body) leaves
/// 27.5 pt between the facing columns — 3.35 em, comfortably above this floor,
/// which sits above the word-space/justification band so ordinary word gaps
/// never seed a corridor.
const ZONE_MIN_CORRIDOR_EM: f64 = 1.5;

/// Minimum number of consecutive rows a corridor must cross to be a zone pair.
///
/// The running-gutter pass needs three *confirmed* splits and the vertical
/// projection needs six clear rows, so a two- or three-row block beside another
/// falls through both and is woven row by row. Two rows is the smallest block
/// whose reading order can differ from the linear one, so it is the floor here.
const ZONE_MIN_ROWS: usize = 2;

/// Maximum raggedness of each side's left edge, in em of body size.
///
/// A real column is left-aligned (its rows start at the same margin), so a
/// genuine pair spans a few points; a recurring gap inside one flowing
/// paragraph makes the "right side" start jump with the sentence and is
/// rejected. The measured false positives (a utility-bill line whose recurring
/// gap moves with the sentence) spread 100-360 pt against 0 pt for the
/// bilingual footer.
const ZONE_MAX_EDGE_SPREAD_EM: f64 = 1.0;

/// Minimum average whitespace-separated words per row on each side. Below this
/// a side is a column of short cells (a value column, a table half), not text.
const ZONE_MIN_WORDS_PER_ROW: f64 = 2.5;

/// Maximum baseline gap between two consecutive rows of one zone run, in em of
/// the lower row's size. A genuine block's rows keep a paragraph-like line
/// pitch; two independent form fields that merely share a corridor sit far
/// apart (measured: a 5.4 em gap between two label/value fields on a
/// certificate, against ~0.9 em line pitch inside the bilingual footer). This
/// mirrors `detect_column_bands`'s existing `2.5 * size` adjacency rule.
const ZONE_MAX_ROW_GAP_EM: f64 = 2.5;

/// Fraction of a side's tokens that may be digit-only before it reads as a
/// value column rather than prose. A left label beside a right amount is the
/// classic false positive; the amounts alone exceed a third of the right side.
const ZONE_MAX_NUMERIC_FRACTION: f64 = 0.34;

/// Candidate gutter midpoints closer than this fraction of body size are the
/// same corridor: 3 pt at a typical 8-10 pt body is ~0.35 em.
const ZONE_GUTTER_DEDUP_EM: f64 = 0.35;

/// Whether `row` leaves one white corridor that covers `gx`, returning its
/// `(left_end, right_start)` bounds.
///
/// `None` when a non-whitespace span straddles `gx` (the row has ink there) or
/// when either side is empty — the row itself is not a crossing row.
fn corridor_at(row: &[Span], gx: f64) -> Option<(f64, f64)> {
    let mut left_end = f64::NEG_INFINITY;
    let mut right_start = f64::INFINITY;
    for s in row {
        if s.text.chars().all(|c| c == ' ') {
            // A space glyph is not ink: let it sit in the corridor.
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
    if left_end.is_finite() && right_start.is_finite() && left_end < right_start {
        Some((left_end, right_start))
    } else {
        None
    }
}

/// Partition one crossing row into its left and right sides at `gx`. The
/// corridor was proved ink-free, so every non-space span falls wholly on one
/// side; whitespace follows its centre.
fn partition_at(row: &[Span], gx: f64) -> (Vec<Span>, Vec<Span>) {
    row.iter().cloned().partition(|s| {
        if s.text.chars().all(|c| c == ' ') {
            s.x + 0.5 * s.advance < gx
        } else {
            s.x + s.advance <= gx
        }
    })
}

/// Fraction of `rows`'s whitespace-separated tokens that are numeric (carry a
/// digit and no letter). A prose column sits near zero; an amount column near
/// one.
fn numeric_token_fraction(rows: &[Vec<Span>]) -> f64 {
    let mut total = 0usize;
    let mut numeric = 0usize;
    for row in rows {
        let text: String = row.iter().map(|s| s.text.as_str()).collect();
        for token in text.split_whitespace() {
            total += 1;
            if token.chars().any(|c| c.is_ascii_digit()) && !token.chars().any(|c| c.is_alphabetic())
            {
                numeric += 1;
            }
        }
    }
    if total == 0 {
        0.0
    } else {
        numeric as f64 / total as f64
    }
}

/// Whether a side `rows` reads as table cells rather than one text column:
/// an internal wide gap inside a row, an aligned internal gutter, or a
/// numeric-dominated vocabulary.
fn side_looks_like_cells(rows: &[Vec<Span>]) -> bool {
    !rows_are_clean(rows)
        || has_aligned_internal_gutter(rows)
        || numeric_token_fraction(rows) > ZONE_MAX_NUMERIC_FRACTION
}

/// Validate one candidate run and split it into its two side streams.
fn zones_from_run(
    rows: &[Vec<Span>],
    start: usize,
    end: usize,
    gx: f64,
    body: f64,
) -> Option<(usize, usize, Vec<Vec<Span>>, Vec<Vec<Span>>)> {
    if end - start + 1 < ZONE_MIN_ROWS {
        return None;
    }
    let mut left: Vec<Vec<Span>> = Vec::new();
    let mut right: Vec<Vec<Span>> = Vec::new();
    for row in &rows[start..=end] {
        let (l, r) = partition_at(row, gx);
        if l.is_empty() || r.is_empty() {
            return None;
        }
        left.push(l);
        right.push(r);
    }
    // Geometry: a real zone pair keeps both facing edges put across the run.
    if column_start_spread(&left) > ZONE_MAX_EDGE_SPREAD_EM * body
        || column_start_spread(&right) > ZONE_MAX_EDGE_SPREAD_EM * body
    {
        return None;
    }
    // Cell guards: either side being a grid or a numeric column is a table row.
    if side_looks_like_cells(&left) || side_looks_like_cells(&right) {
        return None;
    }
    // Running text, not independent label/value pairs: at least one side must
    // continue a sentence across its rows.
    if !wrapped_prose(&left) && !wrapped_prose(&right) {
        return None;
    }
    if words_per_row(&left) < ZONE_MIN_WORDS_PER_ROW || words_per_row(&right) < ZONE_MIN_WORDS_PER_ROW
    {
        return None;
    }
    Some((start, end, left, right))
}

/// Recover a short run of rows that is two independent side-by-side text zones
/// separated by one vertical white corridor, returning
/// `(start_index, end_index, left_rows, right_rows)`.
///
/// This is the geometric counterpart to [`projection_columns_region`]: instead
/// of a prose/besides-grid gate that needs six clear rows, it accepts a
/// contiguous run of [`ZONE_MIN_ROWS`] or more rows when a single corridor at
/// one x crosses *every* row of the run with [`ZONE_MIN_CORRIDOR_EM`] of white
/// and each side's rows share a left edge. Table geometry is rejected by the
/// cell guards in [`zones_from_run`]. The longest run wins; ties keep the
/// leftmost gutter (the page gutter rather than an internal grid one).
pub(super) fn side_by_side_zones(
    rows: &[Vec<Span>],
) -> Option<(usize, usize, Vec<Vec<Span>>, Vec<Vec<Span>>)> {
    if rows.len() < ZONE_MIN_ROWS {
        return None;
    }
    let body = body_size_for(rows).max(1.0);
    let min_corridor = ZONE_MIN_CORRIDOR_EM * body;

    // Candidate gutters: the midpoint of every inter-span gap already wide
    // enough to be a corridor on its own row. Word spaces never seed one.
    let mut cands: Vec<f64> = Vec::new();
    for row in rows {
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
            let Some((le, rs)) = corridor_at(&rows[i], gx) else {
                i += 1;
                continue;
            };
            if rs - le < min_corridor {
                i += 1;
                continue;
            }
            let start = i;
            // Extend the run while the next row stays vertically adjacent and
            // keeps a corridor that wide at gx.
            let mut common_le = le;
            let mut common_rs = rs;
            let mut prev_y = rows[i][0].y;
            i += 1;
            while i < rows.len() {
                if rows[i].is_empty() {
                    break;
                }
                let gap = prev_y - rows[i][0].y;
                let row_size = rows[i][0].size.max(0.1);
                if !(gap >= -1.0 && gap <= ZONE_MAX_ROW_GAP_EM * row_size) {
                    break;
                }
                match corridor_at(&rows[i], gx) {
                    Some((l, r)) if r - l >= min_corridor => {
                        common_le = common_le.max(l);
                        common_rs = common_rs.min(r);
                        prev_y = rows[i][0].y;
                        i += 1;
                    }
                    _ => break,
                }
            }
            let end = i - 1;
            // One *common* corridor must cover the whole run, not just gx.
            if common_rs - common_le < min_corridor {
                continue;
            }
            if let Some(region) = zones_from_run(rows, start, end, gx, body) {
                let better = best
                    .as_ref()
                    .map_or(true, |b| region.1 - region.0 + 1 > b.1 - b.0 + 1);
                if better {
                    best = Some(region);
                }
            }
        }
    }
    best
}
