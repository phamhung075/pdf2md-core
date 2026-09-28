// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Header-anchored operations ledgers.
//!
//! A bank statement's operations ledger is a wide multi-column grid whose
//! header labels are frequently not on one baseline and not left-aligned over
//! the columns they name, so the generic ruler/grid passes either miss the
//! ledger or split it into a nest of 2/3-column fragments. This module
//! recognises the header band itself, derives each column's horizontal extent
//! from the labels *and* from the rows below them (right-aligned amounts), and
//! folds every following line into one row per entry: a section label or a
//! sub-total becomes its own row, a wrapped description stays in the
//! description cell.

use super::ledger_columns::{line_pitch, refine_boundaries, rule_boundaries};
use super::ledger_rows::opening_balance_row;
use super::rulers::{line_words, min_gutter_for, TableHit, WordTok};
use crate::layout::glyph_stream::Span;
use crate::models::{BoundingBox, CELL_LINE_BREAK_PENDING};

/// A ledger header needs at least this many label cells (DATE, VALEUR,
/// description, and at least one amount column).
const MIN_HEADER_CELLS: usize = 4;
/// A header label cell holds at most this many words ...
const MAX_HEADER_CELL_WORDS: usize = 3;
/// ... and this many alphanumeric characters.
const MAX_HEADER_CELL_CHARS: usize = 28;
/// A header must carry at least this many synonym-labelled cells.
const MIN_HEADER_SYNONYMS: usize = 2;
/// Two baselines of one header band lie within this many text sizes.
const HEADER_TWO_BASELINE_SIZE_MULT: f64 = 1.6;
/// A line whose first word starts this far left of the ledger's left edge is
/// page furniture (running footer / legal text), not a ledger row.
const LEDGER_LEFT_TOL_PT: f64 = 2.5;
/// A vertical gap larger than this multiple of the ledger's line pitch starts a
/// new row, separating a section label from the entry above it.
const ROW_GAP_PITCH_MULT: f64 = 1.7;
/// A vertical gap larger than this multiple of the ledger's line pitch ends the
/// ledger. A page footer far below the last entry can start at the ledger's own
/// left edge, so the left-edge and page-counter guards do not catch it. The
/// value sits well above [`ROW_GAP_PITCH_MULT`] — which only opens a new row at
/// an in-ledger section break — so a real ledger is never cut; the pitch is
/// taken from the prefix before the widest gap, so the footer's own outlier gap
/// does not inflate it.
const MAX_ROW_GAP_PITCH_MULT: f64 = 4.0;

/// Which family a header label belongs to. Determines how a row's words are
/// assigned and which lines begin a new row.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ColumnKind {
    Date,
    Value,
    Text,
    Amount,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Family {
    Date,
    Value,
    Text,
    Amount,
}

/// One derived ledger column: its header label plus the horizontal extent the
/// label and the data below it agree on.
#[derive(Clone)]
pub(super) struct HeaderColumn {
    label: String,
    pub(super) x0: f64,
    pub(super) x1: f64,
    pub(super) kind: ColumnKind,
}

/// Header synonyms, matched after accent folding and uppercasing. Kept small
/// and documented: a ledger header is recognised by a date/value column, a
/// description column and at least one amount column, not by any single word.
const SYN_DATE: &[&str] = &["DATE", "DATUM", "FECHA"];
const SYN_VALUE: &[&str] = &["VALEUR", "VALUE", "VALUTA", "VALOR"];
const SYN_TEXT: &[&str] = &[
    "NATURE",
    "DESCRIPTION",
    "OPERATION",
    "LIBELLE",
    "DETAIL",
    "PARTICULAR",
    "NARRATIVE",
    "MOTIF",
    "REFERENCE",
    "TEXT",
];
const SYN_AMOUNT: &[&str] = &[
    "DEBIT",
    "CREDIT",
    "MONTANT",
    "AMOUNT",
    "BETRAG",
    "SUMME",
    "IMPORTE",
    "SALDO",
    "BALANCE",
    "HABEN",
    "AVERE",
    "ADDEBITO",
    "SOLL",
];

/// Uppercase and drop accents/punctuation so "Débit", "DÉBIT" and "DEBIT"
/// compare equal. `to_uppercase` (Unicode) folds the lowercase accented letters
/// a mixed-case header uses — ASCII-only uppercasing leaves `é`/`è` untouched
/// and the label then never matches its accent-stripped synonym.
fn normalize(label: &str) -> String {
    label
        .chars()
        .filter(|c| c.is_alphanumeric())
        .map(|c| c.to_uppercase().next().unwrap_or(c))
        .map(|c| match c {
            'À' | 'Â' | 'Ä' | 'Á' | 'Ã' | 'Å' => 'A',
            'Ç' => 'C',
            'È' | 'É' | 'Ê' | 'Ë' => 'E',
            'Î' | 'Ï' | 'Í' | 'Ì' => 'I',
            'Ô' | 'Ö' | 'Ó' | 'Ò' | 'Õ' => 'O',
            'Ù' | 'Û' | 'Ü' | 'Ú' => 'U',
            'Ÿ' | 'Ý' => 'Y',
            'Ñ' => 'N',
            other => other,
        })
        .collect()
}

