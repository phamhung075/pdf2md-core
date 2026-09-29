// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Classification of the strip a drawn-rule frame excludes. A frame may bound
//! its table horizontally only when that strip is empty or a genuine text
//! column; a strip of table labels/values must stay inside the table. See
//! [`super`] for the table model.

use super::rows::cell_is_value;
use super::{Frame, EDGE_EPS_PT};
use crate::layout::glyph_stream::Span;
use crate::layout::tables::rulers::line_words;

/// A genuine prose strip averages at least this many words per line ...
const STRIP_MIN_WORDS_PER_LINE: f64 = 2.5;
/// ... and at most this fraction of its words may be bare values.
const STRIP_MAX_VALUE_FRACTION: f64 = 0.34;

/// What the words a frame excludes (outside its outer rules, inside its y-band)
/// look like.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum StripKind {
    /// Nothing excluded.
    Empty,
    /// A genuine text column: justified multi-word prose.
    Prose,
    /// Table labels, values, or a numeric column.
    Other,
}

/// Classify the excluded strip.
///
/// The re-cut may push such a strip out of the table only when it is prose:
/// several words per line, few bare values, no numbered-label line and no
/// bare-value line (those are a table's own label/value column). Anything else
/// means the frame does not enclose the whole table and the re-cut is refused.
pub(super) fn classify_excluded_strip(lines: &[Vec<Span>], frame: &Frame) -> StripKind {
    let left = frame.boundaries[0];
    let right = frame.boundaries[frame.ncols()];
    let mut rows: Vec<Vec<String>> = Vec::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let y = line[0].y;
        if !(frame.lo..=frame.hi).contains(&y) {
            continue;
        }
        let words: Vec<String> = line_words(line)
            .into_iter()
            .filter(|w| {
                let c = 0.5 * (w.x0 + w.x1);
                c < left - EDGE_EPS_PT || c > right + EDGE_EPS_PT
            })
            .flat_map(|w| {
                w.text
                    .split_whitespace()
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .collect();
        if !words.is_empty() {
            rows.push(words);
        }
    }
    if rows.is_empty() {
        return StripKind::Empty;
    }
    let total: usize = rows.iter().map(|r| r.len()).sum();
    let values = rows.iter().flatten().filter(|w| cell_is_value(w)).count();
    // Several words per line ...
    if (total as f64) < STRIP_MIN_WORDS_PER_LINE * rows.len() as f64 {
        return StripKind::Other;
    }
    // ... few bare values ...
    if (values as f64) > STRIP_MAX_VALUE_FRACTION * total as f64 {
        return StripKind::Other;
    }
    // ... and no line that is a numbered label or a bare value. A justified
    // prose line starts with a word; "13 - Bases exonérées" and "6,00" are a
    // table's own cells and must stay inside the frame.
    for r in &rows {
        if r.is_empty() {
            continue;
        }
        if r.iter().all(|w| cell_is_value(w)) {
            return StripKind::Other;
        }
        if r[0]
            .chars()
            .next()
            .map(|c| c.is_ascii_digit())
            .unwrap_or(false)
        {
            return StripKind::Other;
        }
    }
    StripKind::Prose
}
