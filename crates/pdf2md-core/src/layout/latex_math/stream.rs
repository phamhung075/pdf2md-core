// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! LaTeX math AST synthesis built directly from 2D glyph geometry.
//!
//! Modern PDF producers typeset math as positioned glyph runs rather than as a
//! semantic math tree: a built-up fraction is a smaller numerator run stacked
//! above a thin horizontal rule (the fraction bar) with a smaller denominator
//! run below it, and a simple super/subscript is a smaller run whose baseline
//! is raised/lowered relative to its base. Before the geometry engine exports
//! the page as Markdown it can now lift these visual patterns into a real
//! [`LatexExpr`] AST and serialize them as semantically correct LaTeX
//! (`$\frac{a}{b}$`, `$x^{2}$`, `$a_{i}$`) instead of emitting them as a flat
//! sequence of glyphs.
//!
//! The detector is intentionally geometric and conservative, and only runs when
//! the caller opts in via `detect_math` so the default extraction path stays
//! byte-identical.

use super::*;

/// Render a single reading-order stream to text, merging detected fractions
/// and cross-line super/subscripts into inline `$...$` math, and applying
/// within-line script detection to the remaining lines.
pub(super) fn render_math_stream(
    stream: &[Vec<Span>],
    bars: &[RuleSeg],
    page_height: f64,
    drop_furniture: bool,
    body_size: f64,
    page_width: Option<f64>,
) -> String {
    let mut fractions = detect_fractions(stream, bars);
    fractions.extend(detect_stacked_fractions(stream));
    let fractions = reject_dense_fraction_clusters(fractions);
    let scripts = detect_scripts_cross_line(stream);

    let mut replacement: Vec<Option<String>> = vec![None; stream.len()];
    let mut skip: Vec<bool> = vec![false; stream.len()];
    for f in &fractions {
        replacement[f.numerator_line] = Some(f.expr.render_inline());
        if f.denominator_line < stream.len() {
            skip[f.denominator_line] = true;
        }
    }
    for s in &scripts {
        if replacement[s.base_line].is_none() && !skip[s.base_line] {
            let mut txt = s.base_prefix.clone();
            if !txt.is_empty() && !txt.ends_with(' ') {
                txt.push(' ');
            }
            txt.push_str(&s.expr.render_inline());
            replacement[s.base_line] = Some(txt);
        }
        skip[s.script_line] = true;
    }

    use crate::layout::reading_order::{
        classify_line, format_structured_line, split_hard_breaks, ListRunState,
    };

    let mut out = String::new();
    let mut prev_y: Option<f64> = None;
    let mut list_state = ListRunState::default();

    for (i, line) in stream.iter().enumerate() {
        if drop_furniture && crate::layout::reading_order::is_page_number_line(line, page_height) {
            continue;
        }
        if skip[i] {
            // Consumed by a fraction / script already emitted at its source line.
            continue;
        }
        let size = line.first().map(|s| s.size).unwrap_or(10.0).max(0.1);
        if let Some(py) = prev_y {
            if py - line.first().map(|s| s.y).unwrap_or(0.0) > 2.0 * size {
                out.push('\n');
            }
        }
        if let Some(text) = replacement[i].take() {
            // Classification runs on the original line geometry for a
            // fraction/script replacement — those substitutions target formula
            // content, which is never itself a heading or list marker.
            let (role, _) = classify_line(line, body_size, &mut list_state);
            out.push_str(&format_structured_line(&role, text.trim_end()));
            out.push('\n');
        } else {
            // Split a row that jams two unrelated regions onto one baseline at
            // the same >2.5em hard break the plain renderer uses
            // (`push_line`), and synthesise math per segment. Without this the
            // within-line script renderer re-welded such a row into one line —
            // `render_math_line` does not apply `render_spans`'s hard break — so
            // a page whose columns `page_read_order` left fused stayed fused.
            for seg in split_hard_breaks(line, page_width) {
                if seg.is_empty() {
                    continue;
                }
                let (role, render_slice) = classify_line(&seg, body_size, &mut list_state);
                let text = render_math_line(render_slice, page_width);
                out.push_str(&format_structured_line(&role, text.trim_end()));
                out.push('\n');
            }
        }
        prev_y = Some(line.first().map(|s| s.y).unwrap_or(0.0));
    }
    out.trim_end().to_string()
}

