// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

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

use super::*;

impl WalkerBudget {
    pub(crate) fn new() -> Self {
        WalkerBudget {
            do_left: MAX_WALKER_DO_PER_PAGE,
            ops_left: MAX_WALKER_OPS_TOTAL,
            bytes_left: MAX_WALKER_BYTES_TOTAL,
            exhausted: false,
        }
    }
}

/// Walk a content stream (a page or a Form XObject) appending decoded text to
/// `out`. Fonts come from `chain`; `Do` recurses into Form XObjects so text
/// buried there — a common payslip/invoice generator shape — is not dropped.
/// Recursion is depth-, `Do`-, operator- and byte-bounded (shared per-page
/// [`WalkerBudget`]) and never revisits a form already on the current path, so
/// neither a self-referential form nor an exponentially expanding DAG can loop
/// or multiply work.
#[allow(clippy::too_many_arguments)]
pub(super) fn walk_content(
    doc: &Document,
    chain: &[&Dictionary],
    ops: &[Operation],
    out: &mut String,
    text_ops_seen: &mut bool,
    form_path: &mut Vec<ObjectId>,
    depth: usize,
    budget: &mut WalkerBudget,
) {
    let mut fonts: BTreeMap<Vec<u8>, &Dictionary> = BTreeMap::new();
    collect_fonts(doc, chain, &mut fonts);
    let codecs: Vec<(Vec<u8>, Codec)> = fonts
        .iter()
        .filter_map(|(name, fd)| resolve_codec(doc, fd).map(|c| (name.clone(), c)))
        .collect();
    // Width tables for the same fonts, used only by the word-gap heuristic.
    let widths: Vec<(Vec<u8>, Widths)> = fonts
        .iter()
        .map(|(name, fd)| (name.clone(), resolve_widths(doc, fd)))
        .collect();

    let mut cur: Option<usize> = None;
    let mut cur_width: Option<usize> = None;
    let mut tp = TextPos::new();
    // `q`/`Q` save and restore the graphics state, which includes the text
    // state (`Tf` font/size and `Tc`/`Tw`/`Tz`/`TL`). Without this, a `Q` after
    // a table cell or form left the previous cell's font selected for the text
    // that follows. Stack depth is bounded so a malformed stream cannot grow it
    // without limit.
    let mut gstate: Vec<(Option<usize>, Option<usize>, TextPos)> = Vec::new();

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
    let has_tj_array = ops.iter().any(|op| op.operator == "TJ");
    let has_abs_pos = ops
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

    for op in ops {
        if budget.ops_left == 0 {
            budget.exhausted = true;
            break;
        }
        budget.ops_left -= 1;
        match op.operator.as_str() {
            "q" => {
                if gstate.len() < 64 {
                    gstate.push((cur, cur_width, tp));
                }
            }
            "Q" => {
                if let Some((c, cw, t)) = gstate.pop() {
                    cur = c;
                    cur_width = cw;
                    tp = t;
                }
            }
            "Tf" => {
                let name = op.operands.first().and_then(|o| o.as_name().ok());
                cur = name.and_then(|nm| codecs.iter().position(|(n, _)| n == nm));
                cur_width = name.and_then(|nm| widths.iter().position(|(n, _)| n == nm));
                if let Some(sz) = op.operands.get(1).and_then(|o| o.as_float().ok()) {
                    tp.size = sz as f64;
                }
            }
            "Tm" => {
                cur_pos = pos2(op, 4);
                // Track the text-space origin for the walker gap heuristic. A
                // scaled/rotated matrix puts text space and device points in
                // different units, so gap inference is disabled for the rest of
                // this stream rather than risk a wrong separator.
                if let Some((x, y)) = pos2(op, 4) {
                    let a = op.operands.first().and_then(|o| o.as_float().ok());
                    let b = op.operands.get(1).and_then(|o| o.as_float().ok());
                    let c = op.operands.get(2).and_then(|o| o.as_float().ok());
                    let d = op.operands.get(3).and_then(|o| o.as_float().ok());
                    let identity = matches!((a, b, c, d),
                        (Some(a), Some(b), Some(c), Some(d))
                            if (a - 1.0).abs() <= 1e-3
                                && b.abs() <= 1e-3
                                && c.abs() <= 1e-3
                                && (d.abs() - 1.0).abs() <= 1e-3);
                    if identity {
                        tp.line_x = x;
                        tp.line_y = y;
                        tp.goto_line(false);
                    } else {
                        tp.unusable = true;
                    }
                }
            }
            "TD" => {
                // `TD` is `Td` plus a leading update. In the string-walker
                // case a vertical move is a line advance even when the
                // producer emits no `ET`/`T*` between the lines (see `Td`).
                if !pos_mode && td_advances_line(op) {
                    break_line(out);
                }
                cur_pos = pos2(op, 0);
                if let Some((tx, ty)) = pos2(op, 0) {
                    let same_line = !td_advances_line(op);
                    tp.line_x += tx;
                    tp.line_y += ty;
                    tp.leading = -ty;
                    tp.goto_line(same_line);
                }
            }
            "Td" => {
                // `Td` translates the text line. Producers that draw each line
                // with `Tj` + `0 -N Td` inside a single `BT`/`ET` (instead of
                // an `ET`/`T*` per line) previously had every visual line
                // fused into one run: `synth_ticket_compressed.pdf` emitted a
                // single paragraph with doubled spaces between the four
                // source lines.
                if !pos_mode && td_advances_line(op) {
                    break_line(out);
                }
                if let Some((tx, ty)) = pos2(op, 0) {
                    let same_line = !td_advances_line(op);
                    tp.line_x += tx;
                    tp.line_y += ty;
                    tp.goto_line(same_line);
                }
            }
            "BT" => {
                // `BT` resets the text matrix (not the rest of the text
                // state), so the next line starts at the origin until `Tm`/`Td`
                // places it.
                tp.line_x = 0.0;
                tp.line_y = 0.0;
                tp.goto_line(false);
            }
            "TL" => {
                if let Some(l) = op.operands.first().and_then(|o| o.as_float().ok()) {
                    tp.leading = l as f64;
                }
            }
            "Tz" => {
                if let Some(v) = op.operands.first().and_then(|o| o.as_float().ok()) {
                    tp.hscale = v as f64 / 100.0;
                }
            }
            "Tc" => {
                if let Some(v) = op.operands.first().and_then(|o| o.as_float().ok()) {
                    tp.char_sp = v as f64;
                }
            }
            "Tw" => {
                if let Some(v) = op.operands.first().and_then(|o| o.as_float().ok()) {
                    tp.word_sp = v as f64;
                }
            }
            "Tj" | "TJ" => {
                *text_ops_seen = true;
                if let Some(ci) = cur {
                    if pos_mode {
                        show_pos(
                            out,
                            &codecs[ci].1,
                            &op.operands,
                            pos_mode,
                            line_eps,
                            space_eps,
                            &mut cur_pos,
                            &mut prev_pos,
                            &mut last_pos_show,
                        );
                    } else {
                        last_pos_show = false;
                        walker_gap(out, &tp);
                        break_column_if_above(out, &tp);
                        show_text(out, &codecs[ci].1, &op.operands);
                        tp.last_show_y = Some(tp.cur_y); tp.last_line_x = tp.line_x;
                        advance_after_show(&mut tp, cur_width, &widths, &codecs[ci].1, &op.operands);
                    }
                }
            }
            "'" => {
                *text_ops_seen = true;
                last_pos_show = false;
                if !ends_with_ws(out) {
                    out.push('\n');
                }
                tp.line_y -= tp.leading;
                tp.goto_line(false);
                if let Some(ci) = cur {
                    break_column_if_above(out, &tp);
                    show_text(out, &codecs[ci].1, &op.operands);
                    tp.last_show_y = Some(tp.cur_y); tp.last_line_x = tp.line_x;
                    advance_after_show(&mut tp, cur_width, &widths, &codecs[ci].1, &op.operands);
                }
            }
            "\"" => {
                *text_ops_seen = true;
                last_pos_show = false;
                if !ends_with_ws(out) {
                    out.push('\n');
                }
                // `aw ac string "` sets word/char spacing before showing.
                if let Some(v) = op.operands.first().and_then(|o| o.as_float().ok()) {
                    tp.word_sp = v as f64;
                }
                if let Some(v) = op.operands.get(1).and_then(|o| o.as_float().ok()) {
                    tp.char_sp = v as f64;
                }
                tp.line_y -= tp.leading;
                tp.goto_line(false);
                if let Some(ci) = cur {
                    if let Some(s) = op.operands.get(2) {
                        break_column_if_above(out, &tp);
                        let one = std::slice::from_ref(s);
                        show_text(out, &codecs[ci].1, one);
                        tp.last_show_y = Some(tp.cur_y); tp.last_line_x = tp.line_x;
                        advance_after_show(&mut tp, cur_width, &widths, &codecs[ci].1, one);
                    }
                }
            }
            // `ET`/`T*` mark line breaks for the string-based case. When the
            // previous show was a positionally-placed glyph, the y-jump already
            // produced the break; suppress the extra newline.
            "T*" | "ET" => {
                if !last_pos_show && !ends_with_ws(out) {
                    out.push('\n');
                }
                if op.operator.as_str() == "T*" {
                    prev_pos = None;
                    tp.line_y -= tp.leading;
                    tp.goto_line(false);
                }
            }
            // Form XObject text: pages whose only content is `/x Do` (some
            // payslip/invoice generators) would otherwise extract nothing.
            "Do" => {
                if budget.do_left == 0 || depth >= MAX_WALKER_FORM_DEPTH {
                    budget.exhausted = true;
                    continue;
                }
                let Some(name) = op.operands.first().and_then(|o| o.as_name().ok()) else {
                    continue;
                };
                let Some((id, form)) = lookup_form(doc, chain, name) else {
                    continue;
                };
                // A form already on the current path draws itself (directly or
                // through a chain); the depth cap alone would still terminate,
                // but this stops the wasted re-walk.
                if id.map_or(false, |id| form_path.contains(&id)) {
                    continue;
                }
                let fchain = form_resource_chain(doc, &form.dict, chain);
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
                budget.ops_left = budget.ops_left.saturating_sub(FORM_INVOCATION_OPS);
                if budget.ops_left == 0 {
                    budget.exhausted = true;
                }
                budget.do_left -= 1;
                if let Some(id) = id {
                    form_path.push(id);
                }
                walk_content(
                    doc,
                    &fchain,
                    &fc.operations,
                    out,
                    text_ops_seen,
                    form_path,
                    depth + 1,
                    budget,
                );
                if id.is_some() {
                    form_path.pop();
                }
            }
            _ => {}
        }
    }
}
