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
