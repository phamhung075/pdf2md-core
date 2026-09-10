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

        let line_text = render_spans(line);

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
    let mut out = String::new();
    for (ci, stream) in streams.iter().enumerate() {
        if ci > 0 && !out.is_empty() {
            out.push('\n');
        }
        let mut prev_y: Option<f64> = None;
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
            out.push_str(&render_line_text(line));
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
    blocks
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
