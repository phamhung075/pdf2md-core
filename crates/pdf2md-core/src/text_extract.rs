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

use std::collections::HashMap;

use lopdf::content::{Content, Operation};
use lopdf::{Dictionary, Document, Object, ObjectId};

use crate::glyph_data::{AGL_NAMES, MAC_ROMAN, WIN_ANSI};

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
    Byte8(ByteTable),
    /// ToUnicode CMap, with an optional byte-table fallback for simple fonts
    /// whose CMap does not cover the actual char codes used on the page.
    CMap(CMapCodec, Option<ByteTable>),
}

fn push_utf16(out: &mut String, units: &[u16]) {
    for c in char::decode_utf16(units.iter().copied()).flatten() {
        out.push(c);
    }
}

impl Codec {
    pub(crate) fn decode(&self, bytes: &[u8], out: &mut String) {
        match self {
            Codec::Byte8(t) => decode_byte_table(t, bytes, out),
            Codec::CMap(cm, fallback) => {
                let before = out.len();
                decode_cmap(cm, bytes, out);
                let produced = out.len() > before;
                if !produced && bytes.iter().any(|&b| b >= 0x20) {
                    if let Some(t) = fallback {
                        decode_byte_table(t, bytes, out);
                    }
                }
            }
        }
    }
}

fn decode_byte_table(t: &ByteTable, bytes: &[u8], out: &mut String) {
    for &b in bytes {
        let u = t.0[b as usize];
        if u != 0 {
            if let Some(ch) = char::from_u32(u as u32) {
                out.push(ch);
            }
        }
    }
}

fn decode_cmap(cm: &CMapCodec, bytes: &[u8], out: &mut String) {
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
                push_utf16(out, units);
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
                push_utf16(out, &[dst as u16]);
                i += n;
                matched = true;
                break;
            }
        }
        if !matched {
            // Unmapped source byte: skip rather than invent characters.
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
                let hex = data[start..end].to_vec();
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

/// Resolve a `/Differences` glyph name to its Unicode value.
/// `Some(0)` means "mapped to no character" (skip); `None` = unknown name.
fn glyph_unicode(name: &[u8]) -> Option<u16> {
    if name == b".notdef" {
        // A code explicitly mapped to /.notdef has no glyph: decode to nothing.
        return Some(0);
    }
    if let Ok(idx) = AGL_NAMES.binary_search_by(|entry: &(&[u8], u16)| entry.0.cmp(name)) {
        return Some(AGL_NAMES[idx].1);
    }
    // AGL "uniXXXX" form (4 hex digits).
    if name.len() == 7 && name.starts_with(b"uni") {
        let hex = std::str::from_utf8(&name[3..]).ok()?;
        return u16::from_str_radix(hex, 16).ok();
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

    if subtype == b"Type0" {
        // CID-keyed font: decode exclusively through its ToUnicode CMap.
        return font_to_unicode(doc, font).map(|cm| Codec::CMap(cm, None));
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
                // semantics without their font program.
                return None;
            }
            // Non-symbolic simple fonts with no declared encoding almost
            // universally use WinAnsi byte values for accented Latin text.
            ByteTable(WIN_ANSI)
        }
    };

    if let Some(cm) = font_to_unicode(doc, font) {
        return Some(Codec::CMap(cm, Some(table)));
    }
    Some(Codec::Byte8(table))
}

// ---------------------------------------------------------------------------
// Page text walker
// ---------------------------------------------------------------------------

fn ends_with_ws(s: &str) -> bool {
    s.chars().next_back().map_or(true, |c| c.is_whitespace())
}

fn push_decoded(out: &mut String, codec: &Codec, bytes: &[u8]) {
    codec.decode(bytes, out);
}

