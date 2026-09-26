// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! PDF content-stream glyph extraction, font metric resolution, and 2D span aggregation.

use super::*;
use super::tests_common::*;
    use lopdf::{dictionary, Stream};

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
