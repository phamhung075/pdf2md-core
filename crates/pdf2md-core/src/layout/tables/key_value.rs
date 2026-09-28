// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Key/value summary boxes (short labels, each with a numeric amount).
//!
//! Some pages open with a compact block of short labels where each label owns a
//! numeric amount either **beside** it (same visual line, to the right) or
//! **below** it (a label-only line whose amounts sit on the following line).
//! The block is too sparse and its columns too ragged for the ruler/grid
//! passes: a "below" label's amounts do not start under the label's own x-band,
//! and one row can hold a single label against several amounts. The generic
//! passes therefore reject the block (or split it), and the plain renderer
//! emits the labels and the amounts as separate lines, out of order. This pass
//! reads the block as a label -> amount list instead of a matrix.

use super::rulers::{line_words, TableHit, WordTok};
use crate::layout::glyph_stream::Span;
use crate::models::BoundingBox;

/// A box line holds at most this many word tokens ...
const MAX_LINE_WORDS: usize = 8;
/// ... and at most this many characters. A prose or address line is longer.
const MAX_LINE_CHARS: usize = 72;
/// A box spans at most this many visual lines.
const MAX_BOX_LINES: usize = 5;
/// A box needs at least this many label -> amount pairs to be worth a table.
const MIN_PAIRS: usize = 2;
/// Consecutive box lines sit at most this many text sizes apart ...
const BOX_GAP_SIZE_MULT: f64 = 1.6;
/// ... and never less than this absolute floor, so a small body size cannot
/// split a genuinely tight block.
const BOX_GAP_FLOOR_PT: f64 = 14.0;
/// A "below" label sits at most this many lines above its amount.
const MAX_LABEL_ABOVE_LINES: usize = 2;
/// A "below" label may join an amount only when their x-bands are this close,
/// in text sizes.
const ABOVE_DX_SIZE_MULT: f64 = 2.0;
/// A "beside" label may join an amount only when the whitespace between them is
/// this narrow, in text sizes. A short label at the left margin and an amount at
/// the far right of a wide row are not a pair: the wide column between them
/// belongs to another field (or to prose that shares the row).
const BESIDE_GAP_SIZE_MULT: f64 = 16.0;
/// A label holds at most this many words ...
const MAX_LABEL_WORDS: usize = 6;
/// ... and at most this many characters.
const MAX_LABEL_CHARS: usize = 40;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Label,
    Money,
    Other,
}

/// A digit-group separator: thin/normal spaces, a point, a comma or an
/// apostrophe (all seen as thousands separators in real statements).
fn is_group_sep(c: char) -> bool {
    matches!(c, ' ' | '\u{00a0}' | '\u{202f}' | '.' | ',' | '\'' | '\u{2019}')
}

/// A token that reads as a monetary amount: an optional sign, digits (with
/// optional thousands grouping), a decimal separator and exactly two fraction
/// digits, with an optional trailing currency/percent symbol.
///
/// A short dot-decimal carrying neither a sign, a currency symbol nor a
/// thousands group (`07.02`) is a date, not an amount.
fn is_money(text: &str) -> bool {
    let t = text.trim();
    let unsigned = t.trim_start_matches(['-', '+', '\u{2212}']);
    let signed = unsigned.len() != t.len();
    let (digits, symbol) = match unsigned.strip_suffix(['€', '$', '£', '¥', '%']) {
        Some(rest) => (rest.trim_end(), true),
        None => (unsigned.trim_end(), false),
    };
    let Some(pos) = digits.rfind(['.', ',']) else {
        return false;
    };
    let (int_part, frac) = digits.split_at(pos);
    let frac = &frac[1..];
    if frac.len() != 2 || !frac.chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    if int_part.is_empty() || !int_part.chars().all(|c| c.is_ascii_digit() || is_group_sep(c)) {
        return false;
    }
    let grouped = int_part.chars().any(is_group_sep);
    let dot = digits.as_bytes()[pos] == b'.';
    !(dot && !symbol && !signed && !grouped && int_part.len() <= 2)
}

/// A bare date (`01.02.2017`, `2017-02-01`) or a day/month-name/year triple
/// (`31 janvier 2017`) is a column key, never a label.
fn is_date_like(text: &str) -> bool {
    let t = text.trim();
    let parts: Vec<&str> = t.split(['.', '/', '-']).collect();
    if parts.len() == 3
        && parts.iter().all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
        && parts[2].len() >= 2
    {
        return true;
    }
    let words: Vec<&str> = t.split_whitespace().collect();
    words.len() == 3
        && words[0].chars().all(|c| c.is_ascii_digit())
        && words[2].chars().all(|c| c.is_ascii_digit())
        && words[1].chars().any(|c| c.is_alphabetic())
}

/// A short alphabetic key: at most [`MAX_LABEL_WORDS`] words and
/// [`MAX_LABEL_CHARS`] characters, with at least one letter, and not a date.
fn label_like(text: &str) -> bool {
    let t = text.trim();
    if t.is_empty() || !t.chars().any(|c| c.is_alphabetic()) {
        return false;
    }
    if t.split_whitespace().count() > MAX_LABEL_WORDS || t.chars().count() > MAX_LABEL_CHARS {
        return false;
    }
    !is_date_like(t)
}

