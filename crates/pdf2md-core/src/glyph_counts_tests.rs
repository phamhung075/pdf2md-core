// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Tests for the undecodable-glyph signal: a Type0 / Identity-H font with no
//! `/ToUnicode` is dropped by every text path, so its codes must be counted and
//! the partial loss marked. All fixtures are synthetic, built in-test with
//! lopdf — no fixture files, no personal data.

use super::*;
use lopdf::{dictionary, Document, Object, ObjectId, Stream};

/// Type0 / Identity-H font with no `/ToUnicode`. `/W` is present so the counted
/// code width is the existing 2-byte Identity-H code width.
fn synthetic_type0_identity_font(doc: &mut Document) -> ObjectId {
    let desc_id = doc.new_object_id();
    let font_id = doc.new_object_id();
    doc.objects.insert(
        desc_id,
        Object::Dictionary(dictionary! {
            "Type" => "Font",
            "Subtype" => "CIDFontType2",
            "BaseFont" => "SyntheticCID",
            "DW" => 1000,
            "W" => Object::Array(vec![
                Object::Integer(0),
                Object::Array(vec![Object::Integer(500)]),
            ]),
        }),
    );
    doc.objects.insert(
        font_id,
        Object::Dictionary(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type0",
            "BaseFont" => "SyntheticCID",
            "Encoding" => "Identity-H",
            "DescendantFonts" => Object::Array(vec![Object::Reference(desc_id)]),
        }),
    );
    font_id
}

fn helvetica_font(doc: &mut Document) -> ObjectId {
    let id = doc.new_object_id();
    doc.objects.insert(
        id,
        Object::Dictionary(dictionary! {
            "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica",
            "Encoding" => "WinAnsiEncoding",
        }),
    );
    id
}

