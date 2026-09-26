//! pdf2md-core — High-performance native Rust core engine for sub-millisecond
//! PDF-to-Markdown extraction and 2D spatial canvas table reconstruction.
//!
//! Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
//! SPDX-License-Identifier: BSL-1.1
//! Licensed under the Business Source License 1.1 (BSL-1.1).

use super::*;
use super::regression_tests_common::*;
    
        

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

    #[test]
    fn stale_startxref_and_object_offsets_are_repaired() {
        let bytes = drifted_xref_pdf();
        // The raw bytes are genuinely unparseable without the repair: this is
        // the exact failure that made a whole FNFE FR invoice fail Tier A.
        assert!(lopdf::Document::load_mem(&bytes).is_err());
        let doc = load_pdf_document(&bytes).expect("repair must recover a drifted xref");
        assert_eq!(doc.get_pages().len(), 1);
    }
