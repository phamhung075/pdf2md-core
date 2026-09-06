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
/// When `detect_tables` is false, returns the plain reading-order text with
/// no table recovery (byte-identical to the table-less renderer).
pub fn extract_page_glyphs(
    doc: &Document,
    page_id: ObjectId,
    detect_tables: bool,
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

    let lines = build_lines(&spans);
    if !detect_tables {
        return Ok(PageText {
            text: render_cluster(&lines),
            text_ops_seen,
            has_fonts,
            tables: 0,
        });
    }
    let hits = find_tables(&lines);
    let text = if hits.is_empty() {
        // Byte-identical to the plain text renderer when no table is found.
        render_cluster(&lines)
    } else {
        render_with_tables(&lines, &hits)
    };

    Ok(PageText {
        text,
        text_ops_seen,
        has_fonts,
        tables: hits.len(),
    })
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
fn find_tables(lines: &[Vec<Span>]) -> Vec<TableHit> {
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

    // Alignment tolerance: same absolute column start on every row.
    let tol = info
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
            let col = bounds
                .iter()
                .position(|&b| w.x0 < b)
                .unwrap_or(ncol - 1);
            cells[col].push(w.text.clone());
        }
        cells.into_iter().map(|c| c.join(" ")).collect()
    }

    // Vertical bands of consecutive rows that could sit in one grid (>= 2
    // words, tight line pitch, no paragraph gap inside).
    let mut bands: Vec<Vec<usize>> = Vec::new();
    for (i, r) in info.iter().enumerate() {
        if r.words.len() < 2 {
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
                let max_size = win_rows.iter().map(|&i| info[i].size).fold(0.0f64, f64::max);
                let ok_gutter = rulers.windows(2).all(|p| p[1] - p[0] > 0.6 * max_size);
                if ok_gutter {
                    // The first cell (left of the first ruler midpoint) must
                    // vary across rows: a constant marker means a list.
                    let mid1 = (rulers[0] + rulers[1]) / 2.0;
                    let mut distinct: std::collections::HashSet<String> = std::collections::HashSet::new();
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
                        let table_rows: Vec<Vec<String>> =
                            win_rows.iter().map(|&i| bucket(&info, i, &rulers)).collect();
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
                            let rows2: Vec<Vec<String>> = table_rows
                                .iter()
                                .map(|r| r[c0..c1].to_vec())
                                .collect();
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
                        if !line_text.is_empty() && !line_text.ends_with(' ') && !line_text.ends_with('\n') {
                            line_text.push(' ');
                        }
                    }
                }
            }
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
