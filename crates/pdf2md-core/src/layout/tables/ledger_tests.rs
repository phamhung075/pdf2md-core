// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Unit tests for the header-anchored ledger model.

use super::*;

fn span(text: &str, x: f64, y: f64, advance: f64) -> Span {
    Span {
        text: text.to_string(),
        x,
        y,
        size: 10.0,
        advance,
        word_advance: advance,
        is_bold: false,
        is_italic: false,
        is_underline: false,
        is_vertical: false,
    }
}

/// A line of `(text, x, advance)` spans on baseline `y`.
fn line(y: f64, cells: &[(&str, f64, f64)]) -> Vec<Span> {
    cells.iter().map(|(t, x, a)| span(t, *x, y, *a)).collect()
}

fn header_lines() -> Vec<Vec<Span>> {
    vec![line(
        700.0,
        &[
            ("DATE", 60.0, 38.0),
            ("VALEUR", 110.0, 46.0),
            ("NATURE DES OPERATIONS", 170.0, 150.0),
            ("DEBIT", 340.0, 40.0),
            ("CREDIT", 420.0, 46.0),
        ],
    )]
}

fn kinds(md_hit: &TableHit) -> Vec<&str> {
    md_hit.rows[0].iter().map(|s| s.as_str()).collect()
}

#[test]
fn header_anchored_ledger_assigns_amounts_by_x() {
    let mut lines = header_lines();
    lines.push(line(
        685.0,
        &[
            ("02.01", 60.0, 30.0),
            ("03.01", 110.0, 30.0),
            ("TRANSFER OUT", 170.0, 90.0),
            ("250,00", 330.0, 40.0),
        ],
    ));
    lines.push(line(
        673.0,
        &[
            ("04.01", 60.0, 30.0),
            ("05.01", 110.0, 30.0),
            ("TRANSFER IN", 170.0, 80.0),
            ("900,00", 410.0, 50.0),
        ],
    ));
    let hits = apply_ledger_model(&lines, Vec::new());
    assert_eq!(hits.len(), 1);
    let t = &hits[0];
    assert_eq!(kinds(t).len(), 5);
    assert_eq!(t.rows[1][3], "250,00", "debit amount must land in DEBIT");
    assert!(t.rows[1][4].is_empty());
    assert_eq!(t.rows[2][4], "900,00", "credit amount must land in CREDIT");
    assert!(t.rows[2][3].is_empty());
}

#[test]
fn header_split_over_two_baselines_is_recognised() {
    let lines = vec![
        line(
            700.0,
            &[
                ("DATE", 60.0, 38.0),
                ("VALEUR", 110.0, 46.0),
                ("NATURE DES OPERATIONS", 170.0, 150.0),
            ],
        ),
        line(688.0, &[("DEBIT", 340.0, 40.0), ("CREDIT", 420.0, 46.0)]),
        line(670.0, &[("02.01", 60.0, 30.0), ("REF", 170.0, 25.0)]),
        line(658.0, &[("03.01", 60.0, 30.0), ("REF", 170.0, 25.0)]),
    ];
    let hits = apply_ledger_model(&lines, Vec::new());
    assert_eq!(kinds(&hits[0]).len(), 5, "two baselines still give 5 columns");
    assert_eq!(hits[0].start, 0);
    assert_eq!(hits[0].end, 3);
}

#[test]
fn section_labels_and_subtotals_become_rows() {
    let mut lines = header_lines();
    lines.push(line(
        680.0,
        &[
            ("TRANSFERS RECEIVED", 170.0, 130.0),
        ],
    ));
    lines.push(line(
        662.0,
        &[
            ("02.01", 60.0, 30.0),
            ("02.01", 110.0, 30.0),
            ("PAYMENT", 170.0, 60.0),
            ("10,00", 330.0, 40.0),
        ],
    ));
    lines.push(line(
        647.0,
        &[
            ("Sub-total", 250.0, 55.0),
            ("10,00", 330.0, 40.0),
        ],
    ));
    let t = &apply_ledger_model(&lines, Vec::new())[0];
    let texts: Vec<&str> = t.rows.iter().map(|r| r[2].as_str()).collect();
    assert!(
        texts.contains(&"TRANSFERS RECEIVED"),
        "section label must be its own row: {texts:?}"
    );
    let sub = t.rows.iter().find(|r| r[2] == "Sub-total").expect("sub-total row");
    assert_eq!(sub[3], "10,00");
}

#[test]
fn wrapped_description_stays_in_one_cell() {
    let mut lines = header_lines();
    lines.push(line(
        680.0,
        &[
            ("02.01", 60.0, 30.0),
            ("02.01", 110.0, 30.0),
            ("CARD PAYMENT", 170.0, 80.0),
            ("12,00", 330.0, 40.0),
        ],
    ));
    lines.push(line(668.0, &[("SECOND LINE", 170.0, 70.0)]));
    lines.push(line(656.0, &[("THIRD LINE", 170.0, 60.0)]));
    let t = &apply_ledger_model(&lines, Vec::new())[0];
    let nature = &t.rows[1][2];
    assert!(nature.contains("CARD PAYMENT"));
    assert!(nature.contains("SECOND LINE"));
    assert!(nature.contains("THIRD LINE"));
    assert!(
        nature.contains(CELL_LINE_BREAK_PENDING),
        "multi-line description must be joined with an in-cell break"
    );
}

#[test]
fn page_furniture_left_of_the_ledger_ends_the_table() {
    let mut lines = header_lines();
    lines.push(line(680.0, &[("02.01", 60.0, 30.0), ("PAYMENT", 170.0, 60.0)]));
    lines.push(line(660.0, &[("02.01", 60.0, 30.0), ("PAYMENT", 170.0, 60.0)]));
    lines.push(line(600.0, &[("Printed statement footer", 20.0, 120.0)]));
    let t = &apply_ledger_model(&lines, Vec::new())[0];
    assert_eq!(t.end, 2, "footer line must stay outside the ledger");
    assert_eq!(t.rows.len(), 3);
}