/// Render page text with LaTeX math synthesis. When the page is a single column
/// this mirrors `render_human_order` except that detected fractions become
/// `$...$` and simple super/subscripts are lifted into their base/script form.
/// When the page is two columns each stream is processed independently.
#[allow(clippy::too_many_arguments)]
pub fn render_math(
    lines: &[Vec<Span>],
    bars: &[RuleSeg],
    page_height: f64,
    drop_furniture: bool,
    page_width: Option<f64>,
) -> String {
    let body_size = crate::layout::reading_order::body_size_for(lines);
    let streams = crate::layout::reading_order::page_read_order(lines);
    if streams.len() == 1 {
        return render_math_stream(&streams[0], bars, page_height, drop_furniture, body_size, page_width);
    }
    let mut out = String::new();
    for (ci, stream) in streams.iter().enumerate() {
        if ci > 0 && !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&render_math_stream(stream, bars, page_height, drop_furniture, body_size, page_width));
    }
    out.trim_end().to_string()
}

// ---------------------------------------------------------------------------
// Cross-line super/subscript + stacked fraction detection (LayoutAST / tagged)
// ---------------------------------------------------------------------------

use crate::cpdf_textpage::TextLine;

/// Convert a clustered `TextLine` into glyph `Span`s (baseline-ordered).
///
/// A `TextLine` from the layout clusterer shares one baseline, so the widths /
/// sizes carry the geometry the math detector needs. Whitespace glyphs are
/// dropped: the plain path falls back to `TextLine::text`, so no spacing is
/// lost; only glyph geometry is inspected.
pub fn spans_from_textline(line: &TextLine) -> Vec<Span> {
    line.chars
        .iter()
        .filter(|c| !c.unicode.is_whitespace())
        .map(|c| Span {
            text: c.unicode.to_string(),
            x: c.origin.0,
            y: c.origin.1,
            size: c.font_size.max(0.1),
            advance: c.advance_width.max(c.font_size * 0.25),
            word_advance: c.advance_width.max(c.font_size * 0.25),
            is_bold: c.is_bold,
            is_italic: c.is_italic,
            is_underline: false,
            is_vertical: false,
        })
        .collect()
}

/// Split a visual line into word tokens at the horizontal gaps that the plain
/// renderer treats as word spaces.
pub(super) fn split_line_tokens(line: &[Span]) -> Vec<Vec<Span>> {
    let mut tokens: Vec<Vec<Span>> = Vec::new();
    let mut cur: Vec<Span> = Vec::new();
    let mut prev_x: Option<f64> = None;
    let mut prev_word_advance = 0.0f64;
    for s in line {
        if let Some(px) = prev_x {
            let gap = s.x - px;
            let size = s.size.max(0.1);
            if gap - prev_word_advance > WORD_GAP_EM * size && !cur.is_empty() {
                tokens.push(std::mem::take(&mut cur));
            }
        }
        prev_x = Some(s.x);
        prev_word_advance = s.word_advance;
        cur.push(s.clone());
    }
    if !cur.is_empty() {
        tokens.push(cur);
    }
    tokens
}

/// Detect simple super/subscripts across adjacent visual lines within a stream.
/// The script attaches to the *trailing token* of the base line, keeping any
/// leading text (e.g. `E = mc²`) rendered plainly.
pub(super) fn detect_scripts_cross_line(stream: &[Vec<Span>]) -> Vec<ScriptHit> {
    let mut used = vec![false; stream.len()];
    let mut hits = Vec::new();
    for i in 0..stream.len() {
        if used[i] || stream[i].is_empty() {
            continue;
        }
        let base = &stream[i];
        let prof = line_profile(base);
        let base_size = prof.size;
        let base_baseline = prof.baseline;
        let tokens = split_line_tokens(base);
        let Some(base_token) = tokens.last() else {
            continue;
        };
        let base_token_text = render_cell_plain(base_token);
        if base_token_text.is_empty() || base_token_text.chars().count() > 8 {
            continue;
        }
        // The base token is the trailing `base_token.len()` spans of the line.
        let base_token_start = base.len() - base_token.len();
        let base_prefix = render_cell_plain(&base[..base_token_start]);
        let (bt_x0, _, bt_x1, _) = line_bbox(base_token);
        // A superscript sits on a higher baseline (the line above, j = i-1);
        // a subscript sits lower (the line below, j = i+1).
        for j in [i.wrapping_sub(1), i + 1] {
            if j >= stream.len() || j == i || used[j] || stream[j].is_empty() {
                continue;
            }
            let script = &stream[j];
            let sprof = line_profile(script);
            let s_size = sprof.size;
            let s_baseline = sprof.baseline;
            if s_size >= base_size * SCRIPT_SIZE_RATIO {
                continue;
            }
            let dy = base_baseline - s_baseline;
            if dy.abs() < SCRIPT_OFFSET_MIN * base_size.max(0.1) {
                continue;
            }
            let (sx0, _, sx1, _) = line_bbox(script);
            // The script hugs the base token horizontally, sitting at its right.
            if sx0 < bt_x0 - 0.1 * base_size.max(0.1)
                || sx0 > bt_x1 + 0.8 * base_size.max(0.1)
                || sx1 < bt_x1 - 0.1 * base_size.max(0.1)
            {
                continue;
            }
            let s_text = render_cell_plain(script);
            if s_text.chars().count() > 4 {
                continue;
            }
            let kind = if s_baseline > base_baseline {
                ScriptKind::Superscript
            } else {
                ScriptKind::Subscript
            };
            let expr = match kind {
                ScriptKind::Superscript => LatexExpr::superscript(
                    LatexExpr::text(base_token_text.clone()),
                    LatexExpr::text(s_text),
                ),
                ScriptKind::Subscript => LatexExpr::subscript(
                    LatexExpr::text(base_token_text.clone()),
                    LatexExpr::text(s_text),
                ),
            };
            hits.push(ScriptHit {
                base_line: i,
                script_line: j,
                kind,
                base_prefix,
                expr,
            });
            used[i] = true;
            used[j] = true;
            break;
        }
    }
    hits
}

