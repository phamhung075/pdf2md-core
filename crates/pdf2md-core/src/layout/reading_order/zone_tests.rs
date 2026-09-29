// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Reading order recovery, multi-column stream separation, and structured DocBlock generation.

use super::column_band_tests_common::{same_baseline_pair, sp};
use super::*;

/// Render a band sequence to `Vec<(&str, Vec<String>)>` so a test can assert
/// which stream each line landed in.
fn streams_of(bands: &[ColumnBand]) -> Vec<(String, Vec<String>)> {
    bands
        .iter()
        .map(|b| match b {
            ColumnBand::Full(rows) => (
                "full".to_string(),
                rows.iter().map(|l| render_line_text(l, None)).collect(),
            ),
            ColumnBand::Columns { left, right } => (
                "columns".to_string(),
                left.iter()
                    .chain(right.iter())
                    .map(|l| render_line_text(l, None))
                    .collect(),
            ),
            ColumnBand::Stacks(cols) => (
                "stacks".to_string(),
                cols.iter()
                    .flatten()
                    .map(|l| render_line_text(l, None))
                    .collect(),
            ),
        })
        .collect()
}

/// The motivating shape: a full-width heading row above a three-row bilingual
/// footer whose every visual line is one `Tj` run per language (2 spans/row, so
/// `split_row_columns`'s >=5-span floor never seeds the running-gutter pass and
/// the projection's six-row floor never opens). The left side continues across
/// rows; both sides keep a fixed left edge. It must read French column, then
/// English column.
#[test]
fn short_bilingual_footer_reads_zone_by_zone() {
    let mut lines = vec![vec![sp("nous vous souhaitons une agreable journee", 30.0, 650.0)]];
    let left = [
        "le texte francais commence ici",
        "et continue sur la ligne suivante",
        "encore une suite de mots ici",
    ];
    let right = [
        "the english text starts here now",
        "and carries on across the line",
        "for one final short row here",
    ];
    for (i, (l, r)) in left.iter().zip(right.iter()).enumerate() {
        let y = 620.0 - i as f64 * 10.0;
        lines.push(vec![sp(l, 30.0, y), sp(r, 305.0, y)]);
    }

    let bands = detect_column_bands(&lines);
    assert_eq!(bands.len(), 2, "heading Full + one Columns band");
    let streams = streams_of(&bands);
    assert_eq!(streams[0].0, "full", "the straddling heading stays full width");
    assert_eq!(streams[1].0, "columns");
    let col = &streams[1].1;
    assert_eq!(col.len(), 6, "both language columns preserved: {col:?}");
    // Left column first, then right column — not woven row by row.
    assert!(col[0].contains("le texte francais"), "{col:?}");
    assert!(col[1].contains("et continue"), "{col:?}");
    assert!(col[2].contains("encore une suite"), "{col:?}");
    assert!(col[3].contains("the english text"), "{col:?}");
    assert!(col[4].contains("and carries on"), "{col:?}");
    assert!(col[5].contains("for one final"), "{col:?}");
    assert!(
        col.iter().all(|l| !(l.contains("francais") && l.contains("english"))),
        "a language must never be woven into the other: {col:?}"
    );
}

/// Two clear rows are the floor: a two-row bilingual pair with stable edges
/// splits, confirming the pass does not need three or six rows.
#[test]
fn two_row_zone_pair_splits() {
    let lines = vec![
        vec![
            sp("la premiere ligne du bloc", 30.0, 300.0),
            sp("the first line of the block", 305.0, 300.0),
        ],
        vec![
            sp("et sa continuation ici", 30.0, 290.0),
            sp("and its continuation here", 305.0, 290.0),
        ],
    ];
    let bands = detect_column_bands(&lines);
    assert_eq!(bands.len(), 1);
    match &bands[0] {
        ColumnBand::Columns { left, right } => {
            assert_eq!(left.len(), 2);
            assert_eq!(right.len(), 2);
        }
        _ => panic!("two-row zone pair must become a Columns band"),
    }
}

/// A lone gap on one row is not a zone: at least two consecutive rows must
/// cross the corridor.
#[test]
fn single_row_gap_is_not_a_zone() {
    let lines = vec![
        vec![
            sp("une seule ligne ici", 30.0, 300.0),
            sp("a single line only here", 305.0, 300.0),
        ],
        vec![sp("une ligne pleine largeur qui traverse", 30.0, 290.0)],
    ];
    let bands = detect_column_bands(&lines);
    assert!(
        bands.iter().all(|b| !matches!(b, ColumnBand::Columns { .. })),
        "a one-row gap must not form a column band: {} bands",
        bands.len()
    );
}

/// A numeric value column beside wrapped prose is a label/value table, not two
/// text zones: the numeric guard keeps the rows row-wise.
#[test]
fn numeric_value_column_is_not_a_zone() {
    let lines = vec![
        vec![
            sp("alpha beta gamma", 30.0, 300.0),
            sp("1 234,56", 305.0, 300.0),
        ],
        vec![
            sp("delta epsilon zeta", 30.0, 290.0),
            sp("9 876,54", 305.0, 290.0),
        ],
    ];
    let bands = detect_column_bands(&lines);
    assert!(
        bands.iter().all(|b| !matches!(b, ColumnBand::Columns { .. })),
        "an amount column must not be read as a text zone: {} bands",
        bands.len()
    );
}

