// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Reading order recovery, multi-column stream separation, and structured DocBlock generation.

use super::*;
use super::column_band_tests_common::*;

    /// Regression for the bilingual e-ticket header whose columns are
    /// *staggered*: a French heading and its English translation sit one above
    /// the other in a narrow column, a neighbouring pair sits alongside, and
    /// their baselines interleave. Merging runs by baseline welds the columns
    /// into spurious rows, and the old top-to-bottom order threaded the English
    /// half of the first pair ("PART ONE") between the second pair's French and
    /// English halves. The staggered columns must be recovered and emitted
    /// left-to-right, each pair together, French first.
    #[test]
    fn staggered_bilingual_heading_pairs_keep_each_pair_together() {
        let lines = vec![
            vec![sp("Preface line above the block", 37.0, 700.0)],
            vec![
                sp("SECTION ONE", 37.0, 665.3),
                sp("LABEL TWO", 217.0, 666.1),
            ],
            vec![
                sp("TRANSLATION TWO", 217.0, 653.0),
                sp("SECTION THREE", 434.0, 657.7),
            ],
            vec![
                sp("PART ONE", 37.0, 651.2),
                sp("PART THREE", 454.0, 647.7),
            ],
            // A bridging row: one wide run across the first two columns. It
            // must stay after the block rather than join a column.
            vec![sp("A note that spans the first two columns", 37.0, 620.0)],
        ];

        // Flatten the page's streams, splitting each visual line back into its
        // segments so a merged row's columns are compared individually. On the
        // unfixed renderer the merged order is SECTION ONE, LABEL TWO,
        // TRANSLATION TWO, SECTION THREE, PART ONE, PART THREE — the English
        // half of the first pair lands between the second pair's halves.
        let seq: Vec<String> = page_read_order(&lines)
            .iter()
            .flatten()
            .flat_map(|l| {
                render_line_text(l, None)
                    .split('\n')
                    .map(|s| s.trim().to_string())
                    .collect::<Vec<_>>()
            })
            .collect();
        let pos = |n: &str| {
            seq.iter()
                .position(|t| t == n)
                .unwrap_or_else(|| panic!("missing {n:?} in {seq:?}"))
        };
        assert!(pos("SECTION ONE") < pos("PART ONE"), "first pair split: {seq:?}");
        assert!(pos("PART ONE") < pos("LABEL TWO"), "first pair not kept together: {seq:?}");
        assert!(pos("LABEL TWO") < pos("TRANSLATION TWO"), "label pair split: {seq:?}");
        assert!(
            pos("TRANSLATION TWO") < pos("SECTION THREE"),
            "label pair not kept together: {seq:?}"
        );
        assert!(pos("SECTION THREE") < pos("PART THREE"), "second pair split: {seq:?}");
        assert!(
            pos("PART THREE") < pos("A note that spans the first two columns"),
            "the bridging note must stay after the block: {seq:?}"
        );
    }

    /// Regression for `mustang_attributeBasedXMP_EN16931.pdf`: a single-column
    /// invoice page with a right-aligned amount column made the vertical-
    /// projection fallback read the page as two columns, so every amount was
    /// emitted after the whole left stream and the `blocks` channel moved a
    /// row's total (`275,00`) and a table header's last cell (`Teilzahlung`) to
    /// the end of the page. The projection's "left half" is not one prose
    /// column but a grid of table cells (label + description + quantity…), so
    /// the reading-order gate must reject it; the low-level projection detector
    /// itself stays ungated.
    #[test]
    fn cell_grid_half_is_not_a_page_column() {
        // One physical row: a label at x=50, a middle cell at x=150 (an
        // internal grid gutter), and an amount pinned to right edge 400 whose
        // left edge moves with its own length.
        let row = |y: f64, a: &str, b: &str, amount: &str| {
            vec![
                sp(a, 50.0, y),
                sp(b, 150.0, y),
                sp(amount, 400.0 - amount.len() as f64 * 6.0, y),
            ]
        };
        let lines = vec![
            row(300.0, "Positionssumme", "Basis", "473,00"),
            row(290.0, "Gesamtbetrag", "zu", "0,00"),
            row(280.0, "Abschlag", "netto", "-0,00"),
            row(270.0, "Rechnungssumme", "ohne", "473,00"),
            row(260.0, "Zahlbetrag", "frei", "529,87"),
        ];
        assert!(
            detect_projection_two_columns(&lines).is_some(),
            "the projection corridor is real; the missing reading-order gate is the bug"
        );
        let pc = page_two_columns(&lines).expect("projection fallback still detects it");
        assert!(
            !columns_are_viable_prose(&pc),
            "a half that is a grid of cells must not read as one prose column"
        );
    }

    /// The shape gate must not reject a genuine two-column page whose lines are
    /// each a single `Tj` run (one span per visual line) — the case the
    /// projection fallback exists for, and the shape the structural benchmark's
    /// `twocol_*` documents use.
    #[test]
    fn single_span_prose_columns_are_still_detected() {
        let row = |y: f64, l: &str, r: &str| vec![sp(l, 50.0, y), sp(r, 350.0, y)];
        let lines = vec![
            row(300.0, "Left line A.", "Right line A."),
            row(290.0, "Left line B.", "Right line B."),
            row(280.0, "Left line C.", "Right line C."),
            row(270.0, "Left line D.", "Right line D."),
        ];
        let pc = page_two_columns(&lines)
            .expect("a genuine two-column page must still be detected");
        assert_eq!(pc.left.len(), 4, "{:?}", pc.left);
        assert_eq!(pc.right.len(), 4, "{:?}", pc.right);
        assert!(
            columns_are_viable_prose(&pc),
            "a genuine two-column page must survive the reading-order gate"
        );
    }

    /// Regression for the target `SeConnecterAMonComptePartenaire.pdf`. The
    /// numbered list's items 4 and 5 are each fused onto one visual line with
    /// the adjacent callout-box heading "Accès à l'Espace / bailleur", whose two
    /// lines sit a couple of points off the list baseline. The reading-order
    /// pass split the fused rows at the gutter but then *rejected* the block
    /// (only two crossing rows) and restored each fused row whole, so the single
    /// linear stream wove item 4 → "Accès" → item 5 → "bailleur" together. With
    /// the fix the list stays contiguous and the box heading is emitted as its
    /// own block after item 5.
    #[test]
    fn fused_list_item_and_callout_heading_keep_the_list_contiguous() {
        let lines = vec![
            vec![sp("1. Renseignez vos identifiants", 50.0, 300.0)],
            vec![sp("2. Renseignez les informations", 50.0, 290.0)],
            vec![sp("3. Personnalisez votre mot de passe", 50.0, 280.0)],
            fused_list_row(270.0, "4.", ["Acceptez", "les", "règles"], 268.0, ["Accès", "à", "l’Espace"]),
            fused_list_row(260.0, "5.", ["Acceptez", "les", "conditions"], 258.0, ["bailleur", "du", "compte"]),
            vec![sp(" ", 50.0, 250.0)],
            vec![sp("Vous êtes un bailleur physique et vous gérez", 50.0, 240.0)],
        ];
        let bands = detect_column_bands(&lines);
        assert_eq!(bands.len(), 3, "Full list preamble, Columns block, Full heading");
        match &bands[1] {
            ColumnBand::Columns { left, right } => {
                let lt: Vec<String> = left.iter().map(|l| render_line_text(l, None)).collect();
                let rt: Vec<String> = right.iter().map(|l| render_line_text(l, None)).collect();
                assert!(lt[0].contains("4."), "list item 4 stays left: {lt:?}");
                assert!(lt[1].contains("5."), "list item 5 stays left: {lt:?}");
                assert!(
                    lt.iter().all(|t| !t.contains("Accès") && !t.contains("bailleur")),
                    "no box heading may be woven into the list: {lt:?}"
                );
                assert_eq!(rt.len(), 2, "the two box lines stay together right: {rt:?}");
                assert!(rt[0].contains("Accès") && rt[1].contains("bailleur"), "{rt:?}");
            }
            ColumnBand::Full(_) => panic!("the fused list/box block was merged instead of split"),
            ColumnBand::Stacks(_) => {
                panic!("the fused list/box block was merged instead of split")
            }
        }
        // And the flattened page order must read 1..5 before the box heading.
        let streams = page_read_order(&lines);
        let seq: Vec<String> = streams.iter().flatten().map(|l| render_line_text(l, None)).collect();
        let pos = |needle: &str| {
            seq.iter()
                .position(|t| t.contains(needle))
                .unwrap_or_else(|| panic!("missing {needle:?} in {seq:?}"))
        };
        assert!(
            pos("1.") < pos("2.") && pos("2.") < pos("3.") && pos("3.") < pos("4.") && pos("4.") < pos("5."),
            "the numbered list must stay contiguous: {seq:?}"
        );
        assert!(
            pos("5.") < pos("Accès") && pos("Accès") < pos("bailleur"),
            "the box heading must follow the list as one block: {seq:?}"
        );
    }

    #[test]
    fn same_baseline_label_value_rows_are_not_transposed_into_columns() {
        let lines = vec![
            same_baseline_pair(300.0, "Bestellung", [":", "B123456789", "vom"]),
            same_baseline_pair(290.0, "Weitere", [":", "A456123", "Art"]),
            vec![sp("Ende", 50.0, 280.0)],
        ];
        let bands = detect_column_bands(&lines);
        assert!(
            bands.iter().all(|b| !matches!(b, ColumnBand::Columns { .. })),
            "same-baseline label/value rows must keep their row order: {} bands",
            bands.len()
        );
    }

    /// Counter-case for the fix: when one-sided rows separate the two crossing
    /// rows (the `text_style_complex` single-column training form), the wide
    /// gaps merely happen to align. The corridor must not be trusted even
    /// though each crossing row is itself a fused pair on different baselines.
    #[test]
    fn non_adjacent_crossings_are_not_a_column_band() {
        let lines = vec![
            fused_list_row(300.0, "1.", ["A", "B", "C"], 298.0, ["D", "E", "F"]),
            vec![sp("intervening line one", 50.0, 290.0)],
            vec![sp("intervening line two", 50.0, 280.0)],
            fused_list_row(270.0, "2.", ["G", "H", "I"], 268.0, ["J", "K", "L"]),
            vec![sp("Ende", 50.0, 260.0)],
        ];
        let bands = detect_column_bands(&lines);
        assert!(
            bands.iter().all(|b| !matches!(b, ColumnBand::Columns { .. })),
            "crossings separated by one-sided rows must not form a column band: {} bands",
            bands.len()
        );
    }

    #[test]
    fn three_narrow_columns_are_recovered_not_woven() {
        // A full-width header whose single span covers both gutters must stay
        // its own stream and must not be absorbed into the column region.
        let mut lines = vec![vec![mc_span("HHHHHHHHHHHHHHHHHHHHHHHHHHHHHHHH", 50.0, 800.0)]];
        for i in 0..10 {
            lines.push(mc_three_col_row(780.0 - i as f64 * 12.0));
        }
        // The projection finds the two gutters directly.
        let region = multi_column_projection(&lines).expect("3 columns recovered");
        assert_eq!(region.columns.len(), 3, "must recover three columns");
        // Each column is a single column's text, in reading order.
        let first: String = region.columns[0]
            .iter()
            .flat_map(|l| l.iter().map(|s| s.text.as_str()))
            .collect();
        assert!(first.starts_with("mot"), "left column leads: {first}");
        assert!(!first.contains("autre"), "no middle column text leaks in: {first}");
        let middle: String = region.columns[1]
            .iter()
            .flat_map(|l| l.iter().map(|s| s.text.as_str()))
            .collect();
        assert!(middle.starts_with("autre"), "middle column follows: {middle}");
        // The region excludes the full-width header.
        assert_eq!(region.start, 1, "header must not join the column region");
    }

    /// A single-column page of ordinary prose must not be carved into columns
    /// by the projection: there is no recurring 0.4em+ gutter to find.
    #[test]
    fn single_column_prose_is_not_split_by_projection() {
        let mut lines = Vec::new();
        for i in 0..12 {
            let (v, _) = mc_fragment(&["une", "ligne", "de", "prose", "ordinaire"], 50.0, 780.0 - i as f64 * 12.0);
            lines.push(v);
        }
        assert!(
            multi_column_projection(&lines).is_none(),
            "single-column prose must not project into columns"
        );
        assert_eq!(page_read_order(&lines).len(), 1);
    }

    /// Regression for the Air France e-ticket receipt block. The last visual
    /// line of the two-column block carries only a short facing segment on the
    /// left ("réservations en cliquant ici"), so the *midpoint* of the white in
    /// front of the right column recedes far left of the established gutter —
    /// while the right column still begins at its usual margin. The
    /// midpoint-only consistency filter discarded that split, `col_bottom`
    /// stopped one row above it, and the line was flushed as `bottom_full`
    /// after the entire opposite column, cutting both the French and the
    /// English sentence in half.
    #[test]
    fn short_final_row_stays_in_its_columns() {
        let full_left = ["Le", "tarif", "reserve", "est", "valable", "pour", "un", "billet"];
        let right = ["France", "web", "site", "by", "clicking"];
        let place = |words: &[&str], mut x: f64, y: f64| -> (Vec<Span>, f64) {
            let mut v = Vec::new();
            for w in words {
                v.push(sp(w, x, y));
                x += w.len() as f64 * 6.0 + 6.0;
            }
            (v, x)
        };
        let mut lines: Vec<Vec<Span>> = Vec::new();
        for i in 0..4 {
            let y = 600.0 - i as f64 * 10.0;
            let (mut row, _) = place(&full_left, 50.0, y); // left text ends at x=308
            let (mut r, _) = place(&right, 450.0, y);
            row.append(&mut r);
            lines.push(row);
        }
        let y = 560.0;
        let (mut row, _) = place(&["Vos", "en", "ici"], 50.0, y); // left text ends at x=110
        let (mut r, _) = place(&right, 450.0, y);
        row.append(&mut r);
        lines.push(row);

        let pc = page_two_columns(&lines).expect("a two-column page must be detected");
        assert!(
            pc.bottom_full.is_empty(),
            "the final row leaked past the columns: {:?}",
            pc.bottom_full
        );
        assert_eq!(pc.left.len(), 5, "left column lost its final line: {:?}", pc.left);
        assert_eq!(pc.right.len(), 5, "right column lost its final line: {:?}", pc.right);
        let tail_left: String = pc.left.last().unwrap().iter().map(|s| s.text.as_str()).collect();
        let tail_right: String = pc.right.last().unwrap().iter().map(|s| s.text.as_str()).collect();
        assert!(tail_left.contains("Vos") && tail_left.contains("ici"), "French tail not on the left: {tail_left:?}");
        assert!(
            tail_right.contains("France") && tail_right.contains("clicking"),
            "English tail not on the right: {tail_right:?}"
        );
        assert!(
            !tail_right.contains("Vos"),
            "the short French tail was woven into the right column: {tail_right:?}"
        );
    }
