// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! PDF content-stream glyph extraction, font metric resolution, and 2D span aggregation.

use super::*;

impl Widths {
    /// Advance width for a char code, in 1/1000 em units.
    pub(crate) fn width(&self, bytes: &[u8]) -> Option<f64> {
        match self {
            Widths::Byte(t) | Widths::ByteType3 { table: t, .. } => {
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
                for_each_cid(encoding, bytes, |cid| {
                    let w = map.get(&cid).copied().unwrap_or(*default);
                    if w > 0.0 {
                        any = true;
                        total += w;
                    }
                });
                if any {
                    Some(total)
                } else {
                    None
                }
            }
            Widths::None => None,
        }
    }

    /// Number of character *codes* in `bytes` under this font's code width,
    /// independent of the advance values. This is the same code segmentation
    /// [`Self::width`] uses (custom `/Encoding` CMap lengths, else 2-byte
    /// Identity-H/V chunks; one byte for a simple font), exposed so the
    /// undecodable-font accounting counts glyphs — not raw bytes — for a font
    /// whose encoding could not be resolved into a [`Codec`].
    ///
    /// [`Codec`]: crate::text_extract::Codec
    pub(crate) fn code_count(&self, bytes: &[u8]) -> usize {
        match self {
            Widths::Byte(_) | Widths::ByteType3 { .. } => bytes.len(),
            Widths::Cid { encoding, .. } => {
                let mut codes = 0usize;
                for_each_cid(encoding, bytes, |_| codes += 1);
                codes
            }
            // Unusable metrics (`/W` or `/DescendantFonts` missing): the callers
            // fall back to the font subtype's fixed code width.
            Widths::None => bytes.len(),
        }
    }

    /// Scale from the content stream's `Tf` size operand to real points for
    /// this font. It is `1.0` for every glyph space that already uses the
    /// standard 1/1000-em convention; a Type3 font carries the scale derived
    /// from its `/FontMatrix` and `/FontBBox` (see `type3_em_scale`).
    pub(crate) fn em_scale(&self) -> f64 {
        match self {
            Widths::ByteType3 { em_scale, .. } => *em_scale,
            _ => 1.0,
        }
    }
}

