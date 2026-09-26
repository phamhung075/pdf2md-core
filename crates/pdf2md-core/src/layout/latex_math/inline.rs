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

/// Compute the modal size + corresponding baseline of a visual line.
pub(super) fn line_profile(line: &[Span]) -> LineProfile {
    use std::collections::HashMap;
    let mut freq: HashMap<String, (usize, f64)> = HashMap::new();
    let mut sizes: Vec<f64> = Vec::new();
    for s in line {
        if s.text.trim().is_empty() {
            continue;
        }
        let key = format!("{:.1}", (s.size * 10.0).round() / 10.0);
        let e = freq.entry(key).or_insert((0, s.size));
        e.0 += 1;
        sizes.push(s.size);
    }
    if sizes.is_empty() {
        return LineProfile {
            size: line.first().map(|s| s.size).unwrap_or(10.0).max(1.0),
            baseline: line.first().map(|s| s.y).unwrap_or(0.0),
        };
    }
    let max_freq = freq.values().map(|(c, _)| *c).max().unwrap_or(0);
    let mut best: Option<f64> = None;
    for s in sizes {
        let key = format!("{:.1}", (s * 10.0).round() / 10.0);
        if let Some((c, sz)) = freq.get(&key) {
            if *c == max_freq {
                // The base (body) run is the *largest* modal size; scripts are
                // smaller. On a frequency tie (e.g. one base + one script
                // glyph) take the larger so the script is recognised.
                best = Some(match best {
                    None => *sz,
                    Some(b) => b.max(*sz),
                });
            }
        }
    }
    let size = best.unwrap_or(10.0).max(1.0);

    // Baseline of the spans that carry the dominant size.
    let mut ys: Vec<f64> = line
        .iter()
        .filter(|s| !s.text.trim().is_empty() && (s.size - size).abs() <= 0.15 * size)
        .map(|s| s.y)
        .collect();
    ys.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let baseline = if ys.is_empty() {
        line.first().map(|s| s.y).unwrap_or(0.0)
    } else if ys.len() % 2 == 1 {
        ys[ys.len() / 2]
    } else {
        (ys[ys.len() / 2 - 1] + ys[ys.len() / 2]) / 2.0
    };
    LineProfile { size, baseline }
}

/// A horizontal span of glyphs sharing one visual size and baseline. Cells are
/// the atomic unit line-level script detection works on.
#[derive(Debug, Clone)]
pub(super) struct Cell {
    spans: Vec<Span>,
    /// Representative size (mean of the cell span sizes).
    size: f64,
    /// Representative baseline (median of the cell span baselines).
    baseline: f64,
    /// Left edge of the cell (leftmost span x).
    start_x: f64,
    /// Right edge of the cell (last span x + advance).
    end_x: f64,
}

impl Cell {
    /// Concatenated plain text of the cell (no emphasis; math inner content).
    fn text(&self) -> String {
        render_cell_plain(&self.spans)
    }
}

/// Render the concatenated text of a run of spans, inserting a word space only
/// where the horizontal gap clearly exceeds a space advance (same rule the
/// plain-text renderer uses). Never inserts emphasis delimiters — math content
/// is rendered as plain LaTeX inner text.
pub(super) fn render_cell_plain(spans: &[Span]) -> String {
    let mut out = String::new();
    let mut prev_x: Option<f64> = None;
    let mut prev_word_advance = 0.0f64;
    for span in spans {
        if span.text.is_empty() {
            continue;
        }
        let size = span.size.max(0.1);
        if let Some(px) = prev_x {
            let gap = span.x - px;
            if gap - prev_word_advance > WORD_GAP_EM * size {
                if !out.is_empty() && !out.ends_with(' ') {
                    out.push(' ');
                }
            }
        }
        out.push_str(&span.text);
        prev_x = Some(span.x);
        prev_word_advance = span.word_advance;
    }
    out.trim_end().to_string()
}

