// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! C ABI (FFI) exports for embedding in Go (cgo), Python, Node, and other native consumers.

use std::ffi::CString;
use std::os::raw::c_char;

use crate::{convert_pdf_bytes_to_markdown, is_digital_pdf_bytes, ConversionOptions};

pub(crate) fn cstring_into_raw(s: String) -> *mut c_char {
    match CString::new(s) {
        Ok(c) => c.into_raw(),
        Err(_) => CString::new("")
            .map(|c| c.into_raw())
            .unwrap_or(std::ptr::null_mut()),
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
    pdf2md_convert_impl(pdf_ptr, pdf_len, None)
}

/// Same as `pdf2md_convert`, but with an explicit `detect_vectors` flag
/// (0 = off, nonzero = on) so sandbox/FFI consumers can request vector figure
/// cuts per call instead of relying on the `P2M_DETECT_VECTORS` env hatch.
#[no_mangle]
pub extern "C" fn pdf2md_convert_ex(
    pdf_ptr: *const u8,
    pdf_len: usize,
    detect_vectors: i32,
) -> *mut c_char {
    pdf2md_convert_impl(pdf_ptr, pdf_len, Some(detect_vectors != 0))
}

fn pdf2md_convert_impl(
    pdf_ptr: *const u8,
    pdf_len: usize,
    vectors_override: Option<bool>,
) -> *mut c_char {
    let json = if pdf_ptr.is_null() {
        serde_json::json!({ "ok": false, "error": "null input pointer" })
    } else {
        let bytes = unsafe { std::slice::from_raw_parts(pdf_ptr, pdf_len) };
        let mut opts = ConversionOptions::default();
        // Optional escape hatch so sandbox/FFI consumers can request vector
        // figure cuts without an ABI change.
        let vectors_on = vectors_override.unwrap_or_else(|| {
            std::env::var("P2M_DETECT_VECTORS").map_or(false, |v| v == "1" || v == "true")
        });
        if vectors_on {
            opts.detect_vectors = true;
        }
        // Optional hatch for LaTeX math AST synthesis (fractions and simple
        // super/subscripts). Default on; set `P2M_DETECT_MATH=0`/`false` to
        // disable, or `1`/`true` to force on (e.g. over a non-standard default).
        if let Ok(v) = std::env::var("P2M_DETECT_MATH") {
            match v.as_str() {
                "0" | "false" | "False" | "FALSE" => opts.detect_math = false,
                _ => opts.detect_math = true,
            }
        }
        match convert_pdf_bytes_to_markdown(bytes, &opts) {
            Ok(r) => {
                let media_json =
                    serde_json::to_value(&r.media).unwrap_or_else(|_| serde_json::json!([]));
                let blocks_json =
                    serde_json::to_value(&r.blocks).unwrap_or_else(|_| serde_json::json!([]));
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

/// Computes a 64-bit perceptual hash (DCT-pHash) of an encoded image buffer
/// (PNG/JPEG). Returns a heap-allocated C string holding 16 lowercase hex chars,
/// or an empty string when the payload is not a decodable image.
/// Caller frees it with `pdf2md_free_string`.
#[cfg(feature = "vision")]
#[no_mangle]
pub extern "C" fn pdf2md_perceptual_hash(img_ptr: *const u8, img_len: usize) -> *mut c_char {
    if img_ptr.is_null() {
        return cstring_into_raw(String::new());
    }
    let bytes = unsafe { std::slice::from_raw_parts(img_ptr, img_len) };
    let out = crate::phash::perceptual_hash_64(bytes)
        .map(|h| format!("{:016x}", h))
        .unwrap_or_default();
    cstring_into_raw(out)
}

/// Computes a whole-document perceptual signature from raw PDF bytes: one 64-bit
/// DCT-pHash per page (up to `max_pages`, 0 = default 8), comma-joined as hex.
/// Returns a heap-allocated C string (caller frees with `pdf2md_free_string`),
/// or "" when the document has no hashable raster (scanned/form pages normally
/// do — see `phash::page_dominant_raster_bytes`).
#[cfg(feature = "vision")]
#[no_mangle]
pub extern "C" fn pdf2md_vision_signature(
    pdf_ptr: *const u8,
    pdf_len: usize,
    max_pages: i32,
) -> *mut c_char {
    if pdf_ptr.is_null() {
        return cstring_into_raw(String::new());
    }
    let bytes = unsafe { std::slice::from_raw_parts(pdf_ptr, pdf_len) };
    let sig = match lopdf::Document::load_mem(bytes) {
        Ok(doc) => {
            let cap = if max_pages > 0 { max_pages as usize } else { 8 };
            crate::phash::vision_signature(&doc, cap)
        }
        Err(_) => String::new(),
    };
    cstring_into_raw(sig)
}