/// One-page PDF. `with_type0` selects whether `F1` is the undecodable Type0
/// font (with Helvetica as `F2`) or Helvetica alone. `form_content`, when set,
/// adds a `/Fm0` Form XObject whose own resources alias the Type0 font as `F1`.
fn build_pdf(page_content: &[u8], form_content: Option<&[u8]>, with_type0: bool) -> Vec<u8> {
    let mut doc = Document::with_version("1.5");
    let helv = helvetica_font(&mut doc);
    let (f1, f2) = if with_type0 {
        (synthetic_type0_identity_font(&mut doc), Some(helv))
    } else {
        (helv, None)
    };
    let form_id = form_content.map(|c| {
        let id = doc.new_object_id();
        doc.objects.insert(
            id,
            Object::Stream(Stream::new(
                dictionary! {
                    "Type" => "XObject",
                    "Subtype" => "Form",
                    "BBox" => Object::Array(vec![
                        Object::Integer(0), Object::Integer(0),
                        Object::Integer(595), Object::Integer(842),
                    ]),
                    "Resources" => dictionary! { "Font" => dictionary! { "F1" => f1 } },
                },
                c.to_vec(),
            )),
        );
        id
    });

    let content_id = doc.new_object_id();
    doc.objects.insert(
        content_id,
        Object::Stream(Stream::new(dictionary! {}, page_content.to_vec())),
    );

    let mut page_fonts = dictionary! { "F1" => f1 };
    if let Some(f2) = f2 {
        page_fonts.set(b"F2", Object::Reference(f2));
    }
    let mut resources = dictionary! { "Font" => Object::Dictionary(page_fonts) };
    if let Some(form_id) = form_id {
        resources.set(
            b"XObject",
            Object::Dictionary(dictionary! { "Fm0" => Object::Reference(form_id) }),
        );
    }

    let page_id = doc.new_object_id();
    let pages_id = doc.new_object_id();
    let catalog_id = doc.new_object_id();
    doc.objects.insert(
        page_id,
        Object::Dictionary(dictionary! {
            "Type" => "Page", "Parent" => pages_id,
            "MediaBox" => Object::Array(vec![
                Object::Integer(0), Object::Integer(0),
                Object::Integer(595), Object::Integer(842),
            ]),
            "Resources" => Object::Dictionary(resources),
            "Contents" => Object::Reference(content_id),
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
    let mut bytes = Vec::new();
    doc.save_to(&mut bytes).expect("save synthetic pdf");
    bytes
}

fn convert(bytes: &[u8]) -> ConversionResult {
    convert_pdf_bytes_to_markdown(bytes, &ConversionOptions::default()).expect("convert")
}

const GLYPH_MARKER: &str = "<!-- pdf2md: {\"undecodable_glyphs\":";

/// The mixed page: three 2-byte Identity-H codes plus a decodable Helvetica
/// line. The Helvetica text survives; the Type0 codes are counted; the partial
/// loss marker is appended with both totals.
#[test]
fn mixed_page_counts_undecodable_codes_and_appends_the_marker() {
    let content = b"BT /F1 12 Tf 1 0 0 1 50 800 Tm <000100020003> Tj ET\n\
                    BT /F2 12 Tf 1 0 0 1 50 780 Tm (Hello world) Tj ET";
    let res = convert(&build_pdf(content, None, true));

    assert!(
        res.markdown.contains("Hello world"),
        "the decodable Helvetica text must be preserved: {:?}",
        res.markdown
    );
    assert_eq!(res.undecodable_glyphs, 3, "three 2-byte Identity-H codes");
    assert_eq!(
        res.decoded_glyphs,
        "Hello world".len(),
        "decoded codes count the Helvetica bytes"
    );
    assert!(
        res.markdown
            .trim_end()
            .ends_with("<!-- pdf2md: {\"undecodable_glyphs\":3,\"decoded_glyphs\":11} -->"),
        "the partial-loss marker must be the last line: {:?}",
        res.markdown
    );
}

/// Undecodable codes shown inside a Form XObject (the F0549 shape) are counted
/// by the same canonical pass that recurses into `/Do`.
#[test]
fn form_xobject_undecodable_codes_are_counted() {
    let page = b"BT /F2 12 Tf 1 0 0 1 50 800 Tm (Visible) Tj ET\n/Fm0 Do";
    let form = b"BT /F1 12 Tf 1 0 0 1 50 700 Tm <0010001100120013> Tj ET";
    let res = convert(&build_pdf(page, Some(form), true));

    assert!(res.markdown.contains("Visible"), "{:?}", res.markdown);
    assert_eq!(res.undecodable_glyphs, 4, "four 2-byte codes inside the form");
    assert_eq!(res.decoded_glyphs, "Visible".len());
    assert!(
        res.markdown.contains(&format!(
            "{GLYPH_MARKER}4,\"decoded_glyphs\":7}} -->"
        )),
        "the marker must report the form's codes: {:?}",
        res.markdown
    );
}

/// An ordinary PDF: no undecodable glyph, no marker, and the decoded count is
/// the shown Helvetica bytes.
#[test]
fn a_normal_pdf_has_no_marker_and_only_decoded_codes() {
    let content = b"BT /F1 12 Tf 1 0 0 1 50 800 Tm (Bonjour le monde) Tj ET";
    let res = convert(&build_pdf(content, None, false));

    assert_eq!(res.undecodable_glyphs, 0);
    assert_eq!(res.decoded_glyphs, "Bonjour le monde".len());
    assert!(
        !res.markdown.contains(GLYPH_MARKER),
        "an ordinary document must stay byte-identical (no marker): {:?}",
        res.markdown
    );
}

/// A document made only of undecodable codes has `total_words == 0`, so it
/// already takes the glyph-encoded status path; the partial-loss marker must
/// not be appended there.
#[test]
fn all_undecodable_document_takes_the_status_path_without_the_marker() {
    let content = b"BT /F1 12 Tf 1 0 0 1 50 800 Tm <000100020003> Tj ET";
    let res = convert(&build_pdf(content, None, true));

    assert!(res.needs_vision_rescue, "zero words must request rescue");
    assert_eq!(res.undecodable_glyphs, 3);
    assert_eq!(res.decoded_glyphs, 0);
    assert!(
        !res.markdown.contains(GLYPH_MARKER),
        "the partial-loss marker is for the total_words > 0 case: {:?}",
        res.markdown
    );
}

/// The two counters are additive JSON fields on the serde surface (used by the
/// CLI `--json` and the WASM binding).
#[test]
fn counters_serialise_as_additive_json_fields() {
    let content = b"BT /F1 12 Tf 1 0 0 1 50 800 Tm <000100020003> Tj ET\n\
                    BT /F2 12 Tf 1 0 0 1 50 780 Tm (Hello) Tj ET";
    let res = convert(&build_pdf(content, None, true));
    let v = serde_json::to_value(&res).expect("serialise");
    assert_eq!(v["undecodable_glyphs"], serde_json::json!(3));
    assert_eq!(v["decoded_glyphs"], serde_json::json!(5));
}
