// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Robust multilingual (FR / VI / EN) PDF text extraction for the fast path.
//!
//! This module replaces `lopdf::Document::extract_text` for the digital-PDF
//! fast path. `lopdf`'s extractor has two defects that corrupt accented Latin
//! text (French, Vietnamese, ...):
//!
//! 1. A `/Encoding` dictionary whose `/Differences` array contains `/.notdef`
//!    (extremely common in real-world generators, e.g. EDF / Enedis bills) is
//!    treated as an *error*, so the whole font silently falls back to lopdf's
//!    `STANDARD_ENCODING` table.
//! 2. That fallback table is corrupt for bytes >= 0xC0: it maps byte `0xE9`
//!    (`é`) to `Ø` and byte `0xE8` (`è`) to `Ł`, among others.
//!
//! We therefore resolve font encodings ourselves with correct data tables and
//! decode each text run accordingly:
//!   * `/ToUnicode` CMaps (bfchar / bfrange) — authoritative when present;
//!   * `/Encoding` by name (`WinAnsiEncoding`, `MacRomanEncoding`, ...);
//!   * `/Encoding` dictionaries with `/Differences` (`.notdef` and unknown
//!     glyph names are tolerated, AGL `uniXXXX` names supported);
//!   * a WinAnsi heuristic for non-symbolic simple fonts with no encoding
//!     information (how the overwhelming majority of producers write accented
//!     Latin text).
//!
//! Page content / font-structure parsing still comes from lopdf (public API
//! only). If content parsing fails, the caller falls back to lopdf's own
//! extractor.

use super::*;

/// ASCII-only base used for StandardEncoding / MacExpertEncoding /
/// PDFDocEncoding / unknown names: high bytes decode to nothing instead of to
/// corrupt letters (lopdf's fallback table maps 0xE9 -> 'Ø' etc.).
pub(super) fn ascii_table() -> ByteTable {
    let mut t = [0u16; 256];
    for i in 0x20..0x7F {
        t[i] = i as u16;
    }
    ByteTable(t)
}

pub(super) fn base_table(name: &[u8]) -> ByteTable {
    match name {
        b"WinAnsiEncoding" => ByteTable(WIN_ANSI),
        b"MacRomanEncoding" => ByteTable(MAC_ROMAN),
        _ => ascii_table(),
    }
}

/// Present Latin ligature glyph names. The bundled AGL table stops at
/// U+2FFF, so the presentation-form names (`fi`, `fl`, …) are not in it; map
/// them explicitly. The walker folds U+FB00–U+FB06 back to letters, so the
/// final output carries the plain letter sequence.
pub(super) fn ligature_name_unicode(name: &[u8]) -> Option<u16> {
    Some(match name {
        b"ff" => 0xFB00,
        b"fi" => 0xFB01,
        b"fl" => 0xFB02,
        b"ffi" => 0xFB03,
        b"ffl" => 0xFB04,
        b"st" => 0xFB06,
        _ => return None,
    })
}

/// AGL lookup (exact name), plus the ligature names the bundled table omits.
pub(super) fn agl_name_unicode(name: &[u8]) -> Option<u16> {
    if let Ok(idx) = AGL_NAMES.binary_search_by(|entry: &(&[u8], u16)| entry.0.cmp(name)) {
        return Some(AGL_NAMES[idx].1);
    }
    ligature_name_unicode(name)
}

/// Decode a `uniXXXX` / `uniXXXXXXXX…` byte string (UTF-16BE hex units) to a
/// single code point. A multi-unit sequence that spells a known ligature
/// (`uni00660069` = "fi") maps to its presentation-form code point so the
/// walker can fold it to letters; otherwise the first unit is kept.
pub(super) fn uni_units_unicode(hex: &[u8]) -> Option<u16> {
    let digits: Vec<u8> = hex
        .iter()
        .map(|&h| (h as char).to_digit(16).map(|d| d as u8))
        .collect::<Option<_>>()?;
    // Every `uniXXXX` unit is four hex digits; a length that is not a multiple
    // of four is a producer-specific prefix like `UNIC0041`, not this form.
    if digits.len() < 4 || digits.len() % 4 != 0 {
        return None;
    }
    let mut units: Vec<u16> = Vec::with_capacity(digits.len() / 4);
    for chunk in digits.chunks(4) {
        units.push(
            ((chunk[0] as u16) << 12)
                | ((chunk[1] as u16) << 8)
                | ((chunk[2] as u16) << 4)
                | chunk[3] as u16,
        );
    }
    if units.len() == 1 {
        return Some(units[0]);
    }
    let s = String::from_utf16(&units).ok()?;
    Some(match s.as_str() {
        "ff" => 0xFB00,
        "fi" => 0xFB01,
        "fl" => 0xFB02,
        "ffi" => 0xFB03,
        "ffl" => 0xFB04,
        "st" => 0xFB06,
        _ => units[0],
    })
}

