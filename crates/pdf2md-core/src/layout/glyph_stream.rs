// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! PDF content-stream glyph extraction, font metric resolution, and 2D span aggregation.

use std::collections::HashMap;

use lopdf::content::{Content, Operation};
use lopdf::{Dictionary, Document, Object, ObjectId};

use crate::text_extract::{deref, get_name, parse_cmap, resolve_codec, CMapCodec, Codec, PageText};
use crate::layout::latex_math::render_math;
use crate::layout::reading_order::{build_doc_blocks, page_two_columns, render_cluster, render_human_order};
use crate::layout::tables::{find_gap_tables, find_tables, render_with_tables};

// ---------------------------------------------------------------------------
// 2x3 affine matrix (PDF matrix: [a b c d e f])
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
pub(crate) struct Mtx {
    pub a: f64,
    pub b: f64,
    pub c: f64,
    pub d: f64,
    pub e: f64,
    pub f: f64,
}

impl Mtx {
    pub(crate) const ID: Self = Mtx {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: 1.0,
        e: 0.0,
        f: 0.0,
    };

    /// Construct from raw PDF matrix values [a b c d e f].
    pub(crate) fn from_parts(a: f64, b: f64, c: f64, d: f64, e: f64, f: f64) -> Self {
        Mtx { a, b, c, d, e, f }
    }

    /// `self = self × rhs` (post-multiply).
    #[allow(dead_code)]
    pub(crate) fn post_mul(&mut self, rhs: &Mtx) {
        let a = self.a * rhs.a + self.b * rhs.c;
        let b = self.a * rhs.b + self.b * rhs.d;
        let c = self.c * rhs.a + self.d * rhs.c;
        let d = self.c * rhs.b + self.d * rhs.d;
        let e = self.e * rhs.a + self.f * rhs.c + rhs.e;
        let f = self.e * rhs.b + self.f * rhs.d + rhs.f;
        *self = Mtx { a, b, c, d, e, f };
    }

    /// `self = lhs × self` (pre-multiply).
    pub(crate) fn pre_mul(&mut self, lhs: &Mtx) {
        let a = lhs.a * self.a + lhs.b * self.c;
        let b = lhs.a * self.b + lhs.b * self.d;
        let c = lhs.c * self.a + lhs.d * self.c;
        let d = lhs.c * self.b + lhs.d * self.d;
        let e = lhs.e * self.a + lhs.f * self.c + self.e;
        let f = lhs.e * self.b + lhs.f * self.d + self.f;
        *self = Mtx { a, b, c, d, e, f };
    }

    pub(crate) fn apply(&self, x: f64, y: f64) -> (f64, f64) {
        (
            self.a * x + self.c * y + self.e,
            self.b * x + self.d * y + self.f,
        )
    }

    pub(crate) fn translate(tx: f64, ty: f64) -> Self {
        Mtx {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            e: tx,
            f: ty,
        }
    }

    pub(crate) fn h_scale(&self) -> f64 {
        (self.a * self.a + self.b * self.b).sqrt()
    }
}

pub(crate) fn num(o: &Object) -> Option<f64> {
    o.as_float()
        .ok()
        .map(|v| v as f64)
        .or_else(|| o.as_i64().ok().map(|v| v as f64))
}

pub(crate) fn mtx_from(op: &Operation) -> Option<Mtx> {
    Some(Mtx {
        a: num(op.operands.get(0)?)?,
        b: num(op.operands.get(1)?)?,
        c: num(op.operands.get(2)?)?,
        d: num(op.operands.get(3)?)?,
        e: num(op.operands.get(4)?)?,
        f: num(op.operands.get(5)?)?,
    })
}

pub(crate) fn string_bytes(o: &Object) -> Option<&[u8]> {
    match o {
        Object::String(bytes, _) => Some(bytes),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Font advance widths
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub(crate) enum Widths {
    /// Simple font: byte code -> width in 1/1000 em (`/Widths` + `/FirstChar`).
    Byte([f64; 256]),
    /// Type0/CID font: CID -> width (`/W` + `/DW`), with an optional code->CID
    /// CMap (Identity-H/V uses None: the code bytes *are* the CID).
    Cid {
        map: HashMap<u32, f64>,
        default: f64,
        encoding: Option<CMapCodec>,
    },
    /// No usable metrics (e.g. base-14 fonts without embedded widths, Type3).
    None,
}

impl Widths {
    /// Advance width for a char code, in 1/1000 em units.
    pub(crate) fn width(&self, bytes: &[u8]) -> Option<f64> {
        match self {
            Widths::Byte(t) => {
                if bytes.is_empty() {
                    return None;
                }
                // Sum *every* glyph's advance. A `Tj`/`TJ` operand carries a
                // whole word or phrase, and every consumer uses
                // `span.x + span.advance` as the run's right edge. Returning
                // only the first glyph's width under-measures multi-character
                // runs, so a run that ends where an absolute `Tm` boundary
                // begins looks like it stops far short of the next span — a
                // phantom gap wide enough for the column/reading-order
                // heuristics to split single-column prose into fake columns.
                let mut total = 0.0;
                let mut any = false;
                for &b in bytes {
                    let w = t[b as usize];
                    if w > 0.0 {
                        any = true;
                        total += w;
                    }
                }
                if any {
                    Some(total)
                } else {
                    None
                }
            }
            Widths::Cid {
                map,
                default,
                encoding,
            } => {
                if bytes.is_empty() {
                    return None;
                }
                // Sum every CID's advance, exactly as the `Widths::Byte` branch
                // above does. Callers use `span.x + span.advance` as the run's
                // right edge *and* advance the text matrix by this value, so
                // folding a multi-glyph run into one merged CID (which misses
                // /W, falls back to /DW, and mis-measures every multi-glyph
                // run) drifts all later x positions and destroys residual-gap
                // space detection — fusing words in Identity-H/CID fonts.
                let mut total = 0.0;
                let mut any = false;
                match encoding {
                    Some(cm) => {
                        // Custom code->CID CMap: consume the code lengths the
                        // CMap knows, exactly as `decode_cmap` does.
                        let mut i = 0usize;
                        while i < bytes.len() {
                            let mut matched = false;
                            for len in 1u8..=4u8 {
                                let n = len as usize;
                                if i + n > bytes.len() {
                                    continue;
                                }
                                if let Some(cid) = cm.lookup(&bytes[i..i + n]) {
                                    let w = map.get(&cid).copied().unwrap_or(*default);
                                    if w > 0.0 {
                                        any = true;
                                        total += w;
                                    }
                                    i += n;
                                    matched = true;
                                    break;
                                }
                            }
                            if !matched {
                                i += 1;
                            }
                        }
                    }
                    None => {
                        // Identity-H/V: the code bytes *are* the CID, two
                        // bytes each.
                        for chunk in bytes.chunks(2) {
                            let mut cid = 0u32;
                            for &b in chunk {
                                cid = (cid << 8) | b as u32;
                            }
                            let w = map.get(&cid).copied().unwrap_or(*default);
                            if w > 0.0 {
                                any = true;
                                total += w;
                            }
                        }
                    }
                }
                if any {
                    Some(total)
                } else {
                    None
                }
            }
            Widths::None => None,
        }
    }
}

pub(crate) fn resolve_widths(doc: &Document, font: &Dictionary) -> Widths {
    let subtype = get_name(font, b"Subtype").unwrap_or(b"");

    if subtype == b"Type0" {
        let desc = font
            .get(b"DescendantFonts")
            .ok()
            .and_then(|o| deref(doc, o))
            .and_then(|o| o.as_array().ok())
            .and_then(|a| a.first())
            .and_then(|o| deref(doc, o))
            .and_then(|o| o.as_dict().ok());
        let Some(desc) = desc else {
            return Widths::None;
        };
        let default = desc
            .get(b"DW")
            .ok()
            .and_then(|o| deref(doc, o))
            .and_then(num)
            .unwrap_or(1000.0);
        let Some(w) = desc.get(b"W").ok().and_then(|o| deref(doc, o)) else {
            return Widths::None;
        };
        let Ok(arr) = w.as_array() else {
            return Widths::None;
        };
        let mut map = HashMap::new();
        // `/W` elements are routinely *indirect* objects — a single shared
        // width object referenced by every CID (and arrays referenced by the
        // `/W` entry itself). Every element must be resolved through `deref`
        // before it is read as a number or an array: otherwise `num()` sees an
        // `Object::Reference`, the whole `/W` table is silently dropped, and
        // every glyph falls back to `/DW` (1 em), inflating run advances and
        // fusing words (e.g. `| Gesamtbetrag der Zuschläge0,00 |` on the
        // intarsys EN16931 fixtures, whose `/W` is `[3 3 20 0 R 8 8 20 0 R …]`
        // with object 20 = 600 while `/DW` defaults to 1000).
        let numd = |o: &Object| -> Option<f64> { deref(doc, o).and_then(num) };
        let mut i = 0usize;
        while i < arr.len() {
            let Some(c0) = numd(&arr[i]).map(|v| v as u32) else {
                i += 1;
                continue;
            };
            i += 1;
            if i >= arr.len() {
                break;
            }
            if let Some(vals) = deref(doc, &arr[i]).and_then(|o| o.as_array().ok()) {
                for (k, it) in vals.iter().enumerate() {
                    if let Some(v) = numd(it) {
                        map.insert(c0 + k as u32, v);
                    }
                }
                i += 1;
            } else if i + 1 < arr.len() {
                if let (Some(c1), Some(v)) = (numd(&arr[i]).map(|x| x as u32), numd(&arr[i + 1])) {
                    for c in c0..=c1 {
                        map.insert(c, v);
                    }
                }
                i += 2;
            } else {
                break;
            }
        }
        // A non-identity `/Encoding` CMap stream maps code -> CID. Identity-H/V
        // are predefined names with no stream: the code bytes are the CID.
        let encoding = font
            .get(b"Encoding")
            .ok()
            .and_then(|o| deref(doc, o))
            .and_then(|o| o.as_stream().ok())
            .and_then(|s| s.get_plain_content_with_limit(16 << 20).ok())
            .and_then(|d| parse_cmap(&d));
        Widths::Cid {
            map,
            default,
            encoding,
        }
    } else {
        let first = font
            .get(b"FirstChar")
            .ok()
            .and_then(|o| deref(doc, o))
            .and_then(num)
            .unwrap_or(0.0) as i32;
        let Some(w) = font.get(b"Widths").ok().and_then(|o| deref(doc, o)) else {
            if let Some(base_font) = font
                .get(b"BaseFont")
                .ok()
                .and_then(|o| deref(doc, o))
                .and_then(|o| o.as_name().ok())
            {
                let name = String::from_utf8_lossy(base_font).to_lowercase();
                if name.contains("courier") {
                    return Widths::Byte([crate::glyph_data::COURIER_WIDTH; 256]);
                } else if name.contains("helvetica-bold")
                    || (name.contains("helvetica") && name.contains("bold"))
                    || (name.contains("arial") && name.contains("bold"))
                {
                    return Widths::Byte(crate::glyph_data::HELVETICA_BOLD_WIDTHS);
                } else if name.contains("helvetica") || name.contains("arial") {
                    return Widths::Byte(crate::glyph_data::HELVETICA_WIDTHS);
                } else if name.contains("times") {
                    return Widths::Byte(crate::glyph_data::TIMES_ROMAN_WIDTHS);
                }
            }
            return Widths::None;
        };
        let Ok(arr) = w.as_array() else {
            return Widths::None;
        };
        let mut t = [0.0f64; 256];
        for (i, item) in arr.iter().enumerate() {
            let code = first + i as i32;
            if (0..=255).contains(&code) {
                t[code as usize] = num(item).unwrap_or(0.0);
            }
        }
        Widths::Byte(t)
    }
}

// ---------------------------------------------------------------------------
// Glyph spans + clustering
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct Span {
    pub text: String,
    pub x: f64,
    pub y: f64,
    /// Effective device font size (used to scale all gap thresholds).
    pub size: f64,
    /// Total natural advance width of the decoded run, in device points.
    pub advance: f64,
    /// Advance a word-gap test must subtract to find the *whitespace* after this
    /// run: `advance` plus the character/word spacing (`Tc`/`Tw`) the content
    /// stream adds after every code. The text matrix advances by a run's glyph
    /// widths *plus* its tracking, but a producer's `Tc` (letter-spacing) is
    /// deliberately applied after each code; measuring the residual gap against
    /// the bare `advance` therefore turns the accumulated tracking of a long
    /// run into a phantom word space when the next run continues the same word
    /// at a `TJ` kerning point. Geometry (columns, tables) keeps the bare
    /// `advance`; only word-gap rendering uses this field.
    pub word_advance: f64,
    pub is_bold: bool,
    pub is_italic: bool,
    pub is_underline: bool,
    pub is_vertical: bool,
}

/// Reads a TrueType/OpenType `sfnt` font program and reports whether its real
/// weight is bold, or `None` when the program is missing or not a parseable
/// sfnt (e.g. a bare-CFF `/FontFile3` or a Type1 `/FontFile`).
///
/// The PDF `/StemV` hint is producer-controlled and is routinely wrong: the
/// ZUGFeRD/intarsys stylesheet writes the *same* `/StemV 600` (and `777`) on
/// both its Regular and Bold subsets, so a stem-width threshold alone marks
/// every run bold. The embedded `OS/2`/`head` tables carry the face's actual
/// weight and are authoritative when they are present.
fn font_program_is_bold(data: &[u8]) -> Option<bool> {
    // sfnt header: version(4) numTables(2) searchRange(2) entrySelector(2)
    // rangeShift(2), followed by 16-byte table records. Reject anything that
    // is not a plain sfnt (e.g. bare-CFF `/FontFile3`) instead of reading
    // table tags out of unrelated bytes.
    if data.len() < 12 || !matches!(&data[0..4], [0x00, 0x01, 0x00, 0x00] | b"true" | b"OTTO") {
        return None;
    }
    let num_tables = u16::from_be_bytes([data[4], data[5]]) as usize;
    let mut dir = 12usize;
    let mut head: Option<(usize, usize)> = None;
    let mut os2: Option<(usize, usize)> = None;
    for _ in 0..num_tables {
        if dir + 16 > data.len() {
            break;
        }
        let tag = &data[dir..dir + 4];
        let offset = u32::from_be_bytes([
            data[dir + 8],
            data[dir + 9],
            data[dir + 10],
            data[dir + 11],
        ]) as usize;
        let length = u32::from_be_bytes([
            data[dir + 12],
            data[dir + 13],
            data[dir + 14],
            data[dir + 15],
        ]) as usize;
        match tag {
            b"OS/2" => os2 = Some((offset, length)),
            b"head" => head = Some((offset, length)),
            _ => {}
        }
        dir += 16;
    }

    // OS/2: usWeightClass (u16 @4) and fsSelection (u16 @62, BOLD bit 0x20).
    // Either a >=600 weight or the BOLD selection bit is definitive; a
    // non-zero weight below 600 is a definitive regular face.
    if let Some((o, len)) = os2 {
        if len >= 6 && o + 6 <= data.len() {
            let weight = u16::from_be_bytes([data[o + 4], data[o + 5]]);
            if weight >= 600 {
                return Some(true);
            }
            if len >= 64 && o + 64 <= data.len() {
                let fs_selection = u16::from_be_bytes([data[o + 62], data[o + 63]]);
                if fs_selection & 0x20 != 0 {
                    return Some(true);
                }
            }
            if weight != 0 {
                return Some(false);
            }
        }
    }

    // head.macStyle (u16 @44), bit 0 = Bold.
    if let Some((o, len)) = head {
        if len >= 46 && o + 46 <= data.len() {
            let mac_style = u16::from_be_bytes([data[o + 44], data[o + 45]]);
            return Some(mac_style & 1 != 0);
        }
    }

    None
}

/// Resolves a FontDescriptor's embedded font program to its real bold flag, if
/// the descriptor carries a parseable TrueType/OpenType program.
fn embedded_font_bold(doc: &Document, fd: &Dictionary) -> Option<bool> {
    let data = fd
        .get(b"FontFile2")
        .or_else(|_| fd.get(b"FontFile3"))
        .or_else(|_| fd.get(b"FontFile"))
        .ok()
        .and_then(|o| deref(doc, o))
        .and_then(|o| o.as_stream().ok())
        .and_then(|s| s.get_plain_content_with_limit(32 << 20).ok())?;
    font_program_is_bold(&data)
}

/// Resolves whether a font dictionary represents a bold or italic typeface.
pub(crate) fn resolve_font_style(doc: &Document, font: &Dictionary) -> (bool, bool) {
    let mut is_bold = false;
    let mut is_italic = false;

    // 1. Inspect /BaseFont name
    if let Some(base_font) = font
        .get(b"BaseFont")
        .ok()
        .and_then(|o| deref(doc, o))
        .and_then(|o| o.as_name().ok())
    {
        let name = String::from_utf8_lossy(base_font).to_lowercase();
        // Widen beyond "bold"/"black"/"heavy" to the typeface conventions that
        // identify a bold face but never spell either word: URW suffixes
        // (`-Medi`, `-Bd`), Computer Modern `CMBX*`/`SFBX*` (which is what
        // `\textbf` maps to), and explicit `Semibold`/`Demi` names. Without
        // these, the whole LaTeX/academic document class loses its bold signal
        // and `detect_heading`'s H1/H3 rules never fire.
        if name.contains("bold")
            || name.contains("black")
            || name.contains("heavy")
            || name.contains("-medi")
            || name.contains("-bd")
            || name.contains("semibold")
            || name.contains("demi")
            || name.contains("cmbx")
            || name.contains("sfbx")
        {
            is_bold = true;
        }
        if name.contains("italic") || name.contains("oblique") || name.contains("slanted") {
            is_italic = true;
        }
    }

    // 2. Inspect /FontDescriptor
    let desc = font
        .get(b"FontDescriptor")
        .ok()
        .and_then(|o| deref(doc, o))
        .and_then(|o| o.as_dict().ok())
        .or_else(|| {
            font.get(b"DescendantFonts")
                .ok()
                .and_then(|o| deref(doc, o))
                .and_then(|o| o.as_array().ok())
                .and_then(|a| a.first())
                .and_then(|o| deref(doc, o))
                .and_then(|o| o.as_dict().ok())
                .and_then(|d| d.get(b"FontDescriptor").ok())
                .and_then(|o| deref(doc, o))
                .and_then(|o| o.as_dict().ok())
        });

    let mut embedded_bold: Option<bool> = None;

    if let Some(fd) = desc {
        // The embedded font program is the most reliable weight signal; the
        // `/StemV` fallback below is skipped whenever it gives an answer.
        embedded_bold = embedded_font_bold(doc, fd);
        if let Some(flags) = fd
            .get(b"Flags")
            .ok()
            .and_then(|o| deref(doc, o))
            .and_then(|o| o.as_i64().ok())
        {
            if (flags & (1 << 6)) != 0 {
                is_italic = true;
            }
            if (flags & (1 << 18)) != 0 {
                is_bold = true;
            }
        }
        if let Some(weight) = fd
            .get(b"FontWeight")
            .ok()
            .and_then(|o| deref(doc, o))
            .and_then(num)
        {
            if weight >= 600.0 {
                is_bold = true;
            }
        }
        // `/FontWeight` is rarely present in embedded subsets. `/StemV` (the
        // vertical stem width in 1/1000 em) is a practical bold signal in the
        // wild — roman faces sit near 70–90, bold faces ~120+ — but it is only
        // a fallback: a producer that writes one out-of-range StemV on every
        // subset (intarsys/ZUGFeRD emits 600 and 777 on its Regular face too)
        // would otherwise mark all text bold. When the embedded program gave a
        // definitive weight, that answer wins.
        if embedded_bold.is_none() {
            if let Some(stemv) = fd
                .get(b"StemV")
                .ok()
                .and_then(|o| deref(doc, o))
                .and_then(num)
            {
                if stemv >= 120.0 {
                    is_bold = true;
                }
            }
        }
    }

    if embedded_bold == Some(true) {
        is_bold = true;
    }

    (is_bold, is_italic)
}

/// Fold the Latin presentation-form ligatures (U+FB00–U+FB06) to their letter
/// sequences. A font's `/ToUnicode` map is allowed to return the ligature
/// codepoint itself, so "fi scal" comes out as "ﬁ scal"; the ligature carries
/// no meaning a reader wants and breaks text search, so normalise it on the
/// way out. `advance` is measured from the raw glyph widths and is unaffected.
fn fold_ligatures(s: &mut String) {
    if !s.chars().any(|c| ('\u{FB00}'..='\u{FB06}').contains(&c)) {
        return;
    }
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\u{FB00}' => out.push_str("ff"),
            '\u{FB01}' => out.push_str("fi"),
            '\u{FB02}' => out.push_str("fl"),
            '\u{FB03}' => out.push_str("ffi"),
            '\u{FB04}' => out.push_str("ffl"),
            '\u{FB05}' | '\u{FB06}' => out.push_str("st"),
            _ => out.push(c),
        }
    }
    *s = out;
}

