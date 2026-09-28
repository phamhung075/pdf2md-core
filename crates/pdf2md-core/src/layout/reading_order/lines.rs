// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Reading order recovery, multi-column stream separation, and structured DocBlock generation.

use super::*;

/// Em fraction of residual whitespace at which a line is hard-broken into two
/// visual elements. Shared by every wide-gap call site so they cannot drift.
pub(crate) const HARD_BREAK_EM: f64 = 2.5;

/// Extra point floor the block builder's `split_line_segments` applies on top of
/// [`HARD_BREAK_EM`]. The column-projection callers of `split_line_segments` use
/// the same value, so their behaviour is unchanged.
const SEGMENT_MIN_GAP_PT: f64 = 20.0;

/// Fraction of the page width past which a lone right-hand monetary value is
/// kept on its label's line instead of being taken for a separate column.
///
/// Derived from the corpus investigation's measured x-fraction distribution of
/// the real `detached_amount` instances: a right-aligned amount's right edge
/// sits at p25 = 0.61 and median = 0.78 of the page width. `0.55` sits below
/// the p25 with margin, so every measured right-aligned amount is inside the
/// band, while the left half of the page (where a genuine two-column gutter or
/// a table's second column starts) is excluded.
pub(crate) const AMOUNT_RIGHT_EDGE_MIN_PAGE_FRAC: f64 = 0.55;

/// Fractional digits a monetary amount must carry. A decimal part is mandatory
/// so a pure integer (page reference, quantity, 4+ digit code) can never match.
const AMOUNT_DECIMALS: usize = 2;

/// Separators accepted as the decimal mark of a monetary amount.
const AMOUNT_DECIMAL_SEPARATORS: [char; 2] = [',', '.'];

/// Separators accepted inside a monetary amount's integer part. Space and the
/// two typographic spaces cover the French convention ("1 234,56"); the dot and
/// comma cover the Anglo/German ones ("1,234.56" / "1.234,56").
const AMOUNT_THOUSANDS_SEPARATORS: [char; 5] = [' ', '\u{00a0}', '\u{202f}', '.', ','];

/// Digits in the leading group of a thousands-grouped number (1..=3). Also the
/// widest digit tail a label may end in before a kept right-aligned amount
/// would fuse with it into one larger space-grouped number.
const AMOUNT_THOUSANDS_GROUP_WIDTH: usize = 3;

/// Currency symbols accepted on either side of a monetary amount.
const AMOUNT_CURRENCY_SYMBOLS: [char; 6] = ['$', '\u{20ac}', '\u{00a3}', '\u{00a5}', '\u{20b9}', '\u{20ab}'];

/// ISO 4217 currency codes accepted on either side of a monetary amount.
const AMOUNT_CURRENCY_CODES: [&str; 13] = [
    "EUR", "USD", "GBP", "CHF", "CAD", "VND", "JPY", "MAD", "TND", "DZD", "XOF", "XAF", "PLN",
];

/// The one wide-gap hard-break decision, shared by `split_line_segments`,
/// `split_hard_breaks` and `render_spans`.
///
/// True when the residual whitespace `gap` (already net of the previous run's
/// advance) is wide enough to separate two visual elements: `gap > HARD_BREAK_EM
/// * size` and `gap > min_gap`. `left` is the run accumulated before the gap and
/// `right` the spans that would begin the next segment.
///
/// One exception keeps a label and its lone right-aligned monetary value on one
/// line (see [`keeps_right_aligned_amount`]). `page_width` is `None` wherever no
/// page geometry is available — the column-detection callers of
/// `split_line_segments` — which disables the exception and preserves their
/// previous behaviour exactly.
pub(crate) fn wide_gap_breaks(
    gap: f64,
    size: f64,
    min_gap: f64,
    left: &[Span],
    right: &[Span],
    page_width: Option<f64>,
) -> bool {
    if !(gap > HARD_BREAK_EM * size && gap > min_gap) {
        return false;
    }
    if let Some(width) = page_width {
        if keeps_right_aligned_amount(left, right, width) {
            return false;
        }
    }
    true
}

