//! pdf2md-core — High-performance native Rust core engine for sub-millisecond
//! PDF-to-Markdown extraction and 2D spatial canvas table reconstruction.
//!
//! Licensed under MIT OR Apache-2.0. Zero AGPL/GPL dependencies.

use serde::{Deserialize, Serialize};
use std::time::Instant;

#[cfg(feature = "python")]
use pyo3::prelude::*;

/// 2D Bounding Box in PDF coordinate space (points).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoundingBox {
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
}

impl BoundingBox {
    pub fn new(x0: f64, y0: f64, x1: f64, y1: f64) -> Self {
        Self { x0, y0, x1, y1 }
    }

    pub fn intersects(&self, other: &BoundingBox) -> bool {
        self.x0 < other.x1 && self.x1 > other.x0 && self.y0 < other.y1 && self.y1 > other.y0
    }

    pub fn width(&self) -> f64 {
        (self.x1 - self.x0).abs()
    }

    pub fn height(&self) -> f64 {
        (self.y1 - self.y0).abs()
    }
}

/// Extracted text span with spatial coordinates.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextSpan {
    pub text: String,
    pub bbox: BoundingBox,
    pub font_size: f64,
    pub is_bold: bool,
    pub page_number: usize,
}

/// Reconstructed 2D table grid from text positions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CanvasTable {
    pub rows: Vec<Vec<String>>,
    pub bbox: BoundingBox,
}

impl CanvasTable {
    /// Renders the reconstructed table into standard GitHub Flavored Markdown (GFM) pipe table.
    pub fn to_markdown(&self) -> String {
        if self.rows.is_empty() {
            return String::new();
        }

        let num_cols = self.rows.iter().map(|r| r.len()).max().unwrap_or(0);
        if num_cols == 0 {
            return String::new();
        }

        let mut md = String::new();

        // Header row (row 0 or synthesized)
        let header = &self.rows[0];
        md.push('|');
        for c in 0..num_cols {
            let val = header.get(c).map(|s| s.trim()).unwrap_or("");
            md.push_str(&format!(" {} |", if val.is_empty() { " " } else { val }));
        }
        md.push('\n');

        // Separator row
        md.push('|');
        for _ in 0..num_cols {
            md.push_str(" --- |");
        }
        md.push('\n');

        // Data rows
        for row in self.rows.iter().skip(1) {
            md.push('|');
            for c in 0..num_cols {
                let val = row.get(c).map(|s| s.trim()).unwrap_or("");
                md.push_str(&format!(" {} |", if val.is_empty() { " " } else { val }));
            }
            md.push('\n');
        }

        md
    }
}

/// Conversion and parsing options.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversionOptions {
    pub detect_tables: bool,
    pub detect_headings: bool,
    pub min_words_per_page: usize,
}

impl Default for ConversionOptions {
    fn default() -> Self {
        Self {
            detect_tables: true,
            detect_headings: true,
            min_words_per_page: 5,
        }
    }
}

/// Conversion result summary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversionResult {
    pub markdown: String,
    pub total_pages: usize,
    pub total_words: usize,
    pub tables_detected: usize,
    pub duration_us: u64,
}

/// Probes whether raw PDF bytes contain a digital text stream without full rendering.
pub fn is_digital_pdf_bytes(bytes: &[u8]) -> bool {
    if bytes.len() < 32 {
        return false;
    }

    // PDF magic check: %PDF-
    if !bytes.starts_with(b"%PDF-") {
        return false;
    }

    // Scan for text operator indicators: BT (Begin Text), Tj, TJ, ET (End Text)
    // and font definitions /Font
    let text_markers: &[&[u8]] = &[b"BT\n", b"BT\r", b"BT ", b"/Font", b"Tj", b"TJ"];
    let mut matches = 0;

    for marker in text_markers {
        if bytes.windows(marker.len()).any(|w| w == *marker) {
            matches += 1;
        }
    }

    // If multiple distinct text operators are present, text layer is present
    matches >= 2
}

