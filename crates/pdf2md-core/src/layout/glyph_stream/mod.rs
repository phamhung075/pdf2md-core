// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! PDF content-stream glyph extraction, font metric resolution, and 2D span aggregation.

mod matrix;
pub(crate) use matrix::*;
mod widths;
pub(crate) use widths::*;
mod fonts;
pub(crate) use fonts::*;
mod page_geometry;
pub use page_geometry::*;
mod zones;
use zones::*;
mod walk;
use walk::*;
mod signals;
pub(crate) use signals::*;
mod extract;
pub use extract::*;

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

#[derive(Clone)]
pub(crate) enum Widths {
    /// Simple font: byte code -> width in 1/1000 em (`/Widths` + `/FirstChar`).
    Byte([f64; 256]),
    /// Type3 simple font: byte code -> width in 1/1000 em, already mapped from
    /// the font's glyph space through its `/FontMatrix` (PDF 32000-1 §9.6.5).
    ///
    /// A Type3 font defines its own glyph space, so the `Tf` size operand is
    /// not an em either; `em_scale` carries the real em the glyphs are set at
    /// (see `type3_em_scale`) so every gap threshold derived from a run's
    /// `size` is measured in real points.
    ByteType3 {
        table: [f64; 256],
        em_scale: f64,
    },
    /// Type0/CID font: CID -> width (`/W` + `/DW`), with an optional code->CID
    /// CMap (Identity-H/V uses None: the code bytes *are* the CID).
    Cid {
        map: HashMap<u32, f64>,
        default: f64,
        encoding: Option<CMapCodec>,
    },
    /// No usable metrics (e.g. base-14 fonts without embedded widths).
    None,
}

#[derive(Clone, Debug)]
pub struct Span {
    pub text: String,
    /// Device-space origin x of the run, in PDF points with a **bottom-left
    /// origin (PDF user space, y grows upward)**, after the page's initial CTM.
    pub x: f64,
    /// Device-space origin y of the run, in PDF points with a **bottom-left
    /// origin (PDF user space, y grows upward)**, after the page's initial CTM.
    pub y: f64,
    /// Effective device font size (used to scale all gap thresholds).
    pub size: f64,
    /// Total natural advance width of the decoded run, in device points.
    pub advance: f64,
    /// Advance a word-gap test must subtract to find the *whitespace* after this
    /// run: `advance` plus the character/word spacing (`Tc`/`Tw`) the content
    /// stream adds after every code. The text matrix advances by a run's glyph
    /// widths *plus* its tracking, but a producer's `Tc` (letter-spacing) is
    /// deliberately applied after each code; measuring the residual gap against
    /// the bare `advance` therefore turns the accumulated tracking of a long
    /// run into a phantom word space when the next run continues the same word
    /// at a `TJ` kerning point. Geometry (columns, tables) keeps the bare
    /// `advance`; only word-gap rendering uses this field.
    pub word_advance: f64,
    pub is_bold: bool,
    pub is_italic: bool,
    pub is_underline: bool,
    pub is_vertical: bool,
}

/// A font resolved for one content-stream resource chain: its resource name,
/// decoder, advance widths, and bold/italic style bits.
#[derive(Clone)]
struct GlyphFontInfo {
    name: Vec<u8>,
    codec: Codec,
    widths: Widths,
    style: (bool, bool),
}