/// The lone exception to [`wide_gap_breaks`]: do not break when the whole right
/// remainder is *exactly one* monetary amount, the left side carries a letter (a
/// textual label, not another numeric column), and the amount's right edge sits
/// in the right margin band of the page.
///
/// "Exactly one" is enforced twice: the remainder must be a single visual
/// segment (no further wide gap, see `split_hard_breaks` with the exception
/// disabled), and the trimmed remainder text must match [`is_monetary_amount`]
/// in full. A label followed by two wide-separated value columns therefore
/// still breaks exactly as it did before the exception existed.
fn keeps_right_aligned_amount(left: &[Span], right: &[Span], page_width: f64) -> bool {
    if !(page_width > 0.0) {
        return false;
    }
    if !left
        .iter()
        .any(|s| s.text.chars().any(|c| c.is_alphabetic()))
    {
        return false;
    }
    // Joining a label whose last token ends in a 1..=3-digit run to a
    // space-grouped amount would read as one larger number ("12 345,67"), so
    // that shape keeps today's split instead of firing the exception.
    if left_ends_with_group_candidate(left) {
        return false;
    }
    // No further wide gap: `page_width = None` disables the exception inside
    // this recursion, so it is a pure wide-gap split of the remainder.
    if split_hard_breaks(right, None).len() != 1 {
        return false;
    }
    let text: String = right.iter().map(|s| s.text.as_str()).collect();
    if !is_monetary_amount(text.trim()) {
        return false;
    }
    let x1 = right
        .iter()
        .map(|s| s.x + s.advance)
        .fold(f64::NEG_INFINITY, f64::max);
    x1 >= AMOUNT_RIGHT_EDGE_MIN_PAGE_FRAC * page_width
}

/// Whether `left`'s last *rendered* whitespace-separated token ends with a
/// digit run of 1..=[`AMOUNT_THOUSANDS_GROUP_WIDTH`] allowed as a thousands
/// leading group (preceded by a non-digit or the start of the token).
///
/// Covers a bare label number (`12`), a trailing code (`AB12`) and a reference
/// (`Réf 123`). A longer pure-digit run (`1234`) or a token with no trailing
/// digit (`TTC`) is not a grouping candidate.
///
/// The token is taken from `render_spans` output, not from the raw span
/// concatenation: producers draw a label's digits as separate glyph spans with
/// no whitespace glyph between them, so only the renderer's own space insertion
/// recovers the visible tokens.
fn left_ends_with_group_candidate(left: &[Span]) -> bool {
    let rendered = render_spans(left, None);
    let cleaned = visible_inline_text(&rendered);
    let Some(token) = cleaned.split_whitespace().last() else {
        return false;
    };
    let digits = token
        .chars()
        .rev()
        .take_while(|c| c.is_ascii_digit())
        .count();
    if digits == 0 || digits > AMOUNT_THOUSANDS_GROUP_WIDTH {
        return false;
    }
    // `digits` counts the maximal trailing digit run, so the character before
    // it (if any) is not a digit; the branch keeps the "preceded by a non-digit
    // or start" contract explicit. Index by chars, not by the reversed
    // iterator: `take_while` consumes the first non-matching character, which
    // would otherwise skip the very character this check needs.
    let prefix_len = token.chars().count() - digits;
    if prefix_len == 0 {
        return true;
    }
    !token
        .chars()
        .nth(prefix_len - 1)
        .is_some_and(|c| c.is_ascii_digit())
}

/// Strip the inline decoration `render_spans` may add (`*`/`_` emphasis,
/// backticks, `<u>`/`</u>` tags) so only visible text remains.
fn visible_inline_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if in_tag => {}
            '*' | '_' | '`' => {}
            _ => out.push(c),
        }
    }
    out
}

