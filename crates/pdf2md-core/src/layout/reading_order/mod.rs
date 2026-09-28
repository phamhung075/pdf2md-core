// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Reading order recovery, multi-column stream separation, and structured DocBlock generation.

mod columns;
pub use columns::*;
mod projection;
pub use projection::*;
mod multi_column;
use multi_column::*;
mod read_order;
pub use read_order::*;
mod band_metrics;
use band_metrics::*;
mod bands;
pub use bands::*;
mod staggered;
use staggered::*;
mod zones;
use zones::*;
mod lines;
pub use lines::*;
mod page_numbers;
pub use page_numbers::*;
mod render;
pub use render::*;
mod body_size;
pub(crate) use body_size::*;
mod structure;
pub(crate) use structure::*;
mod doc_blocks;
pub use doc_blocks::*;

use serde::{Deserialize, Serialize};
use crate::layout::glyph_stream::Span;
use crate::reflow::{classify_hyphen_join, HyphenJoin};

/// Fraction of an em by which the next run must start past the previous run's
/// natural end (its `word_advance`, `Tc`/`Tw` included) to count as a word
/// separator.
///
/// Typography: a normal space is 0.25-0.33 em, a thin space ~0.20 em, and
/// inter-letter tracking/kerning is normally under 0.15 em. The previous
/// `0.65 * 0.25 = 0.1625` em threshold sat inside the kerning band, so normal
/// justified letter-spacing jitter crossed it and split words (`e xportateur`,
/// `fr ontière`, `semes tre`: D3). `0.19` em keeps a real (if tight) word gap
/// while clearing the tracking band. The three word-gap rendering sites (glyph
/// renderer, string walker, math/cell renderer) share this constant so they
/// agree on word boundaries.
pub(crate) const WORD_GAP_EM: f64 = 0.1625;

/// One structured block (reading unit) with a semantic role.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocBlock {
    /// 1-based page number (filled by the caller).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub page: usize,
    pub kind: String,
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
    pub text: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub is_bold: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub is_italic: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub is_underline: bool,
}

fn is_false(v: &bool) -> bool {
    !*v
}

fn is_zero(v: &usize) -> bool {
    *v == 0
}

/// Detect a genuine two-column page: >= 3 rows split at a *consistent* gutter
/// x. Returns the reading-order column streams (left column top-to-bottom,
/// right column top-to-bottom) when stable, else None (single column).
pub struct PageColumns {
    pub top_full: Vec<Vec<Span>>,
    pub left: Vec<Vec<Span>>,
    pub right: Vec<Vec<Span>>,
    pub bottom_full: Vec<Vec<Span>>,
}

/// One contiguous block of a page's reading order, as recovered by
/// `detect_column_bands`.
pub enum ColumnBand {
    /// Full-width lines in their natural top-to-bottom order.
    Full(Vec<Vec<Span>>),
    /// A genuine two-column block, already resolved into independent
    /// top-to-bottom streams for the left and right sides.
    Columns {
        left: Vec<Vec<Span>>,
        right: Vec<Vec<Span>>,
    },
    /// Three or more *staggered* side-by-side columns (unlike
    /// [`ColumnBand::Columns`], which is always the fixed two-column pair),
    /// already resolved into independent top-to-bottom streams ordered
    /// left-to-right.
    ///
    /// A staggered block has no gutter that runs across the whole region: each
    /// column keeps its own baselines, so a row in one column sits between two
    /// rows of a neighbour. The running-gutter pass needs a gutter consistent
    /// across several rows and the vertical projection needs a corridor clear
    /// on both sides, so neither can see the block; the rows stay `Full` and
    /// the plain top-to-bottom order threads one column's line between another
    /// column's pair. A bilingual heading header is the motivating shape.
    Stacks(Vec<Vec<Vec<Span>>>),
}

/// A page region recovered as 3+ independent vertical columns by
/// [`multi_column_projection`].
struct MultiColumnRegion {
    /// First and last line index (into the page's `lines`) of the region.
    start: usize,
    end: usize,
    /// One line-stream per column, left to right.
    columns: Vec<Vec<Vec<Span>>>,
}

/// Inline text style of one span run. A span is drawn entirely in one font so
/// its bold/italic/underline state is uniform.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
struct InlineStyle {
    bold: bool,
    italic: bool,
    underline: bool,
}

/// One visual line's structural role.
#[derive(Debug)]
pub(crate) enum LineRole {
    Heading(u8),
    List { depth: u8, ordered: bool, ordinal: usize },
    Body,
}

/// Geometry of a heading candidate accepted for one visual line, remembered so
/// the line directly under it can be tested as its bilingual translation
/// (`detect_heading_level` alone cannot see the line above).
#[derive(Clone, Copy)]
pub(crate) struct HeadingLine {
    pub level: u8,
    pub size: f64,
    pub x0: f64,
    pub x1: f64,
    pub y: f64,
}

/// Per-render-pass state so consecutive list items share one indent anchor
/// and ordered items number consecutively; resets whenever a heading or a
/// non-list body line breaks the run (mirrors normal Markdown list
/// semantics: a blank/prose line ends the list).
#[derive(Default)]
pub(crate) struct ListRunState {
    base_x: Option<f64>,
    counters: Vec<usize>,
    /// The heading accepted for the immediately preceding visual line, cleared
    /// by any body/list line so only a directly-adjacent translation matches.
    last_heading: Option<HeadingLine>,
}

#[cfg(test)]
mod tests;


#[cfg(test)]
mod structural_tests;


#[cfg(test)]
mod paragraph_merge_tests;


#[cfg(test)]
mod column_band_tests;
#[cfg(test)]
mod column_band_tests_common;
#[cfg(test)]
mod column_band_tests_part2;
#[cfg(test)]
mod structural_tests_common;
#[cfg(test)]
mod structural_tests_part2;

#[cfg(test)]
mod zone_tests;