/// Decode one string operand into a positioned span (or nothing if it decodes
/// to no text). `em_offset` is a preceding TJ-array number in 1/1000 em units.
///
/// Returns the reading direction of a *vertical* span: `Some(1)` when the glyph
/// advance runs upward (bottom-to-top), `Some(-1)` when it runs downward, and
/// `None` for a horizontal span (or empty text). The caller uses this to decide
/// whether a page whose text is dominantly vertical must be rotated upright;
/// the sign is returned rather than stored on `Span` so the ~40 `Span` literals
/// in the tree stay untouched.
pub(crate) fn push_span(
    codec: &Codec,
    width: &Widths,
    bytes: &[u8],
    em_offset: f64,
    tm: &Mtx,
    ctm: &Mtx,
    tfs: f64,
    tc: f64,
    tw: f64,
    style: (bool, bool),
    spans: &mut Vec<Span>,
) -> Option<i8> {
    let mut text = String::new();
    // Raw decode: keep private-use code points so the span geometry and table
    // detection below see exactly the pre-mapping bytes. `normalize_decoded_text`
    // maps them afterwards through the font-unknown union.
    codec.decode_raw(bytes, &mut text);
    fold_ligatures(&mut text);
    if text.is_empty() {
        return None;
    }
    let hscale = tm.h_scale() * ctm.h_scale();
    let size = tfs * hscale;
    let (ux, uy) = tm.apply(em_offset / 1000.0 * tfs, 0.0);
    let (x, y) = ctm.apply(ux, uy);
    let advance = width
        .width(bytes)
        .map(|w| w / 1000.0 * tfs * hscale)
        .unwrap_or_else(|| {
            // No metrics: assume a typical letter advance (~0.5 em) so the
            // gap detector still separates words reasonably.
            0.5 * size
        });
    // Text-matrix advance of this run including `Tc`/`Tw`, in the same device
    // units as `span.x`. `Tc`/`Tw` are added after every code and are *not*
    // part of the glyph outline width, so a TJ run split mid-word would look
    // like it stops short of the next fragment and get a spurious space.
    //
    // Count *decoded characters*, not raw bytes: a 2-byte CID / Identity-H /
    // UTF-16 font has `bytes.len() == 2 * glyphs`, and counting bytes inflated
    // the `Tc` term by 2x. That made `word_advance` overshoot the run's true
    // right edge, so the residual gap to the next run was under-measured and
    // real inter-word spaces were dropped (`de`+`la` -> `dela`, D2).
    let nchars = text.chars().count() as f64;
    let nspaces = text.chars().filter(|c| *c == ' ').count() as f64;
    let word_advance = advance + (tc * nchars + tw * nspaces) * hscale;
    let eff_a = ctm.a * tm.a + ctm.c * tm.b;
    let eff_b = ctm.b * tm.a + ctm.d * tm.b;
    let is_vertical = eff_b.abs() > 0.7 * hscale && eff_a.abs() < 0.3 * hscale;
    spans.push(Span {
        text,
        x,
        y,
        size,
        advance,
        word_advance,
        is_bold: style.0,
        is_italic: style.1,
        is_underline: false,
        is_vertical,
    });
    if is_vertical {
        Some(if eff_b >= 0.0 { 1 } else { -1 })
    } else {
        None
    }
}

/// Height of the page media box in device points (best effort), accounting for page rotation.
pub fn page_height_of(doc: &Document, page_id: ObjectId) -> Option<f64> {
    let (_, h) = page_initial_transform(doc, page_id);
    Some(h)
}

/// Resolve an inheritable page attribute (`/Rotate`, `/MediaBox`, `/CropBox`)
/// by climbing the `/Pages` parent chain, cycle- and depth-bounded. PDF viewers
/// inherit these from ancestors, but the glyph engine previously read only the
/// page dictionary; a page that inherits its `/Rotate` or box from `/Pages`
/// therefore got the wrong orientation/geometry.
fn inherited_page_object<'a>(
    doc: &'a Document,
    page_id: ObjectId,
    key: &[u8],
) -> Option<&'a Object> {
    let mut seen: Vec<ObjectId> = Vec::new();
    let mut cur = Some(page_id);
    while let Some(id) = cur {
        if seen.len() >= 32 || seen.contains(&id) {
            break;
        }
        seen.push(id);
        let Ok(dict) = doc.get_dictionary(id) else { break };
        if let Ok(o) = dict.get(key) {
            return deref(doc, o);
        }
        cur = dict.get(b"Parent").ok().and_then(|o| o.as_reference().ok());
    }
    None
}

/// Initial coordinate transformation matrix and display height for a page,
/// taking into account the page /Rotate attribute (0, 90, 180, 270 degrees clockwise)
/// and /MediaBox / /CropBox boundaries. All three are inherited from the
/// `/Pages` ancestors when absent on the page itself.
pub(crate) fn page_initial_transform(doc: &Document, page_id: ObjectId) -> (Mtx, f64) {
    let rotate = inherited_page_object(doc, page_id, b"Rotate")
        .and_then(|o| o.as_i64().ok())
        .unwrap_or(0);
    let rotate = ((rotate % 360) + 360) % 360;

    let box_obj = inherited_page_object(doc, page_id, b"CropBox")
        .or_else(|| inherited_page_object(doc, page_id, b"MediaBox"))
        .and_then(|o| o.as_array().ok());

    let (x0, y0, x1, y1) = if let Some(b) = box_obj {
        let g = |i: usize| -> f64 {
            b.get(i)
                .and_then(|o| deref(doc, o))
                .and_then(|o| o.as_float().ok().map(|f| f as f64).or_else(|| o.as_i64().ok().map(|v| v as f64)))
                .unwrap_or(0.0)
        };
        (g(0), g(1), g(2), g(3))
    } else {
        (0.0, 0.0, 595.0, 842.0)
    };

    let w = (x1 - x0).abs();
    let h = (y1 - y0).abs();

    match rotate {
        90 => (Mtx::from_parts(0.0, -1.0, 1.0, 0.0, -y0, x1), w),
        180 => (Mtx::from_parts(-1.0, 0.0, 0.0, -1.0, x1, y1), h),
        270 => (Mtx::from_parts(0.0, 1.0, -1.0, 0.0, y1, -x0), w),
        _ => (Mtx::ID, h),
    }
}

