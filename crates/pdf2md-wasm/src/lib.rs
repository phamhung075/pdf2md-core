//! pdf2md-wasm — In-browser client-side WebAssembly engine for PDF-to-Markdown extraction.
//!
//! Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
//! SPDX-License-Identifier: BSL-1.1
//! Licensed under the Business Source License 1.1 (BSL-1.1).
//!
//! Compiles to WebAssembly (wasm32-unknown-unknown) via wasm-bindgen for 100% private,
//! $0.00 cloud-cost document conversions directly inside the user's browser.

use wasm_bindgen::prelude::*;
use pdf2md_core::{
    is_digital_pdf_bytes,
    convert_pdf_bytes_to_markdown,
    reconstruct_canvas_tables,
    ConversionOptions,
    TextSpan,
};

#[cfg(feature = "console_error_panic_hook")]
#[wasm_bindgen(start)]
pub fn init_console_panic_hook() {
    console_error_panic_hook::set_once();
}

/// Checks if a PDF file byte array contains a readable digital text layer.
#[wasm_bindgen]
pub fn is_digital_pdf(bytes: &[u8]) -> bool {
    is_digital_pdf_bytes(bytes)
}

/// Converts PDF byte slice directly into Markdown inside the browser.
/// Returns a JavaScript object with { markdown, total_pages, total_words, tables_detected, duration_us }.
#[wasm_bindgen]
pub fn convert_pdf(bytes: &[u8], detect_tables: Option<bool>) -> Result<JsValue, JsValue> {
    let options = ConversionOptions {
        detect_tables: detect_tables.unwrap_or(true),
        ..Default::default()
    };

    match convert_pdf_bytes_to_markdown(bytes, &options) {
        Ok(result) => {
            serde_wasm_bindgen::to_value(&result)
                .map_err(|e| JsValue::from_str(&format!("Serialization error: {}", e)))
        }
        Err(e) => Err(JsValue::from_str(&e)),
    }
}

/// Reconstructs 2D canvas tables from JSON-encoded text spans.
#[wasm_bindgen]
pub fn reconstruct_tables(spans_json: &str) -> Result<String, JsValue> {
    let spans: Vec<TextSpan> = serde_json::from_str(spans_json)
        .map_err(|e| JsValue::from_str(&format!("Invalid text spans JSON: {}", e)))?;

    let tables = reconstruct_canvas_tables(&spans);
    let mut output = String::new();
    for table in tables {
        output.push_str(&table.to_markdown());
        output.push_str("\n\n");
    }

    Ok(output)
}

/// Returns the compiled WASM engine version string.
#[wasm_bindgen]
pub fn version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

// ---------------------------------------------------------------------------
// Native (host-target) tests.
//
// This crate is deliberately thin: it is wasm-bindgen glue whose real work is
// delegated to `pdf2md-core`. `is_digital_pdf`, `reconstruct_tables` and
// `version` have pure-Rust success paths and can therefore be exercised on a
// normal `cargo test` host build. `convert_pdf` cannot: it serialises the
// result through `serde_wasm_bindgen::to_value`, which needs a live JS runtime
// and panics on a non-wasm target — testing it belongs to a browser/wasm
// harness (the web e2e suite), not here. We intentionally add no new test
// framework dependency (no `wasm-bindgen-test`): the crate's Cargo.lock does
// not carry one and this environment builds offline.
// ---------------------------------------------------------------------------
#[cfg(all(test, not(target_arch = "wasm32")))]
mod native_tests {
    use super::*;
    use serde_json::json;

    /// A minimal but valid one-page PDF with a real text layer. Mirrors the
    /// web e2e generator: object ids are fixed up front and bodies emitted in
    /// id order so the xref offsets and `/F1` reference are consistent.
    fn synthetic_pdf() -> Vec<u8> {
        let catalog = 1;
        let pages = 2;
        let page = 3;
        let font = 4;
        let content = 5;
        let stream = "BT /F1 11 Tf 56 800 Td 13 TL\n(invoice total 1500,00 EUR) Tj\nET";
        let bodies: [String; 5] = [
            format!("<< /Type /Catalog /Pages {pages} 0 R >>"),
            format!("<< /Type /Pages /Kids [{page} 0 R] /Count 1 >>"),
            format!("<< /Type /Page /Parent {pages} 0 R /MediaBox [0 0 595 842] /Resources << /Font << /F1 {font} 0 R >> >> /Contents {content} 0 R >>"),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
            format!("<< /Length {} >>\nstream\n{stream}\nendstream", stream.len()),
        ];
        let mut out = String::from("%PDF-1.4\n");
        let mut offsets = [0usize; 6];
        for (i, body) in bodies.iter().enumerate() {
            let id = i + 1;
            offsets[id] = out.len();
            out.push_str(&format!("{id} 0 obj\n{body}\nendobj\n"));
        }
        let xref_start = out.len();
        out.push_str("xref\n0 6\n0000000000 65535 f \n");
        for id in 1..=5 {
            out.push_str(&format!("{:010} 00000 n \n", offsets[id]));
        }
        out.push_str(&format!(
            "trailer\n<< /Size 6 /Root {catalog} 0 R >>\nstartxref\n{xref_start}\n%%EOF\n"
        ));
        out.into_bytes()
    }

    fn span(text: &str, x0: f64, y0: f64) -> serde_json::Value {
        json!({
            "text": text,
            "bbox": { "x0": x0, "y0": y0, "x1": x0 + 40.0, "y1": y0 + 10.0 },
            "font_size": 11.0,
            "is_bold": false,
            "page_number": 1
        })
    }

    #[test]
    fn version_matches_the_crate_package() {
        assert_eq!(version(), env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn is_digital_pdf_delegates_to_core() {
        assert!(is_digital_pdf(&synthetic_pdf()));
        assert!(!is_digital_pdf(b"definitely not a pdf"));
        assert!(!is_digital_pdf(&[]));
    }

    #[test]
    fn reconstruct_tables_builds_a_markdown_grid_from_spans_json() {
        let spans = json!([
            span("A", 0.0, 100.0),
            span("B", 100.0, 100.0),
            span("C", 0.0, 80.0),
            span("D", 100.0, 80.0),
        ]);
        let out = reconstruct_tables(&spans.to_string()).expect("valid spans JSON");
        assert!(out.contains("| A | B |"), "missing header row: {out:?}");
        assert!(out.contains("| C | D |"), "missing data row: {out:?}");
    }

    #[test]
    fn reconstruct_tables_empty_input_is_ok_and_empty() {
        let out = reconstruct_tables("[]").expect("empty span list is valid JSON");
        assert_eq!(out, "");
    }
}
