// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! PDF content-stream glyph extraction, font metric resolution, and 2D span aggregation.

use std::collections::HashMap;

use lopdf::content::{Content, Operation};
use lopdf::{Dictionary, Document, Object, ObjectId};

use crate::text_extract::{deref, get_name, parse_cmap, resolve_codec, CMapCodec, Codec, PageText};
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
                let code = *bytes.first()? as usize;
                let w = t[code];
                if w > 0.0 {
                    Some(w)
                } else {
                    None
                }
            }
            Widths::Cid {
                map,
                default,
                encoding,
            } => {
                let cid = match encoding {
                    Some(cm) => cm.lookup(bytes)?,
                    None => {
                        let mut v = 0u32;
                        for &b in bytes {
                            v = (v << 8) | b as u32;
                        }
                        v
                    }
                };
                Some(map.get(&cid).copied().unwrap_or(*default))
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
        let mut i = 0usize;
        while i < arr.len() {
            let Some(c0) = num(&arr[i]).map(|v| v as u32) else {
                i += 1;
                continue;
            };
            i += 1;
            if i >= arr.len() {
                break;
            }
            if let Ok(vals) = arr[i].as_array() {
                for (k, it) in vals.iter().enumerate() {
                    if let Some(v) = num(it) {
                        map.insert(c0 + k as u32, v);
                    }
                }
                i += 1;
            } else if i + 1 < arr.len() {
                if let (Some(c1), Some(v)) = (num(&arr[i]).map(|x| x as u32), num(&arr[i + 1])) {
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
    /// Natural advance width of the first glyph, in device points.
    pub advance: f64,
    pub is_bold: bool,
    pub is_italic: bool,
    pub is_vertical: bool,
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
        if name.contains("bold") || name.contains("black") || name.contains("heavy") {
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

    if let Some(fd) = desc {
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
    }

    (is_bold, is_italic)
}

/// Decode one string operand into a positioned span (or nothing if it decodes
/// to no text). `em_offset` is a preceding TJ-array number in 1/1000 em units.
pub(crate) fn push_span(
    codec: &Codec,
    width: &Widths,
    bytes: &[u8],
    em_offset: f64,
    tm: &Mtx,
    ctm: &Mtx,
    tfs: f64,
    style: (bool, bool),
    spans: &mut Vec<Span>,
) {
    let mut text = String::new();
    codec.decode(bytes, &mut text);
    if text.is_empty() {
        return;
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
    let eff_a = ctm.a * tm.a + ctm.c * tm.b;
    let eff_b = ctm.b * tm.a + ctm.d * tm.b;
    let is_vertical = eff_b.abs() > 0.7 * hscale && eff_a.abs() < 0.3 * hscale;
    spans.push(Span {
        text,
        x,
        y,
        size,
        advance,
        is_bold: style.0,
        is_italic: style.1,
        is_vertical,
    });
}

/// Height of the page media box in device points (best effort).
pub fn page_height_of(doc: &Document, page_id: ObjectId) -> Option<f64> {
    let dict = doc.get_dictionary(page_id).ok()?;
    let mb = dict.get(b"MediaBox").ok()?.as_array().ok()?;
    let g = |i: usize| -> Option<f64> {
        mb.get(i)
            .and_then(|o| o.as_float().ok().map(|f| f as f64))
            .or_else(|| mb.get(i).and_then(|o| o.as_i64().ok().map(|v| v as f64)))
    };
    let y0 = g(1)?;
    let y1 = g(3)?;
    Some((y1 - y0).abs())
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
    }

    lines
}

/// Extract text for a glyph-positioned page using geometry reconstruction.
/// When `detect_tables` is false, returns the plain reading-order text with
/// no table recovery (byte-identical to the table-less renderer).
pub fn extract_page_glyphs(
    doc: &Document,
    page_id: ObjectId,
    detect_tables: bool,
    detect_layout: bool,
) -> Result<PageText, String> {
    let fonts = doc.get_page_fonts(page_id).map_err(|e| e.to_string())?;
    let has_fonts = !fonts.is_empty();

    struct FontInfo {
        name: Vec<u8>,
        codec: Codec,
        widths: Widths,
        style: (bool, bool),
    }

    let mut fonts_info: Vec<FontInfo> = Vec::new();
    for (name, fd) in &fonts {
        if let Some(c) = resolve_codec(doc, fd) {
            fonts_info.push(FontInfo {
                name: name.clone(),
                codec: c,
                widths: resolve_widths(doc, fd),
                style: resolve_font_style(doc, fd),
            });
        }
    }

    let content: Content<Vec<Operation>> = doc
        .get_and_decode_page_content(page_id)
        .map_err(|e| e.to_string())?;

    let mut ctm = Mtx::ID;
    let mut ctm_stack: Vec<Mtx> = Vec::new();
    let mut tlm = Mtx::ID;
    let mut tm = Mtx::ID;
    let mut tfs = 0.0f64;
    let mut leading = 0.0f64;
    let mut tc = 0.0f64;
    let mut tw = 0.0f64;
    let mut tz = 100.0f64;
    let mut cur_font: Option<usize> = None;
    let mut text_ops_seen = false;
    let mut spans: Vec<Span> = Vec::new();

    for op in &content.operations {
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
                text_ops_seen = true;
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
                    push_span(codec, width, bytes, 0.0, &tm, &ctm, tfs, style, &mut spans);
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
                text_ops_seen = true;
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
                            push_span(codec, width, bytes, offset, &tm, &ctm, tfs, style, &mut spans);
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
            _ => {}
        }
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
                });
            }
        }
    }

    let lines = build_lines(&horizontal_spans);
    let page_height = page_height_of(doc, page_id).unwrap_or(842.0);
    let mut hits = if detect_tables {
        find_tables(&lines)
    } else {
        Vec::new()
    };
    // Stage-3b: re-run the grid scan with a wider alignment tolerance over
    // rows the strict pass missed (jittered / borderless tables).
    if detect_tables {
        let gap_hits = find_gap_tables(&lines, &hits);
        hits.extend(gap_hits);
        hits.sort_by(|a, b| a.start.cmp(&b.start));
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
                outside_prose
            });
        }
    }
    let table_rendered = !hits.is_empty();
    let mut text = if table_rendered {
        // Byte-identical to the plain text renderer when no table is found.
        render_with_tables(&lines, &hits)
    } else if detect_layout {
        render_human_order(&lines, page_height, true)
    } else {
        render_cluster(&lines)
    };

    let mut blocks = if detect_layout {
        build_doc_blocks(&lines, page_height)
    } else {
        Vec::new()
    };

    if !vertical_text.is_empty() {
        if !text.is_empty() {
            if text.ends_with('\n') {
                text.push('\n');
            } else {
                text.push_str("\n\n");
            }
        }
        text.push_str(&vertical_text);
        blocks.extend(vertical_blocks);
    }

    Ok(PageText {
        text,
        text_ops_seen,
        has_fonts,
        tables: if table_rendered { hits.len() } else { 0 },
        blocks,
    })
}