pub(super) fn synthesize_stream_text_joined<F: Fn(usize, &[Span]) -> String>(
    stream: &[Vec<Span>],
    bars: &[RuleSeg],
    joiner: &str,
    fallback: F,
) -> String {
    let mut hits = detect_fractions(stream, bars);
    hits.extend(detect_stacked_fractions(stream));
    let hits = reject_dense_fraction_clusters(hits);
    let scripts = detect_scripts_cross_line(stream);

    let mut replacement: Vec<Option<String>> = vec![None; stream.len()];
    let mut skip: Vec<bool> = vec![false; stream.len()];
    // Fractions take priority over scripts for a shared numerator line.
    for f in &hits {
        replacement[f.numerator_line] = Some(f.expr.render_inline());
        skip[f.denominator_line] = true;
    }
    for s in &scripts {
        if replacement[s.base_line].is_none() && !skip[s.base_line] {
            let mut txt = s.base_prefix.clone();
            if !txt.is_empty() && !txt.ends_with(' ') {
                txt.push(' ');
            }
            txt.push_str(&s.expr.render_inline());
            replacement[s.base_line] = Some(txt);
        }
        skip[s.script_line] = true;
    }

    let mut out = String::new();
    for (i, line) in stream.iter().enumerate() {
        if skip[i] {
            continue;
        }
        let text = replacement[i]
            .take()
            .unwrap_or_else(|| fallback(i, line));
        if !out.is_empty() {
            out.push_str(joiner);
        }
        out.push_str(&text);
    }
    out
}

/// Synthesize LaTeX math for a semantic block of `TextLine`s (the LayoutAST
/// path). Lines with no detected math keep their original text, so enabling
/// detection never re-renders plain prose. `joiner` is placed between the
/// surviving lines (a hard line break for paragraphs, a space for headings).
pub fn synthesize_block_text(lines: &[TextLine], bars: &[RuleSeg], joiner: &str) -> String {
    let stream: Vec<Vec<Span>> = lines.iter().map(spans_from_textline).collect();
    let originals: Vec<String> = lines.iter().map(|l| l.text.trim().to_string()).collect();
    synthesize_stream_text_joined(&stream, bars, joiner, |i, _line| originals[i].clone())
}

/// Synthesize LaTeX math for a flat run of glyph spans (a marked-content run
/// from the tagged structure path). Spans are grouped into visual lines by
/// baseline before script/fraction detection.
pub fn synthesize_spans_math(spans: &[Span], bars: &[RuleSeg]) -> String {
    let mut lines: Vec<Vec<Span>> = Vec::new();
    for s in spans {
        let eps = 0.5 * s.size.max(0.1);
        let last_y = lines.last().and_then(|l| l.first()).map(|x| x.y).unwrap_or(0.0);
        match lines.last_mut() {
            Some(last) if (s.y - last_y).abs() <= eps => last.push(s.clone()),
            _ => lines.push(vec![s.clone()]),
        }
    }
    lines.sort_by(|a, b| {
        let ay = a.first().map(|x| x.y).unwrap_or(0.0);
        let by = b.first().map(|x| x.y).unwrap_or(0.0);
        by.partial_cmp(&ay).unwrap_or(std::cmp::Ordering::Equal)
    });
    for line in &mut lines {
        line.sort_by(|a, b| a.x.partial_cmp(&b.x).unwrap_or(std::cmp::Ordering::Equal));
    }
    synthesize_stream_text_joined(&lines, bars, "\n", |_, line| render_cell_plain(line))
}