fn classify(text: &str) -> Kind {
    if is_money(text) {
        Kind::Money
    } else if label_like(text) {
        Kind::Label
    } else {
        Kind::Other
    }
}

fn line_chars(words: &[WordTok]) -> usize {
    words.iter().map(|w| w.text.chars().count()).sum::<usize>() + words.len().saturating_sub(1)
}

/// A line may take part in a box when it is short and carries at least one
/// label or amount token.
fn box_line_like(words: &[WordTok]) -> bool {
    !words.is_empty()
        && words.len() <= MAX_LINE_WORDS
        && line_chars(words) <= MAX_LINE_CHARS
        && words.iter().any(|w| classify(&w.text) != Kind::Other)
}

fn max_size(line: &[Span]) -> f64 {
    line.iter().map(|s| s.size).fold(0.0, f64::max)
}

/// Horizontal gap between two token x-bands (0 when they overlap).
fn x_gap(a: &WordTok, b: &WordTok) -> f64 {
    if b.x0 >= a.x1 {
        b.x0 - a.x1
    } else if a.x0 >= b.x1 {
        a.x0 - b.x1
    } else {
        0.0
    }
}

fn union_bbox(lines: &[Vec<Span>], start: usize, end: usize) -> BoundingBox {
    let mut x0 = f64::INFINITY;
    let mut y0 = f64::INFINITY;
    let mut x1 = f64::NEG_INFINITY;
    let mut y1 = f64::NEG_INFINITY;
    for line in &lines[start..=end] {
        for s in line {
            x0 = x0.min(s.x);
            y0 = y0.min(s.y);
            x1 = x1.max(s.x + s.advance);
            y1 = y1.max(s.y + s.size);
        }
    }
    if !x0.is_finite() {
        return BoundingBox::new(0.0, 0.0, 0.0, 0.0);
    }
    BoundingBox::new(x0, y0, x1, y1)
}

/// One paired label and amount, both addressed by their position in `run`.
#[derive(Clone, Copy)]
struct Pair {
    lab: usize,
    lab_wi: usize,
    amt: usize,
    amt_wi: usize,
}

/// Read one tight run of candidate lines as a label -> amount list, or `None`
/// when it does not read as a box (no pair, unpaired token, ragged line).
fn build_box(
    lines: &[Vec<Span>],
    run: &[usize],
    cand: &[Option<Vec<WordTok>>],
) -> Option<TableHit> {
    if run.len() < 2 || run.len() > MAX_BOX_LINES {
        return None;
    }
    let mut kinds: Vec<Vec<Kind>> = Vec::with_capacity(run.len());
    let mut size = 0.0f64;
    for &i in run {
        let words = cand[i].as_ref()?;
        kinds.push(words.iter().map(|w| classify(&w.text)).collect());
        size = size.max(max_size(&lines[i]));
    }
    let size = size.max(0.1);
    let mut lab_used: Vec<Vec<bool>> = kinds.iter().map(|k| vec![false; k.len()]).collect();
    let mut amt_used: Vec<Vec<bool>> = kinds.iter().map(|k| vec![false; k.len()]).collect();
    let mut pairs: Vec<Pair> = Vec::new();

    // Beside: the nearest unused label to the left on the same visual line.
    for (li, &i) in run.iter().enumerate() {
        let words = cand[i].as_ref()?;
        for (mi, m) in words.iter().enumerate() {
            if kinds[li][mi] != Kind::Money || amt_used[li][mi] {
                continue;
            }
            let mut best: Option<(f64, usize)> = None;
            for (wi, l) in words.iter().enumerate() {
                if kinds[li][wi] != Kind::Label || lab_used[li][wi] || l.x0 > m.x0 {
                    continue;
                }
                let d = (m.x0 - l.x1).max(0.0);
                if d > BESIDE_GAP_SIZE_MULT * size {
                    continue;
                }
                if best.map_or(true, |(bd, _)| d < bd) {
                    best = Some((d, wi));
                }
            }
            if let Some((_, wi)) = best {
                lab_used[li][wi] = true;
                amt_used[li][mi] = true;
                pairs.push(Pair { lab: li, lab_wi: wi, amt: li, amt_wi: mi });
            }
        }
    }

    // Below: the nearest unused label on an earlier line, within the x-band.
    for (li, &i) in run.iter().enumerate() {
        let words = cand[i].as_ref()?;
        for (mi, m) in words.iter().enumerate() {
            if kinds[li][mi] != Kind::Money || amt_used[li][mi] {
                continue;
            }
            let mut best: Option<(f64, usize, usize)> = None;
            for above in 0..li {
                if li - above > MAX_LABEL_ABOVE_LINES {
                    continue;
                }
                let aw = cand[run[above]].as_ref()?;
                for (wi, l) in aw.iter().enumerate() {
                    if kinds[above][wi] != Kind::Label || lab_used[above][wi] {
                        continue;
                    }
                    let dx = x_gap(l, m);
                    if dx > ABOVE_DX_SIZE_MULT * size {
                        continue;
                    }
                    let key = (dx, li - above);
                    if best.map_or(true, |(bdx, bvd, _)| key < (bdx, bvd)) {
                        best = Some((dx, li - above, wi));
                    }
                }
            }
            if let Some((_, vd, wi)) = best {
                let above = li - vd;
                lab_used[above][wi] = true;
                amt_used[li][mi] = true;
                pairs.push(Pair { lab: above, lab_wi: wi, amt: li, amt_wi: mi });
            }
        }
    }

    if pairs.len() < MIN_PAIRS {
        return None;
    }
    let start = pairs
        .iter()
        .map(|p| run[p.lab].min(run[p.amt]))
        .min()?;
    let end = pairs
        .iter()
        .map(|p| run[p.lab].max(run[p.amt]))
        .max()?;

    // Every token on a covered line must be consumed by a pair, or the table
    // would silently drop it (the renderer skips the whole claimed line range).
    for (li, &i) in run.iter().enumerate() {
        if i < start || i > end {
            continue;
        }
        let words = cand[i].as_ref()?;
        for wi in 0..words.len() {
            if !lab_used[li][wi] && !amt_used[li][wi] {
                return None;
            }
        }
    }

    // Rows in the reading order of their labels (top-to-bottom, left-to-right).
    let mut ordered = pairs;
    ordered.sort_by(|a, b| {
        let la = cand[run[a.lab]].as_ref().map(|w| w[a.lab_wi].x0).unwrap_or(0.0);
        let lb = cand[run[b.lab]].as_ref().map(|w| w[b.lab_wi].x0).unwrap_or(0.0);
        run[a.lab].cmp(&run[b.lab]).then(
            la.partial_cmp(&lb).unwrap_or(std::cmp::Ordering::Equal),
        )
    });
    let rows: Vec<Vec<String>> = ordered
        .iter()
        .map(|p| {
            let lw = cand[run[p.lab]].as_ref()?;
            let aw = cand[run[p.amt]].as_ref()?;
            Some(vec![lw[p.lab_wi].text.clone(), aw[p.amt_wi].text.clone()])
        })
        .collect::<Option<Vec<_>>>()?;

    Some(TableHit {
        start,
        end,
        rows,
        bbox: union_bbox(lines, start, end),
    })
}

