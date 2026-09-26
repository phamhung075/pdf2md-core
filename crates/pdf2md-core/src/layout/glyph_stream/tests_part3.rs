// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! PDF content-stream glyph extraction, font metric resolution, and 2D span aggregation.

use super::*;
use super::tests_common::*;
    

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