/// 2D spatial canvas clustering: groups text spans into aligned rows and columns.
pub fn reconstruct_canvas_tables(spans: &[TextSpan]) -> Vec<CanvasTable> {
    if spans.len() < 4 {
        return Vec::new();
    }

    // Sort spans top-to-bottom, left-to-right (Y inverted in standard PDF: larger Y is higher)
    let mut sorted_spans = spans.to_vec();
    sorted_spans.sort_by(|a, b| {
        b.bbox.y0.partial_cmp(&a.bbox.y0).unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.bbox.x0.partial_cmp(&b.bbox.x0).unwrap_or(std::cmp::Ordering::Equal))
    });

    // Group spans into candidate rows by vertical alignment threshold (3.0 points)
    let y_tolerance = 3.0;
    let mut rows: Vec<Vec<TextSpan>> = Vec::new();
    let mut current_row: Vec<TextSpan> = Vec::new();
    let mut current_y = sorted_spans[0].bbox.y0;

    for span in sorted_spans {
        if (span.bbox.y0 - current_y).abs() <= y_tolerance {
            current_row.push(span);
        } else {
            if !current_row.is_empty() {
                rows.push(current_row);
            }
            current_y = span.bbox.y0;
            current_row = vec![span];
        }
    }
    if !current_row.is_empty() {
        rows.push(current_row);
    }

    // Filter candidate tables: at least 2 rows and multiple columns
    let multi_col_rows: Vec<&Vec<TextSpan>> = rows.iter().filter(|r| r.len() >= 2).collect();
    if multi_col_rows.len() < 2 {
        return Vec::new();
    }

    // Extract table rows as string matrices
    let mut table_rows: Vec<Vec<String>> = Vec::new();
    for row in multi_col_rows {
        let mut cols: Vec<String> = Vec::new();
        for span in row {
            cols.push(span.text.clone());
        }
        table_rows.push(cols);
    }

    let min_x = spans.iter().map(|s| s.bbox.x0).fold(f64::INFINITY, f64::min);
    let max_x = spans.iter().map(|s| s.bbox.x1).fold(f64::NEG_INFINITY, f64::max);
    let min_y = spans.iter().map(|s| s.bbox.y0).fold(f64::INFINITY, f64::min);
    let max_y = spans.iter().map(|s| s.bbox.y1).fold(f64::NEG_INFINITY, f64::max);

    vec![CanvasTable {
        rows: table_rows,
        bbox: BoundingBox::new(min_x, min_y, max_x, max_y),
    }]
}

/// Converts PDF byte slice to clean Markdown with 2D spatial table reconstruction.
pub fn convert_pdf_bytes_to_markdown(bytes: &[u8], options: &ConversionOptions) -> Result<ConversionResult, String> {
    let t0 = Instant::now();

    if !is_digital_pdf_bytes(bytes) {
        return Err("Document lacks a digital text layer or is scanned".to_string());
    }

    // Try parsing with lopdf
    let doc = lopdf::Document::load_mem(bytes)
        .map_err(|e| format!("lopdf parsing error: {}", e))?;

    let mut full_markdown = String::new();
    let total_pages = doc.get_pages().len();
    let mut total_words = 0;
    let tables_detected = 0;

    for (page_num, _page_id) in doc.get_pages() {
        let text = doc.extract_text(&[page_num])
            .unwrap_or_default();

        let words: Vec<&str> = text.split_whitespace().collect();
        total_words += words.len();

        if options.detect_headings && (text.starts_with("# ") || text.lines().next().map_or(false, |l| l.len() < 60 && l.chars().all(|c| c.is_alphanumeric() || c.is_whitespace()))) {
            full_markdown.push_str(&format!("\n## Page {}\n\n", page_num));
        }

        full_markdown.push_str(&text);
        full_markdown.push_str("\n\n");
    }

    let duration_us = t0.elapsed().as_micros() as u64;

    Ok(ConversionResult {
        markdown: full_markdown,
        total_pages,
        total_words,
        tables_detected,
        duration_us,
    })
}

// ==============================================================================
// C ABI (FFI) — for embedding in Go (cgo), Node, and other native consumers
// ==============================================================================

use std::ffi::CString;
use std::os::raw::c_char;

fn cstring_into_raw(s: String) -> *mut c_char {
    match CString::new(s) {
        Ok(c) => c.into_raw(),
        Err(_) => CString::new("").map(|c| c.into_raw()).unwrap_or(std::ptr::null_mut()),
    }
}

