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

mod inline;
pub use inline::*;
mod fraction;
pub use fraction::*;
mod stream;
pub use stream::*;

use serde::{Deserialize, Serialize};

use crate::layout::glyph_stream::Span;
use crate::layout::reading_order::{render_spans, WORD_GAP_EM};

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
            LatexExpr::Sequence(parts) => {
                // Parts are word-sized units (`synthesize_line_expr` splits a
                // prose run into its word tokens before attaching a script to
                // the last one), so a bare `collect()` glues them together:
                // `comparestheperformanceof…`. Join with a single space unless
                // the junction already carries whitespace.
                let mut out = String::new();
                for p in parts {
                    let s = p.render_inline_mixed();
                    if s.is_empty() {
                        continue;
                    }
                    // A script with an empty base (`$^{1}$` / `$_{i}$`) attaches
                    // to the preceding word with no intervening space, and a
                    // closing punctuation run (`.`, `,`, …) never gets a space
                    // pushed in front of it.
                    let attached_script = s.starts_with("$^") || s.starts_with("$_");
                    let leading_punct = s
                        .chars()
                        .next()
                        .map_or(false, |c| matches!(c, '.' | ',' | '!' | '?' | ';' | ':' | ')' | ']'));
                    let needs_space = !out.is_empty()
                        && !out.chars().last().map_or(true, |c| c.is_whitespace())
                        && !s.chars().next().map_or(true, |c| c.is_whitespace())
                        && !attached_script
                        && !leading_punct;
                    if needs_space {
                        out.push(' ');
                    }
                    out.push_str(&s);
                }
                out
            }
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

/// A super/subscript run attached to a base run within one visual line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScriptKind {
    Superscript,
    Subscript,
}

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

/// A script must be meaningfully smaller than its base: `size < base * RATIO`.
const SCRIPT_SIZE_RATIO: f64 = 0.92;
/// A script must be vertically offset from its base by at least this fraction
/// of the base size.
const SCRIPT_OFFSET_MIN: f64 = 0.12;

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

#[cfg(test)]
mod tests;