/// Whether `text` is *one* monetary amount and nothing else: digits with an
/// optional thousands grouping and a mandatory [`AMOUNT_DECIMALS`]-digit
/// decimal part (`,` or `.`), optionally wrapped in one sign and/or one
/// currency symbol or ISO code on either side.
///
/// The shape deliberately rejects everything the wide-gap exception must not
/// swallow: dates (`12.01.2026`, `12/01/2026` carry a second separator), 2-decimal
/// percentages (`%`), phone-like slashed/dotted groups (group lengths other
/// than 3), and pure integers (no mandatory decimal part). Once the sign and
/// currency wrappers are peeled off, the table engine's `is_numeric_cell`
/// recogniser gates the remaining digit/separator vocabulary.
///
/// Shared with the table consumer that separates two visual amount columns
/// sharing one matrix cell (`layout::tables::consolidation`), so both agree on
/// exactly the same amounts. Keep this the single recogniser.
pub(crate) fn is_monetary_amount(text: &str) -> bool {
    let mut s = text.trim();
    if s.contains('%') || s.contains('/') || s.contains(':') {
        return false;
    }
    // Peel off any wrappers — one sign and/or one currency symbol/code on
    // either side, in any order ("-$1,234.56", "1 234,56 EUR", "EUR 1 234,56")
    // — until the string stops changing; what remains must be the bare number.
    loop {
        let before = s;
        s = s.trim_start_matches(['-', '+']).trim_end_matches(['-', '+']).trim();
        s = s
            .trim_start_matches(|c: char| AMOUNT_CURRENCY_SYMBOLS.contains(&c))
            .trim_end_matches(|c: char| AMOUNT_CURRENCY_SYMBOLS.contains(&c))
            .trim();
        for code in AMOUNT_CURRENCY_CODES {
            if let Some(rest) = s.strip_prefix(code) {
                s = rest.trim_start();
            }
            if let Some(rest) = s.strip_suffix(code) {
                s = rest.trim_end();
            }
        }
        if s == before {
            break;
        }
    }
    if !crate::layout::tables::borderless::is_numeric_cell(s) {
        return false;
    }
    let chars: Vec<char> = s.chars().collect();
    // At least one integer digit + decimal separator + [`AMOUNT_DECIMALS`] digits.
    if chars.len() < AMOUNT_DECIMALS + 2 {
        return false;
    }
    let sep_idx = chars.len() - AMOUNT_DECIMALS - 1;
    if !AMOUNT_DECIMAL_SEPARATORS.contains(&chars[sep_idx]) {
        return false;
    }
    let fraction = &chars[sep_idx + 1..];
    if fraction.len() != AMOUNT_DECIMALS || !fraction.iter().all(|c| c.is_ascii_digit()) {
        return false;
    }
    // Integer part: digit groups joined by thousands separators. A single
    // ungrouped run of any length is fine ("1234,56"); with grouping the leading
    // group is 1..=3 digits and every following group is exactly 3.
    let mut groups: Vec<String> = Vec::new();
    let mut group = String::new();
    for &c in &chars[..sep_idx] {
        if c.is_ascii_digit() {
            group.push(c);
        } else if AMOUNT_THOUSANDS_SEPARATORS.contains(&c) {
            if group.is_empty() {
                return false;
            }
            groups.push(std::mem::take(&mut group));
        } else {
            return false;
        }
    }
    if group.is_empty() {
        return false;
    }
    groups.push(group);
    if groups.len() == 1 {
        return true;
    }
    if groups[0].len() > AMOUNT_THOUSANDS_GROUP_WIDTH {
        return false;
    }
    groups[1..]
        .iter()
        .all(|g| g.len() == AMOUNT_THOUSANDS_GROUP_WIDTH)
}

/// Split `line` at every gap wide enough that `render_spans` would hard-break
/// it internally (mirrors that function's own `gap > 2.5 * size` check,
/// evaluated the same way: per non-space span, against *that* span's own
/// size). Deliberately narrower than the general-purpose `split_line_segments`
/// (used for column-gutter detection, with its own `> 20pt` floor and
/// whole-line max size) — using different thresholds here would split lines
/// `render_spans` was never going to break on its own, feeding
/// `classify_line`/`detect_list_marker` a fragment whose first token (e.g. a
/// lone `-`) looks like a fresh line start and gets misread as a list marker.
/// Matching the threshold exactly means this only pre-splits what would
/// otherwise have broken *mid-render* anyway.
pub(crate) fn split_hard_breaks(line: &[Span], page_width: Option<f64>) -> Vec<Vec<Span>> {
    let mut segments = Vec::new();
    let mut cur: Vec<Span> = Vec::new();
    let mut prev_x: Option<f64> = None;
    let mut prev_advance = 0.0f64;
    for (i, s) in line.iter().enumerate() {
        let is_space = s.text.chars().all(|c| c == ' ');
        if let Some(px) = prev_x {
            if !is_space {
                let size = s.size.max(0.1);
                // Measure the residual *whitespace* between the two spans, the
                // same way `render_spans` does: subtract the previous span's
                // own advance from the start-to-start distance. Comparing the
                // raw start-to-start distance (as this used to) makes any span
                // run wider than ~2.5em look like a new column, so a
                // description drawn as several spans ("Nougat de l'" /
                // "Abbaye" / " 250g") was carved into spurious separate lines
                // even though `render_spans` would have kept it on one.
                let gap = (s.x - px) - prev_advance;
                if wide_gap_breaks(gap, size, 0.0, &cur, &line[i..], page_width)
                    && !cur.is_empty()
                {
                    segments.push(std::mem::take(&mut cur));
                }
            }
        }
        prev_x = Some(s.x);
        prev_advance = s.advance;
        cur.push(s.clone());
    }
    if !cur.is_empty() {
        segments.push(cur);
    }
    segments
}

