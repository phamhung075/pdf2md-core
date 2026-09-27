//! pdf2md-core — High-performance native Rust core engine for sub-millisecond
//! PDF-to-Markdown extraction and 2D spatial canvas table reconstruction.
//!
//! Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
//! SPDX-License-Identifier: BSL-1.1
//! Licensed under the Business Source License 1.1 (BSL-1.1).

use super::*;
use super::regression_tests_common::*;
use super::regression_tests_common2::*;
use super::robustness_repro_fixtures::{
    many_glyph_page_pdf, oom_xref_w_4gib_pdf, GLYPH_ABORT_OPS, GLYPH_AMPLIFICATION_OPS,
};
    use lopdf::dictionary;

/// Upper bound on the Markdown a capped page may produce. Truncation keeps the
/// first `MAX_PAGE_GLYPHS` (64 000) text-show ops, so the output is a few
/// hundred KB while the untruncated streams are 14.4 MB (M1) / 5.4 MB (M2).
const BOUNDED_MARKDOWN_CEILING: usize = 1 << 20;

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

    /// O1: a 542-byte PDF declaring an xref-stream field of 2^32 bytes used to
    /// make lopdf allocate 4 GiB (`vec![0_u8; field_widths[1]]`) and `abort()`
    /// under an address-space limit. The raw-byte pre-validation must reject it
    /// with a clean error, before lopdf allocates. The repro is built in code.
    #[test]
    fn xref_stream_with_4gib_w_width_is_rejected_not_aborted() {
        let bytes = oom_xref_w_4gib_pdf();
        match load_pdf_document(&bytes) {
            Err(err) => assert!(
                err.contains("xref stream rejected") && err.contains("/W"),
                "the rejection must name the /W bound, got: {err}"
            ),
            Ok(_) => panic!("an absurd xref /W width must be rejected, not loaded"),
        }
    }

    /// M1: a content stream that packs one `(word) Tj` per glyph made lopdf
    /// materialise 800 000 `Operation`s (~865 MB per decode, twice over) for a
    /// 35 KB input. The per-page content-operator cap must truncate the raw
    /// bytes before decode and report the truncation with bounded output.
    #[test]
    fn many_glyph_page_is_operator_bounded_and_reports_truncation() {
        let bytes = many_glyph_page_pdf(GLYPH_AMPLIFICATION_OPS);
        let result = convert_pdf_bytes_to_markdown(&bytes, &ConversionOptions::default())
            .expect("an operator-bounded conversion must still succeed");
        assert!(
            result.budget_exhausted,
            "hitting the content-operator budget must set budget_exhausted so truncation is visible"
        );
        assert!(
            result.markdown.len() < BOUNDED_MARKDOWN_CEILING,
            "the capped page must stay bounded, got {} bytes",
            result.markdown.len()
        );
    }

    /// The smaller M2 repro aborted under the 1.6 GB address-space limit the
    /// harness applies; the same content-operator cap must let it finish and
    /// report the truncation with bounded output.
    #[test]
    fn glyph_abort_repro_is_bounded_too() {
        let bytes = many_glyph_page_pdf(GLYPH_ABORT_OPS);
        let result = convert_pdf_bytes_to_markdown(&bytes, &ConversionOptions::default())
            .expect("an operator-bounded conversion must still succeed");
        assert!(result.budget_exhausted);
        assert!(
            result.markdown.len() < BOUNDED_MARKDOWN_CEILING,
            "the capped page must stay bounded, got {} bytes",
            result.markdown.len()
        );
    }

    /// O1 secondary guard: an all-zero `/W` consumes no bytes per xref entry, so
    /// the `/Index` (or `/Size`) count alone drives insertion into lopdf's map.
    /// It must be rejected as degenerate rather than allowed to insert billions
    /// of entries.
    #[test]
    fn xref_zero_width_w_is_rejected() {
        let dict = b"<< /Type /XRef /Size 7 /W [0 0 0] /Index [0 7] >>";
        let err = crate::pdf_load::validate_xref_stream_dicts(dict)
            .expect_err("an all-zero /W must be rejected");
        assert!(err.contains("/W"), "the rejection must name /W, got: {err}");
    }

    /// O1 secondary guard: a single `/Index` pair count above `MAX_XREF_SIZE`
    /// must be rejected before lopdf iterates it.
    #[test]
    fn xref_index_count_over_bound_is_rejected() {
        let dict = b"<< /Type /XRef /Size 7 /W [1 2 1] /Index [0 999999999] >>";
        let err = crate::pdf_load::validate_xref_stream_dicts(dict)
            .expect_err("an over-bound /Index count must be rejected");
        assert!(
            err.contains("/Index"),
            "the rejection must name /Index, got: {err}"
        );
    }

    /// O1: PDF name tokens need no whitespace between them, so a fuzzer
    /// mutation produces `/Type/XRef/W[1 4294967296 1]/Index[0 1]`. The
    /// validator must still see the glued `/W` (the leading `/` starts the name
    /// token) or lopdf allocates 4 GiB and aborts.
    #[test]
    fn xref_stream_with_glued_names_is_still_validated() {
        let dict = b"<</Type/XRef/Size 7/W[1 4294967296 1]/Index[0 1]>>";
        let err = crate::pdf_load::validate_xref_stream_dicts(dict)
            .expect_err("a glued /W must still be validated");
        assert!(err.contains("/W"), "the rejection must name /W, got: {err}");
    }

