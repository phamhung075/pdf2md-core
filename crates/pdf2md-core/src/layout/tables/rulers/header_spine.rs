// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Header-spine column derivation.
//!
//! A grid whose first row is a clean, gutter-separated label row ("header
//! spine") already states its own columns: each cell is one column, and the
//! second header line, when present, refines those labels vertically. Deriving
//! the rulers from all window rows instead lets a tall wrapped first-column
//! cell contribute its interior word-starts as phantom rulers, which shifts
//! every boundary and splits label cells apart, and it drops a header column
//! whose label is the only word starting at its x (the two-line header's first
//! line).
//!
//! This module reconstructs the columns from the spine row when the window
//! opens with a two-line header, and then extends the window over the
//! single-first-column continuation lines that belong to a tall first cell.
//! Activation is deliberately narrow (a label spine plus a contained second
//! header line), so ordinary grids keep the ruler pass unchanged.

use super::*;
use crate::layout::tables::consolidation::assign_columns;

/// Maximum words in one spine cell for it to read as a short column label
/// rather than a prose span.
const SPINE_MAX_CELL_WORDS: usize = 4;
/// Minimum number of cells a row must show to define a table's columns.
const SPINE_MIN_CELLS: usize = 3;
/// Minimum value columns (cells carrying a digit) in the data rows for a spine
/// to be a real table rather than a side-by-side text/address block.
const SPINE_MIN_NUMERIC_COLS: usize = 2;
/// Maximum rows looked ahead when extending over a tall first cell.
const SPINE_MAX_LOOKAHEAD: usize = 24;
/// Maximum rows the window may grow over a tall first cell. A wrapped cell has
/// a handful of continuation lines; a larger growth annexes neighbouring grids.
const SPINE_MAX_EXTEND: usize = 8;
/// Largest seed window the spine pass will re-column. A small summary grid has
/// a genuine two-line header; a tall multi-row grid already clusters stable
/// rulers from its data rows and must keep them.
const SPINE_MAX_WINDOW_ROWS: usize = 8;
/// Alignment slack (in points) when testing that a second-header cell falls
/// inside a first-header cell, on top of the window tolerance.
const SUB_HEADER_SLACK_PT: f64 = 2.0;

/// The cell boundaries of `words`: each cell is a maximal run of words whose
/// consecutive gaps stay below `min_gutter`. Returns `(start, end, text)`.
fn row_cells(words: &[WordTok], min_gutter: f64) -> Vec<(f64, f64, String)> {
    let mut cells: Vec<(f64, f64, String)> = Vec::new();
    for (i, w) in words.iter().enumerate() {
        let new_cell = i == 0 || w.x0 - words[i - 1].x1 >= min_gutter;
        if new_cell {
            cells.push((w.x0, w.x1, w.text.clone()));
        } else if let Some(last) = cells.last_mut() {
            last.1 = w.x1;
            last.2.push(' ');
            last.2.push_str(&w.text);
        }
    }
    cells
}

