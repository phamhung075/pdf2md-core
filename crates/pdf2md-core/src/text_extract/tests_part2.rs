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
        use lopdf::dictionary;

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

    #[test]
    fn tj_td_two_column_table_routes_to_layout_and_keeps_values_detached() {
        // F0686/F0687/F0688 shape: every cell is drawn with a `TJ` array and a
        // plain `x y Td` fragment placement (no `Tj`, no `Tm`, no `TD`). Before
        // the routing fix such a page fell through to the string walker, which
        // never runs table detection and emits each visual column as its own
        // line ("detached amount alone on a line"). It must now reach the
        // layout engine and be recovered as a GFM table.
        let doc = content_doc(
            b"BT /F1 12 Tf 50 760 Td [(Reference)] TJ ET\n\
              BT /F1 12 Tf 320 760 Td [(Montant)] TJ ET\n\
              BT /F1 12 Tf 50 740 Td [(A1)] TJ ET\n\
              BT /F1 12 Tf 320 740 Td [(10,00)] TJ ET\n\
              BT /F1 12 Tf 50 720 Td [(B2)] TJ ET\n\
              BT /F1 12 Tf 320 720 Td [(20,00)] TJ ET",
        );
        let page = extract_page_text_report(&doc, 1, true, true, false).expect("page text");
        assert!(
            page.tables >= 1,
            "TJ+Td grid must be recovered as a table:\n{}",
            page.text
        );
        assert!(
            page.text.contains("---"),
            "GFM table separator missing:\n{}",
            page.text
        );
        let row = page
            .text
            .lines()
            .find(|l| l.contains("A1"))
            .expect("A1 row present");
        assert!(row.contains("10,00"), "amount detached from its row: {row:?}");
        assert!(
            !page.text.lines().any(|l| l.trim() == "10,00"),
            "amount must not be alone on a line:\n{}",
            page.text
        );
    }

    #[test]
    fn tj_td_page_keeps_a_one_off_vertical_margin_code() {
        // Same `TJ`+`Td` shape as the test above, plus a rotated margin code
        // drawn with a `cm` rotation (no `Tm`, so the page is still a
        // `new_geometry` route). 0.2.8 read this page with the string walker,
        // which emitted the code; the layout engine keeps vertical runs out of
        // the horizontal body, so moving the page to the table engine must put
        // the one-off code back instead of losing it. A page that was already on
        // the layout engine in 0.2.8 keeps its exact old output.
        let doc = content_doc(
            b"BT /F1 12 Tf 50 760 Td [(Reference)] TJ ET\n\
              BT /F1 12 Tf 320 760 Td [(Montant)] TJ ET\n\
              BT /F1 12 Tf 50 740 Td [(A1)] TJ ET\n\
              BT /F1 12 Tf 320 740 Td [(10,00)] TJ ET\n\
              BT /F1 12 Tf 50 720 Td [(B2)] TJ ET\n\
              BT /F1 12 Tf 320 720 Td [(20,00)] TJ ET\n\
              q 0 1 -1 0 560 150 cm BT /F1 12 Tf 0 0 Td [(VERTCODE)] TJ ET Q",
        );
        let page = extract_page_text_report(&doc, 1, true, true, false).expect("page text");
        assert!(
            page.tables >= 1,
            "the grid must still be recovered:\n{}",
            page.text
        );
        assert!(
            page.text.contains("VERTCODE"),
            "a one-off vertical margin code was dropped by the layout path:\n{}",
            page.text
        );
    }

    #[test]
    fn a_blank_line_after_a_content_comment_does_not_drop_the_page() {
        // PReS/PrintSoft writes a `%` metadata header followed by a blank line.
        // lopdf's content parser stops there, so the page looked text-free and
        // every numeric token after the header was lost (the F0694-F0705
        // walker-page "true numeric loss" class). The sanitised decode must
        // recover the operators and keep the amount.
        let doc = content_doc(
            b"1 0 0 1 0 0 cm\n% generated header\n\n\
              BT /F1 12 Tf 50 760 Td [(1234,56)] TJ ET",
        );
        let page = extract_page_text_report(&doc, 1, true, true, false).expect("page text");
        assert!(
            page.text.contains("1234,56"),
            "amount after a comment header was lost: {:?}",
            page.text
        );
    }