/// Render one visual line, appending a blank-line paragraph break when the
/// vertical gap from `prev_line_y` is large. Shared by every renderer that
/// walks a flat sequence of lines (`render_with_tables`, `push_band_lines`)
/// so table splicing and column-band splicing apply the exact same
/// heading/list/emphasis rules as plain single-column text.
///
/// A row that jams two unrelated regions onto the same baseline (a value far
/// to the right of its label, or two side-by-side boxes that only share a Y
/// coordinate on this one row) is split first via `split_hard_breaks` and
/// each piece classified/formatted independently. Previously such a row was
/// handed whole to `classify_line`/`format_structured_line` — which pick a
/// single role and a single set of emphasis delimiters for the *entire* row —
/// while `render_spans` separately inserted a bare `\n` mid-string at the
/// same oversized gap. That produced heading/emphasis markers balanced
/// against text that was no longer on the same output line, e.g.
/// `### FACTURE N° :**` (opening `**` never emitted; the stray closer landed
/// on the wrong side of the split). Splitting up front keeps each piece's
/// role and delimiters self-contained.
pub(crate) fn push_line(
    out: &mut String,
    line: &[Span],
    prev_line_y: &mut Option<f64>,
    list_state: &mut ListRunState,
    body_size: f64,
    page_width: Option<f64>,
) {
    if line.is_empty() {
        return;
    }
    let size = line[0].size.max(0.1);
    if let Some(py) = *prev_line_y {
        if py - line[0].y > 2.0 * size {
            out.push('\n');
        }
    }
    for seg in split_hard_breaks(line, page_width) {
        if seg.is_empty() {
            continue;
        }
        let (role, render_slice) = classify_line(&seg, body_size, list_state);
        out.push_str(format_structured_line(&role, &render_spans(render_slice, page_width)).trim_end());
        out.push('\n');
    }
    *prev_line_y = Some(line[0].y);
}

/// Render a `detect_column_bands` result into `out`. A single `Full` band
/// (the common case: no column layout in this stretch of the page) renders
/// byte-identically to walking the same lines one by one — `prev_line_y` and
/// `list_state` carry through unchanged. A `Columns` band renders its left
/// stream fully, then its right stream fully, and a `Stacks` band renders each
/// of its columns left-to-right, each starting its own paragraph/list context
/// (mirroring `render_human_order`'s stream loop) so column content recovered
/// mid-page doesn't inherit spacing or list state from the unrelated column
/// next to it.
pub(crate) fn push_band_lines(
    out: &mut String,
    bands: &[ColumnBand],
    prev_line_y: &mut Option<f64>,
    list_state: &mut ListRunState,
    body_size: f64,
    page_width: Option<f64>,
) {
    for band in bands {
        match band {
            ColumnBand::Full(rows) => {
                for line in rows {
                    push_line(out, line, prev_line_y, list_state, body_size, page_width);
                }
            }
            ColumnBand::Columns { left, right } => {
                if !out.is_empty() && !out.ends_with('\n') {
                    out.push('\n');
                }
                *prev_line_y = None;
                *list_state = ListRunState::default();
                for line in left {
                    push_line(out, line, prev_line_y, list_state, body_size, page_width);
                }
                // Paragraph break between the two column streams. Without it
                // the last line of the left column and the first line of the
                // right column are emitted as consecutive lines, and the
                // paragraph reflow (`reflow.rs`) can join them into one
                // sentence when the left line has no terminator and the right
                // line starts lowercase (WP-C residual: column weld).
                if !right.is_empty() {
                    if !out.ends_with('\n') {
                        out.push('\n');
                    }
                    out.push('\n');
                }
                *prev_line_y = None;
                *list_state = ListRunState::default();
                for line in right {
                    push_line(out, line, prev_line_y, list_state, body_size, page_width);
                }
            }
            ColumnBand::Stacks(columns) => {
                for (ci, col) in columns.iter().enumerate() {
                    // Paragraph break between two column streams (as in
                    // `Columns`), so the reflow pass cannot weld the last line of
                    // one column to the first line of the next. The first column
                    // keeps the surrounding block's spacing state: the line
                    // before the stack (a heading or body line) must still be
                    // able to open a blank line before the stack's first line.
                    if ci > 0 && !col.is_empty() {
                        if !out.is_empty() && !out.ends_with('\n') {
                            out.push('\n');
                        }
                        out.push('\n');
                        *prev_line_y = None;
                        *list_state = ListRunState::default();
                    }
                    for line in col {
                        push_line(out, line, prev_line_y, list_state, body_size, page_width);
                    }
                }
            }
        }
    }
}

