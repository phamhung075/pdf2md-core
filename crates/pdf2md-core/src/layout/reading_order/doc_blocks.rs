// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Reading order recovery, multi-column stream separation, and structured DocBlock generation.

use super::*;

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
                    // A leading `-` is a bullet only when it is followed by
                    // whitespace (`- item`). `-20,48 €` is a negative amount and
                    // `-field` is a hyphenated token; classifying them as "list"
                    // corrupts the structured `blocks` channel (the markdown
                    // channel goes through `detect_list_marker`, which already
                    // requires a real gap, so the two disagreed). Mirrors
                    // `is_star_bullet` above.
                    let is_dash_bullet =
                        t.starts_with('-') && t[1..].chars().next().map_or(false, |c| c.is_whitespace());
                    let is_bullet = is_dash_bullet
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
pub(super) fn merge_paragraph_lines(blocks: Vec<DocBlock>) -> Vec<DocBlock> {
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
            // Two visual lines can only belong to one paragraph when they
            // actually share horizontal extent. Without this, the first-merge
            // exemption below fused lines from *different columns* into one
            // block whenever they happened to be vertically adjacent (e.g. a
            // `DEVISE :` cell in the right column swallowing the
            // `NOM & DESIGNATION` header starting >100pt to the left); the
            // markdown channel already keeps them apart. A genuine first-line
            // indent still overlaps its continuation, so the exemption below
            // keeps working.
            let overlap_ok =
                b.x0 <= p.block.x1 + LEFT_EDGE_TOL && p.block.x0 <= b.x1 + LEFT_EDGE_TOL;
            // The paragraph's first merge (bringing in its 2nd line) is
            // exempt from the left-edge check — the first line may carry a
            // first-line indent that legitimately differs from the body's
            // real left edge, which the 2nd line then establishes for every
            // merge after this one (see the `line_count == 1` branch below).
            let edge_ok =
                overlap_ok && (p.line_count == 1 || (b.x0 - p.body_x0).abs() <= LEFT_EDGE_TOL);
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
/// genuine line-wrap break (a hyphen preceded by a letter, followed by a
/// lowercase *fragment* — not a French clitic / compound tail, e.g.
/// "infor-" + "mation" -> "information"), otherwise joins with a plain space.
pub(super) fn join_paragraph_text(text: &mut String, next: &str) {
    let trimmed_len = text.trim_end().len();
    text.truncate(trimmed_len);
    match classify_hyphen_join(text, next) {
        HyphenJoin::Dehyphenate => {
            text.pop(); // drop the trailing '-'
            text.push_str(next.trim_start());
        }
        HyphenJoin::KeepHyphen => {
            text.push_str(next.trim_start());
        }
        HyphenJoin::None => {
            text.push(' ');
            text.push_str(next);
        }
    }
}
