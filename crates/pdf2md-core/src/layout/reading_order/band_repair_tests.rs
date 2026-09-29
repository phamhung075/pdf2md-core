// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Unit tests for [`repair_column_bands`]: the post-pass that re-joins a
//! two-column block the row-local running-gutter pass fragmented.

use super::*;

/// A 10pt span whose advance is 6pt per character, so a word's ink ends at
/// `x + 6 * text.len()`.
fn sp(text: &str, x: f64, y: f64) -> Span {
    Span {
        text: text.to_string(),
        x,
        y,
        size: 10.0,
        advance: text.len() as f64 * 6.0,
        word_advance: text.len() as f64 * 6.0,
        is_bold: false,
        is_italic: false,
        is_underline: false,
        is_vertical: false,
    }
}

/// Left column at x=50, right column at x=330: a 280pt page gutter, far wider
/// than any word space, so the running-gutter pass can seed here.
fn left_row(y: f64) -> Vec<Span> {
    vec![sp("leftword", 50.0, y)]
}
fn right_row(y: f64) -> Vec<Span> {
    vec![sp("rightword", 330.0, y)]
}

fn gutter_of(band: &ColumnBand) -> f64 {
    match band {
        ColumnBand::Columns { left, right } => {
            let le = left.iter().flat_map(|r| r.iter()).map(|s| s.x + s.advance).fold(f64::NEG_INFINITY, f64::max);
            let rs = right.iter().flat_map(|r| r.iter()).map(|s| s.x).fold(f64::INFINITY, f64::min);
            0.5 * (le + rs)
        }
        _ => panic!("expected a Columns band"),
    }
}

#[test]
fn absorbs_a_leading_one_sided_row_into_its_column() {
    // The right column's first line sits on its own before any crossing row, so
    // the running-gutter pass left it (plus a full-width title) in a `Full`
    // band emitted before the `Columns` band. The repair must move it into the
    // right stream and leave the title above the block.
    let bands = vec![
        ColumnBand::Full(vec![
            vec![sp("FullWidthTitle", 50.0, 640.0)],
            right_row(600.0),
        ]),
        ColumnBand::Columns {
            left: vec![left_row(586.0), left_row(574.0)],
            right: vec![right_row(587.0), right_row(575.0)],
        },
    ];
    let out = repair_column_bands(bands);
    assert_eq!(out.len(), 2, "title must stay a Full band above the columns");
    match (&out[0], &out[1]) {
        (ColumnBand::Full(rows), ColumnBand::Columns { right, .. }) => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0][0].text, "FullWidthTitle");
            assert_eq!(right[0][0].text, "rightword");
            assert_eq!(right[0][0].y, 600.0, "leading row must head the right stream");
            assert_eq!(right.len(), 3);
        }
        other => { let _ = other; panic!("unexpected band pairing") },
    }
}

#[test]
fn merges_two_column_bands_split_by_one_sided_rows() {
    // Two confirmed runs of the same gutter separated by a `Full` band of
    // one-sided rows must become one block, not two interleaved ones.
    let bands = vec![
        ColumnBand::Columns {
            left: vec![left_row(600.0)],
            right: vec![right_row(600.0)],
        },
        ColumnBand::Full(vec![left_row(588.0), right_row(576.0)]),
        ColumnBand::Columns {
            left: vec![left_row(552.0)],
            right: vec![right_row(552.0)],
        },
    ];
    let out = repair_column_bands(bands);
    assert_eq!(out.len(), 1);
    match &out[0] {
        ColumnBand::Columns { left, right } => {
            assert_eq!(left.len(), 3, "left rows 600,588,552");
            assert_eq!(right.len(), 3, "right rows 600,576,552");
            let ys: Vec<f64> = right.iter().map(|r| r[0].y).collect();
            assert_eq!(ys, vec![600.0, 576.0, 552.0], "right stream stays top-to-bottom");
        }
        other => { let _ = other; panic!("unexpected band pairing") },
    }
}

#[test]
fn does_not_absorb_a_row_that_crosses_the_gutter() {
    // A full-width line whose single span covers the gutter is not one-sided;
    // it must stay a `Full` band and keep the two column runs apart.
    let mut wide = sp("WholeLineAcrossTheGutter", 50.0, 588.0);
    wide.advance = 400.0;
    let bands = vec![
        ColumnBand::Columns {
            left: vec![left_row(600.0)],
            right: vec![right_row(600.0)],
        },
        ColumnBand::Full(vec![vec![wide]]),
        ColumnBand::Columns {
            left: vec![left_row(552.0)],
            right: vec![right_row(552.0)],
        },
    ];
    let out = repair_column_bands(bands);
    assert_eq!(out.len(), 3, "a straddling row blocks the absorption");
    assert!(matches!(out[1], ColumnBand::Full(_)));
}

#[test]
fn does_not_merge_bands_with_different_gutters() {
    // Two column blocks whose gutters do not line up are independent regions
    // and must not be transposed into one stream.
    let far_left = ColumnBand::Columns {
        left: vec![left_row(600.0)],
        right: vec![right_row(600.0)],
    };
    let far_right = ColumnBand::Columns {
        left: vec![vec![sp("a", 200.0, 552.0)]],
        right: vec![vec![sp("b", 520.0, 552.0)]],
    };
    let bands = vec![far_left, far_right];
    let out = repair_column_bands(bands);
    assert_eq!(out.len(), 2);
}

#[test]
fn gutter_is_the_midpoint_of_the_facing_edges() {
    let band = ColumnBand::Columns {
        left: vec![left_row(600.0)],
        right: vec![right_row(600.0)],
    };
    // "leftword" starts at 50 and is 8 chars * 6pt = 48pt wide -> ends at 98;
    // the right column starts at 330. Midpoint = 214.
    assert!((gutter_of(&band) - 214.0).abs() < 0.5);
}
