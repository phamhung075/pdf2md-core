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

    // -----------------------------------------------------------------------
    // Render-time same-line monetary joiner (two amount columns in one cell).
    // -----------------------------------------------------------------------

    use crate::convert_pdf_bytes_to_markdown;
    use crate::layout::tables::rulers::min_gutter_for;
    use crate::ConversionOptions;
    use lopdf::content::{Content, Operation};
    use lopdf::{dictionary, Document, Object, Stream};

    fn wt(text: &str, x0: f64, x1: f64) -> WordTok {
        WordTok {
            text: text.to_string(),
            x0,
            x1,
        }
    }

    /// Two complete amounts, separated by a real column gutter, are two visual
    /// columns the sparse ruler filed into one cell: the cell keeps both, with
    /// the deferred in-cell break between them (emitted as `<br>`).
    #[test]
    fn two_gutter_separated_amounts_get_an_in_cell_line_break() {
        let mg = min_gutter_for(10.0); // 11.0
        let words = vec![wt("9 876,54", 380.0, 424.0), wt("1 234,56", 500.0, 544.0)];
        let refs: Vec<&WordTok> = words.iter().collect();
        assert_eq!(
            join_cell_words(&refs, mg),
            format!("9 876,54{}1 234,56", CELL_LINE_BREAK_PENDING)
        );
    }

    /// A single space-grouped amount split into tokens by a thousands gap
    /// shorter than the gutter is *not* two columns: it stays one space-joined
    /// amount.
    #[test]
    fn thousands_gap_between_amount_tokens_stays_a_space() {
        let mg = min_gutter_for(10.0);
        let words = vec![wt("1", 500.0, 506.0), wt("234,56", 508.0, 540.0)];
        let refs: Vec<&WordTok> = words.iter().collect();
        assert_eq!(join_cell_words(&refs, mg), "1 234,56");
    }

    /// Two complete amounts closer than the gutter are a label continuation or
    /// a tight numeric pair, not two columns; they keep today's space.
    #[test]
    fn two_amounts_below_the_gutter_stay_space_joined() {
        let mg = min_gutter_for(10.0);
        let words = vec![wt("1,23", 100.0, 120.0), wt("4,56", 124.0, 145.0)];
        let refs: Vec<&WordTok> = words.iter().collect();
        assert_eq!(join_cell_words(&refs, mg), "1,23 4,56");
    }

    /// A label and its amount in one cell are never two amounts: unchanged.
    #[test]
    fn label_and_amount_in_one_cell_are_space_joined() {
        let mg = min_gutter_for(10.0);
        let words = vec![wt("TTC", 60.0, 80.0), wt("1 234,56", 100.0, 144.0)];
        let refs: Vec<&WordTok> = words.iter().collect();
        assert_eq!(join_cell_words(&refs, mg), "TTC 1 234,56");
    }

    /// Build a one-page, 612x792 statement whose rows are `(y, runs)`, the
    /// runs each an absolute `(x, text)` text showing operation. The rows are
    /// the `rows.pdf` shape: a label column at x=60 and an amount column at
    /// x=500, with room for a second amount column at x=380 on one row.
    fn statement_pdf(rows: &[(f64, Vec<(f64, String)>)]) -> Vec<u8> {
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let font_id = doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "Helvetica",
            "Encoding" => "WinAnsiEncoding",
        });
        let resources_id = doc.add_object(dictionary! {
            "Font" => dictionary! { "F1" => font_id },
        });
        let mut ops = vec![Operation::new("BT", vec![])];
        let show = |ops: &mut Vec<Operation>, x: f64, y: f64, text: &str| {
            ops.push(Operation::new("Tf", vec!["F1".into(), 11.0.into()]));
            ops.push(Operation::new(
                "Tm",
                vec![
                    1.0.into(),
                    0.0.into(),
                    0.0.into(),
                    1.0.into(),
                    x.into(),
                    y.into(),
                ],
            ));
            ops.push(Operation::new(
                "Tj",
                vec![Object::string_literal(text.as_bytes().to_vec())],
            ));
        };
        for (y, runs) in rows {
            for (x, text) in runs {
                show(&mut ops, *x, *y, text);
            }
        }
        show(
            &mut ops,
            60.0,
            760.0,
            "Synthetic statement for layout verification only",
        );
        ops.push(Operation::new("ET", vec![]));
        let content_id = doc.add_object(Stream::new(
            dictionary! {},
            Content { operations: ops }.encode().unwrap(),
        ));
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "Contents" => content_id,
            "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        });
        let pages = dictionary! {
            "Type" => "Pages",
            "Kids" => vec![page_id.into()],
            "Count" => 1,
            "Resources" => resources_id,
        };
        doc.objects.insert(pages_id, Object::Dictionary(pages));
        let catalog_id = doc.add_object(dictionary! {
            "Type" => "Catalog",
            "Pages" => pages_id,
        });
        doc.trailer.set("Root", catalog_id);
        let mut bytes = Vec::new();
        doc.save_to(&mut bytes).expect("save synthetic pdf");
        bytes
    }

    fn convert_rows_pdf(rows: &[(f64, Vec<(f64, String)>)]) -> String {
        convert_pdf_bytes_to_markdown(&statement_pdf(rows), &ConversionOptions::default())
            .expect("convert synthetic pdf")
            .markdown
    }

    /// The GFM body line carrying `needle` (separator rows excluded).
    fn body_line_with(md: &str, needle: &str) -> String {
        md.lines()
            .map(str::trim)
            .find(|l| l.starts_with('|') && l.contains(needle) && !l.contains("---"))
            .unwrap_or_default()
            .to_string()
    }

    /// `rows.pdf`: the middle row draws `9 876,54` at x=380 and `1 234,56` at
    /// x=500. The ruler pass sees one amount column (x=500) and files both
    /// amounts into the cell; the joiner must emit them on separate in-cell
    /// lines, while every other row/cell is untouched.
    #[test]
    fn rows_pdf_middle_cell_renders_two_amounts_on_separate_lines() {
        let rows = vec![
            (
                700.0,
                vec![
                    (60.0, "Total TTC".to_string()),
                    (500.0, "1 234,56".to_string()),
                ],
            ),
            (
                670.0,
                vec![
                    (60.0, "Montant HT".to_string()),
                    (380.0, "9 876,54".to_string()),
                    (500.0, "1 234,56".to_string()),
                ],
            ),
            (
                640.0,
                vec![
                    (60.0, "Lot 12".to_string()),
                    (500.0, "345,67".to_string()),
                ],
            ),
        ];
        let md = convert_rows_pdf(&rows);
        let middle = body_line_with(&md, "Montant HT");
        assert!(
            middle.contains("9 876,54<br>1 234,56"),
            "middle row amounts were not split onto separate lines: {middle:?}"
        );
        let first = body_line_with(&md, "Total TTC");
        assert!(
            first.contains("| 1 234,56") && !first.contains("<br>"),
            "an untouched row changed: {first:?}"
        );
    }

    /// The same statement, but the middle row's single amount is drawn as the
    /// tokens `1` and `234,56` separated by a thousands gap shorter than the
    /// gutter. It must stay the one space-joined amount `1 234,56`.
    #[test]
    fn rows_pdf_split_thousands_amount_stays_one_space_joined_amount() {
        let rows = vec![
            (
                700.0,
                vec![
                    (60.0, "Total TTC".to_string()),
                    (500.0, "1 234,56".to_string()),
                ],
            ),
            (
                670.0,
                vec![
                    (60.0, "Montant HT".to_string()),
                    (500.0, "1".to_string()),
                    (508.5, "234,56".to_string()),
                ],
            ),
            (
                640.0,
                vec![
                    (60.0, "Lot 12".to_string()),
                    (500.0, "345,67".to_string()),
                ],
            ),
        ];
        let md = convert_rows_pdf(&rows);
        let middle = body_line_with(&md, "Montant HT");
        assert!(
            middle.contains("1 234,56") && !middle.contains("1<br>234,56"),
            "a sub-gutter thousands gap was broken into two lines: {middle:?}"
        );
    }