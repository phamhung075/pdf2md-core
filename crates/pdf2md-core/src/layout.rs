//! Geometry-based text layout reconstruction (Stage 1 + 2 of the native layout
//! engine).
//!
//! Some producers (LibreOffice forms, print drivers, table tools) place **one
//! glyph per text object** with absolute `Tm`/`TD` coordinates and no space
//! glyphs between words. For those pages the content stream is *not* in reading
//! order and word boundaries live in the inter-glyph gaps, so string-order
//! extraction produces "V i e P r i v e e" or merged "Passeportencours…".
//!
//! This module rebuilds the page the way a reader sees it:
//!
//! 1. **Glyph geometry** — track the PDF text state (`Tm`/`Td`/`TD`/`T*` and
//!    the `cm`/`q`/`Q` CTM) to place every glyph in device space, and look up
//!    each glyph's natural advance width from the font metrics (`/Widths` for
//!    simple fonts, descendant `/W` + `/DW` for Type0/CID fonts).
//! 2. **Words → lines → blocks** — cluster glyphs by device coordinates,
//!    detecting a word boundary when the actual gap exceeds the glyph's natural
//!    advance (the excess is the encoded space), and sort into reading order
//!    (top-to-bottom, left-to-right).
//!
//! This is the deterministic, sub-millisecond equivalent of the layout stage a
//! heavy ML pipeline (e.g. Docling) performs — exact for digital PDFs and
//! roughly 100x cheaper.

use std::collections::HashMap;

use lopdf::content::{Content, Operation};
use lopdf::{Dictionary, Document, Object, ObjectId};

use crate::text_extract::{deref, get_name, parse_cmap, resolve_codec, CMapCodec, Codec, PageText};

// ---------------------------------------------------------------------------
// 2x3 affine matrix (PDF matrix: [a b c d e f])
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct Mtx {
    a: f64,
    b: f64,
    c: f64,
    d: f64,
    e: f64,
    f: f64,
}

impl Mtx {
    const ID: Self = Mtx {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: 1.0,
        e: 0.0,
        f: 0.0,
    };

    /// `self = self × rhs` (post-multiply).
    fn post_mul(&mut self, rhs: &Mtx) {
        let a = self.a * rhs.a + self.b * rhs.c;
        let b = self.a * rhs.b + self.b * rhs.d;
        let c = self.c * rhs.a + self.d * rhs.c;
        let d = self.c * rhs.b + self.d * rhs.d;
        let e = self.e * rhs.a + self.f * rhs.c + rhs.e;
        let f = self.e * rhs.b + self.f * rhs.d + rhs.f;
        *self = Mtx { a, b, c, d, e, f };
    }

    /// `self = lhs × self` (pre-multiply).
    fn pre_mul(&mut self, lhs: &Mtx) {
        let a = lhs.a * self.a + lhs.b * self.c;
        let b = lhs.a * self.b + lhs.b * self.d;
        let c = lhs.c * self.a + lhs.d * self.c;
        let d = lhs.c * self.b + lhs.d * self.d;
        let e = lhs.e * self.a + lhs.f * self.c + self.e;
        let f = lhs.e * self.b + lhs.f * self.d + self.f;
        *self = Mtx { a, b, c, d, e, f };
    }

    fn apply(&self, x: f64, y: f64) -> (f64, f64) {
        (self.a * x + self.c * y + self.e, self.b * x + self.d * y + self.f)
    }

    fn translate(tx: f64, ty: f64) -> Self {
        Mtx {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            e: tx,
            f: ty,
        }
    }

    fn h_scale(&self) -> f64 {
        (self.a * self.a + self.b * self.b).sqrt()
    }
}

fn num(o: &Object) -> Option<f64> {
    o.as_float()
        .ok()
        .map(|v| v as f64)
        .or_else(|| o.as_i64().ok().map(|v| v as f64))
}

fn mtx_from(op: &Operation) -> Option<Mtx> {
    Some(Mtx {
        a: num(op.operands.get(0)?)?,
        b: num(op.operands.get(1)?)?,
        c: num(op.operands.get(2)?)?,
        d: num(op.operands.get(3)?)?,
        e: num(op.operands.get(4)?)?,
        f: num(op.operands.get(5)?)?,
    })
}