/// Decode an underscore-separated component name (`f_i`) to one code point,
/// folding the joined sequence to a ligature when it is one. Returns `None`
/// when the name has no underscore.
pub(super) fn underscore_name_unicode(name: &[u8]) -> Option<u16> {
    if !name.contains(&b'_') {
        return None;
    }
    let mut units: Vec<u16> = Vec::new();
    for part in name.split(|&b| b == b'_') {
        if part.is_empty() {
            return None;
        }
        units.push(agl_name_unicode(part)?);
    }
    if units.len() == 2 {
        match (units[0], units[1]) {
            (0x0066, 0x0066) => return Some(0xFB00),
            (0x0066, 0x0069) => return Some(0xFB01),
            (0x0066, 0x006C) => return Some(0xFB02),
            _ => {}
        }
    }
    if units.len() == 3 && units[0] == 0x0066 && units[1] == 0x0066 {
        match units[2] {
            0x0069 => return Some(0xFB03),
            0x006C => return Some(0xFB04),
            _ => {}
        }
    }
    units.first().copied()
}

/// Resolve a `/Differences` glyph name to its Unicode value.
/// `Some(0)` means "mapped to no character" (skip); `None` = unknown name.
pub(super) fn glyph_unicode(name: &[u8]) -> Option<u16> {
    if name == b".notdef" {
        // A code explicitly mapped to /.notdef has no glyph: decode to nothing.
        return Some(0);
    }
    if let Some(u) = agl_name_unicode(name) {
        return Some(u);
    }
    // Producer suffixes: the base name carries the glyph (`a.sc` -> `a`,
    // `one.oldstyle` -> `one`).
    if let Some(dot) = name.iter().position(|&b| b == b'.') {
        if dot > 0 {
            if let Some(u) = agl_name_unicode(&name[..dot]) {
                return Some(u);
            }
        }
    }
    if let Some(u) = underscore_name_unicode(name) {
        return Some(u);
    }
    // AGL "uniXXXX" form, including multi-unit sequences (`uni00660069`).
    if name.len() >= 7 && &name[..3] == b"uni" {
        if let Some(u) = uni_units_unicode(&name[3..]) {
            return Some(u);
        }
    }
    // "UNICXXXX" is the same idea with a producer-specific prefix (seen on BNP
    // Paribas Type3 statements, whose /Differences are entirely /UNICxxxx).
    if name.len() >= 8 && name[..4].eq_ignore_ascii_case(b"unic") {
        return uni_units_unicode(&name[4..]);
    }
    // "uXXXX" / "uXXXXX" / "uXXXXXX": a single code point (BMP only — the byte
    // table is 16-bit).
    if name.len() >= 5 && name[0] == b'u' && name[1..].iter().all(|b| b.is_ascii_hexdigit()) {
        let hex = std::str::from_utf8(&name[1..]).ok()?;
        let v = u32::from_str_radix(hex, 16).ok()?;
        return if v <= 0xFFFF { Some(v as u16) } else { None };
    }
    None
}

pub(super) fn apply_differences(table: &mut ByteTable, doc: &Document, array: &Object) {
    let Some(arr) = deref(doc, array).and_then(|o| o.as_array().ok()) else {
        return;
    };
    let mut code: u8 = 0;
    for item in arr {
        match item {
            Object::Integer(v) => {
                if (0..=255).contains(v) {
                    code = *v as u8;
                }
            }
            Object::Name(nm) => {
                table.0[code as usize] = glyph_unicode(nm).unwrap_or(0);
                code = code.wrapping_add(1);
            }
            _ => {}
        }
    }
}