/// Detect key/value summary boxes on a page's visual lines.
///
/// Lines already claimed by an earlier detector (`covered`) are never re-read,
/// so an operations ledger that also carries a date/amount column is left to
/// the ledger model.
pub fn find_key_value_boxes(lines: &[Vec<Span>], covered: &[TableHit]) -> Vec<TableHit> {
    let n = lines.len();
    let mut cand: Vec<Option<Vec<WordTok>>> = vec![None; n];
    for i in 0..n {
        if lines[i].is_empty() || covered.iter().any(|h| h.start <= i && i <= h.end) {
            continue;
        }
        let words = line_words(&lines[i]);
        if box_line_like(&words) {
            cand[i] = Some(words);
        }
    }

    let mut hits = Vec::new();
    let mut run: Vec<usize> = Vec::new();
    for i in 0..=n {
        let is_cand = i < n && cand[i].is_some();
        let keep = is_cand
            && match run.last() {
                Some(&prev) => {
                    let gap = lines[prev][0].y - lines[i][0].y;
                    let size = max_size(&lines[prev]).max(max_size(&lines[i]));
                    i == prev + 1
                        && gap > 0.0
                        && gap <= (BOX_GAP_SIZE_MULT * size).max(BOX_GAP_FLOOR_PT)
                }
                None => true,
            };
        if keep {
            run.push(i);
            continue;
        }
        if !run.is_empty() {
            if let Some(hit) = build_box(lines, &run, &cand) {
                hits.push(hit);
            }
            run.clear();
        }
        if is_cand {
            run.push(i);
        }
    }
    hits
}

/// Append any key/value boxes the generic passes missed, keeping the page's
/// hits sorted by their first line.
///
/// The pass only augments a page that already carries a table. Turning a
/// table-free page into a table page solely for one small box flips it into the
/// table renderer, which bypasses the plain and math renderers and so perturbs
/// unrelated content on that page (a stacked fraction elsewhere, column order)
/// for no table gain; leaving the box as plain text is the smaller change.
pub fn append_key_value_boxes(lines: &[Vec<Span>], mut hits: Vec<TableHit>) -> Vec<TableHit> {
    if hits.is_empty() {
        return hits;
    }
    let boxes = find_key_value_boxes(lines, &hits);
    if boxes.is_empty() {
        return hits;
    }
    hits.extend(boxes);
    hits.sort_by(|a, b| a.start.cmp(&b.start));
    hits
}

#[cfg(test)]
#[path = "key_value_tests.rs"]
mod tests;
