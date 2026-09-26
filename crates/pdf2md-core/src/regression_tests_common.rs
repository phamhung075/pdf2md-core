//! pdf2md-core — High-performance native Rust core engine for sub-millisecond
//! PDF-to-Markdown extraction and 2D spatial canvas table reconstruction.
//!
//! Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
//! SPDX-License-Identifier: BSL-1.1
//! Licensed under the Business Source License 1.1 (BSL-1.1).

    



    /// Split a GFM pipe-table row `| a | b |` into normalized cell strings.
    pub(super) fn split_cells(line: &str) -> Vec<String> {
        let inner = line.trim();
        let inner = inner
            .strip_prefix('|')
            .unwrap_or(inner)
            .strip_suffix('|')
            .unwrap_or(inner);
        inner
            .split('|')
            .map(|c| c.trim().replace("\\|", "|").replace("\\\\", "\\"))
            .collect()
    }

    /// Remove the inline emphasis delimiters (`**`, `*`, `<u>`, `</u>`) the
    /// renderer now inserts, so substring assertions stay valid whether or not
    /// a run was detected as bold/italic/underline.
    pub(super) fn strip_emphasis(s: &str) -> String {
        s.replace("**", "")
            .replace("<u>", "")
            .replace("</u>", "")
            .replace("*", "")
    }

    /// Parse every GFM pipe table (with a `|`-separator row) in the markdown
    /// into a grid of cells. Used by the structural assertions to recover the
    /// reconstructed tables instead of doing substring searches.
    pub(super) fn parse_gfm_tables(md: &str) -> Vec<Vec<Vec<String>>> {
        let lines: Vec<&str> = md.lines().collect();
        let mut out = Vec::new();
        let mut i = 0;
        while i < lines.len() {
            let t = lines[i].trim();
            if t.starts_with('|') && t.ends_with('|') {
                let mut rows = Vec::new();
                while i < lines.len() {
                    let lt = lines[i].trim();
                    if !(lt.starts_with('|') && lt.ends_with('|')) {
                        break;
                    }
                    let cells = split_cells(lt);
                    let is_sep = cells.iter().all(|c| {
                        !c.is_empty() && c.chars().all(|ch| ch == '-' || ch == ':' || ch.is_whitespace())
                    });
                    if !is_sep {
                        rows.push(cells);
                    }
                    i += 1;
                }
                if !rows.is_empty() {
                    out.push(rows);
                }
            } else {
                i += 1;
            }
        }
        out
    }

    /// Build a one-page PDF with a single uncompressed `DeviceRGB` image
    /// XObject of deterministic pseudo-random noise. Noise makes the
    /// reconstructed PNG effectively incompressible (its size scales with
    /// pixel area), which is exactly the shape the adaptive embed-budget step
    /// has to handle. The image is painted at 250x200 pt on a 400x400 page so
    /// `classify_geometry` sees a non-decorative chart, not a full-page
    /// background.
    pub(super) fn synthetic_noise_image_pdf(width: u32, height: u32) -> Vec<u8> {
        let mut samples = Vec::with_capacity(width as usize * height as usize * 3);
        let mut state: u32 = 0x1234_5678;
        for _ in 0..(width as usize * height as usize * 3) {
            // xorshift32 — deterministic and dependency-free.
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            samples.push((state & 0xff) as u8);
        }

        let mut doc = lopdf::Document::new();
        let mut img_dict = lopdf::Dictionary::new();
        img_dict.set(b"Type", lopdf::Object::Name(b"XObject".to_vec()));
        img_dict.set(b"Subtype", lopdf::Object::Name(b"Image".to_vec()));
        img_dict.set(b"Width", lopdf::Object::Integer(width as i64));
        img_dict.set(b"Height", lopdf::Object::Integer(height as i64));
        img_dict.set(b"ColorSpace", lopdf::Object::Name(b"DeviceRGB".to_vec()));
        img_dict.set(b"BitsPerComponent", lopdf::Object::Integer(8));
        let img_id = doc.add_object(lopdf::Object::Stream(lopdf::Stream::new(img_dict, samples)));

        // A minimal Helvetica text layer: without it `convert_pdf_bytes_to_markdown`
        // rejects the synthetic page as a scan before the embed loop runs.
        let mut font_dict = lopdf::Dictionary::new();
        font_dict.set(b"Type", lopdf::Object::Name(b"Font".to_vec()));
        font_dict.set(b"Subtype", lopdf::Object::Name(b"Type1".to_vec()));
        font_dict.set(b"BaseFont", lopdf::Object::Name(b"Helvetica".to_vec()));
        font_dict.set(b"Encoding", lopdf::Object::Name(b"WinAnsiEncoding".to_vec()));
        let font_id = doc.add_object(lopdf::Object::Dictionary(font_dict));

        let content = "BT /F1 12 Tf 60 360 Td (Synthetic sample text for the media budget test) Tj ET\n\
                       q 250 0 0 200 75 100 cm /Im0 Do Q\n";
        let content_id = doc.add_object(lopdf::Object::Stream(lopdf::Stream::new(
            lopdf::Dictionary::new(),
            content.as_bytes().to_vec(),
        )));

        let mut page_dict = lopdf::Dictionary::new();
        page_dict.set(b"Type", lopdf::Object::Name(b"Page".to_vec()));
        page_dict.set(
            b"MediaBox",
            lopdf::Object::Array(vec![
                lopdf::Object::Integer(0),
                lopdf::Object::Integer(0),
                lopdf::Object::Integer(400),
                lopdf::Object::Integer(400),
            ]),
        );
        let mut xobj = lopdf::Dictionary::new();
        xobj.set(b"Im0", lopdf::Object::Reference(img_id));
        let mut font_res = lopdf::Dictionary::new();
        font_res.set(b"F1", lopdf::Object::Reference(font_id));
        let mut res_dict = lopdf::Dictionary::new();
        res_dict.set(b"XObject", lopdf::Object::Dictionary(xobj));
        res_dict.set(b"Font", lopdf::Object::Dictionary(font_res));
        page_dict.set(b"Resources", lopdf::Object::Dictionary(res_dict));
        page_dict.set(b"Contents", lopdf::Object::Reference(content_id));
        let page_id = doc.add_object(lopdf::Object::Dictionary(page_dict));

        let mut pages_dict = lopdf::Dictionary::new();
        pages_dict.set(b"Type", lopdf::Object::Name(b"Pages".to_vec()));
        pages_dict.set(
            b"Kids",
            lopdf::Object::Array(vec![lopdf::Object::Reference(page_id)]),
        );
        pages_dict.set(b"Count", lopdf::Object::Integer(1));
        let pages_id = doc.add_object(lopdf::Object::Dictionary(pages_dict));

        let mut catalog_dict = lopdf::Dictionary::new();
        catalog_dict.set(b"Type", lopdf::Object::Name(b"Catalog".to_vec()));
        catalog_dict.set(b"Pages", lopdf::Object::Reference(pages_id));
        let catalog_id = doc.add_object(lopdf::Object::Dictionary(catalog_dict));
        doc.trailer.set(b"Root", lopdf::Object::Reference(catalog_id));

        let mut bytes = Vec::new();
        doc.save_to(&mut bytes).expect("save synthetic image pdf");
        bytes
    }

    /// Extract the base64 payload of the first inlined data URI.
    pub(super) fn first_inline_b64_len(markdown: &str) -> Option<usize> {
        let start = markdown.find("base64,")? + "base64,".len();
        let end = markdown[start..]
            .find(|c| c == '"' || c == ')')
            .map(|i| start + i)
            .unwrap_or(markdown.len());
        Some(end - start)
    }

    /// Builds a minimal PDF whose classic xref table points every object two
    /// bytes late and whose `startxref` is likewise stale — the drift the real
    /// FNFE/Factur-X French invoice fixtures ship. The object bodies are valid.
    pub(super) fn drifted_xref_pdf() -> Vec<u8> {
        let objects: [&[u8]; 4] = [
            b"<< /Type /Catalog /Pages 2 0 R >>",
            b"<< /Type /Pages /Kids [ 3 0 R ] /Count 1 >>",
            b"<< /Type /Page /Parent 2 0 R /MediaBox [ 0 0 200 200 ] /Contents 4 0 R /Resources << >> >>",
            b"<< /Length 0 >>\nstream\n\nendstream",
        ];
        let mut body = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (i, obj) in objects.iter().enumerate() {
            offsets.push(body.len());
            body.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
            body.extend_from_slice(obj);
            body.extend_from_slice(b"\nendobj\n");
        }
        let xref_pos = body.len();
        let mut tail = format!("xref\n0 {}\n", objects.len() + 1).into_bytes();
        tail.extend_from_slice(b"0000000000 65535 f \n");
        for off in &offsets {
            // Deliberately stale: two bytes past the true object header.
            tail.extend_from_slice(format!("{:010} 00000 n \n", off + 2).as_bytes());
        }
        tail.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{}\n%%EOF\n",
                objects.len() + 1,
                xref_pos + 2
            )
            .as_bytes(),
        );
        body.extend_from_slice(&tail);
        body
    }

    /// Minimal Helvetica font dictionary shared by the synthetic builders.
    pub(super) fn helvetica_font(doc: &mut lopdf::Document) -> lopdf::ObjectId {
        let mut font = lopdf::Dictionary::new();
        font.set(b"Type", lopdf::Object::Name(b"Font".to_vec()));
        font.set(b"Subtype", lopdf::Object::Name(b"Type1".to_vec()));
        font.set(b"BaseFont", lopdf::Object::Name(b"Helvetica".to_vec()));
        font.set(b"Encoding", lopdf::Object::Name(b"WinAnsiEncoding".to_vec()));
        doc.add_object(lopdf::Object::Dictionary(font))
    }

    pub(super) fn finish_catalog(
        doc: &mut lopdf::Document,
        pages_id: lopdf::ObjectId,
    ) -> Vec<u8> {
        let mut catalog = lopdf::Dictionary::new();
        catalog.set(b"Type", lopdf::Object::Name(b"Catalog".to_vec()));
        catalog.set(b"Pages", lopdf::Object::Reference(pages_id));
        let catalog_id = doc.add_object(lopdf::Object::Dictionary(catalog));
        doc.trailer.set(b"Root", lopdf::Object::Reference(catalog_id));
        let mut bytes = Vec::new();
        doc.save_to(&mut bytes).expect("save synthetic pdf");
        bytes
    }

    /// Escape a synthetic literal string for a PDF content stream.
    pub(super) fn pdf_string(s: &str) -> String {
        s.replace('\\', "\\\\")
            .replace('(', "\\(")
            .replace(')', "\\)")
    }

    /// Build a multi-page PDF from raw content streams (Helvetica/WinAnsi), so
    /// a test controls the exact `Tm`/`Td`/`Tj`/`'` operators. All content is
    /// synthetic placeholder text.
    pub(super) fn synth_pages_pdf(pages: &[String]) -> Vec<u8> {
        let mut doc = lopdf::Document::new();
        let font_id = helvetica_font(&mut doc);
        let content_ids: Vec<lopdf::ObjectId> = pages
            .iter()
            .map(|c| {
                doc.add_object(lopdf::Object::Stream(lopdf::Stream::new(
                    lopdf::Dictionary::new(),
                    c.as_bytes().to_vec(),
                )))
            })
            .collect();
        let mut pages = lopdf::Dictionary::new();
        pages.set(b"Type", lopdf::Object::Name(b"Pages".to_vec()));
        pages.set(b"Kids", lopdf::Object::Array(Vec::new()));
        pages.set(b"Count", lopdf::Object::Integer(content_ids.len() as i64));
        let pages_id = doc.add_object(lopdf::Object::Dictionary(pages));
        let mut kids = Vec::new();
        for cid in content_ids {
            let mut fonts = lopdf::Dictionary::new();
            fonts.set(b"F1", lopdf::Object::Reference(font_id));
            let mut res = lopdf::Dictionary::new();
            res.set(b"Font", lopdf::Object::Dictionary(fonts));
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
            page.set(b"Contents", lopdf::Object::Reference(cid));
            page.set(b"Resources", lopdf::Object::Dictionary(res));
            let pid = doc.add_object(lopdf::Object::Dictionary(page));
            kids.push(lopdf::Object::Reference(pid));
        }
        doc.get_object_mut(pages_id)
            .unwrap()
            .as_dict_mut()
            .unwrap()
            .set(b"Kids", lopdf::Object::Array(kids));
        finish_catalog(&mut doc, pages_id)
    }

    /// A `Tm`-positioned text op.
    pub(super) fn tm_text(x: f64, y: f64, s: &str) -> String {
        format!(
            "BT /F1 11 Tf 1 0 0 1 {x} {y} Tm ({}) Tj ET",
            pdf_string(s)
        )
    }

    /// A `Tj`+`Td` content stream; `horizontal` picks fragment placement vs the
    /// vertical-only line advances of ordinary prose.
    pub(super) fn td_text(lines: &[&str], horizontal: bool) -> String {
        let mut parts = vec!["BT /F1 10 Tf".to_string()];
        parts.push(if horizontal {
            "300 800 Td".to_string()
        } else {
            "72 800 Td".to_string()
        });
        for (i, ln) in lines.iter().enumerate() {
            parts.push(format!("({}) Tj", pdf_string(ln)));
            if i + 1 != lines.len() {
                parts.push(if horizontal {
                    "-228 -16 Td".to_string()
                } else {
                    "0 -16 Td".to_string()
                });
            }
        }
        parts.push("ET".to_string());
        parts.join(" ")
    }

    /// A `'`-only content stream at one `Tm` (no `Td`, zero leading).
    pub(super) fn quote_text(lines: &[&str]) -> String {
        let mut parts = vec!["BT /F1 12 Tf 1 0 0 1 72 800 Tm".to_string()];
        for ln in lines {
            parts.push(format!("({}) '", pdf_string(ln)));
        }
        parts.push("ET".to_string());
        parts.join(" ")
    }

    pub(super) fn count_occurrences(haystack: &str, needle: &str) -> usize {
        haystack.matches(needle).count()
    }

    /// One page whose `/Resources` live inline on the `/Pages` parent, not on
    /// the page object (the QZP payslip shape). lopdf 0.44's
    /// `get_page_fonts` only collects inherited resources that are *indirect*
    /// references, so it returns zero fonts here; our own resolver must still
    /// decode the text instead of dropping every `Tj`.
    pub(super) fn inherited_inline_resources_pdf() -> Vec<u8> {
        let mut doc = lopdf::Document::new();
        let font_id = helvetica_font(&mut doc);
        let content = b"BT /F1 12 Tf 40 120 Td (Inherited Resource Text) Tj ET".to_vec();
        let content_id = doc.add_object(lopdf::Object::Stream(lopdf::Stream::new(
            lopdf::Dictionary::new(),
            content,
        )));

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
                lopdf::Object::Integer(595),
                lopdf::Object::Integer(842),
            ]),
        );
        page.set(b"Contents", lopdf::Object::Reference(content_id));
        let page_id = doc.add_object(lopdf::Object::Dictionary(page));

        let mut font_res = lopdf::Dictionary::new();
        font_res.set(b"F1", lopdf::Object::Reference(font_id));
        let mut resources = lopdf::Dictionary::new();
        resources.set(b"Font", lopdf::Object::Dictionary(font_res));
        // The Resources belong to the /Pages node only.
        let pages = doc
            .get_object_mut(pages_id)
            .unwrap()
            .as_dict_mut()
            .unwrap();
        pages.set(
            b"Kids",
            lopdf::Object::Array(vec![lopdf::Object::Reference(page_id)]),
        );
        pages.set(b"Resources", lopdf::Object::Dictionary(resources));
        finish_catalog(&mut doc, pages_id)
    }
