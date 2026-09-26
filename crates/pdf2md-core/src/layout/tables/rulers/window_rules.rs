// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Window-level veto predicates for the strict ruler grid scan.

use super::*;

/// Gap below which two adjacent populated cells read as one wrapped/flowing
/// cell rather than two tabular columns (in units of the row's font size).
const FLOWING_GAP_RATIO: f64 = 0.65;

/// Rows whose bucket layout holds at least two non-empty cells.
pub(super) fn count_multi_col_rows(info: &[RowInfo], win_rows: &[usize], rulers: &[f64]) -> usize {
    win_rows
        .iter()
        .filter(|&&i| {
            let row_cells = bucket(info, i, rulers);
            row_cells.iter().filter(|c| !c.trim().is_empty()).count() >= 2
        })
        .count()
}

/// Rows carrying at least one tight adjacent cell pair (a wrapped/flowing row).
pub(super) fn count_flowing_rows(info: &[RowInfo], win_rows: &[usize], rulers: &[f64]) -> usize {
    win_rows
        .iter()
        .filter(|&&ri| {
            let cells = bucket_words(info, ri, rulers);
            let size = info[ri].size;
            for c in 0..cells.len().saturating_sub(1) {
                if !cells[c].is_empty() && !cells[c + 1].is_empty() {
                    let last_w = cells[c].last().unwrap();
                    let first_next_w = cells[c + 1].first().unwrap();
                    let gap = first_next_w.x0 - last_w.x1;
                    if gap < FLOWING_GAP_RATIO * size {
                        return true;
                    }
                }
            }
            false
        })
        .count()
}

/// True when a majority of measured column boundaries are separated by a wide
/// gutter across the window (a genuine tabular grid even if some rows flow).
pub(super) fn has_wide_column_majority(
    info: &[RowInfo],
    win_rows: &[usize],
    rulers: &[f64],
) -> bool {
    if rulers.len() < 3 {
        return false;
    }
    let mut wide_boundaries = 0usize;
    let mut measured_boundaries = 0usize;
    for c in 0..rulers.len().saturating_sub(1) {
        let mut wide = 0usize;
        let mut total = 0usize;
        for &ri in win_rows {
            let cells = bucket_words(info, ri, rulers);
            if c + 1 < cells.len() && !cells[c].is_empty() && !cells[c + 1].is_empty() {
                let gap = cells[c + 1].first().unwrap().x0 - cells[c].last().unwrap().x1;
                total += 1;
                if gap >= FLOWING_GAP_RATIO * info[ri].size {
                    wide += 1;
                }
            }
        }
        if total > 0 {
            measured_boundaries += 1;
            if wide * 2 >= total {
                wide_boundaries += 1;
            }
        }
    }
    measured_boundaries > 0 && wide_boundaries * 2 >= measured_boundaries
}
