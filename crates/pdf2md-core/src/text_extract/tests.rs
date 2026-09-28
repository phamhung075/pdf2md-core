// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Robust multilingual (FR / VI / EN) PDF text extraction for the fast path.
//!
//! This module replaces `lopdf::Document::extract_text` for the digital-PDF
//! fast path. `lopdf`'s extractor has two defects that corrupt accented Latin
//! text (French, Vietnamese, ...):
//!
//! 1. A `/Encoding` dictionary whose `/Differences` array contains `/.notdef`
//!    (extremely common in real-world generators, e.g. EDF / Enedis bills) is
//!    treated as an *error*, so the whole font silently falls back to lopdf's
//!    `STANDARD_ENCODING` table.
//! 2. That fallback table is corrupt for bytes >= 0xC0: it maps byte `0xE9`
//!    (`é`) to `Ø` and byte `0xE8` (`è`) to `Ł`, among others.
//!
//! We therefore resolve font encodings ourselves with correct data tables and
//! decode each text run accordingly:
//!   * `/ToUnicode` CMaps (bfchar / bfrange) — authoritative when present;
//!   * `/Encoding` by name (`WinAnsiEncoding`, `MacRomanEncoding`, ...);
//!   * `/Encoding` dictionaries with `/Differences` (`.notdef` and unknown
//!     glyph names are tolerated, AGL `uniXXXX` names supported);
//!   * a WinAnsi heuristic for non-symbolic simple fonts with no encoding
//!     information (how the overwhelming majority of producers write accented
//!     Latin text).
//!
//! Page content / font-structure parsing still comes from lopdf (public API
//! only). If content parsing fails, the caller falls back to lopdf's own
//! extractor.