#[test]
fn a_plain_invoice_grid_is_not_a_ledger() {
    // description | qty | unit price | amount: no date/value column.
    let lines = vec![
        line(
            700.0,
            &[
                ("DESCRIPTION", 60.0, 90.0),
                ("QTY", 220.0, 26.0),
                ("UNIT PRICE", 300.0, 70.0),
                ("AMOUNT", 420.0, 60.0),
            ],
        ),
        line(685.0, &[("Widget", 60.0, 40.0), ("2", 220.0, 8.0)]),
        line(670.0, &[("Gadget", 60.0, 40.0), ("1", 220.0, 8.0)]),
    ];
    assert!(
        apply_ledger_model(&lines, Vec::new()).is_empty(),
        "an invoice grid must not be rebuilt by the ledger model"
    );
}

#[test]
fn long_numeric_reference_is_a_continuation_not_a_page_counter() {
    let mut lines = header_lines();
    lines.push(line(
        680.0,
        &[
            ("02.01", 60.0, 30.0),
            ("02.01", 110.0, 30.0),
            ("DIRECT DEBIT", 170.0, 90.0),
            ("12,00", 330.0, 40.0),
        ],
    ));
    // Ten identical-length continuation lines keep the median pitch stable and
    // put a long all-digit reference in the description column.
    for k in 0..10 {
        lines.push(line(668.0 - 12.0 * k as f64, &[("001090367841", 170.0, 70.0)]));
    }
    let t = &apply_ledger_model(&lines, Vec::new())[0];
    assert!(
        t.rows[1][2].contains("001090367841"),
        "a long numeric reference must stay in the description cell"
    );
    assert!(t.end >= 11);
}

/// A ledger that continues on the next page repeats its header. The model must
/// recognise that repeated header and derive exactly the same column signature,
/// which is the join key for continuing the ledger (the rows of the second
/// page's table carry the same five columns).
#[test]
fn repeated_header_on_the_next_page_derives_the_same_columns() {
    let page = |entry: &str| {
        let mut lines = header_lines();
        lines.push(line(
            680.0,
            &[
                ("02.01", 60.0, 30.0),
                ("02.01", 110.0, 30.0),
                (entry, 170.0, 80.0),
                ("10,00", 330.0, 40.0),
            ],
        ));
        lines.push(line(
            668.0,
            &[
                ("03.01", 60.0, 30.0),
                ("03.01", 110.0, 30.0),
                (entry, 170.0, 80.0),
                ("20,00", 330.0, 40.0),
            ],
        ));
        lines
    };
    let first = apply_ledger_model(&page("FIRST PAGE"), Vec::new());
    let second = apply_ledger_model(&page("SECOND PAGE"), Vec::new());
    assert_eq!(first.len(), 1);
    assert_eq!(second.len(), 1);
    assert_eq!(
        first[0].rows[0], second[0].rows[0],
        "the repeated header must derive the same column signature"
    );
    assert_eq!(first[0].rows[0].len(), 5);
}

#[test]
fn a_label_or_subtotal_does_not_absorb_the_line_below_it() {
    let mut lines = header_lines();
    // A balance row: label plus a credit amount, no date/value cell.
    lines.push(line(
        688.0,
        &[("OPENING BALANCE", 170.0, 96.0), ("500,00", 410.0, 46.0)],
    ));
    // A section label directly below, at the normal line pitch.
    lines.push(line(676.0, &[("SECTION ONE", 170.0, 74.0)]));
    // An operation: date, a wrapped two-line description, a debit amount.
    lines.push(line(
        664.0,
        &[
            ("01.01", 60.0, 30.0),
            ("01.01", 110.0, 30.0),
            ("PAYMENT ONE", 170.0, 72.0),
            ("10,00", 330.0, 40.0),
        ],
    ));
    lines.push(line(652.0, &[("SECOND PART", 170.0, 70.0)]));
    // A sub-total row: amount, no date/value cell.
    lines.push(line(
        640.0,
        &[("Sous-total", 250.0, 55.0), ("10,00", 330.0, 40.0)],
    ));
    // Another section label directly below, at the normal line pitch.
    lines.push(line(628.0, &[("SECTION TWO", 170.0, 74.0)]));
    lines.push(line(
        616.0,
        &[
            ("02.01", 60.0, 30.0),
            ("02.01", 110.0, 30.0),
            ("PAYMENT TWO", 170.0, 72.0),
            ("20,00", 410.0, 46.0),
        ],
    ));

    let t = &apply_ledger_model(&lines, Vec::new())[0];
    let titles: Vec<&str> = t.rows.iter().map(|r| r[2].as_str()).collect();
    for label in ["OPENING BALANCE", "SECTION ONE", "Sous-total", "SECTION TWO"] {
        assert!(
            titles.contains(&label),
            "{label} must be its own row: {titles:?}"
        );
    }
    let balance = t.rows.iter().find(|r| r[2] == "OPENING BALANCE").unwrap();
    assert_eq!(balance[4], "500,00");
    let sub = t.rows.iter().find(|r| r[2] == "Sous-total").unwrap();
    assert_eq!(sub[3], "10,00");
    let op = t
        .rows
        .iter()
        .find(|r| r[2].starts_with("PAYMENT ONE"))
        .expect("operation row");
    assert!(op[2].contains("SECOND PART"), "wrapped description stays joined");
    assert!(
        op[2].contains(CELL_LINE_BREAK_PENDING),
        "wrapped description keeps its in-cell break"
    );
    assert_eq!(op[3], "10,00");
}
