// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! PDF content-stream glyph extraction, font metric resolution, and 2D span aggregation.

use super::*;


    pub(super) fn cid_widths(map: HashMap<u32, f64>, encoding: Option<CMapCodec>) -> Widths {
        Widths::Cid {
            map,
            default: 1000.0,
            encoding,
        }
    }

    pub(super) fn font_with_name(name: &[u8]) -> (Document, Dictionary) {
        let mut font = Dictionary::new();
        font.set(b"BaseFont", Object::Name(name.to_vec()));
        (Document::new(), font)
    }

    /// Builds the smallest parseable sfnt program carrying one `OS/2` table
    /// whose `usWeightClass` is `weight`.
    pub(super) fn sfnt_with_os2_weight(weight: u16) -> Vec<u8> {
        let mut os2 = vec![0u8; 64];
        os2[4..6].copy_from_slice(&weight.to_be_bytes());
        let mut out = Vec::new();
        out.extend_from_slice(&0x0001_0000u32.to_be_bytes()); // sfnt version
        out.extend_from_slice(&1u16.to_be_bytes()); // numTables
        out.extend_from_slice(&[0u8; 6]); // searchRange/entrySelector/rangeShift
        out.extend_from_slice(b"OS/2");
        out.extend_from_slice(&0u32.to_be_bytes()); // checksum (unused here)
        let offset = 12 + 16;
        out.extend_from_slice(&(offset as u32).to_be_bytes());
        out.extend_from_slice(&(os2.len() as u32).to_be_bytes());
        out.extend_from_slice(&os2);
        out
    }

    pub(super) fn font_with_embedded_weight(weight: u16, stemv: i64) -> (Document, Dictionary) {
        let mut doc = Document::new();
        let file_id = doc.add_object(Object::Stream(lopdf::Stream::new(
            Dictionary::new(),
            sfnt_with_os2_weight(weight),
        )));
        let mut fd = Dictionary::new();
        fd.set(b"Flags", 6);
        fd.set(b"StemV", stemv);
        fd.set(b"FontFile2", Object::Reference(file_id));
        let mut font = Dictionary::new();
        font.set(b"BaseFont", Object::Name(b"CIDFont+F1".to_vec()));
        font.set(b"FontDescriptor", Object::Dictionary(fd));
        (doc, font)
    }

    // ------------------------------------------------------------------
    // B1: Form XObject recursion in the glyph engine
    // ------------------------------------------------------------------

    use lopdf::{dictionary, Stream};

    pub(super) fn rl(v: f64) -> Object {
        Object::Real(v as f32)
    }

    pub(super) fn show_at(x: f64, y: f64, text: &str) -> Vec<Operation> {
        vec![
            Operation::new(
                "Tm",
                vec![rl(1.0), rl(0.0), rl(0.0), rl(1.0), rl(x), rl(y)],
            ),
            Operation::new("Tj", vec![Object::string_literal(text.to_string())]),
        ]
    }

    pub(super) fn form_dict(matrix: Option<[f64; 6]>, bbox: Option<[f64; 4]>) -> Dictionary {
        let mut fd = Dictionary::new();
        fd.set(b"Type", Object::Name(b"XObject".to_vec()));
        fd.set(b"Subtype", Object::Name(b"Form".to_vec()));
        if let Some(m) = matrix {
            fd.set(
                b"Matrix",
                vec![rl(m[0]), rl(m[1]), rl(m[2]), rl(m[3]), rl(m[4]), rl(m[5])],
            );
        }
        if let Some(b) = bbox {
            fd.set(b"BBox", vec![rl(b[0]), rl(b[1]), rl(b[2]), rl(b[3])]);
        }
        fd
    }

    /// One-page document whose `/Resources/XObject` maps each name to a form.
    /// Each form entry is `(name, form_ops, matrix, bbox, own_font_id)`.
    pub(super) fn pdf_with_named_forms(
        page_ops: Vec<Operation>,
        forms: &[(&[u8], Vec<Operation>, Option<[f64; 6]>, Option<[f64; 4]>, Option<ObjectId>)],
    ) -> (Document, ObjectId) {
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let page_font = doc.add_object(dictionary! {
            "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Courier",
        });
        let mut xobjects = Dictionary::new();
        for (name, ops, matrix, bbox, own_font) in forms {
            let mut fd = form_dict(*matrix, *bbox);
            if let Some(fid) = own_font {
                let res = doc.add_object(dictionary! { "Font" => dictionary! { "F1" => *fid } });
                fd.set(b"Resources", Object::Reference(res));
            }
            let sid = doc.add_object(Stream::new(
                fd,
                Content { operations: ops.clone() }.encode().unwrap(),
            ));
            xobjects.set(name.to_vec(), Object::Reference(sid));
        }
        let page_res = doc.add_object(dictionary! {
            "Font" => dictionary! { "F1" => page_font },
            "XObject" => xobjects,
        });
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
        (doc, page_id)
    }

    pub(super) fn walk_page(doc: &Document, page_id: ObjectId) -> (Vec<Span>, GlyphBudget) {
        let chain = crate::text_extract::resource_dicts(doc, page_id);
        let content = crate::text_extract::decode_page_content(doc, page_id).expect("content");
        let mut budget = GlyphBudget::new();
        let mut cache: HashMap<usize, Vec<GlyphFontInfo>> = HashMap::new();
        let mut seen = false;
        let (spans, _) = {
            let walk = walk_glyphs(
                doc,
                &chain,
                &content.operations,
                Mtx::ID,
                &mut Vec::new(),
                0,
                &mut seen,
                &mut budget,
                &mut cache,
            );
            (walk.spans, walk.underline_segs)
        };
        (spans, budget)
    }

    // ------------------------------------------------------------------
    // B2: inherited page geometry and dominantly-vertical pages
    // ------------------------------------------------------------------

    pub(super) fn pdf_page_with(
        page_ops: Vec<Operation>,
        page_rotate: Option<i64>,
        pages_rotate: Option<i64>,
        media: [f64; 4],
    ) -> (Document, ObjectId) {
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let font = doc.add_object(dictionary! {
            "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Courier",
        });
        let res = doc.add_object(dictionary! { "Font" => dictionary! { "F1" => font } });
        let content = doc.add_object(Stream::new(
            Dictionary::new(),
            Content { operations: page_ops }.encode().unwrap(),
        ));
        let mut pd = dictionary! {
            "Type" => "Page", "Parent" => pages_id, "Contents" => content, "Resources" => res,
        };
        if let Some(r) = page_rotate {
            pd.set(b"Rotate", r);
        }
        let page_id = doc.add_object(pd);
        let mut pages = dictionary! {
            "Type" => "Pages", "Kids" => vec![page_id.into()], "Count" => 1,
            "MediaBox" => vec![rl(media[0]), rl(media[1]), rl(media[2]), rl(media[3])],
        };
        if let Some(r) = pages_rotate {
            pages.set(b"Rotate", r);
        }
        doc.objects.insert(pages_id, Object::Dictionary(pages));
        let cat = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        doc.trailer.set("Root", cat);
        (doc, page_id)
    }

    pub(super) fn tm_at(a: f64, b: f64, c: f64, d: f64, e: f64, f: f64) -> Operation {
        Operation::new(
            "Tm",
            vec![rl(a), rl(b), rl(c), rl(d), rl(e), rl(f)],
        )
    }