fn show_text(out: &mut String, codec: &Codec, operands: &[Object]) {
    for operand in operands {
        match operand {
            Object::String(bytes, _) => push_decoded(out, codec, bytes),
            Object::Array(items) => {
                for item in items {
                    match item {
                        Object::String(bytes, _) => push_decoded(out, codec, bytes),
                        Object::Integer(v) if *v < -100 => {
                            // Large negative kerning behaves as a word gap.
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
}

fn extract_page(
    doc: &Document,
    page_id: ObjectId,
    detect_tables: bool,
    detect_layout: bool,
) -> Result<PageText, String> {
    let fonts = doc.get_page_fonts(page_id).map_err(|e| format!("{e}"))?;
    let has_fonts = !fonts.is_empty();
    let codecs: Vec<(Vec<u8>, Codec)> = fonts
        .iter()
        .filter_map(|(name, fd)| resolve_codec(doc, fd).map(|c| (name.clone(), c)))
        .collect();

    let content: Content<Vec<Operation>> = doc
        .get_and_decode_page_content(page_id)
        .map_err(|e| format!("{e}"))?;

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
    let has_tj = content.operations.iter().any(|op| op.operator == "TJ");
    let has_tj_plain = content.operations.iter().any(|op| op.operator == "Tj");
    let has_td = content.operations.iter().any(|op| op.operator == "TD");
    let has_tm = content.operations.iter().any(|op| op.operator == "Tm");
    if has_tj && !has_tj_plain && (has_td || (detect_tables && has_tm)) {
        return crate::layout::extract_page_glyphs(doc, page_id, detect_tables, detect_layout);
    }

    let mut out = String::new();
    let mut cur: Option<usize> = None;
    let mut text_ops_seen = false;

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
    let has_tj_array = content.operations.iter().any(|op| op.operator == "TJ");
    let has_abs_pos = content
        .operations
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

    for op in &content.operations {
        match op.operator.as_str() {
            "Tf" => {
                let name = op.operands.first().and_then(|o| o.as_name().ok());
                cur = name.and_then(|nm| codecs.iter().position(|(n, _)| n == nm));
            }
            "Tm" => {
                cur_pos = pos2(op, 4);
            }
            "TD" => {
                cur_pos = pos2(op, 0);
            }
            "Tj" => {
                text_ops_seen = true;
                if let Some(ci) = cur {
                    show_pos(
                        &mut out,
                        &codecs[ci].1,
                        &op.operands,
                        pos_mode,
                        line_eps,
                        space_eps,
                        &mut cur_pos,
                        &mut prev_pos,
                        &mut last_pos_show,
                    );
                }
            }
            "TJ" => {
                text_ops_seen = true;
                if let Some(ci) = cur {
                    show_pos(
                        &mut out,
                        &codecs[ci].1,
                        &op.operands,
                        pos_mode,
                        line_eps,
                        space_eps,
                        &mut cur_pos,
                        &mut prev_pos,
                        &mut last_pos_show,
                    );
                }
            }
            "'" => {
                text_ops_seen = true;
                last_pos_show = false;
                if !ends_with_ws(&out) {
                    out.push('\n');
                }
                if let Some(ci) = cur {
                    show_text(&mut out, &codecs[ci].1, &op.operands);
                }
            }
            "\"" => {
                text_ops_seen = true;
                last_pos_show = false;
                if !ends_with_ws(&out) {
                    out.push('\n');
                }
                if let Some(ci) = cur {
                    if let Some(s) = op.operands.get(2) {
                        show_text(&mut out, &codecs[ci].1, std::slice::from_ref(s));
                    }
                }
            }
            // `ET`/`T*` mark line breaks for the string-based case. When the
            // previous show was a positionally-placed glyph, the y-jump already
            // produced the break; suppress the extra newline.
            "T*" | "ET" => {
                if !last_pos_show && !ends_with_ws(&out) {
                    out.push('\n');
                }
                if op.operator.as_str() == "T*" {
                    prev_pos = None;
                }
            }
            _ => {}
        }
    }

    Ok(PageText {
        text: out.trim_end().to_string(),
        text_ops_seen,
        has_fonts,
        tables: 0,
        blocks: Vec::new(),
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
) -> Result<PageText, String> {
    let pages: std::collections::BTreeMap<u32, ObjectId> = doc.get_pages();
    let page_id = pages
        .get(&page_number)
        .copied()
        .ok_or_else(|| format!("page {page_number} not found"))?;
    extract_page(doc, page_id, detect_tables, detect_layout)
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let codec = Codec::Byte8(ByteTable(WIN_ANSI));
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
        let codec = Codec::Byte8(t);
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
        let codec = Codec::CMap(cm, None);
        let bytes = [0x00, 0x41, 0x00, 0xE9, 0x01, 0x01];
        let mut s = String::new();
        codec.decode(&bytes, &mut s);
        assert_eq!(s, "Aéạ");
    }

    #[test]
    fn cmap_fallback_to_byte_table() {
        // 2-byte CMap that does not cover a code actually used on the page:
        // decode must fall back to the byte table so no text is lost.
        let cmap = b"beginbfchar\n<0041> <0041>\nendbfchar\n";
        let cm = parse_cmap(cmap).expect("parse");
        let codec = Codec::CMap(cm, Some(ByteTable(WIN_ANSI)));
        // 0x00E9 with a 2-byte code space, but only <0041> is mapped: the
        // byte table decodes 0xE9 -> é on its own.
        let bytes = [0x00, 0xE9];
        let mut s = String::new();
        codec.decode(&bytes, &mut s);
        assert_eq!(s, "é");
    }
}
