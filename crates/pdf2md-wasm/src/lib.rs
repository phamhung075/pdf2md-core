//! pdf2md-wasm — In-browser client-side WebAssembly engine for PDF-to-Markdown extraction.
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
