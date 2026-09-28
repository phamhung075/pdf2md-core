// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Unit tests for the key/value summary-box model.
//!
//! All fixtures are invented; no document, bank or corpus identifier appears.

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

fn rows_of(hit: &TableHit) -> Vec<Vec<String>> {
    hit.rows.clone()
}

/// A heading-over-values box: two labels on the top line, each owning one of
/// the amounts below, plus two labelled balance lines.
fn box_lines() -> Vec<Vec<Span>> {
    vec![
        line(700.0, &[("Debits :", 300.0, 40.0), ("Credits :", 400.0, 40.0)]),
        line(
            688.0,
            &[("Opening balance", 50.0, 120.0), ("-242,03", 220.0, 40.0)],
        ),
        line(
            676.0,
            &[
                ("Closing balance", 50.0, 120.0),
                ("633,43", 220.0, 40.0),
                ("1 214,05", 320.0, 40.0),
                ("2 089,51", 400.0, 40.0),
            ],
        ),
    ]
}

#[test]
fn labels_over_amounts_box_becomes_paired_rows() {
    let lines = box_lines();
    let hits = find_key_value_boxes(&lines, &[]);
    assert_eq!(hits.len(), 1, "one box expected");
    let hit = &hits[0];
    assert_eq!((hit.start, hit.end), (0, 2));
    assert_eq!(
        rows_of(hit),
        vec![
            vec!["Debits :".to_string(), "1 214,05".to_string()],
            vec!["Credits :".to_string(), "2 089,51".to_string()],
            vec!["Opening balance".to_string(), "-242,03".to_string()],
            vec!["Closing balance".to_string(), "633,43".to_string()],
        ]
    );
}

#[test]
fn prose_paragraph_with_a_number_is_not_a_box() {
    let lines = vec![
        line(
            700.0,
            &[
                ("The", 50.0, 20.0),
                ("amount", 75.0, 45.0),
                ("shown", 125.0, 40.0),
                ("on", 170.0, 15.0),
                ("this", 190.0, 25.0),
                ("statement", 220.0, 55.0),
                ("reaches", 280.0, 45.0),
                ("1 234,56", 330.0, 50.0),
                ("over", 385.0, 28.0),
            ],
        ),
        line(
            688.0,
            &[
                ("the", 50.0, 20.0),
                ("period", 75.0, 40.0),
                ("and", 120.0, 22.0),
                ("the", 145.0, 20.0),
                ("closing", 170.0, 45.0),
                ("balance", 220.0, 45.0),
                ("stays", 270.0, 35.0),
                ("2 000,00", 310.0, 48.0),
                ("positive", 365.0, 45.0),
            ],
        ),
    ];
    assert!(find_key_value_boxes(&lines, &[]).is_empty());
}

#[test]
fn address_block_with_postcodes_is_not_a_box() {
    let lines = vec![
        line(700.0, &[("12 EXAMPLE STREET", 50.0, 120.0)]),
        line(688.0, &[("75002 SAMPLE CITY", 50.0, 110.0)]),
        line(676.0, &[("Tel. 01 23 45 67 89", 50.0, 110.0)]),
    ];
    assert!(find_key_value_boxes(&lines, &[]).is_empty());
}

/// A prose tail sharing the baseline with a real total must not be paired with a
/// far-right amount in another field: the resulting single pair is below the
/// minimum and the whole run is rejected.
#[test]
fn prose_tail_on_a_total_line_is_not_a_box() {
    let lines = vec![
        line(
            700.0,
            &[
                ("carried", 18.0, 45.0),
                ("forward", 70.0, 45.0),
                ("in", 120.0, 15.0),
                ("the", 140.0, 20.0),
                ("books", 165.0, 32.0),
                ("under", 200.0, 35.0),
                ("all", 240.0, 20.0),
                ("terms", 265.0, 30.0),
            ],
        ),
        line(
            693.0,
            &[
                ("previous amounts.", 18.0, 95.0),
                ("TOTAL of the period", 216.0, 110.0),
                ("688,28", 424.0, 40.0),
                ("688,28", 480.0, 40.0),
            ],
        ),
    ];
    assert!(find_key_value_boxes(&lines, &[]).is_empty());
}

#[test]
fn same_line_label_value_pairs_become_rows() {
    let lines = vec![
        line(700.0, &[("Alpha", 50.0, 40.0), ("100,00", 200.0, 40.0)]),
        line(688.0, &[("Beta", 50.0, 35.0), ("20,00", 200.0, 40.0)]),
        line(676.0, &[("Gamma total", 50.0, 75.0), ("120,00", 200.0, 40.0)]),
    ];
    let hits = find_key_value_boxes(&lines, &[]);
    assert_eq!(hits.len(), 1);
    assert_eq!(
        rows_of(&hits[0]),
        vec![
            vec!["Alpha".to_string(), "100,00".to_string()],
            vec!["Beta".to_string(), "20,00".to_string()],
            vec!["Gamma total".to_string(), "120,00".to_string()],
        ]
    );
}

#[test]
fn date_column_is_not_a_label_column() {
    let lines = vec![
        line(700.0, &[("07.02", 50.0, 30.0), ("100,00", 220.0, 40.0)]),
        line(688.0, &[("08.02", 50.0, 30.0), ("200,00", 220.0, 40.0)]),
    ];
    assert!(find_key_value_boxes(&lines, &[]).is_empty());
}

#[test]
fn covered_lines_are_left_to_the_earlier_detector() {
    let lines = box_lines();
    let covered = vec![TableHit {
        start: 1,
        end: 2,
        rows: vec![vec!["a".to_string(), "b".to_string()]],
        bbox: BoundingBox::new(0.0, 0.0, 1.0, 1.0),
    }];
    assert!(find_key_value_boxes(&lines, &covered).is_empty());
}

#[test]
fn a_distant_short_line_is_not_absorbed_into_the_box() {
    let mut lines = vec![line(740.0, &[("Note", 50.0, 35.0)])];
    lines.extend(box_lines());
    let hits = find_key_value_boxes(&lines, &[]);
    assert_eq!(hits.len(), 1);
    assert_eq!(
        (hits[0].start, hits[0].end),
        (1, 3),
        "the 32 pt distant line must stay outside the box"
    );
}

#[test]
fn append_only_augments_a_page_that_already_has_a_table() {
    let lines = box_lines();
    // A table-free page keeps the box as plain text ...
    assert!(append_key_value_boxes(&lines, Vec::new()).is_empty());
    // ... while a page with a table gains the box, sorted before it.
    let existing = TableHit {
        start: 3,
        end: 3,
        rows: vec![vec!["a".to_string()]],
        bbox: BoundingBox::new(0.0, 0.0, 1.0, 1.0),
    };
    let hits = append_key_value_boxes(&lines, vec![existing]);
    assert_eq!(hits.len(), 2);
    assert_eq!((hits[0].start, hits[0].end), (0, 2));
    assert_eq!((hits[1].start, hits[1].end), (3, 3));
}
