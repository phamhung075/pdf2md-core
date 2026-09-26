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

/// True for a Unicode private-use code point. An unmapped one is mojibake, not
/// text: it must never be emitted verbatim.
pub(super) fn is_private_use_char(c: char) -> bool {
    let cp = c as u32;
    (0xE000..=0xF8FF).contains(&cp)
        || (0xF0000..=0xFFFFD).contains(&cp)
        || (0x100000..=0x10FFFD).contains(&cp)
}

/// Push `c`, mapping a private-use code point through the Symbol/Wingdings table
/// for `family`. `Some(family)` is the font-known decode used by the string
/// walker: a Symbol-family font uses the Adobe Symbol charset and a Wingdings
/// face its corpus-verified table, and an unmapped PUA character is dropped.
/// `None` is the raw decode used by the glyph engine, which must keep the
/// pre-mapping bytes so its table detection is unchanged; the shared
/// `normalize_decoded_text` chokepoint maps them afterwards.
pub(super) fn push_mapped(out: &mut String, c: char, family: Option<PuaFamily>) {
    if !is_private_use_char(c) {
        out.push(c);
        return;
    }
    match family {
        Some(family) => {
            if let Some(m) = pua_to_char_for_family(c as u32, family) {
                out.push(m);
            }
        }
        None => out.push(c),
    }
}

pub(super) fn push_utf16(out: &mut String, units: &[u16], family: Option<PuaFamily>) {
    for c in char::decode_utf16(units.iter().copied()).flatten() {
        push_mapped(out, c, family);
    }
}

impl Codec {
    /// Font-known decode (string walker): map PUA by family.
    pub(crate) fn decode(&self, bytes: &[u8], out: &mut String) {
        match self {
            Codec::Byte8(t, family) => decode_byte_table(t, bytes, out, Some(*family)),
            Codec::CMap(cm, fallback, family) => {
                decode_cmap(cm, bytes, fallback.as_ref(), out, Some(*family));
            }
        }
    }

    /// Raw decode (glyph engine): keep private-use code points so the layout
    /// and table passes see exactly the pre-mapping text. `normalize_decoded_text`
    /// maps them later through the font-unknown union.
    pub(crate) fn decode_raw(&self, bytes: &[u8], out: &mut String) {
        match self {
            Codec::Byte8(t, _) => decode_byte_table(t, bytes, out, None),
            Codec::CMap(cm, fallback, _) => {
                decode_cmap(cm, bytes, fallback.as_ref(), out, None);
            }
        }
    }

    /// Number of character *codes* in `bytes` and how many of them are a space,
    /// mirroring [`Self::decode`]'s code consumption.
    ///
    /// `Tc` is added once per code and `Tw` once per space code, so the raw
    /// byte length is the wrong unit for a 2-byte CID / Identity-H / UTF-16
    /// font: there `bytes.len() == 2 * codes`, which doubled the `Tc` term in
    /// `show_advance` and `glyph_stream::push_span`, over-measuring a run's
    /// right edge and fusing the following word onto it (D2). The separate
    /// space count keeps `Tw` correct for a multi-byte font (a raw `b' '` byte
    /// can be the low byte of any code, not only a space).
    pub(crate) fn code_metrics(&self, bytes: &[u8]) -> (usize, usize) {
        match self {
            Codec::Byte8(t, _) => {
                let spaces = bytes.iter().filter(|&&b| t.0[b as usize] == 0x20).count();
                (bytes.len(), spaces)
            }
            Codec::CMap(cm, fallback, _) => {
                let mut codes = 0usize;
                let mut spaces = 0usize;
                let mut i = 0usize;
                while i < bytes.len() {
                    let mut matched = false;
                    for len in 1u8..=4u8 {
                        let n = len as usize;
                        if i + n > bytes.len() {
                            continue;
                        }
                        let mut code: u32 = 0;
                        for k in 0..n {
                            code = (code << 8) | bytes[i + k] as u32;
                        }
                        if let Some(units) = cm.exact.get(&(len, code)) {
                            if units.first() == Some(&0x20) {
                                spaces += 1;
                            }
                            codes += 1;
                            i += n;
                            matched = true;
                            break;
                        }
                        if let Some(dst) = cm
                            .ranges
                            .iter()
                            .find(|(rl, lo, hi, _)| *rl == len && code >= *lo && code <= *hi)
                            .map(|(_, lo, _, dst_lo)| dst_lo + (code - lo))
                        {
                            if dst == 0x20 {
                                spaces += 1;
                            }
                            codes += 1;
                            i += n;
                            matched = true;
                            break;
                        }
                    }
                    if !matched {
                        if let Some(t) = fallback {
                            if t.0[bytes[i] as usize] == 0x20 {
                                spaces += 1;
                            }
                        }
                        codes += 1;
                        i += 1;
                    }
                }
                (codes, spaces)
            }
        }
    }
}

pub(super) fn decode_byte_table(t: &ByteTable, bytes: &[u8], out: &mut String, family: Option<PuaFamily>) {
    for &b in bytes {
        let u = t.0[b as usize];
        if u != 0 {
            if let Some(ch) = char::from_u32(u as u32) {
                push_mapped(out, ch, family);
            }
        }
    }
}

pub(super) fn decode_cmap(
    cm: &CMapCodec,
    bytes: &[u8],
    fallback: Option<&ByteTable>,
    out: &mut String,
    family: Option<PuaFamily>,
) {
    let mut i = 0usize;
    while i < bytes.len() {
        let mut matched = false;
        for len in 1u8..=4u8 {
            let n = len as usize;
            if i + n > bytes.len() {
                continue;
            }
            let mut code: u32 = 0;
            for k in 0..n {
                code = (code << 8) | bytes[i + k] as u32;
            }
            if let Some(units) = cm.exact.get(&(len, code)) {
                push_utf16(out, units, family);
                i += n;
                matched = true;
                break;
            }
            if let Some(dst) = cm
                .ranges
                .iter()
                .find(|(rl, lo, hi, _)| *rl == len && code >= *lo && code <= *hi)
                .map(|(_, lo, _, dst_lo)| dst_lo + (code - lo))
            {
                push_utf16(out, &[dst as u16], family);
                i += n;
                matched = true;
                break;
            }
        }
        if !matched {
            // Unmapped source code: use the byte-encoding fallback for that one
            // code when one is available, otherwise skip rather than invent.
            if let Some(t) = fallback {
                let u = t.0[bytes[i] as usize];
                if u != 0 {
                    if let Some(ch) = char::from_u32(u as u32) {
                        push_mapped(out, ch, family);
                    }
                }
            }
            i += 1;
        }
    }
}

// ---------------------------------------------------------------------------
// CMap parsing (bfchar / bfrange / codespacerange subset)
// ---------------------------------------------------------------------------
