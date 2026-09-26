// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! PDF content-stream glyph extraction, font metric resolution, and 2D span aggregation.

    use super::*;

    fn cid_widths(map: HashMap<u32, f64>, encoding: Option<CMapCodec>) -> Widths {
        Widths::Cid {
            map,
            default: 1000.0,
            encoding,
        }
    }

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

    #[test]
    fn mark_underlines_skips_table_covered_lines() {
        let span = |text: &str, x: f64| Span {
            text: text.into(),
            x,
            y: 700.0,
            size: 12.0,
            advance: 20.0,
            word_advance: 20.0,
            is_bold: false,
            is_italic: false,
            is_underline: false,
            is_vertical: false,
        };
        let mut lines = vec![vec![span("cell", 100.0)]];
        let segs: Vec<(f64, f64, f64)> = vec![(698.0, 90.0, 160.0)];
        let covered: std::collections::HashSet<usize> = [0usize].into_iter().collect();
        mark_underlines(&mut lines, &segs, &covered);
        assert!(!lines[0][0].is_underline, "table row borders must not underline cell text");
    }

    #[test]
    fn mark_underlines_ignores_rule_far_below_baseline() {
        let mut lines = vec![vec![Span {
            text: "Body".into(),
            x: 100.0,
            y: 700.0,
            size: 12.0,
            advance: 30.0,
            word_advance: 30.0,
            is_bold: false,
            is_italic: false,
            is_underline: false,
            is_vertical: false,
        }]];
        // Rule 10pt below baseline (> 0.5 * 12) is a table border, not an underline.
        let segs: Vec<(f64, f64, f64)> = vec![(690.0, 90.0, 160.0)];
        let covered: std::collections::HashSet<usize> = Default::default();
        mark_underlines(&mut lines, &segs, &covered);
        assert!(!lines[0][0].is_underline, "rule too far below baseline is not an underline");
    }

    fn font_with_name(name: &[u8]) -> (Document, Dictionary) {
        let mut font = Dictionary::new();
        font.set(b"BaseFont", Object::Name(name.to_vec()));
        (Document::new(), font)
    }

    #[test]
    fn width_sums_every_glyph_in_a_run() {
        // A `Tj`/`TJ` operand is a whole word or phrase; `span.x + advance`
        // is used as the run's right edge. Only the first glyph's width must
        // not be used, or long runs under-measure and open phantom gaps.
        let mut t = [0.0f64; 256];
        t[b'A' as usize] = 600.0;
        t[b'B' as usize] = 700.0;
        let w = Widths::Byte(t);
        assert_eq!(w.width(b"AB"), Some(1300.0));
        assert_eq!(w.width(b"A"), Some(600.0));
        assert_eq!(w.width(b""), None);
    }

    #[test]
    fn resolve_font_style_recognizes_urw_medi_as_bold() {
        // LaTeX/Nimbus Roman embeds the bold face as *-Medi, which spells
        // neither "bold" nor "black".
        let (doc, font) = font_with_name(b"MCWOPG+NimbusRomNo9L-Medi");
        assert!(resolve_font_style(&doc, &font).0, "-Medi must be bold");
    }

    #[test]
    fn resolve_font_style_recognizes_urw_bd_as_bold() {
        let (doc, font) = font_with_name(b"VSGTBW+NimbusRomNo9L-Regu");
        assert!(!resolve_font_style(&doc, &font).0, "plain Regu must not be bold");
        let (doc, font) = font_with_name(b"ABCDEF+URWGothic-Bd");
        assert!(resolve_font_style(&doc, &font).0, "-Bd must be bold");
    }

    #[test]
    fn resolve_font_style_recognizes_computer_modern_cmbx_as_bold() {
        let (doc, font) = font_with_name(b"CMBX10");
        assert!(resolve_font_style(&doc, &font).0, "CMBX10 (Computer Modern bold) must be bold");
        let (doc, font) = font_with_name(b"SFBX12");
        assert!(resolve_font_style(&doc, &font).0, "SFBX12 (sans bold) must be bold");
        let (doc, font) = font_with_name(b"UDWEWE+CMR10");
        assert!(!resolve_font_style(&doc, &font).0, "CMR10 (regular) must not be bold");
    }

    #[test]
    fn resolve_font_style_uses_stemv_heuristic_when_fontweight_absent() {
        // /FontWeight is rarely present in embedded subsets; /StemV is the
        // practical bold signal. Regular faces sit at ~70–90, bold at 120+.
        let mut bold_fd = Dictionary::new();
        bold_fd.set(b"StemV", 130);
        let mut bold_font = Dictionary::new();
        bold_font.set(b"FontDescriptor", Object::Dictionary(bold_fd));
        let doc = Document::new();
        assert!(resolve_font_style(&doc, &bold_font).0, "StemV >= 120 must be bold");

        let mut light_fd = Dictionary::new();
        light_fd.set(b"StemV", 78);
        let mut light_font = Dictionary::new();
        light_font.set(b"FontDescriptor", Object::Dictionary(light_fd));
        assert!(!resolve_font_style(&doc, &light_font).0, "low StemV must not be bold");
    }

    /// Builds the smallest parseable sfnt program carrying one `OS/2` table
    /// whose `usWeightClass` is `weight`.
    fn sfnt_with_os2_weight(weight: u16) -> Vec<u8> {
        let mut os2 = vec![0u8; 64];
        os2[4..6].copy_from_slice(&weight.to_be_bytes());
        let mut out = Vec::new();
        out.extend_from_slice(&0x0001_0000u32.to_be_bytes()); // sfnt version
        out.extend_from_slice(&1u16.to_be_bytes()); // numTables
        out.extend_from_slice(&[0u8; 6]); // searchRange/entrySelector/rangeShift
        out.extend_from_slice(b"OS/2");
        out.extend_from_slice(&0u32.to_be_bytes()); // checksum (unused here)
        let offset = 12 + 16;
        out.extend_from_slice(&(offset as u32).to_be_bytes());
        out.extend_from_slice(&(os2.len() as u32).to_be_bytes());
        out.extend_from_slice(&os2);
        out
    }

    fn font_with_embedded_weight(weight: u16, stemv: i64) -> (Document, Dictionary) {
        let mut doc = Document::new();
        let file_id = doc.add_object(Object::Stream(lopdf::Stream::new(
            Dictionary::new(),
            sfnt_with_os2_weight(weight),
        )));
        let mut fd = Dictionary::new();
        fd.set(b"Flags", 6);
        fd.set(b"StemV", stemv);
        fd.set(b"FontFile2", Object::Reference(file_id));
        let mut font = Dictionary::new();
        font.set(b"BaseFont", Object::Name(b"CIDFont+F1".to_vec()));
        font.set(b"FontDescriptor", Object::Dictionary(fd));
        (doc, font)
    }

    /// Regression for the intarsys/ZUGFeRD all-bold bug: that stylesheet writes
    /// the same out-of-range `/StemV 600` on both its Regular and Bold subsets,
    /// so the stem-width threshold alone bolds every run. The embedded `OS/2`
    /// table is the authoritative weight signal.
    #[test]
    fn resolve_font_style_prefers_embedded_weight_over_bogus_stemv() {
        let (doc, regular) = font_with_embedded_weight(400, 600);
        assert!(
            !resolve_font_style(&doc, &regular).0,
            "OS/2 usWeightClass=400 must beat /StemV 600 (the every-run-bold bug)"
        );

        let (doc, bold) = font_with_embedded_weight(700, 600);
        assert!(
            resolve_font_style(&doc, &bold).0,
            "OS/2 usWeightClass=700 must still resolve as bold"
        );
    }

    /// Bug 6 regression: `arXiv:2310.06825v1 [cs.CL] 10 Oct 2023` sits in the
    /// left margin as vertical text. With `detect_layout: true` it must not be
    /// appended to the page body prose; with layout analysis off the legacy
    /// append behavior must remain (the text channel is the only consumer).
    #[test]
    fn vertical_margin_text_is_not_appended_to_body_when_detect_layout() {
        let margin = "arXiv:2310.06825v1 [cs.CL] 10 Oct 2023";

        let mut body = String::from("Introduction paragraph.");
        append_vertical_text(&mut body, margin, true);
        assert_eq!(
            body, "Introduction paragraph.",
            "layout analysis must keep margin stamps out of PageText.text"
        );
        assert!(!body.contains("arXiv"), "margin stamp leaked into body flow: {body:?}");

        append_vertical_text(&mut body, margin, false);
        assert!(
            body.ends_with(margin),
            "without layout analysis the legacy append must be preserved: {body:?}"
        );
    }

    /// A `/ToUnicode` map may return a Latin presentation-form ligature
    /// (U+FB00–U+FB06); it must be folded to its letter sequence so the output
    /// reads "fi scal", not "ﬁ scal".
    #[test]
    fn ligatures_fold_to_letter_sequences() {
        let mut s = "Num\u{FB01} scal \u{FB00}ort \u{FB03}cient \u{FB02}eur".to_string();
        fold_ligatures(&mut s);
        assert_eq!(s, "Numfi scal ffort fficient fleur");
    }

    // ------------------------------------------------------------------
    // B1: Form XObject recursion in the glyph engine
    // ------------------------------------------------------------------

    use lopdf::{dictionary, Stream};

    fn rl(v: f64) -> Object {
        Object::Real(v as f32)
    }

    fn show_at(x: f64, y: f64, text: &str) -> Vec<Operation> {
        vec![
            Operation::new(
                "Tm",
                vec![rl(1.0), rl(0.0), rl(0.0), rl(1.0), rl(x), rl(y)],
            ),
            Operation::new("Tj", vec![Object::string_literal(text.to_string())]),
        ]
    }

    fn form_dict(matrix: Option<[f64; 6]>, bbox: Option<[f64; 4]>) -> Dictionary {
        let mut fd = Dictionary::new();
        fd.set(b"Type", Object::Name(b"XObject".to_vec()));
        fd.set(b"Subtype", Object::Name(b"Form".to_vec()));
        if let Some(m) = matrix {
            fd.set(
                b"Matrix",
                vec![rl(m[0]), rl(m[1]), rl(m[2]), rl(m[3]), rl(m[4]), rl(m[5])],
            );
        }
        if let Some(b) = bbox {
            fd.set(b"BBox", vec![rl(b[0]), rl(b[1]), rl(b[2]), rl(b[3])]);
        }
        fd
    }

    /// One-page document whose `/Resources/XObject` maps each name to a form.
    /// Each form entry is `(name, form_ops, matrix, bbox, own_font_id)`.
    fn pdf_with_named_forms(
        page_ops: Vec<Operation>,
        forms: &[(&[u8], Vec<Operation>, Option<[f64; 6]>, Option<[f64; 4]>, Option<ObjectId>)],
    ) -> (Document, ObjectId) {
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let page_font = doc.add_object(dictionary! {
            "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Courier",
        });
        let mut xobjects = Dictionary::new();
        for (name, ops, matrix, bbox, own_font) in forms {
            let mut fd = form_dict(*matrix, *bbox);
            if let Some(fid) = own_font {
                let res = doc.add_object(dictionary! { "Font" => dictionary! { "F1" => *fid } });
                fd.set(b"Resources", Object::Reference(res));
            }
            let sid = doc.add_object(Stream::new(
                fd,
                Content { operations: ops.clone() }.encode().unwrap(),
            ));
            xobjects.set(name.to_vec(), Object::Reference(sid));
        }
        let page_res = doc.add_object(dictionary! {
            "Font" => dictionary! { "F1" => page_font },
            "XObject" => xobjects,
        });
        let content_id = doc.add_object(Stream::new(
            Dictionary::new(),
            Content { operations: page_ops }.encode().unwrap(),
        ));
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page", "Parent" => pages_id, "Contents" => content_id,
        });
        let pages = dictionary! {
            "Type" => "Pages", "Kids" => vec![page_id.into()], "Count" => 1,
            "Resources" => page_res,
            "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
        };
        doc.objects.insert(pages_id, Object::Dictionary(pages));
        let catalog_id = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        doc.trailer.set("Root", catalog_id);
        (doc, page_id)
    }

    fn walk_page(doc: &Document, page_id: ObjectId) -> (Vec<Span>, GlyphBudget) {
        let chain = crate::text_extract::resource_dicts(doc, page_id);
        let content = crate::text_extract::decode_page_content(doc, page_id).expect("content");
        let mut budget = GlyphBudget::new();
        let mut cache: HashMap<usize, Vec<GlyphFontInfo>> = HashMap::new();
        let mut seen = false;
        let (spans, _) = {
            let walk = walk_glyphs(
                doc,
                &chain,
                &content.operations,
                Mtx::ID,
                &mut Vec::new(),
                0,
                &mut seen,
                &mut budget,
                &mut cache,
            );
            (walk.spans, walk.underline_segs)
        };
        (spans, budget)
    }

    #[test]
    fn form_xobject_text_is_walked_with_page_resources() {
        let form_ops = {
            let mut v = vec![
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec![Object::Name(b"F1".to_vec()), rl(10.0)]),
            ];
            v.extend(show_at(50.0, 700.0, "FORM"));
            v.push(Operation::new("ET", vec![]));
            v
        };
        let (doc, page_id) = pdf_with_named_forms(
            vec![Operation::new("Do", vec![Object::Name(b"Fm0".to_vec())])],
            &[(b"Fm0", form_ops, None, None, None)],
        );
        let (spans, _) = walk_page(&doc, page_id);
        assert_eq!(spans.len(), 1, "form text must be extracted");
        assert_eq!(spans[0].text, "FORM");
        assert!((spans[0].x - 50.0).abs() < 0.01 && (spans[0].y - 700.0).abs() < 0.01);
    }

    #[test]
    fn form_matrix_and_page_cm_compose_correctly() {
        // Page cm scales by 2 and translates (100,100); form /Matrix translates
        // (10,20). A form-space origin must land at 2*(10,20)+(100,100).
        let form_ops = {
            let mut v = vec![
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec![Object::Name(b"F1".to_vec()), rl(10.0)]),
            ];
            v.extend(show_at(0.0, 0.0, "X"));
            v.push(Operation::new("ET", vec![]));
            v
        };
        let page_ops = vec![
            Operation::new("q", vec![]),
            Operation::new("cm", vec![rl(2.0), rl(0.0), rl(0.0), rl(2.0), rl(100.0), rl(100.0)]),
            Operation::new("Do", vec![Object::Name(b"Fm0".to_vec())]),
            Operation::new("Q", vec![]),
        ];
        let (doc, page_id) = pdf_with_named_forms(
            page_ops,
            &[(b"Fm0", form_ops, Some([1.0, 0.0, 0.0, 1.0, 10.0, 20.0]), None, None)],
        );
        let (spans, _) = walk_page(&doc, page_id);
        assert_eq!(spans.len(), 1);
        assert!(
            (spans[0].x - 120.0).abs() < 0.01 && (spans[0].y - 140.0).abs() < 0.01,
            "composed device position wrong: ({}, {})",
            spans[0].x,
            spans[0].y
        );
        assert!((spans[0].size - 20.0).abs() < 0.01, "cm scale must apply to the size");
    }

    #[test]
    fn form_local_font_overrides_the_page_font_of_the_same_name() {
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let page_font = doc.add_object(dictionary! {
            "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Courier",
        });
        let cmap = doc.add_object(Stream::new(
            Dictionary::new(),
            b"begincmap\nbeginbfchar\n<41> <005A>\nendbfchar\nendcmap\n".to_vec(),
        ));
        let form_font = doc.add_object(dictionary! {
            "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Courier", "ToUnicode" => cmap,
        });
        let res = doc.add_object(dictionary! { "Font" => dictionary! { "F1" => form_font } });
        let mut fd = form_dict(None, None);
        fd.set(b"Resources", Object::Reference(res));
        let form_ops = vec![
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec![Object::Name(b"F1".to_vec()), rl(10.0)]),
            Operation::new("Tj", vec![Object::string_literal("A".to_string())]),
            Operation::new("ET", vec![]),
        ];
        let form_id = doc.add_object(Stream::new(
            fd,
            Content { operations: form_ops }.encode().unwrap(),
        ));
        let page_res = doc.add_object(dictionary! {
            "Font" => dictionary! { "F1" => page_font },
            "XObject" => dictionary! { "Fm0" => form_id },
        });
        let page_ops = vec![
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec![Object::Name(b"F1".to_vec()), rl(10.0)]),
            Operation::new("Tj", vec![Object::string_literal("A".to_string())]),
            Operation::new("Td", vec![rl(0.0), rl(-20.0)]),
            Operation::new("Tj", vec![Object::string_literal("A".to_string())]),
            Operation::new("ET", vec![]),
            Operation::new("Do", vec![Object::Name(b"Fm0".to_vec())]),
        ];
        let content_id = doc.add_object(Stream::new(
            Dictionary::new(),
            Content { operations: page_ops }.encode().unwrap(),
        ));
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page", "Parent" => pages_id, "Contents" => content_id,
        });
        let pages = dictionary! {
            "Type" => "Pages", "Kids" => vec![page_id.into()], "Count" => 1,
            "Resources" => page_res,
            "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
        };
        doc.objects.insert(pages_id, Object::Dictionary(pages));
        let catalog_id = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        doc.trailer.set("Root", catalog_id);

        let (spans, _) = walk_page(&doc, page_id);
        let page_a = spans.iter().filter(|s| s.text == "A").count();
        let form_z = spans.iter().filter(|s| s.text == "Z").count();
        assert_eq!(page_a, 2, "page /F1 must still decode to 'A'");
        assert_eq!(form_z, 1, "the form's own /F1 must decode to 'Z'");
    }

    #[test]
    fn form_bbox_clips_a_span_outside_it() {
        // Two runs: one inside the BBox, one far to the right of it.
        let form_ops = {
            let mut v = vec![
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec![Object::Name(b"F1".to_vec()), rl(10.0)]),
            ];
            v.extend(show_at(50.0, 700.0, "IN"));
            v.extend(show_at(400.0, 700.0, "OUT"));
            v.push(Operation::new("ET", vec![]));
            v
        };
        let (doc, page_id) = pdf_with_named_forms(
            vec![Operation::new("Do", vec![Object::Name(b"Fm0".to_vec())])],
            &[(b"Fm0", form_ops, None, Some([40.0, 600.0, 120.0, 720.0]), None)],
        );
        let (spans, _) = walk_page(&doc, page_id);
        assert_eq!(spans.len(), 1, "only the span inside the form /BBox survives: {spans:?}");
        assert_eq!(spans[0].text, "IN");
    }

    #[test]
    fn form_drawn_twice_yields_two_runs() {
        let form_ops = {
            let mut v = vec![
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec![Object::Name(b"F1".to_vec()), rl(10.0)]),
            ];
            v.extend(show_at(50.0, 700.0, "AB"));
            v.push(Operation::new("ET", vec![]));
            v
        };
        let page_ops = vec![
            Operation::new("Do", vec![Object::Name(b"Fm0".to_vec())]),
            Operation::new("Do", vec![Object::Name(b"Fm0".to_vec())]),
        ];
        let (doc, page_id) =
            pdf_with_named_forms(page_ops, &[(b"Fm0", form_ops, None, None, None)]);
        let (spans, budget) = walk_page(&doc, page_id);
        assert_eq!(spans.len(), 2, "a form drawn twice contributes two runs");
        assert_eq!(budget.do_left, MAX_FORM_DO_PER_PAGE - 2);
    }

    #[test]
    fn self_referential_form_terminates() {
        // One form object exposed under two names; its content does `/B Do`,
        // which resolves back to the same object and must be skipped.
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let page_font = doc.add_object(dictionary! {
            "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Courier",
        });
        let form_ops = vec![
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec![Object::Name(b"F1".to_vec()), rl(10.0)]),
            Operation::new("Tj", vec![Object::string_literal("R".to_string())]),
            Operation::new("ET", vec![]),
            Operation::new("Do", vec![Object::Name(b"B".to_vec())]),
        ];
        let form_id = doc.add_object(Stream::new(
            form_dict(None, None),
            Content { operations: form_ops }.encode().unwrap(),
        ));
        let page_res = doc.add_object(dictionary! {
            "Font" => dictionary! { "F1" => page_font },
            "XObject" => dictionary! { "A" => form_id, "B" => form_id },
        });
        let content_id = doc.add_object(Stream::new(
            Dictionary::new(),
            Content { operations: vec![Operation::new("Do", vec![Object::Name(b"A".to_vec())])] }
                .encode()
                .unwrap(),
        ));
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page", "Parent" => pages_id, "Contents" => content_id,
        });
        let pages = dictionary! {
            "Type" => "Pages", "Kids" => vec![page_id.into()], "Count" => 1,
            "Resources" => page_res,
            "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
        };
        doc.objects.insert(pages_id, Object::Dictionary(pages));
        let catalog_id = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        doc.trailer.set("Root", catalog_id);

        let (spans, _) = walk_page(&doc, page_id);
        assert_eq!(spans.len(), 1, "a self-referential form must be walked once");
    }

    #[test]
    fn wide_form_expansion_hits_the_do_budget_and_terminates() {
        // One form issues far more `Do`s than the budget allows; without the
        // shared budget the page would decode/process all of them.
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let page_font = doc.add_object(dictionary! {
            "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Courier",
        });
        // Leaf form: draws one glyph.
        let leaf_ops = {
            let mut v = vec![
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec![Object::Name(b"F1".to_vec()), rl(10.0)]),
            ];
            v.extend(show_at(10.0, 10.0, "L"));
            v.push(Operation::new("ET", vec![]));
            v
        };
        let mut leaf_fd = form_dict(None, None);
        leaf_fd.set(
            b"Resources",
            Object::Dictionary(dictionary! { "Font" => dictionary! { "F1" => page_font } }),
        );
        let leaf_id = doc.add_object(Stream::new(
            leaf_fd,
            Content { operations: leaf_ops }.encode().unwrap(),
        ));
        // Parent form: `n` Do's of the leaf, more than the budget.
        let n = MAX_FORM_DO_PER_PAGE + 88;
        let mut parent_ops = Vec::new();
        for _ in 0..n {
            parent_ops.push(Operation::new("Do", vec![Object::Name(b"N".to_vec())]));
        }
        let mut parent_fd = form_dict(None, None);
        parent_fd.set(
            b"Resources",
            Object::Dictionary(dictionary! {
                "Font" => dictionary! { "F1" => page_font },
                "XObject" => dictionary! { "N" => leaf_id },
            }),
        );
        let parent_id = doc.add_object(Stream::new(
            parent_fd,
            Content { operations: parent_ops }.encode().unwrap(),
        ));
        let page_res = doc.add_object(dictionary! {
            "Font" => dictionary! { "F1" => page_font },
            "XObject" => dictionary! { "Fm0" => parent_id },
        });
        let content_id = doc.add_object(Stream::new(
            Dictionary::new(),
            Content { operations: vec![Operation::new("Do", vec![Object::Name(b"Fm0".to_vec())])] }
                .encode()
                .unwrap(),
        ));
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page", "Parent" => pages_id, "Contents" => content_id,
        });
        let pages = dictionary! {
            "Type" => "Pages", "Kids" => vec![page_id.into()], "Count" => 1,
            "Resources" => page_res,
            "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
        };
        doc.objects.insert(pages_id, Object::Dictionary(pages));
        let catalog_id = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        doc.trailer.set("Root", catalog_id);

        let (spans, budget) = walk_page(&doc, page_id);
        assert_eq!(budget.do_left, 0, "the shared Do budget must be exhausted");
        assert!(
            spans.len() <= MAX_FORM_DO_PER_PAGE,
            "expansion must stop at the budget, got {} spans",
            spans.len()
        );
    }

    #[test]
    fn many_distinct_forms_are_all_walked() {
        // 1200 distinct one-word forms, each invoked once (the shape that the
        // old 512 `Do`/page cap truncated): every form must contribute a span.
        let n = 1200usize;
        let names: Vec<Vec<u8>> = (0..n).map(|i| format!("Fm{i}").into_bytes()).collect();
        let forms: Vec<(&[u8], Vec<Operation>, Option<[f64; 6]>, Option<[f64; 4]>, Option<ObjectId>)> =
            names
                .iter()
                .map(|nm| {
                    let mut ops = vec![
                        Operation::new("BT", vec![]),
                        Operation::new("Tf", vec![Object::Name(b"F1".to_vec()), rl(10.0)]),
                    ];
                    ops.extend(show_at(10.0, 700.0, std::str::from_utf8(nm).unwrap()));
                    ops.push(Operation::new("ET", vec![]));
                    (nm.as_slice(), ops, None, None, None)
                })
                .collect();
        let page_ops: Vec<Operation> = (0..n)
            .map(|i| {
                Operation::new("Do", vec![Object::Name(format!("Fm{i}").into_bytes())])
            })
            .collect();
        let (doc, page_id) = pdf_with_named_forms(page_ops, &forms);
        let (spans, budget) = walk_page(&doc, page_id);
        assert_eq!(spans.len(), n, "all distinct forms must contribute a span");
        assert_eq!(budget.do_left, MAX_FORM_DO_PER_PAGE - n);
    }

    #[test]
    fn a_form_repeated_600_times_yields_600_runs() {
        // The same form drawn 600 times at distinct positions is content, not
        // overdraw; the old 512 cap silently dropped the last 88.
        let form_ops = {
            let mut v = vec![
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec![Object::Name(b"F1".to_vec()), rl(10.0)]),
            ];
            v.extend(show_at(10.0, 700.0, "R"));
            v.push(Operation::new("ET", vec![]));
            v
        };
        let n = 600usize;
        let mut page_ops = Vec::new();
        for i in 0..n {
            page_ops.push(Operation::new("q", vec![]));
            page_ops.push(Operation::new(
                "cm",
                vec![
                    rl(1.0), rl(0.0), rl(0.0), rl(1.0),
                    rl(10.0 + i as f64 * 4.0), rl(700.0),
                ],
            ));
            page_ops.push(Operation::new("Do", vec![Object::Name(b"Fm0".to_vec())]));
            page_ops.push(Operation::new("Q", vec![]));
        }
        let (doc, page_id) =
            pdf_with_named_forms(page_ops, &[(b"Fm0", form_ops, None, None, None)]);
        let (spans, budget) = walk_page(&doc, page_id);
        assert_eq!(spans.len(), n, "600 distinct placements must survive");
        assert_eq!(budget.do_left, MAX_FORM_DO_PER_PAGE - n);
    }

    #[test]
    fn form_text_feeds_table_detection() {
        let mut form_ops = vec![
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec![Object::Name(b"F1".to_vec()), rl(10.0)]),
        ];
        let header = ["Ref", "Qty", "Total"];
        let rows = [
            ["A1", "2", "10,00"],
            ["B2", "1", "20,00"],
            ["C3", "5", "30,00"],
        ];
        for (ri, row) in std::iter::once(&header).chain(rows.iter()).enumerate() {
            let y = 700.0 - ri as f64 * 20.0;
            for (ci, cell) in row.iter().enumerate() {
                form_ops.extend(show_at(50.0 + ci as f64 * 100.0, y, cell));
            }
        }
        form_ops.push(Operation::new("ET", vec![]));
        let (doc, page_id) = pdf_with_named_forms(
            vec![Operation::new("Do", vec![Object::Name(b"Fm0".to_vec())])],
            &[(b"Fm0", form_ops, None, None, None)],
        );
        let pt = extract_page_glyphs(&doc, page_id, true, true, false).expect("page text");
        assert!(pt.tables >= 1, "form grid must be recovered as a table");
        assert!(pt.text.contains("---"), "GFM table separator missing:\n{}", pt.text);
    }

    // ------------------------------------------------------------------
    // B2: inherited page geometry and dominantly-vertical pages
    // ------------------------------------------------------------------

    fn pdf_page_with(
        page_ops: Vec<Operation>,
        page_rotate: Option<i64>,
        pages_rotate: Option<i64>,
        media: [f64; 4],
    ) -> (Document, ObjectId) {
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let font = doc.add_object(dictionary! {
            "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Courier",
        });
        let res = doc.add_object(dictionary! { "Font" => dictionary! { "F1" => font } });
        let content = doc.add_object(Stream::new(
            Dictionary::new(),
            Content { operations: page_ops }.encode().unwrap(),
        ));
        let mut pd = dictionary! {
            "Type" => "Page", "Parent" => pages_id, "Contents" => content, "Resources" => res,
        };
        if let Some(r) = page_rotate {
            pd.set(b"Rotate", r);
        }
        let page_id = doc.add_object(pd);
        let mut pages = dictionary! {
            "Type" => "Pages", "Kids" => vec![page_id.into()], "Count" => 1,
            "MediaBox" => vec![rl(media[0]), rl(media[1]), rl(media[2]), rl(media[3])],
        };
        if let Some(r) = pages_rotate {
            pages.set(b"Rotate", r);
        }
        doc.objects.insert(pages_id, Object::Dictionary(pages));
        let cat = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        doc.trailer.set("Root", cat);
        (doc, page_id)
    }

    fn tm_at(a: f64, b: f64, c: f64, d: f64, e: f64, f: f64) -> Operation {
        Operation::new(
            "Tm",
            vec![rl(a), rl(b), rl(c), rl(d), rl(e), rl(f)],
        )
    }

    /// A page rotated 90° clockwise by `/Rotate` must apply the rotation matrix
    /// (and report the swapped height), even when the attribute is inherited
    /// from `/Pages` rather than set on the page.
    #[test]
    fn rotate_is_read_from_the_page_and_inherited_from_pages() {
        let ops = vec![Operation::new("BT", vec![])];
        let (doc, page_id) = pdf_page_with(ops.clone(), Some(90), None, [0.0, 0.0, 200.0, 100.0]);
        let (m, h) = page_initial_transform(&doc, page_id);
        assert!((m.a - 0.0).abs() < 1e-9 && (m.b + 1.0).abs() < 1e-9, "rotate 90 matrix wrong: {m:?}");
        assert!((m.c - 1.0).abs() < 1e-9 && (m.d - 0.0).abs() < 1e-9);
        assert!((h - 200.0).abs() < 1e-9, "rotated display height must swap to width");

        // No page /Rotate, but 270° on the /Pages ancestor: inherited.
        let (doc2, pid2) = pdf_page_with(ops, None, Some(270), [0.0, 0.0, 200.0, 100.0]);
        let (m2, h2) = page_initial_transform(&doc2, pid2);
        assert!((m2.a - 0.0).abs() < 1e-9 && (m2.b - 1.0).abs() < 1e-9, "inherited rotate wrong: {m2:?}");
        assert!((m2.c + 1.0).abs() < 1e-9 && (m2.d - 0.0).abs() < 1e-9);
        assert!((h2 - 200.0).abs() < 1e-9);
    }

    /// A page whose text is entirely vertical (a sideways OCR layer) must be
    /// rotated upright and read, not dropped as margin furniture.
    #[test]
    fn dominantly_vertical_page_is_rotated_upright() {
        let mut ops = vec![
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec![Object::Name(b"F1".to_vec()), rl(10.0)]),
        ];
        // Bottom-to-top reading (upward): text x-axis maps to device +y.
        for (i, ch) in ["S", "I", "D", "E"].iter().enumerate() {
            ops.push(tm_at(0.0, 1.0, -1.0, 0.0, 100.0, 700.0 + i as f64 * 10.0));
            ops.push(Operation::new("Tj", vec![Object::string_literal(ch.to_string())]));
        }
        ops.push(Operation::new("ET", vec![]));
        let (doc, page_id) = pdf_page_with(ops, None, None, [0.0, 0.0, 595.0, 842.0]);
        let pt = extract_page_glyphs(&doc, page_id, true, true, false).expect("page text");
        let letters: String = pt.text.split_whitespace().collect();
        assert!(letters.contains("SIDE"), "vertical page was not rotated upright: {:?}", pt.text);
        assert!(pt.text_ops_seen);
    }

    /// A minority vertical run — a page stamp or letterhead — must still be
    /// dropped from the body when layout analysis is on, and must not trigger
    /// the dominant-vertical rotation.
    #[test]
    fn minority_vertical_stamp_stays_out_of_a_horizontal_body() {
        let ops = vec![
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec![Object::Name(b"F1".to_vec()), rl(10.0)]),
            tm_at(1.0, 0.0, 0.0, 1.0, 50.0, 500.0),
            Operation::new("Tj", vec![Object::string_literal("BODY TEXT".to_string())]),
            tm_at(0.0, 1.0, -1.0, 0.0, 20.0, 100.0),
            Operation::new("Tj", vec![Object::string_literal("STAMP".to_string())]),
            Operation::new("ET", vec![]),
        ];
        let (doc, page_id) = pdf_page_with(ops, None, None, [0.0, 0.0, 595.0, 842.0]);
        let pt = extract_page_glyphs(&doc, page_id, true, true, false).expect("page text");
        assert!(pt.text.contains("BODY"), "horizontal body must survive: {:?}", pt.text);
        assert!(!pt.text.contains("STAMP"), "minority stamp leaked into the body: {:?}", pt.text);
        assert!(
            pt.blocks.iter().any(|b| b.kind == "margin" && b.text.contains("STAMP")),
            "the stamp must still be reported as a margin block"
        );
    }