/// Independent label/value pairs (no sentence continues across the rows) must
/// keep their row order even though both sides are wordy and left-aligned.
#[test]
fn label_value_pairs_are_not_transposed() {
    let lines = vec![
        vec![
            sp("Départ / Departure", 30.0, 300.0),
            sp("GARE CENTRALE NORD", 305.0, 300.0),
        ],
        vec![
            sp("Arrivée / Arrival", 30.0, 290.0),
            sp("PORT DE PLAISANCE DU SUD", 305.0, 290.0),
        ],
    ];
    let bands = detect_column_bands(&lines);
    assert!(
        bands.iter().all(|b| !matches!(b, ColumnBand::Columns { .. })),
        "label/value pairs must not be transposed into columns: {} bands",
        bands.len()
    );
}

/// A side that is itself a two-cell grid (an internal wide gap on every row) is
/// a table half, not a text column.
#[test]
fn grid_side_is_not_a_zone() {
    let lines = vec![
        vec![
            sp("alpha beta gamma", 30.0, 300.0),
            sp("1047", 305.0, 300.0),
            sp("7.2", 360.0, 300.0),
        ],
        vec![
            sp("delta epsilon zeta", 30.0, 290.0),
            sp("1031", 305.0, 290.0),
            sp("6.8", 360.0, 290.0),
        ],
    ];
    let bands = detect_column_bands(&lines);
    assert!(
        bands.iter().all(|b| !matches!(b, ColumnBand::Columns { .. })),
        "a grid half must not be read as a text zone: {} bands",
        bands.len()
    );
}

/// The existing same-baseline label/value counter-case, now with only two rows
/// (the shape the new short-zone pass could newly reach): the wide gaps recur
/// but the cells are independent, so the rows stay row-wise.
#[test]
fn same_baseline_pair_of_two_rows_stays_rowwise() {
    let lines = vec![
        same_baseline_pair(300.0, "Bestellung", [":", "B123456789", "vom"]),
        same_baseline_pair(290.0, "Weitere", [":", "A456123", "Art"]),
    ];
    let bands = detect_column_bands(&lines);
    assert!(
        bands.iter().all(|b| !matches!(b, ColumnBand::Columns { .. })),
        "two same-baseline label/value rows must keep their order: {} bands",
        bands.len()
    );
}

/// Two independent label/value fields that share a corridor but sit far apart
/// vertically must not be welded into one zone. Each side is wordy and
/// wrapped-looking, so only the row-adjacency guard (`ZONE_MAX_ROW_GAP_EM`)
/// rejects the pair.
#[test]
fn far_apart_fields_sharing_a_corridor_are_not_a_zone() {
    let lines = vec![
        vec![
            sp("alpha beta gamma delta", 42.0, 500.0),
            sp("one two three four", 200.0, 500.0),
        ],
        vec![
            sp("epsilon zeta eta theta", 42.0, 440.0),
            sp("five six seven eight", 200.0, 440.0),
        ],
    ];
    let bands = detect_column_bands(&lines);
    assert!(
        bands.iter().all(|b| !matches!(b, ColumnBand::Columns { .. })),
        "fields 6 em apart must not be welded into a zone: {} bands",
        bands.len()
    );
}

/// Two independent blocks whose columns do not share baselines (a staggered
/// address / box pair) must still read left block then right block. Neither
/// side wraps, so only the unequal line counts keep it out of the label/value
/// guard.
#[test]
fn staggered_two_blocks_are_read_sequentially() {
    let lines = vec![
        vec![sp("Alpha beta gamma delta", 30.0, 300.0)],
        vec![
            sp("Epsilon zeta eta theta", 30.0, 288.0),
            sp("One two three four", 305.0, 290.0),
        ],
        vec![
            sp("Iota kappa lambda mu", 30.0, 276.0),
            sp("Five six seven eight", 305.0, 278.0),
        ],
        vec![sp("Nu xi omicron pi", 30.0, 264.0)],
        vec![sp("Nine ten eleven twelve", 305.0, 252.0)],
    ];
    let bands = detect_column_bands(&lines);
    let col = bands.iter().find_map(|b| match b {
        ColumnBand::Columns { left, right } => Some((left, right)),
        _ => None,
    });
    let (left, right) = col.expect("staggered blocks must become a Columns band");
    assert_eq!(left.len(), 4, "left block must keep all its lines: {left:?}");
    assert_eq!(right.len(), 3, "right block must keep all its lines: {right:?}");
    let texts: Vec<String> = left
        .iter()
        .chain(right.iter())
        .map(|l| render_line_text(l, None))
        .collect();
    assert!(texts[0].contains("Alpha beta"), "{texts:?}");
    assert!(texts[3].contains("Nu xi"), "{texts:?}");
    assert!(texts[4].contains("One two"), "{texts:?}");
    assert!(texts[6].contains("Nine ten"), "{texts:?}");
    assert!(
        texts.iter().all(|t| !(t.contains("Alpha") && t.contains("One two"))),
        "blocks must not be woven: {texts:?}"
    );
}

/// A row-aligned pair with equal line counts and no wrapping is a label/value
/// box, not two blocks: the stagger/unequal-count escape must not split it.
#[test]
fn row_aligned_equal_pair_stays_rowwise() {
    let lines = vec![
        vec![sp("Alpha beta gamma", 30.0, 300.0), sp("GARE CENTRALE NORD", 305.0, 300.0)],
        vec![sp("Delta epsilon zeta", 30.0, 290.0), sp("PORT DE PLAISANCE", 305.0, 290.0)],
    ];
    let bands = detect_column_bands(&lines);
    assert!(
        bands.iter().all(|b| !matches!(b, ColumnBand::Columns { .. })),
        "an equal label/value pair must not be transposed: {} bands",
        bands.len()
    );
}
