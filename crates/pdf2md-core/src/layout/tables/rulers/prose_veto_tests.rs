// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Synthetic shapes for the prose/caption veto in the aligned-grid scan.

use super::*;
use super::tests_common::*;

/// A justified body line: many words, no column-like internal gutter, spanning
/// the full width the matrix columns cover. Its word starts deliberately line
/// up with the matrix's column rulers so that, without the prose veto, it would
/// seed a window of its own.
fn prose_line(y: f64) -> Vec<Span> {
    let xs = [100.0, 140.0, 180.0, 220.0, 260.0, 300.0, 340.0, 380.0, 420.0];
    xs.iter()
        .map(|&x| sz("prose", x, y, 30.0, 10.0))
        .collect()
}

/// One 8 pt matrix row: a label plus four short value cells on fixed columns.
fn matrix_row(y: f64, label: &str, cells: [&str; 4]) -> Vec<Span> {
    let mut row = vec![sz(label, 100.0, y, 20.0, 8.0)];
    for (i, c) in cells.iter().enumerate() {
        row.push(sz(c, 180.0 + 80.0 * i as f64, y, 6.0, 8.0));
    }
    row
}

/// Six full-width 10 pt prose lines directly above a five-row 8 pt matrix: the
/// band merge fuses them, but no recovered grid may start on a prose line.
#[test]
fn prose_above_a_matrix_is_not_annexed_into_the_grid() {
    let mut lines: Vec<Vec<Span>> = (0..6).map(|i| prose_line(700.0 - 12.0 * i as f64)).collect();
    let matrix: [(&str, [&str; 4]); 5] = [
        ("hdr", ["Col1", "Col2", "Col3", "Col4"]),
        ("a", ["0", "1", "0", "1"]),
        ("b", ["1", "0", "1", "0"]),
        ("c", ["0", "1", "1", "0"]),
        ("d", ["1", "0", "0", "1"]),
    ];
    for (i, (label, cells)) in matrix.iter().enumerate() {
        lines.push(matrix_row(620.0 - 12.0 * i as f64, label, *cells));
    }

    let hits = find_tables(&lines);
    assert!(!hits.is_empty(), "the matrix was not detected at all");
    for h in &hits {
        assert!(
            h.start >= 6,
            "a detected grid starts on a prose line: hit lines {}..{}",
            h.start,
            h.end
        );
    }
}

/// A caption line is never a table row: not above the grid (it may not be
/// annexed as a header) and not below it (it may not be grown into the body).
#[test]
fn caption_lines_never_become_table_rows() {
    let lines: Vec<Vec<Span>> = vec![
        vec![
            sz("Table", 100.0, 700.0, 30.0, 10.0),
            sz("1:", 136.0, 700.0, 14.0, 10.0),
            sz("A", 156.0, 700.0, 8.0, 10.0),
            sz("caption", 170.0, 700.0, 42.0, 10.0),
            sz("for", 220.0, 700.0, 20.0, 10.0),
            sz("the", 246.0, 700.0, 20.0, 10.0),
            sz("grid", 272.0, 700.0, 26.0, 10.0),
            sz("below", 304.0, 700.0, 32.0, 10.0),
        ],
        vec![sz("Item", 100.0, 660.0, 26.0, 10.0), sz("Value", 250.0, 660.0, 30.0, 10.0)],
        vec![sz("alpha", 100.0, 648.0, 30.0, 10.0), sz("10", 256.0, 648.0, 12.0, 10.0)],
        vec![sz("beta", 100.0, 636.0, 24.0, 10.0), sz("20", 256.0, 636.0, 12.0, 10.0)],
        vec![
            sz("Table", 100.0, 624.0, 30.0, 10.0),
            sz("2:", 136.0, 624.0, 14.0, 10.0),
            sz("a", 156.0, 624.0, 8.0, 10.0),
            sz("closing", 170.0, 624.0, 42.0, 10.0),
            sz("caption", 220.0, 624.0, 42.0, 10.0),
            sz("line", 268.0, 624.0, 24.0, 10.0),
        ],
    ];

    let hits = find_tables(&lines);
    assert!(!hits.is_empty(), "the small grid was not detected");
    for h in &hits {
        assert!(h.start >= 1 && h.end <= 3, "a caption was emitted as a table row: {}..{}", h.start, h.end);
        for row in &h.rows {
            assert!(
                !row.iter().any(|c| c.contains("caption")),
                "caption text ended up inside the grid: {:?}",
                h.rows
            );
        }
    }
}

