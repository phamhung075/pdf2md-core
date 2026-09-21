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

use std::collections::{BTreeMap, HashMap};

use lopdf::content::{Content, Operation};
use lopdf::{Dictionary, Document, Object, ObjectId};

use crate::glyph_data::{
    pua_family_for_base_font, pua_to_char_for_family, AGL_NAMES, MAC_ROMAN, WIN_ANSI, PuaFamily,
};
use crate::layout::glyph_stream::{resolve_widths, Widths};
use crate::layout::reading_order::WORD_GAP_EM;

// ---------------------------------------------------------------------------
// Codecs
// ---------------------------------------------------------------------------

/// Decoded byte-oriented (8-bit simple font) encoding table.
/// `0` means "this char code has no Unicode mapping" (skip).
#[derive(Clone, Copy)]
pub(crate) struct ByteTable([u16; 256]);

/// A parsed `/ToUnicode` CMap: 1-4 byte source codes -> UTF-16 destinations.
#[derive(Clone, Default)]
pub(crate) struct CMapCodec {
    /// Exact single mappings: (source code length in bytes, source code) -> UTF-16.
    exact: HashMap<(u8, u32), Vec<u16>>,
    /// bfrange with a single incrementing destination: (len, lo, hi, dst_lo);
    /// destination for code = dst_lo + (code - lo).
    ranges: Vec<(u8, u32, u32, u32)>,
}

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

#[derive(Clone)]
pub(crate) enum Codec {
    Byte8(ByteTable, PuaFamily),
    /// ToUnicode CMap, with an optional byte-table fallback for simple fonts
    /// whose CMap does not cover the actual char codes used on the page.
    CMap(CMapCodec, Option<ByteTable>, PuaFamily),
}

/// True for a Unicode private-use code point. An unmapped one is mojibake, not
/// text: it must never be emitted verbatim.
fn is_private_use_char(c: char) -> bool {
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
fn push_mapped(out: &mut String, c: char, family: Option<PuaFamily>) {
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

fn push_utf16(out: &mut String, units: &[u16], family: Option<PuaFamily>) {
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

fn decode_byte_table(t: &ByteTable, bytes: &[u8], out: &mut String, family: Option<PuaFamily>) {
    for &b in bytes {
        let u = t.0[b as usize];
        if u != 0 {
            if let Some(ch) = char::from_u32(u as u32) {
                push_mapped(out, ch, family);
            }
        }
    }
}

fn decode_cmap(
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

enum Tok {
    Hex(Vec<u8>), // hex nibble characters
    Word(String),
}

fn cmap_tokens(data: &[u8]) -> Vec<Tok> {
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

fn hex_to_u32(hex: &[u8]) -> Option<u32> {
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

fn hex_to_units(hex: &[u8]) -> Option<Vec<u16>> {
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

pub(crate) fn get_name<'a>(dict: &'a Dictionary, key: &[u8]) -> Option<&'a [u8]> {
    dict.get(key).ok().and_then(|o| o.as_name().ok())
}

/// Follow indirect references to the underlying object.
pub(crate) fn deref<'d>(doc: &'d Document, obj: &'d Object) -> Option<&'d Object> {
    let mut cur = obj;
    for _ in 0..16 {
        match cur {
            Object::Reference(id) => match doc.get_object(*id) {
                Ok(next) => cur = next,
                Err(_) => return None,
            },
            _ => return Some(cur),
        }
    }
    None
}

/// The `/Resources` dictionaries that apply to a page, nearest first (the page
/// itself, then each `/Pages` ancestor up to the root). `lopdf`'s
/// `get_page_resources` only collects ancestor resources that are *indirect*
/// references, so a page whose parent carries `/Resources << ... >>` inline
/// (QZP payslips, some bank exports) resolves to nothing; walking the chain
/// ourselves handles both shapes. Cycle-safe and depth-bounded.
pub(crate) fn resource_dicts<'a>(doc: &'a Document, page_id: ObjectId) -> Vec<&'a Dictionary> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut cur = Some(page_id);
    while let Some(id) = cur {
        if out.len() >= 32 || !seen.insert(id) {
            break;
        }
        let Ok(dict) = doc.get_dictionary(id) else { break };
        if let Some(res) = dict
            .get(b"Resources")
            .ok()
            .and_then(|o| deref(doc, o))
            .and_then(|o| o.as_dict().ok())
        {
            out.push(res);
        }
        cur = dict.get(b"Parent").ok().and_then(|o| o.as_reference().ok());
    }
    out
}

/// Merge the `/Font` entries of a resource chain into `fonts`, nearest-wins and
/// without overwriting a name already found closer to the page.
pub(crate) fn collect_fonts<'a>(
    doc: &'a Document,
    chain: &[&'a Dictionary],
    fonts: &mut BTreeMap<Vec<u8>, &'a Dictionary>,
) {
    for resources in chain {
        let Some(font) = resources
            .get(b"Font")
            .ok()
            .and_then(|o| deref(doc, o))
            .and_then(|o| o.as_dict().ok())
        else {
            continue;
        };
        for (name, value) in font.iter() {
            if fonts.contains_key(name) {
                continue;
            }
            if let Some(fd) = deref(doc, value).and_then(|o| o.as_dict().ok()) {
                fonts.insert(name.clone(), fd);
            }
        }
    }
}

/// Fonts available to a page, resolving `/Resources` inherited from the
/// `/Pages` tree (see [`resource_dicts`]).
pub(crate) fn page_fonts<'a>(
    doc: &'a Document,
    page_id: ObjectId,
) -> BTreeMap<Vec<u8>, &'a Dictionary> {
    let mut fonts = BTreeMap::new();
    let chain = resource_dicts(doc, page_id);
    collect_fonts(doc, &chain, &mut fonts);
    fonts
}

/// The `/Resources` dictionary in effect inside a Form XObject: its own when
/// present, otherwise the enclosing page/form resources.
pub(crate) fn form_resource_chain<'a>(
    doc: &'a Document,
    form: &'a Dictionary,
    parent: &[&'a Dictionary],
) -> Vec<&'a Dictionary> {
    match form
        .get(b"Resources")
        .ok()
        .and_then(|o| deref(doc, o))
        .and_then(|o| o.as_dict().ok())
    {
        Some(res) => vec![res],
        None => parent.to_vec(),
    }
}

/// Resolve a `Do` operand to a Form XObject stream, searching the resource
/// chain nearest-first. The returned id is `Some` when the XObject was an
/// indirect reference, letting callers detect a form that draws itself.
pub(crate) fn lookup_form<'a>(
    doc: &'a Document,
    chain: &[&'a Dictionary],
    name: &[u8],
) -> Option<(Option<ObjectId>, &'a lopdf::Stream)> {
    for resources in chain {
        let Some(xobjects) = resources
            .get(b"XObject")
            .ok()
            .and_then(|o| deref(doc, o))
            .and_then(|o| o.as_dict().ok())
        else {
            continue;
        };
        let Some(value) = xobjects.get(name).ok() else {
            continue;
        };
        let id = value.as_reference().ok();
        if let Some(Object::Stream(stream)) = deref(doc, value) {
            if get_name(&stream.dict, b"Subtype") == Some(b"Form") {
                return Some((id, stream));
            }
        }
    }
    None
}

/// ASCII-only base used for StandardEncoding / MacExpertEncoding /
/// PDFDocEncoding / unknown names: high bytes decode to nothing instead of to
/// corrupt letters (lopdf's fallback table maps 0xE9 -> 'Ø' etc.).
fn ascii_table() -> ByteTable {
    let mut t = [0u16; 256];
    for i in 0x20..0x7F {
        t[i] = i as u16;
    }
    ByteTable(t)
}

