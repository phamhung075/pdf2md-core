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
    pub is_underline: bool,
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
        // `/FontWeight` is rarely present in embedded subsets. `/StemV` (the
        // vertical stem width in 1/1000 em) is the practical bold signal in the
        // wild: roman faces sit near 70–90, bold faces ~120+.
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
        is_underline: false,
        is_vertical,
    });
}

/// Height of the page media box in device points (best effort), accounting for page rotation.
pub fn page_height_of(doc: &Document, page_id: ObjectId) -> Option<f64> {
    let (_, h) = page_initial_transform(doc, page_id);
    Some(h)
}

/// Initial coordinate transformation matrix and display height for a page,
/// taking into account the page /Rotate attribute (0, 90, 180, 270 degrees clockwise)
/// and /MediaBox / /CropBox boundaries.
pub(crate) fn page_initial_transform(doc: &Document, page_id: ObjectId) -> (Mtx, f64) {
    let dict = match doc.get_dictionary(page_id) {
        Ok(d) => d,
        Err(_) => return (Mtx::ID, 842.0),
    };
    let rotate = dict
        .get(b"Rotate")
        .ok()
        .and_then(|o| deref(doc, o))
        .and_then(|o| o.as_i64().ok())
        .unwrap_or(0);
    let rotate = ((rotate % 360) + 360) % 360;

    let box_obj = dict
        .get(b"CropBox")
        .ok()
        .or_else(|| dict.get(b"MediaBox").ok())
        .and_then(|o| deref(doc, o))
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
            let inside = b.x0 >= tx0 && b.x1 <= tx1 && b.y0 >= ty0 && b.y1 <= ty1;
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

    let (init_ctm, page_height) = page_initial_transform(doc, page_id);
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
    let mut text_ops_seen = false;
    let mut spans: Vec<Span> = Vec::new();

    // Underline detection: PDF fonts carry no underline bit, so an underline is
    // a thin horizontal stroke (or an equally thin filled rectangle) painted
    // just below a run's baseline. We collect those candidate "underline
    // segments" (device y, x0, x1) while walking the content stream, then match
    // them to spans after the visual lines are built.
    let mut underline_segs: Vec<(f64, f64, f64)> = Vec::new();
    let mut path_pts: Vec<(f64, f64)> = Vec::new();
    let mut path_start: Option<(f64, f64)> = None;

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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn test_faux_bold_overstrike_deduplication() {
        let spans = vec![
            Span {
                text: "B".into(),
                x: 100.0,
                y: 200.0,
                size: 12.0,
                advance: 8.0,
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
        let mut path = vec![(100.0, 200.0), (300.0, 200.0), (300.0, 300.0)];
        let mut segs: Vec<(f64, f64, f64)> = Vec::new();
        flush_path_segs(&mut path, Some((100.0, 200.0)), &mut segs, true);
        assert_eq!(segs, vec![(200.0, 100.0, 300.0)], "only the horizontal rule survives");
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
}