/// Group consecutive spans into cells by visual size & baseline.
pub(super) fn group_cells(line: &[Span]) -> Vec<Cell> {
    let mut cells: Vec<Cell> = Vec::new();
    for span in line {
        // A whitespace-only run carries no visible glyph, so it can be neither
        // a base nor a super/subscript. Keeping it let a decorative spacer span
        // (smaller size, slightly different baseline) masquerade as a script,
        // emitting `$prose_{}$` / `$prose^{}$` with an empty script. Mirror
        // `line_profile`, which already ignores whitespace-only spans.
        if span.text.trim().is_empty() {
            continue;
        }
        let can_join = cells.last_mut().map_or(false, |c| {
            let tol = 0.2 * c.size.max(0.1);
            (span.size - c.size).abs() <= tol && (span.y - c.baseline).abs() <= tol
        });
        if can_join {
            let c = cells.last_mut().unwrap();
            c.spans.push(span.clone());
            c.baseline = median_baseline(&c.spans);
            c.size = c.spans.iter().map(|s| s.size).sum::<f64>() / c.spans.len() as f64;
            c.end_x = span.x + span.advance;
        } else {
            let size = span.size;
            let baseline = span.y;
            cells.push(Cell {
                spans: vec![span.clone()],
                size,
                baseline,
                start_x: span.x,
                end_x: span.x + span.advance,
            });
        }
    }
    cells
}

pub(super) fn median_baseline(spans: &[Span]) -> f64 {
    let mut ys: Vec<f64> = spans.iter().map(|s| s.y).collect();
    ys.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    if ys.is_empty() {
        0.0
    } else if ys.len() % 2 == 1 {
        ys[ys.len() / 2]
    } else {
        (ys[ys.len() / 2 - 1] + ys[ys.len() / 2]) / 2.0
    }
}

/// Whether a script's base run is genuine math rather than an English prose
/// word. A single glyph (`x`, `n`), a pure number (`10`, `1,200`), or a
/// symbol-only run is math. A multi-character run containing alphabetic
/// characters (`note`, `implementation`, `SkyPilot`, `Face`) is prose: wrapping
/// it in `$...$` renders it in math italics, breaks kerning/ligatures, and
/// defeats text search, so its script is emitted with an empty base instead.
pub(super) fn is_math_base(base: &str) -> bool {
    let trimmed = base.trim();
    if trimmed.is_empty() {
        return false;
    }
    // A single glyph is a variable/symbol (`x`, `n`) and stays math.
    if trimmed.chars().count() == 1 {
        return true;
    }
    // No alphabetic characters: numeric or symbolic (`10`, `1,200`, `++`).
    !trimmed.chars().any(|c| c.is_alphabetic())
}

/// Whether a candidate script cell is a plausible super/subscript rather than a
/// stretch of body text. A real script is a short, single-token run — an
/// exponent (`x^{2}`), an ordinal suffix (`1^{er}`), or a footnote mark
/// (`note^{1}`); a long or multi-word run on an offset baseline is prose
/// (typically a neighbouring column that `page_read_order` failed to separate)
/// and must stay plain text, never `$_{...}$`.
pub(super) fn is_plausible_script(cell: &Cell) -> bool {
    const MAX_SCRIPT_CHARS: usize = 3;
    let text = cell.text();
    let n = text.chars().count();
    n > 0 && n <= MAX_SCRIPT_CHARS && !text.chars().any(|c| c.is_whitespace())
}

