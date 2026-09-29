// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Row/cell construction for a drawn-rule frame: bucket a line's words onto the
//! frame's columns and recognise a header band. See [`super`] for the table
//! model.

use super::super::ledger::column_of;
use super::super::rulers::WordTok;
use super::{EDGE_EPS_PT, HEADER_MAX_CELL_WORDS, MIN_HEADER_COLS};

/// Is `cell` a bare value (number / amount / percent) rather than a label?
pub(super) fn cell_is_value(cell: &str) -> bool {
    let t = cell
        .trim()
        .trim_matches(|c: char| matches!(c, '€' | '$' | '£' | '\u{00a0}'));
    if t.is_empty() {
        return false;
    }
    let mut has_digit = false;
    for ch in t.chars() {
        if ch.is_ascii_digit() {
            has_digit = true;
        } else if !matches!(
            ch,
            '.' | ',' | '-' | '+' | '%' | '/' | '\'' | ' ' | '\u{2028}'
        ) {
            return false;
        }
    }
    has_digit
}

/// A run of words whose inter-word gaps are all ordinary word spaces (never a
/// column gutter) is one text run: a label the producer let cross a drawn column
/// rule must stay whole in the cell where it starts, not be snipped in two.
const MAX_INLINE_GAP_EM: f64 = 0.5;

/// Assign a line's words to the frame's columns. A word that lies *entirely*
/// outside the frame's outer rules belongs to the surrounding flow (a sidebar,
/// the page margin) and is dropped; a word that straddles an outer rule is kept
/// in the nearest column, because `render_with_tables` treats any span
/// overlapping the table's bbox as table content — dropping it here would
/// silently delete it from both the table and the recovered side flow.
pub(super) fn build_row(words: &[WordTok], size: f64, boundaries: &[f64]) -> Vec<String> {
    let ncols = boundaries.len() - 1;
    let left = boundaries[0];
    let right = boundaries[boundaries.len() - 1];
    // `column_of` takes the *interior* boundaries only; the outer rules are
    // explicit edges.
    let interior = &boundaries[1..boundaries.len() - 1];
    let mut cells = vec![String::new(); ncols];

    // One contiguous run (word-space gaps only) is a single cell, anchored at
    // the column where the run starts. A genuine multi-column row always leaves
    // at least one gutter, so this never merges two data columns.
    let mut run: Vec<&WordTok> = words
        .iter()
        .filter(|w| w.x1 > left + EDGE_EPS_PT && w.x0 < right - EDGE_EPS_PT)
        .collect();
    if run.len() >= 2 {
        run.sort_by(|a, b| a.x0.partial_cmp(&b.x0).unwrap_or(std::cmp::Ordering::Equal));
        let contiguous = run
            .windows(2)
            .all(|p| (p[1].x0 - p[0].x1) <= MAX_INLINE_GAP_EM * size.max(0.1));
        if contiguous {
            let center = 0.5 * (run[0].x0 + run[0].x1);
            let c = column_of(center.clamp(left, right), interior).min(ncols - 1);
            let text = run
                .iter()
                .map(|w| w.text.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            push_cell(&mut cells, c, &text);
            return cells;
        }
    }

    for w in words {
        if w.x1 <= left + EDGE_EPS_PT || w.x0 >= right - EDGE_EPS_PT {
            continue;
        }
        // A producer frequently draws two right-aligned numbers in adjacent
        // spans whose advances abut, so `line_words` folds them into one token
        // ("##.#### ####.##"). The drawn rule between them is the authority: a
        // token whose whitespace-separated parts are all bare values is split
        // and each part bucketed by its own interpolated centre. A *label*
        // phrase that merely crosses a rule (an address, a section title) is not
        // split: it stays whole in the cell where it starts.
        if interior
            .iter()
            .any(|&b| b > w.x0 + EDGE_EPS_PT && b < w.x1 - EDGE_EPS_PT)
            && w.text.contains(' ')
        {
            let parts: Vec<&str> = w.text.split_whitespace().collect();
            if parts.len() >= 2 && parts.iter().all(|p| cell_is_value(p)) {
                for (col, part) in split_crossing_token(w, interior) {
                    push_cell(&mut cells, col, &part);
                }
                continue;
            }
            let c = column_of(w.x0.clamp(left, right), interior).min(ncols - 1);
            push_cell(&mut cells, c, w.text.as_str());
            continue;
        }
        let center = 0.5 * (w.x0 + w.x1);
        let c = column_of(center.clamp(left, right), interior).min(ncols - 1);
        push_cell(&mut cells, c, w.text.as_str());
    }
    cells
}

/// Split a word token that crosses an interior rule at its whitespace, assigning
/// each whitespace-separated part a column from its x-position interpolated
/// linearly across the token (digit runs are near-uniform width, so the split
/// lands on the rule). A part past the frame's right rule is dropped.
fn split_crossing_token(w: &WordTok, interior: &[f64]) -> Vec<(usize, String)> {
    let text = w.text.as_str();
    let char_count = text.chars().count().max(1) as f64;
    let total = (w.x1 - w.x0).max(0.1);
    let mut out = Vec::new();
    let mut search_from = 0usize;
    for part in text.split_whitespace() {
        let Some(idx) = text[search_from..].find(part).map(|i| i + search_from) else {
            search_from += part.len();
            continue;
        };
        let c0 = text[..idx].chars().count() as f64;
        let c1 = c0 + part.chars().count() as f64;
        let x0 = w.x0 + total * (c0 / char_count);
        let x1 = w.x0 + total * (c1 / char_count);
        search_from = idx + part.len();
        let col = column_of(0.5 * (x0 + x1), interior);
        out.push((col, part.to_string()));
    }
    out
}

/// Append `text` to `cells[col]`, space-separating multiple words in one cell.
fn push_cell(cells: &mut [String], col: usize, text: &str) {
    if col >= cells.len() || text.trim().is_empty() {
        return;
    }
    if !cells[col].is_empty() {
        cells[col].push(' ');
    }
    cells[col].push_str(text.trim());
}

/// Number of non-empty cells.
pub(super) fn populated(cells: &[String]) -> usize {
    cells.iter().filter(|c| !c.trim().is_empty()).count()
}

/// Is `row` the shape of a header band line: a short label in several columns
/// and no bare value cell?
pub(super) fn is_header_like(row: &[String]) -> bool {
    populated(row) >= MIN_HEADER_COLS
        && !row.iter().any(|c| cell_is_value(c))
        && row
            .iter()
            .all(|c| c.is_empty() || c.split_whitespace().count() <= HEADER_MAX_CELL_WORDS)
}
