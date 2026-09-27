// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! C ABI (FFI) exports for embedding in Go (cgo), Python, Node, and other native consumers.

use std::ffi::CString;
use std::os::raw::c_char;

use crate::{convert_pdf_bytes_to_markdown, is_digital_pdf_bytes, ConversionOptions, MediaMode};

/// Test-only switch that forces a panic inside `pdf2md_convert_impl`, proving
/// that `catch_ffi` turns an FFI panic into the entry's error return. Compiled
/// out of release builds.
#[cfg(test)]
pub(crate) static FORCE_CONVERT_PANIC: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

pub(crate) fn cstring_into_raw(s: String) -> *mut c_char {
    match CString::new(s) {
        Ok(c) => c.into_raw(),
        Err(_) => CString::new("")
            .map(|c| c.into_raw())
            .unwrap_or(std::ptr::null_mut()),
    }
}

/// Run an FFI entry body, converting a Rust panic into `fallback` instead of
/// letting it unwind across the C ABI.
///
/// With the previous `panic = "abort"` release profile any panic aborted the
/// host process (the Go gateway, via CGO). The release profile is now
/// `panic = "unwind"`, so every `#[no_mangle] pub extern "C"` entry point wraps
/// its body here: a panic becomes the same error value the entry already
/// returns on failure (error JSON / null pointer / 0 / empty string), which the
/// gateway already handles as a conversion error. `AssertUnwindSafe` is sound
/// at this boundary because a panicking call is abandoned, not reused.
fn catch_ffi<R>(fallback: impl FnOnce() -> R, body: impl FnOnce() -> R) -> R {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(body)) {
        Ok(value) => value,
        Err(_) => fallback(),
    }
}

/// The error JSON already used for conversion failures, for a caught panic.
fn ffi_panic_error(entry: &str) -> *mut c_char {
    cstring_into_raw(
        serde_json::json!({ "ok": false, "error": format!("internal panic in {entry}") }).to_string(),
    )
}

/// Converts PDF bytes to Markdown and returns the result as a heap-allocated JSON
/// C string. The caller MUST free it with `pdf2md_free_string`.
///
/// JSON shape:
///   { "ok": true, "markdown": "...", "pages": N, "words": N,
///     "pages_below_word_floor": N, "tables": N,
///     "media": [ { page, x0,y0,x1,y1, width,height, format, kind, decorative, repeat, data_b64 } ],
///     "duration_us": N }
///   { "ok": false, "error": "..." }
#[no_mangle]
pub extern "C" fn pdf2md_convert(pdf_ptr: *const u8, pdf_len: usize) -> *mut c_char {
    catch_ffi(
        || ffi_panic_error("pdf2md_convert"),
        || pdf2md_convert_impl(pdf_ptr, pdf_len, None, None, None),
    )
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
    catch_ffi(
        || ffi_panic_error("pdf2md_convert_ex"),
        || pdf2md_convert_impl(pdf_ptr, pdf_len, Some(detect_vectors != 0), None, None),
    )
}

/// Same as `pdf2md_convert_ex`, but with an additional explicit `no_media`
/// flag (0 = off/default, nonzero = on): equivalent to the CLI's `--no-media`
/// and forces [`MediaMode::None`]. The media policy now defaults to none for
/// every entry point, so `no_media = 0` selects that default rather than
/// embedding.
#[no_mangle]
pub extern "C" fn pdf2md_convert_ex2(
    pdf_ptr: *const u8,
    pdf_len: usize,
    detect_vectors: i32,
    no_media: i32,
) -> *mut c_char {
    catch_ffi(
        || ffi_panic_error("pdf2md_convert_ex2"),
        || {
            pdf2md_convert_impl(
                pdf_ptr,
                pdf_len,
                Some(detect_vectors != 0),
                if no_media != 0 {
                    Some(MediaMode::None)
                } else {
                    None
                },
                None,
            )
        },
    )
}

