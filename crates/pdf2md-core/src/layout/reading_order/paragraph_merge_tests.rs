// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Reading order recovery, multi-column stream separation, and structured DocBlock generation.

    use super::*;

    // Font size 10, single-spaced leading (~12pt pitch): y1-y0 = 10.0 so the
    // block's own "font size" for pitch math is 10.0.
    fn body_block(x0: f64, y0: f64, text: &str) -> DocBlock {
        DocBlock {
            page: 0,
            kind: "body".to_string(),
            x0,
            y0,
            x1: x0 + text.len() as f64 * 5.0,
            y1: y0 + 10.0,
            text: text.to_string(),
            is_bold: false,
            is_italic: false,
            is_underline: false,
        }
    }

    fn kind_block(kind: &str, x0: f64, y0: f64, text: &str) -> DocBlock {
        let mut b = body_block(x0, y0, text);
        b.kind = kind.to_string();
        b
    }

    #[test]
    fn two_aligned_normally_spaced_lines_merge_into_one_paragraph() {
        // Line 2's baseline is 12pt below line 1's (typical 1.2x leading for
        // a 10pt font) — gap = prev.y0(700) - b.y1(698) = 2, well within the
        // 1.8*10=18 pitch cap.
        let blocks = vec![
            body_block(100.0, 700.0, "First line of the paragraph"),
            body_block(100.0, 688.0, "second line continues it"),
        ];
        let merged = merge_paragraph_lines(blocks);
        assert_eq!(merged.len(), 1, "two aligned, normally-spaced lines must merge");
        assert_eq!(merged[0].text, "First line of the paragraph second line continues it");
    }

    #[test]
    fn three_line_paragraph_merges_fully() {
        let blocks = vec![
            body_block(100.0, 700.0, "Line one"),
            body_block(100.0, 688.0, "line two"),
            body_block(100.0, 676.0, "line three"),
        ];
        let merged = merge_paragraph_lines(blocks);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].text, "Line one line two line three");
        // Bounding box must union across all 3 merged lines.
        assert_eq!(merged[0].y1, 710.0, "y1 from the topmost line");
        assert_eq!(merged[0].y0, 676.0, "y0 from the bottommost line");
    }

    #[test]
    fn heading_between_two_body_lines_prevents_merge_across_it() {
        let blocks = vec![
            body_block(100.0, 700.0, "Paragraph before the heading"),
            kind_block("heading", 100.0, 688.0, "A Heading"),
            body_block(100.0, 676.0, "Paragraph after the heading"),
        ];
        let merged = merge_paragraph_lines(blocks);
        assert_eq!(merged.len(), 3, "a heading must never merge, and must not bridge two body blocks");
        assert_eq!(merged[0].kind, "body");
        assert_eq!(merged[1].kind, "heading");
        assert_eq!(merged[2].kind, "body");
    }

    #[test]
    fn list_item_never_merges_with_surrounding_body_text() {
        let blocks = vec![
            body_block(100.0, 700.0, "Some intro text"),
            kind_block("list", 100.0, 688.0, "A list item"),
            body_block(100.0, 676.0, "Trailing text"),
        ];
        let merged = merge_paragraph_lines(blocks);
        assert_eq!(merged.len(), 3);
        assert_eq!(merged[1].kind, "list");
        assert_eq!(merged[1].text, "A list item");
    }

    #[test]
    fn negative_amount_is_not_classified_as_a_list_block() {
        // Iteration 14 (fnfe_Avoir_FR_type381_BASIC.pdf): a credit-note amount
        // rendered as the single line `-20,48 €` was classified `kind:"list"`
        // by the bare `t.starts_with('-')` test in `build_doc_blocks`, even
        // though the markdown path (`detect_list_marker`) already rejects the
        // same sign because it has no separating whitespace. The two channels
        // must agree: a negative amount is a value, not a bullet.
        fn line(text: &str, x: f64, y: f64) -> Vec<Span> {
            vec![Span {
                text: text.to_string(),
                x,
                y,
                size: 10.0,
                advance: text.len() as f64 * 6.0,
                word_advance: text.len() as f64 * 6.0,
                is_bold: false,
                is_italic: false,
                is_underline: false,
                is_vertical: false,
            }]
        }
        let lines = vec![line("Description", 100.0, 700.0), line("-20,48 €", 200.0, 660.0)];
        let blocks = build_doc_blocks(&lines, 842.0);
        let neg_block = blocks
            .iter()
            .find(|b| b.text.contains("20,48"))
            .expect("negative amount must survive into the blocks channel");
        assert_eq!(
            neg_block.kind, "body",
            "a negative amount is a value, not a bullet list item: {neg_block:?}"
        );
    }

    #[test]
    fn dash_bullet_line_is_still_classified_as_a_list_block() {
        // The other direction: a real `- item` (dash then whitespace) must
        // remain a list block after the negative-sign guard.
        fn line(text: &str, x: f64, y: f64) -> Vec<Span> {
            vec![Span {
                text: text.to_string(),
                x,
                y,
                size: 10.0,
                advance: text.len() as f64 * 6.0,
                word_advance: text.len() as f64 * 6.0,
                is_bold: false,
                is_italic: false,
                is_underline: false,
                is_vertical: false,
            }]
        }
        let lines = vec![line("Description", 100.0, 700.0), line("- Item text", 100.0, 660.0)];
        let blocks = build_doc_blocks(&lines, 842.0);
        let list_block = blocks
            .iter()
            .find(|b| b.text.contains("Item text"))
            .expect("bullet line must survive into the blocks channel");
        assert_eq!(
            list_block.kind, "list",
            "a real dash bullet separated by whitespace must stay a list: {list_block:?}"
        );
    }

    #[test]
    fn a_wide_vertical_gap_is_a_paragraph_break_not_a_merge() {
        // Gap = prev.y0(700) - b.y1(b.y0+10). For a break we need
        // gap > 1.8*10=18, so put the next line's y0 well below that.
        let blocks = vec![
            body_block(100.0, 700.0, "End of one paragraph."),
            body_block(100.0, 660.0, "Start of an unrelated paragraph."),
        ];
        let merged = merge_paragraph_lines(blocks);
        assert_eq!(merged.len(), 2, "a large vertical gap must read as a paragraph break");
    }

    #[test]
    fn misaligned_left_edges_do_not_merge() {
        // Line 2 establishes body_x0=100; line 3 is indented far enough right
        // (a new nested/quoted block, not a paragraph continuation) to miss
        // the ~3pt tolerance.
        let blocks = vec![
            body_block(100.0, 700.0, "Paragraph line one"),
            body_block(100.0, 688.0, "paragraph line two"),
            body_block(140.0, 676.0, "a differently indented block"),
        ];
        let merged = merge_paragraph_lines(blocks);
        assert_eq!(merged.len(), 2, "a line whose left edge doesn't match the body indent must not merge");
        assert_eq!(merged[0].text, "Paragraph line one paragraph line two");
    }

    #[test]
    fn first_line_indent_is_exempt_from_left_edge_check() {
        // Line 1 (the paragraph's first line) is indented +15pt from line 2 —
        // a classic first-line indent — and must not block the merge; line 2
        // then sets the real body_x0 that line 3 is checked against.
        let blocks = vec![
            body_block(115.0, 700.0, "Indented first line of paragraph"),
            body_block(100.0, 688.0, "flush second line"),
            body_block(100.0, 676.0, "flush third line"),
        ];
        let merged = merge_paragraph_lines(blocks);
        assert_eq!(merged.len(), 1, "a first-line indent must not prevent merging with the rest of the paragraph");
        assert_eq!(
            merged[0].text,
            "Indented first line of paragraph flush second line flush third line"
        );
    }

    #[test]
    fn non_overlapping_adjacent_columns_do_not_merge() {
        // Regression (iteration 11, akretion_invoice_EN16931.pdf): the
        // `DEVISE : EURO (EUR)` cell in the page's right column and the
        // `NOM & DESIGNATION, Période` table header starting ~140pt to its
        // left are vertically adjacent body lines (pitch gap = 2pt, inside
        // the 1.8*10 cap) whose horizontal extents share nothing at all. The
        // first-merge exemption from the left-edge check used to fuse them
        // into one block `DEVISE : EURO (EUR) NOM & DESIGNATION, Période`,
        // even though the markdown channel — a separate code path — keeps
        // them apart. They must remain two blocks.
        let right_col = body_block(350.0, 700.0, "DEVISE : EURO (EUR)");
        let left_col = body_block(100.0, 688.0, "NOM & DESIGNATION, Période");
        let merged = merge_paragraph_lines(vec![right_col, left_col]);
        assert_eq!(
            merged.len(),
            2,
            "adjacent lines from different columns with no horizontal overlap must not merge: {:?}",
            merged.iter().map(|b| b.text.as_str()).collect::<Vec<_>>()
        );
        assert_eq!(merged[0].text, "DEVISE : EURO (EUR)");
        assert_eq!(merged[1].text, "NOM & DESIGNATION, Période");
    }

    #[test]
    fn short_first_line_that_overlaps_its_continuation_still_merges() {
        // The overlap guard must not defeat the first-line-indent exemption:
        // a short indented first line still overlaps the wider flush
        // continuation, so the two remain one paragraph.
        let blocks = vec![
            body_block(120.0, 700.0, "A short opener"),
            body_block(100.0, 688.0, "a much wider second line"),
        ];
        let merged = merge_paragraph_lines(blocks);
        assert_eq!(merged.len(), 1, "overlapping lines must still merge");
        assert_eq!(merged[0].text, "A short opener a much wider second line");
    }

    #[test]
    fn table_zone_and_figure_never_merge_with_body_text() {
        for kind in ["table", "figure", "caption", "header", "footer", "title"] {
            let blocks = vec![
                body_block(100.0, 700.0, "Text before"),
                kind_block(kind, 100.0, 688.0, "Zone content"),
                body_block(100.0, 676.0, "Text after"),
            ];
            let merged = merge_paragraph_lines(blocks);
            assert_eq!(merged.len(), 3, "kind {kind:?} must never merge with body text");
        }
    }

    // -- join_paragraph_text / de-hyphenation -----------------------------

    #[test]
    fn hyphenated_line_wrap_joins_without_space_or_hyphen() {
        let mut text = "This is infor-".to_string();
        join_paragraph_text(&mut text, "mation you need.");
        assert_eq!(text, "This is information you need.");
    }

    #[test]
    fn trailing_dash_preceded_by_space_is_not_treated_as_hyphenation() {
        // A dash used as punctuation (range, aside) — the character right
        // before it is a space, not a letter — must join with a space and
        // keep the dash.
        let mut text = "A notable fact -".to_string();
        join_paragraph_text(&mut text, "worth remembering.");
        assert_eq!(text, "A notable fact - worth remembering.");
    }

    #[test]
    fn ordinary_lines_join_with_a_single_space() {
        let mut text = "First part".to_string();
        join_paragraph_text(&mut text, "second part.");
        assert_eq!(text, "First part second part.");
    }

    #[test]
    fn trailing_whitespace_before_hyphen_is_ignored() {
        let mut text = "infor-  ".to_string(); // trailing spaces after the hyphen
        join_paragraph_text(&mut text, "mation");
        assert_eq!(text, "information");
    }

    #[test]
    fn real_compound_hyphen_is_kept_on_merge() {
        // A clitic tail (inversion) is not a line-wrap fragment.
        let mut text = "Comment va-".to_string();
        join_paragraph_text(&mut text, "t-il ?");
        assert_eq!(text, "Comment va-t-il ?");
        // A hyphenated compound whose second element is a whole word.
        let mut text = "un non-".to_string();
        join_paragraph_text(&mut text, "professionnel ici");
        assert_eq!(text, "un non-professionnel ici");
        // A capitalised continuation is never a line-wrap fragment either.
        let mut text = "la ville de".to_string();
        join_paragraph_text(&mut text, "Paris");
        assert_eq!(text, "la ville de Paris");
    }

    // -- page_two_columns_rows: column membership of unpaired lines -----------

    fn col_span(text: &str, x: f64, y: f64) -> Span {
        Span {
            text: text.to_string(),
            x,
            y,
            size: 10.0,
            advance: text.len() as f64 * 6.0,
            word_advance: text.len() as f64 * 6.0,
            is_bold: false,
            is_italic: false,
            is_underline: false,
            is_vertical: false,
        }
    }

    fn two_col_row(y: f64, left: &[(&str, f64)], right: &[(&str, f64)]) -> Vec<Span> {
        let mut v: Vec<Span> = left.iter().map(|(t, x)| col_span(t, *x, y)).collect();
        v.extend(right.iter().map(|(t, x)| col_span(t, *x, y)));
        v
    }

    #[test]
    fn unpaired_left_line_inside_column_span_stays_in_left_column() {
        // A two-column page whose left column is one short line taller than the
        // right: that line's baseline coincides with no right-column line, so it
        // can never be paired with a `split_row_columns` row. It must still be
        // routed to the left column, between the shared rows above it and the
        // footer rows below — not dumped into `bottom_full`, which renders after
        // the entire right column (the original bug: "intercontinentaux." jumped
        // to the end of the page, past the whole English paragraph).
        let lines = vec![
            two_col_row(228.0,
                &[("Le", 50.0), ("tarif", 70.0), ("réservé", 108.0)],
                &[("The", 300.0), ("fare", 320.0), ("applies", 348.0)]),
            two_col_row(221.0,
                &[("les", 50.0), ("dates", 70.0), ("du", 108.0)],
                &[("the", 300.0), ("dates", 320.0), ("below", 358.0)]),
            two_col_row(214.0,
                &[("pour", 50.0), ("les", 82.0), ("vols", 108.0)],
                &[("for", 300.0), ("the", 326.0), ("flights", 352.0)]),
            two_col_row(180.0,
                &[("La", 50.0), ("Première", 70.0), ("cabins", 130.0)],
                &[("equivalent", 300.0), ("in", 360.0), ("currency", 376.0)]),
            vec![col_span("intercontinentaux.", 50.0, 174.0)],
            two_col_row(130.0,
                &[("Pour", 50.0), ("plus", 70.0), ("d'information,", 100.0)],
                &[("For", 300.0), ("more", 320.0), ("information,", 348.0)]),
            two_col_row(123.0,
                &[("réservations", 50.0), ("en", 128.0), ("cliquant", 148.0)],
                &[("France", 300.0), ("web", 344.0), ("site", 370.0)]),
        ];
        let pc = page_two_columns_rows(&lines).expect("two consistent columns must be detected");
        let bottom: Vec<String> = pc.bottom_full.iter().map(|l| render_line_text(l)).collect();
        assert!(bottom.is_empty(), "no unpaired column line may be dumped to the footer: {bottom:?}");
        assert!(pc.top_full.is_empty(), "nothing sits above the column block");
        let left: Vec<String> = pc.left.iter().map(|l| render_line_text(l)).collect();
        let idx = left
            .iter()
            .position(|t| t.contains("intercontinentaux."))
            .expect("the taller left column's tail line must stay in the left stream");
        assert!(
            left[..idx].iter().any(|t| t.contains("vols")),
            "tail line must follow the left column body: {left:?}"
        );
        assert!(
            left[idx + 1..].iter().any(|t| t.contains("réservations")),
            "tail line must precede the left column footer: {left:?}"
        );
    }

    #[test]
    fn split_hard_breaks_subtracts_previous_span_advance() {
        // `render_spans` measures inter-span whitespace as
        // `next.x - prev.x - prev.advance`; `split_hard_breaks` must use the
        // same residual so the pre-split only cuts where a render would hard
        // break. Here "Nougat de l'" is one span whose own 78pt advance carries
        // the next span's start 78pt to the right — far past the 2.5em (25pt)
        // hard-break threshold if measured start-to-start, but the actual
        // whitespace between them is nil. Measuring the raw distance split the
        // product description into separate lines ("Nougat de l'" / "Abbaye" /
        // " 250g"); the residual gap must keep it one line.
        let line = vec![
            col_span("Nougat de l'", 50.0, 300.0),
            col_span("Abbaye", 128.0, 300.0),
            col_span("250g", 170.0, 300.0),
        ];
        let segs = split_hard_breaks(&line);
        let rendered: Vec<String> = segs.iter().map(|s| render_spans(s)).collect();
        assert_eq!(
            segs.len(),
            1,
            "no spurious mid-line break: {rendered:?}"
        );
        // The whole-row render agrees: it emits no hard newline, so the
        // pre-split must not either.
        assert!(
            !render_spans(&line).contains('\n'),
            "render_spans keeps the row on one line"
        );
        assert!(render_spans(&line).contains("Abbaye 250g"));
    }

    #[test]
    fn split_line_segments_subtracts_previous_span_advance() {
        // Same residual-gap rule as `render_spans` and `split_hard_breaks`: the
        // whitespace between two spans is `next.x - prev.x - prev.advance`. A
        // 78pt span followed flush by the next span therefore has no gap at
        // all, even though its own advance carries the next start 78pt to the
        // right — far past the 2.5em (25pt) / 20pt segment threshold if the raw
        // start-to-start distance is measured. The raw form carved one visual
        // line into spurious "columns" here.
        let line = vec![
            col_span("Nougat de l'", 50.0, 300.0),
            col_span("Abbaye", 128.0, 300.0),
            col_span("250g", 170.0, 300.0),
        ];
        let segs = split_line_segments(&line);
        assert_eq!(
            segs.len(),
            1,
            "flush spans must stay one segment: {:?}",
            segs.iter().map(|s| render_spans(s)).collect::<Vec<_>>()
        );

        // A genuine 30pt empty gutter (previous span ends 24pt in, next starts
        // 30pt after that) must still split into two segments.
        let gutter = vec![
            col_span("left", 50.0, 300.0),
            col_span("right", 104.0, 300.0),
        ];
        assert_eq!(
            split_line_segments(&gutter).len(),
            2,
            "a real column gutter must still split"
        );
    }