/// A trailing caption directly under a grid may contribute its alignment
/// rulers — a ragged label column otherwise yields no left ruler — but it
/// itself is never emitted as a row.
#[test]
fn trailing_caption_contributes_rulers_but_is_not_emitted() {
    let lines: Vec<Vec<Span>> = vec![
        vec![
            sz("Guardrails", 379.0, 520.0, 66.0, 10.0),
            sz("MT", 456.0, 520.0, 14.0, 10.0),
            sz("Bench", 472.0, 520.0, 25.0, 10.0),
        ],
        vec![
            sz("No", 367.0, 510.0, 11.0, 10.0),
            sz("system", 380.0, 510.0, 25.0, 10.0),
            sz("prompt", 407.0, 510.0, 26.0, 10.0),
            sz("6.84", 455.0, 510.0, 15.0, 10.0),
            sz("\u{00b1}", 472.0, 510.0, 8.0, 10.0),
            sz("0.07", 482.0, 510.0, 16.0, 10.0),
        ],
        vec![
            sz("Llama", 358.0, 500.0, 23.0, 10.0),
            sz("2", 383.0, 500.0, 4.0, 10.0),
            sz("system", 390.0, 500.0, 24.0, 10.0),
            sz("prompt", 417.0, 500.0, 26.0, 10.0),
            sz("6.38", 455.0, 500.0, 15.0, 10.0),
            sz("\u{00b1}", 472.0, 500.0, 8.0, 10.0),
            sz("0.07", 482.0, 500.0, 16.0, 10.0),
        ],
        vec![
            sz("Mistral", 360.0, 490.0, 25.0, 10.0),
            sz("system", 387.5, 490.0, 25.0, 10.0),
            sz("prompt", 415.0, 490.0, 26.0, 10.0),
            sz("6.58", 455.0, 490.0, 15.0, 10.0),
            sz("\u{00b1}", 472.0, 490.0, 8.0, 10.0),
            sz("0.05", 482.0, 490.0, 16.0, 10.0),
        ],
        vec![
            // Caption set on the same baselines as the grid it labels; two of
            // its words land on the value column's own rulers.
            sz("Table", 353.0, 480.0, 22.0, 10.0),
            sz("4:", 377.0, 480.0, 8.5, 10.0),
            sz("System", 387.5, 480.0, 27.5, 10.0),
            sz("prompts.", 417.0, 480.0, 36.0, 10.0),
            sz("Mean", 455.0, 480.0, 21.0, 10.0),
            sz("official", 478.0, 480.0, 20.0, 10.0),
        ],
    ];

    // Call the un-banded scanner directly: the ragged label column of a
    // side-set grid would otherwise be split into its own column band.
    let hits = scan_aligned_grids_opts(&lines, 1.0, &[], false);
    let hit = hits
        .iter()
        .find(|h| h.rows.iter().any(|r| r.iter().any(|c| c.contains("Guardrails"))))
        .expect("the side grid was not detected");
    assert!(
        hit.rows.iter().any(|r| r.iter().any(|c| c.contains("Mistral system prompt"))),
        "the third data row is missing: {:?}",
        hit.rows
    );
    assert!(
        hit.end <= 3,
        "the trailing caption was emitted as a table row: hit lines {}..{}",
        hit.start,
        hit.end
    );
}