/// Classify a header label into its column family, if it is one we know.
fn family(label: &str) -> Option<Family> {
    let n = normalize(label);
    if n.is_empty() {
        return None;
    }
    if SYN_AMOUNT.iter().any(|s| n.contains(s)) {
        return Some(Family::Amount);
    }
    if SYN_DATE.iter().any(|s| n.contains(s)) {
        return Some(Family::Date);
    }
    if SYN_VALUE.iter().any(|s| n.contains(s)) {
        return Some(Family::Value);
    }
    if SYN_TEXT.iter().any(|s| n.contains(s)) {
        return Some(Family::Text);
    }
    None
}

/// A header cell is a short label, never a value: at most [`MAX_HEADER_CELL_WORDS`]
/// words, at most [`MAX_HEADER_CELL_CHARS`] normalized characters, and with at
/// least one letter.
fn label_like(cell: &str) -> bool {
    let words = cell.split_whitespace().count();
    words >= 1
        && words <= MAX_HEADER_CELL_WORDS
        && normalize(cell).chars().count() <= MAX_HEADER_CELL_CHARS
        && cell.chars().any(|c| c.is_alphabetic())
}

/// Group a line's words into cells, starting a new cell at each gutter of at
/// least `min_gutter`.
fn group_cells(words: &[WordTok], min_gutter: f64) -> Vec<(f64, f64, String)> {
    let mut out: Vec<(f64, f64, String)> = Vec::new();
    for w in words {
        match out.last_mut() {
            Some((_, x1, text)) if w.x0 - *x1 < min_gutter => {
                text.push(' ');
                text.push_str(&w.text);
                *x1 = w.x1;
            }
            _ => out.push((w.x0, w.x1, w.text.clone())),
        }
    }
    out
}

/// Build ledger columns from a header band's words, or `None` when the words do
/// not read as a ledger header.
fn header_from_words(words: &[WordTok], size: f64) -> Option<Vec<HeaderColumn>> {
    let cells = group_cells(words, min_gutter_for(size));
    if cells.len() < MIN_HEADER_CELLS || !cells.iter().all(|(_, _, t)| label_like(t)) {
        return None;
    }
    let families: Vec<Option<Family>> = cells.iter().map(|(_, _, t)| family(t)).collect();
    let matched = families.iter().filter(|f| f.is_some()).count();
    let has_amount = families.iter().any(|f| *f == Some(Family::Amount));
    let has_date_or_value = families
        .iter()
        .any(|f| matches!(f, Some(Family::Date) | Some(Family::Value)));
    // A ledger header names a date/value column and at least one amount column.
    // Requiring both keeps ordinary invoice grids (description | qty | amount)
    // out of this model.
    if matched < MIN_HEADER_SYNONYMS || !has_amount || !has_date_or_value {
        return None;
    }
    let mut cols: Vec<HeaderColumn> = cells
        .iter()
        .zip(families)
        .map(|((x0, x1, text), fam)| HeaderColumn {
            label: text.clone(),
            x0: *x0,
            x1: *x1,
            kind: match fam {
                Some(Family::Date) => ColumnKind::Date,
                Some(Family::Value) => ColumnKind::Value,
                Some(Family::Amount) => ColumnKind::Amount,
                _ => ColumnKind::Text,
            },
        })
        .collect();
    cols.sort_by(|a, b| a.x0.partial_cmp(&b.x0).unwrap_or(std::cmp::Ordering::Equal));
    Some(cols)
}

fn max_size(line: &[Span]) -> f64 {
    line.iter().map(|s| s.size).fold(0.0, f64::max)
}

/// Locate the ledger header band on a page: a single baseline of >= 4 labels,
/// or two close baselines whose labels together form the header.
fn detect_header(lines: &[Vec<Span>]) -> Option<(usize, usize, Vec<HeaderColumn>)> {
    for (i, line) in lines.iter().enumerate() {
        if line.is_empty() {
            continue;
        }
        if let Some(cols) = header_from_words(&line_words(line), max_size(line)) {
            return Some((i, i, cols));
        }
    }
    for i in 0..lines.len().saturating_sub(1) {
        let (a, b) = (&lines[i], &lines[i + 1]);
        if a.is_empty() || b.is_empty() {
            continue;
        }
        let size = max_size(a).max(max_size(b));
        let gap = a[0].y - b[0].y;
        if !(gap > 0.0 && gap <= size * HEADER_TWO_BASELINE_SIZE_MULT) {
            continue;
        }
        let mut words = line_words(a);
        words.extend(line_words(b));
        words.sort_by(|p, q| p.x0.partial_cmp(&q.x0).unwrap_or(std::cmp::Ordering::Equal));
        if let Some(cols) = header_from_words(&words, size) {
            return Some((i, i + 1, cols));
        }
    }
    None
}