/// Same as `pdf2md_convert_ex`, but with an explicit media policy:
/// `0 = none` (neither extract nor inline, the default), `1 = reference`
/// (extract the JSON `media` list only, never inline a `data:` URI) and
/// `2 = embed` (extract and inline `data:` URIs). Any other value selects the
/// safe middle policy, `reference`. This is the general FFI surface for
/// [`crate::MediaMode`]; `pdf2md_convert_ex2`'s `no_media` flag remains the
/// compatibility alias for `none`.
#[no_mangle]
pub extern "C" fn pdf2md_convert_ex3(
    pdf_ptr: *const u8,
    pdf_len: usize,
    detect_vectors: i32,
    media_mode: i32,
) -> *mut c_char {
    catch_ffi(
        || ffi_panic_error("pdf2md_convert_ex3"),
        || {
            pdf2md_convert_impl(
                pdf_ptr,
                pdf_len,
                Some(detect_vectors != 0),
                Some(match media_mode {
                    0 => MediaMode::None,
                    2 => MediaMode::Embed,
                    // 1, and any unrecognized value, use the middle policy: extract
                    // the `media` side-channel but inline no `data:` URIs.
                    _ => MediaMode::Reference,
                }),
                None,
            )
        },
    )
}

/// Same as `pdf2md_convert_ex3`, but with an additional explicit
/// `page_markers` flag (0 = off/default, nonzero = on): emits an exact
/// per-page HTML-comment boundary marker (`<!-- pdf2w:page n="N" -->`, one per
/// page including page 1) immediately before each page's reflowed content.
/// Only meaningful for digital PDFs; the marker is opt-in so `ex4(..., 0)`
/// stays byte-identical to `ex3`.
#[no_mangle]
pub extern "C" fn pdf2md_convert_ex4(
    pdf_ptr: *const u8,
    pdf_len: usize,
    detect_vectors: i32,
    media_mode: i32,
    page_markers: i32,
) -> *mut c_char {
    catch_ffi(
        || ffi_panic_error("pdf2md_convert_ex4"),
        || {
            pdf2md_convert_impl(
                pdf_ptr,
                pdf_len,
                Some(detect_vectors != 0),
                Some(match media_mode {
                    0 => MediaMode::None,
                    2 => MediaMode::Embed,
                    // 1, and any unrecognized value, use the middle policy: extract
                    // the `media` side-channel but inline no `data:` URIs.
                    _ => MediaMode::Reference,
                }),
                Some(page_markers != 0),
            )
        },
    )
}

