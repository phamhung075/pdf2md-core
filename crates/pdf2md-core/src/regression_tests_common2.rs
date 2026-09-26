//! pdf2md-core — High-performance native Rust core engine for sub-millisecond
//! PDF-to-Markdown extraction and 2D spatial canvas table reconstruction.
//!
//! Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
//! SPDX-License-Identifier: BSL-1.1
//! Licensed under the Business Source License 1.1 (BSL-1.1).

use super::*;
    


use super::regression_tests_common::*;

    /// One page whose content is only `/Fm0 Do`, with the text and fonts inside
    /// the Form XObject — the Bouygues/payslip shape that was classified as
    /// scanned because neither detection nor extraction looked inside the form.
    pub(super) fn form_xobject_text_pdf() -> Vec<u8> {
        let mut doc = lopdf::Document::new();
        let font_id = helvetica_font(&mut doc);

        let mut form_fonts = lopdf::Dictionary::new();
        form_fonts.set(b"F1", lopdf::Object::Reference(font_id));
        let mut form_res = lopdf::Dictionary::new();
        form_res.set(b"Font", lopdf::Object::Dictionary(form_fonts));
        let mut form_dict = lopdf::Dictionary::new();
        form_dict.set(b"Type", lopdf::Object::Name(b"XObject".to_vec()));
        form_dict.set(b"Subtype", lopdf::Object::Name(b"Form".to_vec()));
        form_dict.set(b"Resources", lopdf::Object::Dictionary(form_res));
        form_dict.set(
            b"BBox",
            lopdf::Object::Array(vec![
                lopdf::Object::Integer(0),
                lopdf::Object::Integer(0),
                lopdf::Object::Integer(595),
                lopdf::Object::Integer(842),
            ]),
        );
        let form_content = b"BT /F1 12 Tf 40 120 Td (Form XObject Text) Tj ET".to_vec();
        let form_id = doc.add_object(lopdf::Object::Stream(lopdf::Stream::new(
            form_dict,
            form_content,
        )));

        let page_content = b"q /Fm0 Do Q".to_vec();
        let content_id = doc.add_object(lopdf::Object::Stream(lopdf::Stream::new(
            lopdf::Dictionary::new(),
            page_content,
        )));

        let mut pages = lopdf::Dictionary::new();
        pages.set(b"Type", lopdf::Object::Name(b"Pages".to_vec()));
        pages.set(b"Kids", lopdf::Object::Array(Vec::new()));
        pages.set(b"Count", lopdf::Object::Integer(1));
        let pages_id = doc.add_object(lopdf::Object::Dictionary(pages));

        let mut xobjects = lopdf::Dictionary::new();
        xobjects.set(b"Fm0", lopdf::Object::Reference(form_id));
        let mut page_res = lopdf::Dictionary::new();
        page_res.set(b"XObject", lopdf::Object::Dictionary(xobjects));
        let mut page = lopdf::Dictionary::new();
        page.set(b"Type", lopdf::Object::Name(b"Page".to_vec()));
        page.set(b"Parent", lopdf::Object::Reference(pages_id));
        page.set(
            b"MediaBox",
            lopdf::Object::Array(vec![
                lopdf::Object::Integer(0),
                lopdf::Object::Integer(0),
                lopdf::Object::Integer(595),
                lopdf::Object::Integer(842),
            ]),
        );
        page.set(b"Resources", lopdf::Object::Dictionary(page_res));
        page.set(b"Contents", lopdf::Object::Reference(content_id));
        let page_id = doc.add_object(lopdf::Object::Dictionary(page));
        doc.get_object_mut(pages_id)
            .unwrap()
            .as_dict_mut()
            .unwrap()
            .set(
                b"Kids",
                lopdf::Object::Array(vec![lopdf::Object::Reference(page_id)]),
            );
        finish_catalog(&mut doc, pages_id)
    }

    /// One page that draws a Form XObject which draws *itself* and then shows
    /// text. The self-reference must not multiply the text or loop.
    pub(super) fn self_referential_form_pdf() -> Vec<u8> {
        let mut doc = lopdf::Document::new();
        let font_id = helvetica_font(&mut doc);

        let form_content = b"q /Fm0 Do Q BT /F1 12 Tf 40 120 Td (Self Form Text) Tj ET".to_vec();
        let mut form_dict = lopdf::Dictionary::new();
        form_dict.set(b"Type", lopdf::Object::Name(b"XObject".to_vec()));
        form_dict.set(b"Subtype", lopdf::Object::Name(b"Form".to_vec()));
        form_dict.set(
            b"BBox",
            lopdf::Object::Array(vec![
                lopdf::Object::Integer(0),
                lopdf::Object::Integer(0),
                lopdf::Object::Integer(595),
                lopdf::Object::Integer(842),
            ]),
        );
        let form_id = doc.add_object(lopdf::Object::Stream(lopdf::Stream::new(
            form_dict,
            form_content,
        )));

        // The form's own resources map /Fm0 back to itself plus its font.
        let mut xobjects = lopdf::Dictionary::new();
        xobjects.set(b"Fm0", lopdf::Object::Reference(form_id));
        let mut fonts = lopdf::Dictionary::new();
        fonts.set(b"F1", lopdf::Object::Reference(font_id));
        let mut res = lopdf::Dictionary::new();
        res.set(b"XObject", lopdf::Object::Dictionary(xobjects));
        res.set(b"Font", lopdf::Object::Dictionary(fonts));
        doc.get_object_mut(form_id)
            .unwrap()
            .as_stream_mut()
            .unwrap()
            .dict
            .set(b"Resources", lopdf::Object::Dictionary(res));

        let content_id = doc.add_object(lopdf::Object::Stream(lopdf::Stream::new(
            lopdf::Dictionary::new(),
            b"q /Fm0 Do Q".to_vec(),
        )));
        let mut pages = lopdf::Dictionary::new();
        pages.set(b"Type", lopdf::Object::Name(b"Pages".to_vec()));
        pages.set(b"Kids", lopdf::Object::Array(Vec::new()));
        pages.set(b"Count", lopdf::Object::Integer(1));
        let pages_id = doc.add_object(lopdf::Object::Dictionary(pages));
        let mut page_res = lopdf::Dictionary::new();
        let mut page_xobjects = lopdf::Dictionary::new();
        page_xobjects.set(b"Fm0", lopdf::Object::Reference(form_id));
        page_res.set(b"XObject", lopdf::Object::Dictionary(page_xobjects));
        let mut page = lopdf::Dictionary::new();
        page.set(b"Type", lopdf::Object::Name(b"Page".to_vec()));
        page.set(b"Parent", lopdf::Object::Reference(pages_id));
        page.set(
            b"MediaBox",
            lopdf::Object::Array(vec![
                lopdf::Object::Integer(0),
                lopdf::Object::Integer(0),
                lopdf::Object::Integer(595),
                lopdf::Object::Integer(842),
            ]),
        );
        page.set(b"Resources", lopdf::Object::Dictionary(page_res));
        page.set(b"Contents", lopdf::Object::Reference(content_id));
        let page_id = doc.add_object(lopdf::Object::Dictionary(page));
        doc.get_object_mut(pages_id)
            .unwrap()
            .as_dict_mut()
            .unwrap()
            .set(
                b"Kids",
                lopdf::Object::Array(vec![lopdf::Object::Reference(page_id)]),
            );
        finish_catalog(&mut doc, pages_id)
    }

    /// A one-page PDF whose content stream inflates past the page-content cap
    /// and whose resources carry a font, so the digital probe short-circuits on
    /// the font and the conversion path itself has to reject the stream.
    pub(super) fn content_stream_bomb_pdf() -> Vec<u8> {
        use std::io::Write as _;

        let payload = vec![b' '; text_extract::MAX_PAGE_CONTENT_STREAM + (4 << 20)];
        let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
        enc.write_all(&payload).unwrap();
        let compressed = enc.finish().unwrap();
        assert!(
            compressed.len() < 64 * 1024,
            "the bomb must be small on disk, got {} bytes",
            compressed.len()
        );

        let mut doc = lopdf::Document::new();
        let font_id = helvetica_font(&mut doc);
        let mut stream_dict = lopdf::Dictionary::new();
        stream_dict.set(b"Filter", lopdf::Object::Name(b"FlateDecode".to_vec()));
        let content_id = doc.add_object(lopdf::Object::Stream(lopdf::Stream::new(
            stream_dict,
            compressed,
        )));

        let mut fonts = lopdf::Dictionary::new();
        fonts.set(b"F1", lopdf::Object::Reference(font_id));
        let mut resources = lopdf::Dictionary::new();
        resources.set(b"Font", lopdf::Object::Dictionary(fonts));

        let mut pages = lopdf::Dictionary::new();
        pages.set(b"Type", lopdf::Object::Name(b"Pages".to_vec()));
        pages.set(b"Kids", lopdf::Object::Array(Vec::new()));
        pages.set(b"Count", lopdf::Object::Integer(1));
        let pages_id = doc.add_object(lopdf::Object::Dictionary(pages));

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
        page.set(b"Resources", lopdf::Object::Dictionary(resources));
        page.set(b"Contents", lopdf::Object::Reference(content_id));
        let page_id = doc.add_object(lopdf::Object::Dictionary(page));
        doc.get_object_mut(pages_id)
            .unwrap()
            .as_dict_mut()
            .unwrap()
            .set(
                b"Kids",
                lopdf::Object::Array(vec![lopdf::Object::Reference(page_id)]),
            );
        finish_catalog(&mut doc, pages_id)
    }

    // ---- Synthetic end-to-end cases for the layout batch (QA list) ----

    pub(super) fn convert_synth(pages: &[String]) -> String {
        let bytes = synth_pages_pdf(pages);
        convert_pdf_bytes_to_markdown(&bytes, &ConversionOptions::default())
            .expect("synthetic pdf must convert")
            .markdown
    }

    // ---- Opt-in per-page HTML-comment boundary markers ----

    /// Parse the ordered `(page, content)` sections a marker-delimited markdown
    /// document is split into. Panics on malformed marker syntax, so a
    /// regression cannot pass by emitting no markers at all.
    pub(super) fn parse_page_marker_sections(md: &str) -> Vec<(u32, String)> {
        let needle = "<!-- pdf2w:page n=\"";
        let mut starts: Vec<(usize, u32, usize)> = Vec::new();
        let mut idx = 0;
        while let Some(rel) = md[idx..].find(needle) {
            let start = idx + rel;
            let num_start = start + needle.len();
            let num_end = md[num_start..]
                .find('"')
                .map(|e| num_start + e)
                .expect("page marker number must be quoted");
            let page: u32 = md[num_start..num_end]
                .parse()
                .expect("page marker number must parse");
            let close = md[num_end..]
                .find("-->")
                .map(|e| num_end + e + 3)
                .expect("page marker must be closed");
            starts.push((start, page, close));
            idx = close;
        }
        starts
            .iter()
            .enumerate()
            .map(|(i, &(_, page, close))| {
                let end = starts.get(i + 1).map_or(md.len(), |p| p.0);
                (page, md[close..end].to_string())
            })
            .collect()
    }

    /// Three synthetic pages of *unequal* length, each carrying one unique
    /// token. Equal page lengths can make a mis-placed marker look correct by
    /// accident; unequal lengths expose an off-by-one page boundary.
    pub(super) fn unequal_page_marker_pdf() -> Vec<u8> {
        let page1 = td_text(
            &[
                "ZEBRAONE intro line",
                "ZEBRAONE body alpha",
                "ZEBRAONE body beta",
                "ZEBRAONE body gamma",
            ],
            false,
        );
        let page2 = td_text(&["ZEBRATWO intro line", "ZEBRATWO body alpha"], false);
        let page3 = td_text(
            &[
                "ZEBRATHREE intro line",
                "ZEBRATHREE body alpha",
                "ZEBRATHREE body beta",
                "ZEBRATHREE body gamma",
                "ZEBRATHREE body delta",
                "ZEBRATHREE body epsilon",
            ],
            false,
        );
        synth_pages_pdf(&[page1, page2, page3])
    }

    pub(super) fn convert_with_markers(bytes: &[u8], page_markers: bool) -> String {
        let opts = ConversionOptions {
            page_markers,
            ..Default::default()
        };
        convert_pdf_bytes_to_markdown(bytes, &opts)
            .expect("synthetic pdf must convert")
            .markdown
    }
