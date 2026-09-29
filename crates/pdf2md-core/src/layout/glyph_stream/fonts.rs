// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! PDF content-stream glyph extraction, font metric resolution, and 2D span aggregation.

use super::*;

/// Reads a TrueType/OpenType `sfnt` font program and reports whether its real
/// weight is bold, or `None` when the program is missing or not a parseable
/// sfnt (e.g. a bare-CFF `/FontFile3` or a Type1 `/FontFile`).
///
/// The PDF `/StemV` hint is producer-controlled and is routinely wrong: the
/// ZUGFeRD/intarsys stylesheet writes the *same* `/StemV 600` (and `777`) on
/// both its Regular and Bold subsets, so a stem-width threshold alone marks
/// every run bold. The embedded `OS/2`/`head` tables carry the face's actual
/// weight and are authoritative when they are present.
pub(super) fn font_program_is_bold(data: &[u8]) -> Option<bool> {
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
pub(super) fn embedded_font_bold(doc: &Document, fd: &Dictionary) -> Option<bool> {
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
pub(super) fn fold_ligatures(s: &mut String) {
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
/// `hz` is the text state's horizontal scaling (`Tz / 100`): it scales every
/// horizontal displacement (glyph width, `Tc`/`Tw`, `TJ` kerns) but not the
/// glyph height, so it must reach the run's right edge as well as the text
/// matrix the shell walker advances.
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
    hz: f64,
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
    // `Tz` scales the horizontal glyph displacement (PDF 32000-1 §9.3.3); the
    // shell walker already folds it into the text matrix it advances, so without
    // it here `span.advance` overshoots the run's rendered right edge by
    // `1 / hz`. On the Courier/Franklin forms that set `Tz` ~50 these runs then
    // swallowed the following word space and the column gutter whole.
    let hx = hscale * hz;
    // A Type3 font's `Tf` operand is a size in the font's own glyph space, not
    // an em: the font's `/FontMatrix` and `/FontBBox` give the real one, which
    // travels on the width table. Every other font leaves this 1.0.
    let size = tfs * hscale * width.em_scale();
    let (ux, uy) = tm.apply(em_offset / 1000.0 * tfs * hz, 0.0);
    let (x, y) = ctm.apply(ux, uy);
    let advance = width
        .width(bytes)
        .map(|w| w / 1000.0 * tfs * hx)
        .unwrap_or_else(|| {
            // No metrics: assume a typical letter advance (~0.5 em) so the
            // gap detector still separates words reasonably.
            0.5 * size * hz
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
    let word_advance = advance + (tc * nchars + tw * nspaces) * hx;
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
