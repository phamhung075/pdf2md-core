// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Ledger row classification: the opening-balance line drawn above the column
//! header.

use super::ledger::{column_of, ColumnKind, HeaderColumn};
use super::rulers::{line_words, WordTok};
use crate::layout::glyph_stream::Span;

/// True when a cell reads as a money value: digits plus the punctuation a
/// locale uses for grouping, decimals, sign and currency, and at least one
/// digit. A description never passes, so a label can never be mistaken for an
/// amount.
pub(super) fn amount_like(text: &str) -> bool {
    let mut digit = false;
    for c in text.chars() {
        if c.is_ascii_digit() {
            digit = true;
            continue;
        }
        if c.is_whitespace() || matches!(c, '.' | ',' | '-' | '+' | '\'' | '€' | '$' | '£' | '(' | ')') {
            continue;
        }
        return false;
    }
    digit
}

/// Classify the line directly above the ledger header as an opening balance.
///
/// The shape is a single description label plus a single amount (a label cell
/// with no date/value cell), which is how a statement prints the balance the
/// ledger opens with. A summary line carrying several amounts — the monthly
/// totals some accounts print above the header — does not match, so it stays
/// outside the table.
pub(super) fn opening_balance_row(
    line: &[Span],
    header: &[HeaderColumn],
    boundaries: &[f64],
) -> Option<Vec<String>> {
    let words: Vec<WordTok> = line_words(line);
    if words.is_empty() {
        return None;
    }
    let ncols = header.len();
    let mut cells = vec![String::new(); ncols];
    for w in &words {
        let c = column_of(0.5 * (w.x0 + w.x1), boundaries);
        if c >= ncols {
            return None;
        }
        if !cells[c].is_empty() {
            cells[c].push(' ');
        }
        cells[c].push_str(&w.text);
    }
    let mut labels = 0usize;
    let mut amounts = 0usize;
    let mut filled = 0usize;
    for (c, cell) in cells.iter().enumerate() {
        if cell.is_empty() {
            continue;
        }
        filled += 1;
        match header[c].kind {
            ColumnKind::Date | ColumnKind::Value => return None,
            ColumnKind::Text => {
                if !cell.chars().any(|ch| ch.is_alphabetic()) {
                    return None;
                }
                labels += 1;
            }
            ColumnKind::Amount => {
                if !amount_like(cell) {
                    return None;
                }
                amounts += 1;
            }
        }
    }
    (filled == 2 && labels == 1 && amounts == 1).then_some(cells)
}