fn pdf2md_convert_impl(
    pdf_ptr: *const u8,
    pdf_len: usize,
    vectors_override: Option<bool>,
    media_mode_override: Option<MediaMode>,
    page_markers_override: Option<bool>,
) -> *mut c_char {
    #[cfg(test)]
    if FORCE_CONVERT_PANIC.load(std::sync::atomic::Ordering::Relaxed) {
        panic!("test-only forced panic");
    }
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
        // Media policy: an explicit per-call override (the `media_mode` /
        // `no_media` arguments) wins; otherwise the process-wide
        // `P2M_MEDIA_MODE` hatch (none|reference|embed) can raise it, since a
        // long-lived server process cannot vary the per-call argument for a
        // legacy entry point. Unset uses the default, `none`.
        if let Some(mode) = media_mode_override {
            opts.media_mode = mode;
        } else if let Ok(v) = std::env::var("P2M_MEDIA_MODE") {
            opts.media_mode = match v.trim().to_ascii_lowercase().as_str() {
                "embed" => MediaMode::Embed,
                "reference" | "ref" => MediaMode::Reference,
                _ => MediaMode::None,
            };
        }
        // Per-page HTML-comment boundary markers: explicit opt-in only, no
        // process-wide env hatch. Legacy entry points (`convert`/`ex`/`ex2`/
        // `ex3`) pass `None`, which resolves to the disabled default exactly
        // like the media-mode override above.
        opts.page_markers = page_markers_override.unwrap_or(false);
        // Optional overrides for the media-embed size guardrails (R1): cap the
        // pixel dimension a raster is downscaled to, and the total base64
        // bytes inlined into the markdown per document. Unset uses the
        // ConversionOptions defaults (1536px / 512KB).
        if let Ok(v) = std::env::var("P2M_MAX_IMAGE_DIM") {
            if let Ok(n) = v.parse::<u32>() {
                opts.max_image_dimension = n;
            }
        }
        if let Ok(v) = std::env::var("P2M_MAX_MEDIA_BYTES") {
            if let Ok(n) = v.parse::<usize>() {
                opts.max_media_bytes_per_doc = n;
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
                    "pages_below_word_floor": r.pages_below_word_floor,
                    "tables": r.tables_detected,
                    "undecodable_glyphs": r.undecodable_glyphs,
                    "decoded_glyphs": r.decoded_glyphs,
                    "media": media_json,
                    "blocks": blocks_json,
                    "duration_us": r.duration_us,
                    "needs_vision_rescue": r.needs_vision_rescue,
                    "rescue_reason": r.rescue_reason,
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
    catch_ffi(
        || 0,
        || {
            if pdf_ptr.is_null() {
                return 0;
            }
            let bytes = unsafe { std::slice::from_raw_parts(pdf_ptr, pdf_len) };
            if is_digital_pdf_bytes(bytes) {
                1
            } else {
                0
            }
        },
    )
}

/// Frees a heap-allocated C string returned by `pdf2md_convert` / `pdf2md_version`.
#[no_mangle]
pub extern "C" fn pdf2md_free_string(ptr: *mut c_char) {
    catch_ffi(
        || (),
        || {
            if ptr.is_null() {
                return;
            }
            unsafe {
                drop(CString::from_raw(ptr));
            }
        },
    );
}

/// Returns the engine version as a heap-allocated C string (caller frees it).
#[no_mangle]
pub extern "C" fn pdf2md_version() -> *mut c_char {
    catch_ffi(
        || cstring_into_raw(String::new()),
        || cstring_into_raw(env!("CARGO_PKG_VERSION").to_string()),
    )
}

/// Computes a 64-bit perceptual hash (DCT-pHash) of an encoded image buffer
/// (PNG/JPEG). Returns a heap-allocated C string holding 16 lowercase hex chars,
/// or an empty string when the payload is not a decodable image.
/// Caller frees it with `pdf2md_free_string`.
#[cfg(feature = "vision")]
#[no_mangle]
pub extern "C" fn pdf2md_perceptual_hash(img_ptr: *const u8, img_len: usize) -> *mut c_char {
    catch_ffi(
        || cstring_into_raw(String::new()),
        || {
            if img_ptr.is_null() {
                return cstring_into_raw(String::new());
            }
            let bytes = unsafe { std::slice::from_raw_parts(img_ptr, img_len) };
            let out = crate::phash::perceptual_hash_64(bytes)
                .map(|h| format!("{:016x}", h))
                .unwrap_or_default();
            cstring_into_raw(out)
        },
    )
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
    catch_ffi(
        || cstring_into_raw(String::new()),
        || {
            if pdf_ptr.is_null() {
                return cstring_into_raw(String::new());
            }
            let bytes = unsafe { std::slice::from_raw_parts(pdf_ptr, pdf_len) };
            let sig = match lopdf::Document::load_mem_with_options(
                bytes,
                lopdf::LoadOptions::with_max_decompressed_size(crate::MAX_DECOMPRESSED_STREAM),
            ) {
                Ok(doc) => {
                    let cap = if max_pages > 0 { max_pages as usize } else { 8 };
                    crate::phash::vision_signature(&doc, cap)
                }
                Err(_) => String::new(),
            };
            cstring_into_raw(sig)
        },
    )
}

#[cfg(test)]
mod tests;
