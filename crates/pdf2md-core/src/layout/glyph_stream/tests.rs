// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! PDF content-stream glyph extraction, font metric resolution, and 2D span aggregation.

use super::*;
use super::tests_common::*;
    

    /// Regression for the Type0/Identity-H run-width bug: a `Tj`/`TJ` operand
    /// carrying several 2-byte CIDs must sum every CID's `/W` advance. Folding
    /// the whole run into one bogus CID missed `/W`, fell back to `/DW`
    /// (1 em), drifted the text matrix, and fused words in the Markdown
    /// channel (e.g. "HandelsrechnungNr." instead of "Handelsrechnung Nr.").
    #[test]
    fn cid_identity_run_sums_each_glyph_width() {
        let mut map = HashMap::new();
        map.insert(0x0028u32, 600.0);
        map.insert(0x0056u32, 500.0);
        let w = cid_widths(map, None);
        assert_eq!(w.width(&[0x00, 0x28, 0x00, 0x56]), Some(1100.0));
        // A single glyph still measures correctly.
        assert_eq!(w.width(&[0x00, 0x28]), Some(600.0));
        // A CID missing from /W contributes /DW.
        assert_eq!(w.width(&[0x00, 0x28, 0x00, 0x99]), Some(1600.0));
    }

    /// The custom code->CID `/Encoding` CMap path must likewise split a
    /// multi-code run and sum each code's width.
    #[test]
    fn cid_custom_encoding_run_sums_each_code_width() {
        let cmap = parse_cmap(b"2 beginbfchar\n<01> <0028>\n<02> <0056>\nendbfchar")
            .expect("parse cmap");
        let mut map = HashMap::new();
        map.insert(0x0028u32, 600.0);
        map.insert(0x0056u32, 500.0);
        let w = cid_widths(map, Some(cmap));
        assert_eq!(w.width(&[0x01, 0x02]), Some(1100.0));
        assert_eq!(w.width(&[0x02]), Some(500.0));
    }

    #[test]
    fn cid_empty_run_has_no_width() {
        assert_eq!(cid_widths(HashMap::new(), None).width(&[]), None);
    }

    /// A Type0 `/W` table whose entries are *indirect* objects (one shared
    /// width object referenced by many CIDs — the intarsys EN16931 layout)
    /// must be resolved through `deref`. Before the fix `num()` saw an
    /// `Object::Reference`, dropped every entry, and let each glyph fall back
    /// to `/DW` (1000), inflating run advances by 1000/600 = 5/3 and fusing
    /// neighbouring table cells ("Gesamtbetrag der Zuschläge0,00").
    #[test]
    fn resolve_widths_dereferences_indirect_w_values() {
        let mut doc = Document::new();
        let w600 = doc.add_object(Object::Integer(600));
        let w500 = doc.add_object(Object::Integer(500));
        // Range form `c_first c_last w`, each `w` indirect.
        let warr_compact = doc.add_object(Object::Array(vec![
            Object::Integer(3),
            Object::Integer(3),
            Object::Reference(w600),
            Object::Integer(8),
            Object::Integer(8),
            Object::Reference(w500),
        ]));
        // Array form `c [w1 w2]` with the widths themselves indirect.
        let warr_list = doc.add_object(Object::Array(vec![
            Object::Integer(20),
            Object::Array(vec![Object::Reference(w600), Object::Reference(w500)]),
        ]));

        for warr in [warr_compact, warr_list] {
            let mut cid = Dictionary::new();
            cid.set(b"Subtype", Object::Name(b"CIDFontType2".to_vec()));
            cid.set(b"W", Object::Reference(warr));
            let desc = doc.add_object(Object::Dictionary(cid));
            let mut font = Dictionary::new();
            font.set(b"Subtype", Object::Name(b"Type0".to_vec()));
            font.set(
                b"DescendantFonts",
                Object::Array(vec![Object::Reference(desc)]),
            );
            let widths = resolve_widths(&doc, &font);
            match &widths {
                Widths::Cid { map, default, .. } => {
                    assert_eq!(*default, 1000.0);
                    let (a, b) = if map.contains_key(&3) { (3u32, 8u32) } else { (20u32, 21u32) };
                    assert_eq!(map.get(&a), Some(&600.0), "indirect /W width for CID {a} must resolve");
                    assert_eq!(map.get(&b), Some(&500.0), "indirect /W width for CID {b} must resolve");
                    assert_eq!(widths.width(&[0x00, a as u8]), Some(600.0));
                }
                _ => panic!("expected Widths::Cid for Type0 font"),
            }
        }
    }

    #[test]
    fn test_append_table_zones_emits_table_and_suppresses_contained_fragments() {
        use crate::layout::reading_order::DocBlock;
        use crate::layout::tables::TableHit;
        use crate::models::BoundingBox;

        let mut blocks = vec![
            DocBlock {
                page: 1, kind: "heading".into(),
                x0: 40.0, y0: 700.0, x1: 300.0, y1: 720.0,
                text: "Table".into(), is_bold: false, is_italic: false, is_underline: false,
            },
            DocBlock {
                page: 1, kind: "body".into(),
                x0: 50.0, y0: 400.0, x1: 100.0, y1: 410.0,
                text: "cell a".into(), is_bold: false, is_italic: false, is_underline: false,
            },
            DocBlock {
                page: 1, kind: "list".into(),
                x0: 200.0, y0: 400.0, x1: 250.0, y1: 410.0,
                text: "- cell b".into(), is_bold: false, is_italic: false, is_underline: false,
            },
            DocBlock {
                page: 1, kind: "body".into(),
                x0: 50.0, y0: 200.0, x1: 300.0, y1: 210.0,
                text: "outside para".into(), is_bold: false, is_italic: false, is_underline: false,
            },
        ];
        let hit = TableHit {
            start: 0,
            end: 1,
            rows: vec![
                vec!["h1".to_string(), "h2".to_string()],
                vec!["a".to_string(), "b".to_string()],
            ],
            bbox: BoundingBox::new(45.0, 390.0, 260.0, 415.0),
        };
        append_table_zones(&mut blocks, &[hit]);

        assert_eq!(
            blocks.iter().filter(|b| b.kind == "table").count(),
            1,
            "exactly one table zone must be emitted"
        );
        assert_eq!(
            blocks.len(),
            3,
            "two contained fragments removed; heading + outside body + table remain: {:?}",
            blocks.iter().map(|b| b.kind.clone()).collect::<Vec<_>>()
        );
        assert!(blocks.iter().any(|b| b.kind == "heading"));
        assert!(blocks.iter().any(|b| b.text == "outside para"));
        assert!(blocks.iter().any(|b| b.text.starts_with("table ·")));
        assert!(!blocks.iter().any(|b| b.text == "cell a"));
        assert!(!blocks.iter().any(|b| b.text == "- cell b"));
    }

    #[test]
    fn test_append_table_zones_keeps_structure_inside_table() {
        use crate::layout::reading_order::DocBlock;
        use crate::layout::tables::TableHit;
        use crate::models::BoundingBox;

        let mut blocks = vec![DocBlock {
            page: 1, kind: "figure".into(),
            x0: 100.0, y0: 500.0, x1: 200.0, y1: 550.0,
            text: "[photo]".into(), is_bold: false, is_italic: false, is_underline: false,
        }];
        let hit = TableHit {
            start: 0,
            end: 0,
            rows: vec![vec!["x".to_string(), "y".to_string()]],
            bbox: BoundingBox::new(10.0, 10.0, 300.0, 600.0),
        };
        append_table_zones(&mut blocks, &[hit]);
        // A "figure" fragment inside the table bbox is NOT dropped.
        assert!(blocks.iter().any(|b| b.kind == "figure"));
    }

    #[test]
    fn test_append_table_zones_suppresses_fragments_on_table_edge_rows() {
        // Regression for synth_facture_btp_autoliquidation.pdf: `hit.bbox` is
        // baseline-based (`find_tables` uses the min/max span `y`), but
        // `build_doc_blocks` pads each fragment by half its font size. A
        // fragment on the table's first or last row overhangs the bbox by
        // ~0.5em, so the old strict-containment test never suppressed it: the
        // header cell `Qté` and the last-row cell `1400,00 €` (both padded to
        // y 625..635 / 589..599 around a 594..630 bbox) leaked as "body"
        // fragments next to the table zone, as did the cross-column paragraph
        // merge `Total HT 1`. A genuine prose line below the table must stay.
        use crate::layout::reading_order::DocBlock;
        use crate::layout::tables::TableHit;
        use crate::models::BoundingBox;

        fn frag(kind: &str, x0: f64, y0: f64, x1: f64, y1: f64, t: &str) -> DocBlock {
            DocBlock {
                page: 1,
                kind: kind.into(),
                x0,
                y0,
                x1,
                y1,
                text: t.into(),
                is_bold: false,
                is_italic: false,
                is_underline: false,
            }
        }

        let mut blocks = vec![
            // Header-row cell: baseline 630, padded to 625..635 (overhangs y1).
            frag("body", 300.0, 625.0, 316.0, 635.0, "Qté"),
            // Last-row cell: baseline 594, padded to 589..599 (overhangs y0).
            frag("body", 350.0, 589.0, 394.5, 599.0, "1400,00 €"),
            // Cross-column paragraph merge inside the grid.
            frag("body", 300.0, 607.0, 508.0, 635.0, "Total HT 1"),
            // Genuine prose just below the table: centre outside the bbox.
            frag("body", 56.0, 650.0, 300.0, 660.0, "Autoliquidation"),
        ];
        let hit = TableHit {
            start: 0,
            end: 2,
            rows: vec![
                vec!["Désignation".to_string(), "Qté".to_string()],
                vec!["x".to_string(), "1".to_string()],
                vec!["y".to_string(), "2".to_string()],
            ],
            // Baseline bbox, exactly as find_tables builds it.
            bbox: BoundingBox::new(56.0, 594.0, 514.5, 630.0),
        };
        append_table_zones(&mut blocks, &[hit]);

        assert_eq!(
            blocks.iter().filter(|b| b.kind == "table").count(),
            1,
            "exactly one table zone must be emitted"
        );
        for dropped in ["Qté", "1400,00 €", "Total HT 1"] {
            assert!(
                !blocks.iter().any(|b| b.text == dropped),
                "table-edge fragment {dropped:?} must be collapsed into the table zone: {:?}",
                blocks.iter().map(|b| b.text.clone()).collect::<Vec<_>>()
            );
        }
        assert!(
            blocks.iter().any(|b| b.text == "Autoliquidation"),
            "prose outside the table bbox must survive: {:?}",
            blocks.iter().map(|b| b.text.clone()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_append_table_zones_single_column_layout_wrapper_keeps_prose() {
        // R4/F7: a single-column layout wrapper (a `Table`/`TR`/`TD` structure
        // holding stacked prose rather than tabular data) carries one cell per
        // row. It must neither emit a fake `table` zone nor suppress the
        // paragraph blocks inside its bbox.
        use crate::layout::reading_order::DocBlock;
        use crate::layout::tables::TableHit;
        use crate::models::BoundingBox;

        fn frag(kind: &str, x0: f64, y0: f64, x1: f64, y1: f64, t: &str) -> DocBlock {
            DocBlock {
                page: 1,
                kind: kind.into(),
                x0,
                y0,
                x1,
                y1,
                text: t.into(),
                is_bold: false,
                is_italic: false,
                is_underline: false,
            }
        }

        let para_a = "Le présent document décrit les conditions générales et s'applique à compter de sa date de signature.";
        let para_b = "Une seconde phrase de paragraphe ordinaire.";
        let mut blocks = vec![
            frag("body", 50.0, 400.0, 300.0, 420.0, para_a),
            frag("body", 50.0, 360.0, 300.0, 380.0, para_b),
        ];
        let hit = TableHit {
            start: 0,
            end: 1,
            rows: vec![vec![para_a.to_string()], vec![para_b.to_string()]],
            bbox: BoundingBox::new(45.0, 350.0, 305.0, 425.0),
        };
        append_table_zones(&mut blocks, &[hit]);

        assert!(
            !blocks.iter().any(|b| b.kind == "table"),
            "single-column layout wrapper must not emit a table zone: {:?}",
            blocks.iter().map(|b| (b.kind.clone(), b.text.clone())).collect::<Vec<_>>()
        );
        assert_eq!(
            blocks.iter().filter(|b| b.kind == "body").count(),
            2,
            "both prose paragraphs must survive: {:?}",
            blocks.iter().map(|b| b.text.clone()).collect::<Vec<_>>()
        );
        assert!(blocks.iter().any(|b| b.text == para_a));
    }

    #[test]
    fn test_faux_bold_overstrike_deduplication() {
        let spans = vec![
            Span {
                text: "B".into(),
                x: 100.0,
                y: 200.0,
                size: 12.0,
                advance: 8.0,
                word_advance: 8.0,
                is_bold: false,
                is_italic: false,
                is_underline: false,
                is_vertical: false,
            },
            Span {
                text: "B".into(),
                x: 100.2,
                y: 200.0,
                size: 12.0,
                advance: 8.0,
                word_advance: 8.0,
                is_bold: false,
                is_italic: false,
                is_underline: false,
                is_vertical: false,
            },
            Span {
                text: "U".into(),
                x: 108.0,
                y: 200.0,
                size: 12.0,
                advance: 8.0,
                word_advance: 8.0,
                is_bold: false,
                is_italic: false,
                is_underline: false,
                is_vertical: false,
            },
            Span {
                text: "U".into(),
                x: 108.2,
                y: 200.0,
                size: 12.0,
                advance: 8.0,
                word_advance: 8.0,
                is_bold: false,
                is_italic: false,
                is_underline: false,
                is_vertical: false,
            },
            Span {
                text: "L".into(),
                x: 116.0,
                y: 200.0,
                size: 12.0,
                advance: 8.0,
                word_advance: 8.0,
                is_bold: false,
                is_italic: false,
                is_underline: false,
                is_vertical: false,
            },
            Span {
                text: "L".into(),
                x: 116.2,
                y: 200.0,
                size: 12.0,
                advance: 8.0,
                word_advance: 8.0,
                is_bold: false,
                is_italic: false,
                is_underline: false,
                is_vertical: false,
            },
            // Legitimate second 'L' in BULLETIN at normal horizontal offset:
            Span {
                text: "L".into(),
                x: 124.0,
                y: 200.0,
                size: 12.0,
                advance: 8.0,
                word_advance: 8.0,
                is_bold: false,
                is_italic: false,
                is_underline: false,
                is_vertical: false,
            },
            Span {
                text: "L".into(),
                x: 124.2,
                y: 200.0,
                size: 12.0,
                advance: 8.0,
                word_advance: 8.0,
                is_bold: false,
                is_italic: false,
                is_underline: false,
                is_vertical: false,
            },
        ];
        let lines = build_lines(&spans);
        assert_eq!(lines.len(), 1);
        let text: String = lines[0].iter().map(|s| s.text.as_str()).collect();
        assert_eq!(text, "BULL");
        assert!(lines[0][0].is_bold);
        assert_eq!(lines[0].len(), 4);
    }

    #[test]
    fn flush_path_segs_keeps_only_horizontal_thin_rules() {
        let mut path = vec![(100.0, 200.0), (300.0, 200.0), (300.0, 201.0)];
        let mut segs: Vec<(f64, f64, f64)> = Vec::new();
        flush_path_segs(&mut path, Some((100.0, 200.0)), &mut segs, false);
        assert_eq!(segs, vec![(200.0, 100.0, 300.0)], "only the horizontal rule survives");
    }

    #[test]
    fn flush_path_segs_ignores_a_box_border_edge() {
        // A hyperlink annotation box drawn as `m`/`l`/`h` is ~13pt tall; its top
        // and bottom edges must NOT become "underline" rules, or the top edge
        // underlines the text line just above the box (the FR ACRE slide's URL
        // box underlined " au plus tard dans les 45 jours suiv").
        let mut path = vec![
            (198.1, 257.5),
            (260.4, 257.5),
            (260.4, 244.1),
            (198.1, 244.1),
        ];
        let mut segs: Vec<(f64, f64, f64)> = Vec::new();
        flush_path_segs(&mut path, Some((198.1, 257.5)), &mut segs, true);
        assert!(segs.is_empty(), "a box border must not yield underline rules, got {segs:?}");
    }

    #[test]
    fn mark_underlines_flags_span_below_a_rule() {
        // Baseline at y=700; a thin rule at y=698 (2pt below, well within 0.5*size).
        let mut lines = vec![vec![Span {
            text: "Underlined".into(),
            x: 100.0,
            y: 700.0,
            size: 12.0,
            advance: 60.0,
            word_advance: 60.0,
            is_bold: false,
            is_italic: false,
            is_underline: false,
            is_vertical: false,
        }]];
        let segs: Vec<(f64, f64, f64)> = vec![(698.0, 100.0, 160.0)];
        let covered: std::collections::HashSet<usize> = Default::default();
        mark_underlines(&mut lines, &segs, &covered);
        assert!(lines[0][0].is_underline, "span directly above a rule must be underlined");
    }