use super::*;
use super::tests_common::*;
        

    /// The routing guard must treat a grouped value as one number. The old
    /// per-maximal-run check passed when the glyph page still carried the two
    /// fragments separately even though the whole value was gone.
    #[test]
    fn loses_digit_content_detects_a_lost_grouped_value() {
        // Whole value present: no loss.
        assert!(!loses_digit_content("Total 12 345,67", "Total 12 345,67"));
        assert!(!loses_digit_content("Total 12 345,67", "Total 12345.67"));
        // Glyph page kept only the trailing group: the value is gone.
        assert!(loses_digit_content("Total 12 345,67", "Total 345,67"));
        assert!(loses_digit_content("reference 12 345 678", "reference 678"));
        // A short number (<3 digits) is never a loss signal.
        assert!(!loses_digit_content("article 12", "article"));
        // A pure run lost is still detected.
        assert!(loses_digit_content("reference 123456", "reference"));
    }

    #[test]
    fn glyph_unicode_lookup() {
        assert_eq!(glyph_unicode(b"eacute"), Some(0xE9));
        assert_eq!(glyph_unicode(b"Oslash"), Some(0xD8));
        assert_eq!(glyph_unicode(b"Abreveacute"), Some(0x1EAE));
        assert_eq!(glyph_unicode(b"abreveacute"), Some(0x1EAF));
        assert_eq!(glyph_unicode(b".notdef"), Some(0));
        assert_eq!(glyph_unicode(b"unknownGlyph"), None);
        assert_eq!(glyph_unicode(b"uni1EA1"), Some(0x1EA1));
    }

    #[test]
    fn winansi_decodes_french_accents() {
        let codec = Codec::Byte8(ByteTable(WIN_ANSI), PuaFamily::Unknown);
        let bytes = [
            0xC9, 0x6C, 0x65, 0x63, 0x74, 0x72, 0x69, 0x63, 0x69, 0x74, 0xE9, // Électricité
        ];
        let mut s = String::new();
        codec.decode(&bytes, &mut s);
        assert_eq!(s, "Électricité");
    }

    #[test]
    fn notdef_differences_tolerated() {
        // Base WinAnsi + a Differences entry mapping a valid code to /.notdef
        // must not poison the whole table.
        let mut t = ByteTable(WIN_ANSI);
        assert_eq!(t.0[0xE9], 0xE9);
        // Simulate Differences overriding 0xE9 -> .notdef and 0xE0 -> agrave.
        let names = [b".notdef".as_slice(), b"agrave".as_slice()];
        let mut code: u8 = 0xE9;
        for nm in names {
            t.0[code as usize] = glyph_unicode(nm).unwrap_or(0);
            code = code.wrapping_add(1);
        }
        assert_eq!(t.0[0xE9], 0); // .notdef dropped
        assert_eq!(t.0[0xEA], 0xE0); // next code got agrave
        let codec = Codec::Byte8(t, PuaFamily::Unknown);
        let mut s = String::new();
        codec.decode(&[0xE9, 0xEA], &mut s);
        assert_eq!(s, "à");
    }

    #[test]
    fn cmap_parse_and_decode() {
        let cmap = b"begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n\
                     beginbfchar\n<0041> <0041>\n<00E9> <00E9>\nendbfchar\n\
                     beginbfrange\n<0100> <0102> <1EA0>\nendbfrange\n";
        let cm = parse_cmap(cmap).expect("parse");
        let codec = Codec::CMap(cm, None, PuaFamily::Unknown);
        let bytes = [0x00, 0x41, 0x00, 0xE9, 0x01, 0x01];
        let mut s = String::new();
        codec.decode(&bytes, &mut s);
        assert_eq!(s, "Aéạ");
    }

    #[test]
    fn symbolic_simple_font_with_tounicode_is_not_dropped() {
        // TeX's CMR/CMMI faces are Type1, `/Flags 4` (Symbolic) and carry no
        // `/Encoding` — but they *do* ship a full `/ToUnicode`. Before the
        // fix, `resolve_codec` returned `None` for exactly this shape and
        // every glyph drawn in the font silently vanished.
        use lopdf::Stream;
        let cmap = b"beginbfchar\n<57> <0057>\nendbfchar\n".to_vec();
        let mut fd = Dictionary::new();
        fd.set(b"Flags", 4);
        let mut font = Dictionary::new();
        font.set(b"Subtype", Object::Name(b"Type1".to_vec()));
        font.set(b"BaseFont", Object::Name(b"ABCDEF+CMR9".to_vec()));
        font.set(b"FontDescriptor", Object::Dictionary(fd));
        font.set(b"ToUnicode", Object::Stream(Stream::new(Dictionary::new(), cmap)));
        let doc = Document::new();
        let codec = resolve_codec(&doc, &font)
            .expect("a symbolic simple font with /ToUnicode must still resolve");
        let mut s = String::new();
        codec.decode(&[0x57], &mut s);
        assert_eq!(s, "W");
    }

    #[test]
    fn unic_prefixed_differences_map_to_unicode() {
        // BNP Paribas Type3 statements name every glyph `/UNIC00E9`, a prefix
        // the AGL does not know. Without this the /Differences resolve to
        // nothing and all 859 characters of the statement are dropped.
        assert_eq!(glyph_unicode(b"UNIC00E9"), Some(0xE9));
        assert_eq!(glyph_unicode(b"unic0041"), Some(0x41));
        assert_eq!(glyph_unicode(b"UNKNOWN1"), None);

        let mut enc = Dictionary::new();
        enc.set(b"Type", Object::Name(b"Encoding".to_vec()));
        enc.set(
            b"Differences",
            Object::Array(vec![
                Object::Integer(65),
                Object::Name(b"UNIC0041".to_vec()),
                Object::Name(b"UNIC0042".to_vec()),
            ]),
        );
        let mut font = Dictionary::new();
        font.set(b"Subtype", Object::Name(b"Type3".to_vec()));
        font.set(b"Encoding", Object::Dictionary(enc));
        let doc = Document::new();
        let codec = resolve_codec(&doc, &font).expect("Type3 with UNIC differences resolves");
        let mut s = String::new();
        codec.decode(&[65, 66], &mut s);
        assert_eq!(s, "AB");
    }

    #[test]
    fn symbolic_latin_face_falls_back_but_dingbats_do_not() {
        // Subset CFF/Type1 text faces routinely set the Symbolic flag while
        // drawing StandardEncoding-compatible bytes (Bouygues bills). Decoding
        // them through the Latin table recovers the document; a genuine dingbat
        // face must stay unmapped so it still escalates.
        let mut fd = Dictionary::new();
        fd.set(b"Flags", 4);
        let mut latin = Dictionary::new();
        latin.set(b"Subtype", Object::Name(b"Type1".to_vec()));
        latin.set(b"BaseFont", Object::Name(b"ABCDEF+ArialMT".to_vec()));
        latin.set(b"FontDescriptor", Object::Dictionary(fd.clone()));
        let doc = Document::new();
        let codec =
            resolve_codec(&doc, &latin).expect("a Latin face flagged Symbolic must resolve");
        let mut s = String::new();
        codec.decode(b"Bonjour", &mut s);
        assert_eq!(s, "Bonjour");

        let mut dingbat = Dictionary::new();
        dingbat.set(b"Subtype", Object::Name(b"Type1".to_vec()));
        dingbat.set(b"BaseFont", Object::Name(b"ABCDEF+Wingdings".to_vec()));
        dingbat.set(b"FontDescriptor", Object::Dictionary(fd));
        assert!(
            resolve_codec(&doc, &dingbat).is_none(),
            "a dingbat face must not be decoded as Latin text"
        );
    }

    #[test]
    fn cmap_fallback_to_byte_table() {
        // 2-byte CMap that does not cover a code actually used on the page:
        // decode must fall back to the byte table so no text is lost.
        let cmap = b"beginbfchar\n<0041> <0041>\nendbfchar\n";
        let cm = parse_cmap(cmap).expect("parse");
        let codec = Codec::CMap(cm, Some(ByteTable(WIN_ANSI)), PuaFamily::Unknown);
        // 0x00E9 with a 2-byte code space, but only <0041> is mapped: the
        // byte table decodes 0xE9 -> é on its own.
        let bytes = [0x00, 0xE9];
        let mut s = String::new();
        codec.decode(&bytes, &mut s);
        assert_eq!(s, "é");
    }

    #[test]
    fn td_line_advance_breaks_padded_show_strings() {
        // Before the fix the string walker ignored the `Td` line advance and
        // kept each string's padding, emitting one fused run:
        //   " BILLET ÉLECTRONIQUE  RÉFÉRENCE DE VOTRE RÉSERVATION  Obtenez ..."
        // instead of one line per visual line.
        let doc = td_line_advance_doc();
        let page = extract_page_text_report(&doc, 1, true, true, false).expect("page text");
        assert_eq!(
            page.text,
            "BILLET ÉLECTRONIQUE\nRÉFÉRENCE DE VOTRE RÉSERVATION\n\
             Obtenez votre carte d'embarquement."
        );
    }

    #[test]
    fn horizontal_td_does_not_start_a_new_line() {
        // A pure horizontal `Td` is an in-line move, not a line advance.
        let horizontal = Operation::new("Td", vec![Object::Integer(40), Object::Integer(0)]);
        assert!(!td_advances_line(&horizontal));
        let vertical = Operation::new("Td", vec![Object::Integer(0), Object::Integer(-20)]);
        assert!(td_advances_line(&vertical));
    }

    #[test]
    fn horizontal_td_word_gaps_are_restored() {
        // `Tj` fragments positioned only by horizontal `Td` (no `TJ`, no `Tm`)
        // — the a corpus file producer shape. Word boundaries are the gaps between the
        // previous run's natural end and the next run's start.
        // Courier 12pt advance = 0.6*12 = 7.2 pt/char:
        //   "Bonjour" ends at 50.4; Td 60 -> gap 9.6 (> 0.1625 em)
        //   "le" ends at 74.4;     Td 20 -> gap 5.6
        let doc = content_doc(
            b"BT /F1 12 Tf (Bonjour) Tj 60 0 Td (le) Tj 20 0 Td (monde) Tj ET",
        );
        let page = extract_page_text_report(&doc, 1, true, true, false).expect("page text");
        assert_eq!(page.text, "Bonjour le monde");
    }

    #[test]
    fn horizontal_td_at_natural_advance_does_not_split_a_word() {
        // A producer that splits a word into `Tj` runs placed exactly at the
        // natural advance (a kerning emulation) must NOT gain a space: gap == 0.
        let doc = content_doc(b"BT /F1 12 Tf (Bon) Tj 21.6 0 Td (jour) Tj ET");
        let page = extract_page_text_report(&doc, 1, true, true, false).expect("page text");
        assert_eq!(page.text, "Bonjour");
    }

    #[test]
    fn an_upward_baseline_jump_starts_a_new_paragraph() {
        // Two columns drawn in stream order with `Tj` + `Td` (the string-walker
        // shape, `iv_twocol`): the right column restarts at the top of the page,
        // a baseline *above* the last left-column line. The walker must insert
        // a blank line so a later reflow cannot weld the two columns.
        let doc = content_doc(
            b"BT /F1 12 Tf 50 760 Td (gauche sans fin) Tj ET\n\
              BT /F1 12 Tf 50 744 Td (suite gauche) Tj ET\n\
              BT /F1 12 Tf 320 760 Td (droite minuscule) Tj ET\n\
              BT /F1 12 Tf 320 744 Td (suite droite) Tj ET",
        );
        // `detect_tables` off: this is the string-walker path. With tables on the
        // glyph engine recognises this 2x2 alignment as a grid instead.
        let page = extract_page_text_report(&doc, 1, false, false, false).expect("page text");
        assert!(
            page.text.contains("suite gauche\n\ndroite minuscule"),
            "columns must be separated by a blank line: {:?}",
            page.text
        );
        // Within one column the wrapped lines stay in a single paragraph.
        assert!(
            page.text.contains("gauche sans fin\nsuite gauche"),
            "wrapped lines must not gain a blank line: {:?}",
            page.text
        );
    }

    #[test]
    fn a_sideways_line_above_starts_a_new_paragraph() {
        // A right column that starts only 6 pt above the last left-column line
        // (< 1 em, so the upward-jump rule alone misses it) but at a clearly
        // different x is still a new column and must get a blank line.
        let doc = content_doc(
            b"BT /F1 12 Tf 50 760 Td (gauche sans fin) Tj ET\n\
              BT /F1 12 Tf 50 744 Td (suite gauche) Tj ET\n\
              BT /F1 12 Tf 320 750 Td (droite minuscule) Tj ET\n\
              BT /F1 12 Tf 320 734 Td (suite droite) Tj ET",
        );
        let page = extract_page_text_report(&doc, 1, false, false, false).expect("page text");
        assert!(
            page.text.contains("suite gauche\n\ndroite minuscule"),
            "a sideways line above must start a new paragraph: {:?}",
            page.text
        );
    }

    #[test]
    fn table_row_cells_on_one_baseline_are_not_a_paragraph_break() {
        // Cells of a label:value / table row share a baseline; the large x jump
        // between them is a cell placement, never a line break, so no blank
        // line may appear between them.
        let doc = content_doc(
            b"BT /F1 12 Tf 50 760 Td (Reference) Tj ET\n\
              BT /F1 12 Tf 200 760 Td (Quantite) Tj ET\n\
              BT /F1 12 Tf 350 760 Td (Montant) Tj ET",
        );
        let page = extract_page_text_report(&doc, 1, false, false, false).expect("page text");
        assert!(
            !page.text.contains("\n\n"),
            "same-baseline cells must not become paragraphs: {:?}",
            page.text
        );
    }

    #[test]
    fn right_aligned_amount_on_same_baseline_is_not_a_paragraph_break() {
        // A right-aligned amount next to its label, same baseline.
        let doc = content_doc(
            b"BT /F1 12 Tf 50 760 Td (Total a payer) Tj ET\n\
              BT /F1 12 Tf 400 760 Td (1 234,56) Tj ET",
        );
        let page = extract_page_text_report(&doc, 1, false, false, false).expect("page text");
        assert!(
            !page.text.contains("\n\n"),
            "a right-aligned amount must not become a paragraph: {:?}",
            page.text
        );
    }

    #[test]
    fn wrapped_lines_without_column_geometry_stay_welded() {
        // The qa-int2 `iv_col_end` shape: two consecutive wrapped lines with the
        // same start x and a descending baseline. There is no column geometry
        // (no upward jump, no x jump), so this is an ordinary wrapped line and
        // reflow must be free to join it — inserting a blank line here would
        // break every wrapped paragraph in the corpus.
        let doc = content_doc(
            b"BT /F1 12 Tf 50 760 Td (premiere colonne sans fin) Tj ET\n\
              BT /F1 12 Tf 50 744 Td (droite commence minuscule) Tj ET",
        );
        let page = extract_page_text_report(&doc, 1, false, false, false).expect("page text");
        assert!(
            !page.text.contains("\n\n"),
            "geometry-less wrapped lines must not gain a blank line: {:?}",
            page.text
        );
    }

    #[test]
    fn a_midline_superscript_does_not_start_a_new_paragraph() {
        // qa-int3 `h2x_superscript_midline`: a footnote/superscript mark at the
        // END of a long line, a few points above its baseline. Its start x
        // falls inside the previous show's horizontal span, so it continues the
        // same visual line and must not gain a blank line (QA risk R1).
        let doc = content_doc(
            b"BT /F1 12 Tf 50 760 Td (Un long paragraphe de texte avec une note) Tj ET\n\
              BT /F1 12 Tf 300 764 Td (1) Tj ET\n\
              BT /F1 12 Tf 50 744 Td (qui continue sur la ligne suivante) Tj ET",
        );
        let page = extract_page_text_report(&doc, 1, false, false, false).expect("page text");
        assert!(
            !page.text.contains("\n\n"),
            "a mid-line superscript must not break the paragraph: {:?}",
            page.text
        );
    }

    #[test]
    fn a_footnote_mark_near_the_line_start_does_not_break() {
        // The small-x-jump superscript shape (`h2x_superscript_smalljump`): the
        // mark sits above the previous baseline but horizontally inside the
        // previous line, so it is the same line.
        let doc = content_doc(
            b"BT /F1 12 Tf 50 760 Td (Le total general est) Tj ET\n\
              BT /F1 12 Tf 70 764 Td (1) Tj ET\n\
              BT /F1 12 Tf 50 744 Td (de cent euros environ) Tj ET",
        );
        let page = extract_page_text_report(&doc, 1, false, false, false).expect("page text");
        assert!(
            !page.text.contains("\n\n"),
            "a footnote mark at the line start must not break: {:?}",
            page.text
        );
    }

    #[test]
    fn a_smaller_font_mark_beyond_the_line_end_does_not_break() {
        // A real superscript is a *smaller* face a few points up. Even when it
        // is placed clear of the previous line's end (a right-margin footnote
        // mark), the small rise plus the smaller size must keep it on the same
        // line.
        let doc = content_doc(
            b"BT /F1 12 Tf 50 760 Td (Le montant total est de) Tj ET\n\
              BT /F1 6 Tf 400 764 Td (1) Tj ET\n\
              BT /F1 12 Tf 50 744 Td (cent euros environ) Tj ET",
        );
        let page = extract_page_text_report(&doc, 1, false, false, false).expect("page text");
        assert!(
            !page.text.contains("\n\n"),
            "a smaller-font superscript mark must not break: {:?}",
            page.text
        );
    }

    #[test]
    fn a_near_height_two_column_start_still_breaks() {
        // The H2 win must survive: a right column that starts only 6 pt above
        // the last left-column line (under one em) at a clearly different x is
        // a new block and keeps its blank line.
        let doc = content_doc(
            b"BT /F1 12 Tf 50 760 Td (colonne gauche sans fin) Tj ET\n\
              BT /F1 12 Tf 50 744 Td (la suite de la phrase) Tj ET\n\
              BT /F1 12 Tf 320 750 Td (droite commence) Tj ET\n\
              BT /F1 12 Tf 320 734 Td (deuxieme ligne droite) Tj ET",
        );
        let page = extract_page_text_report(&doc, 1, false, false, false).expect("page text");
        assert!(
            page.text.contains("la suite de la phrase\n\ndroite commence"),
            "a near-height two-column start must keep its blank line: {:?}",
            page.text
        );
    }

    #[test]
    fn a_label_and_value_on_one_baseline_stay_on_one_line() {
        // A label/value cell row shares a baseline; the far x jump is a cell
        // placement, never a paragraph break.
        let doc = content_doc(
            b"BT /F1 12 Tf 50 760 Td (Nom du client :) Tj ET\n\
              BT /F1 12 Tf 300 760 Td (Dupont Jean) Tj ET",
        );
        let page = extract_page_text_report(&doc, 1, false, false, false).expect("page text");
        assert!(
            !page.text.contains("\n\n"),
            "label and value on one baseline must stay joined: {:?}",
            page.text
        );
    }

    #[test]
    fn a_hanging_indent_continuation_stays_joined() {
        // The second line of a hanging-indent paragraph starts further right
        // but *below* the first: no vertical rise, so it must stay in the same
        // paragraph.
        let doc = content_doc(
            b"BT /F1 12 Tf 50 760 Td (premiere ligne du paragraphe) Tj ET\n\
              BT /F1 12 Tf 70 744 Td (suite indentee du meme paragraphe) Tj ET\n\
              BT /F1 12 Tf 70 728 Td (encore la suite de ce paragraphe) Tj ET",
        );
        let page = extract_page_text_report(&doc, 1, false, false, false).expect("page text");
        assert!(
            !page.text.contains("\n\n"),
            "a hanging indent must not break the paragraph: {:?}",
            page.text
        );
    }

    #[test]
    fn a_rotated_tm_block_does_not_disable_later_gap_inference() {
        // A rotated `Tm` (an angled watermark or badge) used to set the
        // walker's `unusable` flag for the rest of the content stream, so
        // every later `Td`-positioned word boundary lost its separator and
        // the text welded into long tokens. `BT` resets the text matrix, so
        // the flag must be scoped to its own text object. Courier's 600/1000
        // advance makes the numbers exact: at size 10 a glyph advances 6.0,
        // so a 12.0 `Td` leaves a 6.0 gap past the glyph end — well over the
        // 0.1625 em word-gap threshold.
        let doc = content_doc(
            b"BT /F1 10 Tf 0.940 0.342 -0.342 0.940 259 387 Tm (N) Tj ET\n\
              BT /F1 10 Tf 100 700 Td (A) Tj 12 0 Td (B) Tj 12 0 Td (C) Tj ET",
        );
        let page = extract_page_text_report(&doc, 1, false, false, false).expect("page text");
        assert!(
            page.text.contains("A B C"),
            "gap inference must resume after a rotated Tm block: {:?}",
            page.text
        );
    }

    #[test]
    fn control_space_code_maps_to_space_by_name_or_width() {
        // A font's own encoding can define a space glyph while its `/ToUnicode`
        // maps that code to a C0 control (U+0001 here). The control is unusable
        // text and is stripped downstream, welding the words around it, so the
        // font's meaning must win. U+0000 is a producer's "unmapped/symbol"
        // target and must stay as it is, or a word gains a break inside it.
        use lopdf::Stream;
        let mut enc = Dictionary::new();
        enc.set(b"Type", Object::Name(b"Encoding".to_vec()));
        enc.set(
            b"Differences",
            Object::Array(vec![
                Object::Integer(1),
                Object::Name(b"g1".to_vec()),
                Object::Name(b"g2".to_vec()),
                Object::Name(b"space".to_vec()),
            ]),
        );
        let cmap = b"beginbfchar\n<01> <0001>\n<02> <0000>\n<03> <0001>\nendbfchar\n".to_vec();
        let mut font = Dictionary::new();
        font.set(b"Subtype", Object::Name(b"Type3".to_vec()));
        font.set(
            b"FontMatrix",
            Object::Array(vec![
                Object::Real(0.001),
                Object::Real(0.0),
                Object::Real(0.0),
                Object::Real(0.001),
                Object::Real(0.0),
                Object::Real(0.0),
            ]),
        );
        font.set(
            b"FontBBox",
            Object::Array(vec![
                Object::Real(0.0),
                Object::Real(0.0),
                Object::Real(1000.0),
                Object::Real(1000.0),
            ]),
        );
        font.set(b"FirstChar", Object::Integer(1));
        font.set(b"Widths", Object::Array(vec![Object::Real(250.0); 3]));
        font.set(b"Encoding", Object::Dictionary(enc));
        font.set(
            b"ToUnicode",
            Object::Stream(Stream::new(Dictionary::new(), cmap)),
        );
        let doc = Document::new();
        let codec = resolve_codec(&doc, &font).expect("Type3 font resolves");
        let mut s = String::new();
        codec.decode(&[0x01], &mut s);
        assert_eq!(
            s, " ",
            "an unnamed space-width code mapped to a control must decode as a space"
        );
        let mut t = String::new();
        codec.decode(&[0x02], &mut t);
        assert_eq!(t, "\u{0}", "a NULL-mapped symbol glyph must not become a space");
        let mut u = String::new();
        codec.decode(&[0x03], &mut u);
        assert_eq!(
            u, " ",
            "a glyph named `space` mapped to a control must decode as a space"
        );
    }

    #[test]
    fn consecutive_absolute_tm_lines_do_not_weld() {
        // Producers that place every visual line with an absolute identity
        // `Tm` and no `ET`/`T*` between lines used to have every line
        // concatenated: only `Td`/`TD`/`ET` emitted a line break. A vertical
        // `Tm` must separate the lines like a vertical `Td` does.
        let doc = content_doc(
            b"BT /F1 10 Tf 1 0 0 1 50 700 Tm (alpha)Tj 1 0 0 1 50 688 Tm (beta)Tj ET",
        );
        let page = extract_page_text_report(&doc, 1, false, false, false).expect("page text");
        assert!(
            !page.text.contains("alphabeta"),
            "an absolute Tm line advance must separate the lines: {:?}",
            page.text
        );
    }
