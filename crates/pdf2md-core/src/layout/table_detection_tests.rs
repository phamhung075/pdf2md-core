// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! High-performance geometry-based text layout reconstruction and semantic AST pipeline.

    use super::*;
    use lopdf::{Dictionary, Document, Object};
    use crate::layout::glyph_stream::resolve_font_style;

    /// Build one visual line from (start_x, text) word spans on a shared
    /// baseline `y` (page coordinates: larger y = higher on the page).
    fn row(y: f64, words: &[(f64, &str)]) -> Vec<Span> {
        words
            .iter()
            .map(|(x, t)| Span {
                text: t.to_string(),
                x: *x,
                y,
                size: 10.0,
                advance: 0.0,
                word_advance: 0.0,
                is_bold: false,
                is_italic: false,
                is_underline: false,
                is_vertical: false,
            })
            .collect()
    }

    fn page(rows: Vec<Vec<Span>>) -> Vec<Vec<Span>> {
        rows
    }

    /// A left-aligned word at `x0` (fixed start, like `row`'s words) next to
    /// a right-aligned word whose `x0` is back-computed from `right_x1` so
    /// its *end* (`x + advance`) lands exactly there — unlike `row`, `advance`
    /// is a real synthetic character width so `x1` is meaningful, letting a
    /// test line up a numeric column by its right edge instead of its start.
    fn left_and_right_aligned_row(y: f64, left_x0: f64, left: &str, right_x1: f64, right: &str) -> Vec<Span> {
        let size = 10.0;
        let right_advance = right.chars().count() as f64 * size * 0.55;
        vec![
            Span {
                text: left.to_string(),
                x: left_x0,
                y,
                size,
                advance: left.chars().count() as f64 * size * 0.55,
                word_advance: left.chars().count() as f64 * size * 0.55,
                is_bold: false,
                is_italic: false,
                is_underline: false,
                is_vertical: false,
            },
            Span {
                text: right.to_string(),
                x: right_x1 - right_advance,
                y,
                size,
                advance: right_advance,
                word_advance: right_advance,
                is_bold: false,
                is_italic: false,
                is_underline: false,
                is_vertical: false,
            },
        ]
    }

    #[test]
    fn right_aligned_numeric_column_is_recovered_by_its_end_not_its_start() {
        // Three amounts of different digit counts — "9,20 €", "145,50 €",
        // "1 200,00 €" — so their x0 (word start) scatters across the page,
        // but they all right-align to the same column edge (x1 = 250.0). A
        // start-only ruler scan finds no common x0 here and drops the whole
        // column; only the end-based (x1) clustering recovers it.
        let lines = page(vec![
            left_and_right_aligned_row(700.0, 50.0, "Frais", 250.0, "9,20 €"),
            left_and_right_aligned_row(690.0, 50.0, "Abonnement", 250.0, "145,50 €"),
            left_and_right_aligned_row(680.0, 50.0, "Consommation", 250.0, "1 200,00 €"),
        ]);

        // Every row's amount has a distinct x0 — the exact case a start-only
        // scan cannot cluster.
        let x0s: std::collections::HashSet<i64> = lines
            .iter()
            .map(|l| (l[1].x * 100.0).round() as i64)
            .collect();
        assert_eq!(x0s.len(), 3, "fixture must actually vary x0 across rows");

        let hits = find_tables(&lines);
        assert_eq!(hits.len(), 1, "right-aligned amount column must be recovered as a table");
        let rows = &hits[0].rows;
        assert_eq!(rows.len(), 3, "all 3 rows expected: {rows:?}");
        assert_eq!(rows[0][0].trim(), "Frais");
        assert_eq!(rows[0][1].trim(), "9,20 €");
        assert_eq!(rows[1][1].trim(), "145,50 €");
        assert_eq!(rows[2][1].trim(), "1 200,00 €");
    }

    #[test]
    fn aligned_two_column_grid_is_detected() {
        let lines = page(vec![
            row(700.0, &[(50.0, "Nom"), (300.0, "DUPONT")]),
            row(690.0, &[(50.0, "Prenom"), (300.0, "Marie")]),
            row(680.0, &[(50.0, "Date"), (300.0, "1985")]),
            row(670.0, &[(50.0, "Pays"), (300.0, "France")]),
        ]);
        let hits = find_tables(&lines);
        assert_eq!(hits.len(), 1, "aligned 2-col grid must be recovered");
        let rows: Vec<Vec<String>> = hits[0].rows.clone();
        assert_eq!(rows.len(), 4, "4 rows expected: {rows:?}");
        assert_eq!(rows[0][0].as_str(), "Nom");
        assert_eq!(rows[0][1].as_str(), "DUPONT");
    }

    #[test]
    fn toc_dot_leader_rows_are_rejected() {
        let lines = page(vec![
            row(700.0, &[(50.0, "1.1"), (300.0, "....."), (420.0, "5")]),
            row(690.0, &[(50.0, "1.2"), (300.0, "....."), (420.0, "6")]),
            row(680.0, &[(50.0, "2.1"), (300.0, "....."), (420.0, "9")]),
            row(670.0, &[(50.0, "2.2"), (300.0, "....."), (420.0, "12")]),
        ]);
        assert!(
            find_tables(&lines).is_empty(),
            "TOC dot leaders must not be tabled"
        );
        assert!(
            find_gap_tables(&lines, &[]).is_empty(),
            "TOC dot leaders must not be tabled (3b)"
        );
    }

    #[test]
    fn aligned_long_prose_columns_are_rejected() {
        let lines = page(vec![
            row(
                700.0,
                &[
                    (50.0, "The quick brown fox jumps over the lazy dog"),
                    (300.0, "second column of equally long words lives"),
                ],
            ),
            row(
                690.0,
                &[
                    (50.0, "Another quite long sentence to fill the first column"),
                    (300.0, "and the right hand column keeps on flowing too"),
                ],
            ),
            row(
                680.0,
                &[
                    (50.0, "A third verbose paragraph placed under the two above"),
                    (300.0, "with prose that keeps reading like a document"),
                ],
            ),
        ]);
        assert!(
            find_tables(&lines).is_empty(),
            "aligned prose must not be tabled"
        );
    }

    #[test]
    fn bullet_marker_column_is_rejected() {
        let lines = page(vec![
            row(700.0, &[(50.0, "-"), (80.0, "option one")]),
            row(690.0, &[(50.0, "-"), (80.0, "option two")]),
            row(680.0, &[(50.0, "-"), (80.0, "option three")]),
            row(670.0, &[(50.0, "-"), (80.0, "option four")]),
        ]);
        assert!(
            find_tables(&lines).is_empty(),
            "bullet marker column must not be tabled"
        );
    }

    #[test]
    fn jittered_grid_is_recovered_by_stage3b_only() {
        let lines = page(vec![
            row(700.0, &[(50.0, "A1"), (300.0, "B1")]),
            row(690.0, &[(50.0, "A2"), (300.8, "B2")]),
            row(680.0, &[(50.0, "A3"), (299.6, "B3")]),
            row(670.0, &[(50.0, "A4"), (300.4, "B4")]),
        ]);
        assert!(
            find_tables(&lines).is_empty(),
            "strict pass must miss the jittered grid"
        );
        let gap = find_gap_tables(&lines, &[]);
        assert_eq!(gap.len(), 1, "Stage-3b must recover the jittered grid");
        let rows: Vec<Vec<String>> = gap[0].rows.clone();
        assert!(rows[0].iter().any(|c| c == "B1"), "cells wrong: {rows:?}");
    }

    #[test]
    fn docblock_bounding_box_has_positive_height_and_preserves_style() {
        let spans = vec![Span {
            text: "Bold Section Heading".to_string(),
            x: 100.0,
            y: 750.0,
            size: 20.0,
            advance: 150.0,
            word_advance: 150.0,
            is_bold: true,
            is_italic: false,
            is_underline: false,
            is_vertical: false,
        }];
        let lines = build_lines(&spans);
        let blocks = build_doc_blocks(&lines, 842.0, None);
        assert_eq!(blocks.len(), 1);
        let block = &blocks[0];
        assert_eq!(block.text, "**Bold Section Heading**", "Bold run must be wrapped in **emphasis**");
        assert!(block.is_bold, "Block must retain bold flag");
        assert!(!block.is_italic, "Block must retain italic flag");
        assert!(
            block.y1 > block.y0,
            "Block height must be positive: y0={}, y1={}",
            block.y0,
            block.y1
        );
        assert_eq!(block.y1 - block.y0, 20.0, "Block height should match typographic size");
    }

    #[test]
    fn resolve_font_style_detects_bold_and_italic() {
        let doc = Document::new();
        let mut normal_dict = Dictionary::new();
        normal_dict.set(b"BaseFont", Object::Name(b"Helvetica".to_vec()));
        assert_eq!(resolve_font_style(&doc, &normal_dict), (false, false));

        let mut bold_dict = Dictionary::new();
        bold_dict.set(b"BaseFont", Object::Name(b"Helvetica-Bold".to_vec()));
        assert_eq!(resolve_font_style(&doc, &bold_dict), (true, false));

        let mut italic_dict = Dictionary::new();
        italic_dict.set(b"BaseFont", Object::Name(b"TimesNewRoman,Italic".to_vec()));
        assert_eq!(resolve_font_style(&doc, &italic_dict), (false, true));

        let mut bold_italic_dict = Dictionary::new();
        bold_italic_dict.set(b"BaseFont", Object::Name(b"Arial-BoldItalicMT".to_vec()));
        assert_eq!(resolve_font_style(&doc, &bold_italic_dict), (true, true));
    }