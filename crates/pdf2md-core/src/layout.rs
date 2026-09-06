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
use crate::{BoundingBox, CanvasTable};

// ---------------------------------------------------------------------------
// 2x3 affine matrix (PDF matrix: [a b c d e f])
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
pub(crate) struct Mtx {
    a: f64,
    b: f64,
    c: f64,
    d: f64,
    e: f64,
    f: f64,
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

    fn h_scale(&self) -> f64 {
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
                if let (Some(ci), Some(bytes)) =
                    (cur_font, op.operands.get(str_idx).and_then(string_bytes))
                {
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

    let lines = build_lines(&spans);
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
    // A genuine two-column *reading* layout (two wide prose columns read
    // column-by-column) must win over the grid detector, which mistakes it for
    // a 2-column table. Real pipe tables have short cell tokens, so we only
    // prefer the layout when every detected "row" is long prose on both sides.
    let prose_columns = detect_layout && page_two_columns(&lines).is_some();
    let table_rendered = !hits.is_empty() && !prose_columns;
    let text = if table_rendered {
        // Byte-identical to the plain text renderer when no table is found.
        render_with_tables(&lines, &hits)
    } else if detect_layout {
        render_human_order(&lines, page_height, true)
    } else {
        render_cluster(&lines)
    };

    let blocks = if detect_layout {
        build_doc_blocks(&lines, page_height)
    } else {
        Vec::new()
    };

    Ok(PageText {
        text,
        text_ops_seen,
        has_fonts,
        tables: if table_rendered { hits.len() } else { 0 },
        blocks,
    })
}

/// Height of the page media box in device points (best effort).
fn page_height_of(doc: &Document, page_id: ObjectId) -> Option<f64> {
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
fn build_lines(spans: &[Span]) -> Vec<Vec<Span>> {
    if spans.is_empty() {
        return Vec::new();
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

    lines
}

/// Render pre-built visual lines to plain text (block/paragraph separation +
/// word gaps). Line model must come from `build_lines`.
fn render_cluster(lines: &[Vec<Span>]) -> String {
    if lines.is_empty() {
        return String::new();
    }

    let mut out = String::new();
    let mut prev_line_y: Option<f64> = None;

    for line in lines {
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
                        if !line_text.is_empty()
                            && !line_text.ends_with(' ')
                            && !line_text.ends_with('\n')
                        {
                            line_text.push(' ');
                        }
                    }
                }
            }
            // Append the glyph, collapsing runs of spaces (producers often emit
            // stray extra space glyphs for alignment).
            if span.text == " " {
                if !line_text.is_empty() && !line_text.ends_with(' ') && !line_text.ends_with('\n')
                {
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

// ---------------------------------------------------------------------------
// Stage 3 — table recovery on the glyph layout
// ---------------------------------------------------------------------------
//
// Real producers (LibreOffice tables, print drivers, invoice tools) draw a
// grid by placing each cell's first word at absolute, left-aligned x
// positions. The deterministic signature of such a grid — and only such a
// grid — is that **several column-start x rulers repeat on every row of a
// run of consecutive rows**:
//
//   1. tokenize each visual line into words (same word-gap rules as the text
//      renderer) and remember each word's start x;
//   2. for each window of >= 3 consecutive rows, find the word-start x
//      positions present on EVERY row of the window (exact alignment, small
//      tolerance). Justified prose, bullet lists and radio-option lists only
//      ever line up one or two *coincidental* positions, and their first
//      column is a constant marker (the bullet / radio glyph), so they do not
//      qualify;
//   3. a window with >= 2 such rulers and >= 3 rows is a table: cells are the
//      words between ruler midpoints (first word of a cell starts at its
//      ruler, subsequent words of the same cell stay in the same band), rows
//      keep stream order, and the block is rendered as a GFM pipe table via
//      `CanvasTable::to_markdown`.
//
// Conservative by design: ragged paragraphs, single-column lists, bullets and
// form fill-ins never fire, so plain pages keep the byte-identical
// `render_cluster` output.

/// One decoded word with its device start x (the column-start ruler).
struct WordTok {
    text: String,
    x0: f64,
}

/// Split one visual line into words (same gap rules as the renderer).
fn line_words(line: &[Span]) -> Vec<WordTok> {
    let mut out: Vec<WordTok> = Vec::new();
    let mut text = String::new();
    let mut x0: Option<f64> = None;
    let mut prev_x: Option<f64> = None;
    let mut prev_advance = 0.0f64;

    let flush = |out: &mut Vec<WordTok>, text: &mut String, x0: &mut Option<f64>| {
        if let Some(s) = x0.take() {
            if !text.trim().is_empty() {
                out.push(WordTok {
                    text: std::mem::take(text).trim().to_string(),
                    x0: s,
                });
            }
            text.clear();
        }
    };

    for sp in line {
        if sp.text.is_empty() {
            continue;
        }
        let size = sp.size.max(0.1);
        let space_adv = 0.25 * size;
        let is_space = sp.text.chars().all(|c| c == ' ');
        if let Some(px) = prev_x {
            let gap = sp.x - px;
            // A big gap (> 2.5 * size) separates columns/elements; a smaller
            // excess over the natural advance is an encoded word space.
            let word_break = is_space
                || (sp.text != " " && (gap > 2.5 * size || gap - prev_advance > 0.35 * space_adv));
            if word_break {
                flush(&mut out, &mut text, &mut x0);
            }
        } else if is_space {
            continue; // leading stray space glyph
        }
        if x0.is_none() {
            x0 = Some(sp.x);
        }
        text.push_str(sp.text.trim());
        prev_x = Some(sp.x);
        prev_advance = sp.advance;
    }
    flush(&mut out, &mut text, &mut x0);
    out
}

/// A detected table: line range [start, end] (inclusive) plus cell rows.
struct TableHit {
    start: usize,
    end: usize,
    rows: Vec<Vec<String>>,
    bbox: BoundingBox,
}

/// Detect grid tables on a page's visual lines.
///
/// Strategy (deterministic, no ML): a glyph-positioned grid drawn by a table
/// producer places every cell's first word at the same absolute column x on
/// every row. We therefore look for **windows of >= 3 vertically-tight rows
/// that all start a word at the same >= 2 x positions** (sub-point tolerance).
/// Words are then bucketed by the midpoints between those column rulers, so a
/// multi-word cell ("Base - 03kVA") stays whole inside its band.
///
/// Guard rails against look-alikes in ordinary prose and forms:
///   * the window needs >= 2 recurring column starts on EVERY row — justified
///     or wrapped prose rarely aligns two word starts across 3+ lines;
///   * the first cell must vary between rows, so bullet / radio / option
///     lists (constant "-", bullet, "o" markers) never qualify;
///   * consecutive rulers must be a real column gutter apart (> 0.6 em), so a
///     single line that merely repeats a few words ("- pages ...", "- pages
///     où ...") is not mistaken for a multi-column grid.
fn scan_aligned_grids(lines: &[Vec<Span>], tol_mult: f64, covered: &[TableHit]) -> Vec<TableHit> {
    if lines.len() < 3 {
        return Vec::new();
    }

    // Word tokens + sorted start positions per visual line.
    struct RowInfo {
        words: Vec<WordTok>,
        starts: Vec<f64>,
        size: f64,
    }
    let info: Vec<RowInfo> = lines
        .iter()
        .map(|l| {
            let words = line_words(l);
            let mut starts: Vec<f64> = words.iter().map(|w| w.x0).collect();
            starts.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            RowInfo {
                words,
                starts,
                size: l[0].size.max(0.1),
            }
        })
        .collect();

    // Alignment tolerance: same absolute column start on every row. Stage-3b
    // (gap recovery) reruns the scan with a wider tolerance to catch tables
    // whose column starts jitter by a few points between rows.
    let tol = tol_mult
        * info
            .iter()
            .map(|r| (0.06 * r.size).clamp(0.5, 1.2))
            .fold(0.0f64, f64::max);

    // Rulers shared by rows lo..=hi (inclusive): word-start x present on every
    // row of the range, jitter-merged.
    fn shared_rulers(info: &[RowInfo], tol: f64, lo: usize, hi: usize) -> Vec<f64> {
        if hi <= lo {
            return Vec::new();
        }
        let mut cur: Vec<f64> = info[lo].starts.clone();
        for ri in (lo + 1)..=hi {
            cur.retain(|x| info[ri].starts.iter().any(|y| (y - x).abs() <= tol));
            if cur.is_empty() {
                return Vec::new();
            }
        }
        let mut merged: Vec<f64> = Vec::new();
        for x in cur {
            match merged.last_mut() {
                Some(m) if (x - *m).abs() <= tol => *m = (*m + x) / 2.0,
                _ => merged.push(x),
            }
        }
        merged
    }

    // Bucket words of row `ri` into columns separated by the ruler midpoints.
    fn bucket(info: &[RowInfo], ri: usize, rulers: &[f64]) -> Vec<String> {
        let ncol = rulers.len();
        let bounds: Vec<f64> = rulers.windows(2).map(|p| (p[0] + p[1]) / 2.0).collect();
        let mut cells: Vec<Vec<String>> = vec![Vec::new(); ncol];
        for w in &info[ri].words {
            // Column k owns words between mid(r_{k-1}, r_k) and mid(r_k, r_{k+1});
            // words left of every ruler midpoint still belong to column 0.
            let col = bounds.iter().position(|&b| w.x0 < b).unwrap_or(ncol - 1);
            cells[col].push(w.text.clone());
        }
        cells.into_iter().map(|c| c.join(" ")).collect()
    }

    // Vertical bands of consecutive rows that could sit in one grid (>= 2
    // words, tight line pitch, no paragraph gap inside). Rows already claimed
    // by a previous (stricter) scan are excluded entirely.
    let mut bands: Vec<Vec<usize>> = Vec::new();
    for (i, r) in info.iter().enumerate() {
        if r.words.len() < 2 {
            continue;
        }
        if covered.iter().any(|h| h.start <= i && i <= h.end) {
            continue;
        }
        match bands.last_mut() {
            Some(band) => {
                let prev = *band.last().unwrap();
                let gap = lines[prev][0].y - lines[i][0].y;
                let scale = info[prev].size.max(r.size);
                if gap >= 0.0 && gap <= 2.4 * scale {
                    band.push(i);
                } else {
                    bands.push(vec![i]);
                }
            }
            None => bands.push(vec![i]),
        }
    }

    let mut hits: Vec<TableHit> = Vec::new();
    for band in bands {
        if band.len() < 3 {
            continue;
        }
        let mut lo = 0usize;
        while lo < band.len() {
            // Need at least 3 rows to form a table window.
            if lo + 2 >= band.len() {
                break;
            }
            // Greedily extend the window while all rows share >= 2 rulers.
            let mut hi = lo + 1;
            let mut rulers = shared_rulers(&info, tol, band[lo], band[hi]);
            while hi + 1 < band.len() && rulers.len() >= 2 {
                let trial = shared_rulers(&info, tol, band[lo], band[hi + 1]);
                if trial.len() >= 2 {
                    hi += 1;
                    rulers = trial;
                } else {
                    break;
                }
            }
            // If the very first pair already lacks 2 rulers, this row can
            // never be the head of a grid: move on.
            if rulers.len() < 2 {
                lo += 1;
                continue;
            }
            let win_rows: Vec<usize> = band[lo..=hi].to_vec();
            if hi - lo + 1 >= 3 && rulers.len() >= 2 {
                // Column gutter sanity: consecutive rulers are far apart.
                let max_size = win_rows
                    .iter()
                    .map(|&i| info[i].size)
                    .fold(0.0f64, f64::max);
                let ok_gutter = rulers.windows(2).all(|p| p[1] - p[0] > 0.6 * max_size);
                if ok_gutter {
                    // The first cell (left of the first ruler midpoint) must
                    // vary across rows: a constant marker means a list.
                    let mid1 = (rulers[0] + rulers[1]) / 2.0;
                    let mut distinct: std::collections::HashSet<String> =
                        std::collections::HashSet::new();
                    for &i in &win_rows {
                        let first: String = info[i]
                            .words
                            .iter()
                            .filter(|w| w.x0 < mid1)
                            .map(|w| w.text.as_str())
                            .collect::<Vec<_>>()
                            .join(" ");
                        if !first.trim().is_empty() {
                            distinct.insert(first);
                        }
                    }
                    if distinct.len() >= 2 {
                        // Cells per row from the ruler bands.
                        let table_rows: Vec<Vec<String>> = win_rows
                            .iter()
                            .map(|&i| bucket(&info, i, &rulers))
                            .collect();
                        // Drop fully-empty edge columns.
                        let ncol = rulers.len();
                        let mut c0 = 0usize;
                        let mut c1 = ncol;
                        while c0 < c1 && table_rows.iter().all(|r| r[c0].trim().is_empty()) {
                            c0 += 1;
                        }
                        while c1 > c0 && table_rows.iter().all(|r| r[c1 - 1].trim().is_empty()) {
                            c1 -= 1;
                        }
                        if c1 - c0 >= 2 {
                            let rows2: Vec<Vec<String>> =
                                table_rows.iter().map(|r| r[c0..c1].to_vec()).collect();
                            // Guard rails against look-alikes that align but are
                            // not data tables: TOC rows with dot leaders, body
                            // prose split into two long-text columns, radio /
                            // bullet lists with a constant marker column.
                            if !is_tabular_rows(&rows2) {
                                lo += 1;
                                continue;
                            }
                            let mut min_x = f64::INFINITY;
                            let mut max_x = f64::NEG_INFINITY;
                            let mut min_y = f64::INFINITY;
                            let mut max_y = f64::NEG_INFINITY;
                            for &i in &win_rows {
                                for sp in &lines[i] {
                                    min_x = min_x.min(sp.x);
                                    max_x = max_x.max(sp.x + sp.advance);
                                    min_y = min_y.min(sp.y);
                                    max_y = max_y.max(sp.y);
                                }
                            }
                            hits.push(TableHit {
                                start: win_rows[0],
                                end: *win_rows.last().unwrap(),
                                rows: rows2,
                                bbox: BoundingBox::new(min_x, min_y, max_x, max_y),
                            });
                            lo = hi + 1;
                            continue;
                        }
                    }
                }
            }
            lo += 1;
        }
    }
    hits
}

// ---------------------------------------------------------------------------
// Stage 3b — jitter-tolerant grid recovery (borderless / ragged tables)
// ---------------------------------------------------------------------------
//
// Stage-3 requires column-start rulers that repeat on *every* row of a window
// (exact alignment). Real-world tables from invoice tools, print drivers and
// form generators frequently break that contract: column starts jitter by a
// few points between rows, a cell wraps onto a second visual line, or one
// column holds a long sentence next to short codes. Stage-3b therefore re-runs
// the *identical* grid scan with a wider alignment tolerance over the rows the
// strict pass did not claim, so tables whose starts are only approximately
// aligned are still recovered, while justified prose (which never shares >= 2
// column starts across 3+ rows) still cannot fire. Cell text keeps the strict
// pass' word bucketing, and the shared `is_tabular_rows` guard below rejects
// TOC dot leaders, aligned body prose and bullet / radio marker columns.

/// Stage-3 table recovery: strict ruler alignment (the default pass).
fn find_tables(lines: &[Vec<Span>]) -> Vec<TableHit> {
    scan_aligned_grids(lines, 1.0, &[])
}

/// Stage-3b recovery pass: the same grid scan with a wider alignment
/// tolerance, over rows the strict pass did not claim (jittered tables).
fn find_gap_tables(lines: &[Vec<Span>], covered: &[TableHit]) -> Vec<TableHit> {
    scan_aligned_grids(lines, 2.0, covered)
}

/// True when a candidate grid is a genuine data table rather than one of the
/// alignment look-alikes. Cells are supplied per row (empty strings allowed).
///
/// Rejections:
///   * any column that is a dot-leader column (>= half of its cells are
///     leader runs like "......") — TOC / index rows;
///   * a constant short first column across rows (bullet / radio markers);
///   * grids where no column is short (<= ~1.6 words/cell on average) and
///     where the overall cell verbosity looks like prose (>= 6 words/cell).
fn is_tabular_rows(rows: &[Vec<String>]) -> bool {
    let is_leader = |c: &str| -> bool {
        let t = c.trim();
        if t.is_empty() {
            return false;
        }
        let n = t.chars().count();
        let filler = t
            .chars()
            .filter(|ch| matches!(ch, '.' | '·' | '•' | '_' | ' '))
            .count();
        (t.starts_with('.') || t.starts_with('·') || t.starts_with('•')) && filler * 2 >= n
    };

    let data: Vec<&Vec<String>> = rows
        .iter()
        .filter(|r| r.iter().any(|c| !c.trim().is_empty()))
        .collect();
    if data.len() < 2 {
        return false;
    }
    let cols = data.iter().map(|r| r.len()).max().unwrap_or(0);
    if cols < 2 {
        return false;
    }
    let non_empty: Vec<usize> = (0..cols)
        .map(|k| {
            data.iter()
                .filter(|r| r.get(k).map_or(false, |c| !c.trim().is_empty()))
                .count()
        })
        .collect();
    let eff: Vec<usize> = (0..cols).filter(|&k| non_empty[k] > 0).collect();
    if eff.len() < 2 {
        return false;
    }

    // Dot-leader column (TOC dotted rows).
    for &k in &eff {
        let cells: Vec<&str> = data
            .iter()
            .filter_map(|r| {
                let c = r.get(k).map_or("", |c| c.as_str()).trim();
                if c.is_empty() {
                    None
                } else {
                    Some(c)
                }
            })
            .collect();
        if cells.is_empty() {
            continue;
        }
        let leaders = cells.iter().filter(|c| is_leader(c)).count();
        if leaders * 2 >= cells.len() {
            return false;
        }
    }

    // Constant short first column (bullets / radio markers like "-" or "o").
    if eff[0] == 0 {
        let first: Vec<&str> = data
            .iter()
            .map(|r| r[0].trim())
            .filter(|c| !c.is_empty())
            .collect();
        if !first.is_empty()
            && first.iter().all(|c| *c == first[0])
            && first[0].chars().count() <= 2
        {
            return false;
        }
    }

    // A data grid must contain at least one short column (codes, amounts,
    // dates, labels); grids whose every cell is long prose are aligned body
    // text, not tables.
    let mut short_col = false;
    let mut total_tokens = 0usize;
    let mut total_cells = 0usize;
    for &k in &eff {
        let cells: Vec<&str> = data
            .iter()
            .filter_map(|r| {
                let c = r.get(k).map_or("", |c| c.as_str()).trim();
                if c.is_empty() {
                    None
                } else {
                    Some(c)
                }
            })
            .collect();
        if cells.is_empty() {
            continue;
        }
        let tokens: usize = cells.iter().map(|c| c.split_whitespace().count()).sum();
        total_tokens += tokens;
        total_cells += cells.len();
        let mean = tokens as f64 / cells.len() as f64;
        if mean <= 2.2 {
            short_col = true;
        }
    }
    if !short_col {
        return false;
    }
    let overall = total_tokens as f64 / total_cells as f64;
    overall < 6.0
}

/// Render a page's visual lines to text, replacing detected table blocks with
/// GFM pipe tables. Non-table lines use the exact same rules as `render_cluster`.
fn render_with_tables(lines: &[Vec<Span>], tables: &[TableHit]) -> String {
    let mut out = String::new();
    let mut prev_line_y: Option<f64> = None;
    let mut t = 0usize;

    let push_line = |out: &mut String, line: &[Span], prev_line_y: &mut Option<f64>| {
        let size = line[0].size.max(0.1);
        if let Some(py) = *prev_line_y {
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
                        if !line_text.is_empty() && !line_text.ends_with('\n') {
                            line_text.push('\n');
                        }
                    } else if gap - prev_advance > 0.35 * space_adv {
                        if !line_text.is_empty()
                            && !line_text.ends_with(' ')
                            && !line_text.ends_with('\n')
                        {
                            line_text.push(' ');
                        }
                    }
                }
            }
            if span.text == " " {
                if !line_text.is_empty() && !line_text.ends_with(' ') && !line_text.ends_with('\n')
                {
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
        *prev_line_y = Some(line[0].y);
    };

    let mut i = 0usize;
    while i < lines.len() {
        // A table block starting exactly at this line: emit GFM instead of
        // the plain rows.
        if t < tables.len() && tables[t].start == i {
            let hit = &tables[t];
            // Blank line before the table (markdown block separation).
            if !out.is_empty() && !out.ends_with("\n\n") {
                out.push('\n');
            }
            let table = CanvasTable {
                rows: hit.rows.clone(),
                bbox: hit.bbox.clone(),
            };
            out.push_str(&table.to_markdown());
            if !out.ends_with('\n') {
                out.push('\n');
            }
            out.push('\n'); // blank line after the table
            prev_line_y = Some(lines[hit.end][0].y);
            t += 1;
            i = hit.end + 1; // skip the table's own lines
            continue;
        }
        // Defensive: a line strictly inside a table range (should not happen
        // because tables are emitted atomically above).
        if t < tables.len() && i > tables[t].start && i <= tables[t].end {
            i += 1;
            continue;
        }
        push_line(&mut out, &lines[i], &mut prev_line_y);
        i += 1;
    }

    out.trim_end().to_string()
}

// ---------------------------------------------------------------------------
// Stage 4 — human reading order (zones / columns / furniture / block list)
// ---------------------------------------------------------------------------
//
// Deterministic, no ML. On the glyph-layout path we know every visual line's
// device position, so we can decide *in which order a human reads the page*:
//
//   1. column detection — if lines split into >= 2 tall x-bands separated by a
//      real gutter, the page is multi-column and each column is read
//      top-to-bottom before the next column (not row-interleaved);
//   2. furniture removal — short numeric-only lines at the very bottom band
//      (page numbers) and repeated corner/band furniture lines are dropped;
//   3. role labels — title (largest font near the top), heading (larger than
//      body), list item (leading bullet/dash), body — emitted as a structured
//      block list so a frontend can render the page like a document.
//
// Pages with no column structure and no removable furniture produce output
// byte-identical to the plain renderer, so existing single-column documents
// (Fiche etc.) never change.

/// One structured block (reading unit) with a semantic role.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DocBlock {
    /// 1-based page number (filled by the caller).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub page: usize,
    pub kind: String,
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
    pub text: String,
}

fn is_zero(v: &usize) -> bool {
    *v == 0
}

/// Split one visual row at a clearly oversized intra-row gap (a column
/// gutter). Rows without such a gap are single-column rows -> None.
fn split_row_columns(spans: &[Span]) -> Option<(Vec<Span>, Vec<Span>)> {
    if spans.len() < 5 {
        return None;
    }
    let size = spans.iter().map(|s| s.size).fold(0.0f64, f64::max).max(0.1);
    let mut best: Option<(f64, usize)> = None; // (gap, split index)
    for i in 0..spans.len() - 1 {
        let a_end = spans[i].x + spans[i].advance;
        let b_start = spans[i + 1].x;
        let gap = (b_start - a_end).max(0.0);
        // A word space is ~0.25em; a true gutter is much wider. Requiring
        // > 1.2em keeps justified prose rows unsplit.
        if gap > 1.2 * size && best.map_or(true, |(g, _)| gap > g) {
            best = Some((gap, i));
        }
    }
    let (_, at) = best?;
    if at < 2 || at >= spans.len() - 3 {
        return None;
    }
    Some((spans[..=at].to_vec(), spans[at + 1..].to_vec()))
}

/// Detect a genuine two-column page: >= 3 rows split at a *consistent* gutter
/// x. Returns the reading-order column streams (left column top-to-bottom,
/// right column top-to-bottom) when stable, else None (single column).
struct PageColumns {
    top_full: Vec<Vec<Span>>,
    left: Vec<Vec<Span>>,
    right: Vec<Vec<Span>>,
    bottom_full: Vec<Vec<Span>>,
}

/// Detect a genuine two-column page and produce reading-order streams:
/// full-width rows above the column block, left column top-to-bottom, right
/// column top-to-bottom, full-width rows below. Returns None when the page is
/// not convincingly two-column.
fn page_two_columns(lines: &[Vec<Span>]) -> Option<PageColumns> {
    if lines.len() < 3 {
        return None;
    }
    struct Split {
        y: f64,
        left: Vec<Span>,
        right: Vec<Span>,
        gutter_x: f64,
    }
    let mut splits: Vec<Split> = Vec::new();
    for l in lines {
        if let Some((left, right)) = split_row_columns(l) {
            let l_end = left
                .iter()
                .map(|x| x.x + x.advance)
                .fold(f64::NEG_INFINITY, f64::max);
            let r_start = right.iter().map(|x| x.x).fold(f64::INFINITY, f64::min);
            splits.push(Split {
                y: l[0].y,
                left,
                right,
                gutter_x: (l_end + r_start) / 2.0,
            });
        }
    }
    if splits.len() < 3 {
        return None;
    }
    let mut gxs: Vec<f64> = splits.iter().map(|s| s.gutter_x).collect();
    gxs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let med = gxs[gxs.len() / 2];
    let tol = (0.15 * med.abs()).max(6.0);
    let keep: Vec<Split> = splits
        .into_iter()
        .filter(|s| (s.gutter_x - med).abs() <= tol)
        .collect();
    if keep.len() < 3 {
        return None;
    }
    let rows_with_text = lines.iter().filter(|l| l.len() >= 3).count().max(1);
    if keep.len() * 2 < rows_with_text {
        return None;
    }
    let col_top = keep.iter().map(|s| s.y).fold(f64::NEG_INFINITY, f64::max);
    let mut left: Vec<(f64, Vec<Span>)> = Vec::new();
    let mut right: Vec<(f64, Vec<Span>)> = Vec::new();
    let mut top_full: Vec<Vec<Span>> = Vec::new();
    let mut bottom_full: Vec<Vec<Span>> = Vec::new();
    for l in lines {
        let y = l[0].y;
        let y_tol = 0.5 * l[0].size.max(0.1);
        if let Some(sp) = keep.iter().find(|s| (s.y - y).abs() < y_tol) {
            left.push((sp.y, sp.left.clone()));
            right.push((sp.y, sp.right.clone()));
            continue;
        }
        if y > col_top + y_tol {
            top_full.push(l.clone());
        } else {
            bottom_full.push(l.clone());
        }
    }
    top_full.sort_by(|a, b| {
        b[0].y
            .partial_cmp(&a[0].y)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    bottom_full.sort_by(|a, b| {
        b[0].y
            .partial_cmp(&a[0].y)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    // Prose gate: a true text column is made of multi-word lines on both
    // sides where the words are spaced like a sentence. Pipe-table rows have
    // short tokens and wide cell gutters inside the "line", so they must keep
    // flowing through the table detector.
    let wc = |rows: &[(f64, Vec<Span>)]| -> f64 {
        if rows.is_empty() {
            return 0.0;
        }
        rows.iter()
            .map(|(_, v)| v.iter().filter(|sp| !sp.text.trim().is_empty()).count() as f64)
            .sum::<f64>()
            / rows.len() as f64
    };
    if wc(&left) < 2.5 || wc(&right) < 2.5 {
        return None;
    }
    // No half may contain a column-wide gutter inside it (that would mean the
    // "column" still holds multiple table cells).
    let clean = |rows: &[(f64, Vec<Span>)]| -> bool {
        rows.iter().all(|(_, v)| {
            let size = v.iter().map(|x| x.size).fold(0.0f64, f64::max).max(0.1);
            v.windows(2).all(|p| {
                let a_end = p[0].x + p[0].advance;
                let gap = (p[1].x - a_end).max(0.0);
                gap <= 1.2 * size
            })
        })
    };
    if !clean(&left) || !clean(&right) {
        return None;
    }
    left.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    right.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    Some(PageColumns {
        top_full,
        left: left.into_iter().map(|(_, v)| v).collect(),
        right: right.into_iter().map(|(_, v)| v).collect(),
        bottom_full,
    })
}

/// Human reading order for the page as streams of visual lines: single-column
/// pages produce one stream (top-down); two-column pages produce full-width
/// header rows, left column, right column, footer rows.
fn page_read_order(lines: &[Vec<Span>]) -> Vec<Vec<Vec<Span>>> {
    if let Some(pc) = page_two_columns(lines) {
        let mut streams = Vec::new();
        if !pc.top_full.is_empty() {
            streams.push(pc.top_full);
        }
        streams.push(pc.left);
        streams.push(pc.right);
        if !pc.bottom_full.is_empty() {
            streams.push(pc.bottom_full);
        }
        return streams;
    }
    vec![lines.to_vec()]
}

/// Decide whether a visual line is a page-number footer (numeric-only, in the
/// bottom band of the page).
fn is_page_number_line(spans: &[Span], page_height: f64) -> bool {
    let y0 = spans.iter().map(|s| s.y).fold(f64::INFINITY, f64::min);
    let text: String = spans.iter().map(|s| s.text.as_str()).collect();
    let t = text.trim();
    if t.is_empty() {
        return false;
    }
    if y0 < page_height * 0.055 {
        let all_num = t
            .chars()
            .all(|c| c.is_ascii_digit() || c.is_whitespace() || c == '/' || c == '-' || c == '.');
        return all_num && t.len() <= 12;
    }
    false
}

/// Human reading order for the page, expressed as streams of visual lines.
/// Single-column pages produce one stream in top-to-bottom order; two-column
/// pages produce two streams (left column then right column, each top-down).

/// Render a single visual line to text (no surrounding blank-line logic).
fn render_line_text(spans: &[Span]) -> String {
    let size = spans.iter().map(|s| s.size).fold(0.0f64, f64::max).max(0.1);
    let mut out = String::new();
    let mut prev_x: Option<f64> = None;
    let mut prev_advance = 0.0f64;
    for span in spans {
        if span.text.is_empty() {
            continue;
        }
        if let Some(px) = prev_x {
            let gap = span.x - px;
            let space_adv = 0.25 * size;
            if span.text != " " {
                if gap > 2.5 * size {
                    if !out.is_empty() && !out.ends_with('\n') {
                        out.push('\n');
                    }
                } else if gap - prev_advance > 0.35 * space_adv {
                    if !out.is_empty() && !out.ends_with(' ') && !out.ends_with('\n') {
                        out.push(' ');
                    }
                }
            }
        }
        if span.text == " " {
            if !out.is_empty() && !out.ends_with(' ') && !out.ends_with('\n') {
                out.push(' ');
            }
        } else {
            out.push_str(&span.text);
        }
        prev_x = Some(span.x);
        prev_advance = span.advance;
    }
    out.trim_end().to_string()
}

/// Render page text in human reading order. When the page is a single column
/// and nothing was removed, output equals `render_cluster` byte-for-byte.
fn render_human_order(lines: &[Vec<Span>], page_height: f64, drop_furniture: bool) -> String {
    let streams = page_read_order(lines);
    if streams.len() == 1 {
        // Single column: identical to the plain renderer unless we strip
        // furniture lines (page numbers).
        if !drop_furniture {
            return render_cluster(lines);
        }
        let keep: Vec<Vec<Span>> = lines
            .iter()
            .filter(|l| !is_page_number_line(l, page_height))
            .cloned()
            .collect();
        return render_cluster(&keep);
    }
    let mut out = String::new();
    for (ci, stream) in streams.iter().enumerate() {
        if ci > 0 && !out.is_empty() {
            out.push('\n');
        }
        let mut prev_y: Option<f64> = None;
        for line in stream {
            if drop_furniture && is_page_number_line(line, page_height) {
                continue;
            }
            let size = line[0].size.max(0.1);
            if let Some(py) = prev_y {
                if py - line[0].y > 2.0 * size {
                    out.push('\n');
                }
            }
            out.push_str(&render_line_text(line));
            out.push('\n');
            prev_y = Some(line[0].y);
        }
    }
    out.trim_end().to_string()
}

/// Build the structured block list for a glyph page in reading order.
fn build_doc_blocks(lines: &[Vec<Span>], page_height: f64) -> Vec<DocBlock> {
    let mut sizes: Vec<f64> = lines
        .iter()
        .map(|l| l.iter().map(|s| s.size).fold(0.0f64, f64::max))
        .filter(|s| *s > 0.0)
        .collect();
    sizes.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let body = sizes.get(sizes.len() / 2).copied().unwrap_or(10.0).max(1.0);
    let title_size = body * 1.6;
    let max_y = lines
        .iter()
        .map(|l| l[0].y)
        .fold(f64::NEG_INFINITY, f64::max);

    let mut blocks: Vec<DocBlock> = Vec::new();
    for stream in page_read_order(lines) {
        for line in stream {
            if is_page_number_line(&line, page_height) {
                continue;
            }
            let size = line.iter().map(|s| s.size).fold(0.0f64, f64::max);
            let x0 = line.iter().map(|s| s.x).fold(f64::INFINITY, f64::min);
            let x1 = line
                .iter()
                .map(|s| s.x + s.advance)
                .fold(f64::NEG_INFINITY, f64::max);
            let y0 = line.iter().map(|s| s.y).fold(f64::INFINITY, f64::min);
            let y1 = line.iter().map(|s| s.y).fold(f64::NEG_INFINITY, f64::max);
            let text = render_line_text(&line);
            let kind = if size >= title_size && y0 >= max_y - 2.0 {
                "title"
            } else if size >= body * 1.25 {
                "heading"
            } else if text
                .trim_start()
                .starts_with(['-', '•', '●', '◦', '·', '*'])
            {
                "list"
            } else {
                "body"
            };
            blocks.push(DocBlock {
                page: 0,
                kind: kind.to_string(),
                x0,
                y0,
                x1,
                y1,
                text,
            });
        }
    }
    blocks
}

#[cfg(test)]
mod table_detection_tests {
    use super::*;

    /// Build one visual line from (start_x, text) word spans on a shared
    /// baseline `y` (page coordinates: larger y = higher on the page).
    fn row(y: f64, words: &[(f64, &str)]) -> Vec<Span> {
        words
            .iter()
            .map(|(x, t)| Span {
                text: t.to_string(),
                x: *x,
                y,
                size: 10.0,
                advance: 0.0,
            })
            .collect()
    }

    fn page(rows: Vec<Vec<Span>>) -> Vec<Vec<Span>> {
        rows
    }

    #[test]
    fn aligned_two_column_grid_is_detected() {
        let lines = page(vec![
            row(700.0, &[(50.0, "Nom"), (300.0, "DUPONT")]),
            row(690.0, &[(50.0, "Prenom"), (300.0, "Marie")]),
            row(680.0, &[(50.0, "Date"), (300.0, "1985")]),
            row(670.0, &[(50.0, "Pays"), (300.0, "France")]),
        ]);
        let hits = find_tables(&lines);
        assert_eq!(hits.len(), 1, "aligned 2-col grid must be recovered");
        let rows: Vec<Vec<String>> = hits[0].rows.clone();
        assert_eq!(rows.len(), 4, "4 rows expected: {rows:?}");
        assert_eq!(rows[0][0].as_str(), "Nom");
        assert_eq!(rows[0][1].as_str(), "DUPONT");
    }

    #[test]
    fn toc_dot_leader_rows_are_rejected() {
        // Real TOC rows: section number, dot leaders, page number. The dots
        // align perfectly but the grid is an index, not a data table.
        let lines = page(vec![
            row(700.0, &[(50.0, "1.1"), (300.0, "....."), (420.0, "5")]),
            row(690.0, &[(50.0, "1.2"), (300.0, "....."), (420.0, "6")]),
            row(680.0, &[(50.0, "2.1"), (300.0, "....."), (420.0, "9")]),
            row(670.0, &[(50.0, "2.2"), (300.0, "....."), (420.0, "12")]),
        ]);
        assert!(
            find_tables(&lines).is_empty(),
            "TOC dot leaders must not be tabled"
        );
        assert!(
            find_gap_tables(&lines, &[]).is_empty(),
            "TOC dot leaders must not be tabled (3b)"
        );
    }

    #[test]
    fn aligned_long_prose_columns_are_rejected() {
        // Body prose whose two ragged columns happen to start at the same x on
        // every line must not become a table: no column holds short codes.
        let lines = page(vec![
            row(
                700.0,
                &[
                    (50.0, "The quick brown fox jumps over the lazy dog"),
                    (300.0, "second column of equally long words lives"),
                ],
            ),
            row(
                690.0,
                &[
                    (50.0, "Another quite long sentence to fill the first column"),
                    (300.0, "and the right hand column keeps on flowing too"),
                ],
            ),
            row(
                680.0,
                &[
                    (50.0, "A third verbose paragraph placed under the two above"),
                    (300.0, "with prose that keeps reading like a document"),
                ],
            ),
        ]);
        assert!(
            find_tables(&lines).is_empty(),
            "aligned prose must not be tabled"
        );
    }

    #[test]
    fn bullet_marker_column_is_rejected() {
        // Radio/bullet option lists: a constant short first column ("o"/"-")
        // is a marker, not data.
        let lines = page(vec![
            row(700.0, &[(50.0, "-"), (80.0, "option one")]),
            row(690.0, &[(50.0, "-"), (80.0, "option two")]),
            row(680.0, &[(50.0, "-"), (80.0, "option three")]),
            row(670.0, &[(50.0, "-"), (80.0, "option four")]),
        ]);
        assert!(
            find_tables(&lines).is_empty(),
            "bullet marker column must not be tabled"
        );
    }

    #[test]
    fn jittered_grid_is_recovered_by_stage3b_only() {
        // A genuine grid whose second column start jitters by < 1 pt between
        // rows: the strict pass misses it, the wider Stage-3b tolerance wins.
        let lines = page(vec![
            row(700.0, &[(50.0, "A1"), (300.0, "B1")]),
            row(690.0, &[(50.0, "A2"), (300.8, "B2")]),
            row(680.0, &[(50.0, "A3"), (299.6, "B3")]),
            row(670.0, &[(50.0, "A4"), (300.4, "B4")]),
        ]);
        assert!(
            find_tables(&lines).is_empty(),
            "strict pass must miss the jittered grid"
        );
        let gap = find_gap_tables(&lines, &[]);
        assert_eq!(gap.len(), 1, "Stage-3b must recover the jittered grid");
        let rows: Vec<Vec<String>> = gap[0].rows.clone();
        assert!(rows[0].iter().any(|c| c == "B1"), "cells wrong: {rows:?}");
    }
}
