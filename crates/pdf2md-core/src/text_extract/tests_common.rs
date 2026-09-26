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


    /// Minimal single-page PDF whose four `Tj` lines live inside one `BT`/`ET`
    /// and are advanced by a vertical `Td` (no `ET`/`T*` between lines), with
    /// each show-string padded by a leading and trailing space — the shape of
    /// `scratch/samples/synth_ticket_compressed.pdf`.
    pub(super) fn td_line_advance_doc() -> Document {
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

    /// Minimal single-page PDF with a base-14 Courier font (600/1000 em advance
    /// for every code, so advances are exactly predictable) and the caller's
    /// content stream.
    pub(super) fn content_doc(content: &[u8]) -> Document {
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

    /// Minimal single-page PDF whose page draws one parent form, and whose
    /// parent form issues `repeats` `/FmLeaf Do`s of a leaf form that shows the
    /// word `mot`. Used to prove the string walker's shared per-page `Do` budget
    /// bounds a form-DAG expansion (mirrors the glyph engine's budget test).
    pub(super) fn repeating_form_doc(repeats: usize) -> Document {
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

    /// A page drawing `n` DISTINCT forms (one word each), the shape that lost
    /// every form past the old 512 `Do`/page cap (qa-int2 `forms_distinct_*`).
    pub(super) fn distinct_forms_doc(n: usize) -> Document {
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