/// Column index for a word centre, given the (ascending) boundaries.
pub(super) fn column_of(center: f64, boundaries: &[f64]) -> usize {
    boundaries.iter().take_while(|&&b| center >= b).count()
}

/// A lone short numeric word positioned in the ledger's right-hand (amount)
/// half is a page counter rather than a description continuation. Left-half
/// numeric lines are real content: a long all-digit reference is an ordinary
/// continuation, and treating it as furniture silently drops it.
fn looks_like_page_counter(words: &[WordTok], ledger_left: f64, ledger_right: f64) -> bool {
    if words.len() != 1 {
        return false;
    }
    let t = words[0].text.trim();
    if !t.chars().any(|c| c.is_ascii_digit()) {
        return false;
    }
    let page_counter_chars = t
        .chars()
        .all(|c| c.is_ascii_digit() || c == '/' || c == '.' || c == '-');
    let right_half = words[0].x0 > ledger_left + 0.6 * (ledger_right - ledger_left);
    page_counter_chars && t.len() <= 6 && right_half
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

/// Rebuild the ledger at `[hstart, hend]` as one table, returning `None` when no
/// rows follow the header.
///
/// `vertical_rules` are the page's thin drawn vertical rules as `(x, y0, y1)`;
/// when they resolve to the ledger's internal column separators they cut the
/// columns exactly, otherwise the label/data-derived boundaries are kept.
fn build_ledger(
    lines: &[Vec<Span>],
    hstart: usize,
    hend: usize,
    header: &[HeaderColumn],
    vertical_rules: &[(f64, f64, f64)],
) -> Option<TableHit> {
    let ledger_left = header.first()?.x0;
    let ledger_right = header.iter().map(|c| c.x1).fold(ledger_left, f64::max);
    let ncols = header.len();
    let mut data: Vec<(usize, f64, Vec<WordTok>)> = Vec::new();
    for (j, line) in lines.iter().enumerate().skip(hend + 1) {
        if line.is_empty() {
            break;
        }
        let words = line_words(line);
        if words.is_empty() {
            break;
        }
        if words[0].x0 < ledger_left - LEDGER_LEFT_TOL_PT
            || looks_like_page_counter(&words, ledger_left, ledger_right)
        {
            break;
        }
        data.push((j, line[0].y, words));
    }
    if data.len() < 2 {
        return None;
    }

    let mut ys: Vec<f64> = data.iter().map(|(_, y, _)| *y).collect();
    // A footer far below the last operation shares the ledger's left edge and
    // slips past the guards above. It contributes the single widest gap, so the
    // typical in-ledger pitch is the median (`line_pitch`) of the prefix before
    // that gap: on a two-line page the median of all gaps would otherwise be the
    // outlier itself and defeat the cut. Data is then cut at the first gap wider
    // than MAX_ROW_GAP_PITCH_MULT pitches before any row is built (the hit's
    // `end` and bbox shrink with it).
    let mut widest = 0usize;
    let mut widest_gap = f64::NEG_INFINITY;
    for (i, w) in ys.windows(2).enumerate() {
        let gap = w[0] - w[1];
        if gap > widest_gap {
            widest_gap = gap;
            widest = i;
        }
    }
    let max_gap = line_pitch(&ys[..=widest]) * MAX_ROW_GAP_PITCH_MULT;
    let cut = data.windows(2).position(|w| w[0].1 - w[1].1 > max_gap);
    if let Some(k) = cut {
        data.truncate(k + 1);
        ys.truncate(k + 1);
    }
    if data.len() < 2 {
        return None;
    }

    let word_rows: Vec<Vec<WordTok>> = data.iter().map(|(_, _, w)| w.clone()).collect();
    let size = lines[hstart..=hend]
        .iter()
        .flat_map(|l| l.iter())
        .map(|s| s.size)
        .fold(0.0, f64::max);
    let mut boundaries = refine_boundaries(header, &word_rows, size);
    let threshold = line_pitch(&ys) * ROW_GAP_PITCH_MULT;
    // A drawn rule is exact, so adopt the rule x's when they resolve to one
    // separator per internal column boundary.
    let band_lo = ys.iter().copied().fold(f64::INFINITY, f64::min);
    let band_hi = lines[hstart..=hend]
        .iter()
        .flat_map(|l| l.iter())
        .map(|s| s.y)
        .fold(f64::NEG_INFINITY, f64::max);
    if let Some(exact) =
        rule_boundaries(vertical_rules, ncols, ledger_left, ledger_right, (band_lo, band_hi))
    {
        boundaries = exact;
    }

    // An opening balance sits directly above the header (below the account
    // band) as one label cell plus one amount cell, with no date/value cell.
    let pre = (hstart > 0)
        .then(|| &lines[hstart - 1])
        .and_then(|line| {
            let inside = line_words(line)
                .first()
                .map(|w| w.x0 >= ledger_left - LEDGER_LEFT_TOL_PT)
                .unwrap_or(false);
            inside.then(|| opening_balance_row(line, header, &boundaries)).flatten()
        });
    let start = if pre.is_some() { hstart - 1 } else { hstart };

    let mut rows: Vec<Vec<String>> = Vec::with_capacity(data.len() + 2);
    rows.push(header.iter().map(|c| c.label.clone()).collect());
    if let Some(cells) = pre {
        rows.push(cells);
    }
    let mut cur: Vec<String> = Vec::new();
    let mut cur_is_operation = false;
    let mut prev_y: Option<f64> = None;
    let mut first = true;
    for (_, y, words) in &data {
        let cols: Vec<usize> = words
            .iter()
            .map(|w| column_of(0.5 * (w.x0 + w.x1), &boundaries))
            .collect();
        let has_date = cols
            .iter()
            .any(|&c| matches!(header[c].kind, ColumnKind::Date | ColumnKind::Value));
        let has_amount = cols.iter().any(|&c| header[c].kind == ColumnKind::Amount);
        // A wrapped continuation can carry the amount on its own line: a
        // numeric-only line with an amount extends the operation above it,
        // while a labelled sub-total (alphabetic text plus an amount) opens
        // its own row.
        let has_label = words
            .iter()
            .any(|w| w.text.chars().any(|c| c.is_alphabetic()));
        let gap = prev_y.map(|py| py - y).unwrap_or(f64::INFINITY);
        // A continuation line may only extend an operation row; a section label
        // or a sub-total must not absorb the line that follows it.
        if first || has_date || (has_amount && has_label) || gap > threshold || !cur_is_operation {
            if !cur.is_empty() {
                rows.push(std::mem::take(&mut cur));
            }
            cur = vec![String::new(); ncols];
            cur_is_operation = has_date;
        }
        let mut line_cells = vec![String::new(); ncols];
        for (w, &c) in words.iter().zip(&cols) {
            if !line_cells[c].is_empty() {
                line_cells[c].push(' ');
            }
            line_cells[c].push_str(&w.text);
        }
        for c in 0..ncols {
            if line_cells[c].is_empty() {
                continue;
            }
            if !cur[c].is_empty() {
                cur[c].push(CELL_LINE_BREAK_PENDING);
            }
            cur[c].push_str(&line_cells[c]);
        }
        prev_y = Some(*y);
        first = false;
    }
    if !cur.is_empty() {
        rows.push(cur);
    }

    let end = data.last()?.0;
    Some(TableHit {
        start,
        end,
        rows,
        bbox: union_bbox(lines, start, end),
    })
}

/// Re-anchor a page's table detectors on any operations ledger it carries.
///
/// When a page has a ledger header, the whole ledger region is rendered as one
/// header-anchored table, replacing the fragment hits the generic passes found
/// there. Tables elsewhere on the page are untouched.
pub fn apply_ledger_model(lines: &[Vec<Span>], hits: Vec<TableHit>) -> Vec<TableHit> {
    apply_ledger_model_with_rules(lines, hits, &[])
}

/// [`apply_ledger_model`] with the page's thin drawn vertical rules supplied as
/// `(x, y0, y1)` in span device space. When those rules resolve to the ledger's
/// internal column separators, the columns are cut on the drawn lines exactly
/// instead of on the label/data-derived midpoints.
pub fn apply_ledger_model_with_rules(
    lines: &[Vec<Span>],
    hits: Vec<TableHit>,
    vertical_rules: &[(f64, f64, f64)],
) -> Vec<TableHit> {
    let Some((hstart, hend, header)) = detect_header(lines) else {
        return hits;
    };
    let Some(ledger) = build_ledger(lines, hstart, hend, &header, vertical_rules) else {
        return hits;
    };
    let mut out: Vec<TableHit> = hits
        .into_iter()
        .filter(|h| h.end < ledger.start || h.start > ledger.end)
        .collect();
    out.push(ledger);
    out.sort_by(|a, b| a.start.cmp(&b.start));
    out
}

#[cfg(test)]
#[path = "ledger_tests.rs"]
mod tests;