/// Render a single visual line to text (no surrounding blank-line logic).
pub fn render_line_text(spans: &[Span], page_width: Option<f64>) -> String {
    render_spans(spans, page_width)
}

// ---------------------------------------------------------------------------
// Inline emphasis rendering (bold / italic / underline)
// ---------------------------------------------------------------------------

/// Split a visual line at any oversized gap into separate visual segments (different columns/margins).
pub fn split_line_segments(line: &[Span], page_width: Option<f64>) -> Vec<Vec<Span>> {
    if line.is_empty() {
        return Vec::new();
    }
    let size = line.iter().map(|s| s.size).fold(0.0f64, f64::max).max(0.1);
    let mut segments: Vec<Vec<Span>> = Vec::new();
    let mut cur: Vec<Span> = Vec::new();
    let mut prev_x: Option<f64> = None;
    let mut prev_advance = 0.0f64;
    for (i, s) in line.iter().enumerate() {
        if let Some(px) = prev_x {
            // Residual whitespace, not the raw start-to-start distance: subtract
            // the previous span's own advance the same way `render_spans` does.
            // A span run wider than the 2.5em / 20pt threshold otherwise looks
            // like a column gutter even when the next span is flush against it,
            // carving one visual line into spurious segments (and feeding
            // `build_doc_blocks` disconnected fragments).
            let gap = (s.x - px) - prev_advance;
            if wide_gap_breaks(gap, size, SEGMENT_MIN_GAP_PT, &cur, &line[i..], page_width)
                && !cur.is_empty()
            {
                segments.push(std::mem::take(&mut cur));
            }
        }
        prev_x = Some(s.x);
        prev_advance = s.advance;
        cur.push(s.clone());
    }
    if !cur.is_empty() {
        segments.push(cur);
    }
    segments
}

#[cfg(test)]
mod monetary_amount_tests {
    use super::is_monetary_amount;

    #[test]
    fn accepts_one_grouped_or_currency_wrapped_amount() {
        for t in [
            "1 234,56",
            "1,234.56",
            "1.234,56",
            "1234.56",
            "-1 234,56",
            "+12,50",
            "1 234,56 EUR",
            "EUR 1 234,56",
            "$1,234.56",
            "-$1,234.56",
            "1 234,56\u{20ac}",
        ] {
            assert!(is_monetary_amount(t), "should accept {t:?}");
        }
    }

    #[test]
    fn rejects_dates_percentages_phones_integers_and_two_values() {
        for t in [
            "12.01.2026",
            "12/01/2026",
            "2026-01-12",
            "12,50 %",
            "12,50%",
            "12,5",
            "12,500",
            "01 23 45 67 89",
            "01.23.45.67",
            "1234",
            "12",
            "99999999",
            "1,23,456.78",
            "9.999,99 9.999,99",
            "1 234,56 9 876,54",
        ] {
            assert!(!is_monetary_amount(t), "should reject {t:?}");
        }
    }
}
