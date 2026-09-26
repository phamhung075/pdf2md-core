// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Tagged-PDF structure-tree reader (ISO 32000 §14.7).
//!
//! When a PDF carries a `/StructTreeRoot` (tagged / PDF/UA documents, plus many
//! Word/LibreOffice/Acrobat exports), the document already encodes the reading
//! order and the semantic role of every element (`H1..H6`, `P`, `Table`, `TR`,
//! `TD`, `Figure`, `Header`, `Footer`, `L`, …) together with `/ActualText`
//! overrides. Re-deriving those from glyph geometry (XY-cut + font-size
//! heuristics) is a guess; the structure tree is ground truth.
//!
//! This reader therefore recovers the block list *and* the markdown for a page
//! directly from the structure tree, using the text layer only to fill in what
//! each marked-content item actually says. It is deliberately conservative: a
//! page only takes this path when the tagged structure resolves to real,
//! non-empty marked-content text, and the caller validates coverage against the
//! geometry fast-path so the (already-tuned) geometric engine is never regressed
//! by a broken/decorative structure tree.
//!
//! Role handling follows the standard PDF role set after expanding the
//! `/RoleMap` (which maps producer-specific role names — e.g. EDF's
//! `SIMM_Lieu_Conso_7.2` — to `TD`/`Text`/…).

    use super::*;
    use lopdf::{dictionary, Object, Stream};

    fn refs(ids: &[ObjectId]) -> Object {
        Object::Array(ids.iter().map(|i| Object::Reference(*i)).collect())
    }
    fn ints(values: &[i64]) -> Object {
        Object::Array(values.iter().map(|v| Object::Integer(*v)).collect())
    }

    /// Build a minimal, well-tagged single-page PDF in memory:
    ///   H1 "Document Title", then a P made of two marked-content lines.
    /// The structure tree (H1 + P) is fully consistent with the content stream's
    /// `/MCID` ranges, so coverage should be ~100% and the reader must activate.
    fn build_tagged_doc() -> (Document, ObjectId) {
        let mut doc = Document::with_version("1.7");

        let font_id = doc.new_object_id();
        let content_id = doc.new_object_id();
        let page_id = doc.new_object_id();
        let pages_id = doc.new_object_id();
        let catalog_id = doc.new_object_id();
        let root_id = doc.new_object_id();
        let parenttree_id = doc.new_object_id();
        let sect_id = doc.new_object_id();
        let h1_id = doc.new_object_id();
        let p1_id = doc.new_object_id();

        doc.objects.insert(
            font_id,
            Object::Dictionary(dictionary! {
                "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica",
                "Encoding" => "WinAnsiEncoding",
            }),
        );

        let content_bytes = b"BT\n/F1 18 Tf\n72 750 Td\nBDC /Span << /MCID 0 >>\n(Document Title) Tj\nEMC\nET\n\
BT\n/F1 12 Tf\n72 720 Td\nBDC /Span << /MCID 1 >>\n(First paragraph sentence one and it flows on.) Tj\nEMC\n\
0 -16 Td\nBDC /Span << /MCID 2 >>\n(Second line keeps right on going.) Tj\nEMC\nET\n"
            .to_vec();
        doc.objects.insert(
            content_id,
            Object::Stream(Stream::new(dictionary! {}, content_bytes)),
        );

        let page_dict = dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "MediaBox" => Object::Array(vec![Object::Integer(0), Object::Integer(0), Object::Integer(612), Object::Integer(792)]),
            "StructParents" => Object::Integer(0),
            "Resources" => dictionary! {
                "Font" => dictionary! { "F1" => font_id },
            },
            "Contents" => content_id,
        };
        doc.objects.insert(page_id, Object::Dictionary(page_dict));
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages", "Kids" => refs(&[page_id]), "Count" => 1,
            }),
        );
        doc.objects.insert(
            catalog_id,
            Object::Dictionary(dictionary! { "Type" => "Catalog", "Pages" => pages_id }),
        );

        // Structure tree.
        let sect = dictionary! {
            "Type" => "StructElem", "S" => "Sect",
            "K" => refs(&[h1_id, p1_id]),
        };
        doc.objects.insert(sect_id, Object::Dictionary(sect));
        let h1 = dictionary! {
            "Type" => "StructElem", "S" => "H1", "K" => Object::Integer(0), "Pg" => page_id,
        };
        doc.objects.insert(h1_id, Object::Dictionary(h1));
        let p1 = dictionary! {
            "Type" => "StructElem", "S" => "P",
            "K" => ints(&[1, 2]),
            "Pg" => page_id,
        };
        doc.objects.insert(p1_id, Object::Dictionary(p1));

        let parenttree = dictionary! {
            "Nums" => Object::Array(vec![Object::Integer(0), refs(&[sect_id])]),
        };
        doc.objects.insert(parenttree_id, Object::Dictionary(parenttree));
        doc.objects.insert(
            root_id,
            Object::Dictionary(dictionary! {
                "Type" => "StructTreeRoot",
                "RoleMap" => dictionary! {},
                "ParentTree" => parenttree_id,
                "K" => refs(&[sect_id]),
            }),
        );
        doc.trailer.set(b"Root", Object::Reference(catalog_id));

        (doc, page_id)
    }

    #[test]
    fn tagged_page_reads_heading_and_paragraph_from_structure() {
        let (doc, page_id) = build_tagged_doc();
        assert!(has_struct_tree(&doc), "fixture must be tagged");

        let tagged = extract_tagged_page(&doc, page_id, false).expect("tagged extraction must succeed");
        // Coverage should be high: the structure marks all text on the page.
        assert!(tagged.words >= 10, "expected a full paragraph, got {} words", tagged.words);
        assert!(tagged.blocks.len() >= 2, "expected heading + paragraph blocks, got {}", tagged.blocks.len());

        // Reading order and roles come from the structure tree.
        let kinds: Vec<&str> = tagged.blocks.iter().map(|b| b.kind.as_str()).collect();
        assert_eq!(kinds[0], "heading", "first block must be the H1 heading");
        assert_eq!(kinds[1], "body", "second block must be the paragraph");

        assert!(
            tagged.text.contains("# Document Title"),
            "markdown must emit an H1 for the heading: {}",
            tagged.text
        );
        assert!(
            tagged.text.contains("First paragraph sentence one and it flows on."),
            "markdown must contain the first paragraph line: {}",
            tagged.text
        );
        assert!(
            tagged.text.contains("Second line keeps right on going."),
            "markdown must contain the second paragraph line: {}",
            tagged.text
        );
    }

    #[test]
    fn tagged_page_returns_none_for_untagged_document() {
        let mut doc = Document::with_version("1.7");
        let page = doc.new_object_id();
        let pages = doc.new_object_id();
        doc.objects.insert(
            pages,
            Object::Dictionary(dictionary! {
                "Type" => "Pages", "Kids" => refs(&[page]), "Count" => 1,
            }),
        );
        doc.objects.insert(
            page,
            Object::Dictionary(dictionary! {
                "Type" => "Page", "Parent" => pages,
                "MediaBox" => Object::Array(vec![Object::Integer(0), Object::Integer(0), Object::Integer(612), Object::Integer(792)]),
            }),
        );
        let cat = doc.new_object_id();
        doc.objects.insert(cat, Object::Dictionary(dictionary! { "Type" => "Catalog", "Pages" => pages }));
        doc.trailer.set(b"Root", Object::Reference(cat));
        assert!(!has_struct_tree(&doc));
        assert!(extract_tagged_page(&doc, page, false).is_none(), "untagged page must not route to structure");
    }

    #[test]
    fn tagged_page_with_math_keeps_plain_text_unchanged() {
        // Enabling math synthesis on a tagged page with only plain prose must
        // not alter the output (no false-positive scripts/fractions).
        let (doc, page_id) = build_tagged_doc();
        let tagged = extract_tagged_page(&doc, page_id, true).expect("tagged extraction must succeed");
        assert!(tagged.text.contains("# Document Title"), "{}", tagged.text);
        assert!(
            tagged.text.contains("First paragraph sentence one and it flows on."),
            "{}",
            tagged.text
        );
        assert!(!tagged.text.contains('$'), "plain tagged text must not gain math: {}", tagged.text);
    }

    #[test]
    fn duplicate_parent_tree_entries_do_not_duplicate_text() {
        // Some producers (observed with GnuAccounting invoices) list the same
        // StructElem reference several times in one page's `/ParentTree` array,
        // e.g. `33 0 R 33 0 R 33 0 R`. The tree is malformed but still
        // resolves; before the fix `extract_tagged_page` walked the element
        // once per reference and re-emitted its marked-content text, so a
        // multi-line table cell / letterhead appeared two, three, or nine
        // times in the markdown.
        let (mut doc, page_id) = build_tagged_doc();

        // Duplicate every page element reference three times.
        let root_id = struct_root_id(&doc).expect("struct root");
        let root = doc.get_object(root_id).unwrap().as_dict().unwrap().clone();
        let pt_ref = root.get(b"ParentTree").unwrap().as_reference().unwrap();
        let pt = doc.get_object(pt_ref).unwrap().as_dict().unwrap().clone();
        let nums = pt.get(b"Nums").unwrap().as_array().unwrap().clone();
        let arr = nums[1].as_array().unwrap();
        let mut dup: Vec<Object> = Vec::new();
        for it in arr {
            for _ in 0..3 {
                dup.push(it.clone());
            }
        }
        let mut new_pt = pt.clone();
        new_pt.set(b"Nums", Object::Array(vec![nums[0].clone(), Object::Array(dup)]));
        doc.objects.insert(pt_ref, Object::Dictionary(new_pt));

        let tagged = extract_tagged_page(&doc, page_id, false).expect("tagged extraction must succeed");
        let needle = "First paragraph sentence one and it flows on.";
        assert_eq!(
            tagged.text.matches(needle).count(),
            1,
            "duplicate ParentTree references must not duplicate emitted text: {}",
            tagged.text
        );
        assert_eq!(
            tagged.text.matches("# Document Title").count(),
            1,
            "heading must appear once, not once per duplicate: {}",
            tagged.text
        );
    }

    #[test]
    fn tagged_doc_full_convert_emits_structure_from_tree() {
        let (mut doc, _page) = build_tagged_doc();
        let mut bytes = Vec::new();
        doc.save_to(&mut bytes).unwrap();
        let res = crate::convert_pdf_bytes_to_markdown(&bytes, &crate::ConversionOptions::default())
            .expect("conversion must succeed");
        // The structure tree is authoritative: heading becomes an H1 and the P is
        // a single paragraph, in reading order (the coverage gate passes here
        // because the tree marks all the page's text).
        assert!(
            res.markdown.contains("# Document Title"),
            "tagged H1 must become '# Document Title': {}",
            res.markdown
        );
        assert!(
            res.markdown.contains("First paragraph sentence one and it flows on."),
            "paragraph line 1 must be present: {}",
            res.markdown
        );
        assert!(
            res.markdown.contains("Second line keeps right on going."),
            "paragraph line 2 must be present: {}",
            res.markdown
        );
    }

    #[test]
    fn layout_table_walk_emits_paragraphs_not_table() {
        use std::collections::HashMap as Map;
        // R4/F7: a single-column `Table` used purely for layout
        // (`Table > TR > TD` with each `TD` marking a prose run) must be walked
        // as a container. Before the fix `table_from_node` rendered it as a
        // one-column GFM pipe table, inflating the table count and replacing
        // the paragraphs with a table block.
        let cell = |mcid: usize, text: &str| Node::Elem {
            role: b"TD".to_vec(),
            actual_text: Some(text.to_string()),
            children: vec![Node::Mcid {
                role: b"TD".to_vec(),
                mcid,
                actual_text: Some(text.to_string()),
            }],
        };
        let row = |mcid: usize, text: &str| Node::Elem {
            role: b"TR".to_vec(),
            actual_text: None,
            children: vec![cell(mcid, text)],
        };
        let para_a = "Le présent document décrit les conditions générales et s'applique à compter de sa date de signature.";
        let para_b = "Une seconde phrase de paragraphe ordinaire.";
        let table = Node::Elem {
            role: b"Table".to_vec(),
            actual_text: None,
            children: vec![row(0, para_a), row(1, para_b)],
        };
        let mut blocks = Vec::new();
        walk(&table, &Map::new(), &mut blocks, false);
        assert!(
            blocks.iter().all(|b| b.kind != "table"),
            "layout table must not yield a table block: {:?}",
            blocks
                .iter()
                .map(|b| (b.kind.clone(), b.text.clone()))
                .collect::<Vec<_>>()
        );
        let body: Vec<&str> = blocks
            .iter()
            .filter(|b| b.kind == "body")
            .map(|b| b.text.as_str())
            .collect();
        assert_eq!(body.len(), 2, "both paragraphs must be emitted as body blocks: {body:?}");
        assert!(body.contains(&para_a));
        assert!(body.contains(&para_b));
    }

    #[test]
    fn genuine_multicolumn_table_walk_still_emits_table() {
        use std::collections::HashMap as Map;
        // The layout heuristic must not disarm a real multi-column, short-cell
        // `Table`: it must still render as one GFM table block.
        let cell = |mcid: usize, text: &str| Node::Elem {
            role: b"TD".to_vec(),
            actual_text: Some(text.to_string()),
            children: vec![Node::Mcid {
                role: b"TD".to_vec(),
                mcid,
                actual_text: Some(text.to_string()),
            }],
        };
        let row = |r: usize, a: &str, b: &str| Node::Elem {
            role: b"TR".to_vec(),
            actual_text: None,
            children: vec![cell(r * 2, a), cell(r * 2 + 1, b)],
        };
        let table = Node::Elem {
            role: b"Table".to_vec(),
            actual_text: None,
            children: vec![row(0, "Désignation", "Montant"), row(1, "Prestation", "1 200,00")],
        };
        let mut blocks = Vec::new();
        walk(&table, &Map::new(), &mut blocks, false);
        assert_eq!(
            blocks.iter().filter(|b| b.kind == "table").count(),
            1,
            "genuine 2x2 table must still be emitted as a table: {:?}",
            blocks
                .iter()
                .map(|b| (b.kind.clone(), b.text.clone()))
                .collect::<Vec<_>>()
        );
    }

    // -----------------------------------------------------------------------
    // Untrusted /K and /Kids recursion bounds (cycle_parse / cycle_numtree).
    // -----------------------------------------------------------------------

    /// Single-page skeleton: font + "Hello cyclic world" content + page tree,
    /// with a benign one-element structure tree whose `/K` and `/ParentTree` the
    /// caller rewires into the malicious shape. Returns `(doc, elem_id,
    /// root_id, parenttree_id)`.
    fn cyclic_base() -> (Document, ObjectId, ObjectId, ObjectId) {
        let mut doc = Document::with_version("1.4");
        let font_id = doc.new_object_id();
        let content_id = doc.new_object_id();
        let page_id = doc.new_object_id();
        let pages_id = doc.new_object_id();
        let catalog_id = doc.new_object_id();
        let root_id = doc.new_object_id();
        let elem_id = doc.new_object_id();
        let parenttree_id = doc.new_object_id();

        doc.objects.insert(font_id, Object::Dictionary(dictionary! {
            "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica",
        }));
        doc.objects.insert(content_id, Object::Stream(Stream::new(
            dictionary! {},
            b"BT /F1 12 Tf 72 720 Td (Hello cyclic world) Tj ET\n".to_vec(),
        )));
        doc.objects.insert(page_id, Object::Dictionary(dictionary! {
            "Type" => "Page", "Parent" => pages_id,
            "MediaBox" => Object::Array(vec![
                Object::Integer(0), Object::Integer(0), Object::Integer(612), Object::Integer(792)]),
            "Resources" => dictionary! { "Font" => dictionary! { "F1" => font_id } },
            "Contents" => content_id,
        }));
        doc.objects.insert(pages_id, Object::Dictionary(dictionary! {
            "Type" => "Pages", "Kids" => refs(&[page_id]), "Count" => 1,
        }));
        doc.objects.insert(catalog_id, Object::Dictionary(dictionary! {
            "Type" => "Catalog", "Pages" => pages_id, "StructTreeRoot" => root_id,
        }));
        doc.objects.insert(elem_id, Object::Dictionary(dictionary! {
            "Type" => "StructElem", "S" => "P", "K" => Object::Array(vec![]),
        }));
        doc.objects.insert(parenttree_id, Object::Dictionary(dictionary! {
            "Nums" => Object::Array(vec![Object::Integer(0), refs(&[elem_id])]),
        }));
        doc.objects.insert(root_id, Object::Dictionary(dictionary! {
            "Type" => "StructTreeRoot", "K" => refs(&[elem_id]), "ParentTree" => parenttree_id,
        }));
        doc.trailer.set(b"Root", Object::Reference(catalog_id));
        (doc, elem_id, root_id, parenttree_id)
    }

    /// Convert the in-memory document and require the page text to survive.
    fn assert_cyclic_doc_converts(doc: &mut Document) -> String {
        let mut bytes = Vec::new();
        doc.save_to(&mut bytes).unwrap();
        let res = crate::convert_pdf_bytes_to_markdown(&bytes, &crate::ConversionOptions::default())
            .expect("a cyclic structure tree must not crash conversion");
        assert!(
            res.markdown.contains("Hello cyclic world"),
            "page text must survive a cyclic structure tree: {}",
            res.markdown
        );
        res.markdown
    }

    #[test]
    fn cyclic_struct_tree_k_reference_does_not_crash() {
        let (mut doc, elem_id, _root_id, _pt) = cyclic_base();
        // `/K` points straight back at the StructElem that owns it.
        if let Ok(d) = doc.get_dictionary_mut(elem_id) {
            d.set(b"K", Object::Reference(elem_id));
        }
        assert_cyclic_doc_converts(&mut doc);
    }

    #[test]
    fn cyclic_number_tree_kids_reference_does_not_crash() {
        let (mut doc, _elem_id, root_id, _pt) = cyclic_base();
        let numtree_id = doc.add_object(Object::Dictionary(dictionary! {
            "Kids" => Object::Array(vec![]), "Limits" => ints(&[0, 0]),
        }));
        if let Ok(d) = doc.get_dictionary_mut(numtree_id) {
            d.set(b"Kids", Object::Array(vec![Object::Reference(numtree_id)]));
        }
        if let Ok(d) = doc.get_dictionary_mut(root_id) {
            d.set(b"ParentTree", Object::Reference(numtree_id));
        }
        assert_cyclic_doc_converts(&mut doc);
    }

    #[test]
    fn deep_non_cyclic_struct_tree_chain_is_depth_bounded() {
        let (mut doc, _elem_id, root_id, _pt) = cyclic_base();
        // 4x the /K cap: a long but acyclic chain must stop at the depth cap
        // rather than exhausting the stack.
        let ids: Vec<ObjectId> = (0..MAX_STRUCT_TREE_DEPTH * 4).map(|_| doc.new_object_id()).collect();
        for w in ids.windows(2) {
            doc.objects.insert(w[0], Object::Dictionary(dictionary! {
                "Type" => "StructElem", "S" => "P", "K" => w[1],
            }));
        }
        let last = *ids.last().unwrap();
        doc.objects.insert(last, Object::Dictionary(dictionary! {
            "Type" => "StructElem", "S" => "P", "K" => Object::Array(vec![]),
        }));
        if let Ok(d) = doc.get_dictionary_mut(root_id) {
            d.set(b"K", refs(&[*ids.first().unwrap()]));
        }
        assert_cyclic_doc_converts(&mut doc);
    }
