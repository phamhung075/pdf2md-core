// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Tests for header-spine column derivation.

use super::*;
use super::tests_common::*;


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
    let mut win_lo = 0;
    let rulers = header_spine_refine(&info, &lines, &band, &mut win_lo, &mut hi, 1.0, 6.0, &[])
        .expect("two-line header spine not recognized");
    // Column left edges: the description column 50..130 (its continuation
    // lines reach past the header), Qty 300..320, Price 400..450.
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

/// End-to-end: a wide first (description) column beside a narrow quantity
/// column must stay two columns. Deriving the cuts from the header cells'
/// *starts* puts the midpoint far left of the description's right edge and
/// fuses "Qté" into the description cell; the cell extents keep them apart.
#[test]
fn wide_description_column_does_not_fuse_the_quantity_column() {
    let lines: Vec<Vec<Span>> = vec![
        line(vec![
            sp_at("Description", 38.0, 700.0, 37.5),
            sp_at("Qty", 309.0, 700.0, 12.1),
            sp_at("Unit price", 347.2, 700.0, 40.8),
            sp_at("VAT rate", 411.5, 700.0, 33.3),
            sp_at("Unit price", 471.1, 700.0, 40.8),
            sp_at("Total", 538.4, 700.0, 16.7),
        ]),
        line(vec![
            sp_at("HT", 378.1, 686.0, 10.0),
            sp_at("TTC", 497.4, 686.0, 14.6),
            sp_at("TTC", 542.6, 686.0, 14.6),
        ]),
        line(vec![
            sp_at("A very long product description that wraps", 38.0, 672.0, 249.1),
            sp_at("1", 316.8, 672.0, 4.2),
            sp_at("26,91", 363.1, 672.0, 25.0),
            sp_at("20 %", 425.6, 672.0, 19.2),
            sp_at("32,29", 486.9, 672.0, 25.0),
            sp_at("32,29", 532.1, 672.0, 25.0),
        ]),
        line(vec![sp_at("second wrapped description line", 38.0, 658.0, 255.1)]),
        line(vec![
            sp_at("Shipping", 38.0, 640.0, 30.0),
            sp_at("4,16", 367.3, 640.0, 20.8),
            sp_at("4,99", 491.1, 640.0, 20.8),
            sp_at("4,99", 536.3, 640.0, 20.8),
        ]),
    ];
    let hits = find_tables(&lines);
    let hit = hits
        .iter()
        .find(|h| h.rows.iter().any(|r| r.iter().any(|c| c.contains("Description"))))
        .expect("six-column grid not detected");
    assert_eq!(hit.rows[0].len(), 6, "wrong column count: {:?}", hit.rows);
    assert_eq!(hit.rows[0][0].trim(), "Description", "first column merged: {:?}", hit.rows[0]);
    assert_eq!(hit.rows[0][1].trim(), "Qty", "quantity column merged: {:?}", hit.rows[0]);
    assert_eq!(hit.rows[0][2].trim(), "Unit price<br>HT", "wrapped header: {:?}", hit.rows[0]);
    assert_eq!(hit.rows[0][5].trim(), "Total<br>TTC", "wrapped header: {:?}", hit.rows[0]);
    assert_eq!(hit.rows[1][1], "1", "quantity moved: {:?}", hit.rows);
    assert_eq!(hit.rows[1][2], "26,91", "unit price moved: {:?}", hit.rows);
    assert!(
        hit.rows[1][0].contains("second wrapped description line")
            && hit.rows[1][0].contains("<br>"),
        "description continuation not joined: {:?}",
        hit.rows
    );
    assert_eq!(hit.rows[2][1].trim(), "", "empty Qty cell not kept: {:?}", hit.rows);
    assert_eq!(hit.rows[2][3].trim(), "", "empty VAT-rate cell not kept: {:?}", hit.rows);
}

/// End-to-end: a short header label ("TVA") over a value wider than it is
/// (a right-aligned amount that starts left of the label) must not pull the
/// value into the neighbouring column.
#[test]
fn short_header_over_a_wide_amount_keeps_its_column() {
    let lines: Vec<Vec<Span>> = vec![
        line(vec![
            sp_at("VAT rate", 373.2, 700.0, 33.3),
            sp_at("Total", 462.3, 700.0, 28.7),
            sp_at("VAT", 542.6, 700.0, 14.6),
        ]),
        line(vec![
            sp_at("20 %", 389.4, 686.0, 17.1),
            sp_at("26,91", 466.0, 686.0, 25.0),
            sp_at("5,38", 536.3, 686.0, 20.8),
        ]),
        line(vec![
            sp_at("Total", 301.4, 672.0, 16.7),
            sp_at("26,91", 466.0, 672.0, 25.0),
            sp_at("5,38", 536.3, 672.0, 20.8),
        ]),
    ];
    let hits = find_tables(&lines);
    let hit = hits
        .iter()
        .find(|h| h.rows.iter().any(|r| r.iter().any(|c| c.contains("VAT rate"))))
        .expect("three-column summary grid not detected");
    assert_eq!(hit.rows[0].len(), 3, "wrong column count: {:?}", hit.rows);
    assert_eq!(hit.rows[1], vec!["20 %", "26,91", "5,38"], "values shifted: {:?}", hit.rows);
    assert_eq!(hit.rows[2], vec!["Total", "26,91", "5,38"], "values shifted: {:?}", hit.rows);
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
    let mut win_lo = 0;
    assert!(header_spine_refine(&info, &lines, &band, &mut win_lo, &mut hi, 1.0, 6.0, &[]).is_none());
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
    let mut win_lo = 0;
    assert!(header_spine_refine(&info, &lines, &band, &mut win_lo, &mut hi, 1.0, 6.0, &[]).is_none());
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
