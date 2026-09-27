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

mod cmap;
pub(crate) use cmap::*;
mod decode;
use decode::*;
mod names;
pub(crate) use names::*;
mod encoding;
use encoding::*;
mod font_codec;
pub(crate) use font_codec::*;
mod text_pos;
use text_pos::*;
mod postprocess;
pub(crate) use postprocess::*;
mod walk_content;
use walk_content::*;
mod page;
pub use page::*;
mod content_sanitize;
use content_sanitize::*;
mod content_bound;
use content_bound::*;

use std::collections::{BTreeMap, HashMap};

use lopdf::content::{Content, Operation};
use lopdf::{Dictionary, Document, Object, ObjectId};

use crate::glyph_data::{
    pua_family_for_base_font, pua_to_char_for_family, AGL_NAMES, MAC_ROMAN, WIN_ANSI, PuaFamily,
};
use crate::layout::glyph_stream::{resolve_widths, Widths};
use crate::layout::reading_order::WORD_GAP_EM;

// ---------------------------------------------------------------------------
// Codecs
// ---------------------------------------------------------------------------

/// Decoded byte-oriented (8-bit simple font) encoding table.
/// `0` means "this char code has no Unicode mapping" (skip).
#[derive(Clone, Copy)]
pub(crate) struct ByteTable([u16; 256]);

/// A parsed `/ToUnicode` CMap: 1-4 byte source codes -> UTF-16 destinations.
#[derive(Clone, Default)]
pub(crate) struct CMapCodec {
    /// Exact single mappings: (source code length in bytes, source code) -> UTF-16.
    exact: HashMap<(u8, u32), Vec<u16>>,
    /// bfrange with a single incrementing destination: (len, lo, hi, dst_lo);
    /// destination for code = dst_lo + (code - lo).
    ranges: Vec<(u8, u32, u32, u32)>,
}

#[derive(Clone)]
pub(crate) enum Codec {
    Byte8(ByteTable, PuaFamily),
    /// ToUnicode CMap, with an optional byte-table fallback for simple fonts
    /// whose CMap does not cover the actual char codes used on the page.
    CMap(CMapCodec, Option<ByteTable>, PuaFamily),
}

enum Tok {
    Hex(Vec<u8>), // hex nibble characters
    Word(String),
}

/// Advance assumed for a glyph when the font has no usable `/Widths` (the
/// geometry engine assumes the same typical letter advance).
const FALLBACK_ADVANCE_EM: f64 = 0.5;

/// Text-space position tracker for the string walker.
///
/// Producers that position every word or fragment with a horizontal `Td` (and
/// no `TJ` array) encode word boundaries as gaps in the text matrix, not as
/// space glyphs. The walker previously ignored those positions, so every word
/// on such a line fused into one token. Tracking the text-space x/y lets
/// [`walker_gap`] insert a separator exactly when the next show starts more
/// than [`WORD_GAP_EM`] em past the previous show's natural end.
#[derive(Clone, Copy)]
struct TextPos {
    line_x: f64,
    line_y: f64,
    cur_x: f64,
    cur_y: f64,
    /// End x of the previous show on the current line (natural advance, before
    /// any separator the walker inserted). Cleared by a real line advance, so
    /// it is only used for word-gap inference.
    prev_end_x: Option<f64>,
    /// End x of the previous show, kept across line advances. The
    /// paragraph-break heuristic needs to know whether the next run continues
    /// the line the previous run drew, even when the next run is placed by a
    /// fresh `Td`/`Tm`.
    last_end_x: Option<f64>,
    /// Font size of the previous show, used to tell a super/subscript (a
    /// smaller face a few points up) from a same-size new column start.
    last_show_size: f64,
    /// Baseline y of the previous show.
    prev_y: f64,
    /// Baseline y of the last text show, across visual lines. A later show on a
    /// baseline more than one em *above* this one starts a new column/block, so
    /// the walker emits a paragraph break there (PDF y grows upward).
    last_show_y: Option<f64>,
    /// Text-space start x of the line the last show was on. A later line that
    /// starts well to the left/right of it *and* sits above the previous
    /// baseline is a new column even when the vertical jump is under one em.
    last_line_x: f64,
    size: f64,
    /// `Tz / 100`: horizontal glyph scaling.
    hscale: f64,
    /// `Tc` / `Tw`: character / word spacing, in text-space points.
    char_sp: f64,
    word_sp: f64,
    /// `TL`: leading used by `T*`.
    leading: f64,
    /// A `Tm` with a non-identity scale or rotation leaves text space
    /// unaligned with device points; the walker cannot compare advances then,
    /// so it stops inserting gaps for the rest of this content stream.
    unusable: bool,
}