fn base_table(name: &[u8]) -> ByteTable {
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
fn ligature_name_unicode(name: &[u8]) -> Option<u16> {
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
fn agl_name_unicode(name: &[u8]) -> Option<u16> {
    if let Ok(idx) = AGL_NAMES.binary_search_by(|entry: &(&[u8], u16)| entry.0.cmp(name)) {
        return Some(AGL_NAMES[idx].1);
    }
    ligature_name_unicode(name)
}

/// Decode a `uniXXXX` / `uniXXXXXXXX…` byte string (UTF-16BE hex units) to a
/// single code point. A multi-unit sequence that spells a known ligature
/// (`uni00660069` = "fi") maps to its presentation-form code point so the
/// walker can fold it to letters; otherwise the first unit is kept.
fn uni_units_unicode(hex: &[u8]) -> Option<u16> {
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
fn underscore_name_unicode(name: &[u8]) -> Option<u16> {
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
fn glyph_unicode(name: &[u8]) -> Option<u16> {
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

fn apply_differences(table: &mut ByteTable, doc: &Document, array: &Object) {
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

/// True for the handful of faces whose byte codes index a built-in glyph set
/// with no Latin semantics (dingbats/dingbat-like fonts). A font merely flagged
/// Symbolic but named as a text face is *not* one of these.
fn is_dingbat_face(font: &Dictionary) -> bool {
    if let Some(bf) = get_name(font, b"BaseFont") {
        let upper = bf.to_ascii_uppercase();
        for marker in [
            b"SYMBOL".as_slice(),
            b"ZAPFDINGBATS",
            b"WINGDINGS",
            b"PICTS",
        ] {
            if upper.windows(marker.len()).any(|w| w == marker) {
                return true;
            }
        }
    }
    false
}

fn is_symbolic(doc: &Document, font: &Dictionary) -> bool {
    if let Some(desc) = font.get(b"FontDescriptor").ok().and_then(|o| deref(doc, o)) {
        if let Object::Dictionary(d) = desc {
            if let Ok(flags) = d.get(b"Flags").and_then(|f| f.as_i64()) {
                if flags & 0x4 != 0 {
                    return true;
                }
            }
        }
    }
    is_dingbat_face(font)
}

/// Extract the `/ToUnicode` CMap of a font (handles indirect references).
fn font_to_unicode(doc: &Document, font: &Dictionary) -> Option<CMapCodec> {
    let obj = font.get(b"ToUnicode").ok()?;
    let obj = deref(doc, obj)?;
    let stream = obj.as_stream().ok()?;
    let data = stream.get_plain_content_with_limit(16 << 20).ok()?;
    parse_cmap(&data)
}

pub(crate) fn resolve_codec(doc: &Document, font: &Dictionary) -> Option<Codec> {
    let subtype = get_name(font, b"Subtype").unwrap_or(b"");

    // PUA code points mean different glyphs in the Symbol and Wingdings
    // charsets, so carry the family (from `BaseFont`, or the descriptor's
    // `FontName`) into the codec and map them at decode time.
    let base_font = get_name(font, b"BaseFont")
        .or_else(|| {
            font.get(b"FontDescriptor")
                .ok()
                .and_then(|o| deref(doc, o))
                .and_then(|o| o.as_dict().ok())
                .and_then(|d| get_name(d, b"FontName"))
        })
        .unwrap_or(b"");
    let pua = pua_family_for_base_font(base_font);

    if subtype == b"Type0" {
        // CID-keyed font: decode exclusively through its ToUnicode CMap.
        return font_to_unicode(doc, font).map(|cm| Codec::CMap(cm, None, pua));
    }

    // Simple fonts: resolve the 8-bit /Encoding first (it also serves as the
    // fallback for a partial /ToUnicode CMap).
    let enc = font.get(b"Encoding").ok().and_then(|o| deref(doc, o));

    let table = match enc {
        Some(Object::Name(n)) => base_table(n),
        Some(Object::Dictionary(d)) => {
            let mut t = match d.get(b"BaseEncoding").ok().and_then(|o| deref(doc, o)) {
                Some(Object::Name(n)) => base_table(n),
                _ => ascii_table(),
            };
            if let Ok(diffs) = d.get(b"Differences") {
                apply_differences(&mut t, doc, diffs);
            }
            t
        }
        _ => {
            if is_symbolic(doc, font) {
                // Symbolic fonts (dingbats etc.) carry no recoverable textual
                // semantics from a standard byte encoding: their codes index
                // the font's built-in glyph set. An explicit /ToUnicode CMap,
                // however, *is* authoritative semantics — and TeX's CMR/CMMI
                // faces are routinely flagged Symbolic with no /Encoding while
                // shipping a full /ToUnicode. Decode through that rather than
                // dropping every glyph drawn in the font.
                if let Some(cm) = font_to_unicode(doc, font) {
                    return Some(Codec::CMap(cm, None, pua));
                }
                // A Latin text face merely flagged Symbolic (very common on
                // subset CFF/Type1 fonts, e.g. Bouygues bills) still draws
                // StandardEncoding-compatible bytes; decode those through the
                // Latin table instead of dropping the whole document. Only the
                // genuine dingbat faces are left unmapped so they escalate.
                if is_dingbat_face(font) {
                    return None;
                }
            }
            // Non-symbolic simple fonts with no declared encoding almost
            // universally use WinAnsi byte values for accented Latin text.
            ByteTable(WIN_ANSI)
        }
    };

    if let Some(cm) = font_to_unicode(doc, font) {
        return Some(Codec::CMap(cm, Some(table), pua));
    }
    Some(Codec::Byte8(table, pua))
}

// ---------------------------------------------------------------------------
// Page text walker
// ---------------------------------------------------------------------------

fn ends_with_ws(s: &str) -> bool {
    s.chars().next_back().map_or(true, |c| c.is_whitespace())
}

fn push_decoded(out: &mut String, codec: &Codec, bytes: &[u8]) {
    let mut decoded = String::new();
    codec.decode(bytes, &mut decoded);
    // Producers routinely pad every show-string with a leading and trailing
    // space (`( text )`). Appending that verbatim duplicates the separator at
    // a run boundary (`ÉLECTRONIQUE  RÉFÉRENCE`) and leaves a stray leading
    // space at the start of the page. Drop the incoming padding when the
    // buffer is empty or already ends in whitespace; keep a genuine word gap
    // (the string's own leading space) when it does not.
    if ends_with_ws(out) {
        out.push_str(decoded.trim_start());
    } else {
        out.push_str(&decoded);
    }
}

/// True when a `Td`/`TD` operand pair moves the text line vertically — the
/// same break `T*` performs. `tx ty Td` translates the text line matrix; a
/// nonzero `ty` starts a new line, while a horizontal-only `tx` move stays on
/// the current line. Threshold matches the `line_eps` used by coordinate mode.
fn td_advances_line(op: &Operation) -> bool {
    op.operands
        .get(1)
        .and_then(|o| o.as_float().ok())
        .map_or(false, |ty| ty.abs() > 0.5)
}

/// Start a new output line, discarding the padding a producer left at the end
/// of the previous show-string (`( text )` ends in a space). Without the trim
/// the trailing space keeps `ends_with_ws` true, so the break is skipped and
/// the next line fuses onto the current one.
fn break_line(out: &mut String) {
    while out.ends_with(' ') || out.ends_with('\t') {
        out.pop();
    }
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
}

/// Emit a paragraph break (blank line) when the next shown baseline is more
/// than one em *above* the previous shown line: that is a new column or block
/// starting at the top of the page, not a wrapped line, so a later markdown
/// reflow must not weld the two together. Does nothing when the position
/// tracker is unusable or the font size is unknown.
fn break_column_if_above(out: &mut String, tp: &TextPos) {
    let Some(prev) = tp.last_show_y else { return };
    if tp.unusable || tp.size <= 0.0 {
        return;
    }
    let shift = tp.cur_y - prev;
    let above = shift > tp.size;
    // A run that starts inside — or immediately after — the horizontal span
    // the previous run covered is continuing the same visual line, whatever
    // the baseline shift: an inline superscript / footnote mark, a subscript,
    // or a kerned fragment. Compare against where the previous text actually
    // ENDED (`last_end_x`), not where the line started. Only a run that starts
    // clear of that span can be a new column.
    let same_line = match tp.last_end_x {
        Some(end) => {
            let lo = tp.last_line_x.min(end);
            let hi = tp.last_line_x.max(end);
            tp.cur_x >= lo - tp.size && tp.cur_x <= hi + tp.size
        }
        // No previous show end known: be conservative and treat it as the
        // same line rather than inventing a break.
        None => true,
    };
    // A new column starts on the next visual row, so require a real vertical
    // move: at least half a line (a few points' rise is a super/subscript, not
    // a column). When the run is in a smaller face it must clear the full
    // 0.7 em threshold, since a small face a few points up is a footnote mark.
    let half_line = shift >= 0.5 * tp.size;
    let full_line = shift >= 0.7 * tp.size;
    let comparable_size = tp.last_show_size <= 0.0
        || (tp.size >= 0.75 * tp.last_show_size && tp.size <= 1.33 * tp.last_show_size);
    let sideways = half_line && !same_line && (full_line || comparable_size);
    if !(above || sideways) {
        return;
    }
    while out.ends_with(' ') || out.ends_with('\t') {
        out.pop();
    }
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    if !out.is_empty() && !out.ends_with("\n\n") {
        out.push('\n');
    }
}

/// Normalise decoded page text before it is rendered:
///
///   * fold the Latin presentation-form ligatures U+FB00–U+FB06 to their letter
///     sequences (the geometry engine already does this in `fold_ligatures`;
///     the string walker must too, since a `/ToUnicode` CMap may return the
///     ligature code point itself);
///   * drop soft hyphens (U+00AD) — invisible formatting, never text;
///   * turn a no-break space (U+00A0) or narrow no-break space (U+202F) into a
///     plain space when it sits next to a digit, the French thousands
///     separator (`1 234,56`). NBSPs elsewhere are left untouched;
///   * map a Microsoft Symbol/Wingdings private-use code point
///     (U+F020–U+F0FF) to its Unicode equivalent, and drop any other
///     private-use character (U+E000, U+F8FF, the supplementary planes): an
///     unmapped PUA glyph is mojibake, not text.
pub(crate) fn normalize_decoded_text(s: &str) -> String {
    if !s.chars().any(|c| {
        matches!(c, '\u{FB00}'..='\u{FB06}' | '\u{00AD}' | '\u{00A0}' | '\u{202F}')
            || is_private_use_char(c)
    }) {
        return s.to_string();
    }
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    for (i, &c) in chars.iter().enumerate() {
        match c {
            '\u{FB00}' => out.push_str("ff"),
            '\u{FB01}' => out.push_str("fi"),
            '\u{FB02}' => out.push_str("fl"),
            '\u{FB03}' => out.push_str("ffi"),
            '\u{FB04}' => out.push_str("ffl"),
            '\u{FB05}' | '\u{FB06}' => out.push_str("st"),
            '\u{00AD}' => {}
            '\u{00A0}' | '\u{202F}' => {
                let prev_digit = i > 0 && chars[i - 1].is_ascii_digit();
                let next_digit = chars.get(i + 1).map_or(false, |n| n.is_ascii_digit());
                if prev_digit || next_digit {
                    out.push(' ');
                } else {
                    out.push(c);
                }
            }
            c if is_private_use_char(c) => {
                // The font is unknown on this shared string chokepoint, so use
                // the corpus-verified/Symbol union: a mapped glyph survives; an
                // unmapped PUA character is dropped rather than emitted.
                if let Some(m) = crate::glyph_data::symbol_pua_to_char(c as u32) {
                    out.push(m);
                }
            }
            _ => out.push(c),
        }
    }
    out
}

/// Advance assumed for a glyph when the font has no usable `/Widths` (the
/// geometry engine assumes the same typical letter advance).
const FALLBACK_ADVANCE_EM: f64 = 0.5;

/// Text-space position tracker for the string walker.
///
/// Producers that position every word or fragment with a horizontal `Td` (and
/// no `TJ` array) encode word boundaries as gaps in the text matrix, not as
/// space glyphs. The walker previously ignored those positions, so every word
/// on such a line fused into one token. Tracking the text-space x/y lets
/// [`walker_gap`] insert a separator exactly when the next show starts more
/// than [`WORD_GAP_EM`] em past the previous show's natural end.
#[derive(Clone, Copy)]
struct TextPos {
    line_x: f64,
    line_y: f64,
    cur_x: f64,
    cur_y: f64,
    /// End x of the previous show on the current line (natural advance, before
    /// any separator the walker inserted). Cleared by a real line advance, so
    /// it is only used for word-gap inference.
    prev_end_x: Option<f64>,
    /// End x of the previous show, kept across line advances. The
    /// paragraph-break heuristic needs to know whether the next run continues
    /// the line the previous run drew, even when the next run is placed by a
    /// fresh `Td`/`Tm`.
    last_end_x: Option<f64>,
    /// Font size of the previous show, used to tell a super/subscript (a
    /// smaller face a few points up) from a same-size new column start.
    last_show_size: f64,
    /// Baseline y of the previous show.
    prev_y: f64,
    /// Baseline y of the last text show, across visual lines. A later show on a
    /// baseline more than one em *above* this one starts a new column/block, so
    /// the walker emits a paragraph break there (PDF y grows upward).
    last_show_y: Option<f64>,
    /// Text-space start x of the line the last show was on. A later line that
    /// starts well to the left/right of it *and* sits above the previous
    /// baseline is a new column even when the vertical jump is under one em.
    last_line_x: f64,
    size: f64,
    /// `Tz / 100`: horizontal glyph scaling.
    hscale: f64,
    /// `Tc` / `Tw`: character / word spacing, in text-space points.
    char_sp: f64,
    word_sp: f64,
    /// `TL`: leading used by `T*`.
    leading: f64,
    /// A `Tm` with a non-identity scale or rotation leaves text space
    /// unaligned with device points; the walker cannot compare advances then,
    /// so it stops inserting gaps for the rest of this content stream.
    unusable: bool,
}

impl TextPos {
    fn new() -> Self {
        Self {
            line_x: 0.0,
            line_y: 0.0,
            cur_x: 0.0,
            cur_y: 0.0,
            prev_end_x: None,
            last_end_x: None,
            last_show_size: 0.0,
            prev_y: 0.0,
            last_show_y: None,
            last_line_x: 0.0,
            size: 0.0,
            hscale: 1.0,
            char_sp: 0.0,
            word_sp: 0.0,
            leading: 0.0,
            unusable: false,
        }
    }

    /// Move the text matrix to the current line origin. `keep_gap` is true for
    /// a horizontal-only `Td`/`TD` (a fragment placement on the same line),
    /// which must not discard the previous show's end; any real line advance
    /// clears it so a gap is never measured across lines.
    fn goto_line(&mut self, keep_gap: bool) {
        self.cur_x = self.line_x;
        self.cur_y = self.line_y;
        if !keep_gap {
            self.prev_end_x = None;
        }
        self.prev_y = self.line_y;
    }
}

/// Natural advance, in device points, of one show operation (a `Tj` string, a
/// whole `TJ` array, or a single string operand). Sums each glyph's `/Widths`
/// entry and applies `Tc`/`Tw`/`Tz`, mirroring `glyph_stream`'s text-matrix
/// advance. A font with no usable metrics falls back to
/// [`FALLBACK_ADVANCE_EM`] per code.
fn show_advance(
    widths: &Widths,
    codec: &Codec,
    operands: &[Object],
    size: f64,
    char_sp: f64,
    word_sp: f64,
    hscale: f64,
) -> f64 {
    let mut w1000 = 0.0f64;
    let mut nchars = 0usize;
    let mut nspaces = 0usize;
    fn add_bytes(
        widths: &Widths,
        codec: &Codec,
        bytes: &[u8],
        w1000: &mut f64,
        nchars: &mut usize,
        nspaces: &mut usize,
    ) {
        // Count codes, not raw bytes: `Tc`/`Tw` are applied per character code,
        // and a 2-byte CID/Identity-H font's byte length is twice its glyph
        // count. The same number drives the no-metrics fallback advance.
        let (codes, spaces) = codec.code_metrics(bytes);
        match widths.width(bytes) {
            Some(w) => *w1000 += w,
            None => *w1000 += FALLBACK_ADVANCE_EM * 1000.0 * codes as f64,
        }
        *nchars += codes;
        *nspaces += spaces;
    }
    for operand in operands {
        match operand {
            Object::String(bytes, _) => {
                add_bytes(widths, codec, bytes, &mut w1000, &mut nchars, &mut nspaces)
            }
            Object::Array(items) => {
                for item in items {
                    match item {
                        Object::String(bytes, _) => {
                            add_bytes(widths, codec, bytes, &mut w1000, &mut nchars, &mut nspaces)
                        }
                        // A `TJ` number is a manual kern in 1/1000 em.
                        Object::Integer(v) => w1000 -= *v as f64,
                        Object::Real(v) => w1000 -= *v as f64,
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    (w1000 / 1000.0 * size + char_sp * nchars as f64 + word_sp * nspaces as f64) * hscale
}

/// Insert a word separator before a show when the text matrix has advanced more
/// than [`WORD_GAP_EM`] em past the previous show's natural end on the same
/// baseline. No separator is inserted across a line break (the tracker resets
/// `prev_end_x`) or when the output already ends in whitespace.
fn walker_gap(out: &mut String, tp: &TextPos) {
    if tp.unusable || tp.size <= 0.0 {
        return;
    }
    let Some(prev_end) = tp.prev_end_x else { return };
    if (tp.cur_y - tp.prev_y).abs() > 0.5 {
        return;
    }
    if tp.cur_x - prev_end > WORD_GAP_EM * tp.size && !ends_with_ws(out) {
        out.push(' ');
    }
}

/// Record the natural advance of a show that was just appended, so the next
/// show can be measured against it.
fn advance_after_show(
    tp: &mut TextPos,
    cur_width: Option<usize>,
    widths: &[(Vec<u8>, Widths)],
    codec: &Codec,
    operands: &[Object],
) {
    if let Some(wi) = cur_width {
        let adv = show_advance(
            &widths[wi].1,
            codec,
            operands,
            tp.size,
            tp.char_sp,
            tp.word_sp,
            tp.hscale,
        );
        tp.cur_x += adv;
        // Word-gap inference only measures within a visual line, so this is
        // cleared by a line move.
        tp.prev_end_x = Some(tp.cur_x);
    }
    // The paragraph-break rule must survive line moves: it asks whether the
    // next run continues the line the previous run drew, so it needs the end
    // even when the font metrics were unusable.
    tp.last_end_x = Some(tp.cur_x);
    tp.last_show_size = tp.size;
    tp.prev_y = tp.cur_y;
}

fn show_text(out: &mut String, codec: &Codec, operands: &[Object]) {
    for operand in operands {
        match operand {
            Object::String(bytes, _) => push_decoded(out, codec, bytes),
            Object::Array(items) => {
                for item in items {
                    match item {
                        Object::String(bytes, _) => push_decoded(out, codec, bytes),
                        // Large negative kerning behaves as a word gap. The
                        // kern may be an Integer or a Real in a `TJ` array.
                        Object::Integer(v) if *v < -100 => {
                            if !ends_with_ws(out) {
                                out.push(' ');
                            }
                        }
                        Object::Real(v) if *v < -100.0 => {
                            if !ends_with_ws(out) {
                                out.push(' ');
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
}

/// Page text result plus whether the page's content stream contains any
/// text-show operators (used to detect glyph-encoded / outlined documents
/// whose text cannot be recovered, so the caller can ask for OCR), plus how
/// many grid tables the geometry engine recovered on the page.
pub struct PageText {
    pub text: String,
    pub text_ops_seen: bool,
    pub has_fonts: bool,
    pub tables: usize,
    /// Structured human-reading-order blocks (geometry path only; empty on
    /// string-walker pages).
    pub blocks: Vec<crate::layout::DocBlock>,
    /// True when any extraction work bound (Form XObject `Do` count, shared
    /// operator/decoded-byte budget, recursion depth) tripped for this page, so
    /// the text may be truncated. See [`WalkerBudget`].
    pub budget_exhausted: bool,
}

/// Work / recursion bounds for Form XObject expansion in the string walker.
///
/// A form graph is a DAG at best and can be a near-exponential tree; each bound
/// below independently caps the blow-up, mirroring the glyph engine's
/// `GlyphBudget` (`layout::glyph_stream`) so both engines bound the same shape
/// identically. The `Do` and decoded-byte budgets are shared across the whole
/// page, so a DAG of forms that each issue `Do` many times cannot multiply the
/// work: when a budget is exhausted the walker simply stops descending and the
/// conversion continues with the text decoded so far (never an error).
pub(crate) const MAX_WALKER_FORM_DEPTH: usize = 8;
/// Maximum number of `Do` invocations expanded for one page (16384 = 2^14).
///
/// Rationale: this is a per-page *cost* backstop, not a content limit. Each
/// invocation is charged [`FORM_INVOCATION_OPS`] against the shared
/// [`MAX_WALKER_OPS_TOTAL`] (8 M-operator) budget, so 16384 tiny forms cost at
/// most ~0.5 M ops — comfortably inside the budget, which is why a real page
/// with a few thousand form placements (the qa-int2 `forms_distinct_*`
/// fixtures) converts in full. An op-heavy or byte-heavy fan-out is bounded by
/// the shared operator and `MAX_WALKER_BYTES_TOTAL` (64 MiB decoded) budgets
/// instead, and a self-referential or exponentially expanding form DAG is
/// independently bounded by [`MAX_WALKER_FORM_DEPTH`] plus the per-path cycle
/// check, so those shapes never reach this count in the first place. The cap
/// exists only to stop a pathological page of > 16384 trivial forms from
/// paying the per-invocation resource-chain cost without bound; when it (or any
/// other bound) trips, [`WalkerBudget::exhausted`] is set and the conversion
/// reports `budget_exhausted` rather than truncating silently.
pub(crate) const MAX_WALKER_DO_PER_PAGE: usize = 16384;
pub(crate) const MAX_WALKER_OPS_TOTAL: usize = 8_000_000;
pub(crate) const MAX_WALKER_BYTES_TOTAL: usize = 64 << 20;
/// Fixed operator-budget cost charged on top of a form's own decoded bytes and
/// operators for every `Do` invocation. Re-walking a form is not proportional
/// to its byte length: the walker resolves the resource chain and collects the
/// form's fonts on each visit. Charging that overhead makes the budget a true
/// cost measure, so a hostile fan-out that reuses a tiny form exhausts the
/// shared budget long before the invocation-count cap.
pub(crate) const FORM_INVOCATION_OPS: usize = 32;

/// Per-page work budget shared by every nested [`walk_content`] call, and by
/// the `page_has_text_layer`/`content_has_text_layer` probe in `lib.rs`.
pub(crate) struct WalkerBudget {
    pub(crate) do_left: usize,
    pub(crate) ops_left: usize,
    pub(crate) bytes_left: usize,
    /// Set when any bound above tripped, so a truncated page can be reported
    /// (as `budget_exhausted`) instead of failing silently.
    pub(crate) exhausted: bool,
}

impl WalkerBudget {
    pub(crate) fn new() -> Self {
        WalkerBudget {
            do_left: MAX_WALKER_DO_PER_PAGE,
            ops_left: MAX_WALKER_OPS_TOTAL,
            bytes_left: MAX_WALKER_BYTES_TOTAL,
            exhausted: false,
        }
    }
}

/// Walk a content stream (a page or a Form XObject) appending decoded text to
/// `out`. Fonts come from `chain`; `Do` recurses into Form XObjects so text
/// buried there — a common payslip/invoice generator shape — is not dropped.
/// Recursion is depth-, `Do`-, operator- and byte-bounded (shared per-page
/// [`WalkerBudget`]) and never revisits a form already on the current path, so
/// neither a self-referential form nor an exponentially expanding DAG can loop
/// or multiply work.
#[allow(clippy::too_many_arguments)]
fn walk_content(
    doc: &Document,
    chain: &[&Dictionary],
    ops: &[Operation],
    out: &mut String,
    text_ops_seen: &mut bool,
    form_path: &mut Vec<ObjectId>,
    depth: usize,
    budget: &mut WalkerBudget,
) {
    let mut fonts: BTreeMap<Vec<u8>, &Dictionary> = BTreeMap::new();
    collect_fonts(doc, chain, &mut fonts);
    let codecs: Vec<(Vec<u8>, Codec)> = fonts
        .iter()
        .filter_map(|(name, fd)| resolve_codec(doc, fd).map(|c| (name.clone(), c)))
        .collect();
    // Width tables for the same fonts, used only by the word-gap heuristic.
    let widths: Vec<(Vec<u8>, Widths)> = fonts
        .iter()
        .map(|(name, fd)| (name.clone(), resolve_widths(doc, fd)))
        .collect();

    let mut cur: Option<usize> = None;
    let mut cur_width: Option<usize> = None;
    let mut tp = TextPos::new();
    // `q`/`Q` save and restore the graphics state, which includes the text
    // state (`Tf` font/size and `Tc`/`Tw`/`Tz`/`TL`). Without this, a `Q` after
    // a table cell or form left the previous cell's font selected for the text
    // that follows. Stack depth is bounded so a malformed stream cannot grow it
    // without limit.
    let mut gstate: Vec<(Option<usize>, Option<usize>, TextPos)> = Vec::new();

    // Position-aware reconstruction for "absolute" layout producers (form
    // generators, table tools, print drivers) that place one glyph per block
    // with explicit `Tm`/`TD` coordinates and a `TJ` array (no spaces in the
    // strings). There word/line boundaries must be inferred from coordinates: a
    // baseline `y` change is a line break, a large `x` gap on the same baseline
    // is a separator, a small gap means "join". This mode is only engaged when
    // the page actually mixes absolute positioning (`Tm`/`TD`) with `TJ`
    // arrays. Ordinary documents that draw whole-line strings with `Tj` and
    // relative `Td` moves (EDF/Enedis, tickets, books) keep the simple
    // heuristic — spaces come from the strings, line breaks from `ET`/`T*` — so
    // coordinate noise never breaks them.
    let has_tj_array = ops.iter().any(|op| op.operator == "TJ");
    let has_abs_pos = ops
        .iter()
        .any(|op| matches!(op.operator.as_str(), "Tm" | "TD"));
    let pos_mode = has_tj_array && has_abs_pos;
    let line_eps = 0.5; // points: a y jump larger than this starts a new line
    let space_eps = 1.0; // points: an x gap larger than this on the same line is a separator
    let mut cur_pos: Option<(f64, f64)> = None;
    let mut prev_pos: Option<(f64, f64)> = None;
    let mut last_pos_show = false;

    fn pos2(op: &Operation, i: usize) -> Option<(f64, f64)> {
        let a = op.operands.get(i)?.as_float().ok()? as f64;
        let b = op.operands.get(i + 1)?.as_float().ok()? as f64;
        Some((a, b))
    }

    // Emit one text-show with optional coordinate-driven spacing/preceding
    // newline. Sets `last_pos_show` so the following `ET` does not double the
    // line break.
    fn show_pos(
        out: &mut String,
        codec: &Codec,
        operands: &[Object],
        pos_mode: bool,
        line_eps: f64,
        space_eps: f64,
        cur_pos: &mut Option<(f64, f64)>,
        prev_pos: &mut Option<(f64, f64)>,
        last_pos_show: &mut bool,
    ) {
        if pos_mode {
            // In coordinate mode spacing is decided by the y/x gaps alone, so
            // the following `ET`/`T*` must never add a second line break.
            *last_pos_show = true;
            if let (Some((px, py)), Some((cx, cy))) = (*prev_pos, *cur_pos) {
                if (cy - py).abs() > line_eps {
                    if !ends_with_ws(out) {
                        out.push('\n');
                    }
                } else if (cx - px).abs() > space_eps {
                    if !ends_with_ws(out) {
                        out.push(' ');
                    }
                }
            }
            show_text(out, codec, operands);
            *prev_pos = *cur_pos;
        } else {
            *last_pos_show = false;
            show_text(out, codec, operands);
        }
    }

    for op in ops {
        if budget.ops_left == 0 {
            budget.exhausted = true;
            break;
        }
        budget.ops_left -= 1;
        match op.operator.as_str() {
            "q" => {
                if gstate.len() < 64 {
                    gstate.push((cur, cur_width, tp));
                }
            }
            "Q" => {
                if let Some((c, cw, t)) = gstate.pop() {
                    cur = c;
                    cur_width = cw;
                    tp = t;
                }
            }
            "Tf" => {
                let name = op.operands.first().and_then(|o| o.as_name().ok());
                cur = name.and_then(|nm| codecs.iter().position(|(n, _)| n == nm));
                cur_width = name.and_then(|nm| widths.iter().position(|(n, _)| n == nm));
                if let Some(sz) = op.operands.get(1).and_then(|o| o.as_float().ok()) {
                    tp.size = sz as f64;
                }
            }
            "Tm" => {
                cur_pos = pos2(op, 4);
                // Track the text-space origin for the walker gap heuristic. A
                // scaled/rotated matrix puts text space and device points in
                // different units, so gap inference is disabled for the rest of
                // this stream rather than risk a wrong separator.
                if let Some((x, y)) = pos2(op, 4) {
                    let a = op.operands.first().and_then(|o| o.as_float().ok());
                    let b = op.operands.get(1).and_then(|o| o.as_float().ok());
                    let c = op.operands.get(2).and_then(|o| o.as_float().ok());
                    let d = op.operands.get(3).and_then(|o| o.as_float().ok());
                    let identity = matches!((a, b, c, d),
                        (Some(a), Some(b), Some(c), Some(d))
                            if (a - 1.0).abs() <= 1e-3
                                && b.abs() <= 1e-3
                                && c.abs() <= 1e-3
                                && (d.abs() - 1.0).abs() <= 1e-3);
                    if identity {
                        tp.line_x = x;
                        tp.line_y = y;
                        tp.goto_line(false);
                    } else {
                        tp.unusable = true;
                    }
                }
            }
            "TD" => {
                // `TD` is `Td` plus a leading update. In the string-walker
                // case a vertical move is a line advance even when the
                // producer emits no `ET`/`T*` between the lines (see `Td`).
                if !pos_mode && td_advances_line(op) {
                    break_line(out);
                }
                cur_pos = pos2(op, 0);
                if let Some((tx, ty)) = pos2(op, 0) {
                    let same_line = !td_advances_line(op);
                    tp.line_x += tx;
                    tp.line_y += ty;
                    tp.leading = -ty;
                    tp.goto_line(same_line);
                }
            }
            "Td" => {
                // `Td` translates the text line. Producers that draw each line
                // with `Tj` + `0 -N Td` inside a single `BT`/`ET` (instead of
                // an `ET`/`T*` per line) previously had every visual line
                // fused into one run: `synth_ticket_compressed.pdf` emitted a
                // single paragraph with doubled spaces between the four
                // source lines.
                if !pos_mode && td_advances_line(op) {
                    break_line(out);
                }
                if let Some((tx, ty)) = pos2(op, 0) {
                    let same_line = !td_advances_line(op);
                    tp.line_x += tx;
                    tp.line_y += ty;
                    tp.goto_line(same_line);
                }
            }
            "BT" => {
                // `BT` resets the text matrix (not the rest of the text
                // state), so the next line starts at the origin until `Tm`/`Td`
                // places it.
                tp.line_x = 0.0;
                tp.line_y = 0.0;
                tp.goto_line(false);
            }
            "TL" => {
                if let Some(l) = op.operands.first().and_then(|o| o.as_float().ok()) {
                    tp.leading = l as f64;
                }
            }
            "Tz" => {
                if let Some(v) = op.operands.first().and_then(|o| o.as_float().ok()) {
                    tp.hscale = v as f64 / 100.0;
                }
            }
            "Tc" => {
                if let Some(v) = op.operands.first().and_then(|o| o.as_float().ok()) {
                    tp.char_sp = v as f64;
                }
            }
            "Tw" => {
                if let Some(v) = op.operands.first().and_then(|o| o.as_float().ok()) {
                    tp.word_sp = v as f64;
                }
            }
            "Tj" | "TJ" => {
                *text_ops_seen = true;
                if let Some(ci) = cur {
                    if pos_mode {
                        show_pos(
                            out,
                            &codecs[ci].1,
                            &op.operands,
                            pos_mode,
                            line_eps,
                            space_eps,
                            &mut cur_pos,
                            &mut prev_pos,
                            &mut last_pos_show,
                        );
                    } else {
                        last_pos_show = false;
                        walker_gap(out, &tp);
                        break_column_if_above(out, &tp);
                        show_text(out, &codecs[ci].1, &op.operands);
                        tp.last_show_y = Some(tp.cur_y); tp.last_line_x = tp.line_x;
                        advance_after_show(&mut tp, cur_width, &widths, &codecs[ci].1, &op.operands);
                    }
                }
            }
            "'" => {
                *text_ops_seen = true;
                last_pos_show = false;
                if !ends_with_ws(out) {
                    out.push('\n');
                }
                tp.line_y -= tp.leading;
                tp.goto_line(false);
                if let Some(ci) = cur {
                    break_column_if_above(out, &tp);
                    show_text(out, &codecs[ci].1, &op.operands);
                    tp.last_show_y = Some(tp.cur_y); tp.last_line_x = tp.line_x;
                    advance_after_show(&mut tp, cur_width, &widths, &codecs[ci].1, &op.operands);
                }
            }
            "\"" => {
                *text_ops_seen = true;
                last_pos_show = false;
                if !ends_with_ws(out) {
                    out.push('\n');
                }
                // `aw ac string "` sets word/char spacing before showing.
                if let Some(v) = op.operands.first().and_then(|o| o.as_float().ok()) {
                    tp.word_sp = v as f64;
                }
                if let Some(v) = op.operands.get(1).and_then(|o| o.as_float().ok()) {
                    tp.char_sp = v as f64;
                }
                tp.line_y -= tp.leading;
                tp.goto_line(false);
                if let Some(ci) = cur {
                    if let Some(s) = op.operands.get(2) {
                        break_column_if_above(out, &tp);
                        let one = std::slice::from_ref(s);
                        show_text(out, &codecs[ci].1, one);
                        tp.last_show_y = Some(tp.cur_y); tp.last_line_x = tp.line_x;
                        advance_after_show(&mut tp, cur_width, &widths, &codecs[ci].1, one);
                    }
                }
            }
            // `ET`/`T*` mark line breaks for the string-based case. When the
            // previous show was a positionally-placed glyph, the y-jump already
            // produced the break; suppress the extra newline.
            "T*" | "ET" => {
                if !last_pos_show && !ends_with_ws(out) {
                    out.push('\n');
                }
                if op.operator.as_str() == "T*" {
                    prev_pos = None;
                    tp.line_y -= tp.leading;
                    tp.goto_line(false);
                }
            }
            // Form XObject text: pages whose only content is `/x Do` (some
            // payslip/invoice generators) would otherwise extract nothing.
            "Do" => {
                if budget.do_left == 0 || depth >= MAX_WALKER_FORM_DEPTH {
                    budget.exhausted = true;
                    continue;
                }
                let Some(name) = op.operands.first().and_then(|o| o.as_name().ok()) else {
                    continue;
                };
                let Some((id, form)) = lookup_form(doc, chain, name) else {
                    continue;
                };
                // A form already on the current path draws itself (directly or
                // through a chain); the depth cap alone would still terminate,
                // but this stops the wasted re-walk.
                if id.map_or(false, |id| form_path.contains(&id)) {
                    continue;
                }
                let fchain = form_resource_chain(doc, &form.dict, chain);
                let Ok(data) = form.get_plain_content_with_limit(16 << 20) else {
                    continue;
                };
                if data.len() > budget.bytes_left {
                    budget.exhausted = true;
                    continue;
                }
                let Ok(fc) = Content::decode(&data) else {
                    continue;
                };
                budget.bytes_left = budget.bytes_left.saturating_sub(data.len());
                budget.ops_left = budget.ops_left.saturating_sub(FORM_INVOCATION_OPS);
                if budget.ops_left == 0 {
                    budget.exhausted = true;
                }
                budget.do_left -= 1;
                if let Some(id) = id {
                    form_path.push(id);
                }
                walk_content(
                    doc,
                    &fchain,
                    &fc.operations,
                    out,
                    text_ops_seen,
                    form_path,
                    depth + 1,
                    budget,
                );
                if id.is_some() {
                    form_path.pop();
                }
            }
            _ => {}
        }
    }
}

/// Largest decoded size of a single page content stream (32 MiB). A content
/// stream is a short list of drawing/text operators — orders of magnitude
/// smaller in any real document — so a stream that inflates past this is a
/// decompression bomb, not content we can use.
pub(crate) const MAX_PAGE_CONTENT_STREAM: usize = 32 << 20;
/// Largest total decoded content of one page (64 MiB), summed over its streams.
pub(crate) const MAX_PAGE_CONTENT_TOTAL: usize = 64 << 20;

/// Decode a page's content streams under explicit decompression caps.
///
/// Bounded replacement for `Document::get_and_decode_page_content`, whose
/// single-stream decode is unbounded: a ~1 MiB Flate page stream can inflate to
/// gigabytes. A page over either cap is rejected with a decompression error —
/// the callers already treat an undecodable page as "no text" — never truncated
/// mid-operator into garbage. A stream that fails for any other reason keeps
/// lopdf's lenient fallback to its raw bytes, still within the page budget.
pub(crate) fn decode_page_content(
    doc: &Document,
    page_id: ObjectId,
) -> lopdf::Result<Content<Vec<Operation>>> {
    let limit_err = || {
        lopdf::Error::Decompress(lopdf::DecompressError::MemoryLimitExceeded {
            limit: MAX_PAGE_CONTENT_TOTAL,
        })
    };
    let mut data = Vec::new();
    for object_id in doc.get_page_contents(page_id) {
        let Ok(stream) = doc.get_object(object_id).and_then(Object::as_stream) else {
            continue;
        };
        let remaining = MAX_PAGE_CONTENT_TOTAL.saturating_sub(data.len());
        let budget = remaining.min(MAX_PAGE_CONTENT_STREAM);
        match stream.get_plain_content_with_limit(budget) {
            Ok(bytes) => data.extend_from_slice(&bytes),
            Err(lopdf::Error::Decompress(lopdf::DecompressError::MemoryLimitExceeded {
                ..
            })) => {
                return Err(limit_err());
            }
            Err(_) => {
                if stream.content.len() > budget {
                    return Err(limit_err());
                }
                data.extend_from_slice(&stream.content);
            }
        }
        data.push(b'\n');
    }
    Content::decode(&data)
}

fn extract_page(
    doc: &Document,
    page_id: ObjectId,
    detect_tables: bool,
    detect_layout: bool,
    detect_math: bool,
) -> Result<(PageText, Option<bool>), String> {
    let chain = resource_dicts(doc, page_id);
    let mut fonts: BTreeMap<Vec<u8>, &Dictionary> = BTreeMap::new();
    collect_fonts(doc, &chain, &mut fonts);
    let has_fonts = !fonts.is_empty();

    let content: Content<Vec<Operation>> =
        decode_page_content(doc, page_id).map_err(|e| format!("{e}"))?;

    // Glyph-positioned pages (one glyph per text object with absolute
    // coordinates, e.g. LibreOffice forms) need a geometry engine — word
    // boundaries are encoded as inter-glyph gaps, not as space glyphs, and the
    // content stream is not in reading order. Delegate those to `layout`.
    //
    // Signature: `TJ` arrays only (no plain `Tj` strings) **and** per-glyph
    // `TD` positioning. Docs that use `Tm` with multi-string `TJ` arrays
    // (e.g. some Enedis bills) still extract correctly through the string
    // walker, so they must not be re-routed.
    //
    // Table recovery is the exception: Enedis-style notes place every cell
    // (and every word) at an absolute `Tm` position, so the geometry engine
    // can reconstruct their grids while the string walker flattens them. When
    // `detect_tables` is on we therefore also route `TJ`-only pages that
    // position with `Tm` (no `TD` needed) through the geometry engine, which
    // recovers reading order *and* Stage-3 grids. Without `detect_tables` the
    // string walker is kept so table-less callers stay byte-identical.
    // Page-stream-only signals. These decide the pre-existing *legacy*
    // geometry routing and must stay byte-for-byte equivalent for pages HEAD
    // already routed.
    let has_tj = content.operations.iter().any(|op| op.operator == "TJ");
    let has_tj_plain = content.operations.iter().any(|op| op.operator == "Tj");
    // `'` / `"` show a string and advance to the next line; some statement
    // generators position every fragment with them (plus `Td`) and no `Tj`/`TJ`.
    let has_quote = content
        .operations
        .iter()
        .any(|op| op.operator == "'" || op.operator == "\"");
    let has_td = content.operations.iter().any(|op| op.operator == "TD");
    // `Td` (lowercase) is the ordinary line/positioning operator. Some
    // producers (CAF payslips, Engie bills) position every fragment with
    // `Tj` + `Td` and no `Tm`/`TD`, so the old check missed them and they
    // stayed in the string walker — no table geometry and content-stream
    // reading order. Route those only when the `Td` moves are *fragment*
    // placements (most carry a horizontal component), not the vertical-only
    // line advances of ordinary one-BT-per-line prose (tickets, books), whose
    // string-walker output must stay byte-identical. The table-less path keeps
    // its exact legacy signature.
    let has_plain_td = {
        let (mut total, mut horizontal) = (0usize, 0usize);
        for op in &content.operations {
            if op.operator == "Td" {
                total += 1;
                if let Some(tx) = op.operands.first().and_then(|o| o.as_float().ok()) {
                    if tx.abs() > 0.01 {
                        horizontal += 1;
                    }
                }
            }
        }
        total >= 2 && horizontal * 2 >= total
    };
    let has_tm = content.operations.iter().any(|op| op.operator == "Tm");
    // Legacy geometry routing (a `TJ`/`Tj` page positioned with `TD`/`Tm`);
    // this is the pre-existing signature and is never re-checked below.
    let legacy_geometry = (has_tj || has_tj_plain) && (has_td || has_tm);

    // B1: text and positioning operators *inside Form XObjects* must count
    // too. A page whose content stream is just `/Fm0 Do` has none of the
    // page-stream flags above, so it went to the string walker, which recurses
    // into forms and produces text but never builds table geometry
    // (corpus files/corpus files/a corpus file). Keep these form-aware flags separate
    // from `legacy_geometry` so the newly routed pages still hit the
    // walker-vs-glyph digit-loss fallback below.
    let sig = crate::layout::glyph_stream::page_content_signals(doc, page_id);
    // The routing signal scan shares the same family of bounds as the walkers;
    // a page whose signal scan was truncated may have been misrouted, so carry
    // that fact into the page result too.
    let signals_exhausted = sig.budget_exhausted;
    let form_geometry =
        (sig.has_tj_array || sig.has_tj_plain) && (sig.has_td_upper || sig.has_tm);
    let form_plain_td = sig.td_total >= 2 && sig.td_horizontal * 2 >= sig.td_total;
    let route_to_layout = if detect_tables {
        // `Tj` positioned with `Td` (CAF payslips, Engie bills) and quote
        // show-ops (`'`/`"`). A quote page is routed only when it also carries
        // explicit `Td` fragment placements: a page that shows every string
        // with `'` at one `Tm` and zero leading is one-string-per-line prose
        // the geometry path would merge into a single run, so it stays on the
        // string walker.
        legacy_geometry
            || ((has_tj_plain || has_quote) && has_plain_td)
            || (form_geometry && !legacy_geometry)
            || ((sig.has_tj_plain || sig.has_quote) && form_plain_td)
    } else {
        has_tj && !has_tj_plain && has_td
    };
    // The `Tj`+`Td` / quote triggers were added by the layout batch. The
    // geometry engine can drop text held in rotated or Form-XObject content
    // that the string walker reaches, so those newly-routed pages are checked
    // against the walker before their output is trusted.
    let new_geometry = route_to_layout && !legacy_geometry && detect_tables;
    if route_to_layout {
        if new_geometry {
            let mut walker = String::new();
            let mut walker_ops = false;
            let mut form_path: Vec<ObjectId> = Vec::new();
            let mut budget = WalkerBudget::new();
            walk_content(
                doc,
                &chain,
                &content.operations,
                &mut walker,
                &mut walker_ops,
                &mut form_path,
                0,
                &mut budget,
            );
            let walker = walker.trim_end().to_string();
            let marker_hint = Some(first_line_looks_like_heading(&walker));
            match crate::layout::extract_page_glyphs(
                doc,
                page_id,
                detect_tables,
                detect_layout,
                detect_math,
            ) {
                Ok(pt) => {
                    // No table recovered, or a decoded digit run is missing:
                    // the walker is the lossless output, so keep it (tables
                    // are not worth unique content).
                    if pt.tables == 0 || loses_digit_content(&walker, &pt.text) {
                        return Ok((
                            PageText {
                                text: walker,
                                text_ops_seen: walker_ops,
                                has_fonts,
                                tables: 0,
                                blocks: Vec::new(),
                                budget_exhausted: budget.exhausted || signals_exhausted,
                            },
                            marker_hint,
                        ));
                    }
                    return Ok((
                        PageText {
                            budget_exhausted: pt.budget_exhausted || signals_exhausted,
                            ..pt
                        },
                        marker_hint,
                    ));
                }
                Err(e) => return Err(e),
            }
        }
        return match crate::layout::extract_page_glyphs(
            doc,
            page_id,
            detect_tables,
            detect_layout,
            detect_math,
        ) {
            Ok(pt) => {
                // The geometry engine classifies a page drawn under a rotated
                // CTM (e.g. landscape content rotated 90 degrees) as vertical
                // text; with layout analysis on it keeps those spans in
                // structured blocks only and leaves `text` empty, so the page
                // previously failed with "no readable words" even though its
                // `/ToUnicode` map is fine (a corpus file). The same drop can leave a
                // single stray word in `text` while the blocks hold the rest
                // (a corpus file). The string walker decodes the same fonts and is the
                // lossless output here, so fall back to it instead of reporting
                // a native failure or returning the fragment.
                let text_words = pt.text.split_whitespace().count();
                let block_words: usize = pt
                    .blocks
                    .iter()
                    .map(|b| b.text.split_whitespace().count())
                    .sum();
                if text_words >= 2 || (text_words > 0 && block_words < 5) {
                    return Ok((
                        PageText {
                            budget_exhausted: pt.budget_exhausted || signals_exhausted,
                            ..pt
                        },
                        None,
                    ));
                }
                // Keep the page-marker decision the geometry path made, so a
                // fallback page does not lose the `## Page N` marker the
                // non-fallback path would have emitted.
                let marker_hint = Some(first_line_looks_like_heading(&pt.text));
                let mut out = String::new();
                let mut ops_seen = false;
                let mut form_path: Vec<ObjectId> = Vec::new();
                let mut budget = WalkerBudget::new();
                walk_content(
                    doc,
                    &chain,
                    &content.operations,
                    &mut out,
                    &mut ops_seen,
                    &mut form_path,
                    0,
                    &mut budget,
                );
                let out = out.trim_end().to_string();
                if out.split_whitespace().count() <= text_words {
                    return Ok((
                        PageText {
                            budget_exhausted: pt.budget_exhausted || signals_exhausted,
                            ..pt
                        },
                        None,
                    ));
                }
                Ok((
                    PageText {
                        text: out,
                        text_ops_seen: ops_seen,
                        has_fonts,
                        tables: 0,
                        blocks: Vec::new(),
                        budget_exhausted: budget.exhausted || signals_exhausted,
                    },
                    marker_hint,
                ))
            }
            Err(e) => Err(e),
        };
    }

    let mut out = String::new();
    let mut text_ops_seen = false;
    let mut form_path: Vec<ObjectId> = Vec::new();
    let mut budget = WalkerBudget::new();
    walk_content(
        doc,
        &chain,
        &content.operations,
        &mut out,
        &mut text_ops_seen,
        &mut form_path,
        0,
        &mut budget,
    );

    Ok((
        PageText {
            text: out.trim_end().to_string(),
            text_ops_seen,
            has_fonts,
            tables: 0,
            blocks: Vec::new(),
            budget_exhausted: budget.exhausted || signals_exhausted,
        },
        None,
    ))
}

/// True when the geometry output dropped a numeric value (>= 3 digits) that the
/// string walker decoded. Compared as a substring of the output's concatenated
/// digits so re-tokenisation (`1 234` → `1234`) is not mistaken for a loss.
///
/// The walker side must be tokenised the same way as the decoder: a grouped
/// value (`12 345`, `1 234,56`, `12.345,67`) is one number whose digits are
/// contiguous after the group separators are removed. Splitting on *every*
/// non-digit would check `12` and `345` independently, so a glyph page that
/// still carried those two fragments separately would pass the guard even
/// though the decoded value was gone (a corpus file 6-digit, a corpus file amount-dot-2dec).
fn loses_digit_content(walker: &str, layout: &str) -> bool {
    let layout_digits: String = layout.chars().filter(|c| c.is_ascii_digit()).collect();
    let chars: Vec<char> = walker.chars().collect();
    let mut i = 0usize;
    while i < chars.len() {
        if !chars[i].is_ascii_digit() {
            i += 1;
            continue;
        }
        // Consume one numeric value: digits plus internal group/decimal
        // separators that are immediately followed by another digit.
        let mut digits = String::new();
        while i < chars.len() {
            let c = chars[i];
            if c.is_ascii_digit() {
                digits.push(c);
                i += 1;
            } else if is_group_separator(c)
                && chars.get(i + 1).map_or(false, |n| n.is_ascii_digit())
            {
                i += 1;
            } else {
                break;
            }
        }
        if digits.len() >= 3 && !layout_digits.contains(&digits) {
            return true;
        }
    }
    false
}

/// Separators that may join two digit groups of one numeric value.
fn is_group_separator(c: char) -> bool {
    matches!(c, ' ' | '\u{00a0}' | '\u{202f}' | '\'' | '.' | ',')
}

/// The page-marker heuristic `convert_pdf_bytes_to_markdown` applies to a
/// page's first line. Computed here on the string-walker text so a newly
/// routed page keeps the marker the non-routed path emitted.
fn first_line_looks_like_heading(text: &str) -> bool {
    text.starts_with("# ")
        || text.lines().next().map_or(false, |l| {
            l.len() < 60 && l.chars().all(|c| c.is_alphanumeric() || c.is_whitespace())
        })
}

/// Reports the text for one page (1-based page numbers, as used by
/// `Document::get_pages`) plus whether the page contains text-show operators
/// at all.
pub fn extract_page_text_report(
    doc: &Document,
    page_number: u32,
    detect_tables: bool,
    detect_layout: bool,
    detect_math: bool,
) -> Result<PageText, String> {
    extract_page_text_report_with_marker(
        doc,
        page_number,
        detect_tables,
        detect_layout,
        detect_math,
    )
    .map(|(pt, _)| pt)
}

/// Like `extract_page_text_report`, but also returns the caller's page-marker
/// hint for a page the layout batch newly routed (`Some`), so the caller can
/// keep the marker the non-routed string-walker path would have emitted.
pub fn extract_page_text_report_with_marker(
    doc: &Document,
    page_number: u32,
    detect_tables: bool,
    detect_layout: bool,
    detect_math: bool,
) -> Result<(PageText, Option<bool>), String> {
    let pages: std::collections::BTreeMap<u32, ObjectId> = doc.get_pages();
    let page_id = pages
        .get(&page_number)
        .copied()
        .ok_or_else(|| format!("page {page_number} not found"))?;
    extract_page(doc, page_id, detect_tables, detect_layout, detect_math)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The routing guard must treat a grouped value as one number. The old
    /// per-maximal-run check passed when the glyph page still carried the two
    /// fragments separately even though the whole value was gone.
    #[test]
    fn loses_digit_content_detects_a_lost_grouped_value() {
        // Whole value present: no loss.
        assert!(!loses_digit_content("Total 12 345,67", "Total 12 345,67"));
        assert!(!loses_digit_content("Total 12 345,67", "Total 12345.67"));
        // Glyph page kept only the trailing group: the value is gone.
        assert!(loses_digit_content("Total 12 345,67", "Total 345,67"));
        assert!(loses_digit_content("reference 12 345 678", "reference 678"));
        // A short number (<3 digits) is never a loss signal.
        assert!(!loses_digit_content("article 12", "article"));
        // A pure run lost is still detected.
        assert!(loses_digit_content("reference 123456", "reference"));
    }

    #[test]
    fn glyph_unicode_lookup() {
        assert_eq!(glyph_unicode(b"eacute"), Some(0xE9));
        assert_eq!(glyph_unicode(b"Oslash"), Some(0xD8));
        assert_eq!(glyph_unicode(b"Abreveacute"), Some(0x1EAE));
        assert_eq!(glyph_unicode(b"abreveacute"), Some(0x1EAF));
        assert_eq!(glyph_unicode(b".notdef"), Some(0));
        assert_eq!(glyph_unicode(b"unknownGlyph"), None);
        assert_eq!(glyph_unicode(b"uni1EA1"), Some(0x1EA1));
    }

    #[test]
    fn winansi_decodes_french_accents() {
        let codec = Codec::Byte8(ByteTable(WIN_ANSI), PuaFamily::Unknown);
        let bytes = [
            0xC9, 0x6C, 0x65, 0x63, 0x74, 0x72, 0x69, 0x63, 0x69, 0x74, 0xE9, // Électricité
        ];
        let mut s = String::new();
        codec.decode(&bytes, &mut s);
        assert_eq!(s, "Électricité");
    }

    #[test]
    fn notdef_differences_tolerated() {
        // Base WinAnsi + a Differences entry mapping a valid code to /.notdef
        // must not poison the whole table.
        let mut t = ByteTable(WIN_ANSI);
        assert_eq!(t.0[0xE9], 0xE9);
        // Simulate Differences overriding 0xE9 -> .notdef and 0xE0 -> agrave.
        let names = [b".notdef".as_slice(), b"agrave".as_slice()];
        let mut code: u8 = 0xE9;
        for nm in names {
            t.0[code as usize] = glyph_unicode(nm).unwrap_or(0);
            code = code.wrapping_add(1);
        }
        assert_eq!(t.0[0xE9], 0); // .notdef dropped
        assert_eq!(t.0[0xEA], 0xE0); // next code got agrave
        let codec = Codec::Byte8(t, PuaFamily::Unknown);
        let mut s = String::new();
        codec.decode(&[0xE9, 0xEA], &mut s);
        assert_eq!(s, "à");
    }

    #[test]
    fn cmap_parse_and_decode() {
        let cmap = b"begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n\
                     beginbfchar\n<0041> <0041>\n<00E9> <00E9>\nendbfchar\n\
                     beginbfrange\n<0100> <0102> <1EA0>\nendbfrange\n";
        let cm = parse_cmap(cmap).expect("parse");
        let codec = Codec::CMap(cm, None, PuaFamily::Unknown);
        let bytes = [0x00, 0x41, 0x00, 0xE9, 0x01, 0x01];
        let mut s = String::new();
        codec.decode(&bytes, &mut s);
        assert_eq!(s, "Aéạ");
    }

    #[test]
    fn symbolic_simple_font_with_tounicode_is_not_dropped() {
        // TeX's CMR/CMMI faces are Type1, `/Flags 4` (Symbolic) and carry no
        // `/Encoding` — but they *do* ship a full `/ToUnicode`. Before the
        // fix, `resolve_codec` returned `None` for exactly this shape and
        // every glyph drawn in the font silently vanished.
        use lopdf::Stream;
        let cmap = b"beginbfchar\n<57> <0057>\nendbfchar\n".to_vec();
        let mut fd = Dictionary::new();
        fd.set(b"Flags", 4);
        let mut font = Dictionary::new();
        font.set(b"Subtype", Object::Name(b"Type1".to_vec()));
        font.set(b"BaseFont", Object::Name(b"ABCDEF+CMR9".to_vec()));
        font.set(b"FontDescriptor", Object::Dictionary(fd));
        font.set(b"ToUnicode", Object::Stream(Stream::new(Dictionary::new(), cmap)));
        let doc = Document::new();
        let codec = resolve_codec(&doc, &font)
            .expect("a symbolic simple font with /ToUnicode must still resolve");
        let mut s = String::new();
        codec.decode(&[0x57], &mut s);
        assert_eq!(s, "W");
    }

    #[test]
    fn unic_prefixed_differences_map_to_unicode() {
        // BNP Paribas Type3 statements name every glyph `/UNIC00E9`, a prefix
        // the AGL does not know. Without this the /Differences resolve to
        // nothing and all 859 characters of the statement are dropped.
        assert_eq!(glyph_unicode(b"UNIC00E9"), Some(0xE9));
        assert_eq!(glyph_unicode(b"unic0041"), Some(0x41));
        assert_eq!(glyph_unicode(b"UNKNOWN1"), None);

        let mut enc = Dictionary::new();
        enc.set(b"Type", Object::Name(b"Encoding".to_vec()));
        enc.set(
            b"Differences",
            Object::Array(vec![
                Object::Integer(65),
                Object::Name(b"UNIC0041".to_vec()),
                Object::Name(b"UNIC0042".to_vec()),
            ]),
        );
        let mut font = Dictionary::new();
        font.set(b"Subtype", Object::Name(b"Type3".to_vec()));
        font.set(b"Encoding", Object::Dictionary(enc));
        let doc = Document::new();
        let codec = resolve_codec(&doc, &font).expect("Type3 with UNIC differences resolves");
        let mut s = String::new();
        codec.decode(&[65, 66], &mut s);
        assert_eq!(s, "AB");
    }

    #[test]
    fn symbolic_latin_face_falls_back_but_dingbats_do_not() {
        // Subset CFF/Type1 text faces routinely set the Symbolic flag while
        // drawing StandardEncoding-compatible bytes (Bouygues bills). Decoding
        // them through the Latin table recovers the document; a genuine dingbat
        // face must stay unmapped so it still escalates.
        let mut fd = Dictionary::new();
        fd.set(b"Flags", 4);
        let mut latin = Dictionary::new();
        latin.set(b"Subtype", Object::Name(b"Type1".to_vec()));
        latin.set(b"BaseFont", Object::Name(b"ABCDEF+ArialMT".to_vec()));
        latin.set(b"FontDescriptor", Object::Dictionary(fd.clone()));
        let doc = Document::new();
        let codec =
            resolve_codec(&doc, &latin).expect("a Latin face flagged Symbolic must resolve");
        let mut s = String::new();
        codec.decode(b"Bonjour", &mut s);
        assert_eq!(s, "Bonjour");

        let mut dingbat = Dictionary::new();
        dingbat.set(b"Subtype", Object::Name(b"Type1".to_vec()));
        dingbat.set(b"BaseFont", Object::Name(b"ABCDEF+Wingdings".to_vec()));
        dingbat.set(b"FontDescriptor", Object::Dictionary(fd));
        assert!(
            resolve_codec(&doc, &dingbat).is_none(),
            "a dingbat face must not be decoded as Latin text"
        );
    }

    #[test]
    fn cmap_fallback_to_byte_table() {
        // 2-byte CMap that does not cover a code actually used on the page:
        // decode must fall back to the byte table so no text is lost.
        let cmap = b"beginbfchar\n<0041> <0041>\nendbfchar\n";
        let cm = parse_cmap(cmap).expect("parse");
        let codec = Codec::CMap(cm, Some(ByteTable(WIN_ANSI)), PuaFamily::Unknown);
        // 0x00E9 with a 2-byte code space, but only <0041> is mapped: the
        // byte table decodes 0xE9 -> é on its own.
        let bytes = [0x00, 0xE9];
        let mut s = String::new();
        codec.decode(&bytes, &mut s);
        assert_eq!(s, "é");
    }

    /// Minimal single-page PDF whose four `Tj` lines live inside one `BT`/`ET`
    /// and are advanced by a vertical `Td` (no `ET`/`T*` between lines), with
    /// each show-string padded by a leading and trailing space — the shape of
    /// `scratch/samples/synth_ticket_compressed.pdf`.
    fn td_line_advance_doc() -> Document {
        use lopdf::{dictionary, Stream};

        let mut doc = Document::with_version("1.4");
        let font_id = doc.new_object_id();
        let content_id = doc.new_object_id();
        let page_id = doc.new_object_id();
        let pages_id = doc.new_object_id();
        let catalog_id = doc.new_object_id();

        doc.objects.insert(
            font_id,
            Object::Dictionary(dictionary! {
                "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica",
                "Encoding" => "WinAnsiEncoding",
            }),
        );
        let content = b"BT /F1 12 Tf 50 800 Td ( BILLET \xc9LECTRONIQUE ) Tj\n\
0 -20 Td ( R\xc9F\xc9RENCE DE VOTRE R\xc9SERVATION ) Tj\n\
0 -20 Td ( Obtenez votre carte d'embarquement. ) Tj ET"
            .to_vec();
        doc.objects
            .insert(content_id, Object::Stream(Stream::new(dictionary! {}, content)));
        doc.objects.insert(
            page_id,
            Object::Dictionary(dictionary! {
                "Type" => "Page", "Parent" => pages_id,
                "MediaBox" => Object::Array(vec![
                    Object::Integer(0), Object::Integer(0),
                    Object::Integer(595), Object::Integer(842),
                ]),
                "Resources" => dictionary! { "Font" => dictionary! { "F1" => font_id } },
                "Contents" => content_id,
            }),
        );
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => Object::Array(vec![Object::Reference(page_id)]),
                "Count" => 1,
            }),
        );
        doc.objects.insert(
            catalog_id,
            Object::Dictionary(dictionary! { "Type" => "Catalog", "Pages" => pages_id }),
        );
        doc.trailer.set("Root", catalog_id);
        doc
    }

    #[test]
    fn td_line_advance_breaks_padded_show_strings() {
        // Before the fix the string walker ignored the `Td` line advance and
        // kept each string's padding, emitting one fused run:
        //   " BILLET ÉLECTRONIQUE  RÉFÉRENCE DE VOTRE RÉSERVATION  Obtenez ..."
        // instead of one line per visual line.
        let doc = td_line_advance_doc();
        let page = extract_page_text_report(&doc, 1, true, true, false).expect("page text");
        assert_eq!(
            page.text,
            "BILLET ÉLECTRONIQUE\nRÉFÉRENCE DE VOTRE RÉSERVATION\n\
             Obtenez votre carte d'embarquement."
        );
    }

    #[test]
    fn horizontal_td_does_not_start_a_new_line() {
        // A pure horizontal `Td` is an in-line move, not a line advance.
        let horizontal = Operation::new("Td", vec![Object::Integer(40), Object::Integer(0)]);
        assert!(!td_advances_line(&horizontal));
        let vertical = Operation::new("Td", vec![Object::Integer(0), Object::Integer(-20)]);
        assert!(td_advances_line(&vertical));
    }

    /// Minimal single-page PDF with a base-14 Courier font (600/1000 em advance
    /// for every code, so advances are exactly predictable) and the caller's
    /// content stream.
    fn content_doc(content: &[u8]) -> Document {
        use lopdf::{dictionary, Stream};

        let mut doc = Document::with_version("1.4");
        let font_id = doc.new_object_id();
        let content_id = doc.new_object_id();
        let page_id = doc.new_object_id();
        let pages_id = doc.new_object_id();
        let catalog_id = doc.new_object_id();

        doc.objects.insert(
            font_id,
            Object::Dictionary(dictionary! {
                "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Courier",
                "Encoding" => "WinAnsiEncoding",
            }),
        );
        doc.objects
            .insert(content_id, Object::Stream(Stream::new(dictionary! {}, content.to_vec())));
        doc.objects.insert(
            page_id,
            Object::Dictionary(dictionary! {
                "Type" => "Page", "Parent" => pages_id,
                "MediaBox" => Object::Array(vec![
                    Object::Integer(0), Object::Integer(0),
                    Object::Integer(595), Object::Integer(842),
                ]),
                "Resources" => dictionary! { "Font" => dictionary! { "F1" => font_id } },
                "Contents" => content_id,
            }),
        );
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => Object::Array(vec![Object::Reference(page_id)]),
                "Count" => 1,
            }),
        );
        doc.objects.insert(
            catalog_id,
            Object::Dictionary(dictionary! { "Type" => "Catalog", "Pages" => pages_id }),
        );
        doc.trailer.set("Root", catalog_id);
        doc
    }

    #[test]
    fn horizontal_td_word_gaps_are_restored() {
        // `Tj` fragments positioned only by horizontal `Td` (no `TJ`, no `Tm`)
        // — the a corpus file producer shape. Word boundaries are the gaps between the
        // previous run's natural end and the next run's start.
        // Courier 12pt advance = 0.6*12 = 7.2 pt/char:
        //   "Bonjour" ends at 50.4; Td 60 -> gap 9.6 (> 0.1625 em)
        //   "le" ends at 74.4;     Td 20 -> gap 5.6
        let doc = content_doc(
            b"BT /F1 12 Tf (Bonjour) Tj 60 0 Td (le) Tj 20 0 Td (monde) Tj ET",
        );
        let page = extract_page_text_report(&doc, 1, true, true, false).expect("page text");
        assert_eq!(page.text, "Bonjour le monde");
    }

    #[test]
    fn horizontal_td_at_natural_advance_does_not_split_a_word() {
        // A producer that splits a word into `Tj` runs placed exactly at the
        // natural advance (a kerning emulation) must NOT gain a space: gap == 0.
        let doc = content_doc(b"BT /F1 12 Tf (Bon) Tj 21.6 0 Td (jour) Tj ET");
        let page = extract_page_text_report(&doc, 1, true, true, false).expect("page text");
        assert_eq!(page.text, "Bonjour");
    }

    #[test]
    fn an_upward_baseline_jump_starts_a_new_paragraph() {
        // Two columns drawn in stream order with `Tj` + `Td` (the string-walker
        // shape, `iv_twocol`): the right column restarts at the top of the page,
        // a baseline *above* the last left-column line. The walker must insert
        // a blank line so a later reflow cannot weld the two columns.
        let doc = content_doc(
            b"BT /F1 12 Tf 50 760 Td (gauche sans fin) Tj ET\n\
              BT /F1 12 Tf 50 744 Td (suite gauche) Tj ET\n\
              BT /F1 12 Tf 320 760 Td (droite minuscule) Tj ET\n\
              BT /F1 12 Tf 320 744 Td (suite droite) Tj ET",
        );
        // `detect_tables` off: this is the string-walker path. With tables on the
        // glyph engine recognises this 2x2 alignment as a grid instead.
        let page = extract_page_text_report(&doc, 1, false, false, false).expect("page text");
        assert!(
            page.text.contains("suite gauche\n\ndroite minuscule"),
            "columns must be separated by a blank line: {:?}",
            page.text
        );
        // Within one column the wrapped lines stay in a single paragraph.
        assert!(
            page.text.contains("gauche sans fin\nsuite gauche"),
            "wrapped lines must not gain a blank line: {:?}",
            page.text
        );
    }

    #[test]
    fn a_sideways_line_above_starts_a_new_paragraph() {
        // A right column that starts only 6 pt above the last left-column line
        // (< 1 em, so the upward-jump rule alone misses it) but at a clearly
        // different x is still a new column and must get a blank line.
        let doc = content_doc(
            b"BT /F1 12 Tf 50 760 Td (gauche sans fin) Tj ET\n\
              BT /F1 12 Tf 50 744 Td (suite gauche) Tj ET\n\
              BT /F1 12 Tf 320 750 Td (droite minuscule) Tj ET\n\
              BT /F1 12 Tf 320 734 Td (suite droite) Tj ET",
        );
        let page = extract_page_text_report(&doc, 1, false, false, false).expect("page text");
        assert!(
            page.text.contains("suite gauche\n\ndroite minuscule"),
            "a sideways line above must start a new paragraph: {:?}",
            page.text
        );
    }

    #[test]
    fn table_row_cells_on_one_baseline_are_not_a_paragraph_break() {
        // Cells of a label:value / table row share a baseline; the large x jump
        // between them is a cell placement, never a line break, so no blank
        // line may appear between them.
        let doc = content_doc(
            b"BT /F1 12 Tf 50 760 Td (Reference) Tj ET\n\
              BT /F1 12 Tf 200 760 Td (Quantite) Tj ET\n\
              BT /F1 12 Tf 350 760 Td (Montant) Tj ET",
        );
        let page = extract_page_text_report(&doc, 1, false, false, false).expect("page text");
        assert!(
            !page.text.contains("\n\n"),
            "same-baseline cells must not become paragraphs: {:?}",
            page.text
        );
    }

    #[test]
    fn right_aligned_amount_on_same_baseline_is_not_a_paragraph_break() {
        // A right-aligned amount next to its label, same baseline.
        let doc = content_doc(
            b"BT /F1 12 Tf 50 760 Td (Total a payer) Tj ET\n\
              BT /F1 12 Tf 400 760 Td (1 234,56) Tj ET",
        );
        let page = extract_page_text_report(&doc, 1, false, false, false).expect("page text");
        assert!(
            !page.text.contains("\n\n"),
            "a right-aligned amount must not become a paragraph: {:?}",
            page.text
        );
    }

    #[test]
    fn wrapped_lines_without_column_geometry_stay_welded() {
        // The qa-int2 `iv_col_end` shape: two consecutive wrapped lines with the
        // same start x and a descending baseline. There is no column geometry
        // (no upward jump, no x jump), so this is an ordinary wrapped line and
        // reflow must be free to join it — inserting a blank line here would
        // break every wrapped paragraph in the corpus.
        let doc = content_doc(
            b"BT /F1 12 Tf 50 760 Td (premiere colonne sans fin) Tj ET\n\
              BT /F1 12 Tf 50 744 Td (droite commence minuscule) Tj ET",
        );
        let page = extract_page_text_report(&doc, 1, false, false, false).expect("page text");
        assert!(
            !page.text.contains("\n\n"),
            "geometry-less wrapped lines must not gain a blank line: {:?}",
            page.text
        );
    }

    #[test]
    fn a_midline_superscript_does_not_start_a_new_paragraph() {
        // qa-int3 `h2x_superscript_midline`: a footnote/superscript mark at the
        // END of a long line, a few points above its baseline. Its start x
        // falls inside the previous show's horizontal span, so it continues the
        // same visual line and must not gain a blank line (QA risk R1).
        let doc = content_doc(
            b"BT /F1 12 Tf 50 760 Td (Un long paragraphe de texte avec une note) Tj ET\n\
              BT /F1 12 Tf 300 764 Td (1) Tj ET\n\
              BT /F1 12 Tf 50 744 Td (qui continue sur la ligne suivante) Tj ET",
        );
        let page = extract_page_text_report(&doc, 1, false, false, false).expect("page text");
        assert!(
            !page.text.contains("\n\n"),
            "a mid-line superscript must not break the paragraph: {:?}",
            page.text
        );
    }

    #[test]
    fn a_footnote_mark_near_the_line_start_does_not_break() {
        // The small-x-jump superscript shape (`h2x_superscript_smalljump`): the
        // mark sits above the previous baseline but horizontally inside the
        // previous line, so it is the same line.
        let doc = content_doc(
            b"BT /F1 12 Tf 50 760 Td (Le total general est) Tj ET\n\
              BT /F1 12 Tf 70 764 Td (1) Tj ET\n\
              BT /F1 12 Tf 50 744 Td (de cent euros environ) Tj ET",
        );
        let page = extract_page_text_report(&doc, 1, false, false, false).expect("page text");
        assert!(
            !page.text.contains("\n\n"),
            "a footnote mark at the line start must not break: {:?}",
            page.text
        );
    }

    #[test]
    fn a_smaller_font_mark_beyond_the_line_end_does_not_break() {
        // A real superscript is a *smaller* face a few points up. Even when it
        // is placed clear of the previous line's end (a right-margin footnote
        // mark), the small rise plus the smaller size must keep it on the same
        // line.
        let doc = content_doc(
            b"BT /F1 12 Tf 50 760 Td (Le montant total est de) Tj ET\n\
              BT /F1 6 Tf 400 764 Td (1) Tj ET\n\
              BT /F1 12 Tf 50 744 Td (cent euros environ) Tj ET",
        );
        let page = extract_page_text_report(&doc, 1, false, false, false).expect("page text");
        assert!(
            !page.text.contains("\n\n"),
            "a smaller-font superscript mark must not break: {:?}",
            page.text
        );
    }

    #[test]
    fn a_near_height_two_column_start_still_breaks() {
        // The H2 win must survive: a right column that starts only 6 pt above
        // the last left-column line (under one em) at a clearly different x is
        // a new block and keeps its blank line.
        let doc = content_doc(
            b"BT /F1 12 Tf 50 760 Td (colonne gauche sans fin) Tj ET\n\
              BT /F1 12 Tf 50 744 Td (la suite de la phrase) Tj ET\n\
              BT /F1 12 Tf 320 750 Td (droite commence) Tj ET\n\
              BT /F1 12 Tf 320 734 Td (deuxieme ligne droite) Tj ET",
        );
        let page = extract_page_text_report(&doc, 1, false, false, false).expect("page text");
        assert!(
            page.text.contains("la suite de la phrase\n\ndroite commence"),
            "a near-height two-column start must keep its blank line: {:?}",
            page.text
        );
    }

    #[test]
    fn a_label_and_value_on_one_baseline_stay_on_one_line() {
        // A label/value cell row shares a baseline; the far x jump is a cell
        // placement, never a paragraph break.
        let doc = content_doc(
            b"BT /F1 12 Tf 50 760 Td (Nom du client :) Tj ET\n\
              BT /F1 12 Tf 300 760 Td (Dupont Jean) Tj ET",
        );
        let page = extract_page_text_report(&doc, 1, false, false, false).expect("page text");
        assert!(
            !page.text.contains("\n\n"),
            "label and value on one baseline must stay joined: {:?}",
            page.text
        );
    }

    #[test]
    fn a_hanging_indent_continuation_stays_joined() {
        // The second line of a hanging-indent paragraph starts further right
        // but *below* the first: no vertical rise, so it must stay in the same
        // paragraph.
        let doc = content_doc(
            b"BT /F1 12 Tf 50 760 Td (premiere ligne du paragraphe) Tj ET\n\
              BT /F1 12 Tf 70 744 Td (suite indentee du meme paragraphe) Tj ET\n\
              BT /F1 12 Tf 70 728 Td (encore la suite de ce paragraphe) Tj ET",
        );
        let page = extract_page_text_report(&doc, 1, false, false, false).expect("page text");
        assert!(
            !page.text.contains("\n\n"),
            "a hanging indent must not break the paragraph: {:?}",
            page.text
        );
    }

    #[test]
    fn horizontal_td_gaps_inside_a_form_xobject_are_restored() {
        // a corpus file's shape: the whole text lives in a Form XObject whose content
        // is `Tj`-only, positioned by horizontal `Td`.
        use lopdf::{dictionary, Stream};

        let mut doc = Document::with_version("1.4");
        let font_id = doc.new_object_id();
        let form_id = doc.new_object_id();
        let page_id = doc.new_object_id();
        let pages_id = doc.new_object_id();
        let catalog_id = doc.new_object_id();

        doc.objects.insert(
            font_id,
            Object::Dictionary(dictionary! {
                "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Courier",
                "Encoding" => "WinAnsiEncoding",
            }),
        );
        let form_content = b"BT /F1 12 Tf (Bonjour) Tj 60 0 Td (le) Tj 20 0 Td (monde) Tj ET";
        let mut form_dict = dictionary! {
            "Type" => "XObject",
            "Subtype" => "Form",
            "BBox" => Object::Array(vec![
                Object::Integer(0), Object::Integer(0),
                Object::Integer(595), Object::Integer(842),
            ]),
            "Resources" => dictionary! { "Font" => dictionary! { "F1" => font_id } },
        };
        form_dict.set("Length", form_content.len() as i64);
        doc.objects.insert(
            form_id,
            Object::Stream(Stream::new(form_dict, form_content.to_vec())),
        );
        let page_content = b"q /Fm0 Do Q";
        let page_content_id = doc.new_object_id();
        doc.objects.insert(
            page_content_id,
            Object::Stream(Stream::new(dictionary! {}, page_content.to_vec())),
        );
        doc.objects.insert(
            page_id,
            Object::Dictionary(dictionary! {
                "Type" => "Page", "Parent" => pages_id,
                "MediaBox" => Object::Array(vec![
                    Object::Integer(0), Object::Integer(0),
                    Object::Integer(595), Object::Integer(842),
                ]),
                "Resources" => dictionary! { "XObject" => dictionary! { "Fm0" => form_id } },
                "Contents" => page_content_id,
            }),
        );
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => Object::Array(vec![Object::Reference(page_id)]),
                "Count" => 1,
            }),
        );
        doc.objects.insert(
            catalog_id,
            Object::Dictionary(dictionary! { "Type" => "Catalog", "Pages" => pages_id }),
        );
        doc.trailer.set("Root", catalog_id);

        let page = extract_page_text_report(&doc, 1, true, true, false).expect("page text");
        assert_eq!(page.text, "Bonjour le monde");
    }

    /// Minimal single-page PDF whose page draws one parent form, and whose
    /// parent form issues `repeats` `/FmLeaf Do`s of a leaf form that shows the
    /// word `mot`. Used to prove the string walker's shared per-page `Do` budget
    /// bounds a form-DAG expansion (mirrors the glyph engine's budget test).
    fn repeating_form_doc(repeats: usize) -> Document {
        use lopdf::{dictionary, Stream};

        let mut doc = Document::with_version("1.5");
        let font_id = doc.new_object_id();
        let leaf_id = doc.new_object_id();
        let parent_id = doc.new_object_id();
        let page_content_id = doc.new_object_id();
        let page_id = doc.new_object_id();
        let pages_id = doc.new_object_id();
        let catalog_id = doc.new_object_id();

        doc.objects.insert(
            font_id,
            Object::Dictionary(dictionary! {
                "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Courier",
                "Encoding" => "WinAnsiEncoding",
            }),
        );
        let leaf_content = b"BT /F1 12 Tf (mot) Tj ET".to_vec();
        doc.objects.insert(
            leaf_id,
            Object::Stream(Stream::new(
                dictionary! {
                    "Type" => "XObject", "Subtype" => "Form",
                    "BBox" => Object::Array(vec![
                        Object::Integer(0), Object::Integer(0),
                        Object::Integer(595), Object::Integer(842),
                    ]),
                    "Resources" => dictionary! { "Font" => dictionary! { "F1" => font_id } },
                    "Length" => leaf_content.len() as i64,
                },
                leaf_content,
            )),
        );
        let mut parent_content = Vec::new();
        for _ in 0..repeats {
            parent_content.extend_from_slice(b"/FmLeaf Do\n");
        }
        doc.objects.insert(
            parent_id,
            Object::Stream(Stream::new(
                dictionary! {
                    "Type" => "XObject", "Subtype" => "Form",
                    "BBox" => Object::Array(vec![
                        Object::Integer(0), Object::Integer(0),
                        Object::Integer(595), Object::Integer(842),
                    ]),
                    "Resources" =>
                        dictionary! { "XObject" => dictionary! { "FmLeaf" => leaf_id } },
                    "Length" => parent_content.len() as i64,
                },
                parent_content,
            )),
        );
        let page_content = b"/Fm0 Do".to_vec();
        doc.objects.insert(
            page_content_id,
            Object::Stream(Stream::new(dictionary! {}, page_content)),
        );
        doc.objects.insert(
            page_id,
            Object::Dictionary(dictionary! {
                "Type" => "Page", "Parent" => pages_id,
                "MediaBox" => Object::Array(vec![
                    Object::Integer(0), Object::Integer(0),
                    Object::Integer(595), Object::Integer(842),
                ]),
                "Resources" => dictionary! { "XObject" => dictionary! { "Fm0" => parent_id } },
                "Contents" => page_content_id,
            }),
        );
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => Object::Array(vec![Object::Reference(page_id)]),
                "Count" => 1,
            }),
        );
        doc.objects.insert(
            catalog_id,
            Object::Dictionary(dictionary! { "Type" => "Catalog", "Pages" => pages_id }),
        );
        doc.trailer.set("Root", catalog_id);
        doc
    }

    #[test]
    fn a_form_drawn_twice_is_walked_twice() {
        // The legitimate repeated-`Do` case (same form, two invocations) must
        // still yield both drawings: the budget only stops pathological
        // expansion, it must not deduplicate.
        let doc = repeating_form_doc(2);
        let page = extract_page_text_report(&doc, 1, true, true, false).expect("page text");
        assert_eq!(page.text.matches("mot").count(), 2);
    }

    #[test]
    fn wide_form_expansion_hits_the_do_budget_and_terminates() {
        // One parent form issues far more `Do`s than the per-page budget allows.
        // Without the shared budget the walker would decode every invocation.
        let repeats = MAX_WALKER_DO_PER_PAGE + 88;
        let doc = repeating_form_doc(repeats);
        let page = extract_page_text_report(&doc, 1, true, true, false).expect("page text");
        let n = page.text.matches("mot").count();
        assert!(
            n <= MAX_WALKER_DO_PER_PAGE,
            "expansion must stop at the shared Do budget, got {n}"
        );
        assert!(
            n >= MAX_WALKER_DO_PER_PAGE - 1,
            "the budget must actually be reached, got {n}"
        );
    }

    #[test]
    fn digital_probe_is_bounded_on_a_form_dag() {
        // `is_digital_pdf_bytes` must find the leaf text through the form DAG
        // (and must not expand it without bound to do so).
        let repeats = MAX_WALKER_DO_PER_PAGE + 88;
        let mut doc = repeating_form_doc(repeats);
        let mut bytes = Vec::new();
        doc.save_to(&mut bytes).expect("save pdf");
        assert!(crate::is_digital_pdf_bytes(&bytes));
    }

    /// A page drawing `n` DISTINCT forms (one word each), the shape that lost
    /// every form past the old 512 `Do`/page cap (qa-int2 `forms_distinct_*`).
    fn distinct_forms_doc(n: usize) -> Document {
        use lopdf::{dictionary, Stream};

        let mut doc = Document::with_version("1.5");
        let font_id = doc.add_object(dictionary! {
            "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Courier",
            "Encoding" => "WinAnsiEncoding",
        });
        let leaf_res = dictionary! { "Font" => dictionary! { "F1" => font_id } };
        let mut xobjects = lopdf::Dictionary::new();
        let mut page_content = Vec::new();
        for i in 0..n {
            let word = format!("w{i:04}");
            let leaf_content = format!("BT /F1 12 Tf ({word}) Tj ET").into_bytes();
            let leaf = Stream::new(
                dictionary! {
                    "Type" => "XObject", "Subtype" => "Form",
                    "BBox" => Object::Array(vec![
                        Object::Integer(0), Object::Integer(0),
                        Object::Integer(595), Object::Integer(842),
                    ]),
                    "Resources" => leaf_res.clone(),
                    "Length" => leaf_content.len() as i64,
                },
                leaf_content,
            );
            let id = doc.add_object(leaf);
            xobjects.set(format!("Fm{i}"), Object::Reference(id));
            page_content.extend_from_slice(format!("/Fm{i} Do\n").as_bytes());
        }
        let page_content_id = doc.add_object(Stream::new(dictionary! {}, page_content));
        let pages_id = doc.new_object_id();
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page", "Parent" => pages_id,
            "MediaBox" => Object::Array(vec![
                Object::Integer(0), Object::Integer(0),
                Object::Integer(595), Object::Integer(842),
            ]),
            "Resources" => dictionary! { "XObject" => xobjects },
            "Contents" => page_content_id,
        });
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages", "Kids" => Object::Array(vec![Object::Reference(page_id)]),
                "Count" => 1,
            }),
        );
        let catalog_id = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        doc.trailer.set("Root", catalog_id);
        doc
    }

    #[test]
    fn many_distinct_forms_are_all_walked() {
        // 1200 distinct one-word forms: every invocation must be walked once
        // (qa-int2 `forms_distinct_1200`), and the words must survive reflow.
        let doc = distinct_forms_doc(1200);
        let page = extract_page_text_report(&doc, 1, true, true, false).expect("page text");
        let words = page.text.split_whitespace().filter(|w| w.starts_with('w')).count();
        assert_eq!(words, 1200, "all distinct forms must survive");
    }

    #[test]
    fn a_form_repeated_600_times_is_walked_600_times() {
        // One form drawn 600 times: repeated invocations are content, not
        // overdraw (qa-int2 `forms_repeat_600`).
        let doc = repeating_form_doc(600);
        let page = extract_page_text_report(&doc, 1, true, true, false).expect("page text");
        assert_eq!(page.text.matches("mot").count(), 600);
        assert!(
            !page.budget_exhausted,
            "600 invocations are well inside the budgets"
        );
    }

    #[test]
    fn a_page_over_the_do_cap_reports_budget_exhausted() {
        // qa-int3 `forms_distinct_20000`: a page whose `Do` count exceeds the
        // per-page cap is truncated, but the truncation must be reported, not
        // silent. A repeated parent form drives the same `do_left` bound with a
        // far cheaper fixture than 16k+ distinct form objects.
        let doc = repeating_form_doc(MAX_WALKER_DO_PER_PAGE + 88);
        let page = extract_page_text_report(&doc, 1, true, true, false).expect("page text");
        assert!(
            page.budget_exhausted,
            "a page over the Do cap must set budget_exhausted"
        );
    }

    #[test]
    fn a_page_under_the_do_cap_does_not_report_budget_exhausted() {
        let doc = distinct_forms_doc(600);
        let page = extract_page_text_report(&doc, 1, true, true, false).expect("page text");
        let words = page.text.split_whitespace().filter(|w| w.starts_with('w')).count();
        assert_eq!(words, 600);
        assert!(
            !page.budget_exhausted,
            "600 distinct forms are inside the budgets"
        );
    }

    #[test]
    fn ligature_hex_and_suffix_glyph_names_map() {
        assert_eq!(glyph_unicode(b"fi"), Some(0xFB01));
        assert_eq!(glyph_unicode(b"ffl"), Some(0xFB04));
        assert_eq!(glyph_unicode(b"a.sc"), Some(0x61));
        assert_eq!(glyph_unicode(b"eacute.sc"), Some(0xE9));
        assert_eq!(glyph_unicode(b"f_i"), Some(0xFB01));
        assert_eq!(glyph_unicode(b"u00E9"), Some(0xE9));
        // Astral code point: the byte table is 16-bit, so it is not mappable.
        assert_eq!(glyph_unicode(b"u1F600"), None);
        // Multi-unit `uni` sequence spelling a ligature.
        assert_eq!(glyph_unicode(b"uni00660069"), Some(0xFB01));
        assert_eq!(glyph_unicode(b"uni1EA1"), Some(0x1EA1));
    }

    #[test]
    fn winansi_soft_hyphen_decodes_as_a_hyphen() {
        let codec = Codec::Byte8(ByteTable(WIN_ANSI), PuaFamily::Unknown);
        let mut s = String::new();
        codec.decode(&[0xAD], &mut s);
        assert_eq!(s, "-");
    }

    #[test]
    fn decoded_text_normalises_ligatures_hyphens_and_nbsp() {
        let input = "1\u{00A0}234,56 \u{FB01}n x\u{00AD}y";
        assert_eq!(normalize_decoded_text(input), "1 234,56 fin xy");
        // An NBSP that is not a digit separator is left untouched.
        assert_eq!(normalize_decoded_text("a\u{00A0}b"), "a\u{00A0}b");
    }

    #[test]
    fn symbol_pua_is_mapped_and_unmapped_pua_is_dropped() {
        // Wingdings/Symbol private-use glyphs map to their Unicode equivalent.
        assert_eq!(normalize_decoded_text("a\u{F0B7}b"), "a\u{2022}b");
        assert_eq!(normalize_decoded_text("a\u{F0A7}b"), "a\u{25AA}b");
        assert_eq!(normalize_decoded_text("a\u{F0FC}b"), "a\u{2714}b");
        assert_eq!(normalize_decoded_text("\u{F0D8}"), "\u{2B9A}");
        // The full Adobe Symbol charset is now covered: U+F0B9 is NOT EQUAL TO.
        assert_eq!(normalize_decoded_text("a\u{F0B9}b"), "a\u{2260}b");
        // Greek letters from a Symbol-family ToUnicode CMap survive.
        assert_eq!(normalize_decoded_text("\u{F061}\u{F062}"), "\u{03B1}\u{03B2}");
        // A private-use glyph outside the Symbol range, or one the table does
        // not know, is dropped, not emitted.
        assert_eq!(normalize_decoded_text("a\u{E123}b"), "ab");
        assert_eq!(normalize_decoded_text("a\u{F0123}b"), "ab");
        assert_eq!(normalize_decoded_text("a\u{F0D2}b"), "ab"); // CUS-only code
    }

    #[test]
    fn pua_mapping_is_font_family_aware() {
        use crate::glyph_data::{pua_to_char_for_family, PuaFamily};
        // 0x52 means Rho in the Symbol charset but a sun in the corpus-verified
        // Wingdings census: the family selects which one is emitted.
        assert_eq!(
            pua_to_char_for_family(0xF052, PuaFamily::Symbol),
            Some('\u{03A1}')
        );
        assert_eq!(
            pua_to_char_for_family(0xF052, PuaFamily::Wingdings),
            Some('\u{263C}')
        );
        // The font-unknown union keeps the corpus-verified value on a collision.
        assert_eq!(pua_to_char_for_family(0xF052, PuaFamily::Unknown), Some('\u{263C}'));
        assert_eq!(
            crate::glyph_data::pua_family_for_base_font(b"ABCDEF+Symbol"),
            PuaFamily::Symbol
        );
        assert_eq!(
            crate::glyph_data::pua_family_for_base_font(b"ABCDEF+Wingdings"),
            PuaFamily::Wingdings
        );
        assert_eq!(
            crate::glyph_data::pua_family_for_base_font(b"ABCDEF+ArialMT"),
            PuaFamily::Unknown
        );
    }

    #[test]
    fn symbol_font_tounicode_pua_decodes_through_the_symbol_charset() {
        // A Type1 Symbol-family font whose ToUnicode maps 0x61/0x62 to the
        // private-use convention must decode to Greek alpha/beta.
        let cmap = b"beginbfchar\n<61> <F061>\n<62> <F062>\nendbfchar\n".to_vec();
        let mut font = Dictionary::new();
        font.set(b"Subtype", Object::Name(b"Type1".to_vec()));
        font.set(b"BaseFont", Object::Name(b"ABCDEF+Symbol".to_vec()));
        font.set(b"ToUnicode", Object::Stream(lopdf::Stream::new(Dictionary::new(), cmap)));
        font.set(
            b"Encoding",
            Object::Name(b"WinAnsiEncoding".to_vec()),
        );
        let doc = Document::new();
        let codec = resolve_codec(&doc, &font).expect("symbol font resolves");
        let mut s = String::new();
        codec.decode(&[0x61, 0x62], &mut s);
        assert_eq!(s, "\u{03B1}\u{03B2}");
    }

    #[test]
    fn cmap_tolerates_whitespace_inside_hex() {
        // `< 0041 >` is the same two-byte code as `<0041>`; the whitespace must
        // not make the entry unparseable.
        let cmap = b"beginbfchar\n< 0041 > < 0041 >\nendbfchar\n";
        let cm = parse_cmap(cmap).expect("parse");
        let codec = Codec::CMap(cm, None, PuaFamily::Unknown);
        let mut s = String::new();
        codec.decode(&[0x00, 0x41], &mut s);
        assert_eq!(s, "A");
    }

    #[test]
    fn cmap_partial_coverage_falls_back_per_code() {
        // Only 'A' is named by the CMap; 'B' must come from the byte table.
        let cmap = b"beginbfchar\n<41> <0041>\nendbfchar\n";
        let cm = parse_cmap(cmap).expect("parse");
        let codec = Codec::CMap(cm, Some(ByteTable(WIN_ANSI)), PuaFamily::Unknown);
        let mut s = String::new();
        codec.decode(&[0x41, 0x42], &mut s);
        assert_eq!(s, "AB");
    }

    #[test]
    fn code_metrics_counts_codes_not_bytes_for_cid_fonts() {
        // A 2-byte Identity-H run `A B` (with a space code between) is three
        // character codes, not six bytes, and exactly one of them is a space.
        // The old byte-length accounting doubled the `Tc` term and could
        // mistake any 0x20 low byte for a space, fusing the next word (D2).
        let cmap = b"beginbfchar\n<0041> <0041>\n<0020> <0020>\n<0042> <0042>\nendbfchar\n";
        let cm = parse_cmap(cmap).expect("parse");
        let codec = Codec::CMap(cm, None, PuaFamily::Unknown);
        let bytes = [0x00, 0x41, 0x00, 0x20, 0x00, 0x42];
        assert_eq!(codec.code_metrics(&bytes), (3, 1));
        // A matching byte-oriented font counts every byte as one code.
        let byte = Codec::Byte8(ByteTable(WIN_ANSI), PuaFamily::Unknown);
        assert_eq!(byte.code_metrics(&[b'A', b' ', b'B']), (3, 1));
    }

    #[test]
    fn real_negative_kerning_is_a_word_gap() {
        let codec = Codec::Byte8(ByteTable(WIN_ANSI), PuaFamily::Unknown);
        let ops = vec![Object::Array(vec![
            Object::String(b"Total".to_vec(), lopdf::StringFormat::Literal),
            Object::Real(-200.0),
            Object::String(b"HT".to_vec(), lopdf::StringFormat::Literal),
        ])];
        let mut out = String::new();
        show_text(&mut out, &codec, &ops);
        assert_eq!(out, "Total HT");
    }

    #[test]
    fn q_restores_the_text_font_after_a_scope() {
        // Inside `q … Q` an unknown font is selected; the surrounding `Tj` runs
        // must keep decoding through the font selected before `q`.
        let doc = content_doc(
            b"BT /F1 12 Tf (AB) Tj q /F0 12 Tf (XY) Tj Q (CD) Tj ET",
        );
        let page = extract_page_text_report(&doc, 1, true, true, false).expect("page text");
        assert_eq!(page.text, "ABCD");
    }

    #[test]
    fn rotated_ctm_page_falls_back_to_the_string_walker() {
        // a corpus file's shape: a rotated `cm` makes the geometry engine classify
        // every span as vertical and, with layout analysis on, it drops them
        // from `text` (keeping only blocks). The walker must be used instead of
        // reporting "no readable words".
        let doc = content_doc(
            b"q 0 -0.2215 0.2215 0 0 792 cm BT /F1 1 Tf \
              24.6667 0 0 40 1.25 100 Tm (Bonjour) Tj ET Q",
        );
        let page = extract_page_text_report(&doc, 1, true, true, false).expect("page text");
        assert_eq!(page.text.split_whitespace().collect::<Vec<_>>(), ["Bonjour"]);
    }
}
