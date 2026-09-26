// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Reading order recovery, multi-column stream separation, and structured DocBlock generation.

use super::*;

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
pub(crate) fn split_hard_breaks(line: &[Span]) -> Vec<Vec<Span>> {
    let mut segments = Vec::new();
    let mut cur: Vec<Span> = Vec::new();
    let mut prev_x: Option<f64> = None;
    let mut prev_advance = 0.0f64;
    for s in line {
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
                if gap > 2.5 * size && !cur.is_empty() {
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
    for seg in split_hard_breaks(line) {
        if seg.is_empty() {
            continue;
        }
        let (role, render_slice) = classify_line(&seg, body_size, list_state);
        out.push_str(format_structured_line(&role, &render_spans(render_slice)).trim_end());
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
) {
    for band in bands {
        match band {
            ColumnBand::Full(rows) => {
                for line in rows {
                    push_line(out, line, prev_line_y, list_state, body_size);
                }
            }
            ColumnBand::Columns { left, right } => {
                if !out.is_empty() && !out.ends_with('\n') {
                    out.push('\n');
                }
                *prev_line_y = None;
                *list_state = ListRunState::default();
                for line in left {
                    push_line(out, line, prev_line_y, list_state, body_size);
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
                    push_line(out, line, prev_line_y, list_state, body_size);
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
                        push_line(out, line, prev_line_y, list_state, body_size);
                    }
                }
            }
        }
    }
}

/// Decide whether a visual line is a page-number footer (numeric-only, in the
/// bottom band of the page).
pub fn is_page_number_line(spans: &[Span], page_height: f64) -> bool {
    let y0 = spans.iter().map(|s| s.y).fold(f64::INFINITY, f64::min);
    let text: String = spans.iter().map(|s| s.text.as_str()).collect();
    let t = text.trim();
    if t.is_empty() {
        return false;
    }
    if y0 < page_height * 0.055 {
        let all_num = t
            .chars()
            .all(|c| c.is_ascii_digit() || c.is_whitespace() || c == '/' || c == '-' || c == '.');
        return all_num && t.len() <= 12;
    }
    false
}

/// Render a single visual line to text (no surrounding blank-line logic).
pub fn render_line_text(spans: &[Span]) -> String {
    render_spans(spans)
}

// ---------------------------------------------------------------------------
// Inline emphasis rendering (bold / italic / underline)
// ---------------------------------------------------------------------------

/// Split a visual line at any oversized gap into separate visual segments (different columns/margins).
pub fn split_line_segments(line: &[Span]) -> Vec<Vec<Span>> {
    if line.is_empty() {
        return Vec::new();
    }
    let size = line.iter().map(|s| s.size).fold(0.0f64, f64::max).max(0.1);
    let mut segments: Vec<Vec<Span>> = Vec::new();
    let mut cur: Vec<Span> = Vec::new();
    let mut prev_x: Option<f64> = None;
    let mut prev_advance = 0.0f64;
    for s in line {
        if let Some(px) = prev_x {
            // Residual whitespace, not the raw start-to-start distance: subtract
            // the previous span's own advance the same way `render_spans` does.
            // A span run wider than the 2.5em / 20pt threshold otherwise looks
            // like a column gutter even when the next span is flush against it,
            // carving one visual line into spurious segments (and feeding
            // `build_doc_blocks` disconnected fragments).
            let gap = (s.x - px) - prev_advance;
            if gap > 2.5 * size && gap > 20.0 && !cur.is_empty() {
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
