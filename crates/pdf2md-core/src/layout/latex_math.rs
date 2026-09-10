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

use serde::{Deserialize, Serialize};

use crate::layout::glyph_stream::Span;
use crate::layout::reading_order::render_spans;

/// Recursive LaTeX math expression AST.
///
/// The variants map 1:1 onto the visual patterns the geometry engine can
/// recognise: text runs, a built-up fraction, and a simple super/subscript.
/// A single inline expression is generally a [`LatexExpr::Sequence`] of those
/// building blocks in left-to-right order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum LatexExpr {
    /// A plain text run (may carry emphasis in the surrounding Markdown).
    Text(String),
    /// Built-up fraction: numerator over denominator.
    Fraction {
        numerator: Box<LatexExpr>,
        denominator: Box<LatexExpr>,
    },
    /// Simple superscript: `base^{exponent}`.
    Superscript {
        base: Box<LatexExpr>,
        exponent: Box<LatexExpr>,
    },
    /// Simple subscript: `base_{index}`.
    Subscript {
        base: Box<LatexExpr>,
        index: Box<LatexExpr>,
    },
    /// Horizontal concatenation of sub-expressions.
    Sequence(Vec<LatexExpr>),
}

impl LatexExpr {
    /// Concise constructor for a plain text node.
    pub fn text(t: impl Into<String>) -> Self {
        LatexExpr::Text(t.into())
    }

    /// Constructor for a built-up fraction.
    pub fn fraction(numerator: Self, denominator: Self) -> Self {
        LatexExpr::Fraction {
            numerator: Box::new(numerator),
            denominator: Box::new(denominator),
        }
    }

    /// Constructor for a simple superscript.
    pub fn superscript(base: Self, exponent: Self) -> Self {
        LatexExpr::Superscript {
            base: Box::new(base),
            exponent: Box::new(exponent),
        }
    }

    /// Constructor for a simple subscript.
    pub fn subscript(base: Self, index: Self) -> Self {
        LatexExpr::Subscript {
            base: Box::new(base),
            index: Box::new(index),
        }
    }

    /// Flatten a list of expressions into a `Sequence` (or a single node when
    /// there is only one element). Used to keep serialization compact.
    pub fn seq(parts: Vec<Self>) -> Self {
        match parts.len() {
            0 => LatexExpr::Text(String::new()),
            1 => parts.into_iter().next().unwrap(),
            _ => LatexExpr::Sequence(parts),
        }
    }

    /// Serialize to raw LaTeX source (no surrounding `$` delimiters).
    pub fn to_latex(&self) -> String {
        match self {
            LatexExpr::Text(s) => s.clone(),
            LatexExpr::Fraction {
                numerator,
                denominator,
            } => format!(
                "\\frac{{{}}}{{{}}}",
                numerator.to_latex(),
                denominator.to_latex()
            ),
            LatexExpr::Superscript { base, exponent } => {
                format!("{}^{{{}}}", base.to_latex(), exponent.to_latex())
            }
            LatexExpr::Subscript { base, index } => {
                format!("{}_{{{}}}", base.to_latex(), index.to_latex())
            }
            LatexExpr::Sequence(parts) => parts.iter().map(Self::to_latex).collect::<String>(),
        }
    }

    /// Render as inline math surrounded by `$` delimiters (`$\frac{a}{b}$`).
    pub fn render_inline(&self) -> String {
        format!("${}$", self.to_latex())
    }

