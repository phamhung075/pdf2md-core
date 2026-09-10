// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Reading order recovery, multi-column stream separation, and structured DocBlock generation.

use serde::{Deserialize, Serialize};
use crate::layout::glyph_stream::Span;

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

/// Split one visual row at a clearly oversized intra-row gap (a column
/// gutter). Rows without such a gap are single-column rows -> None.
pub fn split_row_columns(spans: &[Span]) -> Option<(Vec<Span>, Vec<Span>)> {
    if spans.len() < 5 {
        return None;
    }
    let size = spans.iter().map(|s| s.size).fold(0.0f64, f64::max).max(0.1);
    let mut best: Option<(f64, usize)> = None; // (gap, split index)
    for i in 0..spans.len() - 1 {
        let a_end = spans[i].x + spans[i].advance;
        let b_start = spans[i + 1].x;
        let gap = (b_start - a_end).max(0.0);
        // A word space is ~0.25em; a true gutter is much wider. Requiring
        // > 1.2em keeps justified prose rows unsplit.
        if gap > 1.2 * size && best.map_or(true, |(g, _)| gap > g) {
            best = Some((gap, i));
        }
    }
    let (_, at) = best?;
    if at < 2 || at >= spans.len() - 3 {
        return None;
    }
    Some((spans[..=at].to_vec(), spans[at + 1..].to_vec()))
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

/// Detect a genuine two-column page via simultaneous row-based gutters.
pub fn page_two_columns_rows(lines: &[Vec<Span>]) -> Option<PageColumns> {
    if lines.len() < 3 {
        return None;
    }
    struct Split {
        y: f64,
        left: Vec<Span>,
        right: Vec<Span>,
        gutter_x: f64,
    }
    let mut splits: Vec<Split> = Vec::new();
    for l in lines {
        if let Some((left, right)) = split_row_columns(l) {
            let l_end = left
                .iter()
                .map(|x| x.x + x.advance)
                .fold(f64::NEG_INFINITY, f64::max);
            let r_start = right.iter().map(|x| x.x).fold(f64::INFINITY, f64::min);
            splits.push(Split {
                y: l[0].y,
                left,
                right,
                gutter_x: (l_end + r_start) / 2.0,
            });
        }
    }
    if splits.len() < 3 {
        return None;
    }
    let mut gxs: Vec<f64> = splits.iter().map(|s| s.gutter_x).collect();
    gxs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let med = gxs[gxs.len() / 2];
    let tol = (0.15 * med.abs()).max(6.0);
    let keep: Vec<Split> = splits
        .into_iter()
        .filter(|s| (s.gutter_x - med).abs() <= tol)
        .collect();
    if keep.len() < 3 {
        return None;
    }
    let rows_with_text = lines.iter().filter(|l| l.len() >= 3).count().max(1);
    if keep.len() * 2 < rows_with_text {
        return None;
    }
    let col_top = keep.iter().map(|s| s.y).fold(f64::NEG_INFINITY, f64::max);
    let mut left: Vec<(f64, Vec<Span>)> = Vec::new();
    let mut right: Vec<(f64, Vec<Span>)> = Vec::new();
    let mut top_full: Vec<Vec<Span>> = Vec::new();
    let mut bottom_full: Vec<Vec<Span>> = Vec::new();
    for l in lines {
        let y = l[0].y;
        let y_tol = 0.5 * l[0].size.max(0.1);
        if let Some(sp) = keep.iter().find(|s| (s.y - y).abs() < y_tol) {
            left.push((sp.y, sp.left.clone()));
            right.push((sp.y, sp.right.clone()));
            continue;
        }
        if y > col_top + y_tol {
            top_full.push(l.clone());
        } else {
            bottom_full.push(l.clone());
        }
    }
    top_full.sort_by(|a, b| {
        b[0].y
            .partial_cmp(&a[0].y)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    bottom_full.sort_by(|a, b| {
        b[0].y
            .partial_cmp(&a[0].y)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    // Prose gate: a true text column is made of multi-word lines on both
    // sides where the words are spaced like a sentence. Pipe-table rows have
    // short tokens and wide cell gutters inside the "line", so they must keep
    // flowing through the table detector.
    let wc = |rows: &[(f64, Vec<Span>)]| -> f64 {
        if rows.is_empty() {
            return 0.0;
        }
        rows.iter()
            .map(|(_, v)| v.iter().filter(|sp| !sp.text.trim().is_empty()).count() as f64)
            .sum::<f64>()
            / rows.len() as f64
    };
    if wc(&left) < 2.5 || wc(&right) < 2.5 {
        return None;
    }
    // No half may contain a column-wide gutter inside it (that would mean the
    // "column" still holds multiple table cells).
    let clean = |rows: &[(f64, Vec<Span>)]| -> bool {
        rows.iter().all(|(_, v)| {
            let size = v.iter().map(|x| x.size).fold(0.0f64, f64::max).max(0.1);
            v.windows(2).all(|p| {
                let a_end = p[0].x + p[0].advance;
                let gap = (p[1].x - a_end).max(0.0);
                gap <= 1.2 * size
            })
        })
    };
    if !clean(&left) || !clean(&right) {
        return None;
    }
    left.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    right.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    Some(PageColumns {
        top_full,
        left: left.into_iter().map(|(_, v)| v).collect(),
        right: right.into_iter().map(|(_, v)| v).collect(),
        bottom_full,
    })
}

/// Detect two columns via vertical projection gutter across the page (handles staggered
/// lines such as sidebar panels next to body text where baselines do not align).
pub fn detect_projection_two_columns(lines: &[Vec<Span>]) -> Option<PageColumns> {
    if lines.len() < 4 {
        return None;
    }

    struct LineBox {
        y: f64,
        x0: f64,
        x1: f64,
        size: f64,
        line: Vec<Span>,
    }
    let mut boxes = Vec::with_capacity(lines.len());
    let mut min_x = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    for l in lines {
        if l.is_empty() {
            continue;
        }
        let x0 = l.iter().map(|s| s.x).fold(f64::INFINITY, f64::min);
        let x1 = l.iter().map(|s| s.x + s.advance).fold(f64::NEG_INFINITY, f64::max);
        let size = l.iter().map(|s| s.size).fold(0.0f64, f64::max).max(0.1);
        min_x = min_x.min(x0);
        max_x = max_x.max(x1);
        boxes.push(LineBox {
            y: l[0].y,
            x0,
            x1,
            size,
            line: l.clone(),
        });
    }
    if max_x - min_x < 120.0 {
        return None;
    }

    let mut candidate_xs: Vec<f64> = Vec::new();
    for b in &boxes {
        if b.x1 > min_x + 50.0 && b.x1 < max_x - 50.0 {
            candidate_xs.push(b.x1 + 5.0);
        }
        for seg in split_line_segments(&b.line) {
            let seg_x1 = seg.iter().map(|s| s.x + s.advance).fold(f64::NEG_INFINITY, f64::max);
            if seg_x1 > min_x + 50.0 && seg_x1 < max_x - 50.0 {
                candidate_xs.push(seg_x1 + 5.0);
            }
        }
    }
    candidate_xs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    candidate_xs.dedup_by(|a, b| (*a - *b).abs() < 5.0);

    let mut best_gutter: Option<(f64, f64, f64)> = None;

    for &gx in &candidate_xs {
        let mut left_count = 0;
        let mut right_count = 0;
        let mut max_l_x = f64::NEG_INFINITY;
        let mut min_r_x = f64::INFINITY;
        let mut crossing_count = 0;

        for b in &boxes {
            if b.x1 <= gx {
                left_count += 1;
                max_l_x = max_l_x.max(b.x1);
            } else if b.x0 >= gx {
                right_count += 1;
                min_r_x = min_r_x.min(b.x0);
            } else {
                let left_spans: Vec<Span> = b.line.iter().filter(|s| s.x + s.advance <= gx).cloned().collect();
                let right_spans: Vec<Span> = b.line.iter().filter(|s| s.x >= gx).cloned().collect();
                if !left_spans.is_empty() && !right_spans.is_empty() {
                    let l_end = left_spans.iter().map(|s| s.x + s.advance).fold(f64::NEG_INFINITY, f64::max);
                    let r_start = right_spans.iter().map(|s| s.x).fold(f64::INFINITY, f64::min);
                    if r_start - l_end >= 1.2 * b.size {
                        left_count += 1;
                        right_count += 1;
                        max_l_x = max_l_x.max(l_end);
                        min_r_x = min_r_x.min(r_start);
                        continue;
                    }
                }
                crossing_count += 1;
            }
        }

        let gutter_width = min_r_x - max_l_x;
        if gutter_width >= 15.0 && left_count >= 3 && right_count >= 3 {
            let score = gutter_width * (left_count.min(right_count) as f64) - (crossing_count as f64 * 100.0);
            if crossing_count <= 2 && best_gutter.map_or(true, |(_, _, s)| score > s) {
                best_gutter = Some((max_l_x, min_r_x, score));
            }
        }
    }

    let (gl, gr, _) = best_gutter?;
    let gx = (gl + gr) / 2.0;

    let mut col_top = f64::NEG_INFINITY;
    let mut col_bottom = f64::INFINITY;
    for b in &boxes {
        if b.x1 <= gx || b.x0 >= gx {
            col_top = col_top.max(b.y);
            col_bottom = col_bottom.min(b.y);
        }
    }

    let mut top_full: Vec<Vec<Span>> = Vec::new();
    let mut bottom_full: Vec<Vec<Span>> = Vec::new();
    let mut left: Vec<(f64, Vec<Span>)> = Vec::new();
    let mut right: Vec<(f64, Vec<Span>)> = Vec::new();

    for b in boxes {
        if b.x0 < gx && b.x1 > gx {
            let left_spans: Vec<Span> = b.line.iter().filter(|s| s.x + s.advance <= gx).cloned().collect();
            let right_spans: Vec<Span> = b.line.iter().filter(|s| s.x >= gx).cloned().collect();
            if !left_spans.is_empty() && !right_spans.is_empty() {
                left.push((b.y, left_spans));
                right.push((b.y, right_spans));
                continue;
            }
            if b.y >= col_top - 5.0 {
                top_full.push(b.line);
            } else if b.y <= col_bottom + 5.0 {
                bottom_full.push(b.line);
            }
            continue;
        }

        if b.x1 <= gx {
            left.push((b.y, b.line));
        } else {
            right.push((b.y, b.line));
        }
    }

    left.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    right.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    top_full.sort_by(|a, b| b[0].y.partial_cmp(&a[0].y).unwrap_or(std::cmp::Ordering::Equal));
    bottom_full.sort_by(|a, b| b[0].y.partial_cmp(&a[0].y).unwrap_or(std::cmp::Ordering::Equal));

    Some(PageColumns {
        top_full,
        left: left.into_iter().map(|(_, v)| v).collect(),
        right: right.into_iter().map(|(_, v)| v).collect(),
        bottom_full,
    })
}

/// Detect a genuine two-column page and produce reading-order streams:
/// full-width rows above the column block, left column top-to-bottom, right
/// column top-to-bottom, full-width rows below. Returns None when the page is
/// not convincingly two-column.
pub fn page_two_columns(lines: &[Vec<Span>]) -> Option<PageColumns> {
    if let Some(pc) = page_two_columns_rows(lines) {
        return Some(pc);
    }
    detect_projection_two_columns(lines)
}

/// Human reading order for the page as streams of visual lines: single-column
/// pages produce one stream (top-down); two-column pages produce full-width
/// header rows, left column, right column, footer rows.
pub fn page_read_order(lines: &[Vec<Span>]) -> Vec<Vec<Vec<Span>>> {
    if let Some(pc) = page_two_columns(lines) {
        let mut streams = Vec::new();
        if !pc.top_full.is_empty() {
            streams.push(pc.top_full);
        }
        streams.push(pc.left);
        streams.push(pc.right);
        if !pc.bottom_full.is_empty() {
            streams.push(pc.bottom_full);
        }
        return streams;
    }
    vec![lines.to_vec()]
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

/// Inline text style of one span run. A span is drawn entirely in one font so
/// its bold/italic/underline state is uniform.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
struct InlineStyle {
    bold: bool,
    italic: bool,
    underline: bool,
}

impl InlineStyle {
    fn of(span: &Span) -> Self {
        Self {
            bold: span.is_bold,
            italic: span.is_italic,
            underline: span.is_underline,
        }
    }
}

/// Open the Markdown emphasis delimiters for `st`. Order matters for the
/// combined bold+italic case: `<u>` then `*` then `**` yields `***text***`
/// (bold-italic) wrapped in `<u>`.
fn open_style(out: &mut String, st: InlineStyle) {
    if st.underline {
        out.push_str("<u>");
    }
    if st.italic {
        out.push('*');
    }
    if st.bold {
        out.push_str("**");
    }
}

/// Close the Markdown emphasis delimiters for `st` (reverse order of `open_style`).
fn close_style(out: &mut String, st: InlineStyle) {
    if st.bold {
        out.push_str("**");
    }
    if st.italic {
        out.push('*');
    }
    if st.underline {
        out.push_str("</u>");
    }
}

/// Render one visual line's spans to text with inline `**bold**` / `*italic*` /
/// `<u>underline</u>` emphasis. The spatial spacing rules are identical to the
/// legacy plain-text renderer (`render_cluster`), so a line of unstyled spans
/// produces byte-identical output; only runs whose style is non-plain gain
/// emphasis delimiters.
pub(crate) fn render_spans(line: &[Span]) -> String {
    let mut out = String::new();
    let mut prev_x: Option<f64> = None;
    let mut prev_advance = 0.0f64;
    let mut cur = InlineStyle::default();

    for span in line {
        if span.text.is_empty() {
            continue;
        }
        let size = span.size.max(0.1);
        let space_adv = 0.25 * size;
        let is_space = span.text.chars().all(|c| c == ' ');

        if let Some(px) = prev_x {
            let gap = span.x - px;
            if !is_space {
                if gap > 2.5 * size {
                    // A distinct column / element on the same row: break the
                    // line AND close any open emphasis first.
                    close_style(&mut out, cur);
                    cur = InlineStyle::default();
                    if !out.is_empty() && !out.ends_with('\n') {
                        out.push('\n');
                    }
                } else if gap - prev_advance > 0.65 * space_adv {
                    close_style(&mut out, cur);
                    cur = InlineStyle::default();
                    if !out.is_empty() && !out.ends_with(' ') && !out.ends_with('\n') {
                        out.push(' ');
                    }
                }
            }
        }

        if is_space {
            // A space glyph is never emphasized; close any open emphasis so the
            // delimiter sits flush against the word (Markdown rejects emphasis
            // with leading/trailing spaces inside the delimiters).
            if !out.is_empty() && !out.ends_with(' ') && !out.ends_with('\n') {
                close_style(&mut out, cur);
                cur = InlineStyle::default();
                out.push(' ');
            }
        } else {
            let st = InlineStyle::of(span);
            if st != cur {
                close_style(&mut out, cur);
                open_style(&mut out, st);
                cur = st;
            }
            out.push_str(&span.text);
        }

        prev_x = Some(span.x);
        prev_advance = span.advance;
    }

    close_style(&mut out, cur);
    out.trim_end().to_string()
}

/// Render pre-built visual lines to plain text (block/paragraph separation +
/// word gaps). Line model must come from `build_lines`.
pub fn render_cluster(lines: &[Vec<Span>]) -> String {
    if lines.is_empty() {
        return String::new();
    }

    let body_size = body_size_for(lines);
    let mut list_state = ListRunState::default();
    let mut out = String::new();
    let mut prev_line_y: Option<f64> = None;

    for line in lines {
        let size = line[0].size.max(0.1);

        // A large vertical gap opens a new block (paragraph).
        if let Some(py) = prev_line_y {
            if py - line[0].y > 2.0 * size {
                out.push('\n');
            }
        }

        let (role, render_slice) = classify_line(line, body_size, &mut list_state);
        let line_text = format_structured_line(&role, &render_spans(render_slice));

        out.push_str(line_text.trim_end());
        out.push('\n');
        prev_line_y = Some(line[0].y);
    }

    out.trim_end().to_string()
}

/// Render page text in human reading order. When the page is a single column
/// and nothing was removed, output equals `render_cluster` byte-for-byte.
pub fn render_human_order(lines: &[Vec<Span>], page_height: f64, drop_furniture: bool) -> String {
    let streams = page_read_order(lines);
    if streams.len() == 1 {
        // Single column: identical to the plain renderer unless we strip
        // furniture lines (page numbers).
        if !drop_furniture {
            return render_cluster(lines);
        }
        let keep: Vec<Vec<Span>> = lines
            .iter()
            .filter(|l| !is_page_number_line(l, page_height))
            .cloned()
            .collect();
        return render_cluster(&keep);
    }
    let body_size = body_size_for(lines);
    let mut out = String::new();
    for (ci, stream) in streams.iter().enumerate() {
        if ci > 0 && !out.is_empty() {
            out.push('\n');
        }
        let mut prev_y: Option<f64> = None;
        let mut list_state = ListRunState::default();
        for line in stream {
            if drop_furniture && is_page_number_line(line, page_height) {
                continue;
            }
            let size = line[0].size.max(0.1);
            if let Some(py) = prev_y {
                if py - line[0].y > 2.0 * size {
                    out.push('\n');
                }
            }
            let (role, render_slice) = classify_line(line, body_size, &mut list_state);
            out.push_str(&format_structured_line(&role, &render_line_text(render_slice)));
            out.push('\n');
            prev_y = Some(line[0].y);
        }
    }
    out.trim_end().to_string()
}

/// Split a visual line at any oversized gap into separate visual segments (different columns/margins).
pub fn split_line_segments(line: &[Span]) -> Vec<Vec<Span>> {
    if line.is_empty() {
        return Vec::new();
    }
    let size = line.iter().map(|s| s.size).fold(0.0f64, f64::max).max(0.1);
    let mut segments: Vec<Vec<Span>> = Vec::new();
    let mut cur: Vec<Span> = Vec::new();
    let mut prev_x: Option<f64> = None;
    for s in line {
        if let Some(px) = prev_x {
            let gap = s.x - px;
            if gap > 2.5 * size && gap > 20.0 && !cur.is_empty() {
                segments.push(std::mem::take(&mut cur));
            }
        }
        prev_x = Some(s.x);
        cur.push(s.clone());
    }
    if !cur.is_empty() {
        segments.push(cur);
    }
    segments
}

/// Robustly estimate the body (regular prose) font size for a page.
///
/// The median is a poor anchor for heading detection: when a page carries many
/// mid-size headings (outlines, structured reports), the heading sizes pull the
/// median up to the heading size, so `size >= body * 1.25` no longer fires and
/// the headings hide themselves. Instead use the most frequent size class — the
/// mode — and, when several classes tie for frequency, take the *smallest* of
/// them. Body text is the smallest regular size class and headings are larger,
/// so this keeps the heading threshold anchored on the body and never lets the
/// headings inflate it.
fn estimate_body_size(sizes: &[f64]) -> f64 {
    use std::collections::HashMap;
    if sizes.is_empty() {
        return 10.0;
    }
    let mut freq: HashMap<String, usize> = HashMap::new();
    for s in sizes {
        // Quantize to a tenth of a point so metric rounding doesn't split a
        // single nominal size into many near-identical keys.
        let key = format!("{:.1}", (s * 10.0).round() / 10.0);
        *freq.entry(key).or_insert(0) += 1;
    }
    let max_freq = freq.values().copied().max().unwrap_or(0);
    let mut best: Option<f64> = None;
    for s in sizes {
        let key = format!("{:.1}", (s * 10.0).round() / 10.0);
        if freq.get(&key).copied().unwrap_or(0) == max_freq {
            best = Some(match best {
                None => *s,
                Some(b) => b.min(*s),
            });
        }
    }
    best.unwrap_or(10.0).max(1.0)
}

/// Computes the body (regular prose) font size for a full page's lines — the
/// same anchor `build_doc_blocks` already uses to classify a line as a
/// title/heading, now also the anchor the render functions below use to
/// decide when to emit `#`/`##`/`###` instead of flat text.
pub(crate) fn body_size_for(lines: &[Vec<Span>]) -> f64 {
    let sizes: Vec<f64> = lines
        .iter()
        .map(|l| l.iter().map(|s| s.size).fold(0.0f64, f64::max))
        .filter(|s| *s > 0.0)
        .collect();
    estimate_body_size(&sizes).max(1.0)
}

// ---------------------------------------------------------------------------
// Structural Markdown emission (R7): headings (#, ##, ###) and lists (-, 1.)
// ---------------------------------------------------------------------------
//
// `build_doc_blocks` above already classifies each line into "title" /
// "heading" / "list" / "body" for the JSON `blocks` output, but nothing
// consulted that classification when building the actual Markdown string —
// every renderer in this module emitted flat text with only inline
// `**bold**`/`*italic*` emphasis, regardless of a line's role. A heading
// looked exactly like a paragraph that happened to be bold.
//
// This section is the shared classifier + formatter every render entry point
// (`render_cluster`, `render_human_order`, `render_math_stream` in
// latex_math.rs, `render_with_tables` in tables/mod.rs) now calls per line,
// so the emitted Markdown and the `blocks` JSON list are never a "heading"
// according to one and flat text according to the other.
//
// The heading-level thresholds mirror `layout::semantic::detect_heading`
// (a statistical 3-level H1/H2/H3 classifier that already existed, already
// tested, but lived only in the separate `ModernLayoutEngine`/`xy_cut`
// pipeline `convert_pdf_bytes_to_markdown` never calls) adapted onto the
// `Span`-based lines this live pipeline actually uses, anchored on
// `body_size_for`'s mode-based estimate rather than semantic.rs's median
// (see `estimate_body_size`'s own doc comment for why the mode is the safer
// anchor). List-item detection similarly mirrors
// `layout::semantic::detect_list_item`'s marker checks (bullets, checkboxes,
// ordered markers), adapted to slice the marker off a `Span` line instead of
// a `TextLine`'s first `TextWord`.

/// One visual line's structural role.
#[derive(Debug)]
pub(crate) enum LineRole {
    Heading(u8),
    List { depth: u8, ordered: bool, ordinal: usize },
    Body,
}

/// Per-render-pass state so consecutive list items share one indent anchor
/// and ordered items number consecutively; resets whenever a heading or a
/// non-list body line breaks the run (mirrors normal Markdown list
/// semantics: a blank/prose line ends the list).
#[derive(Default)]
pub(crate) struct ListRunState {
    base_x: Option<f64>,
    counters: Vec<usize>,
}

impl ListRunState {
    fn end_run(&mut self) {
        self.base_x = None;
        self.counters.clear();
    }

    /// Depth 0 is the first list item's own indent; deeper items are bucketed
    /// in units of `1.5 * body_size` from that anchor (mirrors
    /// `layout::semantic::detect_list_item`'s indent-to-depth formula).
    fn depth_for(&mut self, x: f64, body_size: f64) -> u8 {
        let base = *self.base_x.get_or_insert(x);
        let raw = (x - base) / (1.5 * body_size.max(1.0));
        raw.round().clamp(0.0, 4.0) as u8
    }

    /// Next ordinal for an ordered item at `depth`; resets any deeper
    /// counters (a new depth-0 item restarts nested numbering underneath it).
    fn next_ordinal(&mut self, depth: u8) -> usize {
        let d = depth as usize;
        if self.counters.len() <= d {
            self.counters.resize(d + 1, 0);
        }
        for c in self.counters.iter_mut().skip(d + 1) {
            *c = 0;
        }
        self.counters[d] += 1;
        self.counters[d]
    }
}

/// Statistical 3-level heading detector — see this section's module-level
/// doc comment for provenance. Returns `None` for anything that doesn't look
/// like a heading, including prose that merely happens to be short or bold.
fn detect_heading_level(line: &[Span], body_size: f64) -> Option<u8> {
    let sized: Vec<&Span> = line.iter().filter(|s| !s.text.trim().is_empty()).collect();
    if sized.is_empty() {
        return None;
    }
    let plain: String = sized.iter().map(|s| s.text.as_str()).collect();
    let trimmed = plain.trim();
    if trimmed.is_empty() || trimmed.chars().count() > 180 {
        return None;
    }
    // Trailing full stop with no letters at all, or two-plus sentence
    // breaks, reads as prose rather than a heading.
    if trimmed.ends_with('.') && !trimmed.contains(|c: char| c.is_ascii_alphabetic()) {
        return None;
    }
    if trimmed.matches(". ").count() >= 2 {
        return None;
    }

    let total_chars: f64 = sized.iter().map(|s| s.text.chars().count().max(1) as f64).sum();
    let avg_fs = sized
        .iter()
        .map(|s| s.size * s.text.chars().count().max(1) as f64)
        .sum::<f64>()
        / total_chars.max(1.0);
    let is_bold = sized.iter().filter(|s| s.is_bold).count() * 2 > sized.len();

    if avg_fs >= 1.6 * body_size || (avg_fs >= 1.4 * body_size && is_bold) {
        return Some(1);
    }
    if avg_fs >= 1.3 * body_size {
        return Some(2);
    }
    if avg_fs >= 1.15 * body_size && is_bold {
        return Some(3);
    }
    None
}

/// Detects a leading list marker (bullet, checkbox, or ordered) on `line`'s
/// first non-space span and returns `(ordered, spans_to_skip)` — how many
/// leading spans are the marker itself (plus a following pure-space span),
/// to be sliced off before rendering the item's own text. A checkbox marker
/// (`[ ]`/`[x]`) is kept as part of the item text (GFM task-list syntax is
/// `- [ ] text`, not a separate marker), so it reports 0 spans to skip.
fn detect_list_marker(line: &[Span]) -> Option<(bool, usize)> {
    let mut idx = 0;
    while idx < line.len() && line[idx].text.trim().is_empty() {
        idx += 1;
    }
    let w0 = line.get(idx)?.text.trim();
    if w0.is_empty() {
        return None;
    }

    let is_checkbox = w0 == "[ ]" || w0.eq_ignore_ascii_case("[x]");
    if is_checkbox {
        // Require item text after the checkbox; a lone checkbox is noise.
        return if idx + 1 < line.len() { Some((false, idx)) } else { None };
    }

    let is_bullet = matches!(w0, "-" | "*" | "•" | "●" | "+" | "◦" | "▪");
    let is_ordered = if (w0.ends_with('.') || w0.ends_with(')')) && w0.len() <= 5 {
        let digits = &w0[..w0.len() - 1];
        !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit())
    } else if (w0.starts_with('(') && w0.ends_with(')')) || (w0.starts_with('[') && w0.ends_with(']')) {
        let inner = &w0[1..w0.len().saturating_sub(1)];
        !inner.is_empty() && inner.chars().all(|c| c.is_ascii_digit())
    } else {
        false
    };
    if !is_bullet && !is_ordered {
        return None;
    }

    let mut skip = idx + 1;
    if skip < line.len() && line[skip].text.chars().all(|c| c == ' ') {
        skip += 1;
    }
    if skip >= line.len() {
        return None; // marker with no item text
    }
    Some((is_ordered, skip))
}

/// Classifies one visual line, threading `list_state` across consecutive
/// calls in one render pass. Returns the role plus the span slice the caller
/// should actually render as the line's text — the marker sliced off for a
/// list item (the caller prefixes the Markdown bullet/number itself instead),
/// the full line otherwise.
pub(crate) fn classify_line<'a>(
    line: &'a [Span],
    body_size: f64,
    list_state: &mut ListRunState,
) -> (LineRole, &'a [Span]) {
    if line.is_empty() {
        return (LineRole::Body, line);
    }
    if let Some(level) = detect_heading_level(line, body_size) {
        list_state.end_run();
        return (LineRole::Heading(level), line);
    }
    if let Some((ordered, skip)) = detect_list_marker(line) {
        let depth = list_state.depth_for(line[0].x, body_size);
        let ordinal = list_state.next_ordinal(depth);
        return (LineRole::List { depth, ordered, ordinal }, &line[skip..]);
    }
    list_state.end_run();
    (LineRole::Body, line)
}

/// Strips one or more layers of outer `**`/`*`/`<u></u>` wrapping from an
/// already-rendered line. Headings render as clean text rather than
/// redundantly double-marking a `## **Heading**` when the source line
/// happened to be entirely bold — exactly the common case, since bold is one
/// of `detect_heading_level`'s own signals (the H3 threshold requires it).
fn strip_outer_emphasis(s: &str) -> String {
    let mut t = s.trim();
    loop {
        if let Some(inner) = t.strip_prefix("<u>").and_then(|r| r.strip_suffix("</u>")) {
            t = inner.trim();
            continue;
        }
        if let Some(inner) = t.strip_prefix("**").and_then(|r| r.strip_suffix("**")) {
            t = inner.trim();
            continue;
        }
        if let Some(inner) = t.strip_prefix('*').and_then(|r| r.strip_suffix('*')) {
            t = inner.trim();
            continue;
        }
        break;
    }
    t.to_string()
}

/// Formats a classified line's already-rendered inline text with its
/// structural Markdown prefix. `inline_text` must come from rendering
/// `classify_line`'s returned span slice (the marker-stripped remainder for
/// a list item), not the original full line.
pub(crate) fn format_structured_line(role: &LineRole, inline_text: &str) -> String {
    match role {
        LineRole::Heading(level) => {
            let hashes = "#".repeat((*level).clamp(1, 6) as usize);
            let text = strip_outer_emphasis(inline_text);
            if text.is_empty() {
                inline_text.to_string()
            } else {
                format!("{hashes} {text}")
            }
        }
        LineRole::List { depth, ordered, ordinal } => {
            let indent = "  ".repeat(*depth as usize);
            if *ordered {
                format!("{indent}{ordinal}. {inline_text}")
            } else {
                format!("{indent}- {inline_text}")
            }
        }
        LineRole::Body => inline_text.to_string(),
    }
}

/// Build the structured block list for a glyph page in reading order.
pub fn build_doc_blocks(lines: &[Vec<Span>], page_height: f64) -> Vec<DocBlock> {
    let sizes: Vec<f64> = lines
        .iter()
        .map(|l| l.iter().map(|s| s.size).fold(0.0f64, f64::max))
        .filter(|s| *s > 0.0)
        .collect();
    let body = estimate_body_size(&sizes).max(1.0);
    let title_size = body * 1.6;
    let max_y = lines
        .iter()
        .map(|l| l[0].y)
        .fold(f64::NEG_INFINITY, f64::max);

    let mut blocks: Vec<DocBlock> = Vec::new();
    for stream in page_read_order(lines) {
        for line in stream {
            if is_page_number_line(&line, page_height) {
                continue;
            }
            for seg in split_line_segments(&line) {
                let size = seg.iter().map(|s| s.size).fold(0.0f64, f64::max).max(1.0);
                let x0 = seg.iter().map(|s| s.x).fold(f64::INFINITY, f64::min);
                let x1 = seg
                    .iter()
                    .map(|s| s.x + s.advance)
                    .fold(f64::NEG_INFINITY, f64::max);
                let baseline_min = seg.iter().map(|s| s.y).fold(f64::INFINITY, f64::min);
                let baseline_max = seg.iter().map(|s| s.y).fold(f64::NEG_INFINITY, f64::max);
                // Real typographic bounding box centered on visual baseline
                let y0 = baseline_min - 0.5 * size;
                let y1 = baseline_max + 0.5 * size;
                let text = render_line_text(&seg);
                if text.trim().is_empty() {
                    continue;
                }
                let is_line_bold = !seg.is_empty() && seg.iter().any(|s| s.is_bold);
                let is_line_italic = !seg.is_empty() && seg.iter().any(|s| s.is_italic);
                let is_line_underline = !seg.is_empty() && seg.iter().any(|s| s.is_underline);
                let kind = if size >= title_size && baseline_min >= max_y - 2.0 {
                    "title"
                } else if size >= body * 1.25 || (is_line_bold && size >= body * 1.05) {
                    "heading"
                } else {
                    let t = text.trim_start();
                    // A leading `*` is a bullet only when it is followed by
                    // whitespace (e.g. `* item`); `*italic*` is inline emphasis.
                    let is_star_bullet =
                        t.starts_with('*') && t[1..].chars().next().map_or(false, |c| c.is_whitespace());
                    let is_bullet = t.starts_with('-')
                        || t.starts_with('•')
                        || t.starts_with('●')
                        || t.starts_with('◦')
                        || t.starts_with('·')
                        || is_star_bullet;
                    if is_bullet {
                        "list"
                    } else {
                        "body"
                    }
                };
                blocks.push(DocBlock {
                    page: 0,
                    kind: kind.to_string(),
                    x0,
                    y0,
                    x1,
                    y1,
                    text,
                    is_bold: is_line_bold,
                    is_italic: is_line_italic,
                    is_underline: is_line_underline,
                });
            }
        }
    }
    merge_paragraph_lines(blocks)
}

/// Merges consecutive "body" blocks that read as one continuous paragraph
/// into a single block, instead of leaving one block per *visual line*
/// (typically 4-7 words). Two problems this fixes directly: RAG chunkers
/// consuming the `blocks` JSON see whole paragraphs instead of shattered
/// single lines, and CheckColumnInterleaving (which needs >= 10 real prose
/// blocks on a page before its column-alternation check even engages) was
/// starved of blocks to reason about.
///
/// Two adjacent "body" lines merge only when ALL of:
///   - kind boundary: both are "body" — a heading/list/table-zone/figure/
///     caption/header/footer never merges with anything, in either direction
///     (enforced simply by requiring `kind == "body"` on both sides: a
///     non-body line always closes out whatever paragraph came before it).
///   - line pitch: the vertical gap between them is normal single-spaced
///     leading, not a paragraph break — capped at 1.8x the line's own font
///     size (`y1 - y0`).
///   - left-edge alignment: within ~3pt of the paragraph's established body
///     indent — except the *first* merge into a paragraph, which is exempt so
///     a first-line indent doesn't wrongly split a paragraph from its own
///     second line; the second line then sets the body indent every further
///     line in that paragraph must match.
///
/// De-hyphenation: when the earlier line's text ends in a hyphen preceded by
/// a letter (a line-wrap break, not a bullet/dash/range), the hyphen is
/// dropped and the next line's text is joined directly with no space;
/// otherwise a single space joins them.
fn merge_paragraph_lines(blocks: Vec<DocBlock>) -> Vec<DocBlock> {
    const LEFT_EDGE_TOL: f64 = 3.0;
    const MAX_PITCH_RATIO: f64 = 1.8;

    /// A paragraph being accumulated: `block` grows in place (text joined,
    /// bbox unioned) as more lines merge into it.
    struct Para {
        block: DocBlock,
        line_count: usize,
        body_x0: f64,
        last_y0: f64,
        last_size: f64,
    }

    let mut out: Vec<DocBlock> = Vec::with_capacity(blocks.len());
    let mut cur: Option<Para> = None;

    for b in blocks {
        if b.kind != "body" {
            if let Some(p) = cur.take() {
                out.push(p.block);
            }
            out.push(b);
            continue;
        }

        if let Some(p) = &mut cur {
            let gap = p.last_y0 - b.y1;
            let pitch_ok = gap >= 0.0 && gap <= MAX_PITCH_RATIO * p.last_size;
            // The paragraph's first merge (bringing in its 2nd line) is
            // exempt from the left-edge check — the first line may carry a
            // first-line indent that legitimately differs from the body's
            // real left edge, which the 2nd line then establishes for every
            // merge after this one (see the `line_count == 1` branch below).
            let edge_ok = p.line_count == 1 || (b.x0 - p.body_x0).abs() <= LEFT_EDGE_TOL;
            if pitch_ok && edge_ok {
                join_paragraph_text(&mut p.block.text, &b.text);
                p.block.x0 = p.block.x0.min(b.x0);
                p.block.y0 = p.block.y0.min(b.y0);
                p.block.x1 = p.block.x1.max(b.x1);
                p.block.y1 = p.block.y1.max(b.y1);
                p.block.is_bold |= b.is_bold;
                p.block.is_italic |= b.is_italic;
                p.block.is_underline |= b.is_underline;
                if p.line_count == 1 {
                    // The paragraph's first line may carry a first-line
                    // indent; its *second* line establishes the real body
                    // left edge every subsequent line must match.
                    p.body_x0 = b.x0;
                }
                p.line_count += 1;
                p.last_y0 = b.y0;
                p.last_size = (b.y1 - b.y0).max(1.0);
                continue;
            }
            out.push(cur.take().unwrap().block);
        }

        cur = Some(Para {
            block: b.clone(),
            line_count: 1,
            body_x0: b.x0,
            last_y0: b.y0,
            last_size: (b.y1 - b.y0).max(1.0),
        });
    }
    if let Some(p) = cur.take() {
        out.push(p.block);
    }
    out
}

/// Joins `next` onto `text` as a paragraph continuation: de-hyphenates a
/// genuine line-wrap break (a hyphen preceded by a letter, e.g. "infor-" +
/// "mation" -> "information"), otherwise joins with a plain space.
fn join_paragraph_text(text: &mut String, next: &str) {
    let trimmed_len = text.trim_end().len();
    let is_hyphenated_break = trimmed_len > 0
        && text.as_bytes()[..trimmed_len].last() == Some(&b'-')
        && text[..trimmed_len - 1]
            .chars()
            .last()
            .map_or(false, |c| c.is_alphabetic());

    text.truncate(trimmed_len);
    if is_hyphenated_break {
        text.pop(); // drop the trailing '-'
        text.push_str(next.trim_start());
    } else {
        text.push(' ');
        text.push_str(next);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::glyph_stream::Span;

    fn span(text: &str, x: f64, style: (bool, bool, bool)) -> Span {
        Span {
            text: text.to_string(),
            x,
            y: 700.0,
            size: 10.0,
            advance: text.len() as f64 * 6.0,
            is_bold: style.0,
            is_italic: style.1,
            is_underline: style.2,
            is_vertical: false,
        }
    }

    /// A word + explicit space span + word (space spans are the common case
    /// from PDF producers).
    fn words_spaced(pairs: &[(&str, (bool, bool, bool))]) -> Vec<Span> {
        let mut v = Vec::new();
        let mut x = 100.0;
        for (i, (t, st)) in pairs.iter().enumerate() {
            if i > 0 {
                v.push(span(" ", x, (false, false, false)));
                x += 3.0;
            }
            v.push(span(t, x, *st));
            x += text_width(t) + 2.0;
        }
        v
    }

    fn text_width(t: &str) -> f64 {
        t.len() as f64 * 6.0
    }

    #[test]
    fn unstyled_line_has_no_markers() {
        let line = words_spaced(&[("Hello", (false, false, false)), ("world", (false, false, false))]);
        assert_eq!(render_spans(&line), "Hello world");
    }

    #[test]
    fn bold_run_is_wrapped_in_asterisks_per_word() {
        let line = words_spaced(&[
            ("Bold", (true, false, false)),
            ("text", (true, false, false)),
        ]);
        assert_eq!(render_spans(&line), "**Bold** **text**");
    }

    #[test]
    fn italic_run_uses_single_asterisk() {
        let line = words_spaced(&[("Note", (false, true, false))]);
        assert_eq!(render_spans(&line), "*Note*");
    }

    #[test]
    fn underline_run_uses_html_u() {
        let line = words_spaced(&[("Link", (false, false, true))]);
        assert_eq!(render_spans(&line), "<u>Link</u>");
    }

    #[test]
    fn style_transitions_close_and_reopen() {
        let line = words_spaced(&[
            ("Bold", (true, false, false)),
            ("normal", (false, false, false)),
            ("Italic", (false, true, false)),
        ]);
        assert_eq!(render_spans(&line), "**Bold** normal *Italic*");
    }

    #[test]
    fn explicit_space_span_never_emphasized() {
        let line = vec![
            span("BoldTail", 100.0, (true, false, false)),
            span(" ", 160.0, (true, false, false)), // space glyph, style ignored
            span("After", 165.0, (false, false, false)),
        ];
        assert_eq!(render_spans(&line), "**BoldTail** After");
    }
}

#[cfg(test)]
mod structural_tests {
    use super::*;

    const BODY: f64 = 10.0;

    fn word(text: &str, x: f64, size: f64, bold: bool) -> Span {
        Span {
            text: text.to_string(),
            x,
            y: 700.0,
            size,
            advance: text.len() as f64 * size * 0.6,
            is_bold: bold,
            is_italic: false,
            is_underline: false,
            is_vertical: false,
        }
    }

    fn one_span_line(text: &str, size: f64, bold: bool) -> Vec<Span> {
        vec![word(text, 100.0, size, bold)]
    }

    // -- detect_heading_level ------------------------------------------------

    #[test]
    fn h1_for_a_line_at_1_6x_body() {
        let line = one_span_line("Chapter One", BODY * 1.6, false);
        assert_eq!(detect_heading_level(&line, BODY), Some(1));
    }

    #[test]
    fn h1_for_a_bold_line_at_1_4x_body() {
        let line = one_span_line("Chapter One", BODY * 1.4, true);
        assert_eq!(detect_heading_level(&line, BODY), Some(1));
    }

    #[test]
    fn h2_for_a_line_at_1_3x_body() {
        let line = one_span_line("Section 1.1", BODY * 1.3, false);
        assert_eq!(detect_heading_level(&line, BODY), Some(2));
    }

    #[test]
    fn h3_for_a_bold_line_at_1_15x_body() {
        let line = one_span_line("Subsection", BODY * 1.15, true);
        assert_eq!(detect_heading_level(&line, BODY), Some(3));
    }

    #[test]
    fn non_bold_line_at_1_15x_body_is_not_a_heading() {
        // H3 requires bold; size alone at this ratio is not enough.
        let line = one_span_line("Subsection", BODY * 1.15, false);
        assert_eq!(detect_heading_level(&line, BODY), None);
    }

    #[test]
    fn body_sized_bold_text_is_not_a_heading() {
        let line = one_span_line("Just a bold word", BODY, true);
        assert_eq!(detect_heading_level(&line, BODY), None);
    }

    #[test]
    fn trailing_period_on_non_alphabetic_content_is_not_a_heading() {
        // A stray section/page-number fragment (no letters at all) ending in a
        // full stop — e.g. a ToC dot-leader remnant — must not read as a title
        // just because it happens to be large.
        let line = one_span_line("1.2.3.", BODY * 1.6, true);
        assert_eq!(detect_heading_level(&line, BODY), None);
    }

    #[test]
    fn three_or_more_sentences_is_not_a_heading() {
        // Two internal ". " separators (three sentences) reads as a dense
        // prose line rather than a title, regardless of size/boldness.
        let line = one_span_line("One. Two. Three.", BODY * 1.6, true);
        assert_eq!(detect_heading_level(&line, BODY), None);
    }

    #[test]
    fn a_single_sentence_can_still_be_a_heading_when_large_enough() {
        // Only an all-numeric/symbol trailing period, or 3+ sentences, is
        // excluded — an ordinary sentence-like heading ending in a period
        // (e.g. "Chapter 1.") is not penalized just for having a full stop.
        let line = one_span_line("This is a complete sentence.", BODY * 1.6, true);
        assert_eq!(detect_heading_level(&line, BODY), Some(1));
    }

    #[test]
    fn empty_line_is_not_a_heading() {
        let line = one_span_line("   ", BODY * 1.6, true);
        assert_eq!(detect_heading_level(&line, BODY), None);
    }

    // -- detect_list_marker ---------------------------------------------------

    fn bulleted_line(marker: &str) -> Vec<Span> {
        vec![
            word(marker, 100.0, BODY, false),
            word(" ", 100.0 + marker.len() as f64 * 6.0, BODY, false),
            word("Item text", 120.0, BODY, false),
        ]
    }

    #[test]
    fn dash_bullet_is_detected_unordered() {
        let line = bulleted_line("-");
        let (ordered, skip) = detect_list_marker(&line).expect("must detect bullet");
        assert!(!ordered);
        assert_eq!(skip, 2, "marker span + trailing space span skipped");
    }

    #[test]
    fn bullet_glyph_variants_are_detected() {
        for marker in ["•", "●", "◦", "▪", "+", "*"] {
            let line = bulleted_line(marker);
            assert!(
                detect_list_marker(&line).is_some(),
                "expected {marker:?} to be recognized as a bullet"
            );
        }
    }

    #[test]
    fn ordered_dot_marker_is_detected() {
        let line = bulleted_line("1.");
        let (ordered, _) = detect_list_marker(&line).expect("must detect ordered marker");
        assert!(ordered);
    }

    #[test]
    fn ordered_paren_marker_is_detected() {
        let line = bulleted_line("2)");
        let (ordered, _) = detect_list_marker(&line).expect("must detect ordered marker");
        assert!(ordered);
    }

    #[test]
    fn checkbox_marker_is_unordered_and_keeps_text() {
        let line = vec![
            word("[ ]", 100.0, BODY, false),
            word(" ", 118.0, BODY, false),
            word("Task", 124.0, BODY, false),
        ];
        let (ordered, skip) = detect_list_marker(&line).expect("must detect checkbox");
        assert!(!ordered);
        assert_eq!(skip, 0, "checkbox itself stays in the rendered text");
    }

    #[test]
    fn plain_prose_line_has_no_list_marker() {
        let line = one_span_line("This is a normal paragraph.", BODY, false);
        assert!(detect_list_marker(&line).is_none());
    }

    #[test]
    fn lone_marker_with_no_item_text_is_not_a_list_item() {
        let line = vec![word("-", 100.0, BODY, false)];
        assert!(detect_list_marker(&line).is_none());
    }

    // -- classify_line / ListRunState -----------------------------------------

    #[test]
    fn classify_line_promotes_heading_and_ends_list_run() {
        let mut state = ListRunState::default();
        // Start a list run first.
        let item = bulleted_line("-");
        let (role, _) = classify_line(&item, BODY, &mut state);
        assert!(matches!(role, LineRole::List { .. }));

        // A heading line must end the run rather than being treated as list depth.
        let heading = one_span_line("A Real Heading", BODY * 1.6, false);
        let (role, _) = classify_line(&heading, BODY, &mut state);
        assert!(matches!(role, LineRole::Heading(1)));

        // The next list item starts a *fresh* run (ordinal resets to 1), proving
        // the heading actually cleared state rather than just being skipped.
        let item2 = bulleted_line("1.");
        let (role, _) = classify_line(&item2, BODY, &mut state);
        match role {
            LineRole::List { ordinal, .. } => assert_eq!(ordinal, 1),
            other => panic!("expected a fresh list run, got {other:?}"),
        }
    }

    #[test]
    fn consecutive_ordered_items_number_sequentially() {
        let mut state = ListRunState::default();
        let mut ordinals = Vec::new();
        for _ in 0..3 {
            let item = bulleted_line("1.");
            let (role, _) = classify_line(&item, BODY, &mut state);
            if let LineRole::List { ordinal, .. } = role {
                ordinals.push(ordinal);
            }
        }
        assert_eq!(ordinals, vec![1, 2, 3]);
    }

    #[test]
    fn deeper_indent_increases_depth() {
        let mut state = ListRunState::default();
        let shallow = vec![
            word("-", 100.0, BODY, false),
            word(" ", 106.0, BODY, false),
            word("Top level", 112.0, BODY, false),
        ];
        let (role, _) = classify_line(&shallow, BODY, &mut state);
        let shallow_depth = match role {
            LineRole::List { depth, .. } => depth,
            _ => panic!("expected a list item"),
        };

        let nested = vec![
            word("-", 100.0 + 3.0 * BODY, BODY, false), // indented ~2 depth-units right
            word(" ", 106.0 + 3.0 * BODY, BODY, false),
            word("Nested", 112.0 + 3.0 * BODY, BODY, false),
        ];
        let (role, _) = classify_line(&nested, BODY, &mut state);
        let nested_depth = match role {
            LineRole::List { depth, .. } => depth,
            _ => panic!("expected a list item"),
        };

        assert_eq!(shallow_depth, 0);
        assert!(nested_depth > shallow_depth, "indented item must report a deeper depth");
    }

    // -- format_structured_line / strip_outer_emphasis -------------------------

    #[test]
    fn heading_strips_redundant_outer_bold() {
        let out = format_structured_line(&LineRole::Heading(2), "**Section Title**");
        assert_eq!(out, "## Section Title");
    }

    #[test]
    fn heading_with_no_emphasis_is_unchanged() {
        let out = format_structured_line(&LineRole::Heading(1), "Plain Title");
        assert_eq!(out, "# Plain Title");
    }

    #[test]
    fn unordered_list_item_gets_dash_prefix() {
        let out = format_structured_line(
            &LineRole::List { depth: 0, ordered: false, ordinal: 1 },
            "Item text",
        );
        assert_eq!(out, "- Item text");
    }

    #[test]
    fn ordered_list_item_gets_numbered_prefix() {
        let out = format_structured_line(
            &LineRole::List { depth: 0, ordered: true, ordinal: 3 },
            "Third item",
        );
        assert_eq!(out, "3. Third item");
    }

    #[test]
    fn nested_list_item_is_indented() {
        let out = format_structured_line(
            &LineRole::List { depth: 2, ordered: false, ordinal: 1 },
            "Deep item",
        );
        assert_eq!(out, "    - Deep item");
    }

    #[test]
    fn body_role_passes_text_through_unchanged() {
        let out = format_structured_line(&LineRole::Body, "Just a paragraph.");
        assert_eq!(out, "Just a paragraph.");
    }

    // -- end-to-end: render_cluster now emits structural Markdown --------------

    #[test]
    fn render_cluster_emits_heading_and_list_markdown() {
        let lines = vec![
            one_span_line("Document Title", 20.0, false), // body ~10 -> 2.0x -> H1
            one_span_line("First paragraph of body text.", 10.0, false),
            bulleted_line("-"),
            bulleted_line("-"),
        ];
        let md = render_cluster(&lines);
        assert!(md.contains("# Document Title"), "got:\n{md}");
        assert!(md.contains("- Item text"), "got:\n{md}");
        assert!(
            !md.contains("**Document Title**"),
            "heading text must not be redundantly bold-wrapped, got:\n{md}"
        );
    }
}

#[cfg(test)]
mod paragraph_merge_tests {
    use super::*;

    // Font size 10, single-spaced leading (~12pt pitch): y1-y0 = 10.0 so the
    // block's own "font size" for pitch math is 10.0.
    fn body_block(x0: f64, y0: f64, text: &str) -> DocBlock {
        DocBlock {
            page: 0,
            kind: "body".to_string(),
            x0,
            y0,
            x1: x0 + text.len() as f64 * 5.0,
            y1: y0 + 10.0,
            text: text.to_string(),
            is_bold: false,
            is_italic: false,
            is_underline: false,
        }
    }

    fn kind_block(kind: &str, x0: f64, y0: f64, text: &str) -> DocBlock {
        let mut b = body_block(x0, y0, text);
        b.kind = kind.to_string();
        b
    }

    #[test]
    fn two_aligned_normally_spaced_lines_merge_into_one_paragraph() {
        // Line 2's baseline is 12pt below line 1's (typical 1.2x leading for
        // a 10pt font) — gap = prev.y0(700) - b.y1(698) = 2, well within the
        // 1.8*10=18 pitch cap.
        let blocks = vec![
            body_block(100.0, 700.0, "First line of the paragraph"),
            body_block(100.0, 688.0, "second line continues it"),
        ];
        let merged = merge_paragraph_lines(blocks);
        assert_eq!(merged.len(), 1, "two aligned, normally-spaced lines must merge");
        assert_eq!(merged[0].text, "First line of the paragraph second line continues it");
    }

    #[test]
    fn three_line_paragraph_merges_fully() {
        let blocks = vec![
            body_block(100.0, 700.0, "Line one"),
            body_block(100.0, 688.0, "line two"),
            body_block(100.0, 676.0, "line three"),
        ];
        let merged = merge_paragraph_lines(blocks);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].text, "Line one line two line three");
        // Bounding box must union across all 3 merged lines.
        assert_eq!(merged[0].y1, 710.0, "y1 from the topmost line");
        assert_eq!(merged[0].y0, 676.0, "y0 from the bottommost line");
    }

    #[test]
    fn heading_between_two_body_lines_prevents_merge_across_it() {
        let blocks = vec![
            body_block(100.0, 700.0, "Paragraph before the heading"),
            kind_block("heading", 100.0, 688.0, "A Heading"),
            body_block(100.0, 676.0, "Paragraph after the heading"),
        ];
        let merged = merge_paragraph_lines(blocks);
        assert_eq!(merged.len(), 3, "a heading must never merge, and must not bridge two body blocks");
        assert_eq!(merged[0].kind, "body");
        assert_eq!(merged[1].kind, "heading");
        assert_eq!(merged[2].kind, "body");
    }

    #[test]
    fn list_item_never_merges_with_surrounding_body_text() {
        let blocks = vec![
            body_block(100.0, 700.0, "Some intro text"),
            kind_block("list", 100.0, 688.0, "A list item"),
            body_block(100.0, 676.0, "Trailing text"),
        ];
        let merged = merge_paragraph_lines(blocks);
        assert_eq!(merged.len(), 3);
        assert_eq!(merged[1].kind, "list");
        assert_eq!(merged[1].text, "A list item");
    }

    #[test]
    fn a_wide_vertical_gap_is_a_paragraph_break_not_a_merge() {
        // Gap = prev.y0(700) - b.y1(b.y0+10). For a break we need
        // gap > 1.8*10=18, so put the next line's y0 well below that.
        let blocks = vec![
            body_block(100.0, 700.0, "End of one paragraph."),
            body_block(100.0, 660.0, "Start of an unrelated paragraph."),
        ];
        let merged = merge_paragraph_lines(blocks);
        assert_eq!(merged.len(), 2, "a large vertical gap must read as a paragraph break");
    }

    #[test]
    fn misaligned_left_edges_do_not_merge() {
        // Line 2 establishes body_x0=100; line 3 is indented far enough right
        // (a new nested/quoted block, not a paragraph continuation) to miss
        // the ~3pt tolerance.
        let blocks = vec![
            body_block(100.0, 700.0, "Paragraph line one"),
            body_block(100.0, 688.0, "paragraph line two"),
            body_block(140.0, 676.0, "a differently indented block"),
        ];
        let merged = merge_paragraph_lines(blocks);
        assert_eq!(merged.len(), 2, "a line whose left edge doesn't match the body indent must not merge");
        assert_eq!(merged[0].text, "Paragraph line one paragraph line two");
    }

    #[test]
    fn first_line_indent_is_exempt_from_left_edge_check() {
        // Line 1 (the paragraph's first line) is indented +15pt from line 2 —
        // a classic first-line indent — and must not block the merge; line 2
        // then sets the real body_x0 that line 3 is checked against.
        let blocks = vec![
            body_block(115.0, 700.0, "Indented first line of paragraph"),
            body_block(100.0, 688.0, "flush second line"),
            body_block(100.0, 676.0, "flush third line"),
        ];
        let merged = merge_paragraph_lines(blocks);
        assert_eq!(merged.len(), 1, "a first-line indent must not prevent merging with the rest of the paragraph");
        assert_eq!(
            merged[0].text,
            "Indented first line of paragraph flush second line flush third line"
        );
    }

    #[test]
    fn table_zone_and_figure_never_merge_with_body_text() {
        for kind in ["table", "figure", "caption", "header", "footer", "title"] {
            let blocks = vec![
                body_block(100.0, 700.0, "Text before"),
                kind_block(kind, 100.0, 688.0, "Zone content"),
                body_block(100.0, 676.0, "Text after"),
            ];
            let merged = merge_paragraph_lines(blocks);
            assert_eq!(merged.len(), 3, "kind {kind:?} must never merge with body text");
        }
    }

    // -- join_paragraph_text / de-hyphenation -----------------------------

    #[test]
    fn hyphenated_line_wrap_joins_without_space_or_hyphen() {
        let mut text = "This is infor-".to_string();
        join_paragraph_text(&mut text, "mation you need.");
        assert_eq!(text, "This is information you need.");
    }

    #[test]
    fn trailing_dash_preceded_by_space_is_not_treated_as_hyphenation() {
        // A dash used as punctuation (range, aside) — the character right
        // before it is a space, not a letter — must join with a space and
        // keep the dash.
        let mut text = "A notable fact -".to_string();
        join_paragraph_text(&mut text, "worth remembering.");
        assert_eq!(text, "A notable fact - worth remembering.");
    }

    #[test]
    fn ordinary_lines_join_with_a_single_space() {
        let mut text = "First part".to_string();
        join_paragraph_text(&mut text, "second part.");
        assert_eq!(text, "First part second part.");
    }

    #[test]
    fn trailing_whitespace_before_hyphen_is_ignored() {
        let mut text = "infor-  ".to_string(); // trailing spaces after the hyphen
        join_paragraph_text(&mut text, "mation");
        assert_eq!(text, "information");
    }
}
