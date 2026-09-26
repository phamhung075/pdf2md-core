// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Stage 3 & Stage 3b ruler scanning, grid alignment, and column corridor analysis.

use super::*;

/// A detected table: line range [start, end] (inclusive) plus cell rows.
#[derive(Debug, Clone)]
pub struct TableHit {
    pub start: usize,
    pub end: usize,
    pub rows: Vec<Vec<String>>,
    pub bbox: BoundingBox,
}

/// One decoded word with its device start x and end x (the column-start ruler).
#[derive(Debug, Clone)]
pub struct WordTok {
    pub text: String,
    pub x0: f64,
    pub x1: f64,
}

/// Word tokens + sorted start/end positions per visual line. `ends` (word
/// `x1`) exists alongside `starts` (word `x0`) so a column can be recognized
/// by either its left edge (ordinary left-aligned text: descriptions, names)
/// or its right edge (right-aligned numeric columns: quantities, unit prices,
/// Montant HT/TVA/TTC) — see `table_rulers`'s doc comment.
#[derive(Debug, Clone)]
pub struct RowInfo {
    pub words: Vec<WordTok>,
    pub starts: Vec<f64>,
    pub ends: Vec<f64>,
    pub size: f64,
}

/// Split one visual line into words (same gap rules as the renderer).
pub fn line_words(line: &[Span]) -> Vec<WordTok> {
    let mut out: Vec<WordTok> = Vec::new();
    let mut text = String::new();
    let mut x0: Option<f64> = None;
    let mut prev_x: Option<f64> = None;
    let mut prev_advance = 0.0f64;
    let mut last_end = 0.0f64;

    let flush = |out: &mut Vec<WordTok>, text: &mut String, x0: &mut Option<f64>, last_end: f64| {
        if let Some(s) = x0.take() {
            if !text.trim().is_empty() {
                out.push(WordTok {
                    text: std::mem::take(text).trim().to_string(),
                    x0: s,
                    x1: last_end,
                });
            }
            text.clear();
        }
    };

    for sp in line {
        if sp.text.is_empty() {
            continue;
        }
        let size = sp.size.max(0.1);
        let space_adv = 0.25 * size;
        let is_space = sp.text.chars().all(|c| c == ' ');
        if let Some(px) = prev_x {
            // Measure the *residual* whitespace between the two spans
            // (start-to-start distance minus the previous span's own advance),
            // exactly as `reading_order::render_spans` and `split_hard_breaks`
            // do. Comparing the raw start-to-start distance against the 2.5em
            // column threshold makes any multi-span word wider than ~2.5em in
            // total look like a new column: "Customer VAT Number" drawn as
            // `C`+`us`+`t`+`ome`+`r `+`V`+`A`+`T`+` `+`N`+`umbe`+`r` fragmented
            // into "Numbe" + "r", and "Huile d'olive à l'ancienne" into
            // "Huile d'" + "olive à l'" + "ancien" + "ne" — the table bucketer
            // then dropped each stray fragment into its own column/cell. The
            // residual here is only the real inter-glyph gap, so a genuine
            // column gutter (which `render_spans` still breaks on) keeps
            // splitting while intra-word kerns never do.
            let gap = (sp.x - px) - prev_advance;
            let word_break =
                is_space || (sp.text != " " && (gap > 2.5 * size || gap > 0.65 * space_adv));
            if word_break {
                flush(&mut out, &mut text, &mut x0, last_end);
            }
        } else if is_space {
            continue;
        }
        if x0.is_none() {
            x0 = Some(sp.x);
        }
        // Keep the span's own internal whitespace. PDF producers routinely
        // split one line into chunks that *carry* the separating space, e.g.
        // `(Pé)(r)(i)(o)(de)( de)( 0)...` or `(RE )(N° )(:)`; the old
        // `sp.text.trim()` dropped those spaces and fused whole table cells
        // into one token ("Périodede01/12/2018au31/12/2018", "FACTUREN°:").
        // The outer ends are still trimmed by `flush`, so a token never keeps
        // leading/trailing spaces, and the token count / x positions used for
        // column detection are unchanged.
        text.push_str(sp.text.as_str());
        prev_x = Some(sp.x);
        prev_advance = sp.advance;
        last_end = sp.x + sp.advance;
    }
    flush(&mut out, &mut text, &mut x0, last_end);
    merge_value_symbol_tokens(merge_sign_tokens(out))
}