/// Group consecutive spans into runs by device baseline y, preserving
/// stream order, then merge adjacent same-visual-line runs and sort lines
/// top-to-bottom. This is the line model used both for the plain text renderer
/// and for table detection.
pub fn build_lines(spans: &[Span]) -> Vec<Vec<Span>> {
    if spans.is_empty() {
        return Vec::new();
    }

    // Group consecutive spans into runs by device baseline y, preserving
    // stream order.
    let mut runs: Vec<Vec<Span>> = Vec::new();
    for span in spans {
        let eps = 0.5 * span.size.max(0.1);
        match runs.last_mut() {
            Some(last) if (span.y - last.last().unwrap().y).abs() <= eps => last.push(span.clone()),
            _ => runs.push(vec![span.clone()]),
        }
    }

    // Sort all runs top-to-bottom by device baseline y.
    runs.sort_by(|a, b| {
        b[0].y
            .partial_cmp(&a[0].y)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    // Merge runs that sit on the same visual baseline y (after sorting by y,
    // runs from different columns on the same row are now adjacent).
    let mut lines: Vec<Vec<Span>> = Vec::new();
    for run in runs {
        let eps = 0.5 * run[0].size.max(0.1);
        match lines.last_mut() {
            Some(last) if (run[0].y - last[0].y).abs() <= eps => {
                last.extend(run);
            }
            _ => lines.push(run),
        }
    }

    // Within each visual line, sort spans left-to-right by x, using stable sort
    // to preserve stream order for glyphs sharing the same x position.
    for line in &mut lines {
        line.sort_by(|a, b| {
            a.x.partial_cmp(&b.x).unwrap_or(std::cmp::Ordering::Equal)
        });

        // Deduplicate overstrike / shadow spans (faux-bolding):
        // In many PDFs (invoices, pay slips, forms), bold text is created by drawing identical
        // glyphs twice at the same or micro-shifted coordinates (dx <= 0.75 pt or 0.2 * size).
        let mut deduped: Vec<Span> = Vec::with_capacity(line.len());
        for span in line.drain(..) {
            let is_duplicate = if let Some(prev) = deduped.last_mut() {
                if prev.text == span.text {
                    let dx = (span.x - prev.x).abs();
                    let dy = (span.y - prev.y).abs();
                    let max_dx = (0.2 * span.size).max(0.75);
                    if dx <= max_dx && dy <= 0.75 {
                        prev.is_bold = true;
                        true
                    } else {
                        false
                    }
                } else {
                    false
                }
            } else {
                false
            };
            if !is_duplicate {
                deduped.push(span);
            }
        }
        *line = deduped;
    }

    lines
}

/// Append one `table` zone to `blocks` per recovered canvas grid, and drop the
/// reading-order fragments (body/list/caption) that fall entirely inside a
/// recovered table bbox so the zone view shows the table as a single region
/// rather than scattered cells. The table zone carries a compact summary in its
/// `text` field (row × column count and a leading non-empty cell).
fn append_table_zones(blocks: &mut Vec<crate::layout::reading_order::DocBlock>, hits: &[crate::layout::tables::TableHit]) {
    use crate::layout::reading_order::DocBlock;
    let original_len = blocks.len();
    let mut contained = vec![false; original_len];

    for hit in hits {
        let tx0 = hit.bbox.x0.min(hit.bbox.x1);
        let tx1 = hit.bbox.x0.max(hit.bbox.x1);
        let ty0 = hit.bbox.y0.min(hit.bbox.y1);
        let ty1 = hit.bbox.y0.max(hit.bbox.y1);
        let rows = hit.rows.len();
        let cols = hit.rows.iter().map(|r| r.len()).max().unwrap_or(0);
        let label = hit
            .rows
            .first()
            .and_then(|r| r.iter().find(|c| !c.trim().is_empty()))
            .cloned()
            .unwrap_or_default();
        let text = format!("table · {} rows × {} cols · {}", rows, cols, label);
        blocks.push(DocBlock {
            page: 0,
            kind: "table".to_string(),
            x0: tx0,
            y0: ty0,
            x1: tx1,
            y1: ty1,
            text,
            is_bold: false,
            is_italic: false,
            is_underline: false,
        });
        for (i, b) in blocks.iter().take(original_len).enumerate() {
            if contained[i] {
                continue;
            }
            // `hit.bbox` is built from span *baselines* (`min_y`/`max_y` of
            // `sp.y` in `find_tables`), while `build_doc_blocks` pads every
            // block by half its font size above/below its baseline band. A
            // strict bbox containment test therefore never matches a fragment
            // sitting on the table's first or last row — it always overhangs
            // the bbox by ~0.5em — so those cell fragments (and any
            // cross-column paragraph merge inside the grid) leak into the zone
            // list beside the table zone instead of being collapsed into it.
            // Compare the fragment's centre, which is padding-independent,
            // against the baseline bbox; the small epsilon absorbs the
            // half-em overhang on a single-row fragment whose centre lands
            // exactly on the bbox edge.
            const EDGE_EPS: f64 = 0.5;
            let cx = 0.5 * (b.x0 + b.x1);
            let cy = 0.5 * (b.y0 + b.y1);
            let inside = cx >= tx0 - EDGE_EPS
                && cx <= tx1 + EDGE_EPS
                && cy >= ty0 - EDGE_EPS
                && cy <= ty1 + EDGE_EPS;
            if inside && matches!(b.kind.as_str(), "body" | "list" | "caption") {
                contained[i] = true;
            }
        }
    }

    if contained.iter().any(|c| *c) {
        let mut idx = 0;
        blocks.retain(|_| {
            let keep = if idx < original_len {
                !contained[idx]
            } else {
                true // newly pushed "table" zones are always kept
            };
            idx += 1;
            keep
        });
    }
}

// ---------------------------------------------------------------------------
// Underline (thin horizontal rule) detection
// ---------------------------------------------------------------------------

/// Record a thin horizontal rule between device points `p` and `q` as a
/// candidate underline segment `(y, x0, x1)`.
fn record_horiz_seg(out: &mut Vec<(f64, f64, f64)>, p: (f64, f64), q: (f64, f64)) {
    let (x0, y0) = p;
    let (x1, y1) = q;
    if (y1 - y0).abs() < 0.6 {
        let w = (x1 - x0).abs();
        if w >= 1.5 {
            out.push((y0, x0.min(x1), x0.max(x1)));
        }
    }
}

/// Flush the current path as consecutive line segments into `out`, keeping only
/// thin horizontal rules. `close` wraps the last point back to the path start.
fn flush_path_segs(
    path: &mut Vec<(f64, f64)>,
    start: Option<(f64, f64)>,
    out: &mut Vec<(f64, f64, f64)>,
    close: bool,
) {
    // A genuine underline is a *thin* horizontal rule. A taller path is a
    // rectangle border — e.g. the hyperlink annotation box a producer draws with
    // `m`/`l` around a link — whose top and bottom edges would otherwise each be
    // recorded as an "underline". On the FR "Statut EI" ACRE slide the URL link
    // box's top edge sat a couple of points below the *previous* text line's
    // baseline, so it underlined " au plus tard dans les 45 jours suiv" even
    // though only `l'URSSAF` and the URL are underlined on the page. Reject a
    // path whose overall vertical extent is not that of a rule, using the same
    // < 2pt threshold the thin-filled-`re` branch already applies.
    let mut ymin = f64::INFINITY;
    let mut ymax = f64::NEG_INFINITY;
    for &(_, y) in path.iter() {
        ymin = ymin.min(y);
        ymax = ymax.max(y);
    }
    if close {
        if let Some((_, y)) = start {
            ymin = ymin.min(y);
            ymax = ymax.max(y);
        }
    }
    if !path.is_empty() && ymax - ymin < 2.0 {
        for w in path.windows(2) {
            record_horiz_seg(out, w[0], w[1]);
        }
        if close {
            if let Some(s) = start {
                if let Some(&last) = path.last() {
                    record_horiz_seg(out, last, s);
                }
            }
        }
    }
    path.clear();
}

/// Mark the spans of each (non-table) visual line that sit directly above a
/// thin horizontal rule as underlined. A rule qualifies when its device `y` sits
/// a small distance below the span's baseline (PDF y-axis up) and it horizontally
/// overlaps the span.
fn mark_underlines(
    lines: &mut [Vec<Span>],
    segs: &[(f64, f64, f64)],
    covered_lines: &std::collections::HashSet<usize>,
) {
    for (li, line) in lines.iter_mut().enumerate() {
        if covered_lines.contains(&li) {
            continue;
        }
        for span in line.iter_mut() {
            if span.is_underline || span.is_vertical {
                continue;
            }
            let size = span.size.max(0.1);
            let baseline = span.y;
            let sx0 = span.x;
            let sx1 = span.x + span.advance;
            let span_w = (sx1 - sx0).max(0.1);
            for &(sy, rx0, rx1) in segs {
                let d = baseline - sy; // positive when the rule is below the baseline
                if !(0.05 * size..=0.5 * size).contains(&d) {
                    continue;
                }
                let overlap = (sx1.min(rx1) - sx0.max(rx0)).max(0.0);
                // A rule that hugs the span horizontally, or a rule of about the
                // same width as the span that overlaps it.
                let wide_enough = overlap / span_w >= 0.5;
                let similar_width = (rx1 - rx0).abs() - span_w <= 0.4 * size;
                if wide_enough || (similar_width && overlap > 0.0) {
                    span.is_underline = true;
                    break;
                }
            }
        }
    }
}

/// Append extracted vertical margin text to the page body text.
///
/// Vertical runs are side furniture (arXiv side stamps, running headers): when
/// layout analysis / human reading order is enabled they must stay out of the
/// body prose, or the stamp is injected mid-paragraph. The caller still emits
/// the margin blocks for zone inspectors either way. Without layout analysis
/// the legacy append behavior is kept.
fn append_vertical_text(text: &mut String, vertical_text: &str, detect_layout: bool) {
    if vertical_text.is_empty() || detect_layout {
        return;
    }
    if !text.is_empty() {
        if text.ends_with('\n') {
            text.push('\n');
        } else {
            text.push_str("\n\n");
        }
    }
    text.push_str(vertical_text);
}

/// A font resolved for one content-stream resource chain: its resource name,
/// decoder, advance widths, and bold/italic style bits.
#[derive(Clone)]
struct GlyphFontInfo {
    name: Vec<u8>,
    codec: Codec,
    widths: Widths,
    style: (bool, bool),
}

/// Resolve every collected `/Font` into a [`GlyphFontInfo`].
fn build_glyph_font_table<'a>(
    doc: &'a Document,
    fonts: &std::collections::BTreeMap<Vec<u8>, &'a Dictionary>,
) -> Vec<GlyphFontInfo> {
    let mut out: Vec<GlyphFontInfo> = Vec::new();
    for (name, fd) in fonts {
        if let Some(c) = resolve_codec(doc, fd) {
            out.push(GlyphFontInfo {
                name: name.clone(),
                codec: c,
                widths: resolve_widths(doc, fd),
                style: resolve_font_style(doc, fd),
            });
        }
    }
    out
}

/// Collect the `/Font` entries reachable from `chain` and resolve them.
fn glyph_font_table_for_chain<'a>(
    doc: &'a Document,
    chain: &[&'a Dictionary],
) -> Vec<GlyphFontInfo> {
    let mut fonts: std::collections::BTreeMap<Vec<u8>, &Dictionary> =
        std::collections::BTreeMap::new();
    crate::text_extract::collect_fonts(doc, chain, &mut fonts);
    build_glyph_font_table(doc, &fonts)
}

/// Work / recursion bounds for Form XObject expansion in the glyph engine. A
/// form graph is a DAG at best and can be a near-exponential tree; each bound
/// below independently caps the blow-up. The decoded-byte and operator budgets
/// are shared across the whole page, so a DAG of forms each issuing `Do` many
/// times cannot multiply work.
const MAX_FORM_DEPTH: usize = 8;
/// Maximum number of `Do` invocations expanded for one page (16384 = 2^14).
///
/// Rationale: a per-page cost backstop, not a content limit. Every invocation
/// is also charged `text_extract::FORM_INVOCATION_OPS` against the shared
/// `MAX_FORM_OPS_TOTAL` (8 M-operator) budget and its decoded bytes against
/// `MAX_FORM_BYTES_TOTAL` (64 MiB), so an op-heavy or byte-heavy fan-out is
/// bounded by those budgets; a self-referential or exponentially expanding
/// form DAG is bounded by `MAX_FORM_DEPTH` plus the per-path cycle check, and
/// therefore never reaches this count. The cap only stops a pathological page
/// of > 16384 trivial forms; when it (or any other bound) trips,
/// [`GlyphBudget::exhausted`] is set so the truncation is reported as
/// `budget_exhausted` instead of being silent.
const MAX_FORM_DO_PER_PAGE: usize = 16384;
const MAX_FORM_OPS_TOTAL: usize = 8_000_000;
const MAX_FORM_BYTES_TOTAL: usize = 64 << 20;

struct GlyphBudget {
    do_left: usize,
    ops_left: usize,
    bytes_left: usize,
    /// Set when any bound above tripped, so a truncated page is reported.
    exhausted: bool,
}

impl GlyphBudget {
    fn new() -> Self {
        GlyphBudget {
            do_left: MAX_FORM_DO_PER_PAGE,
            ops_left: MAX_FORM_OPS_TOTAL,
            bytes_left: MAX_FORM_BYTES_TOTAL,
            exhausted: false,
        }
    }
}

/// The device-space bounding rectangle of a form's `/BBox` under `fctm`, used
/// to drop spans the form paints outside its own clip (the spec makes `/BBox`
/// clip the form's content). `None` when the form has no usable `/BBox`.
fn form_device_bbox(doc: &Document, form: &Dictionary, fctm: &Mtx) -> Option<(f64, f64, f64, f64)> {
    let b = form
        .get(b"BBox")
        .ok()
        .and_then(|o| deref(doc, o))
        .and_then(|o| o.as_array().ok())?;
    let g = |i: usize| -> Option<f64> { b.get(i).and_then(|o| deref(doc, o)).and_then(num) };
    let (llx, lly, urx, ury) = (g(0)?, g(1)?, g(2)?, g(3)?);
    if ![llx, lly, urx, ury].iter().all(|v| v.is_finite()) {
        return None;
    }
    let mut x0 = f64::INFINITY;
    let mut x1 = f64::NEG_INFINITY;
    let mut y0 = f64::INFINITY;
    let mut y1 = f64::NEG_INFINITY;
    for (x, y) in [(llx, lly), (urx, lly), (urx, ury), (llx, ury)] {
        let (dx, dy) = fctm.apply(x, y);
        x0 = x0.min(dx);
        x1 = x1.max(dx);
        y0 = y0.min(dy);
        y1 = y1.max(dy);
    }
    Some((x0, y0, x1, y1))
}

/// One content-stream walk's output: positioned spans, underline rules, and the
/// character counts of vertical runs by reading direction (upward / downward).
/// The direction counts are what let the page-level caller detect a dominantly
/// vertical page and rotate it upright.
struct GlyphWalk {
    spans: Vec<Span>,
    underline_segs: Vec<(f64, f64, f64)>,
    vertical_up_chars: usize,
    vertical_down_chars: usize,
}

/// Recursively walk one content stream (page or Form XObject) into positioned
/// spans, expanding `/Do` forms with their own `/Matrix`, `/Resources` font
/// table and `/BBox`. Depth, `Do` count, decoded bytes and total operators are
/// shared across the page (see [`GlyphBudget`]).
#[allow(clippy::too_many_arguments)]
fn walk_glyphs(
    doc: &Document,
    chain: &[&Dictionary],
    ops: &[Operation],
    init_ctm: Mtx,
    form_path: &mut Vec<ObjectId>,
    depth: usize,
    text_ops_seen: &mut bool,
    budget: &mut GlyphBudget,
    font_cache: &mut HashMap<usize, Vec<GlyphFontInfo>>,
) -> GlyphWalk {
    // Font table for this frame, cached by the resource chain's head pointer so
    // a repeated form does not re-resolve its codecs/widths on every `Do`.
    let font_key = chain
        .first()
        .map(|d| *d as *const Dictionary as usize)
        .unwrap_or(0);
    if !font_cache.contains_key(&font_key) {
        let table = glyph_font_table_for_chain(doc, chain);
        font_cache.insert(font_key, table);
    }
    let fonts_info: Vec<GlyphFontInfo> = font_cache.get(&font_key).cloned().unwrap_or_default();

    let mut ctm = init_ctm;
    let mut ctm_stack: Vec<Mtx> = Vec::new();
    let mut tlm = Mtx::ID;
    let mut tm = Mtx::ID;
    let mut tfs = 0.0f64;
    let mut leading = 0.0f64;
    let mut tc = 0.0f64;
    let mut tw = 0.0f64;
    let mut tz = 100.0f64;
    let mut cur_font: Option<usize> = None;
    let mut spans: Vec<Span> = Vec::new();
    let mut vertical_up_chars = 0usize;
    let mut vertical_down_chars = 0usize;

    // Underline detection: PDF fonts carry no underline bit, so an underline is
    // a thin horizontal stroke (or an equally thin filled rectangle) painted
    // just below a run's baseline. We collect those candidate "underline
    // segments" (device y, x0, x1) while walking the content stream, then match
    // them to spans after the visual lines are built.
    let mut underline_segs: Vec<(f64, f64, f64)> = Vec::new();
    let mut path_pts: Vec<(f64, f64)> = Vec::new();
    let mut path_start: Option<(f64, f64)> = None;

    for op in ops {
        if budget.ops_left == 0 {
            budget.exhausted = true;
            break;
        }
        budget.ops_left -= 1;
        match op.operator.as_str() {
            "q" => ctm_stack.push(ctm),
            "Q" => {
                if let Some(m) = ctm_stack.pop() {
                    ctm = m;
                }
            }
            "cm" => {
                if let Some(m) = mtx_from(op) {
                    ctm.pre_mul(&m);
                }
            }
            "BT" => {
                tlm = Mtx::ID;
                tm = Mtx::ID;
            }
            "Tm" => {
                if let Some(m) = mtx_from(op) {
                    tlm = m;
                    tm = m;
                }
            }
            "Td" | "TD" => {
                if let (Some(tx), Some(ty)) = (
                    num(op.operands.first().unwrap_or(&Object::Null)),
                    num(op.operands.get(1).unwrap_or(&Object::Null)),
                ) {
                    tlm.pre_mul(&Mtx::translate(tx, ty));
                    tm = tlm;
                    if op.operator == "TD" {
                        leading = -ty;
                    }
                }
            }
            "T*" => {
                tlm.pre_mul(&Mtx::translate(0.0, -leading));
                tm = tlm;
            }
            "TL" => {
                if let Some(v) = op.operands.first().and_then(num) {
                    leading = v;
                }
            }
            "Tc" => {
                if let Some(v) = op.operands.first().and_then(num) {
                    tc = v;
                }
            }
            "Tw" => {
                if let Some(v) = op.operands.first().and_then(num) {
                    tw = v;
                }
            }
            "Tz" => {
                if let Some(v) = op.operands.first().and_then(num) {
                    tz = v;
                }
            }
            "Tf" => {
                let name = op.operands.first().and_then(|o| o.as_name().ok());
                cur_font = name.and_then(|nm| fonts_info.iter().position(|f| f.name.as_slice() == nm));
                if let Some(sz) = op.operands.get(1).and_then(num) {
                    tfs = sz;
                }
            }
            "Tj" | "'" | "\"" => {
                *text_ops_seen = true;
                let str_idx = if op.operator == "\"" { 2 } else { 0 };
                if op.operator == "'" {
                    tlm.pre_mul(&Mtx::translate(0.0, -leading));
                    tm = tlm;
                }
                if let (Some(ci), Some(bytes)) =
                    (cur_font, op.operands.get(str_idx).and_then(string_bytes))
                {
                    let f = &fonts_info[ci];
                    let (codec, width, style) = (&f.codec, &f.widths, f.style);
                    if let Some(dir) =
                        push_span(codec, width, bytes, 0.0, &tm, &ctm, tfs, tc, tw, style, &mut spans)
                    {
                        let n = spans.last().map(|s| s.text.chars().count()).unwrap_or(0);
                        if dir > 0 {
                            vertical_up_chars += n;
                        } else {
                            vertical_down_chars += n;
                        }
                    }
                    let w = width.width(bytes).unwrap_or(500.0 * bytes.len() as f64);
                    let adv = if tc != 0.0 || tw != 0.0 || (tz - 100.0).abs() >= 1e-4 {
                        let space_count = bytes.iter().filter(|&&b| b == b' ').count() as f64;
                        let char_count = bytes.len() as f64;
                        ((w / 1000.0 * tfs) + tc * char_count + tw * space_count) * (tz / 100.0)
                    } else {
                        w / 1000.0 * tfs
                    };
                    tm.pre_mul(&Mtx::translate(adv, 0.0));
                }
            }
            "TJ" => {
                *text_ops_seen = true;
                let Some(arr) = op.operands.first().and_then(|o| o.as_array().ok()) else {
                    continue;
                };
                let Some(ci) = cur_font else { continue };
                let f = &fonts_info[ci];
                let (codec, width, style) = (&f.codec, &f.widths, f.style);
                // `offset` is the running horizontal displacement from the
                // current text position, in 1/1000 em units. A TJ-array number
                // N is *subtracted* from the position (positive N moves left),
                // and each string advances by its glyph width.
                let mut offset = 0.0f64;
                for item in arr {
                    match item {
                        Object::String(bytes, _) => {
                            if let Some(dir) = push_span(
                                codec, width, bytes, offset, &tm, &ctm, tfs, tc, tw, style,
                                &mut spans,
                            ) {
                                let n = spans.last().map(|s| s.text.chars().count()).unwrap_or(0);
                                if dir > 0 {
                                    vertical_up_chars += n;
                                } else {
                                    vertical_down_chars += n;
                                }
                            }
                            let w = width.width(bytes).unwrap_or(500.0);
                            let item_adv = if (tc != 0.0 || tw != 0.0) && tfs > 0.0 {
                                let space_count = bytes.iter().filter(|&&b| b == b' ').count() as f64;
                                let char_count = bytes.len() as f64;
                                w + (tc * char_count + tw * space_count) / tfs * 1000.0
                            } else {
                                w
                            };
                            offset += item_adv;
                        }
                        Object::Integer(v) => offset -= *v as f64,
                        Object::Real(v) => offset -= *v as f64,
                        _ => {}
                    }
                }
                let adv = if (tz - 100.0).abs() >= 1e-4 {
                    (offset / 1000.0 * tfs) * (tz / 100.0)
                } else {
                    offset / 1000.0 * tfs
                };
                tm.pre_mul(&Mtx::translate(adv, 0.0));
            }
            // --- Underline-candidate path tracking (thin horizontal rules) ---
            "m" => {
                flush_path_segs(&mut path_pts, path_start, &mut underline_segs, false);
                if let (Some(x), Some(y)) =
                    (op.operands.first().and_then(num), op.operands.get(1).and_then(num))
                {
                    let d = ctm.apply(x, y);
                    path_start = Some(d);
                    path_pts.push(d);
                }
            }
            "l" => {
                if let (Some(x), Some(y)) =
                    (op.operands.first().and_then(num), op.operands.get(1).and_then(num))
                {
                    path_pts.push(ctm.apply(x, y));
                }
            }
            "c" => {
                if let (Some(x), Some(y)) =
                    (op.operands.get(4).and_then(num), op.operands.get(5).and_then(num))
                {
                    path_pts.push(ctm.apply(x, y));
                }
            }
            "v" | "y" => {
                if let (Some(x), Some(y)) =
                    (op.operands.get(2).and_then(num), op.operands.get(3).and_then(num))
                {
                    path_pts.push(ctm.apply(x, y));
                }
            }
            "h" => {
                if let Some(s) = path_start {
                    path_pts.push(s);
                }
            }
            "re" => {
                flush_path_segs(&mut path_pts, path_start, &mut underline_segs, false);
                if let (Some(x), Some(y), Some(w), Some(h)) = (
                    op.operands.first().and_then(num),
                    op.operands.get(1).and_then(num),
                    op.operands.get(2).and_then(num),
                    op.operands.get(3).and_then(num),
                ) {
                    let p0 = ctm.apply(x, y);
                    let p1 = ctm.apply(x + w, y);
                    let p2 = ctm.apply(x + w, y + h);
                    let p3 = ctm.apply(x, y + h);
                    let x0 = p0.0.min(p1.0).min(p2.0).min(p3.0);
                    let x1 = p0.0.max(p1.0).max(p2.0).max(p3.0);
                    let y0 = p0.1.min(p1.1).min(p2.1).min(p3.1);
                    let y1 = p0.1.max(p1.1).max(p2.1).max(p3.1);
                    if (y1 - y0).abs() < 2.0 && (x1 - x0).abs() >= 1.5 {
                        // A thin filled rectangle beneath text is an underline
                        // rule; record its bottom edge (device y is smaller).
                        underline_segs.push((y0.min(y1), x0.min(x1), x0.max(x1)));
                    }
                }
            }
            "S" | "s" | "B" | "B*" | "b" | "b*" => {
                flush_path_segs(&mut path_pts, path_start, &mut underline_segs, true);
            }
            "f" | "F" | "f*" => {
                flush_path_segs(&mut path_pts, path_start, &mut underline_segs, true);
            }
            "n" => path_pts.clear(),
            // Form XObject text. Fonts come from the form's own resource chain
            // (or the enclosing chain when it declares none); the form `/Matrix`
            // is folded into the CTM exactly as the media walkers do.
            "Do" => {
                if budget.do_left == 0 || depth >= MAX_FORM_DEPTH {
                    budget.exhausted = true;
                    continue;
                }
                let Some(name) = op.operands.first().and_then(|o| o.as_name().ok()) else {
                    continue;
                };
                let Some((id, form)) = crate::text_extract::lookup_form(doc, chain, name) else {
                    continue;
                };
                // A form already on the current path draws itself (directly or
                // through a chain); skip rather than re-walk.
                if id.map_or(false, |id| form_path.contains(&id)) {
                    continue;
                }
                let fchain = crate::text_extract::form_resource_chain(doc, &form.dict, chain);
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
                budget.ops_left = budget
                    .ops_left
                    .saturating_sub(crate::text_extract::FORM_INVOCATION_OPS);
                if budget.ops_left == 0 {
                    budget.exhausted = true;
                }
                let mut fctm = ctm;
                if let Ok(m) = form.dict.get(b"Matrix") {
                    if let Ok(arr) = m.as_array() {
                        let g = |i: usize| -> f64 { arr.get(i).and_then(num).unwrap_or(0.0) };
                        fctm.pre_mul(&Mtx::from_parts(g(0), g(1), g(2), g(3), g(4), g(5)));
                    }
                }
                let clip = form_device_bbox(doc, &form.dict, &fctm);
                budget.do_left -= 1;
                if let Some(id) = id {
                    form_path.push(id);
                }
                let sub = walk_glyphs(
                    doc,
                    &fchain,
                    &fc.operations,
                    fctm,
                    form_path,
                    depth + 1,
                    text_ops_seen,
                    budget,
                    font_cache,
                );
                if id.is_some() {
                    form_path.pop();
                }
                match clip {
                    Some((x0, y0, x1, y1)) => {
                        // `/BBox` clips the form's painted content; keep only
                        // spans whose baseline falls inside it (1 pt slack).
                        const SLACK: f64 = 1.0;
                        for s in sub.spans {
                            if s.x >= x0 - SLACK
                                && s.x <= x1 + SLACK
                                && s.y >= y0 - SLACK
                                && s.y <= y1 + SLACK
                            {
                                spans.push(s);
                            }
                        }
                    }
                    None => spans.extend(sub.spans),
                }
                underline_segs.extend(sub.underline_segs);
                vertical_up_chars += sub.vertical_up_chars;
                vertical_down_chars += sub.vertical_down_chars;
            }
            _ => {}
        }
    }

    GlyphWalk {
        spans,
        underline_segs,
        vertical_up_chars,
        vertical_down_chars,
    }
}

/// Text-show / text-positioning operators seen on a page, including inside its
/// Form XObjects.
///
/// `text_extract::extract_page`'s legacy routing inspects only the *page's own*
/// content stream, so a page whose text lives entirely in a form
/// (`/Fm0 Do`, e.g. a corpus file) is never routed to the glyph engine and
/// gets no table recovery. Callers can OR these form-aware signals into that
/// routing decision. The scan is bounded exactly like `walk_glyphs`.
#[derive(Default, Clone, Copy, Debug)]
#[allow(dead_code)]
pub(crate) struct ContentSignals {
    pub has_tj_array: bool,
    pub has_tj_plain: bool,
    pub has_quote: bool,
    pub has_td_upper: bool,
    pub has_tm: bool,
    /// Number of `Td` operators seen anywhere.
    pub td_total: usize,
    /// Of `td_total`, how many carry a horizontal component.
    pub td_horizontal: usize,
    /// True when the bounded signal scan stopped early (operator, `Do` or byte
    /// budget), so the routing decision may be based on incomplete signals.
    pub budget_exhausted: bool,
}

#[allow(dead_code)]
pub(crate) fn page_content_signals(doc: &Document, page_id: ObjectId) -> ContentSignals {
    let mut sig = ContentSignals::default();
    let Ok(content) = crate::text_extract::decode_page_content(doc, page_id) else {
        return sig;
    };
    let chain = crate::text_extract::resource_dicts(doc, page_id);
    let mut budget = GlyphBudget::new();
    let mut form_path: Vec<ObjectId> = Vec::new();
    scan_content_signals(
        doc,
        &chain,
        &content.operations,
        0,
        &mut form_path,
        &mut budget,
        &mut sig,
    );
    sig.budget_exhausted = budget.exhausted;
    sig
}

#[allow(clippy::too_many_arguments)]
fn scan_content_signals(
    doc: &Document,
    chain: &[&Dictionary],
    ops: &[Operation],
    depth: usize,
    form_path: &mut Vec<ObjectId>,
    budget: &mut GlyphBudget,
    sig: &mut ContentSignals,
) {
    for op in ops {
        if budget.ops_left == 0 {
            budget.exhausted = true;
            return;
        }
        budget.ops_left -= 1;
        match op.operator.as_str() {
            "TJ" => sig.has_tj_array = true,
            "Tj" => sig.has_tj_plain = true,
            "'" | "\"" => sig.has_quote = true,
            "TD" => sig.has_td_upper = true,
            "Tm" => sig.has_tm = true,
            "Td" => {
                sig.td_total += 1;
                if let Some(tx) = op.operands.first().and_then(num) {
                    if tx.abs() > 0.01 {
                        sig.td_horizontal += 1;
                    }
                }
            }
            "Do" => {
                if budget.do_left == 0 || depth >= MAX_FORM_DEPTH {
                    budget.exhausted = true;
                    continue;
                }
                let Some(name) = op.operands.first().and_then(|o| o.as_name().ok()) else {
                    continue;
                };
                let Some((id, form)) = crate::text_extract::lookup_form(doc, chain, name) else {
                    continue;
                };
                if id.map_or(false, |id| form_path.contains(&id)) {
                    continue;
                }
                let fchain = crate::text_extract::form_resource_chain(doc, &form.dict, chain);
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
                budget.ops_left = budget
                    .ops_left
                    .saturating_sub(crate::text_extract::FORM_INVOCATION_OPS);
                if budget.ops_left == 0 {
                    budget.exhausted = true;
                }
                budget.do_left -= 1;
                if let Some(id) = id {
                    form_path.push(id);
                }
                scan_content_signals(doc, &fchain, &fc.operations, depth + 1, form_path, budget, sig);
                if id.is_some() {
                    form_path.pop();
                }
            }
            _ => {}
        }
    }
}

/// Rotate every span of a dominantly vertical page upright so the ordinary
/// horizontal pipeline can read it. `upward` selects the reading direction:
/// `(x, y) -> (y, -x)` for text drawn bottom-to-top, `(-y, x)` for top-to-bottom.
/// The rotated spans are translated to a `(0,0)` origin and `Span::is_vertical`
/// is cleared; the returned value is the new display height.
fn rotate_spans_upright(spans: &mut [Span], upward: bool) -> f64 {
    let mut min_x = f64::INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    for s in spans.iter_mut() {
        let (nx, ny) = if upward {
            (s.y, -s.x)
        } else {
            (-s.y, s.x)
        };
        s.x = nx;
        s.y = ny;
        s.is_vertical = false;
        min_x = min_x.min(nx);
        min_y = min_y.min(ny);
        max_y = max_y.max(ny);
    }
    if !min_x.is_finite() || !min_y.is_finite() {
        return 0.0;
    }
    for s in spans.iter_mut() {
        s.x -= min_x;
        s.y -= min_y;
    }
    (max_y - min_y).max(1.0)
}

/// Extract text for a glyph-positioned page using geometry reconstruction.
/// When `detect_tables` is false, returns the plain reading-order text with
/// no table recovery (byte-identical to the table-less renderer).
pub fn extract_page_glyphs(
    doc: &Document,
    page_id: ObjectId,
    detect_tables: bool,
    detect_layout: bool,
    detect_math: bool,
) -> Result<PageText, String> {
    let chain = crate::text_extract::resource_dicts(doc, page_id);
    let mut raw_fonts: std::collections::BTreeMap<Vec<u8>, &Dictionary> =
        std::collections::BTreeMap::new();
    crate::text_extract::collect_fonts(doc, &chain, &mut raw_fonts);
    let has_fonts = !raw_fonts.is_empty();

    let content: Content<Vec<Operation>> = crate::text_extract::decode_page_content(doc, page_id)
        .map_err(|e| e.to_string())?;

    let (init_ctm, mut page_height) = page_initial_transform(doc, page_id);

    let font_key = chain
        .first()
        .map(|d| *d as *const Dictionary as usize)
        .unwrap_or(0);
    let mut font_cache: HashMap<usize, Vec<GlyphFontInfo>> = HashMap::new();
    font_cache.insert(font_key, build_glyph_font_table(doc, &raw_fonts));
    let mut text_ops_seen = false;
    let mut budget = GlyphBudget::new();
    let walk = walk_glyphs(
        doc,
        &chain,
        &content.operations,
        init_ctm,
        &mut Vec::new(),
        0,
        &mut text_ops_seen,
        &mut budget,
        &mut font_cache,
    );
    let GlyphWalk {
        mut spans,
        underline_segs,
        vertical_up_chars,
        vertical_down_chars,
    } = walk;

    // A page whose text is dominantly vertical (a sideways OCR layer, or a
    // producer that draws the whole page rotated 90° with a text matrix instead
    // of `/Rotate`) is otherwise emptied: every run is classified vertical and
    // dropped as margin furniture by `append_vertical_text`. Rotate such a page
    // upright and run the normal horizontal pipeline. A horizontal-majority
    // page keeps the existing margin behaviour for its minority vertical runs
    // (side stamps, letterheads).
    let total_chars: usize = spans.iter().map(|s| s.text.chars().count()).sum();
    let vertical_chars = vertical_up_chars + vertical_down_chars;
    if total_chars > 0 && vertical_chars * 5 >= total_chars * 3 {
        page_height = rotate_spans_upright(&mut spans, vertical_up_chars >= vertical_down_chars);
    }

    let (horizontal_spans, vertical_spans): (Vec<Span>, Vec<Span>) =
        spans.into_iter().partition(|s| !s.is_vertical);


    let mut vertical_blocks: Vec<crate::layout::reading_order::DocBlock> = Vec::new();
    let mut vertical_text = String::new();
    if !vertical_spans.is_empty() {
        let mut runs: Vec<Vec<Span>> = Vec::new();
        for s in vertical_spans {
            match runs.last_mut() {
                Some(last) => {
                    let last_sp = last.last().unwrap();
                    let dist = ((s.x - last_sp.x).powi(2) + (s.y - last_sp.y).powi(2)).sqrt();
                    if dist <= 3.5 * s.size.max(last_sp.size) {
                        last.push(s);
                    } else {
                        runs.push(vec![s]);
                    }
                }
                None => runs.push(vec![s]),
            }
        }
        for run in runs {
            let mut run_text = String::new();
            let mut min_x = f64::INFINITY;
            let mut min_y = f64::INFINITY;
            let mut max_x = f64::NEG_INFINITY;
            let mut max_y = f64::NEG_INFINITY;
            for sp in &run {
                min_x = min_x.min(sp.x);
                min_y = min_y.min(sp.y);
                max_x = max_x.max(sp.x + sp.advance);
                max_y = max_y.max(sp.y + sp.size);
                if sp.text == " " {
                    if !run_text.is_empty() && !run_text.ends_with(' ') {
                        run_text.push(' ');
                    }
                } else {
                    run_text.push_str(&sp.text);
                }
            }
            let trimmed = run_text.trim();
            if !trimmed.is_empty() {
                if !vertical_text.is_empty() {
                    vertical_text.push('\n');
                }
                vertical_text.push_str(trimmed);
                vertical_blocks.push(crate::layout::reading_order::DocBlock {
                    page: 0,
                    kind: "margin".to_string(),
                    x0: min_x,
                    y0: min_y,
                    x1: max_x,
                    y1: max_y,
                    text: trimmed.to_string(),
                    is_bold: false,
                    is_italic: false,
                    is_underline: false,
                });
            }
        }
    }

    // Lightweight Hough/Radon skew correction, applied BEFORE line clustering.
    // `build_lines` groups spans by their baseline `y`, so a page tilted by
    // even a couple of degrees fragments rows (spans on one visual line no
    // longer share a `y`) and smears column gutters. Estimate the tilt from the
    // raw span cloud — no line grouping required — and rotate the spans back
    // onto the page axes, so clustering, two-column detection and grid
    // recovery all see axis-aligned rows. On an axis-aligned page the estimator
    // returns a sub-threshold angle and the spans pass through untouched.
    let skew_deg = crate::layout::skew::estimate_skew_angle_deg_from_spans(
        &horizontal_spans,
        crate::layout::skew::DEFAULT_MAX_SKEW_DEG,
        crate::layout::skew::COARSE_STEP_DEG,
    );
    let mut lines = if skew_deg.abs() >= crate::layout::skew::MIN_SKEW_TO_CORRECT_DEG {
        build_lines(&crate::layout::skew::deskew_spans(&horizontal_spans, skew_deg))
    } else {
        build_lines(&horizontal_spans)
    };
    if std::env::var("PDF2MD_DEBUG").is_ok() {
        for (i, l) in lines.iter().enumerate() {
            let txt: String = l.iter().map(|s| format!("({},{}) '{}'", s.x.round(), s.y.round(), s.text)).collect::<Vec<_>>().join(" | ");
            eprintln!("Line {}: {}", i, txt);
        }
    }
    let mut hits = if detect_tables {
        find_tables(&lines)
    } else {
        Vec::new()
    };
    // Stage-3b: re-run the grid scan with a wider alignment tolerance over
    // rows the strict pass missed (jittered / borderless tables).
    if detect_tables {
        let gap_hits = find_gap_tables(&lines, &hits);
        if std::env::var("PDF2MD_DEBUG").is_ok() {
            eprintln!("DBG: find_tables hits={}, gap_hits={}", hits.len(), gap_hits.len());
        }
        hits.extend(gap_hits);
        hits.sort_by(|a, b| a.start.cmp(&b.start));
    }
    if std::env::var("PDF2MD_DEBUG").is_ok() {
        eprintln!("DBG: total hits before 2col filter: {}", hits.len());
    }
    // When a page contains a 2-column prose reading block, filter out any
    // 2-column table hits that lie inside the prose block (false positive prose).
    // Genuine tables with >= 3 columns or outside the prose block are kept.
    if detect_layout {
        if let Some(pc) = page_two_columns(&lines) {
            let col_top = pc.left.iter().chain(pc.right.iter())
                .map(|l| l[0].y).fold(f64::NEG_INFINITY, f64::max);
            let col_bottom = pc.left.iter().chain(pc.right.iter())
                .map(|l| l[0].y).fold(f64::INFINITY, f64::min);
            hits.retain(|h| {
                let num_cols = h.rows.iter().map(|r| r.len()).max().unwrap_or(0);
                if num_cols >= 3 {
                    return true;
                }
                let hit_y0 = lines[h.end][0].y;
                let hit_y1 = lines[h.start][0].y;
                let outside_prose = hit_y0 > col_top || hit_y1 < col_bottom;
                if outside_prose {
                    return true;
                }
                // Inside the two-column block: a genuine 2/3-column *table*
                // should be kept even when `page_two_columns` misreads the
                // table's own aligned columns as a prose gutter. Only drop a
                // hit that is actually prose noise: ragged columns, or cells
                // that hold long flowing text (a paragraph fragment). A real
                // table has a consistent column count and brief cells.
                let cols = h.rows.first().map(|r| r.len()).unwrap_or(0);
                let rectangular = cols >= 2 && h.rows.iter().all(|r| r.len() == cols);
                let max_cell_words = h
                    .rows
                    .iter()
                    .flat_map(|r| r.iter())
                    .map(|c| c.split_whitespace().count())
                    .max()
                    .unwrap_or(0);
                rectangular && max_cell_words <= 5
            });
        }
    }
    // Underline: match collected thin horizontal rules to spans, skipping any
    // visual line already claimed by a recovered table (whose row borders are
    // the same sort of thin rule).
    if !underline_segs.is_empty() {
        let covered: std::collections::HashSet<usize> =
            hits.iter().flat_map(|h| h.start..=h.end).collect();
        mark_underlines(&mut lines, &underline_segs, &covered);
    }
    let table_rendered = !hits.is_empty();
    let mut text = if table_rendered {
        // Byte-identical to the plain text renderer when no table is found.
        render_with_tables(&lines, &hits)
    } else if detect_layout {
        if detect_math {
            render_math(&lines, &underline_segs, page_height, true)
        } else {
            render_human_order(&lines, page_height, true)
        }
    } else if detect_math {
        render_math(&lines, &underline_segs, page_height, false)
    } else {
        render_cluster(&lines)
    };


    let mut blocks = if detect_layout {
        build_doc_blocks(&lines, page_height)
    } else {
        Vec::new()
    };

    // Emit one "table" zone per recovered canvas grid so callers can visualise
    // the detected layer. Reading-order fragments (body/list text) that fall
    // entirely inside a recovered table bbox are suppressed so the zone view
    // shows the table as a single region rather than scattered cells.
    if detect_tables && !hits.is_empty() {
        append_table_zones(&mut blocks, &hits);
    }

    if !vertical_text.is_empty() {
        // Side furniture/margin stamps stay out of the reading-order body text
        // when layout analysis is on (their blocks are still emitted below for
        // zone inspectors). Without layout analysis, preserve legacy behavior.
        append_vertical_text(&mut text, &vertical_text, detect_layout);
        blocks.extend(vertical_blocks);
    }

    Ok(PageText {
        text,
        text_ops_seen,
        has_fonts,
        tables: if table_rendered { hits.len() } else { 0 },
        blocks,
        budget_exhausted: budget.exhausted,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cid_widths(map: HashMap<u32, f64>, encoding: Option<CMapCodec>) -> Widths {
        Widths::Cid {
            map,
            default: 1000.0,
            encoding,
        }
    }

    /// Regression for the Type0/Identity-H run-width bug: a `Tj`/`TJ` operand
    /// carrying several 2-byte CIDs must sum every CID's `/W` advance. Folding
    /// the whole run into one bogus CID missed `/W`, fell back to `/DW`
    /// (1 em), drifted the text matrix, and fused words in the Markdown
    /// channel (e.g. "HandelsrechnungNr." instead of "Handelsrechnung Nr.").
    #[test]
    fn cid_identity_run_sums_each_glyph_width() {
        let mut map = HashMap::new();
        map.insert(0x0028u32, 600.0);
        map.insert(0x0056u32, 500.0);
        let w = cid_widths(map, None);
        assert_eq!(w.width(&[0x00, 0x28, 0x00, 0x56]), Some(1100.0));
        // A single glyph still measures correctly.
        assert_eq!(w.width(&[0x00, 0x28]), Some(600.0));
        // A CID missing from /W contributes /DW.
        assert_eq!(w.width(&[0x00, 0x28, 0x00, 0x99]), Some(1600.0));
    }

    /// The custom code->CID `/Encoding` CMap path must likewise split a
    /// multi-code run and sum each code's width.
    #[test]
    fn cid_custom_encoding_run_sums_each_code_width() {
        let cmap = parse_cmap(b"2 beginbfchar\n<01> <0028>\n<02> <0056>\nendbfchar")
            .expect("parse cmap");
        let mut map = HashMap::new();
        map.insert(0x0028u32, 600.0);
        map.insert(0x0056u32, 500.0);
        let w = cid_widths(map, Some(cmap));
        assert_eq!(w.width(&[0x01, 0x02]), Some(1100.0));
        assert_eq!(w.width(&[0x02]), Some(500.0));
    }

    #[test]
    fn cid_empty_run_has_no_width() {
        assert_eq!(cid_widths(HashMap::new(), None).width(&[]), None);
    }

    /// A Type0 `/W` table whose entries are *indirect* objects (one shared
    /// width object referenced by many CIDs — the intarsys EN16931 layout)
    /// must be resolved through `deref`. Before the fix `num()` saw an
    /// `Object::Reference`, dropped every entry, and let each glyph fall back
    /// to `/DW` (1000), inflating run advances by 1000/600 = 5/3 and fusing
    /// neighbouring table cells ("Gesamtbetrag der Zuschläge0,00").
    #[test]
    fn resolve_widths_dereferences_indirect_w_values() {
        let mut doc = Document::new();
        let w600 = doc.add_object(Object::Integer(600));
        let w500 = doc.add_object(Object::Integer(500));
        // Range form `c_first c_last w`, each `w` indirect.
        let warr_compact = doc.add_object(Object::Array(vec![
            Object::Integer(3),
            Object::Integer(3),
            Object::Reference(w600),
            Object::Integer(8),
            Object::Integer(8),
            Object::Reference(w500),
        ]));
        // Array form `c [w1 w2]` with the widths themselves indirect.
        let warr_list = doc.add_object(Object::Array(vec![
            Object::Integer(20),
            Object::Array(vec![Object::Reference(w600), Object::Reference(w500)]),
        ]));

        for warr in [warr_compact, warr_list] {
            let mut cid = Dictionary::new();
            cid.set(b"Subtype", Object::Name(b"CIDFontType2".to_vec()));
            cid.set(b"W", Object::Reference(warr));
            let desc = doc.add_object(Object::Dictionary(cid));
            let mut font = Dictionary::new();
            font.set(b"Subtype", Object::Name(b"Type0".to_vec()));
            font.set(
                b"DescendantFonts",
                Object::Array(vec![Object::Reference(desc)]),
            );
            let widths = resolve_widths(&doc, &font);
            match &widths {
                Widths::Cid { map, default, .. } => {
                    assert_eq!(*default, 1000.0);
                    let (a, b) = if map.contains_key(&3) { (3u32, 8u32) } else { (20u32, 21u32) };
                    assert_eq!(map.get(&a), Some(&600.0), "indirect /W width for CID {a} must resolve");
                    assert_eq!(map.get(&b), Some(&500.0), "indirect /W width for CID {b} must resolve");
                    assert_eq!(widths.width(&[0x00, a as u8]), Some(600.0));
                }
                _ => panic!("expected Widths::Cid for Type0 font"),
            }
        }
    }

    #[test]
    fn test_append_table_zones_emits_table_and_suppresses_contained_fragments() {
        use crate::layout::reading_order::DocBlock;
        use crate::layout::tables::TableHit;
        use crate::models::BoundingBox;

        let mut blocks = vec![
            DocBlock {
                page: 1, kind: "heading".into(),
                x0: 40.0, y0: 700.0, x1: 300.0, y1: 720.0,
                text: "Table".into(), is_bold: false, is_italic: false, is_underline: false,
            },
            DocBlock {
                page: 1, kind: "body".into(),
                x0: 50.0, y0: 400.0, x1: 100.0, y1: 410.0,
                text: "cell a".into(), is_bold: false, is_italic: false, is_underline: false,
            },
            DocBlock {
                page: 1, kind: "list".into(),
                x0: 200.0, y0: 400.0, x1: 250.0, y1: 410.0,
                text: "- cell b".into(), is_bold: false, is_italic: false, is_underline: false,
            },
            DocBlock {
                page: 1, kind: "body".into(),
                x0: 50.0, y0: 200.0, x1: 300.0, y1: 210.0,
                text: "outside para".into(), is_bold: false, is_italic: false, is_underline: false,
            },
        ];
        let hit = TableHit {
            start: 0,
            end: 1,
            rows: vec![
                vec!["h1".to_string(), "h2".to_string()],
                vec!["a".to_string(), "b".to_string()],
            ],
            bbox: BoundingBox::new(45.0, 390.0, 260.0, 415.0),
        };
        append_table_zones(&mut blocks, &[hit]);

        assert_eq!(
            blocks.iter().filter(|b| b.kind == "table").count(),
            1,
            "exactly one table zone must be emitted"
        );
        assert_eq!(
            blocks.len(),
            3,
            "two contained fragments removed; heading + outside body + table remain: {:?}",
            blocks.iter().map(|b| b.kind.clone()).collect::<Vec<_>>()
        );
        assert!(blocks.iter().any(|b| b.kind == "heading"));
        assert!(blocks.iter().any(|b| b.text == "outside para"));
        assert!(blocks.iter().any(|b| b.text.starts_with("table ·")));
        assert!(!blocks.iter().any(|b| b.text == "cell a"));
        assert!(!blocks.iter().any(|b| b.text == "- cell b"));
    }

    #[test]
    fn test_append_table_zones_keeps_structure_inside_table() {
        use crate::layout::reading_order::DocBlock;
        use crate::layout::tables::TableHit;
        use crate::models::BoundingBox;

        let mut blocks = vec![DocBlock {
            page: 1, kind: "figure".into(),
            x0: 100.0, y0: 500.0, x1: 200.0, y1: 550.0,
            text: "[photo]".into(), is_bold: false, is_italic: false, is_underline: false,
        }];
        let hit = TableHit {
            start: 0,
            end: 0,
            rows: vec![vec!["x".to_string(), "y".to_string()]],
            bbox: BoundingBox::new(10.0, 10.0, 300.0, 600.0),
        };
        append_table_zones(&mut blocks, &[hit]);
        // A "figure" fragment inside the table bbox is NOT dropped.
        assert!(blocks.iter().any(|b| b.kind == "figure"));
    }

    #[test]
    fn test_append_table_zones_suppresses_fragments_on_table_edge_rows() {
        // Regression for synth_facture_btp_autoliquidation.pdf: `hit.bbox` is
        // baseline-based (`find_tables` uses the min/max span `y`), but
        // `build_doc_blocks` pads each fragment by half its font size. A
        // fragment on the table's first or last row overhangs the bbox by
        // ~0.5em, so the old strict-containment test never suppressed it: the
        // header cell `Qté` and the last-row cell `1400,00 €` (both padded to
        // y 625..635 / 589..599 around a 594..630 bbox) leaked as "body"
        // fragments next to the table zone, as did the cross-column paragraph
        // merge `Total HT 1`. A genuine prose line below the table must stay.
        use crate::layout::reading_order::DocBlock;
        use crate::layout::tables::TableHit;
        use crate::models::BoundingBox;

        fn frag(kind: &str, x0: f64, y0: f64, x1: f64, y1: f64, t: &str) -> DocBlock {
            DocBlock {
                page: 1,
                kind: kind.into(),
                x0,
                y0,
                x1,
                y1,
                text: t.into(),
                is_bold: false,
                is_italic: false,
                is_underline: false,
            }
        }

        let mut blocks = vec![
            // Header-row cell: baseline 630, padded to 625..635 (overhangs y1).
            frag("body", 300.0, 625.0, 316.0, 635.0, "Qté"),
            // Last-row cell: baseline 594, padded to 589..599 (overhangs y0).
            frag("body", 350.0, 589.0, 394.5, 599.0, "1400,00 €"),
            // Cross-column paragraph merge inside the grid.
            frag("body", 300.0, 607.0, 508.0, 635.0, "Total HT 1"),
            // Genuine prose just below the table: centre outside the bbox.
            frag("body", 56.0, 650.0, 300.0, 660.0, "Autoliquidation"),
        ];
        let hit = TableHit {
            start: 0,
            end: 2,
            rows: vec![
                vec!["Désignation".to_string(), "Qté".to_string()],
                vec!["x".to_string(), "1".to_string()],
                vec!["y".to_string(), "2".to_string()],
            ],
            // Baseline bbox, exactly as find_tables builds it.
            bbox: BoundingBox::new(56.0, 594.0, 514.5, 630.0),
        };
        append_table_zones(&mut blocks, &[hit]);

        assert_eq!(
            blocks.iter().filter(|b| b.kind == "table").count(),
            1,
            "exactly one table zone must be emitted"
        );
        for dropped in ["Qté", "1400,00 €", "Total HT 1"] {
            assert!(
                !blocks.iter().any(|b| b.text == dropped),
                "table-edge fragment {dropped:?} must be collapsed into the table zone: {:?}",
                blocks.iter().map(|b| b.text.clone()).collect::<Vec<_>>()
            );
        }
        assert!(
            blocks.iter().any(|b| b.text == "Autoliquidation"),
            "prose outside the table bbox must survive: {:?}",
            blocks.iter().map(|b| b.text.clone()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_faux_bold_overstrike_deduplication() {
        let spans = vec![
            Span {
                text: "B".into(),
                x: 100.0,
                y: 200.0,
                size: 12.0,
                advance: 8.0,
                word_advance: 8.0,
                is_bold: false,
                is_italic: false,
                is_underline: false,
                is_vertical: false,
            },
            Span {
                text: "B".into(),
                x: 100.2,
                y: 200.0,
                size: 12.0,
                advance: 8.0,
                word_advance: 8.0,
                is_bold: false,
                is_italic: false,
                is_underline: false,
                is_vertical: false,
            },
            Span {
                text: "U".into(),
                x: 108.0,
                y: 200.0,
                size: 12.0,
                advance: 8.0,
                word_advance: 8.0,
                is_bold: false,
                is_italic: false,
                is_underline: false,
                is_vertical: false,
            },
            Span {
                text: "U".into(),
                x: 108.2,
                y: 200.0,
                size: 12.0,
                advance: 8.0,
                word_advance: 8.0,
                is_bold: false,
                is_italic: false,
                is_underline: false,
                is_vertical: false,
            },
            Span {
                text: "L".into(),
                x: 116.0,
                y: 200.0,
                size: 12.0,
                advance: 8.0,
                word_advance: 8.0,
                is_bold: false,
                is_italic: false,
                is_underline: false,
                is_vertical: false,
            },
            Span {
                text: "L".into(),
                x: 116.2,
                y: 200.0,
                size: 12.0,
                advance: 8.0,
                word_advance: 8.0,
                is_bold: false,
                is_italic: false,
                is_underline: false,
                is_vertical: false,
            },
            // Legitimate second 'L' in BULLETIN at normal horizontal offset:
            Span {
                text: "L".into(),
                x: 124.0,
                y: 200.0,
                size: 12.0,
                advance: 8.0,
                word_advance: 8.0,
                is_bold: false,
                is_italic: false,
                is_underline: false,
                is_vertical: false,
            },
            Span {
                text: "L".into(),
                x: 124.2,
                y: 200.0,
                size: 12.0,
                advance: 8.0,
                word_advance: 8.0,
                is_bold: false,
                is_italic: false,
                is_underline: false,
                is_vertical: false,
            },
        ];
        let lines = build_lines(&spans);
        assert_eq!(lines.len(), 1);
        let text: String = lines[0].iter().map(|s| s.text.as_str()).collect();
        assert_eq!(text, "BULL");
        assert!(lines[0][0].is_bold);
        assert_eq!(lines[0].len(), 4);
    }

    #[test]
    fn flush_path_segs_keeps_only_horizontal_thin_rules() {
        let mut path = vec![(100.0, 200.0), (300.0, 200.0), (300.0, 201.0)];
        let mut segs: Vec<(f64, f64, f64)> = Vec::new();
        flush_path_segs(&mut path, Some((100.0, 200.0)), &mut segs, false);
        assert_eq!(segs, vec![(200.0, 100.0, 300.0)], "only the horizontal rule survives");
    }

    #[test]
    fn flush_path_segs_ignores_a_box_border_edge() {
        // A hyperlink annotation box drawn as `m`/`l`/`h` is ~13pt tall; its top
        // and bottom edges must NOT become "underline" rules, or the top edge
        // underlines the text line just above the box (the FR ACRE slide's URL
        // box underlined " au plus tard dans les 45 jours suiv").
        let mut path = vec![
            (198.1, 257.5),
            (260.4, 257.5),
            (260.4, 244.1),
            (198.1, 244.1),
        ];
        let mut segs: Vec<(f64, f64, f64)> = Vec::new();
        flush_path_segs(&mut path, Some((198.1, 257.5)), &mut segs, true);
        assert!(segs.is_empty(), "a box border must not yield underline rules, got {segs:?}");
    }

    #[test]
    fn mark_underlines_flags_span_below_a_rule() {
        // Baseline at y=700; a thin rule at y=698 (2pt below, well within 0.5*size).
        let mut lines = vec![vec![Span {
            text: "Underlined".into(),
            x: 100.0,
            y: 700.0,
            size: 12.0,
            advance: 60.0,
            word_advance: 60.0,
            is_bold: false,
            is_italic: false,
            is_underline: false,
            is_vertical: false,
        }]];
        let segs: Vec<(f64, f64, f64)> = vec![(698.0, 100.0, 160.0)];
        let covered: std::collections::HashSet<usize> = Default::default();
        mark_underlines(&mut lines, &segs, &covered);
        assert!(lines[0][0].is_underline, "span directly above a rule must be underlined");
    }

    #[test]
    fn mark_underlines_skips_table_covered_lines() {
        let span = |text: &str, x: f64| Span {
            text: text.into(),
            x,
            y: 700.0,
            size: 12.0,
            advance: 20.0,
            word_advance: 20.0,
            is_bold: false,
            is_italic: false,
            is_underline: false,
            is_vertical: false,
        };
        let mut lines = vec![vec![span("cell", 100.0)]];
        let segs: Vec<(f64, f64, f64)> = vec![(698.0, 90.0, 160.0)];
        let covered: std::collections::HashSet<usize> = [0usize].into_iter().collect();
        mark_underlines(&mut lines, &segs, &covered);
        assert!(!lines[0][0].is_underline, "table row borders must not underline cell text");
    }

    #[test]
    fn mark_underlines_ignores_rule_far_below_baseline() {
        let mut lines = vec![vec![Span {
            text: "Body".into(),
            x: 100.0,
            y: 700.0,
            size: 12.0,
            advance: 30.0,
            word_advance: 30.0,
            is_bold: false,
            is_italic: false,
            is_underline: false,
            is_vertical: false,
        }]];
        // Rule 10pt below baseline (> 0.5 * 12) is a table border, not an underline.
        let segs: Vec<(f64, f64, f64)> = vec![(690.0, 90.0, 160.0)];
        let covered: std::collections::HashSet<usize> = Default::default();
        mark_underlines(&mut lines, &segs, &covered);
        assert!(!lines[0][0].is_underline, "rule too far below baseline is not an underline");
    }

    fn font_with_name(name: &[u8]) -> (Document, Dictionary) {
        let mut font = Dictionary::new();
        font.set(b"BaseFont", Object::Name(name.to_vec()));
        (Document::new(), font)
    }

    #[test]
    fn width_sums_every_glyph_in_a_run() {
        // A `Tj`/`TJ` operand is a whole word or phrase; `span.x + advance`
        // is used as the run's right edge. Only the first glyph's width must
        // not be used, or long runs under-measure and open phantom gaps.
        let mut t = [0.0f64; 256];
        t[b'A' as usize] = 600.0;
        t[b'B' as usize] = 700.0;
        let w = Widths::Byte(t);
        assert_eq!(w.width(b"AB"), Some(1300.0));
        assert_eq!(w.width(b"A"), Some(600.0));
        assert_eq!(w.width(b""), None);
    }

    #[test]
    fn resolve_font_style_recognizes_urw_medi_as_bold() {
        // LaTeX/Nimbus Roman embeds the bold face as *-Medi, which spells
        // neither "bold" nor "black".
        let (doc, font) = font_with_name(b"MCWOPG+NimbusRomNo9L-Medi");
        assert!(resolve_font_style(&doc, &font).0, "-Medi must be bold");
    }

    #[test]
    fn resolve_font_style_recognizes_urw_bd_as_bold() {
        let (doc, font) = font_with_name(b"VSGTBW+NimbusRomNo9L-Regu");
        assert!(!resolve_font_style(&doc, &font).0, "plain Regu must not be bold");
        let (doc, font) = font_with_name(b"ABCDEF+URWGothic-Bd");
        assert!(resolve_font_style(&doc, &font).0, "-Bd must be bold");
    }

    #[test]
    fn resolve_font_style_recognizes_computer_modern_cmbx_as_bold() {
        let (doc, font) = font_with_name(b"CMBX10");
        assert!(resolve_font_style(&doc, &font).0, "CMBX10 (Computer Modern bold) must be bold");
        let (doc, font) = font_with_name(b"SFBX12");
        assert!(resolve_font_style(&doc, &font).0, "SFBX12 (sans bold) must be bold");
        let (doc, font) = font_with_name(b"UDWEWE+CMR10");
        assert!(!resolve_font_style(&doc, &font).0, "CMR10 (regular) must not be bold");
    }

    #[test]
    fn resolve_font_style_uses_stemv_heuristic_when_fontweight_absent() {
        // /FontWeight is rarely present in embedded subsets; /StemV is the
        // practical bold signal. Regular faces sit at ~70–90, bold at 120+.
        let mut bold_fd = Dictionary::new();
        bold_fd.set(b"StemV", 130);
        let mut bold_font = Dictionary::new();
        bold_font.set(b"FontDescriptor", Object::Dictionary(bold_fd));
        let doc = Document::new();
        assert!(resolve_font_style(&doc, &bold_font).0, "StemV >= 120 must be bold");

        let mut light_fd = Dictionary::new();
        light_fd.set(b"StemV", 78);
        let mut light_font = Dictionary::new();
        light_font.set(b"FontDescriptor", Object::Dictionary(light_fd));
        assert!(!resolve_font_style(&doc, &light_font).0, "low StemV must not be bold");
    }

    /// Builds the smallest parseable sfnt program carrying one `OS/2` table
    /// whose `usWeightClass` is `weight`.
    fn sfnt_with_os2_weight(weight: u16) -> Vec<u8> {
        let mut os2 = vec![0u8; 64];
        os2[4..6].copy_from_slice(&weight.to_be_bytes());
        let mut out = Vec::new();
        out.extend_from_slice(&0x0001_0000u32.to_be_bytes()); // sfnt version
        out.extend_from_slice(&1u16.to_be_bytes()); // numTables
        out.extend_from_slice(&[0u8; 6]); // searchRange/entrySelector/rangeShift
        out.extend_from_slice(b"OS/2");
        out.extend_from_slice(&0u32.to_be_bytes()); // checksum (unused here)
        let offset = 12 + 16;
        out.extend_from_slice(&(offset as u32).to_be_bytes());
        out.extend_from_slice(&(os2.len() as u32).to_be_bytes());
        out.extend_from_slice(&os2);
        out
    }

    fn font_with_embedded_weight(weight: u16, stemv: i64) -> (Document, Dictionary) {
        let mut doc = Document::new();
        let file_id = doc.add_object(Object::Stream(lopdf::Stream::new(
            Dictionary::new(),
            sfnt_with_os2_weight(weight),
        )));
        let mut fd = Dictionary::new();
        fd.set(b"Flags", 6);
        fd.set(b"StemV", stemv);
        fd.set(b"FontFile2", Object::Reference(file_id));
        let mut font = Dictionary::new();
        font.set(b"BaseFont", Object::Name(b"CIDFont+F1".to_vec()));
        font.set(b"FontDescriptor", Object::Dictionary(fd));
        (doc, font)
    }

    /// Regression for the intarsys/ZUGFeRD all-bold bug: that stylesheet writes
    /// the same out-of-range `/StemV 600` on both its Regular and Bold subsets,
    /// so the stem-width threshold alone bolds every run. The embedded `OS/2`
    /// table is the authoritative weight signal.
    #[test]
    fn resolve_font_style_prefers_embedded_weight_over_bogus_stemv() {
        let (doc, regular) = font_with_embedded_weight(400, 600);
        assert!(
            !resolve_font_style(&doc, &regular).0,
            "OS/2 usWeightClass=400 must beat /StemV 600 (the every-run-bold bug)"
        );

        let (doc, bold) = font_with_embedded_weight(700, 600);
        assert!(
            resolve_font_style(&doc, &bold).0,
            "OS/2 usWeightClass=700 must still resolve as bold"
        );
    }

    /// Bug 6 regression: `arXiv:2310.06825v1 [cs.CL] 10 Oct 2023` sits in the
    /// left margin as vertical text. With `detect_layout: true` it must not be
    /// appended to the page body prose; with layout analysis off the legacy
    /// append behavior must remain (the text channel is the only consumer).
    #[test]
    fn vertical_margin_text_is_not_appended_to_body_when_detect_layout() {
        let margin = "arXiv:2310.06825v1 [cs.CL] 10 Oct 2023";

        let mut body = String::from("Introduction paragraph.");
        append_vertical_text(&mut body, margin, true);
        assert_eq!(
            body, "Introduction paragraph.",
            "layout analysis must keep margin stamps out of PageText.text"
        );
        assert!(!body.contains("arXiv"), "margin stamp leaked into body flow: {body:?}");

        append_vertical_text(&mut body, margin, false);
        assert!(
            body.ends_with(margin),
            "without layout analysis the legacy append must be preserved: {body:?}"
        );
    }

    /// A `/ToUnicode` map may return a Latin presentation-form ligature
    /// (U+FB00–U+FB06); it must be folded to its letter sequence so the output
    /// reads "fi scal", not "ﬁ scal".
    #[test]
    fn ligatures_fold_to_letter_sequences() {
        let mut s = "Num\u{FB01} scal \u{FB00}ort \u{FB03}cient \u{FB02}eur".to_string();
        fold_ligatures(&mut s);
        assert_eq!(s, "Numfi scal ffort fficient fleur");
    }

    // ------------------------------------------------------------------
    // B1: Form XObject recursion in the glyph engine
    // ------------------------------------------------------------------

    use lopdf::{dictionary, Stream};

    fn rl(v: f64) -> Object {
        Object::Real(v as f32)
    }

    fn show_at(x: f64, y: f64, text: &str) -> Vec<Operation> {
        vec![
            Operation::new(
                "Tm",
                vec![rl(1.0), rl(0.0), rl(0.0), rl(1.0), rl(x), rl(y)],
            ),
            Operation::new("Tj", vec![Object::string_literal(text.to_string())]),
        ]
    }

    fn form_dict(matrix: Option<[f64; 6]>, bbox: Option<[f64; 4]>) -> Dictionary {
        let mut fd = Dictionary::new();
        fd.set(b"Type", Object::Name(b"XObject".to_vec()));
        fd.set(b"Subtype", Object::Name(b"Form".to_vec()));
        if let Some(m) = matrix {
            fd.set(
                b"Matrix",
                vec![rl(m[0]), rl(m[1]), rl(m[2]), rl(m[3]), rl(m[4]), rl(m[5])],
            );
        }
        if let Some(b) = bbox {
            fd.set(b"BBox", vec![rl(b[0]), rl(b[1]), rl(b[2]), rl(b[3])]);
        }
        fd
    }

    /// One-page document whose `/Resources/XObject` maps each name to a form.
    /// Each form entry is `(name, form_ops, matrix, bbox, own_font_id)`.
    fn pdf_with_named_forms(
        page_ops: Vec<Operation>,
        forms: &[(&[u8], Vec<Operation>, Option<[f64; 6]>, Option<[f64; 4]>, Option<ObjectId>)],
    ) -> (Document, ObjectId) {
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let page_font = doc.add_object(dictionary! {
            "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Courier",
        });
        let mut xobjects = Dictionary::new();
        for (name, ops, matrix, bbox, own_font) in forms {
            let mut fd = form_dict(*matrix, *bbox);
            if let Some(fid) = own_font {
                let res = doc.add_object(dictionary! { "Font" => dictionary! { "F1" => *fid } });
                fd.set(b"Resources", Object::Reference(res));
            }
            let sid = doc.add_object(Stream::new(
                fd,
                Content { operations: ops.clone() }.encode().unwrap(),
            ));
            xobjects.set(name.to_vec(), Object::Reference(sid));
        }
        let page_res = doc.add_object(dictionary! {
            "Font" => dictionary! { "F1" => page_font },
            "XObject" => xobjects,
        });
        let content_id = doc.add_object(Stream::new(
            Dictionary::new(),
            Content { operations: page_ops }.encode().unwrap(),
        ));
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page", "Parent" => pages_id, "Contents" => content_id,
        });
        let pages = dictionary! {
            "Type" => "Pages", "Kids" => vec![page_id.into()], "Count" => 1,
            "Resources" => page_res,
            "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
        };
        doc.objects.insert(pages_id, Object::Dictionary(pages));
        let catalog_id = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        doc.trailer.set("Root", catalog_id);
        (doc, page_id)
    }

    fn walk_page(doc: &Document, page_id: ObjectId) -> (Vec<Span>, GlyphBudget) {
        let chain = crate::text_extract::resource_dicts(doc, page_id);
        let content = crate::text_extract::decode_page_content(doc, page_id).expect("content");
        let mut budget = GlyphBudget::new();
        let mut cache: HashMap<usize, Vec<GlyphFontInfo>> = HashMap::new();
        let mut seen = false;
        let (spans, _) = {
            let walk = walk_glyphs(
                doc,
                &chain,
                &content.operations,
                Mtx::ID,
                &mut Vec::new(),
                0,
                &mut seen,
                &mut budget,
                &mut cache,
            );
            (walk.spans, walk.underline_segs)
        };
        (spans, budget)
    }

    #[test]
    fn form_xobject_text_is_walked_with_page_resources() {
        let form_ops = {
            let mut v = vec![
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec![Object::Name(b"F1".to_vec()), rl(10.0)]),
            ];
            v.extend(show_at(50.0, 700.0, "FORM"));
            v.push(Operation::new("ET", vec![]));
            v
        };
        let (doc, page_id) = pdf_with_named_forms(
            vec![Operation::new("Do", vec![Object::Name(b"Fm0".to_vec())])],
            &[(b"Fm0", form_ops, None, None, None)],
        );
        let (spans, _) = walk_page(&doc, page_id);
        assert_eq!(spans.len(), 1, "form text must be extracted");
        assert_eq!(spans[0].text, "FORM");
        assert!((spans[0].x - 50.0).abs() < 0.01 && (spans[0].y - 700.0).abs() < 0.01);
    }

    #[test]
    fn form_matrix_and_page_cm_compose_correctly() {
        // Page cm scales by 2 and translates (100,100); form /Matrix translates
        // (10,20). A form-space origin must land at 2*(10,20)+(100,100).
        let form_ops = {
            let mut v = vec![
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec![Object::Name(b"F1".to_vec()), rl(10.0)]),
            ];
            v.extend(show_at(0.0, 0.0, "X"));
            v.push(Operation::new("ET", vec![]));
            v
        };
        let page_ops = vec![
            Operation::new("q", vec![]),
            Operation::new("cm", vec![rl(2.0), rl(0.0), rl(0.0), rl(2.0), rl(100.0), rl(100.0)]),
            Operation::new("Do", vec![Object::Name(b"Fm0".to_vec())]),
            Operation::new("Q", vec![]),
        ];
        let (doc, page_id) = pdf_with_named_forms(
            page_ops,
            &[(b"Fm0", form_ops, Some([1.0, 0.0, 0.0, 1.0, 10.0, 20.0]), None, None)],
        );
        let (spans, _) = walk_page(&doc, page_id);
        assert_eq!(spans.len(), 1);
        assert!(
            (spans[0].x - 120.0).abs() < 0.01 && (spans[0].y - 140.0).abs() < 0.01,
            "composed device position wrong: ({}, {})",
            spans[0].x,
            spans[0].y
        );
        assert!((spans[0].size - 20.0).abs() < 0.01, "cm scale must apply to the size");
    }

    #[test]
    fn form_local_font_overrides_the_page_font_of_the_same_name() {
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let page_font = doc.add_object(dictionary! {
            "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Courier",
        });
        let cmap = doc.add_object(Stream::new(
            Dictionary::new(),
            b"begincmap\nbeginbfchar\n<41> <005A>\nendbfchar\nendcmap\n".to_vec(),
        ));
        let form_font = doc.add_object(dictionary! {
            "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Courier", "ToUnicode" => cmap,
        });
        let res = doc.add_object(dictionary! { "Font" => dictionary! { "F1" => form_font } });
        let mut fd = form_dict(None, None);
        fd.set(b"Resources", Object::Reference(res));
        let form_ops = vec![
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec![Object::Name(b"F1".to_vec()), rl(10.0)]),
            Operation::new("Tj", vec![Object::string_literal("A".to_string())]),
            Operation::new("ET", vec![]),
        ];
        let form_id = doc.add_object(Stream::new(
            fd,
            Content { operations: form_ops }.encode().unwrap(),
        ));
        let page_res = doc.add_object(dictionary! {
            "Font" => dictionary! { "F1" => page_font },
            "XObject" => dictionary! { "Fm0" => form_id },
        });
        let page_ops = vec![
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec![Object::Name(b"F1".to_vec()), rl(10.0)]),
            Operation::new("Tj", vec![Object::string_literal("A".to_string())]),
            Operation::new("Td", vec![rl(0.0), rl(-20.0)]),
            Operation::new("Tj", vec![Object::string_literal("A".to_string())]),
            Operation::new("ET", vec![]),
            Operation::new("Do", vec![Object::Name(b"Fm0".to_vec())]),
        ];
        let content_id = doc.add_object(Stream::new(
            Dictionary::new(),
            Content { operations: page_ops }.encode().unwrap(),
        ));
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page", "Parent" => pages_id, "Contents" => content_id,
        });
        let pages = dictionary! {
            "Type" => "Pages", "Kids" => vec![page_id.into()], "Count" => 1,
            "Resources" => page_res,
            "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
        };
        doc.objects.insert(pages_id, Object::Dictionary(pages));
        let catalog_id = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        doc.trailer.set("Root", catalog_id);

        let (spans, _) = walk_page(&doc, page_id);
        let page_a = spans.iter().filter(|s| s.text == "A").count();
        let form_z = spans.iter().filter(|s| s.text == "Z").count();
        assert_eq!(page_a, 2, "page /F1 must still decode to 'A'");
        assert_eq!(form_z, 1, "the form's own /F1 must decode to 'Z'");
    }

    #[test]
    fn form_bbox_clips_a_span_outside_it() {
        // Two runs: one inside the BBox, one far to the right of it.
        let form_ops = {
            let mut v = vec![
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec![Object::Name(b"F1".to_vec()), rl(10.0)]),
            ];
            v.extend(show_at(50.0, 700.0, "IN"));
            v.extend(show_at(400.0, 700.0, "OUT"));
            v.push(Operation::new("ET", vec![]));
            v
        };
        let (doc, page_id) = pdf_with_named_forms(
            vec![Operation::new("Do", vec![Object::Name(b"Fm0".to_vec())])],
            &[(b"Fm0", form_ops, None, Some([40.0, 600.0, 120.0, 720.0]), None)],
        );
        let (spans, _) = walk_page(&doc, page_id);
        assert_eq!(spans.len(), 1, "only the span inside the form /BBox survives: {spans:?}");
        assert_eq!(spans[0].text, "IN");
    }

    #[test]
    fn form_drawn_twice_yields_two_runs() {
        let form_ops = {
            let mut v = vec![
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec![Object::Name(b"F1".to_vec()), rl(10.0)]),
            ];
            v.extend(show_at(50.0, 700.0, "AB"));
            v.push(Operation::new("ET", vec![]));
            v
        };
        let page_ops = vec![
            Operation::new("Do", vec![Object::Name(b"Fm0".to_vec())]),
            Operation::new("Do", vec![Object::Name(b"Fm0".to_vec())]),
        ];
        let (doc, page_id) =
            pdf_with_named_forms(page_ops, &[(b"Fm0", form_ops, None, None, None)]);
        let (spans, budget) = walk_page(&doc, page_id);
        assert_eq!(spans.len(), 2, "a form drawn twice contributes two runs");
        assert_eq!(budget.do_left, MAX_FORM_DO_PER_PAGE - 2);
    }

    #[test]
    fn self_referential_form_terminates() {
        // One form object exposed under two names; its content does `/B Do`,
        // which resolves back to the same object and must be skipped.
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let page_font = doc.add_object(dictionary! {
            "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Courier",
        });
        let form_ops = vec![
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec![Object::Name(b"F1".to_vec()), rl(10.0)]),
            Operation::new("Tj", vec![Object::string_literal("R".to_string())]),
            Operation::new("ET", vec![]),
            Operation::new("Do", vec![Object::Name(b"B".to_vec())]),
        ];
        let form_id = doc.add_object(Stream::new(
            form_dict(None, None),
            Content { operations: form_ops }.encode().unwrap(),
        ));
        let page_res = doc.add_object(dictionary! {
            "Font" => dictionary! { "F1" => page_font },
            "XObject" => dictionary! { "A" => form_id, "B" => form_id },
        });
        let content_id = doc.add_object(Stream::new(
            Dictionary::new(),
            Content { operations: vec![Operation::new("Do", vec![Object::Name(b"A".to_vec())])] }
                .encode()
                .unwrap(),
        ));
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page", "Parent" => pages_id, "Contents" => content_id,
        });
        let pages = dictionary! {
            "Type" => "Pages", "Kids" => vec![page_id.into()], "Count" => 1,
            "Resources" => page_res,
            "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
        };
        doc.objects.insert(pages_id, Object::Dictionary(pages));
        let catalog_id = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        doc.trailer.set("Root", catalog_id);

        let (spans, _) = walk_page(&doc, page_id);
        assert_eq!(spans.len(), 1, "a self-referential form must be walked once");
    }

    #[test]
    fn wide_form_expansion_hits_the_do_budget_and_terminates() {
        // One form issues far more `Do`s than the budget allows; without the
        // shared budget the page would decode/process all of them.
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let page_font = doc.add_object(dictionary! {
            "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Courier",
        });
        // Leaf form: draws one glyph.
        let leaf_ops = {
            let mut v = vec![
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec![Object::Name(b"F1".to_vec()), rl(10.0)]),
            ];
            v.extend(show_at(10.0, 10.0, "L"));
            v.push(Operation::new("ET", vec![]));
            v
        };
        let mut leaf_fd = form_dict(None, None);
        leaf_fd.set(
            b"Resources",
            Object::Dictionary(dictionary! { "Font" => dictionary! { "F1" => page_font } }),
        );
        let leaf_id = doc.add_object(Stream::new(
            leaf_fd,
            Content { operations: leaf_ops }.encode().unwrap(),
        ));
        // Parent form: `n` Do's of the leaf, more than the budget.
        let n = MAX_FORM_DO_PER_PAGE + 88;
        let mut parent_ops = Vec::new();
        for _ in 0..n {
            parent_ops.push(Operation::new("Do", vec![Object::Name(b"N".to_vec())]));
        }
        let mut parent_fd = form_dict(None, None);
        parent_fd.set(
            b"Resources",
            Object::Dictionary(dictionary! {
                "Font" => dictionary! { "F1" => page_font },
                "XObject" => dictionary! { "N" => leaf_id },
            }),
        );
        let parent_id = doc.add_object(Stream::new(
            parent_fd,
            Content { operations: parent_ops }.encode().unwrap(),
        ));
        let page_res = doc.add_object(dictionary! {
            "Font" => dictionary! { "F1" => page_font },
            "XObject" => dictionary! { "Fm0" => parent_id },
        });
        let content_id = doc.add_object(Stream::new(
            Dictionary::new(),
            Content { operations: vec![Operation::new("Do", vec![Object::Name(b"Fm0".to_vec())])] }
                .encode()
                .unwrap(),
        ));
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page", "Parent" => pages_id, "Contents" => content_id,
        });
        let pages = dictionary! {
            "Type" => "Pages", "Kids" => vec![page_id.into()], "Count" => 1,
            "Resources" => page_res,
            "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
        };
        doc.objects.insert(pages_id, Object::Dictionary(pages));
        let catalog_id = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        doc.trailer.set("Root", catalog_id);

        let (spans, budget) = walk_page(&doc, page_id);
        assert_eq!(budget.do_left, 0, "the shared Do budget must be exhausted");
        assert!(
            spans.len() <= MAX_FORM_DO_PER_PAGE,
            "expansion must stop at the budget, got {} spans",
            spans.len()
        );
    }

    #[test]
    fn many_distinct_forms_are_all_walked() {
        // 1200 distinct one-word forms, each invoked once (the shape that the
        // old 512 `Do`/page cap truncated): every form must contribute a span.
        let n = 1200usize;
        let names: Vec<Vec<u8>> = (0..n).map(|i| format!("Fm{i}").into_bytes()).collect();
        let forms: Vec<(&[u8], Vec<Operation>, Option<[f64; 6]>, Option<[f64; 4]>, Option<ObjectId>)> =
            names
                .iter()
                .map(|nm| {
                    let mut ops = vec![
                        Operation::new("BT", vec![]),
                        Operation::new("Tf", vec![Object::Name(b"F1".to_vec()), rl(10.0)]),
                    ];
                    ops.extend(show_at(10.0, 700.0, std::str::from_utf8(nm).unwrap()));
                    ops.push(Operation::new("ET", vec![]));
                    (nm.as_slice(), ops, None, None, None)
                })
                .collect();
        let page_ops: Vec<Operation> = (0..n)
            .map(|i| {
                Operation::new("Do", vec![Object::Name(format!("Fm{i}").into_bytes())])
            })
            .collect();
        let (doc, page_id) = pdf_with_named_forms(page_ops, &forms);
        let (spans, budget) = walk_page(&doc, page_id);
        assert_eq!(spans.len(), n, "all distinct forms must contribute a span");
        assert_eq!(budget.do_left, MAX_FORM_DO_PER_PAGE - n);
    }

    #[test]
    fn a_form_repeated_600_times_yields_600_runs() {
        // The same form drawn 600 times at distinct positions is content, not
        // overdraw; the old 512 cap silently dropped the last 88.
        let form_ops = {
            let mut v = vec![
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec![Object::Name(b"F1".to_vec()), rl(10.0)]),
            ];
            v.extend(show_at(10.0, 700.0, "R"));
            v.push(Operation::new("ET", vec![]));
            v
        };
        let n = 600usize;
        let mut page_ops = Vec::new();
        for i in 0..n {
            page_ops.push(Operation::new("q", vec![]));
            page_ops.push(Operation::new(
                "cm",
                vec![
                    rl(1.0), rl(0.0), rl(0.0), rl(1.0),
                    rl(10.0 + i as f64 * 4.0), rl(700.0),
                ],
            ));
            page_ops.push(Operation::new("Do", vec![Object::Name(b"Fm0".to_vec())]));
            page_ops.push(Operation::new("Q", vec![]));
        }
        let (doc, page_id) =
            pdf_with_named_forms(page_ops, &[(b"Fm0", form_ops, None, None, None)]);
        let (spans, budget) = walk_page(&doc, page_id);
        assert_eq!(spans.len(), n, "600 distinct placements must survive");
        assert_eq!(budget.do_left, MAX_FORM_DO_PER_PAGE - n);
    }

    #[test]
    fn form_text_feeds_table_detection() {
        let mut form_ops = vec![
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec![Object::Name(b"F1".to_vec()), rl(10.0)]),
        ];
        let header = ["Ref", "Qty", "Total"];
        let rows = [
            ["A1", "2", "10,00"],
            ["B2", "1", "20,00"],
            ["C3", "5", "30,00"],
        ];
        for (ri, row) in std::iter::once(&header).chain(rows.iter()).enumerate() {
            let y = 700.0 - ri as f64 * 20.0;
            for (ci, cell) in row.iter().enumerate() {
                form_ops.extend(show_at(50.0 + ci as f64 * 100.0, y, cell));
            }
        }
        form_ops.push(Operation::new("ET", vec![]));
        let (doc, page_id) = pdf_with_named_forms(
            vec![Operation::new("Do", vec![Object::Name(b"Fm0".to_vec())])],
            &[(b"Fm0", form_ops, None, None, None)],
        );
        let pt = extract_page_glyphs(&doc, page_id, true, true, false).expect("page text");
        assert!(pt.tables >= 1, "form grid must be recovered as a table");
        assert!(pt.text.contains("---"), "GFM table separator missing:\n{}", pt.text);
    }

    // ------------------------------------------------------------------
    // B2: inherited page geometry and dominantly-vertical pages
    // ------------------------------------------------------------------

    fn pdf_page_with(
        page_ops: Vec<Operation>,
        page_rotate: Option<i64>,
        pages_rotate: Option<i64>,
        media: [f64; 4],
    ) -> (Document, ObjectId) {
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let font = doc.add_object(dictionary! {
            "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Courier",
        });
        let res = doc.add_object(dictionary! { "Font" => dictionary! { "F1" => font } });
        let content = doc.add_object(Stream::new(
            Dictionary::new(),
            Content { operations: page_ops }.encode().unwrap(),
        ));
        let mut pd = dictionary! {
            "Type" => "Page", "Parent" => pages_id, "Contents" => content, "Resources" => res,
        };
        if let Some(r) = page_rotate {
            pd.set(b"Rotate", r);
        }
        let page_id = doc.add_object(pd);
        let mut pages = dictionary! {
            "Type" => "Pages", "Kids" => vec![page_id.into()], "Count" => 1,
            "MediaBox" => vec![rl(media[0]), rl(media[1]), rl(media[2]), rl(media[3])],
        };
        if let Some(r) = pages_rotate {
            pages.set(b"Rotate", r);
        }
        doc.objects.insert(pages_id, Object::Dictionary(pages));
        let cat = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        doc.trailer.set("Root", cat);
        (doc, page_id)
    }

    fn tm_at(a: f64, b: f64, c: f64, d: f64, e: f64, f: f64) -> Operation {
        Operation::new(
            "Tm",
            vec![rl(a), rl(b), rl(c), rl(d), rl(e), rl(f)],
        )
    }

    /// A page rotated 90° clockwise by `/Rotate` must apply the rotation matrix
    /// (and report the swapped height), even when the attribute is inherited
    /// from `/Pages` rather than set on the page.
    #[test]
    fn rotate_is_read_from_the_page_and_inherited_from_pages() {
        let ops = vec![Operation::new("BT", vec![])];
        let (doc, page_id) = pdf_page_with(ops.clone(), Some(90), None, [0.0, 0.0, 200.0, 100.0]);
        let (m, h) = page_initial_transform(&doc, page_id);
        assert!((m.a - 0.0).abs() < 1e-9 && (m.b + 1.0).abs() < 1e-9, "rotate 90 matrix wrong: {m:?}");
        assert!((m.c - 1.0).abs() < 1e-9 && (m.d - 0.0).abs() < 1e-9);
        assert!((h - 200.0).abs() < 1e-9, "rotated display height must swap to width");

        // No page /Rotate, but 270° on the /Pages ancestor: inherited.
        let (doc2, pid2) = pdf_page_with(ops, None, Some(270), [0.0, 0.0, 200.0, 100.0]);
        let (m2, h2) = page_initial_transform(&doc2, pid2);
        assert!((m2.a - 0.0).abs() < 1e-9 && (m2.b - 1.0).abs() < 1e-9, "inherited rotate wrong: {m2:?}");
        assert!((m2.c + 1.0).abs() < 1e-9 && (m2.d - 0.0).abs() < 1e-9);
        assert!((h2 - 200.0).abs() < 1e-9);
    }

    /// A page whose text is entirely vertical (a sideways OCR layer) must be
    /// rotated upright and read, not dropped as margin furniture.
    #[test]
    fn dominantly_vertical_page_is_rotated_upright() {
        let mut ops = vec![
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec![Object::Name(b"F1".to_vec()), rl(10.0)]),
        ];
        // Bottom-to-top reading (upward): text x-axis maps to device +y.
        for (i, ch) in ["S", "I", "D", "E"].iter().enumerate() {
            ops.push(tm_at(0.0, 1.0, -1.0, 0.0, 100.0, 700.0 + i as f64 * 10.0));
            ops.push(Operation::new("Tj", vec![Object::string_literal(ch.to_string())]));
        }
        ops.push(Operation::new("ET", vec![]));
        let (doc, page_id) = pdf_page_with(ops, None, None, [0.0, 0.0, 595.0, 842.0]);
        let pt = extract_page_glyphs(&doc, page_id, true, true, false).expect("page text");
        let letters: String = pt.text.split_whitespace().collect();
        assert!(letters.contains("SIDE"), "vertical page was not rotated upright: {:?}", pt.text);
        assert!(pt.text_ops_seen);
    }

    /// A minority vertical run — a page stamp or letterhead — must still be
    /// dropped from the body when layout analysis is on, and must not trigger
    /// the dominant-vertical rotation.
    #[test]
    fn minority_vertical_stamp_stays_out_of_a_horizontal_body() {
        let ops = vec![
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec![Object::Name(b"F1".to_vec()), rl(10.0)]),
            tm_at(1.0, 0.0, 0.0, 1.0, 50.0, 500.0),
            Operation::new("Tj", vec![Object::string_literal("BODY TEXT".to_string())]),
            tm_at(0.0, 1.0, -1.0, 0.0, 20.0, 100.0),
            Operation::new("Tj", vec![Object::string_literal("STAMP".to_string())]),
            Operation::new("ET", vec![]),
        ];
        let (doc, page_id) = pdf_page_with(ops, None, None, [0.0, 0.0, 595.0, 842.0]);
        let pt = extract_page_glyphs(&doc, page_id, true, true, false).expect("page text");
        assert!(pt.text.contains("BODY"), "horizontal body must survive: {:?}", pt.text);
        assert!(!pt.text.contains("STAMP"), "minority stamp leaked into the body: {:?}", pt.text);
        assert!(
            pt.blocks.iter().any(|b| b.kind == "margin" && b.text.contains("STAMP")),
            "the stamp must still be reported as a margin block"
        );
    }
}