fn string_bytes(o: &Object) -> Option<&[u8]> {
    match o {
        Object::String(bytes, _) => Some(bytes),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Font advance widths
// ---------------------------------------------------------------------------

enum Widths {
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
    fn width(&self, bytes: &[u8]) -> Option<f64> {
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

fn resolve_widths(doc: &Document, font: &Dictionary) -> Widths {
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

#[derive(Clone)]
struct Span {
    text: String,
    x: f64,
    y: f64,
    /// Effective device font size (used to scale all gap thresholds).
    size: f64,
    /// Natural advance width of the first glyph, in device points.
    advance: f64,
}

/// Decode one string operand into a positioned span (or nothing if it decodes
/// to no text). `em_offset` is a preceding TJ-array number in 1/1000 em units.
fn push_span(
    codec: &Codec,
    width: &Widths,
    bytes: &[u8],
    em_offset: f64,
    tlm: &Mtx,
    ctm: &Mtx,
    tfs: f64,
    spans: &mut Vec<Span>,
) {
    let mut text = String::new();
    codec.decode(bytes, &mut text);
    if text.is_empty() {
        return;
    }
    let hscale = tlm.h_scale() * ctm.h_scale();
    let size = tfs * hscale;
    let (x, y) = ctm.apply(tlm.e + em_offset / 1000.0 * tfs * tlm.h_scale(), tlm.f);
    let advance = width
        .width(bytes)
        .map(|w| w / 1000.0 * tfs * hscale)
        .unwrap_or_else(|| {
            // No metrics: assume a typical letter advance (~0.5 em) so the
            // gap detector still separates words reasonably.
            0.5 * size
        });
    spans.push(Span {
        text,
        x,
        y,
        size,
        advance,
    });
}

/// Extract text for a glyph-positioned page using geometry reconstruction.
pub fn extract_page_glyphs(doc: &Document, page_id: ObjectId) -> Result<PageText, String> {
    let fonts = doc.get_page_fonts(page_id).map_err(|e| e.to_string())?;
    let has_fonts = !fonts.is_empty();

    let mut codecs: Vec<(Vec<u8>, Codec)> = Vec::new();
    let mut widths: Vec<(Vec<u8>, Widths)> = Vec::new();
    for (name, fd) in &fonts {
        if let Some(c) = resolve_codec(doc, fd) {
            codecs.push((name.clone(), c));
        }
        widths.push((name.clone(), resolve_widths(doc, fd)));
    }

    let content: Content<Vec<Operation>> = doc
        .get_and_decode_page_content(page_id)
        .map_err(|e| e.to_string())?;

    let mut ctm = Mtx::ID;
    let mut ctm_stack: Vec<Mtx> = Vec::new();
    let mut tlm = Mtx::ID;
    let mut tfs = 0.0f64;
    let mut leading = 0.0f64;
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
                    ctm.post_mul(&m);
                }
            }
            "BT" => tlm = Mtx::ID,
            "Tm" => {
                if let Some(m) = mtx_from(op) {
                    tlm = m;
                }
            }
            "Td" | "TD" => {
                if let (Some(tx), Some(ty)) = (
                    num(op.operands.first().unwrap_or(&Object::Null)),
                    num(op.operands.get(1).unwrap_or(&Object::Null)),
                ) {
                    tlm.pre_mul(&Mtx::translate(tx, ty));
                    if op.operator == "TD" {
                        leading = -ty;
                    }
                }
            }
            "T*" => tlm.pre_mul(&Mtx::translate(0.0, -leading)),
            "TL" => {
                if let Some(v) = op.operands.first().and_then(num) {
                    leading = v;
                }
            }
            "Tf" => {
                let name = op.operands.first().and_then(|o| o.as_name().ok());
                cur_font = name.and_then(|nm| codecs.iter().position(|(n, _)| n.as_slice() == nm));
                if let Some(sz) = op.operands.get(1).and_then(num) {
                    tfs = sz;
                }
            }
            "Tj" | "'" | "\"" => {
                text_ops_seen = true;
                let str_idx = if op.operator == "\"" { 2 } else { 0 };
                if op.operator == "'" {
                    tlm.pre_mul(&Mtx::translate(0.0, -leading));
                }
                if let (Some(ci), Some(bytes)) = (
                    cur_font,
                    op.operands.get(str_idx).and_then(string_bytes),
                ) {
                    let (codec, width) = (&codecs[ci].1, &widths[ci].1);
                    push_span(codec, width, bytes, 0.0, &tlm, &ctm, tfs, &mut spans);
                }
            }
            "TJ" => {
                text_ops_seen = true;
                let Some(arr) = op.operands.first().and_then(|o| o.as_array().ok()) else {
                    continue;
                };
                let Some(ci) = cur_font else { continue };
                let (codec, width) = (&codecs[ci].1, &widths[ci].1);
                // `offset` is the running horizontal displacement from the
                // current text position, in 1/1000 em units. A TJ-array number
                // N is *subtracted* from the position (positive N moves left),
                // and each string advances by its glyph width.
                let mut offset = 0.0f64;
                for item in arr {
                    match item {
                        Object::String(bytes, _) => {
                            push_span(codec, width, bytes, offset, &tlm, &ctm, tfs, &mut spans);
                            offset += width.width(bytes).unwrap_or(500.0);
                        }
                        Object::Integer(v) => offset -= *v as f64,
                        Object::Real(v) => offset -= *v as f64,
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    Ok(PageText {
        text: cluster(&spans),
        text_ops_seen,
        has_fonts,
    })
}

/// Reassemble device-positioned glyph spans into reading order.
fn cluster(spans: &[Span]) -> String {
    if spans.is_empty() {
        return String::new();
    }

    // Group consecutive spans into runs by device baseline y, preserving
    // stream order. For glyph-positioned producers each visual line is emitted
    // as one contiguous, already-left-to-right run, so we must NOT re-sort by x
    // (a space glyph can share an x with a following letter — e.g. after a
    // bullet — and x-sorting would move it past the letter).
    let mut runs: Vec<Vec<Span>> = Vec::new();
    for span in spans {
        let eps = 0.5 * span.size.max(0.1);
        match runs.last_mut() {
            Some(last) if (span.y - last.last().unwrap().y).abs() <= eps => last.push(span.clone()),
            _ => runs.push(vec![span.clone()]),
        }
    }

    // Merge adjacent runs that sit on the same visual line (e.g. a two-column
    // row where the right column was emitted right after the left one).
    let mut lines: Vec<Vec<Span>> = Vec::new();
    for run in runs {
        let eps = 0.5 * run[0].size.max(0.1);
        match lines.last_mut() {
            Some(last) if (run[0].y - last[0].y).abs() <= eps => last.extend(run),
            _ => lines.push(run),
        }
    }

    // Top-to-bottom (larger device y first).
    lines.sort_by(|a, b| {
        b[0].y
            .partial_cmp(&a[0].y)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let mut out = String::new();
    let mut prev_line_y: Option<f64> = None;

    for line in &lines {
        let size = line[0].size.max(0.1);

        // A large vertical gap opens a new block (paragraph).
        if let Some(py) = prev_line_y {
            if py - line[0].y > 2.0 * size {
                out.push('\n');
            }
        }

        let mut line_text = String::new();
        let mut prev_x: Option<f64> = None;
        let mut prev_advance = 0.0f64;

        for span in line {
            if span.text.is_empty() {
                continue;
            }
            if let Some(px) = prev_x {
                let gap = span.x - px;
                let space_adv = 0.25 * size;
                if span.text != " " {
                    if gap > 2.5 * size {
                        // Distinct column / element on the same row.
                        if !line_text.is_empty() && !line_text.ends_with('\n') {
                            line_text.push('\n');
                        }
                    } else if gap - prev_advance > 0.35 * space_adv {
                        // Encoded word gap (actual gap exceeds the glyph's
                        // natural advance).
                        if !line_text.is_empty() && !line_text.ends_with(' ') && !line_text.ends_with('\n') {
                            line_text.push(' ');
                        }
                    }
                }
            }
            // Append the glyph, collapsing runs of spaces (producers often emit
            // stray extra space glyphs for alignment).
            if span.text == " " {
                if !line_text.is_empty() && !line_text.ends_with(' ') && !line_text.ends_with('\n') {
                    line_text.push(' ');
                }
            } else {
                line_text.push_str(&span.text);
            }
            prev_x = Some(span.x);
            prev_advance = span.advance;
        }

        out.push_str(line_text.trim_end());
        out.push('\n');
        prev_line_y = Some(line[0].y);
    }

    out.trim_end().to_string()
}