    /// Render into Markdown, putting inline `$...$` delimiters **only** around
    /// the math nodes (fraction / superscript / subscript) and leaving plain
    /// text runs untouched.
    ///
    /// Prior behaviour wrapped the whole line (`other.render_inline()`), so a
    /// prose line carrying a single footnote marker or a `$1,200 … $3,400`
    /// amount got fully swallowed by `$...$` — invalid LaTeX that renders as
    /// garbage in Obsidian, and a single spurious span that satisfied the
    /// quality gate's "LaTeX math fences present" check and disabled it. A pure
    /// math expression still renders as `$x^{2}$`; surrounding prose stays plain.
    pub fn render_inline_mixed(&self) -> String {
        match self {
            LatexExpr::Text(s) => s.clone(),
            LatexExpr::Fraction { .. }
            | LatexExpr::Superscript { .. }
            | LatexExpr::Subscript { .. } => format!("${}$", self.to_latex()),
            LatexExpr::Sequence(parts) => parts.iter().map(Self::render_inline_mixed).collect(),
        }
    }
}

// ---------------------------------------------------------------------------
// Within-line super/subscript detection
// ---------------------------------------------------------------------------

/// The base (body) size class and baseline of a visual line, derived by modal
/// statistics so a single large heading or an inserted script does not skew it.
#[derive(Debug, Clone, Copy)]
struct LineProfile {
    size: f64,
    baseline: f64,
}

