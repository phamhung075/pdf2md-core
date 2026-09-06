//! pdf2md-core — High-performance native Rust core engine for sub-millisecond
//! PDF-to-Markdown extraction and 2D spatial canvas table reconstruction.
//!
//! Licensed under MIT OR Apache-2.0. Zero AGPL/GPL dependencies.

mod glyph_data;
mod layout;
mod media;
mod text_extract;

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
    /// Extract placed images and return them as base64 media items.
    pub detect_media: bool,
    /// Rebuild reading order with zones/columns/furniture handling on the
    /// geometry path (fallback is byte-identical for simple single-column
    /// pages).
    pub detect_layout: bool,
    /// Embed extracted, non-decorative images into the markdown itself as
    /// self-contained data-URI lines (placed top-to-bottom per page). When
    /// false, images are only returned in the `media` JSON list.
    pub embed_media: bool,
    /// Detect pure-vector figure regions (charts/diagrams/logos drawn with
    /// paths, no raster) and cut them out as standalone clipped PDFs.
    pub detect_vectors: bool,
}

impl Default for ConversionOptions {
    fn default() -> Self {
        Self {
            detect_tables: true,
            detect_headings: true,
            min_words_per_page: 5,
            detect_media: true,
            detect_layout: true,
            embed_media: true,
            detect_vectors: false,
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
    /// Extracted image placements (base64 payloads) when `detect_media`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub media: Vec<media::MediaItem>,
    /// Structured reading-order blocks for pages handled by the geometry
    /// layout engine.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocks: Vec<layout::DocBlock>,
}

/// Probes whether raw PDF bytes contain a digital text stream without full rendering.
pub fn is_digital_pdf_bytes(bytes: &[u8]) -> bool {
    if bytes.len() < 32 || !bytes.starts_with(b"%PDF-") {
        return false;
    }

    // Structural check: parse and look for a real text layer. Unlike a raw
    // string scan, this handles content streams that are FlateDecode-compressed
    // (where "BT"/"Tj" never appear in the raw bytes) — e.g. PDFCreator/
    // Ghostscript tickets and most modern PDFs. A page is "digital" if it
    // references any font or issues any text-show operator.
    if let Ok(doc) = lopdf::Document::load_mem(bytes) {
        for (_page_num, page_id) in doc.get_pages() {
            if doc.get_page_fonts(page_id).map_or(false, |f| !f.is_empty()) {
                return true;
            }
            if let Ok(content) = doc.get_and_decode_page_content(page_id) {
                if content.operations.iter().any(|op| {
                    matches!(op.operator.as_str(), "Tj" | "TJ" | "'" | "\"")
                }) {
                    return true;
                }
            }
        }
        return false;
    }

    // Fallback (failed parse / truncated input): raw markers.
    let text_markers: &[&[u8]] = &[b"BT\n", b"BT\r", b"BT ", b"/Font", b"Tj", b"TJ"];
    let mut matches = 0;
    for marker in text_markers {
        if bytes.windows(marker.len()).any(|w| w == *marker) {
            matches += 1;
        }
    }
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


/// Best-effort device page box ([x0, y0, x1, y1]) from the page /MediaBox,
/// used to classify full-page background images.
fn page_media_box(doc: &lopdf::Document, page_id: lopdf::ObjectId) -> Option<(f64, f64, f64, f64)> {
    let dict = doc.get_dictionary(page_id).ok()?;
    let mb = dict.get(b"MediaBox").ok()?.as_array().ok()?;
    let g = |i: usize| -> Option<f64> {
        mb.get(i)
            .and_then(|o| o.as_float().ok().map(|f| f as f64))
            .or_else(|| mb.get(i).and_then(|o| o.as_i64().ok().map(|v| v as f64)))
    };
    Some((g(0)?, g(1)?, g(2)?, g(3)?))
}

/// Converts PDF byte slice to clean Markdown with 2D spatial table reconstruction.
pub fn convert_pdf_bytes_to_markdown(bytes: &[u8], options: &ConversionOptions) -> Result<ConversionResult, String> {
    let t0 = Instant::now();

    // Try parsing with lopdf
    let doc = lopdf::Document::load_mem(bytes)
        .map_err(|e| format!("lopdf parsing error: {}", e))?;

    let mut full_markdown = String::new();
    let total_pages = doc.get_pages().len();
    let mut total_words = 0;
    let mut any_text_ops = false;
    let mut any_fonts = false;
    let mut tables_detected = 0usize;
    let mut media_items: Vec<media::MediaItem> = Vec::new();
    let mut block_items: Vec<layout::DocBlock> = Vec::new();

    for (page_num, page_id) in doc.get_pages() {
        // Prefer our own multilingual decoder (correct WinAnsi/Differences/
        // ToUnicode handling — see text_extract.rs) and only fall back to
        // lopdf's extractor when the page content cannot be parsed at all.
        let page_text = match text_extract::extract_page_text_report(&doc, page_num, options.detect_tables, options.detect_layout) {
            Ok(pt) => pt,
            Err(_) => text_extract::PageText {
                text: doc.extract_text(&[page_num]).unwrap_or_default(),
                text_ops_seen: true,
                has_fonts: doc.get_page_fonts(page_id).map_or(false, |f| !f.is_empty()),
                tables: 0,
                blocks: Vec::new(),
            },
        };
        any_text_ops |= page_text.text_ops_seen;
        any_fonts |= page_text.has_fonts;
        tables_detected += page_text.tables;
        let text = page_text.text.clone();
        let mut page_blocks: Vec<layout::DocBlock> = page_text.blocks;
        for b in &mut page_blocks {
            b.page = page_num as usize;
        }

        let page_media: Vec<media::MediaItem> = if options.detect_media {
            let page_bbox = page_media_box(&doc, page_id);
            let mut m = media::extract_page_media(&doc, page_id, page_num as usize, page_bbox);
            if options.detect_vectors {
                m.extend(media::extract_page_vector_figures(
                    &doc,
                    page_id,
                    page_num as usize,
                    page_bbox,
                ));
            }
            m
        } else {
            Vec::new()
        };

        // Figure + caption adjacency: when this page has structured layout
        // blocks, mark the short text line attached to a content image as a
        // caption so the block list reads like a document ("figure -> caption").
        if options.detect_layout && !page_media.is_empty() && !page_blocks.is_empty() {
            for m in page_media.iter().filter(|m| !m.decorative) {
                let my = (m.y0 + m.y1) / 2.0;
                let best = page_blocks.iter_mut().filter(|b| b.kind != "figure").min_by(|a, b2| {
                    let da = ((a.y0 + a.y1) / 2.0 - my).abs();
                    let db = ((b2.y0 + b2.y1) / 2.0 - my).abs();
                    da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
                });
                if let Some(b) = best {
                    let gap = (my - (b.y0 + b.y1) / 2.0).abs();
                    let size_hint = (b.y1 - b.y0).abs().max(8.0);
                    if gap <= 2.5 * size_hint && b.text.split_whitespace().count() <= 9 {
                        b.kind = "caption".to_string();
                    }
                }
            }
        }
        block_items.extend(page_blocks);

        // Words are counted from the text layer only, so corpus text-word
        // baselines are unaffected by image embedding.
        let words: Vec<&str> = text.split_whitespace().collect();
        total_words += words.len();
        for m in &page_media {
            media_items.push(m.clone());
        }

        if options.detect_headings && (text.starts_with("# ") || text.lines().next().map_or(false, |l| l.len() < 60 && l.chars().all(|c| c.is_alphanumeric() || c.is_whitespace()))) {
            full_markdown.push_str(&format!("\n## Page {}\n\n", page_num));
        }

        full_markdown.push_str(&text);
        full_markdown.push_str("\n\n");
        if options.embed_media {
            // Non-decorative images of this page, reading order top-to-bottom
            // (larger device y first), as self-contained markdown images.
            let mut imgs: Vec<&media::MediaItem> = page_media
                .iter()
                .filter(|m| {
                    !m.decorative
                        && !m.data_b64.is_empty()
                        && (m.format == "image/jpeg" || m.format == "image/png")
                })
                .collect();
            imgs.sort_by(|a, b| {
                let ay = (a.y0 + a.y1) / 2.0;
                let by = (b.y0 + b.y1) / 2.0;
                by.partial_cmp(&ay).unwrap_or(std::cmp::Ordering::Equal)
            });
            if !imgs.is_empty() {
                for m in imgs {
                    full_markdown.push_str(&format!(
                        "![{}](data:{};base64,{})\n\n",
                        m.kind.as_str(),
                        m.format,
                        m.data_b64
                    ));
                }
            }
        }
    }

    // When nothing was decoded, give an actionable reason instead of a silent
    // empty markdown:
    //  * text-show operators but no readable text -> glyph-encoded (Type3) doc;
    //  * no fonts and no text-show operators at all -> scanned/image page.
    if total_words == 0 {
        if any_text_ops {
            return Err(
                "Document text layer is glyph-encoded (e.g. Type3/outlined) with no Unicode mapping; \
                 no readable text found — route through the OCR/Vision pipeline (Docling/Gemini)."
                    .to_string(),
            );
        }
        if !any_fonts {
            return Err(
                "Document lacks a digital text layer or is scanned (images only); \
                 route through the OCR/Vision pipeline (Docling/Gemini)."
                    .to_string(),
            );
        }
        return Err(
            "Document references fonts but no readable text was found (scanned or outlined); \
             route through the OCR/Vision pipeline (Docling/Gemini)."
                .to_string(),
        );
    }

    let duration_us = t0.elapsed().as_micros() as u64;

    Ok(ConversionResult {
        markdown: full_markdown,
        total_pages,
        total_words,
        tables_detected,
        duration_us,
        media: media_items,
        blocks: block_items,
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
///   { "ok": true, "markdown": "...", "pages": N, "words": N, "tables": N,
///     "media": [ { page, x0,y0,x1,y1, width,height, format, kind, decorative, repeat, data_b64 } ],
///     "duration_us": N }
///   { "ok": false, "error": "..." }
#[no_mangle]
pub extern "C" fn pdf2md_convert(pdf_ptr: *const u8, pdf_len: usize) -> *mut c_char {
    let json = if pdf_ptr.is_null() {
        serde_json::json!({ "ok": false, "error": "null input pointer" })
    } else {
        let bytes = unsafe { std::slice::from_raw_parts(pdf_ptr, pdf_len) };
        let mut opts = ConversionOptions::default();
        // Optional escape hatch so sandbox/FFI consumers can request vector
        // figure cuts without an ABI change.
        if std::env::var("P2M_DETECT_VECTORS").map_or(false, |v| v == "1" || v == "true") {
            opts.detect_vectors = true;
        }
        match convert_pdf_bytes_to_markdown(bytes, &opts) {
            Ok(r) => {
                let media_json = serde_json::to_value(&r.media).unwrap_or_else(|_| serde_json::json!([]));
                let blocks_json = serde_json::to_value(&r.blocks).unwrap_or_else(|_| serde_json::json!([]));
                serde_json::json!({
                    "ok": true,
                    "markdown": r.markdown,
                    "pages": r.total_pages,
                    "words": r.total_words,
                    "tables": r.tables_detected,
                    "media": media_json,
                    "blocks": blocks_json,
                    "duration_us": r.duration_us,
                })
            }
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