/// Consume every code in `bytes` under a CID font's code width, visiting the
/// code's CID. Single source of truth for the CID code segmentation shared by
/// [`Widths::width`] and [`Widths::code_count`]: a custom `/Encoding` CMap
/// consumes the code lengths it knows and skips an unmatched byte, while
/// Identity-H/V (no CMap stream) reads two-byte codes.
fn for_each_cid(encoding: &Option<CMapCodec>, bytes: &[u8], mut visit: impl FnMut(u32)) {
    match encoding {
        Some(cm) => {
            let mut i = 0usize;
            while i < bytes.len() {
                let mut matched = false;
                for len in 1u8..=4u8 {
                    let n = len as usize;
                    if i + n > bytes.len() {
                        continue;
                    }
                    if let Some(cid) = cm.lookup(&bytes[i..i + n]) {
                        visit(cid);
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
            for chunk in bytes.chunks(2) {
                let mut cid = 0u32;
                for &b in chunk {
                    cid = (cid << 8) | b as u32;
                }
                visit(cid);
            }
        }
    }
}

/// Vertical (and horizontal) scale of the standard PDF glyph-to-text matrix
/// `[0.001 0 0 0.001 0 0]`: glyph space is 1/1000 em, the convention every
/// non-Type3 font uses implicitly.
pub(super) const STANDARD_GLYPH_SCALE: f64 = 0.001;

/// Relative tolerance for matching a matrix scale against
/// [`STANDARD_GLYPH_SCALE`]; `1e-6` admits float noise but no real producer.
const STANDARD_GLYPH_SCALE_TOLERANCE: f64 = 1e-6;

/// The `/FontMatrix` of `font` as `[a b c d e f]`.
///
/// It maps a Type3 font's own glyph space to text space (PDF 32000-1 §9.6.5).
/// Everything absent or malformed falls back to the standard 1/1000 scale that
/// every other font type uses implicitly.
pub(super) fn font_matrix(doc: &Document, font: &Dictionary) -> [f64; 6] {
    const STANDARD_1_1000: [f64; 6] = [
        STANDARD_GLYPH_SCALE,
        0.0,
        0.0,
        STANDARD_GLYPH_SCALE,
        0.0,
        0.0,
    ];
    let Some(arr) = font
        .get(b"FontMatrix")
        .ok()
        .and_then(|o| deref(doc, o))
        .and_then(|o| o.as_array().ok())
    else {
        return STANDARD_1_1000;
    };
    let mut m = STANDARD_1_1000;
    for (slot, item) in m.iter_mut().zip(arr) {
        if let Some(v) = deref(doc, item).and_then(num) {
            if v.is_finite() {
                *slot = v;
            }
        }
    }
    m
}

/// Horizontal scale of a glyph-to-text matrix: the factor a glyph-space
/// distance along the text x-axis is multiplied by. That is `FontMatrix[0]` for
/// an unrotated matrix; a quarter-turn rotation moves the advance onto
/// `FontMatrix[1]`, so that is the fallback when the primary component is zero.
fn horizontal_scale(m: &[f64; 6]) -> f64 {
    if m[0].abs() > f64::EPSILON {
        m[0].abs()
    } else {
        m[1].abs()
    }
}

/// Vertical scale of a glyph-to-text matrix, the counterpart of
/// [`horizontal_scale`] on `FontMatrix[3]`/`FontMatrix[2]`.
fn vertical_scale(m: &[f64; 6]) -> f64 {
    if m[3].abs() > f64::EPSILON {
        m[3].abs()
    } else {
        m[2].abs()
    }
}

/// Typical height of a font's `/FontBBox`, in ems. The box spans the face's
/// ascender to descender, which for the common text faces is a little over one
/// em: Helvetica 1.156, Times-Roman 1.116, Courier 1.055, and more in foundries
/// that bake vertical padding into the box. `1.2` sits at the practical middle
/// of that range: on the usual `[.24 …]` Type3 statement font it turns the
/// producer's explicit word offsets into the canonical ~0.25 em space, and it
/// keeps a distinct, larger section heading from being read as a table row.
/// Dividing a Type3 box height by this constant returns the em its glyphs are
/// visually set at.
const TYPICAL_FONT_BBOX_EM: f64 = 1.2;

/// Real em of a Type3 font, as a multiple of its `Tf` size operand.
///
/// A Type3 glyph is described in the font's own glyph space; `/FontMatrix` maps
/// that space to text space, so `Tf` is not an em until it is multiplied by the
/// matrix's vertical scale. The `/FontBBox` height then gives the glyphs' full
/// vertical extent, which for a typical face is [`TYPICAL_FONT_BBOX_EM`] ems.
/// On the common `[.24 0 0 .24 …]` bank-statement fonts this turns a `Tf 1`
/// operand into the ~9 pt the text visually is, instead of the 1 pt a bare
/// `Tf` implies. A matrix whose vertical scale is already the standard
/// [`STANDARD_GLYPH_SCALE`] (TeX bitmap fonts and other Type3 faces that keep
/// glyph space at 1/1000 em) needs no rescaling: `Tf` already is the em, so
/// the scale there is exactly 1.0 and `/Widths` are used as-is. A missing box
/// or degenerate matrix keeps the unscaled `Tf` (scale 1.0) rather than guess.
pub(super) fn type3_em_scale(doc: &Document, font: &Dictionary) -> f64 {
    let v = vertical_scale(&font_matrix(doc, font));
    if !(v > 0.0) {
        return 1.0;
    }
    // Standard 1/1000 glyph space: the `Tf` operand is already the em.
    if (v - STANDARD_GLYPH_SCALE).abs()
        <= STANDARD_GLYPH_SCALE_TOLERANCE * STANDARD_GLYPH_SCALE
    {
        return 1.0;
    }
    let Some(bbox) = font
        .get(b"FontBBox")
        .ok()
        .and_then(|o| deref(doc, o))
        .and_then(|o| o.as_array().ok())
    else {
        return 1.0;
    };
    let coord = |i: usize| bbox.get(i).and_then(|o| deref(doc, o)).and_then(num);
    let (Some(y0), Some(y1)) = (coord(1), coord(3)) else {
        return 1.0;
    };
    let height = (y1 - y0).abs();
    if !height.is_finite() || height <= 0.0 {
        return 1.0;
    }
    v * height / TYPICAL_FONT_BBOX_EM
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
        let is_type3 = subtype == b"Type3";
        // Type3 `/Widths` are in the font's own glyph space, not 1/1000 em:
        // `/FontMatrix[0]` maps them to text space (PDF 32000-1 §9.6.5). Fold
        // the mapping into the table here so every width consumer keeps
        // working in the engine's one 1/1000-em convention. The standard
        // 0.001 matrix leaves a non-Type3 font's `/Widths` untouched.
        let width_scale = if is_type3 {
            horizontal_scale(&font_matrix(doc, font)) * 1000.0
        } else {
            1.0
        };
        let first = font
            .get(b"FirstChar")
            .ok()
            .and_then(|o| deref(doc, o))
            .and_then(num)
            .unwrap_or(0.0) as i32;
        let Some(w) = font.get(b"Widths").ok().and_then(|o| deref(doc, o)) else {
            // `/Widths` is required for a Type3 font; without it its glyph
            // space has no base-14 equivalent, so there are no usable metrics.
            if is_type3 {
                return Widths::None;
            }
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
                t[code as usize] = num(item).unwrap_or(0.0) * width_scale;
            }
        }
        if is_type3 {
            Widths::ByteType3 {
                table: t,
                em_scale: type3_em_scale(doc, font),
            }
        } else {
            Widths::Byte(t)
        }
    }
}

// ---------------------------------------------------------------------------
// Glyph spans + clustering
// ---------------------------------------------------------------------------
