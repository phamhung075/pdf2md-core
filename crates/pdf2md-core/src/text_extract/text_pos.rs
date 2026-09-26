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

pub(super) fn ends_with_ws(s: &str) -> bool {
    s.chars().next_back().map_or(true, |c| c.is_whitespace())
}

pub(super) fn push_decoded(out: &mut String, codec: &Codec, bytes: &[u8]) {
    let mut decoded = String::new();
    codec.decode(bytes, &mut decoded);
    // Producers routinely pad every show-string with a leading and trailing
    // space (`( text )`). Appending that verbatim duplicates the separator at
    // a run boundary (`ÉLECTRONIQUE  RÉFÉRENCE`) and leaves a stray leading
    // space at the start of the page. Drop the incoming padding when the
    // buffer is empty or already ends in whitespace; keep a genuine word gap
    // (the string's own leading space) when it does not.
    if ends_with_ws(out) {
        out.push_str(decoded.trim_start());
    } else {
        out.push_str(&decoded);
    }
}

/// True when a `Td`/`TD` operand pair moves the text line vertically — the
/// same break `T*` performs. `tx ty Td` translates the text line matrix; a
/// nonzero `ty` starts a new line, while a horizontal-only `tx` move stays on
/// the current line. Threshold matches the `line_eps` used by coordinate mode.
pub(super) fn td_advances_line(op: &Operation) -> bool {
    op.operands
        .get(1)
        .and_then(|o| o.as_float().ok())
        .map_or(false, |ty| ty.abs() > 0.5)
}

/// Start a new output line, discarding the padding a producer left at the end
/// of the previous show-string (`( text )` ends in a space). Without the trim
/// the trailing space keeps `ends_with_ws` true, so the break is skipped and
/// the next line fuses onto the current one.
pub(super) fn break_line(out: &mut String) {
    while out.ends_with(' ') || out.ends_with('\t') {
        out.pop();
    }
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
}

/// Emit a paragraph break (blank line) when the next shown baseline is more
/// than one em *above* the previous shown line: that is a new column or block
/// starting at the top of the page, not a wrapped line, so a later markdown
/// reflow must not weld the two together. Does nothing when the position
/// tracker is unusable or the font size is unknown.
pub(super) fn break_column_if_above(out: &mut String, tp: &TextPos) {
    let Some(prev) = tp.last_show_y else { return };
    if tp.unusable || tp.size <= 0.0 {
        return;
    }
    let shift = tp.cur_y - prev;
    let above = shift > tp.size;
    // A run that starts inside — or immediately after — the horizontal span
    // the previous run covered is continuing the same visual line, whatever
    // the baseline shift: an inline superscript / footnote mark, a subscript,
    // or a kerned fragment. Compare against where the previous text actually
    // ENDED (`last_end_x`), not where the line started. Only a run that starts
    // clear of that span can be a new column.
    let same_line = match tp.last_end_x {
        Some(end) => {
            let lo = tp.last_line_x.min(end);
            let hi = tp.last_line_x.max(end);
            tp.cur_x >= lo - tp.size && tp.cur_x <= hi + tp.size
        }
        // No previous show end known: be conservative and treat it as the
        // same line rather than inventing a break.
        None => true,
    };
    // A new column starts on the next visual row, so require a real vertical
    // move: at least half a line (a few points' rise is a super/subscript, not
    // a column). When the run is in a smaller face it must clear the full
    // 0.7 em threshold, since a small face a few points up is a footnote mark.
    let half_line = shift >= 0.5 * tp.size;
    let full_line = shift >= 0.7 * tp.size;
    let comparable_size = tp.last_show_size <= 0.0
        || (tp.size >= 0.75 * tp.last_show_size && tp.size <= 1.33 * tp.last_show_size);
    let sideways = half_line && !same_line && (full_line || comparable_size);
    if !(above || sideways) {
        return;
    }
    while out.ends_with(' ') || out.ends_with('\t') {
        out.pop();
    }
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    if !out.is_empty() && !out.ends_with("\n\n") {
        out.push('\n');
    }
}

impl TextPos {
    pub(super) fn new() -> Self {
        Self {
            line_x: 0.0,
            line_y: 0.0,
            cur_x: 0.0,
            cur_y: 0.0,
            prev_end_x: None,
            last_end_x: None,
            last_show_size: 0.0,
            prev_y: 0.0,
            last_show_y: None,
            last_line_x: 0.0,
            size: 0.0,
            hscale: 1.0,
            char_sp: 0.0,
            word_sp: 0.0,
            leading: 0.0,
            unusable: false,
        }
    }

    /// Move the text matrix to the current line origin. `keep_gap` is true for
    /// a horizontal-only `Td`/`TD` (a fragment placement on the same line),
    /// which must not discard the previous show's end; any real line advance
    /// clears it so a gap is never measured across lines.
    pub(super) fn goto_line(&mut self, keep_gap: bool) {
        self.cur_x = self.line_x;
        self.cur_y = self.line_y;
        if !keep_gap {
            self.prev_end_x = None;
        }
        self.prev_y = self.line_y;
    }
}