/// Whether every cell is a short label with no digit — the shape of a header
/// spine row rather than a data row.
fn cells_are_labels(cells: &[(f64, f64, String)]) -> bool {
    cells.iter().all(|(_, _, t)| {
        !t.trim().is_empty()
            && !t.chars().any(|c| c.is_ascii_digit())
            && t.split_whitespace().count() <= SPINE_MAX_CELL_WORDS
    })
}

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
/// and extend `hi` over the single-first-column continuation lines that follow,
/// when the window opens with a two-line header. Returns the spine rulers when
/// the pattern applies, otherwise `None` (the caller keeps its own rulers).
///
/// The extension keeps a single-first-column line only while a later
/// multi-column row still lies within the row pitch, so a trailing paragraph
/// whose lines happen to start at the table's left edge is not annexed.
pub(super) fn header_spine_refine(
    info: &[RowInfo],
    lines: &[Vec<Span>],
    band: &[usize],
    win_lo: usize,
    hi: &mut usize,
    tol: f64,
    min_gutter: f64,
) -> Option<Vec<f64>> {
    if win_lo + 1 > *hi || win_lo >= band.len() || *hi - win_lo >= SPINE_MAX_WINDOW_ROWS {
        return None;
    }
    let spine = row_cells(&info[band[win_lo]].words, min_gutter);
    if spine.len() < SPINE_MIN_CELLS || !cells_are_labels(&spine) {
        return None;
    }
    let sub = row_cells(&info[band[win_lo + 1]].words, min_gutter);
    if !cells_are_labels(&sub) {
        return None;
    }
    let slack = tol + SUB_HEADER_SLACK_PT;
    if !sub_header_fits(&spine, &sub, slack) {
        return None;
    }
    let rulers: Vec<f64> = spine.iter().map(|c| c.0).collect();

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
        let pitch = 2.2 * info[prev].size.max(info[cur].size).max(1.0);
        if !(gap > 0.0 && gap <= pitch) {
            break;
        }
        let populated: std::collections::BTreeSet<usize> =
            assign_columns(info, cur, &rulers).into_iter().collect();
        if populated.len() >= 2 {
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

    // A real two-line-header grid has at least two value columns in its data
    // rows. A side-by-side address/form block — short text cells with no
    // numeric column — is not a table and must keep the plain renderer.
    if win_lo + 2 > new_hi {
        return None;
    }
    let data_rows = band[win_lo + 2..=new_hi]
        .iter()
        .copied()
        .filter(|&k| assign_columns(info, k, &rulers).into_iter().collect::<std::collections::BTreeSet<_>>().len() >= 2);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::tables::rulers::tests_common::*;

    fn line(spans: Vec<Span>) -> Vec<Span> {
        spans
    }

    /// A tall first-column cell (four description lines) followed by a fresh
    /// data row: the window must extend over the continuation lines and the
    /// spine's cell starts must define the columns.
    #[test]
    fn tall_first_column_row_extends_window() {
        let lines: Vec<Vec<Span>> = vec![
            line(vec![sp_at("Description", 50.0, 700.0, 60.0), sp_at("Qty", 300.0, 700.0, 20.0), sp_at("Price", 400.0, 700.0, 50.0)]),
            line(vec![sp_at("(excl. VAT)", 398.0, 686.0, 46.0)]),
            line(vec![sp_at("Product one", 50.0, 672.0, 40.0), sp_at("1", 305.0, 672.0, 6.0), sp_at("8.99", 420.0, 672.0, 20.0)]),
            line(vec![sp_at("long wrapped", 50.0, 658.0, 70.0)]),
            line(vec![sp_at("description tail", 50.0, 644.0, 80.0)]),
            line(vec![sp_at("Shipping", 50.0, 626.0, 45.0), sp_at("3.99", 420.0, 626.0, 20.0)]),
        ];
        let info: Vec<RowInfo> = lines.iter().map(|l| row_info_of(l)).collect();
        let band: Vec<usize> = (0..lines.len()).collect();
        let mut hi = 2;
        let rulers = header_spine_refine(&info, &lines, &band, 0, &mut hi, 1.0, 6.0)
            .expect("two-line header spine not recognized");
        assert_eq!(rulers, vec![50.0, 300.0, 400.0]);
        assert_eq!(hi, 5, "window did not extend over the tall first cell");
    }

    /// End-to-end: a three-column grid with a two-line header label on the
    /// middle column must render the label joined with `<br>` in that cell, the
    /// values in the right columns, and the total row's left-hand label in the
    /// first column.
    #[test]
    fn two_line_header_label_is_joined_per_column() {
        let lines: Vec<Vec<Span>> = vec![
            line(vec![sp_at("VAT rate", 336.0, 700.0, 35.0), sp_at("Item subtotal", 406.0, 700.0, 60.0), sp_at("VAT subtotal", 505.0, 700.0, 55.0)]),
            line(vec![sp_at("(excl. VAT)", 421.0, 686.0, 46.0)]),
            line(vec![sp_at("0%", 358.0, 672.0, 12.0), sp_at("€ 12.98", 435.0, 672.0, 31.0), sp_at("€ 0.00", 534.0, 672.0, 26.0)]),
            line(vec![sp_at("Total", 289.0, 654.0, 30.0), sp_at("€ 12.98", 435.0, 654.0, 31.0), sp_at("€ 0.00", 534.0, 654.0, 26.0)]),
        ];
        let hits = find_tables(&lines);
        let hit = hits
            .iter()
            .find(|h| h.rows.iter().any(|r| r.iter().any(|c| c.contains("VAT rate"))))
            .expect("three-column summary grid not detected");
        assert_eq!(hit.rows[0].len(), 3, "wrong column count: {:?}", hit.rows);
        assert_eq!(hit.rows.len(), 3, "wrong row count: {:?}", hit.rows);
        assert!(
            hit.rows[0][1].contains("<br>") && hit.rows[0][1].contains("(excl. VAT)"),
            "two-line header label was not joined: {:?}",
            hit.rows
        );
        assert_eq!(hit.rows[1], vec!["0%", "€ 12.98", "€ 0.00"]);
        assert_eq!(hit.rows[2], vec!["Total", "€ 12.98", "€ 0.00"]);
    }

    /// End-to-end: a tall first (description) column whose continuation lines
    /// fall in that column only, followed by rows whose Qty/VAT-rate cells are
    /// empty, must stay one table with the continuations joined into row 1.
    #[test]
    fn tall_first_column_and_empty_middle_cells_stay_one_table() {
        let lines: Vec<Vec<Span>> = vec![
            line(vec![
                sp_at("Description", 50.0, 700.0, 55.0),
                sp_at("Qty", 298.0, 700.0, 18.0),
                sp_at("Unit price", 330.0, 700.0, 42.0),
                sp_at("VAT rate", 390.0, 700.0, 35.0),
                sp_at("Unit price", 445.0, 700.0, 42.0),
                sp_at("Item subtotal", 500.0, 700.0, 55.0),
            ]),
            line(vec![
                sp_at("(excl. VAT)", 330.0, 686.0, 42.0),
                sp_at("(incl. VAT)", 445.0, 686.0, 42.0),
                sp_at("(incl. VAT)", 500.0, 686.0, 50.0),
            ]),
            line(vec![
                sp_at("Widget alpha beta", 50.0, 672.0, 240.0),
                sp_at("1", 305.0, 672.0, 6.0),
                sp_at("8.99", 345.0, 672.0, 25.0),
                sp_at("0%", 400.0, 672.0, 12.0),
                sp_at("8.99", 460.0, 672.0, 20.0),
                sp_at("8.99", 520.0, 672.0, 20.0),
            ]),
            line(vec![sp_at("wrapped description line", 50.0, 658.0, 120.0)]),
            line(vec![sp_at("second wrapped line", 50.0, 644.0, 100.0)]),
            line(vec![
                sp_at("Shipping Charges", 50.0, 626.0, 90.0),
                sp_at("3.99", 345.0, 626.0, 25.0),
                sp_at("3.99", 460.0, 626.0, 20.0),
                sp_at("3.99", 520.0, 626.0, 20.0),
            ]),
            line(vec![
                sp_at("Gift Wrap", 50.0, 608.0, 60.0),
                sp_at("0.00", 345.0, 608.0, 25.0),
                sp_at("0.00", 460.0, 608.0, 20.0),
                sp_at("0.00", 520.0, 608.0, 20.0),
            ]),
        ];
        let hits = find_tables(&lines);
        let hit = hits
            .iter()
            .find(|h| h.rows.iter().any(|r| r.iter().any(|c| c.contains("Description"))))
            .expect("invoice-style grid not detected");
        assert_eq!(hit.rows[0].len(), 6, "wrong column count: {:?}", hit.rows);
        assert!(
            hit.rows[0][2].contains("<br>") && hit.rows[0][2].contains("(excl. VAT)"),
            "two-line header label was not joined: {:?}",
            hit.rows
        );
        assert_eq!(hit.rows.len(), 4, "table split or rows folded: {:?}", hit.rows);
        assert!(
            hit.rows[1][0].contains("wrapped description line")
                && hit.rows[1][0].contains("second wrapped line")
                && hit.rows[1][0].contains("<br>"),
            "tall first cell continuation was not joined: {:?}",
            hit.rows
        );
        assert_eq!(hit.rows[1][1], "1", "row 1 quantity moved: {:?}", hit.rows);
        assert_eq!(hit.rows[2][1].trim(), "", "empty Qty cell not kept: {:?}", hit.rows);
        assert_eq!(hit.rows[2][3].trim(), "", "empty VAT-rate cell not kept: {:?}", hit.rows);
        assert_eq!(hit.rows[3][1].trim(), "", "empty Qty cell not kept: {:?}", hit.rows);
    }

    /// A plain data window whose first row is not a two-line header keeps the
    /// caller's rulers (no activation).
    #[test]
    fn data_first_row_is_not_a_spine() {
        let lines: Vec<Vec<Span>> = vec![
            line(vec![sp_at("Widget", 50.0, 700.0, 40.0), sp_at("3", 300.0, 700.0, 6.0), sp_at("9.99", 400.0, 700.0, 20.0)]),
            line(vec![sp_at("Gadget", 50.0, 686.0, 40.0), sp_at("4", 300.0, 686.0, 6.0), sp_at("19.99", 400.0, 686.0, 25.0)]),
        ];
        let info: Vec<RowInfo> = lines.iter().map(|l| row_info_of(l)).collect();
        let band: Vec<usize> = (0..lines.len()).collect();
        let mut hi = 1;
        assert!(header_spine_refine(&info, &lines, &band, 0, &mut hi, 1.0, 6.0).is_none());
    }

    /// A two-line header with no data row below it is not a table: the numeric
    /// gate must reject it without slicing past the window end.
    #[test]
    fn spine_without_a_data_row_is_rejected() {
        let lines: Vec<Vec<Span>> = vec![
            line(vec![sp_at("Description", 50.0, 700.0, 60.0), sp_at("Qty", 300.0, 700.0, 20.0), sp_at("Price", 400.0, 700.0, 50.0)]),
            line(vec![sp_at("(excl. VAT)", 398.0, 686.0, 46.0)]),
        ];
        let info: Vec<RowInfo> = lines.iter().map(|l| row_info_of(l)).collect();
        let band: Vec<usize> = (0..lines.len()).collect();
        let mut hi = 1;
        assert!(header_spine_refine(&info, &lines, &band, 0, &mut hi, 1.0, 6.0).is_none());
        assert_eq!(hi, 1, "window end moved without a data row");
    }

    fn row_info_of(l: &[Span]) -> RowInfo {
        let words = line_words(l);
        let mut starts: Vec<f64> = words.iter().map(|w| w.x0).collect();
        starts.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let mut ends: Vec<f64> = words.iter().map(|w| w.x1).collect();
        ends.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        RowInfo {
            words,
            starts,
            ends,
            size: l[0].size.max(0.1),
        }
    }
}
