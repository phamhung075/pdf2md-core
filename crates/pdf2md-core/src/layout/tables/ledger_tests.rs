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
fn a_far_below_footer_is_not_a_ledger_row() {
    // The footer sits at the ledger's own left edge (~40 line pitches below the
    // last operation), so only the vertical-gap cut can keep it out.
    let mut lines = header_lines();
    lines.push(line(
        685.0,
        &[
            ("02.01", 60.0, 30.0),
            ("03.01", 110.0, 30.0),
            ("FIRST OPERATION", 170.0, 90.0),
            ("10,00", 330.0, 40.0),
        ],
    ));
    lines.push(line(
        673.0,
        &[
            ("03.01", 60.0, 30.0),
            ("04.01", 110.0, 30.0),
            ("SECOND OPERATION", 170.0, 100.0),
            ("20,00", 330.0, 40.0),
        ],
    ));
    lines.push(line(
        661.0,
        &[
            ("04.01", 60.0, 30.0),
            ("05.01", 110.0, 30.0),
            ("THIRD OPERATION", 170.0, 95.0),
            ("30,00", 330.0, 40.0),
        ],
    ));
    lines.push(line(205.0, &[("Printed footer", 60.0, 80.0)]));

    let t = &apply_ledger_model(&lines, Vec::new())[0];
    assert_eq!(t.rows.len(), 4, "header plus the three operations only");
    assert_eq!(t.end, 3, "the footer index must be after the hit's end");
    assert!(
        t.rows
            .iter()
            .all(|r| !r.iter().any(|c| c.contains("Printed footer"))),
        "the footer must not be a ledger row: {:?}",
        t.rows
    );
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

#[test]
fn numeric_continuation_carrying_the_amount_joins_its_operation() {
    // A wrapped operation can print the amount on its second line. That line is
    // numeric only, so it extends the operation above instead of opening an
    // amount-only row; a *labelled* sub-total still opens its own row.
    let mut lines = header_lines();
    lines.push(line(
        685.0,
        &[
            ("02.01", 60.0, 30.0),
            ("03.01", 110.0, 30.0),
            ("TRANSFER", 170.0, 60.0),
        ],
    ));
    lines.push(line(673.0, &[("0274536", 170.0, 60.0), ("10,00", 410.0, 40.0)]));
    lines.push(line(655.0, &[("Sous-total", 250.0, 70.0), ("10,00", 330.0, 40.0)]));
    let t = &apply_ledger_model(&lines, Vec::new())[0];
    let op = t
        .rows
        .iter()
        .find(|r| r[2].contains("TRANSFER"))
        .expect("operation row");
    assert!(
        op[2].contains("0274536"),
        "the numeric continuation joins the description: {:?}",
        t.rows
    );
    assert!(op[2].contains(CELL_LINE_BREAK_PENDING));
    assert_eq!(op[4], "10,00", "the continuation amount stays with the operation");
    let sub = t
        .rows
        .iter()
        .find(|r| r[2] == "Sous-total")
        .expect("sub-total row");
    assert_eq!(sub[3], "10,00");
}

#[test]
fn mixed_case_accented_header_is_recognised() {
    // A statement prints its header in mixed case with accents ("Débit",
    // "Crédit"); accent folding must fold the *lowercase* accented letters too,
    // or no amount column is found and the ledger is rejected.
    let lines = vec![
        line(
            700.0,
            &[
                ("Date", 60.0, 34.0),
                ("Valeur", 110.0, 46.0),
                ("Nature des opérations", 170.0, 130.0),
                ("Débit", 340.0, 34.0),
                ("Crédit", 410.0, 38.0),
            ],
        ),
        line(
            685.0,
            &[
                ("02.01", 60.0, 30.0),
                ("03.01", 110.0, 30.0),
                ("TRANSFER OUT", 170.0, 80.0),
                ("250,00", 330.0, 40.0),
            ],
        ),
        line(
            673.0,
            &[
                ("04.01", 60.0, 30.0),
                ("05.01", 110.0, 30.0),
                ("TRANSFER IN", 170.0, 75.0),
                ("900,00", 410.0, 44.0),
            ],
        ),
    ];
    let hits = apply_ledger_model(&lines, Vec::new());
    assert_eq!(hits.len(), 1, "a mixed-case accented header must be a ledger");
    assert_eq!(hits[0].rows[0][0], "Date");
    assert_eq!(hits[0].rows[0][3], "Débit");
    assert_eq!(hits[0].rows[0][4], "Crédit");
    assert_eq!(hits[0].rows[1][3], "250,00");
    assert_eq!(hits[0].rows[2][4], "900,00");
}

#[test]
fn drawn_vertical_rules_cut_the_columns_exactly() {
    // The DEBIT header sits far left of its right-aligned values, so the
    // label/data-derived cut would drag the DEBIT/CREDIT boundary left of the
    // debit value and file it as a credit. The drawn rules must win.
    let lines = vec![
        line(
            700.0,
            &[
                ("DATE", 60.0, 30.0),
                ("VALEUR", 110.0, 30.0),
                ("DESCRIPTION", 170.0, 60.0),
                ("DEBIT", 250.0, 30.0),
                ("CREDIT", 480.0, 30.0),
            ],
        ),
        line(
            685.0,
            &[
                ("02.01", 60.0, 30.0),
                ("03.01", 110.0, 30.0),
                ("CARD PAYMENT", 170.0, 70.0),
                ("12,00", 430.0, 25.0),
            ],
        ),
        line(
            673.0,
            &[
                ("04.01", 60.0, 30.0),
                ("05.01", 110.0, 30.0),
                ("REFUND", 170.0, 45.0),
                ("20,00", 490.0, 25.0),
            ],
        ),
    ];
    let rules = [
        (100.0, 665.0, 706.0),
        (155.0, 665.0, 706.0),
        (240.0, 665.0, 706.0),
        (460.0, 665.0, 706.0),
    ];
    let hits = apply_ledger_model_with_rules(&lines, Vec::new(), &rules);
    assert_eq!(hits.len(), 1);
    let t = &hits[0];
    assert_eq!(t.rows[1][3], "12,00", "the debit value must land under DEBIT");
    assert!(t.rows[1][4].is_empty());
    assert_eq!(t.rows[2][4], "20,00", "the credit value must land under CREDIT");
    assert!(t.rows[2][3].is_empty());
}

#[test]
fn opening_balance_above_the_header_becomes_the_first_row() {
    let lines = vec![
        line(
            730.0,
            &[("ACCOUNT NAME", 60.0, 70.0), ("RIB : 0000 0000 0000", 400.0, 90.0)],
        ),
        line(
            700.0,
            &[("OPENING BALANCE", 170.0, 92.0), ("500,00", 410.0, 44.0)],
        ),
        line(
            688.0,
            &[
                ("DATE", 60.0, 32.0),
                ("VALEUR", 110.0, 40.0),
                ("DESCRIPTION", 170.0, 70.0),
                ("DEBIT", 340.0, 34.0),
                ("CREDIT", 410.0, 38.0),
            ],
        ),
        line(
            673.0,
            &[
                ("02.01", 60.0, 30.0),
                ("03.01", 110.0, 30.0),
                ("PAYMENT", 170.0, 55.0),
                ("12,00", 330.0, 40.0),
            ],
        ),
        line(
            661.0,
            &[
                ("04.01", 60.0, 30.0),
                ("05.01", 110.0, 30.0),
                ("PAYMENT", 170.0, 55.0),
                ("20,00", 330.0, 40.0),
            ],
        ),
    ];
    let t = &apply_ledger_model(&lines, Vec::new())[0];
    assert_eq!(t.start, 1, "the table begins at the opening balance line");
    assert_eq!(t.rows[1][2], "OPENING BALANCE");
    assert_eq!(t.rows[1][4], "500,00");
    assert_eq!(t.rows[0][0], "DATE");
}

#[test]
fn account_band_lines_stay_outside_the_ledger_table() {
    // Two account-info lines (the level-1 band) sit above the opening balance
    // and must not be absorbed as ledger rows.
    let lines = vec![
        line(
            742.0,
            &[("ACCOUNT NAME", 60.0, 70.0), ("RIB : 0000 0000 0000", 400.0, 90.0)],
        ),
        line(
            730.0,
            &[("HOLDER NAME", 60.0, 66.0), ("COMPTE EN EUROS", 420.0, 92.0)],
        ),
        line(
            700.0,
            &[("OPENING BALANCE", 170.0, 92.0), ("500,00", 410.0, 44.0)],
        ),
        line(
            688.0,
            &[
                ("DATE", 60.0, 32.0),
                ("VALEUR", 110.0, 40.0),
                ("DESCRIPTION", 170.0, 70.0),
                ("DEBIT", 340.0, 34.0),
                ("CREDIT", 410.0, 38.0),
            ],
        ),
        line(
            673.0,
            &[
                ("02.01", 60.0, 30.0),
                ("03.01", 110.0, 30.0),
                ("PAYMENT", 170.0, 55.0),
                ("12,00", 330.0, 40.0),
            ],
        ),
        line(
            661.0,
            &[
                ("04.01", 60.0, 30.0),
                ("05.01", 110.0, 30.0),
                ("PAYMENT", 170.0, 55.0),
                ("20,00", 330.0, 40.0),
            ],
        ),
    ];
    let t = &apply_ledger_model(&lines, Vec::new())[0];
    assert_eq!(t.start, 2, "only the opening balance joins the table");
    assert!(
        t.rows.iter().all(|r| !r.iter().any(|c| c.contains("ACCOUNT NAME") || c.contains("HOLDER NAME"))),
        "the account band must not become ledger rows: {:?}",
        t.rows
    );
}

#[test]
fn right_aligned_balance_label_stays_in_the_description_column() {
    let mut lines = header_lines();
    lines.push(line(
        685.0,
        &[
            ("02.01", 60.0, 30.0),
            ("03.01", 110.0, 30.0),
            ("PAYMENT", 170.0, 50.0),
            ("12,00", 330.0, 40.0),
        ],
    ));
    lines.push(line(
        670.0,
        &[
            ("02.01", 60.0, 30.0),
            ("03.01", 110.0, 30.0),
            ("PAYMENT", 170.0, 50.0),
            ("20,00", 330.0, 40.0),
        ],
    ));
    lines.push(line(
        658.0,
        &[("CLOSING BALANCE", 250.0, 100.0), ("900,00", 410.0, 46.0)],
    ));
    let t = &apply_ledger_model(&lines, Vec::new())[0];
    let last = t.rows.last().unwrap();
    assert_eq!(last[2], "CLOSING BALANCE", "a right-aligned label keeps its column");
    assert_eq!(last[4], "900,00");
    assert!(last[3].is_empty());
}

#[test]
fn a_ruled_non_ledger_table_is_left_untouched() {
    // A description/qty/amount grid is not a ledger; a hit the generic pass
    // already found must pass straight through.
    let lines = vec![
        line(
            700.0,
            &[
                ("DESCRIPTION", 60.0, 80.0),
                ("QTY", 220.0, 26.0),
                ("AMOUNT", 420.0, 60.0),
            ],
        ),
        line(
            685.0,
            &[("Widget", 60.0, 40.0), ("2", 220.0, 8.0), ("10,00", 420.0, 30.0)],
        ),
    ];
    let existing = TableHit {
        start: 0,
        end: 1,
        rows: vec![
            vec!["DESCRIPTION".into(), "QTY".into(), "AMOUNT".into()],
            vec!["Widget".into(), "2".into(), "10,00".into()],
        ],
        bbox: BoundingBox::new(60.0, 680.0, 480.0, 700.0),
    };
    let rules = [(200.0, 680.0, 700.0)];
    let out = apply_ledger_model_with_rules(&lines, vec![existing.clone()], &rules);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].rows, existing.rows);
}