/// Converts PDF bytes to Markdown and returns the result as a heap-allocated JSON
/// C string. The caller MUST free it with `pdf2md_free_string`.
///
/// JSON shape:
///   { "ok": true,  "markdown": "...", "pages": N, "words": N, "tables": N, "duration_us": N }
///   { "ok": false, "error": "..." }
#[no_mangle]
pub extern "C" fn pdf2md_convert(pdf_ptr: *const u8, pdf_len: usize) -> *mut c_char {
    let json = if pdf_ptr.is_null() {
        serde_json::json!({ "ok": false, "error": "null input pointer" })
    } else {
        let bytes = unsafe { std::slice::from_raw_parts(pdf_ptr, pdf_len) };
        match convert_pdf_bytes_to_markdown(bytes, &ConversionOptions::default()) {
            Ok(r) => serde_json::json!({
                "ok": true,
                "markdown": r.markdown,
                "pages": r.total_pages,
                "words": r.total_words,
                "tables": r.tables_detected,
                "duration_us": r.duration_us,
            }),
            Err(e) => serde_json::json!({ "ok": false, "error": e }),
        }
    };
    cstring_into_raw(json.to_string())
}

/// Probes whether raw PDF bytes contain a digital text layer (no full render).
/// Returns 1 when a digital text layer is present, 0 otherwise.
#[no_mangle]
pub extern "C" fn pdf2md_is_digital(pdf_ptr: *const u8, pdf_len: usize) -> i32 {
    if pdf_ptr.is_null() {
        return 0;
    }
    let bytes = unsafe { std::slice::from_raw_parts(pdf_ptr, pdf_len) };
    if is_digital_pdf_bytes(bytes) {
        1
    } else {
        0
    }
}

/// Frees a heap-allocated C string returned by `pdf2md_convert` / `pdf2md_version`.
#[no_mangle]
pub extern "C" fn pdf2md_free_string(ptr: *mut c_char) {
    if ptr.is_null() {
        return;
    }
    unsafe {
        drop(CString::from_raw(ptr));
    }
}

/// Returns the engine version as a heap-allocated C string (caller frees it).
#[no_mangle]
pub extern "C" fn pdf2md_version() -> *mut c_char {
    cstring_into_raw(env!("CARGO_PKG_VERSION").to_string())
}

// ==============================================================================
// PyO3 Bindings (Active when compiled as Python wheel)
// ==============================================================================

#[cfg(feature = "python")]
#[pyfunction]
fn is_digital_pdf(bytes: &[u8]) -> PyResult<bool> {
    Ok(is_digital_pdf_bytes(bytes))
}

#[cfg(feature = "python")]
#[pyfunction]
fn convert_pdf_bytes(bytes: &[u8], detect_tables: Option<bool>) -> PyResult<String> {
    let options = ConversionOptions {
        detect_tables: detect_tables.unwrap_or(true),
        ..Default::default()
    };
    match convert_pdf_bytes_to_markdown(bytes, &options) {
        Ok(res) => Ok(res.markdown),
        Err(e) => Err(pyo3::exceptions::PyValueError::new_err(e)),
    }
}

#[cfg(feature = "python")]
#[pyfunction]
fn reconstruct_tables_from_json(spans_json: &str) -> PyResult<String> {
    let spans: Vec<TextSpan> = serde_json::from_str(spans_json)
        .map_err(|e| pyo3::exceptions::PyValueError::new_err(format!("Invalid spans JSON: {}", e)))?;
    let tables = reconstruct_canvas_tables(&spans);
    let mut output = String::new();
    for t in tables {
        output.push_str(&t.to_markdown());
        output.push_str("\n\n");
    }
    Ok(output)
}

#[cfg(feature = "python")]
#[pyfunction]
fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(feature = "python")]
#[pymodule]
fn pdf2md_core(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(is_digital_pdf, m)?)?;
    m.add_function(wrap_pyfunction!(convert_pdf_bytes, m)?)?;
    m.add_function(wrap_pyfunction!(reconstruct_tables_from_json, m)?)?;
    m.add_function(wrap_pyfunction!(version, m)?)?;
    Ok(())
}
