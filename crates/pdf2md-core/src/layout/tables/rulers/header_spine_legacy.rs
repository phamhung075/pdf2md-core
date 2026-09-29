// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! The header-spine pass.
//!
//! This is the previous release's two-line-header logic, moved verbatim, plus a
//! single-line branch for a header row that sits directly above its value rows.
//! [`super::header_spine::header_spine_refine`] recovers the header group above
//! the seed and calls this; the caller keeps its own grid whenever this
//! declines.

use super::header_spine::{
    cells_are_labels, row_cells, SPINE_MAX_EXTEND, SPINE_MAX_LOOKAHEAD, SPINE_MAX_WINDOW_ROWS,
    SPINE_MIN_CELLS, SPINE_MIN_NUMERIC_COLS, SPINE_ROW_GAP_PITCH_MULT, SUB_HEADER_SLACK_PT,
};
use super::*;
use crate::layout::tables::consolidation::assign_columns;

/// Whether `sub` refines `spine`: a shorter second line whose every cell falls
/// inside one of the spine's cells (a vertical continuation of some column
/// labels). A second header line labels a subset of the columns; a genuine
/// data row fills as many cells as there are columns and is rejected by the
/// `sub.len() < spine.len()` gate even when its values are digitless and
/// narrow.
fn sub_header_fits(spine: &[(f64, f64, String)], sub: &[(f64, f64, String)], tol: f64) -> bool {
    !sub.is_empty()
        && sub.len() < spine.len()
        && sub
            .iter()
            .all(|(s0, s1, _)| spine.iter().any(|(p0, p1, _)| *s0 >= p0 - tol && *s1 <= p1 + tol))
}

