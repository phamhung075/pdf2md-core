// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! PDF content-stream glyph extraction, font metric resolution, and 2D span aggregation.

use super::*;

/// Resolve every collected `/Font` into a [`GlyphFontInfo`].
pub(super) fn build_glyph_font_table<'a>(
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
pub(super) fn glyph_font_table_for_chain<'a>(
    doc: &'a Document,
    chain: &[&'a Dictionary],
) -> Vec<GlyphFontInfo> {
    let mut fonts: std::collections::BTreeMap<Vec<u8>, &Dictionary> =
        std::collections::BTreeMap::new();
    crate::text_extract::collect_fonts(doc, chain, &mut fonts);
    build_glyph_font_table(doc, &fonts)
}

impl GlyphBudget {
    pub(super) fn new() -> Self {
        GlyphBudget {
            do_left: MAX_FORM_DO_PER_PAGE,
            ops_left: MAX_FORM_OPS_TOTAL,
            bytes_left: MAX_FORM_BYTES_TOTAL,
            spans_left: MAX_GLYPH_SPANS_PER_PAGE,
            exhausted: false,
        }
    }
}

/// The device-space bounding rectangle of a form's `/BBox` under `fctm`, used
/// to drop spans the form paints outside its own clip (the spec makes `/BBox`
/// clip the form's content). `None` when the form has no usable `/BBox`.
pub(super) fn form_device_bbox(doc: &Document, form: &Dictionary, fctm: &Mtx) -> Option<(f64, f64, f64, f64)> {
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

/// Recursively walk one content stream (page or Form XObject) into positioned
/// spans, expanding `/Do` forms with their own `/Matrix`, `/Resources` font
/// table and `/BBox`. Depth, `Do` count, decoded bytes and total operators are
/// shared across the page (see [`GlyphBudget`]).
#[allow(clippy::too_many_arguments)]
pub(super) fn walk_glyphs(
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
                    if budget.spans_left == 0 {
                        budget.exhausted = true;
                    } else {
                        let before = spans.len();
                        if let Some(dir) = push_span(
                            codec, width, bytes, 0.0, &tm, &ctm, tfs, tc, tw, style, &mut spans,
                        ) {
                            let n = spans.last().map(|s| s.text.chars().count()).unwrap_or(0);
                            if dir > 0 {
                                vertical_up_chars += n;
                            } else {
                                vertical_down_chars += n;
                            }
                        }
                        if spans.len() > before {
                            budget.spans_left -= 1;
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
                            if budget.spans_left == 0 {
                                budget.exhausted = true;
                            } else {
                                let before = spans.len();
                                if let Some(dir) = push_span(
                                    codec, width, bytes, offset, &tm, &ctm, tfs, tc, tw, style,
                                    &mut spans,
                                ) {
                                    let n =
                                        spans.last().map(|s| s.text.chars().count()).unwrap_or(0);
                                    if dir > 0 {
                                        vertical_up_chars += n;
                                    } else {
                                        vertical_down_chars += n;
                                    }
                                }
                                if spans.len() > before {
                                    budget.spans_left -= 1;
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
