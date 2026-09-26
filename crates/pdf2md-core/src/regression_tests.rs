//! pdf2md-core — High-performance native Rust core engine for sub-millisecond
//! PDF-to-Markdown extraction and 2D spatial canvas table reconstruction.
//!
//! Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
//! SPDX-License-Identifier: BSL-1.1
//! Licensed under the Business Source License 1.1 (BSL-1.1).

    use super::*;
    use lopdf::dictionary;

    /// Split a GFM pipe-table row `| a | b |` into normalized cell strings.
    fn split_cells(line: &str) -> Vec<String> {
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
    fn strip_emphasis(s: &str) -> String {
        s.replace("**", "")
            .replace("<u>", "")
            .replace("</u>", "")
            .replace("*", "")
    }

    /// Parse every GFM pipe table (with a `|`-separator row) in the markdown
    /// into a grid of cells. Used by the structural assertions to recover the
    /// reconstructed tables instead of doing substring searches.
    fn parse_gfm_tables(md: &str) -> Vec<Vec<Vec<String>>> {
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

    #[test]
    fn test_billet_electronique_itinerary_table_cohesion() {
        let pdf_path = "../../../scratch/tests/fixtures/billet_electronique.pdf";
        if let Ok(bytes) = std::fs::read(pdf_path) {
            let res = convert_pdf_bytes_to_markdown(&bytes, &ConversionOptions::default()).unwrap();
            let raw_md = &res.markdown;
            let md = strip_emphasis(raw_md);

            // STRUCTURAL assertion, not "contains AF7331": the four flight legs
            // must be reconstructed as a single unified GFM table with the
            // documented 9-column bilingual header, and each leg must occupy
            // exactly one data row's Flight column.
            let tables = parse_gfm_tables(&md);
            let itinerary = tables
                .iter()
                .find(|t| t.first().map_or(false, |hdr| hdr.iter().any(|c| c.contains("Vol") && c.contains("Flight"))))
                .expect("itinerary table (9-col bilingual header) must be reconstructed");

            let num_cols = itinerary[0].len();
            assert_eq!(num_cols, 9, "itinerary must be a 9-column table");
            assert_eq!(itinerary.len() - 1, 4, "exactly one row per flight leg");
            for row in itinerary.iter().skip(1) {
                assert_eq!(row.len(), num_cols, "every flight row must have 9 cells");
                // The Flight cell may carry `<br>`-separated metadata; the leg
                // code is its leading token.
                let flight = row[3].split(['<', '\n']).next().unwrap_or("").trim();
                assert!(
                    matches!(flight, "AF7331" | "AF0258" | "AF0253" | "AF7342"),
                    "Flight column must carry a known leg code, got {:?}",
                    row[3]
                );
            }
            let legs: Vec<String> = itinerary
                .iter()
                .skip(1)
                .map(|r| r[3].split(['<', '\n']).next().unwrap_or("").trim().to_string())
                .collect();
            for want in ["AF7331", "AF0258", "AF0253", "AF7342"] {
                assert!(legs.iter().any(|l| l == want), "missing flight leg {}", want);
            }
        }
    }

    #[test]
    fn test_edf_facture_complex_extraction() {
        let pdf_path = "../../../scratch/samples/edf-facture-complex.pdf";
        if let Ok(bytes) = std::fs::read(pdf_path) {
            let res = convert_pdf_bytes_to_markdown(&bytes, &ConversionOptions::default()).unwrap();
            assert_eq!(res.total_pages, 10, "Must have 10 pages");
            let md = strip_emphasis(&res.markdown);

            // 1. Unified address & proper French text without artificial spaces
            assert!(md.contains("Mlle PALMA BRIGITTE"), "Must contain unified 'Mlle PALMA BRIGITTE'");
            assert!(!md.contains("Ml l e PALMA"), "Must NOT contain letter-split 'Ml l e PALMA'");
            assert!(!md.contains("BRI GI TTE"), "Must NOT contain letter-split 'BRI GI TTE'");
            assert!(md.contains("LE GALOIS"), "Must contain unified 'LE GALOIS'");
            assert!(!md.contains("LE GALOI S"), "Must NOT contain letter-split 'LE GALOI S'");
            assert!(md.contains("13014 MARSEILLE"), "Must contain unified '13014 MARSEILLE'");
            assert!(!md.contains("13014 MARSEI LLE"), "Must NOT contain letter-split '13014 MARSEI LLE'");

            // 2. Sidebar contact preservation
            assert!(md.contains("NOUS CONTACTER"), "Must extract 'NOUS CONTACTER' header");
            assert!(md.contains("5 002 674 443"), "Must extract client number");

            // 2b. Regression test for a real bug: this document has zero
            // genuine built-up fractions, but before the is_fraction_bar /
            // detect_stacked_fractions fixes, a decorative rule under "NOUS
            // CONTACTER" (misread as a fraction bar between it and the
            // unrelated "N° client" line below) and a vertical reference-
            // number strip near "Nimes, le 19 mai 2026" (misread as a long
            // chain of stacked fractions, one digit per pseudo-fraction)
            // both produced spurious `\frac{...}{...}` output. The
            // `md.contains("NOUS CONTACTER")` check above alone would not
            // have caught this — that substring survives fine inside
            // `$\frac{NOUS CONTACTER}{...}$` too.
            assert!(
                !res.markdown.contains(r"\frac{"),
                "this document has no genuine built-up fractions; any \\frac{{}} is a false positive:\n{}",
                res.markdown
            );

            // 3. French diacritics & elision preservation
            assert!(md.contains("d'électricité") || md.contains("d’électricité"), "Must preserve elision in 'd'électricité'");
            assert!(md.contains("Médiateur"), "Must preserve French accent in 'Médiateur'");
            assert!(md.contains("Détail de la facture"), "Must preserve French accent in 'Détail de la facture'");

            // 4. Page 9 separated blocks
            assert!(md.contains("MA CONSO"), "Must contain 'MA CONSO'");
            assert!(md.contains("& MOI"), "Must contain '& MOI'");

            // 5. Margin text partitioned and not interleaved into prose
            assert!(!md.contains("Mademoiselle, 7 1 3 1 8"), "Vertical margin must not interleave into letter greeting");

            // 6. STRUCTURAL: the reconstructions must be well-formed GFM tables
            // (a uniform rectangle) — not merely contain the right substrings.
            let tables = parse_gfm_tables(&md);
            assert!(!tables.is_empty(), "facture must reconstruct at least one table");
            for t in &tables {
                let cols = t[0].len();
                assert!(cols >= 2, "a reconstructed table must have >= 2 columns");
                for row in t {
                    assert_eq!(row.len(), cols, "every row of a reconstructed table must share the header column count");
                }
            }
        }
    }

    #[test]
    fn embed_width_frac_maps_page_fraction() {
        use crate::media::MediaKind;
        let page = Some((0.0, 0.0, 612.0, 792.0));
        let make = |x0: f64, x1: f64| media::MediaItem {
            page: 1,
            x0,
            y0: 0.0,
            x1,
            y1: 60.0,
            width: 600,
            height: 600,
            format: "image/jpeg".into(),
            kind: MediaKind::Photo,
            decorative: false,
            repeat: 1,
            data: Vec::new(),
            data_b64: String::new(),
        };

        // 100pt of a 612pt-wide page => the image covers 16.34% of the width.
        let frac = embed_width_frac(&make(0.0, 100.0), page).unwrap();
        assert!((frac - 100.0 / 612.0).abs() < 1e-9, "got {frac}");

        // A placement that is tiny on the page still gets the floor.
        assert_eq!(
            embed_width_frac(&make(0.0, 4.0), page),
            Some(MIN_EMBED_WIDTH_FRAC)
        );

        // Clamp anything that bleeds past the page edge to full width.
        assert_eq!(embed_width_frac(&make(0.0, 800.0), page), Some(1.0));

        // No measurable page box => fall back to natural size.
        assert_eq!(embed_width_frac(&make(0.0, 100.0), None), None);
    }

    #[test]
    fn embedded_images_are_resized_to_page_footprint() {
        let pdf_path = "../../../scratch/tests/fixtures/synth_logo_image.pdf";
        if let Ok(bytes) = std::fs::read(pdf_path) {
            let opts = ConversionOptions {
                media_mode: MediaMode::Embed,
                ..Default::default()
            };
            let res = convert_pdf_bytes_to_markdown(&bytes, &opts).unwrap();
            let md = &res.markdown;

            // The fixture must actually embed a raster image so the assertion is
            // meaningful.
            assert!(
                md.contains("data:image/"),
                "fixture must embed a raster image, got: {md}"
            );

            // Every embedded image is an HTML <img> carrying a percent width equal
            // to its on-page footprint — never a bare markdown image that the
            // reader renders at the image's full pixel width.
            assert!(
                !md.contains("](data:image/"),
                "must not embed as a bare markdown image (renders at full pixel size)"
            );
            assert!(
                md.contains("width=") && md.contains("%\""),
                "embedded image must carry a percent width attribute"
            );
        }
    }

    #[test]
    fn image_insertion_never_splits_a_structural_prefix() {
        // Regression test for a real bug R7 exposed: the figure-insertion pass
        // finds an insertion point by searching for a block's plain text (no
        // "#"/"-"/"1. " prefix) inside the already-rendered markdown. Once
        // headings/lists gained real Markdown prefixes, a match could land
        // mid-line — right after the "# " — splicing the image in between and
        // leaving an orphaned "# " with nothing after it, and the heading's own
        // text stranded, unprefixed, below the image. The fix snaps the
        // insertion point back to the start of the matched line.
        let pdf_path = "../../../scratch/tests/fixtures/billet_electronique.pdf";
        if let Ok(bytes) = std::fs::read(pdf_path) {
            let opts = ConversionOptions {
                media_mode: MediaMode::Embed,
                ..Default::default()
            };
            let res = convert_pdf_bytes_to_markdown(&bytes, &opts).unwrap();
            let md = &res.markdown;
            for line in md.lines() {
                let trimmed = line.trim_end();
                let after_hashes = trimmed.trim_start_matches('#');
                if after_hashes.len() == trimmed.len() {
                    continue; // doesn't start with '#' — not a heading line
                }
                // A line starting with one or more '#' must be a real heading
                // marker (space then non-empty text), never a bare "#"/"##"
                // run with nothing — or only whitespace — after it.
                assert!(
                    after_hashes.starts_with(' ') && !after_hashes.trim().is_empty(),
                    "heading marker with no text after it: {trimmed:?}\nfull markdown:\n{md}"
                );
            }
        }
    }

    #[test]
    fn media_budget_replaces_data_uri_with_placeholder_once_exhausted() {
        let pdf_path = "../../../scratch/tests/fixtures/synth_logo_image.pdf";
        if let Ok(bytes) = std::fs::read(pdf_path) {
            let opts = ConversionOptions {
                max_media_bytes_per_doc: 1, // any real image blows this instantly
                media_mode: MediaMode::Embed,
                ..Default::default()
            };
            let res = convert_pdf_bytes_to_markdown(&bytes, &opts).unwrap();
            let md = &res.markdown;

            assert!(
                !md.contains("data:image/"),
                "over-budget image must not be inlined as a data URI, got: {md}"
            );
            assert!(
                md.contains("omitted") && md.contains("per-document image budget"),
                "over-budget image must leave a placeholder, got: {md}"
            );
            // The JSON media side-channel is a separate opt-in payload and must
            // still carry the full image — only the inline markdown embed is
            // budget-capped.
            assert!(
                res.media.iter().any(|m| !m.data_b64.is_empty()),
                "media list must still report the full image out-of-band"
            );
        }
    }

    /// The media policy defaults to `None` (no extraction, no inline data
    /// URIs); `Embed` remains a working explicit opt-in.
    #[test]
    fn media_mode_defaults_to_none_and_embed_remains_opt_in() {
        let pdf_path = "../../../scratch/tests/fixtures/synth_logo_image.pdf";
        if let Ok(bytes) = std::fs::read(pdf_path) {
            let default_res =
                convert_pdf_bytes_to_markdown(&bytes, &ConversionOptions::default()).unwrap();
            assert!(
                !default_res.markdown.contains("data:image/"),
                "the default media policy must not inline an image: {}",
                default_res.markdown
            );
            assert!(
                default_res.media.is_empty(),
                "the default media policy must not extract media"
            );

            let opts = ConversionOptions {
                media_mode: MediaMode::Embed,
                ..Default::default()
            };
            let res = convert_pdf_bytes_to_markdown(&bytes, &opts).unwrap();
            assert!(
                res.markdown.contains("data:image/"),
                "an explicit `Embed` media policy must still inline a small fixture image"
            );
        }
    }

    /// Build a one-page PDF with a single uncompressed `DeviceRGB` image
    /// XObject of deterministic pseudo-random noise. Noise makes the
    /// reconstructed PNG effectively incompressible (its size scales with
    /// pixel area), which is exactly the shape the adaptive embed-budget step
    /// has to handle. The image is painted at 250x200 pt on a 400x400 page so
    /// `classify_geometry` sees a non-decorative chart, not a full-page
    /// background.
    fn synthetic_noise_image_pdf(width: u32, height: u32) -> Vec<u8> {
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
    fn first_inline_b64_len(markdown: &str) -> Option<usize> {
        let start = markdown.find("base64,")? + "base64,".len();
        let end = markdown[start..]
            .find(|c| c == '"' || c == ')')
            .map(|i| start + i)
            .unwrap_or(markdown.len());
        Some(end - start)
    }

    #[test]
    fn media_budget_downscales_an_oversized_image_to_fit() {
        let bytes = synthetic_noise_image_pdf(1000, 1000);
        // The full-size PNG is several MB of base64; 1.5 MB can only be met
        // after the adaptive downscale, and sits well above the ~400 px floor
        // so the figure must be embedded rather than omitted.
        let opts = ConversionOptions {
            max_media_bytes_per_doc: 1_500_000,
            media_mode: MediaMode::Embed,
            ..Default::default()
        };
        let res = convert_pdf_bytes_to_markdown(&bytes, &opts).unwrap();
        let md = &res.markdown;
        assert!(
            md.contains("data:image/"),
            "an oversized image must be adaptively downscaled and embedded, got: {md}"
        );
        // The default (non-`vision`) build has no JPEG codec, so its only
        // shrink path is a PNG downscale.
        #[cfg(not(feature = "vision"))]
        assert!(
            md.contains("data:image/png"),
            "the default build must re-encode the shrunk copy as PNG, got: {md}"
        );
        assert!(
            !md.contains("omitted"),
            "a downscaled-to-fit image must not fall back to the placeholder"
        );
        // The JSON side-channel keeps the full-fidelity bytes; only the inline
        // markdown copy is shrunk.
        let full = res
            .media
            .iter()
            .find(|m| !m.data_b64.is_empty())
            .expect("media side-channel must still carry the image");
        let inline = first_inline_b64_len(md).expect("inline data URI");
        assert!(
            inline <= opts.max_media_bytes_per_doc,
            "inlined payload ({inline}) must respect the budget"
        );
        assert!(
            inline < full.data_b64.len(),
            "inline copy ({inline}) must be the shrunk one while the side-channel keeps the full {} bytes",
            full.data_b64.len()
        );
    }

    #[test]
    fn media_budget_omits_when_the_downscale_floor_still_exceeds_budget() {
        let bytes = synthetic_noise_image_pdf(1000, 1000);
        // Far below what even the ~400 px floor can fit, so the omission
        // placeholder remains the true last resort.
        let opts = ConversionOptions {
            max_media_bytes_per_doc: 32 * 1024,
            media_mode: MediaMode::Embed,
            ..Default::default()
        };
        let res = convert_pdf_bytes_to_markdown(&bytes, &opts).unwrap();
        let md = &res.markdown;
        assert!(
            !md.contains("data:image/"),
            "nothing should inline at a 32 KB budget, got: {md}"
        );
        assert!(
            md.contains("omitted") && md.contains("per-document image budget"),
            "a still-over-budget-at-the-floor image must leave the placeholder, got: {md}"
        );
    }

    #[cfg(feature = "vision")]
    #[test]
    fn media_budget_vision_jpeg_reencode_fits_where_png_downscale_cannot() {
        let bytes = synthetic_noise_image_pdf(1000, 1000);
        // Calibrated so the PNG downscale floor (~600 KB of base64 for this
        // noise image) still exceeds the budget while a JPEG re-encode at the
        // quality floor fits. Without the vision JPEG path this case would be
        // omitted.
        let budget = 300 * 1024;
        let opts = ConversionOptions {
            max_media_bytes_per_doc: budget,
            media_mode: MediaMode::Embed,
            ..Default::default()
        };
        let res = convert_pdf_bytes_to_markdown(&bytes, &opts).unwrap();
        let md = &res.markdown;
        assert!(
            md.contains("data:image/jpeg"),
            "vision build must JPEG-recompress to fit, got: {md}"
        );
        assert!(
            !md.contains("omitted"),
            "JPEG re-encode should avoid the placeholder, got: {md}"
        );
        let inline = first_inline_b64_len(md).expect("inline data URI");
        assert!(
            inline <= budget,
            "inlined JPEG ({inline}) must respect the budget"
        );
        // Side-channel still advertises the original PNG, untouched.
        let full = res
            .media
            .iter()
            .find(|m| !m.data_b64.is_empty())
            .expect("media side-channel must still carry the image");
        assert_eq!(full.format, "image/png");
        assert!(
            inline < full.data_b64.len(),
            "inlined JPEG ({inline}) must be smaller than the full PNG ({})",
            full.data_b64.len()
        );
    }

    #[test]
    fn pages_below_word_floor_is_low_for_a_text_rich_document() {
        let pdf_path = "../../../scratch/samples/edf-facture-complex.pdf";
        if let Ok(bytes) = std::fs::read(pdf_path) {
            let res = convert_pdf_bytes_to_markdown(&bytes, &ConversionOptions::default()).unwrap();
            // A real 10-page invoice legitimately has a couple of sparse pages
            // (a mostly-blank separator, a footer-only page) below the 5-word
            // floor without the document being "mostly scanned" — the gateway's
            // escalation decision cares about the *ratio* (well under its ~30%
            // threshold here), not a strict zero.
            assert!(
                res.pages_below_word_floor <= 2,
                "expected at most 2 of 10 pages below the word floor, got {} (failures would indicate the \
                 per-page counter is over-firing, not that the document changed)",
                res.pages_below_word_floor
            );
        }
    }

    #[test]
    fn pages_below_word_floor_counts_pages_under_a_custom_threshold() {
        // This fixture's single page carries exactly 5 words of real text next
        // to an embedded logo image — below the default floor (5) it passes,
        // but a caller asking for a stricter per-page floor must see it counted.
        let pdf_path = "../../../scratch/tests/fixtures/synth_logo_image.pdf";
        if let Ok(bytes) = std::fs::read(pdf_path) {
            let opts = ConversionOptions {
                min_words_per_page: 10,
                ..Default::default()
            };
            let res = convert_pdf_bytes_to_markdown(&bytes, &opts).unwrap();
            assert_eq!(res.total_pages, 1);
            assert_eq!(
                res.pages_below_word_floor, 1,
                "the single page must be counted below a raised 10-word floor"
            );
        }
    }

    /// Builds a minimal PDF whose classic xref table points every object two
    /// bytes late and whose `startxref` is likewise stale — the drift the real
    /// FNFE/Factur-X French invoice fixtures ship. The object bodies are valid.
    fn drifted_xref_pdf() -> Vec<u8> {
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

    #[test]
    fn stale_startxref_and_object_offsets_are_repaired() {
        let bytes = drifted_xref_pdf();
        // The raw bytes are genuinely unparseable without the repair: this is
        // the exact failure that made a whole FNFE FR invoice fail Tier A.
        assert!(lopdf::Document::load_mem(&bytes).is_err());
        let doc = load_pdf_document(&bytes).expect("repair must recover a drifted xref");
        assert_eq!(doc.get_pages().len(), 1);
    }

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

    /// Minimal Helvetica font dictionary shared by the synthetic builders.
    fn helvetica_font(doc: &mut lopdf::Document) -> lopdf::ObjectId {
        let mut font = lopdf::Dictionary::new();
        font.set(b"Type", lopdf::Object::Name(b"Font".to_vec()));
        font.set(b"Subtype", lopdf::Object::Name(b"Type1".to_vec()));
        font.set(b"BaseFont", lopdf::Object::Name(b"Helvetica".to_vec()));
        font.set(b"Encoding", lopdf::Object::Name(b"WinAnsiEncoding".to_vec()));
        doc.add_object(lopdf::Object::Dictionary(font))
    }

    fn finish_catalog(
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
    fn pdf_string(s: &str) -> String {
        s.replace('\\', "\\\\")
            .replace('(', "\\(")
            .replace(')', "\\)")
    }

    /// Build a multi-page PDF from raw content streams (Helvetica/WinAnsi), so
    /// a test controls the exact `Tm`/`Td`/`Tj`/`'` operators. All content is
    /// synthetic placeholder text.
    fn synth_pages_pdf(pages: &[String]) -> Vec<u8> {
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
    fn tm_text(x: f64, y: f64, s: &str) -> String {
        format!(
            "BT /F1 11 Tf 1 0 0 1 {x} {y} Tm ({}) Tj ET",
            pdf_string(s)
        )
    }

    /// A `Tj`+`Td` content stream; `horizontal` picks fragment placement vs the
    /// vertical-only line advances of ordinary prose.
    fn td_text(lines: &[&str], horizontal: bool) -> String {
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
    fn quote_text(lines: &[&str]) -> String {
        let mut parts = vec!["BT /F1 12 Tf 1 0 0 1 72 800 Tm".to_string()];
        for ln in lines {
            parts.push(format!("({}) '", pdf_string(ln)));
        }
        parts.push("ET".to_string());
        parts.join(" ")
    }

    fn count_occurrences(haystack: &str, needle: &str) -> usize {
        haystack.matches(needle).count()
    }

    /// One page whose `/Resources` live inline on the `/Pages` parent, not on
    /// the page object (the QZP payslip shape). lopdf 0.44's
    /// `get_page_fonts` only collects inherited resources that are *indirect*
    /// references, so it returns zero fonts here; our own resolver must still
    /// decode the text instead of dropping every `Tj`.
    fn inherited_inline_resources_pdf() -> Vec<u8> {
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

    /// One page whose content is only `/Fm0 Do`, with the text and fonts inside
    /// the Form XObject — the Bouygues/payslip shape that was classified as
    /// scanned because neither detection nor extraction looked inside the form.
    fn form_xobject_text_pdf() -> Vec<u8> {
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

    /// One page that draws a Form XObject which draws *itself* and then shows
    /// text. The self-reference must not multiply the text or loop.
    fn self_referential_form_pdf() -> Vec<u8> {
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
        use std::io::Write as _;

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

    /// A one-page PDF whose content stream inflates past the page-content cap
    /// and whose resources carry a font, so the digital probe short-circuits on
    /// the font and the conversion path itself has to reject the stream.
    fn content_stream_bomb_pdf() -> Vec<u8> {
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
        use std::io::Write as _;

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

    /// Regression for the `enedis_facture_hta` integration bug: a page whose
    /// retained *last* block is a lone short heading (`# 8`) while the repeated
    /// letterhead before it (in the footer band) is dropped must keep its
    /// trailing blank-line page separator. Without it the next page's first
    /// block — a table row — is welded on with zero characters (`# 8|   | …`),
    /// which is not a valid GFM table start.
    #[test]
    fn collapse_repeated_furniture_blocks_keeps_page_boundary_before_next_page_table() {
        let letterhead = "Enedis, SA a directoire et a conseil de surveillance\n\
                          Tour Enedis 92079 Paris La Defense Cedex - RCS de NANTERRE 444608442";
        let table =
            "|   | Page « Détails des éléments |\n| --- | --- |\n|   | facturés hors taxes » |";
        // Each page chunk ends with `\n\n` — the separator page assembly always
        // appends (see the `chunk.push_str("\n\n")` at page build time).
        let mut pages: Vec<(u32, String)> = vec![
            (
                1,
                format!("{letterhead}\n\nintro one\n\nbody one\n\nbody two\n\nbody three\n\n# 1\n\n"),
            ),
            // Page 2 ends with the retained lone heading, immediately after the
            // dropped furniture: exactly the enedis shape.
            (
                2,
                format!("header two\n\nintro two\n\nbody two a\n\nbody two b\n\n{letterhead}\n\n# 8\n\n"),
            ),
            // Page 3 begins with the table block (no blank line before it).
            (
                3,
                format!("header three\n\nintro three\n\nbody three a\n\nbody three b\n\n{letterhead}\n\n{table}\n\n"),
            ),
            (
                4,
                format!("header four\n\nintro four\n\nbody four a\n\nbody four b\n\n{letterhead}\n\n# 10\n\n"),
            ),
        ];

        collapse_repeated_furniture_blocks(&mut pages);

        let all = pages
            .iter()
            .map(|(_, s)| s.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        // (a) Letterhead dedup still holds: first occurrence kept, later dropped.
        assert_eq!(
            all.matches("Enedis, SA a directoire").count(),
            1,
            "letterhead dedup lost: {all}"
        );
        // (b) The page ending in the lone heading keeps a newline boundary.
        assert!(
            pages[1].1.ends_with('\n'),
            "rebuilt page must keep a boundary: {:?}",
            pages[1].1
        );
        // (c) Production concatenates pages with `push_str` and no separator:
        // the heading must never be glued to the following table row.
        let mut out = String::new();
        for (_, s) in &pages {
            out.push_str(s);
        }
        assert!(
            !out.contains("# 8|"),
            "heading welded to following table row: {out}"
        );
        assert!(
            out.contains("# 8\n"),
            "heading must be separated from the table by a newline: {out}"
        );
    }

    /// A near-identical footer whose only varying line is a real data value is
    /// not furniture and must survive on every page.
    #[test]
    fn collapse_repeated_furniture_blocks_keeps_distinct_data_blocks() {
        let footer = |amount: &str| {
            format!("Releve de compte\n| Total | {amount} |\n| --- | --- |\n| Reglement | 08/08/2024 |")
        };
        let mut pages: Vec<(u32, String)> = (1..=4)
            .map(|i| {
                (
                    i,
                    format!(
                        "lead {i}\n\na\n\nb\n\nc\n\n{}",
                        footer(&format!("1 234,5{i}"))
                    ),
                )
            })
            .collect();
        collapse_repeated_furniture_blocks(&mut pages);
        let all = pages.iter().map(|(_, s)| s.as_str()).collect::<Vec<_>>().join("\n");
        for i in 1..=4 {
            assert!(
                all.contains(&format!("1 234,5{i}")),
                "per-page amount {i} must survive: {all}"
            );
        }
    }

    /// A repeated block that sits in the middle of a page (outside the
    /// header/footer bands) is body content and must not be collapsed.
    #[test]
    fn collapse_repeated_furniture_blocks_keeps_mid_page_repeats() {
        let mid = "repeated middle paragraph";
        let mut pages: Vec<(u32, String)> = (1..=4)
            .map(|i| {
                (
                    i,
                    format!("first {i}\n\nsecond {i}\n\n{mid}\n\nfourth {i}\n\nfifth {i}"),
                )
            })
            .collect();
        collapse_repeated_furniture_blocks(&mut pages);
        let all = pages.iter().map(|(_, s)| s.as_str()).collect::<Vec<_>>().join("\n");
        assert_eq!(
            all.matches(mid).count(),
            4,
            "mid-page repeated block must survive on every page: {all}"
        );
    }

    /// A wholly-repeated sparse page (<= 4 blocks) is content, not furniture.
    #[test]
    fn collapse_repeated_furniture_blocks_keeps_sparse_repeated_pages() {
        let body = "TICKET DE CAISSE\nArticle un 5,00\nArticle deux 7,50\nTOTAL 12,50";
        let mut pages: Vec<(u32, String)> = (1..=3).map(|i| (i, body.to_string())).collect();
        collapse_repeated_furniture_blocks(&mut pages);
        let all = pages.iter().map(|(_, s)| s.as_str()).collect::<Vec<_>>().join("\n");
        assert_eq!(
            all.matches("TICKET DE CAISSE").count(),
            3,
            "sparse repeated page must survive: {all}"
        );
    }

    // ---- Synthetic end-to-end cases for the layout batch (QA list) ----

    fn convert_synth(pages: &[String]) -> String {
        let bytes = synth_pages_pdf(pages);
        convert_pdf_bytes_to_markdown(&bytes, &ConversionOptions::default())
            .expect("synthetic pdf must convert")
            .markdown
    }

    /// A ratio inside a table row and as a standalone footer-style line must
    /// both survive: it is not a corroborated page counter.
    #[test]
    fn synthetic_ratio_cell_and_standalone_survive() {
        let pages: Vec<String> = (1..=4)
            .map(|i| {
                [
                    tm_text(72.0, 815.0, &format!("RATIO-SHEET-{i}")),
                    tm_text(72.0, 720.0, "Ratio"),
                    tm_text(72.0, 705.0, "3/4"),
                    tm_text(72.0, 300.0, &format!("unique prose line number {i} for this sheet only")),
                    tm_text(72.0, 90.0, "3/4"),
                ]
                .join("\n")
            })
            .collect();
        let md = convert_synth(&pages);
        assert_eq!(count_occurrences(&md, "3/4"), 8, "ratio cells/footers lost:\n{md}");
        for i in 1..=4 {
            assert!(md.contains(&format!("RATIO-SHEET-{i}")), "page {i} header lost:\n{md}");
        }
    }

    /// `Sous-total page N: <amount>` data rows must survive: the label carries
    /// a varying amount, not a plain counter.
    #[test]
    fn synthetic_subtotal_rows_survive() {
        let amounts = ["120,50", "98,00", "75,25", "61,10"];
        let pages: Vec<String> = (0..4)
            .map(|i| {
                [
                    tm_text(72.0, 815.0, &format!("RELEVE-{} EN-TETE COURANT", i + 1)),
                    tm_text(72.0, 500.0, &format!("ligne de donnees propre a la page {}", i + 1)),
                    tm_text(72.0, 95.0, &format!("Sous-total page {}: {}", i + 1, amounts[i])),
                ]
                .join("\n")
            })
            .collect();
        let md = convert_synth(&pages);
        for a in amounts {
            assert!(md.contains(a), "amount {a} lost:\n{md}");
        }
        assert_eq!(count_occurrences(&md, "Sous-total"), 4, "subtotal rows lost:\n{md}");
    }

    /// An identical amount row repeated in the footer band on every page is a
    /// real data row, not running furniture: it must survive on every page.
    #[test]
    fn synthetic_identical_footer_amount_row_survives() {
        let pages: Vec<String> = (1..=4)
            .map(|i| {
                [
                    tm_text(72.0, 815.0, "RELEVE MENSUEL"),
                    tm_text(72.0, 500.0, &format!("operation unique de la page {i}")),
                    tm_text(72.0, 80.0, "Frais de dossier: 45,00"),
                ]
                .join("\n")
            })
            .collect();
        let md = convert_synth(&pages);
        assert_eq!(
            count_occurrences(&md, "45,00"),
            4,
            "identical footer amount row suppressed as furniture:\n{md}"
        );
        assert_eq!(
            count_occurrences(&md, "Frais de dossier"),
            4,
            "identical footer amount label suppressed as furniture:\n{md}"
        );
    }

    /// A repeated column header inside table rows must survive.
    #[test]
    fn synthetic_repeated_montant_header_survives() {
        let pages: Vec<String> = (1..=3)
            .map(|i| {
                [
                    tm_text(72.0, 780.0, "Montant"),
                    tm_text(300.0, 780.0, "Detail"),
                    tm_text(72.0, 765.0, &format!("poste-{i}A")),
                    tm_text(300.0, 765.0, "100,00"),
                    tm_text(72.0, 750.0, &format!("poste-{i}B")),
                    tm_text(300.0, 750.0, "200,00"),
                ]
                .join("\n")
            })
            .collect();
        let md = convert_synth(&pages);
        assert_eq!(count_occurrences(&md, "Montant"), 3, "repeated header lost:\n{md}");
    }

    /// Distinct postal codes must never be folded into one furniture key.
    #[test]
    fn synthetic_postal_codes_survive() {
        let pages: Vec<String> = (1..=4)
            .map(|i| {
                let city = if i % 2 == 1 { "75001 PARIS" } else { "69001 LYON" };
                [
                    tm_text(72.0, 800.0, &format!("Adresse: {city}")),
                    tm_text(72.0, 780.0, &format!("dossier numero {i}0000001")),
                    tm_text(72.0, 300.0, &format!("texte metier distinct page {i}")),
                ]
                .join("\n")
            })
            .collect();
        let md = convert_synth(&pages);
        assert_eq!(count_occurrences(&md, "75001 PARIS"), 2, "postal code lost:\n{md}");
        assert_eq!(count_occurrences(&md, "69001 LYON"), 2, "postal code lost:\n{md}");
    }

    /// `Page 2/7`, `2 / 7`, `2/7` counters that vary across pages are dropped.
    #[test]
    fn synthetic_page_counters_are_removed() {
        let forms = ["Page 2/7", "2 / 7", "2/7", "Page 5/7"];
        let pages: Vec<String> = (1..=4)
            .map(|i| {
                [
                    tm_text(72.0, 815.0, forms[i - 1]),
                    tm_text(72.0, 300.0, &format!("contenu reel de la page {i} a conserver")),
                    tm_text(72.0, 90.0, forms[i % 4]),
                ]
                .join("\n")
            })
            .collect();
        let md = convert_synth(&pages);
        for f in forms {
            assert!(!md.contains(f), "counter {f} survived:\n{md}");
        }
        assert_eq!(count_occurrences(&md, "contenu reel"), 4, "body lost:\n{md}");
    }

    /// A legal footer whose only varying field is a page number is dropped,
    /// while a body line that merely says "corps page N" survives.
    #[test]
    fn synthetic_page_varying_legal_footer_removed_body_kept() {
        let pages: Vec<String> = (1..=4)
            .map(|i| {
                [
                    tm_text(72.0, 700.0, &format!("corps page {i}")),
                    tm_text(72.0, 80.0, &format!("Societe Exemple SAS - RCS 123 456 789 - page {i}")),
                ]
                .join("\n")
            })
            .collect();
        let md = convert_synth(&pages);
        assert_eq!(count_occurrences(&md, "RCS"), 1, "page-varying footer kept:\n{md}");
        assert_eq!(count_occurrences(&md, "corps page"), 4, "unique body line dropped:\n{md}");
    }

    /// A ticket whose lines repeat identically on every page must survive.
    #[test]
    fn synthetic_repeated_page_ticket_survives() {
        let body = ["TICKET DE CAISSE", "Article un 5,00", "Article deux 7,50", "TOTAL 12,50"];
        let page = td_text(&body, false);
        let pages = vec![page; 3];
        let md = convert_synth(&pages);
        assert_eq!(
            count_occurrences(&md, "TICKET DE CAISSE"),
            3,
            "repeated ticket content emptied:\n{md}"
        );
        for b in body {
            assert_eq!(count_occurrences(&md, b), 3, "line {b} lost:\n{md}");
        }
    }

    /// A horizontally-positioned `Tj`+`Td` prose page must keep every line.
    #[test]
    fn synthetic_td_prose_page_keeps_content() {
        let body = [
            "Le contenu de cette page est dispose",
            "par fragments successifs avec des",
            "deplacements horizontaux puis verticaux",
            "afin de tester le routage vers",
            "le moteur de mise en page du document",
        ];
        let pages = vec![td_text(&body, true); 3];
        let md = convert_synth(&pages);
        for b in body {
            assert_eq!(count_occurrences(&md, b), 3, "prose line {b} lost:\n{md}");
        }
    }

    /// A `'`-only letter (one `Tm`, zero leading) must keep word boundaries
    /// instead of being merged into a single fragment.
    #[test]
    fn synthetic_quote_show_letter_keeps_words() {
        let body = [
            "Objet: votre demande de dossier",
            "Madame, Monsieur,",
            "Nous accusons reception de votre courrier",
            "et vous remercions de votre confiance.",
        ];
        let pages = vec![quote_text(&body); 2];
        let md = convert_synth(&pages);
        assert!(!md.contains("dossierMadame"), "quote fragments merged:\n{md}");
        for b in body {
            assert!(md.contains(b), "letter line lost: {b}\n{md}");
        }
    }

    /// A table header row immediately preceding data rows without a ruler
    /// must be annexed into the markdown table header rather than emitted
    /// as loose prose.
    #[test]
    fn synthetic_preceding_header_row_annexation() {
        let page = [
            tm_text(40.0, 500.0, "Code"),
            tm_text(140.0, 500.0, "Description"),
            tm_text(300.0, 500.0, "Montant"),
            tm_text(40.0, 485.0, "A10"),
            tm_text(140.0, 485.0, "Prestation de service"),
            tm_text(300.0, 485.0, "150.00"),
            tm_text(40.0, 470.0, "B20"),
            tm_text(140.0, 470.0, "Fourniture materiel"),
            tm_text(300.0, 470.0, "230.00"),
            tm_text(40.0, 455.0, "C30"),
            tm_text(140.0, 455.0, "Frais de deplacement"),
            tm_text(300.0, 455.0, "45.00"),
        ]
        .join("\n");
        let md = convert_synth(&[page]);
        assert!(
            md.contains("| Code | Description | Montant |") || md.contains("|Code|Description|Montant|"),
            "header row was not annexed into markdown table; md was:\n{md}"
        );
        assert!(md.contains("| A10 |") || md.contains("|A10|"), "row A10 missing from table:\n{md}");
    }

    /// Multi-word cells across columns must bucket properly without cross-column corruption.
    #[test]
    fn synthetic_multi_word_cell_midpoint_bucketing() {
        let page = [
            tm_text(40.0, 500.0, "Compte principal"),
            tm_text(180.0, 500.0, "Assistance sur site"),
            tm_text(340.0, 500.0, "1 250,00 EUR"),
            tm_text(40.0, 485.0, "Compte secondaire"),
            tm_text(180.0, 485.0, "Formation utilisateurs"),
            tm_text(340.0, 485.0, "840,00 EUR"),
            tm_text(40.0, 470.0, "Compte tertiaire"),
            tm_text(180.0, 470.0, "Support annuel"),
            tm_text(340.0, 470.0, "3 100,00 EUR"),
        ]
        .join("\n");
        let md = convert_synth(&[page]);
        assert!(md.contains("Assistance sur site"), "missing Assistance sur site:\n{md}");
        assert!(md.contains("1 250,00 EUR"), "missing amount 1 250,00 EUR:\n{md}");
        assert!(!md.contains("Compte 1 250,00"), "cell bucket leaked across columns:\n{md}");
    }

    /// Stopword-dense French account labels beside numeric amount columns must survive
    /// as a table and not be dropped as flowing prose.
    #[test]
    fn synthetic_numeric_stopword_bypass_on_invoice_grids() {
        let page = [
            tm_text(40.0, 500.0, "011"),
            tm_text(90.0, 500.0, "Charges a caractere general"),
            tm_text(320.0, 500.0, "35 799.00 EUR"),
            tm_text(40.0, 485.0, "12"),
            tm_text(90.0, 485.0, "Virement de la section de fonctionnement"),
            tm_text(320.0, 485.0, "7 100.00 EUR"),
            tm_text(40.0, 470.0, "20"),
            tm_text(90.0, 470.0, "Dotations fonds divers et reserve"),
            tm_text(320.0, 470.0, "16 024.00 EUR"),
            tm_text(40.0, 455.0, "21"),
            tm_text(90.0, 455.0, "Immobilisations en cours"),
            tm_text(320.0, 455.0, "0.00 EUR"),
        ]
        .join("\n");
        let md = convert_synth(&[page]);
        assert!(
            md.lines().any(|l| l.trim().starts_with('|') && l.contains("Virement de la section")),
            "stopword-dense grid was collapsed into prose:\n{md}"
        );
    }

    /// The layout path's overdraw dedup is intentional: it folds only an exact
    /// overstrike (same text, same baseline) and must never merge two distinct
    /// strings that happen to overlap.
    #[test]
    fn overdraw_dedup_folds_only_identical_spans() {
        use crate::layout::{build_lines, Span};
        let span = |text: &str, x: f64, y: f64| Span {
            text: text.to_string(),
            x,
            y,
            size: 10.0,
            advance: 20.0,
            word_advance: 20.0,
            is_bold: false,
            is_italic: false,
            is_underline: false,
            is_vertical: false,
        };
        let overstrike = build_lines(&[span("Montant", 100.0, 500.0), span("Montant", 100.2, 500.0)]);
        assert_eq!(
            overstrike.iter().map(|l| l.len()).sum::<usize>(),
            1,
            "an identical overstrike must collapse to one span"
        );
        let distinct = build_lines(&[
            span("ALPHA-COST", 100.0, 500.0),
            span("BRAVO-COST", 100.0, 500.4),
        ]);
        let texts: Vec<&str> = distinct.iter().flatten().map(|s| s.text.as_str()).collect();
        assert!(
            texts.contains(&"ALPHA-COST") && texts.contains(&"BRAVO-COST"),
            "distinct overlapping strings must both survive: {texts:?}"
        );
    }

    /// An encrypted PDF the empty user password cannot open leaves an
    /// `/Encrypt` trailer entry; that is an encryption failure, not a scanned
    /// document.
    #[test]
    fn encrypted_document_without_pages_is_reported() {
        let mut doc = lopdf::Document::with_version("1.4");
        doc.trailer
            .set("Encrypt", lopdf::Object::Dictionary(lopdf::Dictionary::new()));
        assert!(encrypted_undecrypted(&doc));
        assert_eq!(ENCRYPTED_PDF_ERROR, "encrypted PDF: password required");
    }

    #[test]
    fn unencrypted_empty_document_is_not_encrypted() {
        let doc = lopdf::Document::with_version("1.4");
        assert!(!encrypted_undecrypted(&doc));
    }

    /// End-to-end: a serialized PDF with an `/Encrypt` entry and no pages must
    /// come back as [`ENCRYPTED_PDF_ERROR`], so the gateway can report it
    /// instead of paying for a vision rescue that cannot read it either.
    #[test]
    fn encrypted_pdf_bytes_are_reported_as_encrypted() {
        let mut doc = lopdf::Document::with_version("1.4");
        let pages_id = doc.new_object_id();
        doc.objects.insert(
            pages_id,
            lopdf::Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => lopdf::Object::Array(vec![]),
                "Count" => 0,
            }),
        );
        let catalog_id = doc.new_object_id();
        doc.objects.insert(
            catalog_id,
            lopdf::Object::Dictionary(dictionary! {
                "Type" => "Catalog", "Pages" => pages_id,
            }),
        );
        doc.trailer.set("Root", catalog_id);
        doc.trailer.set(
            "Encrypt",
            lopdf::Object::Dictionary(dictionary! {
                "Filter" => "Standard", "V" => 1, "R" => 2,
                "O" => lopdf::Object::String(vec![0u8; 32], lopdf::StringFormat::Literal),
                "U" => lopdf::Object::String(vec![0u8; 32], lopdf::StringFormat::Literal),
                "P" => -1,
            }),
        );
        let mut bytes = Vec::new();
        doc.save_to(&mut bytes).expect("serialize encrypted stub");
        match load_pdf_document(&bytes) {
            Err(e) => assert_eq!(e, ENCRYPTED_PDF_ERROR),
            Ok(d) => panic!("expected an encryption error, got {} pages", d.get_pages().len()),
        }
    }

    /// The public probe the CLI uses must agree with the load error for an
    /// encrypted stub, and the stub must not look "digital" (otherwise the CLI
    /// pre-gate would print the scanned-image message before ever checking).
    #[test]
    fn pdf_password_required_probe_matches_the_conversion_error() {
        let mut doc = lopdf::Document::with_version("1.4");
        let pages_id = doc.new_object_id();
        doc.objects.insert(
            pages_id,
            lopdf::Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => lopdf::Object::Array(vec![]),
                "Count" => 0,
            }),
        );
        let catalog_id = doc.new_object_id();
        doc.objects.insert(
            catalog_id,
            lopdf::Object::Dictionary(dictionary! {
                "Type" => "Catalog", "Pages" => pages_id,
            }),
        );
        doc.trailer.set("Root", catalog_id);
        doc.trailer.set(
            "Encrypt",
            lopdf::Object::Dictionary(dictionary! {
                "Filter" => "Standard", "V" => 1, "R" => 2,
                "O" => lopdf::Object::String(vec![0u8; 32], lopdf::StringFormat::Literal),
                "U" => lopdf::Object::String(vec![0u8; 32], lopdf::StringFormat::Literal),
                "P" => -1,
            }),
        );
        let mut bytes = Vec::new();
        doc.save_to(&mut bytes).expect("serialize encrypted stub");

        assert!(pdf_password_required(&bytes));
        assert!(!is_digital_pdf_bytes(&bytes));
        let err = convert_pdf_bytes_to_markdown(&bytes, &ConversionOptions::default())
            .expect_err("encrypted bytes must fail conversion");
        assert_eq!(err, ENCRYPTED_PDF_ERROR);
    }

    /// A plain unencrypted document is never reported as password-protected.
    #[test]
    fn pdf_password_required_probe_is_false_for_a_plain_pdf() {
        let mut doc = lopdf::Document::with_version("1.4");
        let pages_id = doc.new_object_id();
        doc.objects.insert(
            pages_id,
            lopdf::Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => lopdf::Object::Array(vec![]),
                "Count" => 0,
            }),
        );
        let catalog_id = doc.new_object_id();
        doc.objects.insert(
            catalog_id,
            lopdf::Object::Dictionary(dictionary! {
                "Type" => "Catalog", "Pages" => pages_id,
            }),
        );
        doc.trailer.set("Root", catalog_id);
        let mut bytes = Vec::new();
        doc.save_to(&mut bytes).expect("serialize plain stub");
        assert!(!pdf_password_required(&bytes));
    }

    // ---- Opt-in per-page HTML-comment boundary markers ----

    /// Parse the ordered `(page, content)` sections a marker-delimited markdown
    /// document is split into. Panics on malformed marker syntax, so a
    /// regression cannot pass by emitting no markers at all.
    fn parse_page_marker_sections(md: &str) -> Vec<(u32, String)> {
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
    fn unequal_page_marker_pdf() -> Vec<u8> {
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

    fn convert_with_markers(bytes: &[u8], page_markers: bool) -> String {
        let opts = ConversionOptions {
            page_markers,
            ..Default::default()
        };
        convert_pdf_bytes_to_markdown(bytes, &opts)
            .expect("synthetic pdf must convert")
            .markdown
    }

    /// One marker per page including page 1, ordered, and each marker precedes
    /// exactly its own page's content. Off by default.
    #[test]
    fn page_markers_opt_in_emits_one_marker_per_page_before_its_content() {
        let bytes = unequal_page_marker_pdf();

        let off = convert_with_markers(&bytes, false);
        assert!(
            !off.contains("pdf2w:page"),
            "page_markers=false must emit no marker:\n{off}"
        );

        let on = convert_with_markers(&bytes, true);
        let sections = parse_page_marker_sections(&on);
        assert_eq!(
            sections.iter().map(|(p, _)| *p).collect::<Vec<_>>(),
            vec![1, 2, 3],
            "one marker per page, 1-indexed and in order:\n{on}"
        );

        let tokens = [(1u32, "ZEBRAONE"), (2, "ZEBRATWO"), (3, "ZEBRATHREE")];
        for (page, section) in &sections {
            let token = tokens.iter().find(|(p, _)| p == page).unwrap().1;
            assert!(
                section.contains(token),
                "marker for page {page} must precede that page's content:\n{on}"
            );
            for (other, other_token) in tokens {
                if other != *page {
                    assert!(
                        !section.contains(other_token),
                        "page {page} section must not contain page {other}'s token:\n{on}"
                    );
                }
            }
        }
    }

    /// Regression: every page starts with the identical running header, exactly
    /// the cross-page shape the furniture passes collapse. A marker inserted
    /// *before* those passes (inside the per-page build loop) would sit ahead of
    /// the header and could be normalized to a single cross-page key and
    /// silently dropped on every page but the first; the post-pass insertion
    /// point must keep one marker per page.
    #[test]
    fn page_markers_survive_a_repeated_running_header() {
        let header = "ACME RUNNING HEADER CONFIDENTIAL";
        let page = |body: &str| {
            td_text(
                &[
                    header,
                    body,
                    "second body line",
                    "third body line",
                    "fourth body line",
                ],
                false,
            )
        };
        let bytes = synth_pages_pdf(&[
            page("PAGEONE uniquebody"),
            page("PAGETWO uniquebody"),
            page("PAGETHREE uniquebody"),
        ]);
        let on = convert_with_markers(&bytes, true);
        let sections = parse_page_marker_sections(&on);
        assert_eq!(
            sections.iter().map(|(p, _)| *p).collect::<Vec<_>>(),
            vec![1, 2, 3],
            "every page must keep its marker even with a repeated header:\n{on}"
        );
        for (page_no, section) in &sections {
            let token = match page_no {
                1 => "PAGEONE",
                2 => "PAGETWO",
                _ => "PAGETHREE",
            };
            assert!(
                section.contains(token),
                "page {page_no} body lost with a repeated header:\n{on}"
            );
        }
    }

    /// TEST-ONLY independent oracle: the system `pdftotext -f N -l N` binary
    /// (never invoked from library/production code) proves each marker
    /// delimits that PDF page's real text. Skips — rather than fails — when the
    /// poppler binary is absent so the suite stays portable.
    #[test]
    fn page_markers_match_pdftotext_page_oracle() {
        if std::process::Command::new("pdftotext")
            .arg("-v")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_err()
        {
            eprintln!("skipping: system pdftotext not installed");
            return;
        }

        let bytes = unequal_page_marker_pdf();
        let dir =
            std::env::temp_dir().join(format!("pdf2md-page-markers-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let path = dir.join("unequal-pages.pdf");
        std::fs::write(&path, &bytes).expect("write fixture pdf");

        let oracle = |page: u32| -> String {
            let out = std::process::Command::new("pdftotext")
                .args(["-f", &page.to_string(), "-l", &page.to_string()])
                .arg(&path)
                .arg("-")
                .output()
                .expect("run system pdftotext");
            assert!(out.status.success(), "pdftotext failed for page {page}");
            String::from_utf8_lossy(&out.stdout).into_owned()
        };

        let on = convert_with_markers(&bytes, true);
        let sections = parse_page_marker_sections(&on);
        assert_eq!(sections.len(), 3, "expected three marker sections:\n{on}");

        let tokens = [(1u32, "ZEBRAONE"), (2, "ZEBRATWO"), (3, "ZEBRATHREE")];
        for (page, section) in &sections {
            let token = tokens.iter().find(|(p, _)| p == page).unwrap().1;
            let page_text = oracle(*page);
            assert!(
                page_text.contains(token),
                "oracle page {page} must really carry {token}:\n{page_text}"
            );
            assert!(
                section.contains(token),
                "marker {page} must delimit the oracle's page {page} text:\n{on}"
            );
            for (other, other_token) in tokens {
                if other != *page {
                    assert!(
                        !section.contains(other_token),
                        "marker {page} section must not contain page {other}'s oracle text:\n{on}"
                    );
                }
            }
        }

        let _ = std::fs::remove_dir_all(&dir);
    }