/// Natural advance, in device points, of one show operation (a `Tj` string, a
/// whole `TJ` array, or a single string operand). Sums each glyph's `/Widths`
/// entry and applies `Tc`/`Tw`/`Tz`, mirroring `glyph_stream`'s text-matrix
/// advance. A font with no usable metrics falls back to
/// [`FALLBACK_ADVANCE_EM`] per code.
pub(super) fn show_advance(
    widths: &Widths,
    codec: &Codec,
    operands: &[Object],
    size: f64,
    char_sp: f64,
    word_sp: f64,
    hscale: f64,
) -> f64 {
    let mut w1000 = 0.0f64;
    let mut nchars = 0usize;
    let mut nspaces = 0usize;
    fn add_bytes(
        widths: &Widths,
        codec: &Codec,
        bytes: &[u8],
        w1000: &mut f64,
        nchars: &mut usize,
        nspaces: &mut usize,
    ) {
        // Count codes, not raw bytes: `Tc`/`Tw` are applied per character code,
        // and a 2-byte CID/Identity-H font's byte length is twice its glyph
        // count. The same number drives the no-metrics fallback advance.
        let (codes, spaces) = codec.code_metrics(bytes);
        match widths.width(bytes) {
            Some(w) => *w1000 += w,
            None => *w1000 += FALLBACK_ADVANCE_EM * 1000.0 * codes as f64,
        }
        *nchars += codes;
        *nspaces += spaces;
    }
    for operand in operands {
        match operand {
            Object::String(bytes, _) => {
                add_bytes(widths, codec, bytes, &mut w1000, &mut nchars, &mut nspaces)
            }
            Object::Array(items) => {
                for item in items {
                    match item {
                        Object::String(bytes, _) => {
                            add_bytes(widths, codec, bytes, &mut w1000, &mut nchars, &mut nspaces)
                        }
                        // A `TJ` number is a manual kern in 1/1000 em.
                        Object::Integer(v) => w1000 -= *v as f64,
                        Object::Real(v) => w1000 -= *v as f64,
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    (w1000 / 1000.0 * size + char_sp * nchars as f64 + word_sp * nspaces as f64) * hscale
}

/// Insert a word separator before a show when the text matrix has advanced more
/// than [`WORD_GAP_EM`] em past the previous show's natural end on the same
/// baseline. No separator is inserted across a line break (the tracker resets
/// `prev_end_x`) or when the output already ends in whitespace.
pub(super) fn walker_gap(out: &mut String, tp: &TextPos) {
    if tp.unusable || tp.size <= 0.0 {
        return;
    }
    let Some(prev_end) = tp.prev_end_x else { return };
    if (tp.cur_y - tp.prev_y).abs() > 0.5 {
        return;
    }
    if tp.cur_x - prev_end > WORD_GAP_EM * tp.size && !ends_with_ws(out) {
        out.push(' ');
    }
}

/// Record the natural advance of a show that was just appended, so the next
/// show can be measured against it.
pub(super) fn advance_after_show(
    tp: &mut TextPos,
    cur_width: Option<usize>,
    widths: &[(Vec<u8>, Widths)],
    codec: &Codec,
    operands: &[Object],
) {
    if let Some(wi) = cur_width {
        let adv = show_advance(
            &widths[wi].1,
            codec,
            operands,
            tp.size,
            tp.char_sp,
            tp.word_sp,
            tp.hscale,
        );
        tp.cur_x += adv;
        // Word-gap inference only measures within a visual line, so this is
        // cleared by a line move.
        tp.prev_end_x = Some(tp.cur_x);
    }
    // The paragraph-break rule must survive line moves: it asks whether the
    // next run continues the line the previous run drew, so it needs the end
    // even when the font metrics were unusable.
    tp.last_end_x = Some(tp.cur_x);
    tp.last_show_size = tp.size;
    tp.prev_y = tp.cur_y;
}

pub(super) fn show_text(out: &mut String, codec: &Codec, operands: &[Object]) {
    for operand in operands {
        match operand {
            Object::String(bytes, _) => push_decoded(out, codec, bytes),
            Object::Array(items) => {
                for item in items {
                    match item {
                        Object::String(bytes, _) => push_decoded(out, codec, bytes),
                        // Large negative kerning behaves as a word gap. The
                        // kern may be an Integer or a Real in a `TJ` array.
                        Object::Integer(v) if *v < -100 => {
                            if !ends_with_ws(out) {
                                out.push(' ');
                            }
                        }
                        Object::Real(v) if *v < -100.0 => {
                            if !ends_with_ws(out) {
                                out.push(' ');
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
}