/// Derive the columns of the window `band[win_lo..=hi]` from its header spine
/// and extend `hi` over the single-first-column continuation lines that follow.
/// Returns the spine rulers when the pattern applies, otherwise `None` (the
/// caller keeps its own rulers).
///
/// The window opens on the header row. When the row below is a shorter,
/// contained label line it is the header's wrapped second line; otherwise it is
/// the first value row and the header is a single line. The extension keeps a
/// single-first-column line only while a later multi-column row still lies
/// within the row pitch, so a trailing paragraph whose lines happen to start at
/// the table's left edge is not annexed.
#[allow(clippy::too_many_arguments)] // the pass geometry is threaded verbatim
pub(super) fn legacy_spine_refine(
    info: &[RowInfo],
    lines: &[Vec<Span>],
    band: &[usize],
    win_lo: usize,
    hi: &mut usize,
    tol: f64,
    min_gutter: f64,
    generic_first_cells: Option<usize>,
    wide_grow: bool,
) -> Option<Vec<f64>> {
    if win_lo + 1 > *hi || win_lo >= band.len() || *hi - win_lo >= SPINE_MAX_WINDOW_ROWS {
        return None;
    }
    let spine = row_cells(&info[band[win_lo]].words, min_gutter);
    if spine.len() < SPINE_MIN_CELLS || !cells_are_labels(&spine) {
        return None;
    }
    let rulers: Vec<f64> = spine.iter().map(|c| c.0).collect();

    // Two-line header: the next row wraps a subset of the header cells.
    // Single-line header: the next row is a value row, which must actually look
    // like one (two populated columns, one carrying a digit) so a prose or
    // address line is not mistaken for the first table row.
    let sub = row_cells(&info[band[win_lo + 1]].words, min_gutter);
    let two_line = cells_are_labels(&sub) && sub_header_fits(&spine, &sub, tol + SUB_HEADER_SLACK_PT);
    if cells_are_labels(&sub) && !two_line {
        return None;
    }
    if !two_line {
        // The generic grid must already be tabular *and* its header row must
        // have fewer cells than this header line: that is exactly the merged
        // header the single-line branch un-merges. A header above prose (the
        // generic pass rejected it) or one the generic pass already read in
        // full is left alone.
        if generic_first_cells.is_none_or(|n| n >= spine.len()) {
            return None;
        }
        // A column label starts with a letter. A cell that starts with a
        // currency or percent sign (`€ HT`, `%TVA`) is a unit/format token; a
        // line of those is a value band or a wrapped header, not a label spine.
        if spine
            .iter()
            .any(|(_, _, t)| !t.trim_start().chars().next().is_some_and(|c| c.is_alphabetic()))
        {
            return None;
        }
        let sub_ri = band[win_lo + 1];
        let populated: std::collections::BTreeSet<usize> =
            assign_columns(info, sub_ri, &rulers).into_iter().collect();
        let has_digit = info[sub_ri]
            .words
            .iter()
            .any(|w| w.text.chars().any(|c| c.is_ascii_digit()));
        if populated.len() < 2 || !has_digit {
            return None;
        }
    }
    let header_rows = if two_line { 2 } else { 1 };

    // Grow through the following rows while they stay within the row pitch and
    // fit the spine's columns: a multi-column data row, or a single line whose
    // words all fall in the first (text) column. The extension is provisional
    // until the numeric-column gate below accepts the spine.
    let mut accepted: Vec<usize> = Vec::new();
    let mut last_data: Option<usize> = None;
    let start = *hi + 1;
    let end = band.len().min(start + SPINE_MAX_LOOKAHEAD);
    for k in start..end {
        if accepted.len() >= SPINE_MAX_EXTEND {
            break;
        }
        let prev = band[*accepted.last().unwrap_or(&(*hi))];
        let cur = band[k];
        let gap = lines[prev][0].y - lines[cur][0].y;
        // The header-group path bridges the extra leading a line-item table
        // leaves before its next row; the plain two-line path keeps the
        // previous release's tighter pitch so its grids are unchanged.
        let pitch_mult = if wide_grow { SPINE_ROW_GAP_PITCH_MULT } else { 2.2 };
        let pitch = pitch_mult * info[prev].size.max(info[cur].size).max(1.0);
        if !(gap > 0.0 && gap <= pitch) {
            break;
        }
        let populated: std::collections::BTreeSet<usize> =
            assign_columns(info, cur, &rulers).into_iter().collect();
        if populated.len() >= 2 {
            // A new row anchors on column 0 or carries two or more value
            // columns; a row starting to the right with a single amount is page
            // furniture (the indented total line below a line-item table), not
            // a row of it.
            let numeric_cols = assign_columns(info, cur, &rulers)
                .into_iter()
                .zip(info[cur].words.iter())
                .filter(|(_, w)| w.text.chars().any(|c| c.is_ascii_digit()))
                .map(|(c, _)| c)
                .collect::<std::collections::BTreeSet<_>>();
            if wide_grow && !populated.contains(&0) && numeric_cols.len() < 2 {
                break;
            }
            last_data = Some(accepted.len());
            accepted.push(k);
        } else if populated.len() == 1 && populated.contains(&0) {
            accepted.push(k);
        } else {
            break;
        }
    }
    // Only the continuation lines that precede a real data row belong to a tall
    // first cell; a trailing single-column line is a wrapped paragraph.
    let keep = match last_data {
        Some(i) => i + 1,
        None => 0,
    };
    accepted.truncate(keep);
    let new_hi = accepted.last().copied().unwrap_or(*hi);

    // A real value grid has at least two value columns in its data rows. A
    // side-by-side address/form block — short text cells with no numeric column
    // — is not a table and must keep the plain renderer.
    if win_lo + header_rows > new_hi {
        return None;
    }
    let data_rows = band[win_lo + header_rows..=new_hi].iter().copied().filter(|&k| {
        assign_columns(info, k, &rulers)
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            >= 2
    });
    let mut numeric_cols: std::collections::BTreeSet<usize> = std::collections::BTreeSet::new();
    for k in data_rows {
        for (wi, col) in assign_columns(info, k, &rulers).into_iter().enumerate() {
            if info[k].words[wi].text.chars().any(|c| c.is_ascii_digit()) {
                numeric_cols.insert(col);
            }
        }
    }
    if numeric_cols.len() < SPINE_MIN_NUMERIC_COLS {
        return None;
    }
    t(&format!(
        "  SPINE win_lo={} (line {}) cells={:?} numeric_cols={}",
        win_lo, band[win_lo], rulers, numeric_cols.len()
    ));
    *hi = new_hi;
    Some(rulers)
}
