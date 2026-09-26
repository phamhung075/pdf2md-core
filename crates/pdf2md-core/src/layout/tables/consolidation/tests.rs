// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Table cell bucketing, multi-line row consolidation, and complementary column merging.

    use super::*;

    fn sp(text: &str, y: f64) -> Span {
        Span {
            text: text.to_string(),
            x: 10.0,
            y,
            size: 10.0,
            advance: 5.0,
            word_advance: 5.0,
            is_bold: false,
            is_italic: false,
            is_underline: false,
            is_vertical: false,
        }
    }

    /// C2 `036c761a_lettre_no100_informations_covid-19_du_30_decembre_2021_0`:
    /// a sparse date row with no first column ("15/11") was followed by a new
    /// data row that *does* carry a first column ("Nombre de tests réalisés").
    /// The unconditional `!curr_has_col0 && row_has_col0 => continuation`
    /// branch fused the two into "15/11<br>34155". A row whose first-column
    /// cell is a descriptive label starts a new logical row.
    #[test]
    fn new_row_with_col0_is_not_merged_into_a_row_without_col0() {
        let lines: Vec<Vec<Span>> = vec![
            vec![sp("Date", 300.0)],
            vec![sp("15/11", 288.0)],
            vec![sp("Nombre de tests réalisés", 276.0)],
        ];
        let info: Vec<RowInfo> = lines
            .iter()
            .map(|_| RowInfo {
                words: Vec::new(),
                starts: Vec::new(),
                ends: Vec::new(),
                size: 10.0,
            })
            .collect();
        let win_rows: Vec<usize> = (0..3).collect();
        let rows = vec![
            vec!["Date".to_string(), "Cas".to_string()],
            vec!["".to_string(), "15/11".to_string()],
            vec!["Nombre de tests réalisés".to_string(), "34155".to_string()],
        ];
        let out = consolidate_table_rows(rows, &win_rows, &lines, &info);
        assert_eq!(
            out.len(),
            3,
            "the new data row was folded into the sparse row: {out:?}"
        );
        let data = out
            .iter()
            .find(|r| r[0].contains("Nombre de tests"))
            .expect("data row missing");
        assert_eq!(data[1], "34155", "value was fused: {out:?}");
        assert!(
            !data[1].contains("15/11"),
            "date header leaked into the data row: {out:?}"
        );
    }

    /// The counterpart to the test above: a multi-line record whose
    /// first-column anchor sits on its *second* visual line (the electronic
    /// ticket's times line over its "28MAR …" date/leg line) must still be
    /// joined, or every flight leg splits into two GFM rows.
    #[test]
    fn compact_date_anchor_on_second_line_joins_its_record() {
        let lines: Vec<Vec<Span>> = vec![
            vec![sp("Date", 300.0)],
            vec![sp("10:05", 288.0)],
            vec![sp("28MAR", 276.0)],
        ];
        let info: Vec<RowInfo> = lines
            .iter()
            .map(|_| RowInfo {
                words: Vec::new(),
                starts: Vec::new(),
                ends: Vec::new(),
                size: 10.0,
            })
            .collect();
        let win_rows: Vec<usize> = (0..3).collect();
        let rows = vec![
            vec!["Date".to_string(), "Departure".to_string()],
            vec!["".to_string(), "10:05".to_string()],
            vec!["28MAR".to_string(), "Marseille".to_string()],
        ];
        let out = consolidate_table_rows(rows, &win_rows, &lines, &info);
        assert_eq!(
            out.len(),
            2,
            "the times line and its date/leg line were not joined: {out:?}"
        );
        assert!(
            out[1].iter().any(|c| c.contains("10:05")) && out[1][0] == "28MAR",
            "joined record is missing its parts: {out:?}"
        );
    }

    /// `billet_table_complex`: a *headerless* label/value grid (no header row —
    /// the first row already holds the passenger label and the `28.00€` amount)
    /// whose first logical row's cells spill onto the following visual lines.
    /// Those lines fill only later columns and must fold back into the row
    /// above instead of becoming blank-keyed rows of their own.
    #[test]
    fn headerless_first_row_absorbs_wrapped_cell_continuations() {
        let lines: Vec<Vec<Span>> = vec![
            vec![sp("DAI HUNG PHAM - 1989", 253.0)],
            vec![sp("bagages en indiquant mes", 250.0)],
            vec![sp("cabine sous mon siège", 245.0)],
            vec![sp("coordonnées", 240.0)],
            vec![sp("Place Standard", 226.0)],
        ];
        let info: Vec<RowInfo> = lines
            .iter()
            .map(|_| RowInfo {
                words: Vec::new(),
                starts: Vec::new(),
                ends: Vec::new(),
                size: 10.0,
            })
            .collect();
        let win_rows: Vec<usize> = (0..5).collect();
        let rows = vec![
            vec![
                "DAI HUNG PHAM - 1989".to_string(),
                "28.00€".to_string(),
                "Je place mon bagage".to_string(),
                "".to_string(),
            ],
            vec!["".to_string(), "".to_string(), "".to_string(), "bagages en indiquant mes".to_string()],
            vec!["".to_string(), "".to_string(), "cabine sous mon siège".to_string(), "".to_string()],
            vec!["".to_string(), "".to_string(), "".to_string(), "coordonnées".to_string()],
            vec!["Place Standard".to_string(), "".to_string(), "".to_string(), "".to_string()],
        ];
        let out = consolidate_table_rows(rows, &win_rows, &lines, &info);
        assert_eq!(
            out.len(),
            2,
            "continuation lines were not folded into the headerless first row: {out:?}"
        );
        assert_eq!(out[0][0], "DAI HUNG PHAM - 1989", "{out:?}");
        assert!(
            out[0][2].contains("Je place mon bagage") && out[0][2].contains("cabine sous mon siège"),
            "middle-column continuation was lost: {out:?}"
        );
        assert!(
            out[0][3].contains("bagages en indiquant mes") && out[0][3].contains("coordonnées"),
            "last-column continuation was lost: {out:?}"
        );
        assert_eq!(out[1][0], "Place Standard", "a fresh col-0 row started a new record: {out:?}");
    }