//! pdf2md-core — High-performance native Rust core engine for sub-millisecond
//! PDF-to-Markdown extraction and 2D spatial canvas table reconstruction.
//!
//! Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
//! SPDX-License-Identifier: BSL-1.1
//! Licensed under the Business Source License 1.1 (BSL-1.1).

use super::*;
use super::regression_tests_common::*;
use super::regression_tests_common2::*;
use std::io::Write as _;
    

    /// Bug 4 regression: an academic caption is often a descriptive paragraph
    /// well over nine words. A `**Figure 1:` / `Figure` / `Fig.` prefixed block
    /// within a slightly looser gap must still be a caption, while an unrelated
    /// long paragraph whose first word merely starts with "Figure" must not.
    #[test]
    fn descriptive_figure_caption_with_many_words_is_recognized() {
        let text = "**Figure 1: Sliding Window Attention.** The number of \
                    operations in vanilla attention is quadratic in the sequence \
                    length, so the attention diagram spans the full page width.";
        assert!(
            text.split_whitespace().count() > 9,
            "test premise: the caption paragraph must exceed nine words"
        );
        // 3.0 * size_hint sits inside the extended 4.0 * size_hint caption gap.
        assert!(
            is_caption_block(text, 3.0 * 12.0, 12.0),
            "a long Figure-prefixed paragraph must be recognized as a caption"
        );
        // Beyond even the extended gap it is no longer treated as a caption.
        assert!(!is_caption_block(text, 4.5 * 12.0, 12.0));

        // A short line far away is not a caption either.
        assert!(!is_caption_block("unrelated body text", 3.0 * 12.0, 12.0));
    }

    #[test]
    fn inline_resources_inherited_from_pages_are_resolved() {
        let bytes = inherited_inline_resources_pdf();
        // Documenting the root cause: the stock lopdf resolver misses the
        // inline ancestor dictionary, so without our fix no codec resolves.
        let stock = lopdf::Document::load_mem(&bytes).unwrap();
        let page_id = *stock.get_pages().values().next().unwrap();
        assert_eq!(
            stock.get_page_fonts(page_id).map(|f| f.len()).unwrap_or(0),
            0,
            "premise: lopdf must miss inline /Pages resources for this regression to matter"
        );

        assert!(is_digital_pdf_bytes(&bytes));
        let res = convert_pdf_bytes_to_markdown(&bytes, &ConversionOptions::default())
            .expect("text must decode once inherited resources resolve");
        assert!(
            res.markdown.contains("Inherited Resource Text"),
            "got: {}",
            res.markdown
        );
    }

    #[test]
    fn text_inside_form_xobjects_is_detected_and_extracted() {
        let bytes = form_xobject_text_pdf();
        assert!(
            is_digital_pdf_bytes(&bytes),
            "a page whose only text lives in a Form XObject must classify as digital"
        );
        let res = convert_pdf_bytes_to_markdown(&bytes, &ConversionOptions::default())
            .expect("Form XObject text must be extracted");
        assert!(res.markdown.contains("Form XObject Text"), "got: {}", res.markdown);
    }

    #[test]
    fn trailing_bytes_after_eof_do_not_defeat_loading() {
        let mut padded = inherited_inline_resources_pdf();
        // Host buffers pad files with NULs; once the tail exceeds lopdf's
        // last-512-byte `startxref` window the classic xref is unreadable.
        padded.extend(std::iter::repeat(0u8).take(2048));
        assert!(
            lopdf::Document::load_mem(&padded).is_err(),
            "premise: the padded file must fail the stock loader"
        );
        let doc = load_pdf_document(&padded).expect("recovery must trim past %%EOF");
        assert_eq!(doc.get_pages().len(), 1);
        assert!(is_digital_pdf_bytes(&padded));
    }

    #[test]
    fn objstm_objects_separated_by_comments_are_recovered() {
        // lopdf parses ObjStm entries with a parser that does not skip `%`
        // comments; real producers (ORNIKAR CGV) prefix every embedded object
        // with `% N G`, so the whole page tree vanishes. The recovery blanks
        // those comments and re-parses.
        let index = b"3 0 4 36\n";
        let mut content = index.to_vec();
        content.extend_from_slice(b"% 3 0\n<< /Type /Pages /Count 0 >>\n");
        content.extend_from_slice(b"% 4 0\n<< /Type /Catalog >>\n");
        let mut dict = lopdf::Dictionary::new();
        dict.set(b"Type", lopdf::Object::Name(b"ObjStm".to_vec()));
        dict.set(b"N", lopdf::Object::Integer(2));
        dict.set(b"First", lopdf::Object::Integer(index.len() as i64));
        let mut doc = lopdf::Document::new();
        doc.add_object(lopdf::Object::Stream(lopdf::Stream::new(dict, content)));
        // Premise: without the recovery, the comment-prefixed objects are lost.
        assert!(!doc.objects.contains_key(&(3, 0)));
        let doc = recover_object_streams(doc);
        assert!(
            doc.objects.contains_key(&(3, 0)),
            "comment-separated ObjStm objects must be recovered"
        );
        assert!(doc.objects.contains_key(&(4, 0)));
    }

    #[test]
    fn self_referential_form_is_walked_once() {
        let bytes = self_referential_form_pdf();
        assert!(is_digital_pdf_bytes(&bytes));
        let res = convert_pdf_bytes_to_markdown(&bytes, &ConversionOptions::default())
            .expect("text after the self-reference must be extracted");
        assert_eq!(
            res.markdown.matches("Self Form Text").count(),
            1,
            "a form that draws itself must be entered once, got: {}",
            res.markdown
        );
    }

    #[test]
    fn object_stream_bomb_is_rejected_without_inflating() {

        // A payload far above the per-stream cap that still compresses to a few
        // KiB: the classic decompression-bomb shape. The first object is valid,
        // so an unbounded decoder would recover it and allocate the whole 20 MiB.
        let index = b"0 0 ";
        let mut payload = index.to_vec();
        payload.extend_from_slice(b"<< /Type /Pages /Kids [] /Count 1 >>");
        payload.resize(MAX_DECOMPRESSED_STREAM + (4 << 20), b'A');
        let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
        enc.write_all(&payload).unwrap();
        let compressed = enc.finish().unwrap();
        assert!(
            compressed.len() < 64 * 1024,
            "the bomb must be small on disk, got {} bytes",
            compressed.len()
        );

        let mut dict = lopdf::Dictionary::new();
        dict.set(b"Type", lopdf::Object::Name(b"ObjStm".to_vec()));
        dict.set(b"N", lopdf::Object::Integer(1));
        dict.set(b"First", lopdf::Object::Integer(index.len() as i64));
        dict.set(b"Filter", lopdf::Object::Name(b"FlateDecode".to_vec()));

        // Premise: the guard, not the data, is what keeps this finite.
        let probe = lopdf::Stream::new(dict.clone(), compressed.clone());
        assert!(
            probe
                .decompressed_content_with_limit(MAX_DECOMPRESSED_STREAM)
                .is_err(),
            "premise: the payload must exceed the per-stream cap"
        );

        let mut doc = lopdf::Document::new();
        let mut pages = lopdf::Dictionary::new();
        pages.set(b"Type", lopdf::Object::Name(b"Pages".to_vec()));
        pages.set(b"Kids", lopdf::Object::Array(Vec::new()));
        pages.set(b"Count", lopdf::Object::Integer(0));
        let pages_id = doc.add_object(lopdf::Object::Dictionary(pages));
        doc.add_object(lopdf::Object::Stream(lopdf::Stream::new(dict, compressed)));
        let bytes = finish_catalog(&mut doc, pages_id);

        let loaded = load_pdf_document(&bytes).expect("a bomb must not fail the load");
        assert!(
            loaded.get_pages().is_empty(),
            "the bomb object stream must be skipped, not expanded"
        );
        assert!(
            convert_pdf_bytes_to_markdown(&bytes, &ConversionOptions::default()).is_err(),
            "the bomb must fall through to the explicit no-text-layer error"
        );
    }

    #[test]
    fn content_stream_bomb_is_rejected_without_inflating() {
        let bytes = content_stream_bomb_pdf();
        // The font makes the probe report a digital layer without decoding the
        // bomb, so this exercises the conversion path, not the probe.
        assert!(is_digital_pdf_bytes(&bytes));

        let doc = load_pdf_document(&bytes).expect("a bomb must not fail the load");
        let page_id = *doc.get_pages().values().next().unwrap();
        let err = text_extract::decode_page_content(&doc, page_id)
            .expect_err("an over-cap page stream must be rejected");
        assert!(
            matches!(
                err,
                lopdf::Error::Decompress(lopdf::DecompressError::MemoryLimitExceeded { .. })
            ),
            "expected a limit error, got {err:?}"
        );

        let res = convert_pdf_bytes_to_markdown(&bytes, &ConversionOptions::default());
        assert!(
            res.is_err(),
            "an undecodable page must fail explicitly instead of emitting a prefix, got: {res:?}"
        );
    }

    #[test]
    fn image_xobject_bomb_is_rejected_before_allocating() {

        let doc = lopdf::Document::new();

        // Declared tiny, but the Flate stream inflates past the sample cap.
        let payload = vec![0u8; media::MAX_IMAGE_SAMPLES + (1 << 20)];
        let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
        enc.write_all(&payload).unwrap();
        let compressed = enc.finish().unwrap();

        let mut dict = lopdf::Dictionary::new();
        dict.set(b"Width", lopdf::Object::Integer(64));
        dict.set(b"Height", lopdf::Object::Integer(64));
        dict.set(b"BitsPerComponent", lopdf::Object::Integer(8));
        dict.set(b"ColorSpace", lopdf::Object::Name(b"DeviceGray".to_vec()));
        dict.set(b"Filter", lopdf::Object::Name(b"FlateDecode".to_vec()));
        let bomb = lopdf::Object::Stream(lopdf::Stream::new(dict, compressed));
        assert!(
            media::raster::decode_xobject_bytes(&doc, &bomb, u32::MAX).is_none(),
            "an inflating image stream must be rejected"
        );

        // A huge declared frame is rejected without touching the stream bytes.
        let mut dict = lopdf::Dictionary::new();
        dict.set(b"Width", lopdf::Object::Integer(100_000));
        dict.set(b"Height", lopdf::Object::Integer(100_000));
        dict.set(b"BitsPerComponent", lopdf::Object::Integer(8));
        dict.set(b"ColorSpace", lopdf::Object::Name(b"DeviceGray".to_vec()));
        let huge = lopdf::Object::Stream(lopdf::Stream::new(dict, vec![0u8; 16]));
        assert!(
            media::raster::decode_xobject_bytes(&doc, &huge, u32::MAX).is_none(),
            "a 100000x100000 declaration must be rejected"
        );

        // Just past the pixel cap at a legal dimension.
        let mut dict = lopdf::Dictionary::new();
        dict.set(b"Width", lopdf::Object::Integer(16_384));
        dict.set(b"Height", lopdf::Object::Integer(16_384));
        dict.set(b"BitsPerComponent", lopdf::Object::Integer(8));
        dict.set(b"ColorSpace", lopdf::Object::Name(b"DeviceGray".to_vec()));
        let wide = lopdf::Object::Stream(lopdf::Stream::new(dict, vec![0u8; 16]));
        assert!(
            media::raster::decode_xobject_bytes(&doc, &wide, u32::MAX).is_none(),
            "a 16384x16384 declaration must be rejected"
        );
    }

    #[test]
    fn page_count_over_limit_is_rejected() {
        let mut doc = lopdf::Document::new();
        let mut pages = lopdf::Dictionary::new();
        pages.set(b"Type", lopdf::Object::Name(b"Pages".to_vec()));
        pages.set(b"Count", lopdf::Object::Integer(MAX_PAGES as i64 + 1));
        let pages_id = doc.add_object(lopdf::Object::Dictionary(pages));

        let mut kids = Vec::with_capacity(MAX_PAGES + 1);
        for _ in 0..=MAX_PAGES {
            let mut page = lopdf::Dictionary::new();
            page.set(b"Type", lopdf::Object::Name(b"Page".to_vec()));
            page.set(b"Parent", lopdf::Object::Reference(pages_id));
            page.set(
                b"MediaBox",
                lopdf::Object::Array(vec![
                    lopdf::Object::Integer(0),
                    lopdf::Object::Integer(0),
                    lopdf::Object::Integer(612),
                    lopdf::Object::Integer(792),
                ]),
            );
            kids.push(lopdf::Object::Reference(
                doc.add_object(lopdf::Object::Dictionary(page)),
            ));
        }
        doc.get_object_mut(pages_id)
            .unwrap()
            .as_dict_mut()
            .unwrap()
            .set(b"Kids", lopdf::Object::Array(kids));
        let bytes = finish_catalog(&mut doc, pages_id);

        let err = convert_pdf_bytes_to_markdown(&bytes, &ConversionOptions::default())
            .expect_err("a document over the page cap must fail explicitly");
        assert!(err.contains("page limit"), "unexpected error: {err}");
    }

    /// The furniture key masks only a page number set off from a `page`/`p.`
    /// marker by a separator, so a page-varying footer matches across pages
    /// while body prose and longer identifiers stay distinct.
    #[test]
    fn furniture_key_masks_only_separated_page_tails() {
        assert_eq!(
            furniture_line_key("Societe Exemple SAS - page 2"),
            furniture_line_key("Societe Exemple SAS - page 3")
        );
        assert_eq!(
            furniture_line_key("Mentions legales | page 12"),
            furniture_line_key("Mentions legales | page 47")
        );
        assert_ne!(
            furniture_line_key("corps page 1"),
            furniture_line_key("corps page 2"),
            "a page word embedded in body prose must not be normalized"
        );
        assert_ne!(
            furniture_line_key("13004 MARSEILLE"),
            furniture_line_key("13009 MARSEILLE"),
            "distinct postal codes must not collapse to one key"
        );
        assert_ne!(
            furniture_line_key("page 2024"),
            furniture_line_key("page 2025"),
            "four-digit year-like tokens must not be masked"
        );
    }

    #[test]
    fn page_counter_line_detection() {
        assert_eq!(parse_page_counter("2/7"), Some((2, Some(7))));
        assert_eq!(parse_page_counter(" 2 / 7 "), Some((2, Some(7))));
        assert_eq!(parse_page_counter("Page 3/9"), Some((3, Some(9))));
        assert_eq!(parse_page_counter("2 sur 7"), Some((2, Some(7))));
        assert_eq!(parse_page_counter("Page 12"), Some((12, None)));
        assert_eq!(parse_page_counter("12"), None, "a bare number is not a counter");
        assert_eq!(parse_page_counter("13004 MARSEILLE"), None);
        assert_eq!(parse_page_counter("RCS 542 107 651"), None);
    }

    /// Only corroborated counters fall: a standalone ratio repeated unchanged
    /// on every page must survive.
    #[test]
    fn suppress_page_counters_drops_corroborated_only() {
        let mut pages: Vec<(u32, String)> = (1..=4)
            .map(|i| (i, format!("corps {i}\n3/4")))
            .collect();
        suppress_page_counters(&mut pages);
        let all = pages.iter().map(|(_, s)| s.as_str()).collect::<Vec<_>>().join("\n");
        assert_eq!(all.matches("3/4").count(), 4, "uncorroborated ratio dropped: {all}");

        let mut counters: Vec<(u32, String)> = (1..=4)
            .map(|i| (i, format!("corps {i}\nPage {}/4", i)))
            .collect();
        suppress_page_counters(&mut counters);
        let call = counters.iter().map(|(_, s)| s.as_str()).collect::<Vec<_>>().join("\n");
        for i in 1..=4 {
            assert!(!call.contains(&format!("Page {i}/4")), "counter survived: {call}");
        }
    }

    /// The line-based pass drops a repeated band header after page 1 only on
    /// dense pages; a sparse page whose whole body repeats (a ticket) survives.
    #[test]
    fn strip_running_lines_keeps_sparse_repeated_content() {
        let filler = "l1\nl2\nl3\nl4\nl5\nl6\nl7";
        let mut pages = vec![
            (1u32, format!("head band\n{filler}\nbody alpha\n")),
            (2u32, format!("head band\n{filler}\nbody beta\n")),
            (3u32, format!("head band\n{filler}\nbody gamma\n")),
        ];
        strip_running_lines(&mut pages);
        let all = pages.iter().map(|(_, s)| s.as_str()).collect::<Vec<_>>().join("\n");
        assert_eq!(all.matches("head band").count(), 1, "repeated header kept once: {all}");
        for unique in ["body alpha", "body beta", "body gamma"] {
            assert!(all.contains(unique), "unique line {unique} was dropped: {all}");
        }

        let ticket = "TICKET DE CAISSE\nArticle un 5,00\nArticle deux 7,50\nTOTAL 12,50";
        let mut sparse: Vec<(u32, String)> = (1..=3).map(|i| (i, ticket.to_string())).collect();
        strip_running_lines(&mut sparse);
        let kept = sparse.iter().map(|(_, s)| s.as_str()).collect::<Vec<_>>().join("\n");
        assert_eq!(
            kept.matches("TICKET DE CAISSE").count(),
            3,
            "sparse repeated content must survive: {kept}"
        );
    }

    /// A multi-line footer block (running title + page-varying status table)
    /// repeats on every page: only the first occurrence survives, and every
    /// page's unique body block is kept.
    #[test]
    fn collapse_repeated_furniture_blocks_drops_footer_after_first_page() {
        let footer = |n: u32| {
            format!(
                "| Enedis-NOI-CF_110E | Page : {n}/4 |\n| --- | --- |\n| 4.0 | 08/08/2024 |\nModalités spécifiques aux points de connexion"
            )
        };
        let mut pages: Vec<(u32, String)> = (1..=4)
            .map(|i| {
                (
                    i,
                    format!("body alpha {i}\n\nmore body {i}\n\nfiller one\n\nfiller two\n\n{}", footer(i)),
                )
            })
            .collect();
        collapse_repeated_furniture_blocks(&mut pages);
        let all = pages.iter().map(|(_, s)| s.as_str()).collect::<Vec<_>>().join("\n");
        assert_eq!(
            all.matches("Enedis-NOI-CF_110E").count(),
            1,
            "repeated footer table must be kept once: {all}"
        );
        assert_eq!(
            all.matches("Modalités spécifiques").count(),
            1,
            "repeated running title must be kept once: {all}"
        );
        for i in 1..=4 {
            assert!(all.contains(&format!("body alpha {i}")), "page {i} body lost: {all}");
        }
        // Dropping the trailing footer must leave a blank-line boundary so the
        // neighbouring pages' bodies do not weld into one paragraph.
        assert!(
            pages[1].1.ends_with("\n\n"),
            "dropping a trailing footer must keep a page boundary: {:?}",
            pages[1].1
        );
    }
