// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Prose-sweep veto: reject grids that are a page's flowing text columns cut
//! at both their left and right text edges.
//!
//! When the ruler scan misreads a multi-column prose page as a grid, every
//! real text column becomes a dense sentence-length column and the white
//! gutter beside it becomes a sparse filler column shared by only a few rows.
//! A genuine table, by contrast, pairs at most one sentence-length text column
//! with short value columns and has no all-but-empty filler column. The same
//! cut also leaves the paragraph's line-break hyphens inside the cells.

/// A grid column filled in fewer than this fraction of the grid's rows is an
/// alignment gutter (the space beside a real text column), not a data column.
const SPARSE_COL_FILL_RATIO: f64 = 0.5;

/// Minimum filled cells before a column's average length is meaningful; a
/// two-cell column is too small to characterize as prose.
const PROSE_COL_MIN_CELLS: usize = 3;

/// Columns averaging this many whitespace tokens per cell read as prose
/// sentences rather than as data labels/values. A wrapped prose column of
/// sentence fragments averages ~5-9 tokens; a tabular description column,
/// which a real table pairs with short value columns, is excluded by the
/// sparse-filler requirement.
const PROSE_COL_MEAN_TOKENS: f64 = 5.0;

/// Two independent sentence-length columns beside a filler column is the
/// signature of a page's two prose columns split at both text edges; a real
/// table keeps at most one such column beside short data columns.
const PROSE_COLS_FOR_SWEEP: usize = 2;

/// Fraction of non-empty cells that may break off with a line-end hyphen
/// before the grid reads as wrapped running text. Real table cells are short
/// and whole; a swept-in prose column carries the line-break hyphens of the
/// paragraph it was cut from.
const HYPHEN_FRAGMENT_RATIO: f64 = 0.25;

/// Whether `rows` (with `non_empty[k]` the filled-cell count of column `k`)
/// is a page's prose columns swept into a grid rather than a data table.
pub(crate) fn is_prose_sweep(rows: &[&Vec<String>], non_empty: &[usize]) -> bool {
    let row_total = rows.len();
    if row_total == 0 {
        return false;
    }
    let mut sparse_cols = 0usize;
    let mut prose_cols = 0usize;
    let mut filled_cells = 0usize;
    let mut hyphen_cells = 0usize;
    for (k, &ne) in non_empty.iter().enumerate() {
        if ne == 0 {
            continue;
        }
        if (ne as f64) < row_total as f64 * SPARSE_COL_FILL_RATIO {
            sparse_cols += 1;
        }
        let mut col_tokens = 0usize;
        let mut col_cells = 0usize;
        for r in rows {
            let c = r.get(k).map_or("", |c| c.as_str()).trim();
            if c.is_empty() {
                continue;
            }
            col_cells += 1;
            col_tokens += c.split_whitespace().count();
            filled_cells += 1;
            if c.ends_with('-') {
                hyphen_cells += 1;
            }
        }
        if col_cells >= PROSE_COL_MIN_CELLS
            && col_tokens as f64 / col_cells as f64 >= PROSE_COL_MEAN_TOKENS
        {
            prose_cols += 1;
        }
    }
    let hyphen_ratio = if filled_cells == 0 {
        0.0
    } else {
        hyphen_cells as f64 / filled_cells as f64
    };
    sparse_cols >= 1 && (prose_cols >= PROSE_COLS_FOR_SWEEP || hyphen_ratio >= HYPHEN_FRAGMENT_RATIO)
}