/// Rebuild a single visual line's spans into a [`LatexExpr`], recognising simple
/// super/subscripts. When nothing looks like a script the result is a plain
/// `Text` node whose string is byte-identical to the legacy line renderer.
pub fn synthesize_line_expr(line: &[Span]) -> LatexExpr {
    if line.is_empty() {
        return LatexExpr::Text(String::new());
    }
    let profile = line_profile(line);
    let base_size = profile.size;
    let base_baseline = profile.baseline;
    let cells = group_cells(line);

    let mut parts: Vec<LatexExpr> = Vec::new();
    let mut pending_base: Option<Cell> = None;
    let mut consumed = false;
    let size_tol = 0.25 * base_size.max(0.1);

    for cell in cells {
        let is_base_like =
            (cell.size - base_size).abs() <= size_tol && (cell.baseline - base_baseline).abs() <= size_tol;
        let is_script = !is_base_like
            && is_plausible_script(&cell)
            && cell.size < base_size
            && (cell.baseline - base_baseline).abs() >= 0.12 * base_size.max(0.1);

        if is_script {
            if let Some(base) = pending_base.take() {
                if cell.start_x >= base.end_x - 0.5 * base_size.max(0.1) {
                    // Split the base run into word tokens at horizontal gaps so
                    // a footnote-style marker attaches only to the *immediately*
                    // preceding word instead of swallowing the entire prose run
                    // into `$...$` (which rendered as invalid LaTeX and disabled
                    // the quality gate's math check). Preceding words remain
                    // plain text. A single-word base (the common x² / m³ case)
                    // is unaffected.
                    let tokens = split_line_tokens(&base.spans);
                    let base_spans = tokens.last().map(|t| t.as_slice()).unwrap_or(&base.spans);
                    for tok in &tokens[..tokens.len().saturating_sub(1)] {
                        let txt = render_cell_plain(tok);
                        if !txt.is_empty() {
                            parts.push(LatexExpr::text(txt));
                        }
                    }
                    let kind = if cell.baseline > base_baseline {
                        ScriptKind::Superscript
                    } else {
                        ScriptKind::Subscript
                    };
                    let base_text = render_cell_plain(base_spans);
                    let script_expr = LatexExpr::text(cell.text());
                    if is_math_base(&base_text) {
                        let base_expr = LatexExpr::text(base_text);
                        let combined = match kind {
                            ScriptKind::Superscript => {
                                LatexExpr::superscript(base_expr, script_expr)
                            }
                            ScriptKind::Subscript => {
                                LatexExpr::subscript(base_expr, script_expr)
                            }
                        };
                        parts.push(combined);
                    } else {
                        // Prose word / footnote reference: keep the word as
                        // plain text and give the script an empty base so it
                        // serializes as `word$^{1}$`, not `$word^{1}$`.
                        parts.push(LatexExpr::text(base_text));
                        let combined = match kind {
                            ScriptKind::Superscript => {
                                LatexExpr::superscript(LatexExpr::text(""), script_expr)
                            }
                            ScriptKind::Subscript => {
                                LatexExpr::subscript(LatexExpr::text(""), script_expr)
                            }
                        };
                        parts.push(combined);
                    }
                    consumed = true;
                    continue;
                }
                // Script is not to the right of the base: emit the base as text
                // and let the script start a fresh (unattached) base.
                parts.push(LatexExpr::text(base.text()));
            }
            // No eligible base to attach to: emit the isolated script as text so
            // nothing is silently dropped (conservative fallback).
            parts.push(LatexExpr::text(cell.text()));
            continue;
        }

        if let Some(base) = pending_base.take() {
            parts.push(LatexExpr::text(base.text()));
        }
        pending_base = Some(cell);
    }

    if let Some(base) = pending_base.take() {
        parts.push(LatexExpr::text(base.text()));
    }

    if !consumed {
        // No script recognised: fall back to the byte-identical legacy renderer
        // so enabling detection never alters plain text.
        return LatexExpr::Text(render_spans(line));
    }
    LatexExpr::seq(parts)
}

/// Render a visual line to inline text, honouring super/subscripts as `$...$`
/// math. When no script is present this is byte-identical to the legacy
/// `render_spans` output.
pub fn render_math_line(line: &[Span]) -> String {
    let expr = synthesize_line_expr(line);
    match expr {
        LatexExpr::Text(s) => s,
        other => other.render_inline_mixed(),
    }
}

/// Return the inline `$...$` math for a visual line when a super/subscript is
/// detected, else `None`. Callers use this to keep the *original* text for
/// plain lines (byte-identity) and only synthesise math where it exists.
pub fn math_inline_for_line(line: &[Span]) -> Option<String> {
    match synthesize_line_expr(line) {
        LatexExpr::Text(_) => None,
        other => Some(other.render_inline_mixed()),
    }
}

// ---------------------------------------------------------------------------
// Cross-line built-up fraction detection
// ---------------------------------------------------------------------------