/// Compute the modal size + corresponding baseline of a visual line.
fn line_profile(line: &[Span]) -> LineProfile {
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
struct Cell {
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
fn render_cell_plain(spans: &[Span]) -> String {
    let mut out = String::new();
    let mut prev_x: Option<f64> = None;
    let mut prev_advance = 0.0f64;
    for span in spans {
        if span.text.is_empty() {
            continue;
        }
        let size = span.size.max(0.1);
        if let Some(px) = prev_x {
            let gap = span.x - px;
            if gap - prev_advance > 0.65 * (0.25 * size) {
                if !out.is_empty() && !out.ends_with(' ') {
                    out.push(' ');
                }
            }
        }
        out.push_str(&span.text);
        prev_x = Some(span.x);
        prev_advance = span.advance;
    }
    out.trim_end().to_string()
}

/// Group consecutive spans into cells by visual size & baseline.
fn group_cells(line: &[Span]) -> Vec<Cell> {
    let mut cells: Vec<Cell> = Vec::new();
    for span in line {
        if span.text.is_empty() {
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

fn median_baseline(spans: &[Span]) -> f64 {
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

/// A super/subscript run attached to a base run within one visual line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScriptKind {
    Superscript,
    Subscript,
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
                    let base_expr = LatexExpr::text(render_cell_plain(base_spans));
                    let script_expr = LatexExpr::text(cell.text());
                    let combined = match kind {
                        ScriptKind::Superscript => {
                            LatexExpr::superscript(base_expr, script_expr)
                        }
                        ScriptKind::Subscript => LatexExpr::subscript(base_expr, script_expr),
                    };
                    parts.push(combined);
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

/// A thin horizontal rule candidate. `(y, x0, x1)` in device coordinates
/// (larger `y` is higher on the page).
pub type RuleSeg = (f64, f64, f64);

/// A detected built-up fraction.
#[derive(Debug, Clone)]
pub struct FractionHit {
    /// Index (within the stream) of the numerator visual line.
    pub numerator_line: usize,
    /// Index (within the stream) of the denominator visual line.
    pub denominator_line: usize,
    /// Synthesised fraction expression.
    pub expr: LatexExpr,
}

fn line_bbox(line: &[Span]) -> (f64, f64, f64, f64) {
    let x0 = line.iter().map(|s| s.x).fold(f64::INFINITY, f64::min);
    let x1 = line
        .iter()
        .map(|s| s.x + s.advance)
        .fold(f64::NEG_INFINITY, f64::max);
    let y = line.first().map(|s| s.y).unwrap_or(0.0);
    let size = line
        .iter()
        .map(|s| s.size)
        .fold(f64::NEG_INFINITY, f64::max)
        .max(0.1);
    (x0, y - 0.5 * size, x1, y + 0.5 * size)
}

fn overlap(a0: f64, a1: f64, b0: f64, b1: f64) -> f64 {
    (a1.min(b1) - a0.max(b0)).max(0.0)
}

/// Check whether a thin horizontal rule between two stacked runs looks like a
/// fraction bar rather than a paragraph rule / table border / underline.
fn is_fraction_bar(line: RuleSeg, num_line: &[Span], den_line: &[Span]) -> bool {
    let (bar_y, bx0, bx1) = line;
    let bar_w = (bx1 - bx0).abs();
    let (nx0, _, nx1, _) = line_bbox(num_line);
    let (dx0, _, dx1, _) = line_bbox(den_line);
    let num_size = num_line
        .iter()
        .map(|s| s.size)
        .fold(f64::NEG_INFINITY, f64::max)
        .max(0.1);

    // A fraction bar must be a short rule (not a full-width divider).
    if bar_w <= 0.0 {
        return false;
    }
    // The bar must be horizontally near the numerator/denominator (centred),
    // and overlap them.
    let num_overlap = overlap(nx0, nx1, bx0, bx1);
    let den_overlap = overlap(dx0, dx1, bx0, bx1);
    if num_overlap < 0.5 * bar_w.min(nx1 - nx0) || den_overlap < 0.5 * bar_w.min(dx1 - dx0) {
        return false;
    }
    // Stacked tightly: numerator baseline above the bar, denominator below.
    let num_baseline = num_line.first().map(|s| s.y).unwrap_or(0.0);
    let den_baseline = den_line.first().map(|s| s.y).unwrap_or(0.0);
    if !(num_baseline > bar_y && den_baseline < bar_y) {
        return false;
    }
    let above = num_baseline - bar_y;
    let below = bar_y - den_baseline;
    // Tight vertical stack relative to the numerator size.
    if above > 2.2 * num_size || below > 2.2 * num_size {
        return false;
    }
    true
}

/// Detect built-up fractions across a reading-order stream, using the thin
/// horizontal rules collected from the page content stream as fraction bars.
/// Returns the detected fractions in top-to-bottom order.
pub fn detect_fractions(stream: &[Vec<Span>], bars: &[RuleSeg]) -> Vec<FractionHit> {
    let mut hits = Vec::new();
    let mut used: Vec<bool> = vec![false; stream.len()];

    for (li, line) in stream.iter().enumerate() {
        if used[li] || line.is_empty() {
            continue;
        }
        let (lx0, _, lx1, _) = line_bbox(line);
        let line_size = line
            .iter()
            .map(|s| s.size)
            .fold(f64::NEG_INFINITY, f64::max)
            .max(0.1);
        // Find a bar that sits near this line's vertical belly and overlaps it
        // horizontally. We only try to match numerator (line above bar).
        for &(bar_y, bx0, bx1) in bars {
            let baseline = line.first().map(|s| s.y).unwrap_or(0.0);
            // The numerator must be above the bar and reasonably close.
            if baseline <= bar_y {
                continue;
            }
            if baseline - bar_y > 2.2 * line_size {
                continue;
            }
            if overlap(lx0, lx1, bx0, bx1) < 0.5 * line_size {
                continue;
            }
            // Search for a denominator line below the bar.
            let mut den_idx = None;
            for (di, dline) in stream.iter().enumerate() {
                if used[di] || di == li || dline.is_empty() {
                    continue;
                }
                let (dx0, _, dx1, _) = line_bbox(dline);
                let dbaseline = dline.first().map(|s| s.y).unwrap_or(0.0);
                if dbaseline >= bar_y {
                    continue;
                }
                if bar_y - dbaseline > 2.2 * line_size {
                    continue;
                }
                if overlap(dx0, dx1, bx0, bx1) < 0.5 * line_size {
                    continue;
                }
                den_idx = Some(di);
                break;
            }
            let Some(di) = den_idx else { continue };
            if used[di] {
                continue;
            }
            if !is_fraction_bar((bar_y, bx0, bx1), line, &stream[di]) {
                continue;
            }
            let num_expr = LatexExpr::text(render_cell_plain(line));
            let den_expr = LatexExpr::text(render_cell_plain(&stream[di]));
            hits.push(FractionHit {
                numerator_line: li,
                denominator_line: di,
                expr: LatexExpr::fraction(num_expr, den_expr),
            });
            used[li] = true;
            used[di] = true;
            break;
        }
    }
    // Top-to-bottom order.
    hits.sort_by(|a, b| a.numerator_line.cmp(&b.numerator_line));
    hits
}

// ---------------------------------------------------------------------------
// Page-level math-aware renderer
// ---------------------------------------------------------------------------

/// Render a single reading-order stream to text, merging detected fractions
/// and cross-line super/subscripts into inline `$...$` math, and applying
/// within-line script detection to the remaining lines.
fn render_math_stream(
    stream: &[Vec<Span>],
    bars: &[RuleSeg],
    page_height: f64,
    drop_furniture: bool,
) -> String {
    let mut fractions = detect_fractions(stream, bars);
    fractions.extend(detect_stacked_fractions(stream));
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

    let mut out = String::new();
    let mut prev_y: Option<f64> = None;

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
        let text = replacement[i]
            .take()
            .unwrap_or_else(|| render_math_line(line));
        out.push_str(text.trim_end());
        out.push('\n');
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
) -> String {
    let streams = crate::layout::reading_order::page_read_order(lines);
    if streams.len() == 1 {
        return render_math_stream(&streams[0], bars, page_height, drop_furniture);
    }
    let mut out = String::new();
    for (ci, stream) in streams.iter().enumerate() {
        if ci > 0 && !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&render_math_stream(stream, bars, page_height, drop_furniture));
    }
    out.trim_end().to_string()
}

// ---------------------------------------------------------------------------
// Cross-line super/subscript + stacked fraction detection (LayoutAST / tagged)
// ---------------------------------------------------------------------------

use crate::cpdf_textpage::TextLine;

/// A script must be meaningfully smaller than its base: `size < base * RATIO`.
const SCRIPT_SIZE_RATIO: f64 = 0.92;
/// A script must be vertically offset from its base by at least this fraction
/// of the base size.
const SCRIPT_OFFSET_MIN: f64 = 0.12;

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
            is_bold: c.is_bold,
            is_italic: c.is_italic,
            is_underline: false,
            is_vertical: false,
        })
        .collect()
}

/// A super/subscript spread across two adjacent visual lines (the common case
/// once clustering splits a script onto its own baseline).
#[derive(Debug, Clone)]
pub struct ScriptHit {
    pub base_line: usize,
    pub script_line: usize,
    pub kind: ScriptKind,
    /// Leading text on the base line *before* the token the script attaches to
    /// (e.g. the `E = ` in `E = mc²`), rendered plainly.
    pub base_prefix: String,
    pub expr: LatexExpr,
}

/// Split a visual line into word tokens at the horizontal gaps that the plain
/// renderer treats as word spaces.
fn split_line_tokens(line: &[Span]) -> Vec<Vec<Span>> {
    let mut tokens: Vec<Vec<Span>> = Vec::new();
    let mut cur: Vec<Span> = Vec::new();
    let mut prev_x: Option<f64> = None;
    let mut prev_advance = 0.0f64;
    for s in line {
        if let Some(px) = prev_x {
            let gap = s.x - px;
            let size = s.size.max(0.1);
            if gap - prev_advance > 0.65 * (0.25 * size) && !cur.is_empty() {
                tokens.push(std::mem::take(&mut cur));
            }
        }
        prev_x = Some(s.x);
        prev_advance = s.advance;
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
fn detect_scripts_cross_line(stream: &[Vec<Span>]) -> Vec<ScriptHit> {
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

/// Detect a built-up fraction WITHOUT an explicit rule, as a stacked pair of
/// short, similarly-sized glyph runs that are tightly stacked and horizontally
/// centred. Used when the producer draws no fraction bar (many simple formulas).
fn detect_stacked_fractions(stream: &[Vec<Span>]) -> Vec<FractionHit> {
    let mut used = vec![false; stream.len()];
    let mut hits = Vec::new();
    for i in 0..stream.len().saturating_sub(1) {
        if used[i] || stream[i].is_empty() {
            continue;
        }
        let j = i + 1;
        if used[j] || stream[j].is_empty() {
            continue;
        }
        let num = &stream[i];
        let den = &stream[j];
        let np = line_profile(num);
        let dp = line_profile(den);
        // Numerator above numerator, denominator below (stream is top-to-bottom).
        if np.baseline <= dp.baseline {
            continue;
        }
        let gap = np.baseline - dp.baseline;
        let max_size = np.size.max(dp.size).max(0.1);
        // Tight vertical stack (a fraction, not two prose lines).
        if !(0.6 * max_size..=2.2 * max_size).contains(&gap) {
            continue;
        }
        if (np.size - dp.size).abs() > 0.2 * max_size {
            continue;
        }
        let n_text = render_cell_plain(num);
        let d_text = render_cell_plain(den);
        if n_text.chars().count() > 6 || d_text.chars().count() > 6 {
            continue;
        }
        let (nx0, _, nx1, _) = line_bbox(num);
        let (dx0, _, dx1, _) = line_bbox(den);
        let n_center = (nx0 + nx1) / 2.0;
        let d_center = (dx0 + dx1) / 2.0;
        if (n_center - d_center).abs() > 0.5 * max_size {
            continue;
        }
        if overlap(nx0, nx1, dx0, dx1) < 0.4 * (nx1 - nx0).min(dx1 - dx0).max(0.1) {
            continue;
        }
        hits.push(FractionHit {
            numerator_line: i,
            denominator_line: j,
            expr: LatexExpr::fraction(LatexExpr::text(n_text), LatexExpr::text(d_text)),
        });
        used[i] = true;
        used[j] = true;
    }
    hits
}

/// Render a stream of visual lines with math synthesis, replacing the base
/// (or numerator) line with `$...$` and skipping the consumed script/denominator
/// line. Lines with no detected math fall back to `fallback(i, line)` so the
/// caller can keep the original text for plain content.
fn synthesize_stream_text_joined<F: Fn(usize, &[Span]) -> String>(
    stream: &[Vec<Span>],
    bars: &[RuleSeg],
    joiner: &str,
    fallback: F,
) -> String {
    let mut hits = detect_fractions(stream, bars);
    hits.extend(detect_stacked_fractions(stream));
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

#[cfg(test)]
mod tests {
    use super::*;

    fn span(text: &str, x: f64, y: f64, size: f64, advance: f64) -> Span {
        Span {
            text: text.to_string(),
            x,
            y,
            size,
            advance,
            is_bold: false,
            is_italic: false,
            is_underline: false,
            is_vertical: false,
        }
    }

    // --- AST serialization ---

    #[test]
    fn fraction_serializes_to_latex_frac() {
        let expr = LatexExpr::fraction(LatexExpr::text("a"), LatexExpr::text("b"));
        assert_eq!(expr.to_latex(), r"\frac{a}{b}");
        assert_eq!(expr.render_inline(), r"$\frac{a}{b}$");
    }

    #[test]
    fn superscript_serializes_to_caret() {
        let expr = LatexExpr::superscript(LatexExpr::text("x"), LatexExpr::text("2"));
        assert_eq!(expr.to_latex(), r"x^{2}");
        assert_eq!(expr.render_inline(), r"$x^{2}$");
    }

    #[test]
    fn subscript_serializes_to_underscore() {
        let expr = LatexExpr::subscript(LatexExpr::text("a"), LatexExpr::text("i"));
        assert_eq!(expr.to_latex(), r"a_{i}");
        assert_eq!(expr.render_inline(), r"$a_{i}$");
    }

    #[test]
    fn nested_sequence_serializes() {
        let expr = LatexExpr::seq(vec![
            LatexExpr::text("S"),
            LatexExpr::superscript(LatexExpr::text("2"), LatexExpr::text("3")),
            LatexExpr::text("+"),
            LatexExpr::fraction(LatexExpr::text("1"), LatexExpr::text("2")),
        ]);
        assert_eq!(expr.to_latex(), r"S2^{3}+\frac{1}{2}");
    }

    // --- Within-line super/subscript detection ---

    /// "x" baseline 700 at size 12, superscript "2" baseline 712 at size 8.
    #[test]
    fn detects_superscript_within_a_line() {
        let line = vec![
            span("x", 100.0, 700.0, 12.0, 8.0),
            span("2", 110.0, 712.0, 8.0, 5.0),
        ];
        let expr = synthesize_line_expr(&line);
        assert_eq!(expr.to_latex(), r"x^{2}");
        assert_eq!(render_math_line(&line), r"$x^{2}$");
    }

    /// "a" baseline 700, subscript "i" baseline 694.
    #[test]
    fn detects_subscript_within_a_line() {
        let line = vec![
            span("a", 100.0, 700.0, 12.0, 8.0),
            span("i", 108.0, 694.0, 8.0, 5.0),
        ];
        assert_eq!(synthesize_line_expr(&line).to_latex(), r"a_{i}");
    }

    /// Plain body text must remain byte-identical (no false positives).
    #[test]
    fn plain_line_remains_unchanged() {
        // Realistic body-word geometry: consecutive word starts stay within
        // 2.5*size of each other, so the legacy renderer keeps them on one line.
        let line = vec![
            span("Estim", 100.0, 700.0, 10.0, 20.0),
            span("caf", 124.0, 700.0, 10.0, 14.0),
            span("total", 146.0, 700.0, 10.0, 20.0),
        ];
        let rendered = render_math_line(&line);
        // The legacy renderer joins words with a single space.
        assert!(rendered.contains("Estim caf total"), "got {rendered}");
        assert!(!rendered.contains('$'), "got {rendered}");
    }

    /// A prose line carrying a footnote-style marker must NOT have its whole
    /// content swallowed into `$...$`: only the marker's immediate base word
    /// participates, surrounding words stay plain.
    #[test]
    fn script_attaches_to_immediate_word_not_whole_line() {
        // "See" and "note" share size 12 / baseline 700 (one cell); the raised
        // "1" (size 8, baseline 712) is a superscript attached to "note".
        let line = vec![
            span("See", 100.0, 700.0, 12.0, 32.0),
            span("note", 134.0, 700.0, 12.0, 40.0),
            span("1", 176.0, 712.0, 8.0, 5.0),
        ];
        let rendered = render_math_line(&line);
        assert!(
            !rendered.contains("$See"),
            "leading prose must stay plain, got {rendered}"
        );
        assert!(
            rendered.contains("$note^{1}$"),
            "only the base word is math-wrapped, got {rendered}"
        );
        assert!(rendered.starts_with("See"), "got {rendered}");
    }

    #[test]
    fn render_inline_mixed_leaves_trailing_prose_plain() {
        // A base+script followed by more prose: the math node is delimited but
        // the trailing text is left untouched.
        let expr = LatexExpr::seq(vec![
            LatexExpr::superscript(LatexExpr::text("mc"), LatexExpr::text("2")),
            LatexExpr::text(" meters"),
        ]);
        assert_eq!(expr.render_inline_mixed(), "$mc^{2}$ meters");
    }

    #[test]
    fn math_inline_for_line_none_for_plain_text() {
        let line = vec![
            span("Estim", 100.0, 700.0, 10.0, 20.0),
            span("caf", 124.0, 700.0, 10.0, 14.0),
        ];
        assert_eq!(math_inline_for_line(&line), None);
    }

    // --- Cross-line fraction detection ---

    #[test]
    fn detects_built_up_fraction_across_lines() {
        let stream = vec![
            // numerator "1" above the bar
            vec![span("1", 150.0, 715.0, 8.0, 5.0)],
            // fraction bar (y=708, x 150..170)
            vec![],
            // denominator "2" below the bar
            vec![span("2", 150.0, 701.0, 8.0, 5.0)],
        ];
        // The bar is supplied as a rule segment, not a span line.
        let bars: Vec<RuleSeg> = vec![(708.0, 150.0, 170.0)];
        let hits = detect_fractions(&stream, &bars);
        assert_eq!(hits.len(), 1, "must detect one fraction: {hits:?}");
        assert_eq!(hits[0].numerator_line, 0);
        assert_eq!(hits[0].denominator_line, 2);
        assert_eq!(hits[0].expr.to_latex(), r"\frac{1}{2}");
    }

    #[test]
    fn fraction_bar_far_away_is_rejected() {
        let stream = vec![
            vec![span("1", 150.0, 715.0, 8.0, 5.0)],
            vec![span("2", 150.0, 640.0, 8.0, 5.0)],
        ];
        // A bar that is not vertically between the two runs.
        let bars: Vec<RuleSeg> = vec![(700.0, 150.0, 170.0)];
        assert!(detect_fractions(&stream, &bars).is_empty());
    }

    #[test]
    fn render_math_merges_fraction_into_inline_delimiters() {
        let stream = vec![
            vec![span("1", 150.0, 715.0, 8.0, 5.0)],
            vec![span("2", 150.0, 701.0, 8.0, 5.0)],
        ];
        let bars: Vec<RuleSeg> = vec![(708.0, 150.0, 170.0)];
        let text = render_math(&stream, &bars, 842.0, true);
        assert!(text.contains(r"$\frac{1}{2}$"), "got {text}");
    }

    // --- Cross-line super/subscript + stacked fraction (LayoutAST/tagged) ---

    #[test]
    fn cross_line_superscript_is_lifted() {
        // Base "x" (size 12, baseline 700) with the superscript "2" (size 8,
        // higher baseline 712) emitted as a separate visual line.
        let spans = vec![
            span("x", 100.0, 700.0, 12.0, 8.0),
            span("2", 110.0, 712.0, 8.0, 5.0),
        ];
        let text = synthesize_spans_math(&spans, &[]);
        assert_eq!(text, r"$x^{2}$", "got {text}");
    }

    #[test]
    fn cross_line_subscript_is_lifted() {
        let spans = vec![
            span("a", 100.0, 700.0, 12.0, 8.0),
            span("i", 108.0, 694.0, 8.0, 5.0),
        ];
        let text = synthesize_spans_math(&spans, &[]);
        assert_eq!(text, r"$a_{i}$", "got {text}");
    }

    #[test]
    fn stacked_fraction_without_bar_is_lifted() {
        // Numerator above denominator, same small size, horizontally centred,
        // tight vertical stack -> bar-free fraction.
        let spans = vec![
            span("1", 150.0, 715.0, 8.0, 5.0),
            span("2", 150.0, 701.0, 8.0, 5.0),
        ];
        let text = synthesize_spans_math(&spans, &[]);
        assert_eq!(text, r"$\frac{1}{2}$", "got {text}");
    }

    #[test]
    fn synthesize_spans_math_preserves_plain_lines() {
        // "E = mc" on the base baseline with the superscript "2" on a higher
        // baseline just right of "mc": only the trailing token becomes math.
        let spans = vec![
            span("E", 100.0, 700.0, 10.0, 8.0),
            span("=", 110.0, 700.0, 10.0, 8.0),
            span("mc", 120.0, 700.0, 10.0, 12.0),
            span("2", 134.0, 712.0, 7.0, 5.0),
        ];
        let text = synthesize_spans_math(&spans, &[]);
        assert!(text.contains(r"$mc^{2}$"), "got {text}");
        assert!(text.contains("E ="), "leading text must be preserved: {text}");
    }
}
