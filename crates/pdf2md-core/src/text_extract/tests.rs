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

    /// Minimal single-page PDF whose four `Tj` lines live inside one `BT`/`ET`
    /// and are advanced by a vertical `Td` (no `ET`/`T*` between lines), with
    /// each show-string padded by a leading and trailing space — the shape of
    /// `scratch/samples/synth_ticket_compressed.pdf`.
    fn td_line_advance_doc() -> Document {
        use lopdf::{dictionary, Stream};

        let mut doc = Document::with_version("1.4");
        let font_id = doc.new_object_id();
        let content_id = doc.new_object_id();
        let page_id = doc.new_object_id();
        let pages_id = doc.new_object_id();
        let catalog_id = doc.new_object_id();

        doc.objects.insert(
            font_id,
            Object::Dictionary(dictionary! {
                "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica",
                "Encoding" => "WinAnsiEncoding",
            }),
        );
        let content = b"BT /F1 12 Tf 50 800 Td ( BILLET \xc9LECTRONIQUE ) Tj\n\
0 -20 Td ( R\xc9F\xc9RENCE DE VOTRE R\xc9SERVATION ) Tj\n\
0 -20 Td ( Obtenez votre carte d'embarquement. ) Tj ET"
            .to_vec();
        doc.objects
            .insert(content_id, Object::Stream(Stream::new(dictionary! {}, content)));
        doc.objects.insert(
            page_id,
            Object::Dictionary(dictionary! {
                "Type" => "Page", "Parent" => pages_id,
                "MediaBox" => Object::Array(vec![
                    Object::Integer(0), Object::Integer(0),
                    Object::Integer(595), Object::Integer(842),
                ]),
                "Resources" => dictionary! { "Font" => dictionary! { "F1" => font_id } },
                "Contents" => content_id,
            }),
        );
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => Object::Array(vec![Object::Reference(page_id)]),
                "Count" => 1,
            }),
        );
        doc.objects.insert(
            catalog_id,
            Object::Dictionary(dictionary! { "Type" => "Catalog", "Pages" => pages_id }),
        );
        doc.trailer.set("Root", catalog_id);
        doc
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

    /// Minimal single-page PDF with a base-14 Courier font (600/1000 em advance
    /// for every code, so advances are exactly predictable) and the caller's
    /// content stream.
    fn content_doc(content: &[u8]) -> Document {
        use lopdf::{dictionary, Stream};

        let mut doc = Document::with_version("1.4");
        let font_id = doc.new_object_id();
        let content_id = doc.new_object_id();
        let page_id = doc.new_object_id();
        let pages_id = doc.new_object_id();
        let catalog_id = doc.new_object_id();

        doc.objects.insert(
            font_id,
            Object::Dictionary(dictionary! {
                "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Courier",
                "Encoding" => "WinAnsiEncoding",
            }),
        );
        doc.objects
            .insert(content_id, Object::Stream(Stream::new(dictionary! {}, content.to_vec())));
        doc.objects.insert(
            page_id,
            Object::Dictionary(dictionary! {
                "Type" => "Page", "Parent" => pages_id,
                "MediaBox" => Object::Array(vec![
                    Object::Integer(0), Object::Integer(0),
                    Object::Integer(595), Object::Integer(842),
                ]),
                "Resources" => dictionary! { "Font" => dictionary! { "F1" => font_id } },
                "Contents" => content_id,
            }),
        );
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => Object::Array(vec![Object::Reference(page_id)]),
                "Count" => 1,
            }),
        );
        doc.objects.insert(
            catalog_id,
            Object::Dictionary(dictionary! { "Type" => "Catalog", "Pages" => pages_id }),
        );
        doc.trailer.set("Root", catalog_id);
        doc
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
    fn horizontal_td_gaps_inside_a_form_xobject_are_restored() {
        // a corpus file's shape: the whole text lives in a Form XObject whose content
        // is `Tj`-only, positioned by horizontal `Td`.
        use lopdf::{dictionary, Stream};

        let mut doc = Document::with_version("1.4");
        let font_id = doc.new_object_id();
        let form_id = doc.new_object_id();
        let page_id = doc.new_object_id();
        let pages_id = doc.new_object_id();
        let catalog_id = doc.new_object_id();

        doc.objects.insert(
            font_id,
            Object::Dictionary(dictionary! {
                "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Courier",
                "Encoding" => "WinAnsiEncoding",
            }),
        );
        let form_content = b"BT /F1 12 Tf (Bonjour) Tj 60 0 Td (le) Tj 20 0 Td (monde) Tj ET";
        let mut form_dict = dictionary! {
            "Type" => "XObject",
            "Subtype" => "Form",
            "BBox" => Object::Array(vec![
                Object::Integer(0), Object::Integer(0),
                Object::Integer(595), Object::Integer(842),
            ]),
            "Resources" => dictionary! { "Font" => dictionary! { "F1" => font_id } },
        };
        form_dict.set("Length", form_content.len() as i64);
        doc.objects.insert(
            form_id,
            Object::Stream(Stream::new(form_dict, form_content.to_vec())),
        );
        let page_content = b"q /Fm0 Do Q";
        let page_content_id = doc.new_object_id();
        doc.objects.insert(
            page_content_id,
            Object::Stream(Stream::new(dictionary! {}, page_content.to_vec())),
        );
        doc.objects.insert(
            page_id,
            Object::Dictionary(dictionary! {
                "Type" => "Page", "Parent" => pages_id,
                "MediaBox" => Object::Array(vec![
                    Object::Integer(0), Object::Integer(0),
                    Object::Integer(595), Object::Integer(842),
                ]),
                "Resources" => dictionary! { "XObject" => dictionary! { "Fm0" => form_id } },
                "Contents" => page_content_id,
            }),
        );
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => Object::Array(vec![Object::Reference(page_id)]),
                "Count" => 1,
            }),
        );
        doc.objects.insert(
            catalog_id,
            Object::Dictionary(dictionary! { "Type" => "Catalog", "Pages" => pages_id }),
        );
        doc.trailer.set("Root", catalog_id);

        let page = extract_page_text_report(&doc, 1, true, true, false).expect("page text");
        assert_eq!(page.text, "Bonjour le monde");
    }

    /// Minimal single-page PDF whose page draws one parent form, and whose
    /// parent form issues `repeats` `/FmLeaf Do`s of a leaf form that shows the
    /// word `mot`. Used to prove the string walker's shared per-page `Do` budget
    /// bounds a form-DAG expansion (mirrors the glyph engine's budget test).
    fn repeating_form_doc(repeats: usize) -> Document {
        use lopdf::{dictionary, Stream};

        let mut doc = Document::with_version("1.5");
        let font_id = doc.new_object_id();
        let leaf_id = doc.new_object_id();
        let parent_id = doc.new_object_id();
        let page_content_id = doc.new_object_id();
        let page_id = doc.new_object_id();
        let pages_id = doc.new_object_id();
        let catalog_id = doc.new_object_id();

        doc.objects.insert(
            font_id,
            Object::Dictionary(dictionary! {
                "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Courier",
                "Encoding" => "WinAnsiEncoding",
            }),
        );
        let leaf_content = b"BT /F1 12 Tf (mot) Tj ET".to_vec();
        doc.objects.insert(
            leaf_id,
            Object::Stream(Stream::new(
                dictionary! {
                    "Type" => "XObject", "Subtype" => "Form",
                    "BBox" => Object::Array(vec![
                        Object::Integer(0), Object::Integer(0),
                        Object::Integer(595), Object::Integer(842),
                    ]),
                    "Resources" => dictionary! { "Font" => dictionary! { "F1" => font_id } },
                    "Length" => leaf_content.len() as i64,
                },
                leaf_content,
            )),
        );
        let mut parent_content = Vec::new();
        for _ in 0..repeats {
            parent_content.extend_from_slice(b"/FmLeaf Do\n");
        }
        doc.objects.insert(
            parent_id,
            Object::Stream(Stream::new(
                dictionary! {
                    "Type" => "XObject", "Subtype" => "Form",
                    "BBox" => Object::Array(vec![
                        Object::Integer(0), Object::Integer(0),
                        Object::Integer(595), Object::Integer(842),
                    ]),
                    "Resources" =>
                        dictionary! { "XObject" => dictionary! { "FmLeaf" => leaf_id } },
                    "Length" => parent_content.len() as i64,
                },
                parent_content,
            )),
        );
        let page_content = b"/Fm0 Do".to_vec();
        doc.objects.insert(
            page_content_id,
            Object::Stream(Stream::new(dictionary! {}, page_content)),
        );
        doc.objects.insert(
            page_id,
            Object::Dictionary(dictionary! {
                "Type" => "Page", "Parent" => pages_id,
                "MediaBox" => Object::Array(vec![
                    Object::Integer(0), Object::Integer(0),
                    Object::Integer(595), Object::Integer(842),
                ]),
                "Resources" => dictionary! { "XObject" => dictionary! { "Fm0" => parent_id } },
                "Contents" => page_content_id,
            }),
        );
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => Object::Array(vec![Object::Reference(page_id)]),
                "Count" => 1,
            }),
        );
        doc.objects.insert(
            catalog_id,
            Object::Dictionary(dictionary! { "Type" => "Catalog", "Pages" => pages_id }),
        );
        doc.trailer.set("Root", catalog_id);
        doc
    }

    #[test]
    fn a_form_drawn_twice_is_walked_twice() {
        // The legitimate repeated-`Do` case (same form, two invocations) must
        // still yield both drawings: the budget only stops pathological
        // expansion, it must not deduplicate.
        let doc = repeating_form_doc(2);
        let page = extract_page_text_report(&doc, 1, true, true, false).expect("page text");
        assert_eq!(page.text.matches("mot").count(), 2);
    }

    #[test]
    fn wide_form_expansion_hits_the_do_budget_and_terminates() {
        // One parent form issues far more `Do`s than the per-page budget allows.
        // Without the shared budget the walker would decode every invocation.
        let repeats = MAX_WALKER_DO_PER_PAGE + 88;
        let doc = repeating_form_doc(repeats);
        let page = extract_page_text_report(&doc, 1, true, true, false).expect("page text");
        let n = page.text.matches("mot").count();
        assert!(
            n <= MAX_WALKER_DO_PER_PAGE,
            "expansion must stop at the shared Do budget, got {n}"
        );
        assert!(
            n >= MAX_WALKER_DO_PER_PAGE - 1,
            "the budget must actually be reached, got {n}"
        );
    }

    #[test]
    fn digital_probe_is_bounded_on_a_form_dag() {
        // `is_digital_pdf_bytes` must find the leaf text through the form DAG
        // (and must not expand it without bound to do so).
        let repeats = MAX_WALKER_DO_PER_PAGE + 88;
        let mut doc = repeating_form_doc(repeats);
        let mut bytes = Vec::new();
        doc.save_to(&mut bytes).expect("save pdf");
        assert!(crate::is_digital_pdf_bytes(&bytes));
    }

    /// A page drawing `n` DISTINCT forms (one word each), the shape that lost
    /// every form past the old 512 `Do`/page cap (qa-int2 `forms_distinct_*`).
    fn distinct_forms_doc(n: usize) -> Document {
        use lopdf::{dictionary, Stream};

        let mut doc = Document::with_version("1.5");
        let font_id = doc.add_object(dictionary! {
            "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Courier",
            "Encoding" => "WinAnsiEncoding",
        });
        let leaf_res = dictionary! { "Font" => dictionary! { "F1" => font_id } };
        let mut xobjects = lopdf::Dictionary::new();
        let mut page_content = Vec::new();
        for i in 0..n {
            let word = format!("w{i:04}");
            let leaf_content = format!("BT /F1 12 Tf ({word}) Tj ET").into_bytes();
            let leaf = Stream::new(
                dictionary! {
                    "Type" => "XObject", "Subtype" => "Form",
                    "BBox" => Object::Array(vec![
                        Object::Integer(0), Object::Integer(0),
                        Object::Integer(595), Object::Integer(842),
                    ]),
                    "Resources" => leaf_res.clone(),
                    "Length" => leaf_content.len() as i64,
                },
                leaf_content,
            );
            let id = doc.add_object(leaf);
            xobjects.set(format!("Fm{i}"), Object::Reference(id));
            page_content.extend_from_slice(format!("/Fm{i} Do\n").as_bytes());
        }
        let page_content_id = doc.add_object(Stream::new(dictionary! {}, page_content));
        let pages_id = doc.new_object_id();
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page", "Parent" => pages_id,
            "MediaBox" => Object::Array(vec![
                Object::Integer(0), Object::Integer(0),
                Object::Integer(595), Object::Integer(842),
            ]),
            "Resources" => dictionary! { "XObject" => xobjects },
            "Contents" => page_content_id,
        });
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages", "Kids" => Object::Array(vec![Object::Reference(page_id)]),
                "Count" => 1,
            }),
        );
        let catalog_id = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        doc.trailer.set("Root", catalog_id);
        doc
    }

    #[test]
    fn many_distinct_forms_are_all_walked() {
        // 1200 distinct one-word forms: every invocation must be walked once
        // (qa-int2 `forms_distinct_1200`), and the words must survive reflow.
        let doc = distinct_forms_doc(1200);
        let page = extract_page_text_report(&doc, 1, true, true, false).expect("page text");
        let words = page.text.split_whitespace().filter(|w| w.starts_with('w')).count();
        assert_eq!(words, 1200, "all distinct forms must survive");
    }

    #[test]
    fn a_form_repeated_600_times_is_walked_600_times() {
        // One form drawn 600 times: repeated invocations are content, not
        // overdraw (qa-int2 `forms_repeat_600`).
        let doc = repeating_form_doc(600);
        let page = extract_page_text_report(&doc, 1, true, true, false).expect("page text");
        assert_eq!(page.text.matches("mot").count(), 600);
        assert!(
            !page.budget_exhausted,
            "600 invocations are well inside the budgets"
        );
    }

    #[test]
    fn a_page_over_the_do_cap_reports_budget_exhausted() {
        // qa-int3 `forms_distinct_20000`: a page whose `Do` count exceeds the
        // per-page cap is truncated, but the truncation must be reported, not
        // silent. A repeated parent form drives the same `do_left` bound with a
        // far cheaper fixture than 16k+ distinct form objects.
        let doc = repeating_form_doc(MAX_WALKER_DO_PER_PAGE + 88);
        let page = extract_page_text_report(&doc, 1, true, true, false).expect("page text");
        assert!(
            page.budget_exhausted,
            "a page over the Do cap must set budget_exhausted"
        );
    }

    #[test]
    fn a_page_under_the_do_cap_does_not_report_budget_exhausted() {
        let doc = distinct_forms_doc(600);
        let page = extract_page_text_report(&doc, 1, true, true, false).expect("page text");
        let words = page.text.split_whitespace().filter(|w| w.starts_with('w')).count();
        assert_eq!(words, 600);
        assert!(
            !page.budget_exhausted,
            "600 distinct forms are inside the budgets"
        );
    }

    #[test]
    fn ligature_hex_and_suffix_glyph_names_map() {
        assert_eq!(glyph_unicode(b"fi"), Some(0xFB01));
        assert_eq!(glyph_unicode(b"ffl"), Some(0xFB04));
        assert_eq!(glyph_unicode(b"a.sc"), Some(0x61));
        assert_eq!(glyph_unicode(b"eacute.sc"), Some(0xE9));
        assert_eq!(glyph_unicode(b"f_i"), Some(0xFB01));
        assert_eq!(glyph_unicode(b"u00E9"), Some(0xE9));
        // Astral code point: the byte table is 16-bit, so it is not mappable.
        assert_eq!(glyph_unicode(b"u1F600"), None);
        // Multi-unit `uni` sequence spelling a ligature.
        assert_eq!(glyph_unicode(b"uni00660069"), Some(0xFB01));
        assert_eq!(glyph_unicode(b"uni1EA1"), Some(0x1EA1));
    }

    #[test]
    fn winansi_soft_hyphen_decodes_as_a_hyphen() {
        let codec = Codec::Byte8(ByteTable(WIN_ANSI), PuaFamily::Unknown);
        let mut s = String::new();
        codec.decode(&[0xAD], &mut s);
        assert_eq!(s, "-");
    }

    #[test]
    fn decoded_text_normalises_ligatures_hyphens_and_nbsp() {
        let input = "1\u{00A0}234,56 \u{FB01}n x\u{00AD}y";
        assert_eq!(normalize_decoded_text(input), "1 234,56 fin xy");
        // An NBSP that is not a digit separator is left untouched.
        assert_eq!(normalize_decoded_text("a\u{00A0}b"), "a\u{00A0}b");
    }

    #[test]
    fn symbol_pua_is_mapped_and_unmapped_pua_is_dropped() {
        // Wingdings/Symbol private-use glyphs map to their Unicode equivalent.
        assert_eq!(normalize_decoded_text("a\u{F0B7}b"), "a\u{2022}b");
        assert_eq!(normalize_decoded_text("a\u{F0A7}b"), "a\u{25AA}b");
        assert_eq!(normalize_decoded_text("a\u{F0FC}b"), "a\u{2714}b");
        assert_eq!(normalize_decoded_text("\u{F0D8}"), "\u{2B9A}");
        // The full Adobe Symbol charset is now covered: U+F0B9 is NOT EQUAL TO.
        assert_eq!(normalize_decoded_text("a\u{F0B9}b"), "a\u{2260}b");
        // Greek letters from a Symbol-family ToUnicode CMap survive.
        assert_eq!(normalize_decoded_text("\u{F061}\u{F062}"), "\u{03B1}\u{03B2}");
        // A private-use glyph outside the Symbol range, or one the table does
        // not know, is dropped, not emitted.
        assert_eq!(normalize_decoded_text("a\u{E123}b"), "ab");
        assert_eq!(normalize_decoded_text("a\u{F0123}b"), "ab");
        assert_eq!(normalize_decoded_text("a\u{F0D2}b"), "ab"); // CUS-only code
    }

    #[test]
    fn pua_mapping_is_font_family_aware() {
        use crate::glyph_data::{pua_to_char_for_family, PuaFamily};
        // 0x52 means Rho in the Symbol charset but a sun in the corpus-verified
        // Wingdings census: the family selects which one is emitted.
        assert_eq!(
            pua_to_char_for_family(0xF052, PuaFamily::Symbol),
            Some('\u{03A1}')
        );
        assert_eq!(
            pua_to_char_for_family(0xF052, PuaFamily::Wingdings),
            Some('\u{263C}')
        );
        // The font-unknown union keeps the corpus-verified value on a collision.
        assert_eq!(pua_to_char_for_family(0xF052, PuaFamily::Unknown), Some('\u{263C}'));
        assert_eq!(
            crate::glyph_data::pua_family_for_base_font(b"ABCDEF+Symbol"),
            PuaFamily::Symbol
        );
        assert_eq!(
            crate::glyph_data::pua_family_for_base_font(b"ABCDEF+Wingdings"),
            PuaFamily::Wingdings
        );
        assert_eq!(
            crate::glyph_data::pua_family_for_base_font(b"ABCDEF+ArialMT"),
            PuaFamily::Unknown
        );
    }

    #[test]
    fn symbol_font_tounicode_pua_decodes_through_the_symbol_charset() {
        // A Type1 Symbol-family font whose ToUnicode maps 0x61/0x62 to the
        // private-use convention must decode to Greek alpha/beta.
        let cmap = b"beginbfchar\n<61> <F061>\n<62> <F062>\nendbfchar\n".to_vec();
        let mut font = Dictionary::new();
        font.set(b"Subtype", Object::Name(b"Type1".to_vec()));
        font.set(b"BaseFont", Object::Name(b"ABCDEF+Symbol".to_vec()));
        font.set(b"ToUnicode", Object::Stream(lopdf::Stream::new(Dictionary::new(), cmap)));
        font.set(
            b"Encoding",
            Object::Name(b"WinAnsiEncoding".to_vec()),
        );
        let doc = Document::new();
        let codec = resolve_codec(&doc, &font).expect("symbol font resolves");
        let mut s = String::new();
        codec.decode(&[0x61, 0x62], &mut s);
        assert_eq!(s, "\u{03B1}\u{03B2}");
    }

    #[test]
    fn cmap_tolerates_whitespace_inside_hex() {
        // `< 0041 >` is the same two-byte code as `<0041>`; the whitespace must
        // not make the entry unparseable.
        let cmap = b"beginbfchar\n< 0041 > < 0041 >\nendbfchar\n";
        let cm = parse_cmap(cmap).expect("parse");
        let codec = Codec::CMap(cm, None, PuaFamily::Unknown);
        let mut s = String::new();
        codec.decode(&[0x00, 0x41], &mut s);
        assert_eq!(s, "A");
    }

    #[test]
    fn cmap_partial_coverage_falls_back_per_code() {
        // Only 'A' is named by the CMap; 'B' must come from the byte table.
        let cmap = b"beginbfchar\n<41> <0041>\nendbfchar\n";
        let cm = parse_cmap(cmap).expect("parse");
        let codec = Codec::CMap(cm, Some(ByteTable(WIN_ANSI)), PuaFamily::Unknown);
        let mut s = String::new();
        codec.decode(&[0x41, 0x42], &mut s);
        assert_eq!(s, "AB");
    }

    #[test]
    fn code_metrics_counts_codes_not_bytes_for_cid_fonts() {
        // A 2-byte Identity-H run `A B` (with a space code between) is three
        // character codes, not six bytes, and exactly one of them is a space.
        // The old byte-length accounting doubled the `Tc` term and could
        // mistake any 0x20 low byte for a space, fusing the next word (D2).
        let cmap = b"beginbfchar\n<0041> <0041>\n<0020> <0020>\n<0042> <0042>\nendbfchar\n";
        let cm = parse_cmap(cmap).expect("parse");
        let codec = Codec::CMap(cm, None, PuaFamily::Unknown);
        let bytes = [0x00, 0x41, 0x00, 0x20, 0x00, 0x42];
        assert_eq!(codec.code_metrics(&bytes), (3, 1));
        // A matching byte-oriented font counts every byte as one code.
        let byte = Codec::Byte8(ByteTable(WIN_ANSI), PuaFamily::Unknown);
        assert_eq!(byte.code_metrics(&[b'A', b' ', b'B']), (3, 1));
    }

    #[test]
    fn real_negative_kerning_is_a_word_gap() {
        let codec = Codec::Byte8(ByteTable(WIN_ANSI), PuaFamily::Unknown);
        let ops = vec![Object::Array(vec![
            Object::String(b"Total".to_vec(), lopdf::StringFormat::Literal),
            Object::Real(-200.0),
            Object::String(b"HT".to_vec(), lopdf::StringFormat::Literal),
        ])];
        let mut out = String::new();
        show_text(&mut out, &codec, &ops);
        assert_eq!(out, "Total HT");
    }

    #[test]
    fn q_restores_the_text_font_after_a_scope() {
        // Inside `q … Q` an unknown font is selected; the surrounding `Tj` runs
        // must keep decoding through the font selected before `q`.
        let doc = content_doc(
            b"BT /F1 12 Tf (AB) Tj q /F0 12 Tf (XY) Tj Q (CD) Tj ET",
        );
        let page = extract_page_text_report(&doc, 1, true, true, false).expect("page text");
        assert_eq!(page.text, "ABCD");
    }

    #[test]
    fn rotated_ctm_page_falls_back_to_the_string_walker() {
        // a corpus file's shape: a rotated `cm` makes the geometry engine classify
        // every span as vertical and, with layout analysis on, it drops them
        // from `text` (keeping only blocks). The walker must be used instead of
        // reporting "no readable words".
        let doc = content_doc(
            b"q 0 -0.2215 0.2215 0 0 792 cm BT /F1 1 Tf \
              24.6667 0 0 40 1.25 100 Tm (Bonjour) Tj ET Q",
        );
        let page = extract_page_text_report(&doc, 1, true, true, false).expect("page text");
        assert_eq!(page.text.split_whitespace().collect::<Vec<_>>(), ["Bonjour"]);
    }