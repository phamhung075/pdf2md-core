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

impl CMapCodec {
    /// Look up a source code and return its destination as a single integer
    /// (first UTF-16 unit). Used for `/Encoding` CID maps (code -> CID), whose
    /// destinations are single 16-bit values.
    pub(crate) fn lookup(&self, bytes: &[u8]) -> Option<u32> {
        if bytes.is_empty() {
            return None;
        }
        let mut code: u32 = 0;
        for &b in bytes {
            code = (code << 8) | b as u32;
        }
        for len in 1u8..=4u8 {
            let n = len as usize;
            if n != bytes.len() {
                continue;
            }
            if let Some(units) = self.exact.get(&(len, code)) {
                return units.first().map(|&u| u as u32);
            }
            if let Some(dst) = self
                .ranges
                .iter()
                .find(|(rl, lo, hi, _)| *rl == len && code >= *lo && code <= *hi)
                .map(|(_, lo, _, dst_lo)| dst_lo + (code - lo))
            {
                return Some(dst);
            }
        }
        None
    }
}

pub(crate) fn parse_cmap(data: &[u8]) -> Option<CMapCodec> {
    let toks = cmap_tokens(data);
    let mut cm = CMapCodec::default();
    let mut i = 0usize;
    while i < toks.len() {
        let word = match &toks[i] {
            Tok::Word(w) => w.clone(),
            _ => {
                i += 1;
                continue;
            }
        };
        let next_hex = |toks: &[Tok], i: &mut usize| -> Option<Vec<u8>> {
            if *i < toks.len() {
                if let Tok::Hex(h) = &toks[*i] {
                    *i += 1;
                    return Some(h.clone());
                }
            }
            None
        };
        let is_end = |toks: &[Tok], i: usize, name: &str| -> bool {
            matches!(&toks.get(i), Some(Tok::Word(w)) if w == name)
        };
        match word.as_str() {
            "beginbfchar" => {
                i += 1;
                while i < toks.len() && !is_end(&toks, i, "endbfchar") {
                    let Some(src) = next_hex(&toks, &mut i) else {
                        break;
                    };
                    let Some(dst) = next_hex(&toks, &mut i) else {
                        break;
                    };
                    if let (Some(code), Some(units)) = (hex_to_u32(&src), hex_to_units(&dst)) {
                        let byte_len = src.len().div_ceil(2).clamp(1, 4) as u8;
                        cm.exact.insert((byte_len, code), units);
                    }
                }
                while i < toks.len() && !is_end(&toks, i, "endbfchar") {
                    i += 1;
                }
                i += 1;
            }
            "beginbfrange" => {
                i += 1;
                while i < toks.len() && !is_end(&toks, i, "endbfrange") {
                    let Some(lo_h) = next_hex(&toks, &mut i) else {
                        break;
                    };
                    let Some(hi_h) = next_hex(&toks, &mut i) else {
                        break;
                    };
                    let (Some(lo), Some(hi)) = (hex_to_u32(&lo_h), hex_to_u32(&hi_h)) else {
                        break;
                    };
                    if i >= toks.len() {
                        break;
                    }
                    let byte_len = lo_h.len().div_ceil(2).clamp(1, 4) as u8;
                    match &toks[i] {
                        Tok::Hex(dst) => {
                            // Single incrementing destination.
                            if let Some(d0) = hex_to_u32(dst) {
                                cm.ranges.push((byte_len, lo, hi, d0));
                            }
                            i += 1;
                        }
                        Tok::Word(w) if w == "[" => {
                            i += 1;
                            let mut vals = Vec::new();
                            while i < toks.len() {
                                match &toks[i] {
                                    Tok::Word(w2) if w2 == "]" => {
                                        i += 1;
                                        break;
                                    }
                                    Tok::Hex(h) => {
                                        if let Some(units) = hex_to_units(h) {
                                            vals.push(units);
                                        }
                                        i += 1;
                                    }
                                    _ => i += 1,
                                }
                            }
                            for (off, units) in vals.into_iter().enumerate() {
                                let code = lo + off as u32;
                                if code <= hi {
                                    cm.exact.insert((byte_len, code), units);
                                }
                            }
                        }
                        _ => i += 1,
                    }
                }
                while i < toks.len() && !is_end(&toks, i, "endbfrange") {
                    i += 1;
                }
                i += 1;
            }
            _ => i += 1,
        }
    }
    Some(cm)
}

pub(super) fn cmap_tokens(data: &[u8]) -> Vec<Tok> {
    let mut toks = Vec::new();
    let mut i = 0usize;
    while i < data.len() {
        let c = data[i];
        match c {
            b'%' => {
                while i < data.len() && data[i] != b'\n' {
                    i += 1;
                }
            }
            b' ' | b'\t' | b'\r' | b'\n' | 0 => i += 1,
            b'<' => {
                let start = i + 1;
                let mut end = start;
                while end < data.len() && data[end] != b'>' {
                    end += 1;
                }
                // Whitespace is legal inside a PDF hex string (`< 0041 >`) and
                // is ignored; keeping it makes every numeric parse fail and the
                // whole `bfchar` entry silently drop.
                let hex: Vec<u8> = data[start..end]
                    .iter()
                    .copied()
                    .filter(|b| !b.is_ascii_whitespace())
                    .collect();
                i = end + 1;
                toks.push(Tok::Hex(hex));
            }
            b'[' | b']' => {
                let ch = c as char;
                toks.push(Tok::Word(ch.to_string()));
                i += 1;
            }
            _ if c.is_ascii_alphanumeric() => {
                let start = i;
                while i < data.len() && data[i].is_ascii_alphanumeric() {
                    i += 1;
                }
                toks.push(Tok::Word(
                    String::from_utf8_lossy(&data[start..i]).into_owned(),
                ));
            }
            _ => i += 1,
        }
    }
    toks
}

pub(super) fn hex_to_u32(hex: &[u8]) -> Option<u32> {
    if hex.is_empty() {
        return None;
    }
    let mut v: u32 = 0;
    for &h in hex {
        let d = (h as char).to_digit(16)?;
        v = (v << 4).checked_add(d)?;
    }
    Some(v)
}

pub(super) fn hex_to_units(hex: &[u8]) -> Option<Vec<u16>> {
    if hex.is_empty() {
        return None;
    }
    let mut digits: Vec<u8> = hex
        .iter()
        .map(|&h| (h as char).to_digit(16).map(|d| d as u8))
        .collect::<Option<_>>()?;
    // Pad on the left so each unit is exactly 4 hex digits.
    while digits.len() % 4 != 0 {
        digits.insert(0, 0);
    }
    let mut units = Vec::with_capacity(digits.len() / 4);
    for chunk in digits.chunks(4) {
        let v = ((chunk[0] as u16) << 12)
            | ((chunk[1] as u16) << 8)
            | ((chunk[2] as u16) << 4)
            | chunk[3] as u16;
        units.push(v);
    }
    Some(units)
}

// ---------------------------------------------------------------------------
// Font encoding resolution
// ---------------------------------------------------------------------------