/// Fold a lone sign glyph back onto the number it prefixes ("-" + "93" →
/// "-93"). A producer often draws the sign as its own run a few points left of
/// the digits — a kerning gap wider than the word-space threshold in
/// `line_words` — so the two become separate tokens, are bucketed into
/// separate columns, and the sign is emitted in one cell while the digits land
/// in the next ("- -" / "93<br>269"). Joining them keeps the value — and its
/// sign — in one cell.
///
/// Only a token that is *exactly* a sign and is immediately followed by the
/// line's final token is joined. That is the shape of a right-aligned signed
/// amount at the end of a row; an inline separator inside a longer run
/// ("022 735 - 3477 (service …)") is left untouched, as is a standalone dash
/// cell or a text bullet.
pub(super) fn merge_sign_tokens(words: Vec<WordTok>) -> Vec<WordTok> {
    let last = words.len().saturating_sub(1);
    let mut out: Vec<WordTok> = Vec::with_capacity(words.len());
    for (i, w) in words.into_iter().enumerate() {
        if let Some(prev) = out.last_mut() {
            let prev_is_sign = matches!(prev.text.as_str(), "-" | "−" | "+");
            let starts_digit = w
                .text
                .chars()
                .next()
                .map(|c| c.is_ascii_digit())
                .unwrap_or(false);
            if i == last && prev_is_sign && starts_digit {
                prev.text.push_str(&w.text);
                prev.x1 = prev.x1.max(w.x1);
                continue;
            }
        }
        out.push(w);
    }
    out
}

/// A lone currency/percent symbol that a PDF producer emitted as its own glyph
/// run. These are units, never standalone table columns.
pub(super) fn is_value_symbol(text: &str) -> bool {
    matches!(text, "€" | "$" | "£" | "¥" | "%" | "₽" | "¢")
}

/// Fold a bare trailing currency/percent symbol back into the numeric token it
/// immediately follows. Producers routinely draw "81,90 €" as two runs, the
/// amount and then the symbol starting exactly at the amount's right edge (the
/// intervening space span is consumed by `line_words`). Left alone, that symbol
/// becomes a word whose start x is zero-distance from the number's end, which
/// the ruler scanner promotes to a *phantom last column*: every data row then
/// splits into a number cell plus an adjacent symbol cell, and the window-level
/// `flowing` veto reads the near-zero gap as prose and rejects the whole table.
///
/// The merge is deliberately narrow: the symbol must be exactly one of the
/// known unit symbols, the previous token must end in a digit, and the symbol
/// must abut that token (no intervening whitespace). It can never fuse two
/// genuine columns, because a real column boundary is separated by a column
/// gutter, far wider than the zero/single-point gap tested here.
pub(super) fn merge_value_symbol_tokens(words: Vec<WordTok>) -> Vec<WordTok> {
    let mut out: Vec<WordTok> = Vec::with_capacity(words.len());
    for w in words {
        if let Some(prev) = out.last_mut() {
            let prev_ends_digit = prev
                .text
                .chars()
                .last()
                .map(|c| c.is_ascii_digit())
                .unwrap_or(false);
            let abuts = (w.x0 - prev.x1).abs() <= 1.0;
            if prev_ends_digit && is_value_symbol(&w.text) && abuts {
                // French/typographic convention: a space before a currency
                // symbol ("81,90 €") but none before a percent ("10%").
                if w.text != "%" {
                    prev.text.push(' ');
                }
                prev.text.push_str(&w.text);
                prev.x1 = prev.x1.max(w.x1);
                continue;
            }
        }
        out.push(w);
    }
    out
}

/// Sequentially clusters sorted `(x, row)` pairs within `tol` of the running
/// cluster mean, keeping only clusters that span at least 2 distinct rows.
/// Shared by `table_rulers`'s two clustering passes (word starts, word ends)
/// so both use exactly the same tolerance/majority logic.
pub(super) fn cluster_positions(mut points: Vec<(f64, usize)>, tol: f64) -> Vec<f64> {
    points.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

    struct Clust {
        sum_x: f64,
        count: usize,
        rows: std::collections::HashSet<usize>,
    }
    let mut clusters: Vec<Clust> = Vec::new();
    for (x, ri) in points {
        let mut matched = false;
        if let Some(last) = clusters.last_mut() {
            let mean = last.sum_x / last.count as f64;
            if (x - mean).abs() <= tol {
                last.sum_x += x;
                last.count += 1;
                last.rows.insert(ri);
                matched = true;
            }
        }
        if !matched {
            let mut rows = std::collections::HashSet::new();
            rows.insert(ri);
            clusters.push(Clust { sum_x: x, count: 1, rows });
        }
    }

    clusters
        .into_iter()
        .filter(|c| c.rows.len() >= 2)
        .map(|c| c.sum_x / c.count as f64)
        .collect()
}