/// Work / recursion bounds for Form XObject expansion in the glyph engine. A
/// form graph is a DAG at best and can be a near-exponential tree; each bound
/// below independently caps the blow-up. The decoded-byte and operator budgets
/// are shared across the whole page, so a DAG of forms each issuing `Do` many
/// times cannot multiply work.
const MAX_FORM_DEPTH: usize = 8;
/// Maximum number of `Do` invocations expanded for one page (16384 = 2^14).
///
/// Rationale: a per-page cost backstop, not a content limit. Every invocation
/// is also charged `text_extract::FORM_INVOCATION_OPS` against the shared
/// `MAX_FORM_OPS_TOTAL` (8 M-operator) budget and its decoded bytes against
/// `MAX_FORM_BYTES_TOTAL` (64 MiB), so an op-heavy or byte-heavy fan-out is
/// bounded by those budgets; a self-referential or exponentially expanding
/// form DAG is bounded by `MAX_FORM_DEPTH` plus the per-path cycle check, and
/// therefore never reaches this count. The cap only stops a pathological page
/// of > 16384 trivial forms; when it (or any other bound) trips,
/// [`GlyphBudget::exhausted`] is set so the truncation is reported as
/// `budget_exhausted` instead of being silent.
const MAX_FORM_DO_PER_PAGE: usize = 16384;
const MAX_FORM_OPS_TOTAL: usize = 8_000_000;
const MAX_FORM_BYTES_TOTAL: usize = 64 << 20;
/// Maximum number of positioned text spans accumulated for one page.
///
/// A page's spans are the dominant per-page allocation of the glyph engine, and
/// a hostile content stream can pack one `Tj` per handful of decoded bytes: a
/// 35 KB flate stream expands to 14 MB and ~650 k spans, reaching 1.9 GB RSS.
/// This is a per-page cost backstop, not a content limit. The largest single-page
/// span count measured across the 811-document corpus is 8 131, so 64 000 leaves
/// roughly 8x headroom for any real page (dense multi-thousand-page documents
/// included — pages are walked one at a time). When the cap trips, the walk
/// stops adding spans and sets [`GlyphBudget::exhausted`] so the truncation is
/// reported as `budget_exhausted` instead of being silent.
const MAX_GLYPH_SPANS_PER_PAGE: usize = 64_000;

struct GlyphBudget {
    do_left: usize,
    ops_left: usize,
    bytes_left: usize,
    /// Spans still allowed on this page; decremented as runs are pushed.
    spans_left: usize,
    /// Set when any bound above tripped, so a truncated page is reported.
    exhausted: bool,
}

/// One content-stream walk's output: positioned spans, the thin horizontal
/// underline rules, the thin vertical column rules, and the character counts of
/// vertical runs by reading direction (upward / downward). The direction counts
/// are what let the page-level caller detect a dominantly vertical page and
/// rotate it upright.
struct GlyphWalk {
    spans: Vec<Span>,
    underline_segs: Vec<(f64, f64, f64)>,
    vertical_segs: Vec<(f64, f64, f64)>,
    /// Drawn axis-aligned cell rectangles `(x0, y0, x1, y1)`, the raw material
    /// for a grid whose columns are stated per cell rather than by long rules.
    cell_rects: Vec<(f64, f64, f64, f64)>,
    vertical_up_chars: usize,
    vertical_down_chars: usize,
}

/// Text-show / text-positioning operators seen on a page, including inside its
/// Form XObjects.
///
/// `text_extract::extract_page`'s legacy routing inspects only the *page's own*
/// content stream, so a page whose text lives entirely in a form
/// (`/Fm0 Do`, e.g. a corpus file) is never routed to the glyph engine and
/// gets no table recovery. Callers can OR these form-aware signals into that
/// routing decision. The scan is bounded exactly like `walk_glyphs`.
#[derive(Default, Clone, Copy, Debug)]
#[allow(dead_code)]
pub(crate) struct ContentSignals {
    pub has_tj_array: bool,
    pub has_tj_plain: bool,
    pub has_quote: bool,
    pub has_td_upper: bool,
    pub has_tm: bool,
    /// Number of `Td` operators seen anywhere.
    pub td_total: usize,
    /// Of `td_total`, how many carry a horizontal component.
    pub td_horizontal: usize,
    /// True when the bounded signal scan stopped early (operator, `Do` or byte
    /// budget), so the routing decision may be based on incomplete signals.
    pub budget_exhausted: bool,
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_common;
#[cfg(test)]
mod tests_part2;
#[cfg(test)]
mod tests_part3;
#[cfg(test)]
mod span_api_tests;
#[cfg(test)]
mod type3_metrics_tests;
