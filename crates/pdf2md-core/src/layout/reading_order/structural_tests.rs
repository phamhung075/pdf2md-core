// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Reading order recovery, multi-column stream separation, and structured DocBlock generation.

use super::*;
use super::structural_tests_common::*;

    // -- detect_heading_level ------------------------------------------------

    #[test]
    fn h1_for_a_line_at_1_6x_body() {
        let line = one_span_line("Chapter One", BODY * 1.6, false);
        assert_eq!(detect_heading_level(&line, BODY), Some(1));
    }

    #[test]
    fn h1_for_a_bold_line_at_1_4x_body() {
        let line = one_span_line("Chapter One", BODY * 1.4, true);
        assert_eq!(detect_heading_level(&line, BODY), Some(1));
    }

    #[test]
    fn h2_for_a_line_at_1_3x_body() {
        let line = one_span_line("Section 1.1", BODY * 1.3, false);
        assert_eq!(detect_heading_level(&line, BODY), Some(2));
    }

    #[test]
    fn h3_for_a_bold_line_at_1_15x_body() {
        let line = one_span_line("Subsection", BODY * 1.15, true);
        assert_eq!(detect_heading_level(&line, BODY), Some(3));
    }

    #[test]
    fn non_bold_line_at_1_15x_body_is_not_a_heading() {
        // H3 requires bold; size alone at this ratio is not enough.
        let line = one_span_line("Subsection", BODY * 1.15, false);
        assert_eq!(detect_heading_level(&line, BODY), None);
    }

    #[test]
    fn body_sized_bold_text_is_not_a_heading() {
        let line = one_span_line("Just a bold word", BODY, true);
        assert_eq!(detect_heading_level(&line, BODY), None);
    }

    #[test]
    fn trailing_period_on_non_alphabetic_content_is_not_a_heading() {
        // A stray section/page-number fragment (no letters at all) ending in a
        // full stop — e.g. a ToC dot-leader remnant — must not read as a title
        // just because it happens to be large.
        let line = one_span_line("1.2.3.", BODY * 1.6, true);
        assert_eq!(detect_heading_level(&line, BODY), None);
    }

    #[test]
    fn three_or_more_sentences_is_not_a_heading() {
        // Two internal ". " separators (three sentences) reads as a dense
        // prose line rather than a title, regardless of size/boldness.
        let line = one_span_line("One. Two. Three.", BODY * 1.6, true);
        assert_eq!(detect_heading_level(&line, BODY), None);
    }

    #[test]
    fn a_single_sentence_can_still_be_a_heading_when_large_enough() {
        // Only an all-numeric/symbol trailing period, or 3+ sentences, is
        // excluded — an ordinary sentence-like heading ending in a period
        // (e.g. "Chapter 1.") is not penalized just for having a full stop.
        let line = one_span_line("This is a complete sentence.", BODY * 1.6, true);
        assert_eq!(detect_heading_level(&line, BODY), Some(1));
    }

    #[test]
    fn empty_line_is_not_a_heading() {
        let line = one_span_line("   ", BODY * 1.6, true);
        assert_eq!(detect_heading_level(&line, BODY), None);
    }

    #[test]
    fn lowercase_continuation_is_not_a_heading() {
        // Regression: a bold 12pt emphasis run that wraps list item 1
        // ("cliquez sur « Connexion »") sits at ~1.15x body and must stay in
        // the list item instead of being promoted to an H3.
        let line = one_span_line("cliquez sur « Connexion »", BODY * 1.15, true);
        assert_eq!(detect_heading_level(&line, BODY), None);
    }

    #[test]
    fn lowercase_continuation_at_h2_size_is_not_a_heading() {
        // The guard covers the whole sub-H1 band (< 1.4x body), not just H3.
        let line = one_span_line("de la Caf au service", BODY * 1.3, true);
        assert_eq!(detect_heading_level(&line, BODY), None);
    }

    #[test]
    fn long_bold_sentence_ending_in_period_is_not_a_heading() {
        // Regression: a 100%-bold 12pt prose sentence in the H3 size band is
        // an emphasized body sentence, not a section heading.
        let sentence =
            "Il vous sera demandé d'utiliser une adresse mail différente pour chaque compte.";
        assert!(sentence.chars().count() > 50);
        let line = one_span_line(sentence, BODY * 1.15, true);
        assert_eq!(detect_heading_level(&line, BODY), None);
    }

    #[test]
    fn short_bold_heading_ending_in_period_is_still_a_heading() {
        // The long-sentence guard must not swallow a short H3 label that
        // merely happens to end in a full stop.
        let line = one_span_line("Résumé.", BODY * 1.15, true);
        assert_eq!(detect_heading_level(&line, BODY), Some(3));
    }

    #[test]
    fn uniform_small_table_text_does_not_become_the_body_size() {
        // Regression: a page whose small (7pt) table/caption cells are perfectly
        // uniform while the real 10pt prose carries ordinary metric jitter
        // (9.8/9.9/10.0/10.1/10.2). The old per-0.1pt mode saw 16 identical 7pt
        // lines beat each 4-line prose bin and returned 6.97; every real body
        // line then measured >= 1.3 * body and was emitted as a `##` heading
        // (observed on scratch/samples/mistral-pdf-tests.pdf page 6).
        let mut lines: Vec<Vec<Span>> = Vec::new();
        for _ in 0..16 {
            lines.push(one_span_line("cell", 7.0, false));
        }
        for i in 0..20 {
            let size = [9.8, 9.9, 10.0, 10.1, 10.2][i % 5];
            lines.push(one_span_line(&format!("Body line {i}"), size, false));
        }

        let body = body_size_for(&lines);
        assert!(
            body >= 9.5,
            "body size must follow the 10pt prose cluster, not the 7pt table: got {body}"
        );

        // The prose line itself must stay ordinary body text, not a heading.
        let prose = one_span_line("Our work demonstrates that models compress knowledge", 10.0, false);
        assert_eq!(
            detect_heading_level(&prose, body),
            None,
            "a body-sized prose line must not be promoted to a heading"
        );
    }

    #[test]
    fn dash_bullet_is_detected_unordered() {
        let line = bulleted_line("-");
        let (ordered, skip) = detect_list_marker(&line).expect("must detect bullet");
        assert!(!ordered);
        assert_eq!(skip, 2, "marker span + trailing space span skipped");
    }

    #[test]
    fn bullet_glyph_variants_are_detected() {
        for marker in ["•", "●", "◦", "▪", "+", "*"] {
            let line = bulleted_line(marker);
            assert!(
                detect_list_marker(&line).is_some(),
                "expected {marker:?} to be recognized as a bullet"
            );
        }
    }

    #[test]
    fn ordered_dot_marker_is_detected() {
        let line = bulleted_line("1.");
        let (ordered, _) = detect_list_marker(&line).expect("must detect ordered marker");
        assert!(ordered);
    }

    #[test]
    fn ordered_paren_marker_is_detected() {
        let line = bulleted_line("2)");
        let (ordered, _) = detect_list_marker(&line).expect("must detect ordered marker");
        assert!(ordered);
    }

    #[test]
    fn kerned_decimal_is_not_an_ordered_list_marker() {
        // A tax rate "20.0" reaches this layer as the marker-shaped span "20."
        // immediately followed (flush, negative kern) by "0". That is one
        // decimal number, not an ordered-list item; treating it as one both
        // corrupts the value and renumbers it to "1.".
        let line = vec![
            word("20.", 100.0, BODY, false),
            word("0", 118.0, BODY, false), // flush against "20."'s advance (100 + 18)
        ];
        assert!(
            detect_list_marker(&line).is_none(),
            "the integer part of a kerned decimal must not become a list marker"
        );
    }

    #[test]
    fn negative_amount_minus_is_not_an_unordered_list_marker() {
        // A credit-note amount reaches this layer as glyph runs: `-218,48`
        // arrives as the lone span `-` immediately followed (flush, ~0 pt gap)
        // by the digits. That is a numeric sign, not a Markdown bullet;
        // treating it as one strips the minus and turns the credit positive.
        let line = vec![
            word("-", 100.0, BODY, false),
            word("218,48", 106.0, BODY, false), // flush against "-"'s advance (100 + 6)
            word(" ", 142.0, BODY, false),
            word("€", 148.0, BODY, false),
        ];
        assert!(
            detect_list_marker(&line).is_none(),
            "a negative amount's sign must not become an unordered-list bullet"
        );
    }

    #[test]
    fn dash_bullet_with_a_real_space_gap_is_still_detected() {
        // No explicit space span, but a genuine positional gap: a real dash
        // bullet must survive the negative-sign guard.
        let line = vec![
            word("-", 100.0, BODY, false),
            word("Item text", 112.0, BODY, false),
        ];
        let (ordered, _) = detect_list_marker(&line).expect("real bullet with a gap");
        assert!(!ordered);
    }

    #[test]
    fn ordered_marker_with_a_real_space_gap_is_still_detected() {
        // No explicit space span, but a genuine positional gap: a real ordered
        // marker must survive the kerned-decimal guard.
        let line = vec![
            word("1.", 100.0, BODY, false),
            word("Item text", 130.0, BODY, false),
        ];
        let (ordered, _) = detect_list_marker(&line).expect("real marker with a gap");
        assert!(ordered);
    }

    #[test]
    fn checkbox_marker_is_unordered_and_keeps_text() {
        let line = vec![
            word("[ ]", 100.0, BODY, false),
            word(" ", 118.0, BODY, false),
            word("Task", 124.0, BODY, false),
        ];
        let (ordered, skip) = detect_list_marker(&line).expect("must detect checkbox");
        assert!(!ordered);
        assert_eq!(skip, 0, "checkbox itself stays in the rendered text");
    }

    #[test]
    fn plain_prose_line_has_no_list_marker() {
        let line = one_span_line("This is a normal paragraph.", BODY, false);
        assert!(detect_list_marker(&line).is_none());
    }

    #[test]
    fn lone_marker_with_no_item_text_is_not_a_list_item() {
        let line = vec![word("-", 100.0, BODY, false)];
        assert!(detect_list_marker(&line).is_none());
    }

    // -- classify_line / ListRunState -----------------------------------------

    #[test]
    fn classify_line_promotes_heading_and_ends_list_run() {
        let mut state = ListRunState::default();
        // Start a list run first.
        let item = bulleted_line("-");
        let (role, _) = classify_line(&item, BODY, &mut state);
        assert!(matches!(role, LineRole::List { .. }));

        // A heading line must end the run rather than being treated as list depth.
        let heading = one_span_line("A Real Heading", BODY * 1.6, false);
        let (role, _) = classify_line(&heading, BODY, &mut state);
        assert!(matches!(role, LineRole::Heading(1)));

        // The next list item starts a *fresh* run (ordinal resets to 1), proving
        // the heading actually cleared state rather than just being skipped.
        let item2 = bulleted_line("1.");
        let (role, _) = classify_line(&item2, BODY, &mut state);
        match role {
            LineRole::List { ordinal, .. } => assert_eq!(ordinal, 1),
            other => panic!("expected a fresh list run, got {other:?}"),
        }
    }

    #[test]
    fn consecutive_ordered_items_number_sequentially() {
        let mut state = ListRunState::default();
        let mut ordinals = Vec::new();
        for _ in 0..3 {
            let item = bulleted_line("1.");
            let (role, _) = classify_line(&item, BODY, &mut state);
            if let LineRole::List { ordinal, .. } = role {
                ordinals.push(ordinal);
            }
        }
        assert_eq!(ordinals, vec![1, 2, 3]);
    }

    #[test]
    fn bracketed_citation_markers_keep_their_own_number_across_continuation_lines() {
        // A paper's reference list reaches this layer as "[1] Ainslie …",
        // "… continuation body line", "[2] Austin …". The continuation `Body`
        // line ends the Markdown list run, so the synthetic run counter
        // restarted at 1 for every item and the whole bibliography rendered as
        // "1. 1. 1. …". The explicit bracket index must survive.
        let bracketed = |n: usize, text: &str| -> Vec<Span> {
            let marker = format!("[{n}]");
            vec![
                word(&marker, 100.0, BODY, false),
                word(" ", 100.0 + marker.len() as f64 * BODY * 0.6, BODY, false),
                word(text, 120.0, BODY, false),
            ]
        };
        let lines: Vec<Vec<Span>> = vec![
            bracketed(1, "Joshua Ainslie, James Lee-Thorp"),
            one_span_line("Sumit Sanghai. Gqa: Training generalized multi-query", BODY, false),
            bracketed(2, "Jacob Austin, Augustus Odena"),
            one_span_line("language models. arXiv preprint arXiv:2108.07732, 2021.", BODY, false),
            bracketed(11, "Michael Collins"),
        ];
        let md = render_cluster(&lines, None);
        assert!(md.contains("1. Joshua Ainslie"), "{md}");
        assert!(md.contains("2. Jacob Austin"), "{md}");
        assert!(md.contains("11. Michael Collins"), "{md}");
        assert!(!md.contains("1. Jacob Austin"), "{md}");
    }

    #[test]
    fn dot_numbered_sections_keep_their_own_number_across_body_lines() {
        // A contract's numbered sections each carry their own literal number
        // (`1.`, `2.`, `3.` …) and are separated by ordinary body lines. Every
        // body line ends the Markdown list run, so the synthetic counter
        // restarted at 1 and all the sections rendered as "1." — the real
        // section numbering the document carries was lost (observed on
        // scratch/samples/text_style_complex.pdf, where sections 2–5 became
        // "1."). A dot marker that runs ahead of the counter must win.
        let lines: Vec<Vec<Span>> = vec![
            bulleted_line("1."),
            one_span_line("Body between one and two.", BODY, false),
            bulleted_line("2."),
            one_span_line("Body between two and three.", BODY, false),
            bulleted_line("3."),
            one_span_line("Body between three and four.", BODY, false),
            bulleted_line("5."),
        ];
        let md = render_cluster(&lines, None);
        assert!(md.contains("1. Item text"), "{md}");
        assert!(md.contains("2. Item text"), "{md}");
        assert!(md.contains("3. Item text"), "{md}");
        assert!(md.contains("5. Item text"), "{md}");
        assert_eq!(md.matches("1. Item text").count(), 1, "later sections must not collapse to 1.:\n{md}");
    }

    #[test]
    fn repeated_one_marker_still_numbers_consecutively() {
        // The complementary case the counter exists for: a producer that stamps
        // the same "1." on every item must still emit 1, 2, 3 (adopting a
        // literal number only when it runs *ahead* never fires here).
        let lines = vec![bulleted_line("1."), bulleted_line("1."), bulleted_line("1.")];
        let md = render_cluster(&lines, None);
        assert!(md.contains("1. Item text"), "{md}");
        assert!(md.contains("2. Item text"), "{md}");
        assert!(md.contains("3. Item text"), "{md}");
    }

    #[test]
    fn explicit_dot_ordinal_parses_the_three_marker_shapes() {
        assert_eq!(explicit_dot_ordinal(&bulleted_line("2.")), Some(2));
        assert_eq!(explicit_dot_ordinal(&bulleted_line("3)")), Some(3));
        assert_eq!(explicit_dot_ordinal(&bulleted_line("(4)")), Some(4));
        assert_eq!(explicit_dot_ordinal(&bulleted_line("-")), None);
        assert_eq!(explicit_dot_ordinal(&bulleted_line("[7]")), None);
    }

    #[test]
    fn deeper_indent_increases_depth() {
        let mut state = ListRunState::default();
        let shallow = vec![
            word("-", 100.0, BODY, false),
            word(" ", 106.0, BODY, false),
            word("Top level", 112.0, BODY, false),
        ];
        let (role, _) = classify_line(&shallow, BODY, &mut state);
        let shallow_depth = match role {
            LineRole::List { depth, .. } => depth,
            _ => panic!("expected a list item"),
        };

        let nested = vec![
            word("-", 100.0 + 3.0 * BODY, BODY, false), // indented ~2 depth-units right
            word(" ", 106.0 + 3.0 * BODY, BODY, false),
            word("Nested", 112.0 + 3.0 * BODY, BODY, false),
        ];
        let (role, _) = classify_line(&nested, BODY, &mut state);
        let nested_depth = match role {
            LineRole::List { depth, .. } => depth,
            _ => panic!("expected a list item"),
        };

        assert_eq!(shallow_depth, 0);
        assert!(nested_depth > shallow_depth, "indented item must report a deeper depth");
    }

    // -- format_structured_line / strip_outer_emphasis -------------------------

    #[test]
    fn heading_strips_redundant_outer_bold() {
        let out = format_structured_line(&LineRole::Heading(2), "**Section Title**");
        assert_eq!(out, "## Section Title");
    }

    #[test]
    fn heading_with_no_emphasis_is_unchanged() {
        let out = format_structured_line(&LineRole::Heading(1), "Plain Title");
        assert_eq!(out, "# Plain Title");
    }

    #[test]
    fn unordered_list_item_gets_dash_prefix() {
        let out = format_structured_line(
            &LineRole::List { depth: 0, ordered: false, ordinal: 1 },
            "Item text",
        );
        assert_eq!(out, "- Item text");
    }