/// Page text result plus whether the page's content stream contains any
/// text-show operators (used to detect glyph-encoded / outlined documents
/// whose text cannot be recovered, so the caller can ask for OCR), plus how
/// many grid tables the geometry engine recovered on the page.
pub struct PageText {
    pub text: String,
    pub text_ops_seen: bool,
    pub has_fonts: bool,
    pub tables: usize,
    /// Structured human-reading-order blocks (geometry path only; empty on
    /// string-walker pages).
    pub blocks: Vec<crate::layout::DocBlock>,
    /// True when any extraction work bound (Form XObject `Do` count, shared
    /// operator/decoded-byte budget, recursion depth) tripped for this page, so
    /// the text may be truncated. See [`WalkerBudget`].
    pub budget_exhausted: bool,
}

/// Work / recursion bounds for Form XObject expansion in the string walker.
///
/// A form graph is a DAG at best and can be a near-exponential tree; each bound
/// below independently caps the blow-up, mirroring the glyph engine's
/// `GlyphBudget` (`layout::glyph_stream`) so both engines bound the same shape
/// identically. The `Do` and decoded-byte budgets are shared across the whole
/// page, so a DAG of forms that each issue `Do` many times cannot multiply the
/// work: when a budget is exhausted the walker simply stops descending and the
/// conversion continues with the text decoded so far (never an error).
pub(crate) const MAX_WALKER_FORM_DEPTH: usize = 8;
/// Maximum number of `Do` invocations expanded for one page (16384 = 2^14).
///
/// Rationale: this is a per-page *cost* backstop, not a content limit. Each
/// invocation is charged [`FORM_INVOCATION_OPS`] against the shared
/// [`MAX_WALKER_OPS_TOTAL`] (8 M-operator) budget, so 16384 tiny forms cost at
/// most ~0.5 M ops — comfortably inside the budget, which is why a real page
/// with a few thousand form placements (the qa-int2 `forms_distinct_*`
/// fixtures) converts in full. An op-heavy or byte-heavy fan-out is bounded by
/// the shared operator and `MAX_WALKER_BYTES_TOTAL` (64 MiB decoded) budgets
/// instead, and a self-referential or exponentially expanding form DAG is
/// independently bounded by [`MAX_WALKER_FORM_DEPTH`] plus the per-path cycle
/// check, so those shapes never reach this count in the first place. The cap
/// exists only to stop a pathological page of > 16384 trivial forms from
/// paying the per-invocation resource-chain cost without bound; when it (or any
/// other bound) trips, [`WalkerBudget::exhausted`] is set and the conversion
/// reports `budget_exhausted` rather than truncating silently.
pub(crate) const MAX_WALKER_DO_PER_PAGE: usize = 16384;
pub(crate) const MAX_WALKER_OPS_TOTAL: usize = 8_000_000;
pub(crate) const MAX_WALKER_BYTES_TOTAL: usize = 64 << 20;
/// Fixed operator-budget cost charged on top of a form's own decoded bytes and
/// operators for every `Do` invocation. Re-walking a form is not proportional
/// to its byte length: the walker resolves the resource chain and collects the
/// form's fonts on each visit. Charging that overhead makes the budget a true
/// cost measure, so a hostile fan-out that reuses a tiny form exhausts the
/// shared budget long before the invocation-count cap.
pub(crate) const FORM_INVOCATION_OPS: usize = 32;

/// Per-page work budget shared by every nested [`walk_content`] call, and by
/// the `page_has_text_layer`/`content_has_text_layer` probe in `lib.rs`.
pub(crate) struct WalkerBudget {
    pub(crate) do_left: usize,
    pub(crate) ops_left: usize,
    pub(crate) bytes_left: usize,
    /// Set when any bound above tripped, so a truncated page can be reported
    /// (as `budget_exhausted`) instead of failing silently.
    pub(crate) exhausted: bool,
}

/// Largest decoded size of a single page content stream (32 MiB). A content
/// stream is a short list of drawing/text operators — orders of magnitude
/// smaller in any real document — so a stream that inflates past this is a
/// decompression bomb, not content we can use.
pub(crate) const MAX_PAGE_CONTENT_STREAM: usize = 32 << 20;
/// Largest total decoded content of one page (64 MiB), summed over its streams.
pub(crate) const MAX_PAGE_CONTENT_TOTAL: usize = 64 << 20;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_common;
#[cfg(test)]
mod tests_part2;
