// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Reading order recovery, multi-column stream separation, and structured DocBlock generation.

use serde::{Deserialize, Serialize};
use crate::layout::glyph_stream::Span;
use crate::reflow::{classify_hyphen_join, HyphenJoin};

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

/// Split one visual row at an *already-established* column gutter `gx`, even
/// when the local gap is narrower than `split_row_columns`'s standalone
/// `1.2em` threshold.
///
/// A justified two-column layout can leave barely ~1em of whitespace between
/// its columns — below the threshold that distinguishes a genuine gutter from
/// ordinary (possibly stretched) word spacing on a single row, so
/// `split_row_columns` alone misses those rows. Once `detect_column_bands` has
/// confirmed a consistent gutter across several rows, the remaining rows of the
/// same block can be split reliably against it: the gap is taken only when its
/// midpoint sits at the known gutter, it is at least `0.6em`, and it is wider
/// than every other inter-word gap in the row, so ordinary prose never splits.
fn split_row_at_gutter(spans: &[Span], gx: f64) -> Option<(Vec<Span>, Vec<Span>)> {
    if spans.len() < 5 {
        return None;
    }
    let size = spans.iter().map(|s| s.size).fold(0.0f64, f64::max).max(0.1);
    let mut at_gutter: Option<(f64, usize)> = None; // (gap, split index)
    let mut max_other_gap = 0.0f64;
    for i in 0..spans.len() - 1 {
        let a_end = spans[i].x + spans[i].advance;
        let b_start = spans[i + 1].x;
        let gap = (b_start - a_end).max(0.0);
        let mid = (a_end + b_start) / 2.0;
        if (mid - gx).abs() <= 0.5 * size + 6.0 {
            if at_gutter.map_or(true, |(g, _)| gap > g) {
                at_gutter = Some((gap, i));
            }
        } else if gap > max_other_gap {
            max_other_gap = gap;
        }
    }
    let (gap, at) = at_gutter?;
    if at < 2 || at >= spans.len() - 2 {
        return None;
    }
    if gap < 0.6 * size || gap <= max_other_gap {
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
    let col_bottom = keep.iter().map(|s| s.y).fold(f64::INFINITY, f64::min);
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
        // A line that never paired with a facing line, yet sits *inside* the
        // column block's vertical span and lies entirely on one side of the
        // common gutter, still belongs to that column — e.g. the short final
        // line of a taller left column ("… pour les vols" / "intercontinentaux.")
        // whose baseline coincides with no right-column line. Testing only the
        // y extent against `col_top` sent every such line to `bottom_full`, so
        // it was rendered *after* the whole right column, jumping to the end of
        // the page instead of staying inside its own paragraph.
        if y <= col_top + y_tol && y >= col_bottom - y_tol {
            let x0 = l.iter().map(|s| s.x).fold(f64::INFINITY, f64::min);
            let x1 = l.iter().map(|s| s.x + s.advance).fold(f64::NEG_INFINITY, f64::max);
            if x1 <= med {
                left.push((y, l.clone()));
                continue;
            }
            if x0 >= med {
                right.push((y, l.clone()));
                continue;
            }
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
                    // A non-whitespace span that *straddles* the candidate
                    // gutter (`s.x < gx < s.x + advance`) satisfies neither the
                    // left (`end <= gx`) nor the right (`x >= gx`) filter, so it
                    // is absent from both and `r_start - l_end` reports a wide
                    // apparent white corridor that the straddling run actually
                    // fills. Producers routinely split one word across several
                    // `Tj` runs (`L|e statut et le r|égim|e`, `c|our|s`), so on a
                    // centered single-column line this manufactures a phantom
                    // page gutter: `detect_projection_two_columns` then reads
                    // the page as two columns and emits each line's tail after
                    // every line's head. Subtract the straddlers' ink and accept
                    // the gutter only when real whitespace of gutter width is
                    // left over. A short straddler genuinely sitting between two
                    // columns (the invoice `Z` regression) leaves enough gap to
                    // still count as a split.
                    let covered: f64 = b
                        .line
                        .iter()
                        .filter(|s| {
                            !s.text.trim().is_empty() && s.x < gx && s.x + s.advance > gx
                        })
                        .map(|s| {
                            let lo = s.x.max(l_end);
                            let hi = (s.x + s.advance).min(r_start);
                            (hi - lo).max(0.0)
                        })
                        .sum();
                    if r_start - l_end - covered >= 1.2 * b.size {
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
            // Partition the row's spans completely: `left` takes everything
            // that ends at or before the gutter and `right` takes the exact
            // complement. Two independent half-open filters (`x + advance <=
            // gx` and `x >= gx`) leave any span that *straddles* the gutter —
            // e.g. a glyph whose advance crosses it — in neither half, so that
            // span's text is silently deleted from BOTH columns (the row is
            // still pushed because each half is non-empty). On the FNFE
            // MINIMUM invoice this dropped the middle `0` of `120 000,00 €`
            // from the `blocks` channel while the Markdown, rendered through
            // the band path, kept it. The complement filter is total: every
            // span lands in exactly one side, and a straddler follows the side
            // its box leans to (it is kept whole instead of vanishing).
            let left_spans: Vec<Span> = b.line.iter().filter(|s| s.x + s.advance <= gx).cloned().collect();
            let right_spans: Vec<Span> = b.line.iter().filter(|s| s.x + s.advance > gx).cloned().collect();
            if !left_spans.is_empty() && !right_spans.is_empty() {
                left.push((b.y, left_spans));
                right.push((b.y, right_spans));
                continue;
            }
            if b.y >= col_top - 5.0 {
                top_full.push(b.line);
            } else if b.y <= col_bottom + 5.0 {
                bottom_full.push(b.line);
            } else {
                // A mid-block row that cannot be split into two non-empty
                // halves — e.g. the whole line is a *single* span (PDF
                // producers often draw `TVA Intracommunautaire : FR…` as one
                // TJ run) whose advance crosses the gutter, so the
                // complement partition puts it entirely on one side and the
                // other half comes back empty. The top/bottom fallback above
                // only accepts rows at the block's edge, so without this a
                // mid-block row vanished from BOTH reading-order streams (and
                // therefore from the `blocks` channel) even though the
                // Markdown, rendered through the band path, kept it. Keep it
                // whole on the side its box centre leans to.
                let cx = 0.5 * (b.x0 + b.x1);
                if cx <= gx {
                    left.push((b.y, b.line.clone()));
                } else {
                    right.push((b.y, b.line.clone()));
                }
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

/// Whether `pc` is safe to read as two independent prose columns.
///
/// `page_two_columns_rows` already requires both halves to be a single clean
/// column; the vertical-projection fallback has no such gate, because it must
/// also serve pages whose whole line is a single `Tj` run (one span per visual
/// line, so an average-word gate would reject every genuine column). That
/// leaves it free to mistake the gap in front of an invoice's right-aligned
/// value column for the page gutter: the "left half" of its split then still
/// holds several table cells (label, description, quantity...) separated by
/// column-wide internal gutters, and the "right half" holds the amounts.
/// Reading the page in two streams emits every amount after the entire left
/// stream, so the `blocks` channel jumps a row's total to the end of the page
/// instead of keeping it beside its label (the target fixture's `275,00` and
/// `Teilzahlung`). A half that is itself a grid of cells is not a prose column;
/// apply the very rule `page_two_columns_rows` uses.
///
/// This decision is deliberately kept out of `page_two_columns` itself: the
/// glyph-stream table filter uses a detected two-column *prose block* to drop
/// 2-column table hits that are really prose noise, and must keep doing so even
/// when the same page is read linearly here.
fn columns_are_viable_prose(pc: &PageColumns) -> bool {
    rows_are_clean(&pc.left) && rows_are_clean(&pc.right)
}

/// Human reading order for the page as streams of visual lines: single-column
/// pages produce one stream (top-down); two-column pages produce full-width
/// header rows, left column, right column, footer rows.
pub fn page_read_order(lines: &[Vec<Span>]) -> Vec<Vec<Vec<Span>>> {
    if let Some(pc) = page_two_columns(lines).filter(columns_are_viable_prose) {
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
    // A 3+-column page whose narrow (~0.7em) gutters no single-gutter detector
    // can seed: `page_two_columns` needs one gutter across the whole page and
    // `detect_column_bands` splits a run at one gutter, leaving the remaining
    // columns of the split half welded. Recover every column at once by
    // vertical projection before the band pass.
    if let Some(region) = multi_column_projection(lines) {
        let mut streams = Vec::new();
        if region.start > 0 {
            streams.push(lines[..region.start].to_vec());
        }
        streams.extend(region.columns);
        if region.end + 1 < lines.len() {
            streams.push(lines[region.end + 1..].to_vec());
        }
        return streams;
    }
    // `page_two_columns` only recognizes a page that is two-column under ONE
    // consistent gutter for its entire height. Real business documents often
    // change layout partway down (e.g. a seller/buyer two-column header block,
    // a full-width item table, then a differently-positioned two-column
    // footer block with bank details) — no single global gutter fits all of
    // that, so the whole-page detector bails out and the page fell back to
    // one linear top-to-bottom stream, weaving unrelated columns together
    // line-by-line. `detect_column_bands` recovers each such block
    // independently instead of requiring one page-wide layout.
    let bands = detect_column_bands(lines);
    if bands.len() > 1 || matches!(bands.first(), Some(ColumnBand::Columns { .. })) {
        let mut streams = Vec::new();
        for band in bands {
            match band {
                ColumnBand::Full(rows) => streams.push(rows),
                ColumnBand::Columns { left, right } => {
                    streams.push(left);
                    streams.push(right);
                }
            }
        }
        return streams;
    }
    vec![lines.to_vec()]
}

/// One contiguous block of a page's reading order, as recovered by
/// `detect_column_bands`.
pub enum ColumnBand {
    /// Full-width lines in their natural top-to-bottom order.
    Full(Vec<Vec<Span>>),
    /// A genuine two-column block, already resolved into independent
    /// top-to-bottom streams for the left and right sides.
    Columns {
        left: Vec<Vec<Span>>,
        right: Vec<Vec<Span>>,
    },
}

/// Median gutter x (midpoint between left/right content) of the `Split`
/// rows accumulated so far in an in-progress column run, or `None` before
/// the run has any confirmed split.
fn run_median_gutter(gutters: &[f64]) -> Option<f64> {
    if gutters.is_empty() {
        return None;
    }
    let mut gs = gutters.to_vec();
    gs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = gs.len();
    // True median: with an even count the two central gutters are averaged.
    // Picking the upper one skews the estimate high on the first two rows,
    // which then mis-classifies a short facing line that starts just left of
    // the inflated midpoint as neither side and breaks the column run.
    Some(if n % 2 == 1 {
        gs[n / 2]
    } else {
        0.5 * (gs[n / 2 - 1] + gs[n / 2])
    })
}

/// Average number of non-empty spans per row (0 for an empty slice). A cheap
/// "is this a text column rather than a grid of short cells" signal.
fn avg_words_per_row(rows: &[Vec<Span>]) -> f64 {
    if rows.is_empty() {
        return 0.0;
    }
    rows.iter()
        .map(|v| v.iter().filter(|sp| !sp.text.trim().is_empty()).count() as f64)
        .sum::<f64>()
        / rows.len() as f64
}

/// Whether every row's internal span gaps stay below a column-gutter width —
/// i.e. the rows form a single column, not a multi-column table grid.
fn rows_are_clean(rows: &[Vec<Span>]) -> bool {
    rows.iter().all(|v| {
        let size = v.iter().map(|x| x.size).fold(0.0f64, f64::max).max(0.1);
        v.windows(2).all(|p| {
            let a_end = p[0].x + p[0].advance;
            let gap = (p[1].x - a_end).max(0.0);
            gap <= 1.2 * size
        })
    })
}

/// Whether `rows` is a *wrapped* text column: some consecutive pair where the
/// earlier row neither ends a sentence nor is a closed item and the later row
/// plainly continues it (starts lowercase), or the earlier row ends in a
/// line-break hyphen. A run of table cells never continues one row into the
/// next, so this is what separates real body text sitting beside a grid from an
/// ordinary label/value or multi-column table.
fn wrapped_prose(rows: &[Vec<Span>]) -> bool {
    let plain: Vec<String> = rows
        .iter()
        .map(|r| r.iter().map(|s| s.text.as_str()).collect::<String>())
        .collect();
    plain.windows(2).any(|w| {
        let a = w[0].trim_end();
        let b = w[1].trim_start();
        let hyphen_break =
            a.ends_with('-') && a.chars().rev().nth(1).map_or(false, |c| c.is_alphabetic());
        let sentence_end = a.ends_with(['.', '!', '?', ':', ';', ')', ']', '€', '%']);
        let lower_next = b.chars().next().map_or(false, |c| c.is_lowercase());
        hyphen_break || (!sentence_end && lower_next)
    })
}

/// A flowing prose column facing a multi-column grid (or its mirror). The two
/// halves are independent regions even though one of them is not a single clean
/// column, so the column band must be kept rather than re-merged row by row —
/// which weaves the grid's cells into the prose and, when the grid uses smaller
/// type, renders them as fake LaTeX super/subscripts. Both halves unclean means
/// a single wide grid was cut in two, not a prose/table split.
fn prose_beside_grid(left: &[Vec<Span>], right: &[Vec<Span>]) -> bool {
    let (left_clean, right_clean) = (rows_are_clean(left), rows_are_clean(right));
    let (lw, rw) = (avg_words_per_row(left), avg_words_per_row(right));
    (left_clean && !right_clean && lw >= 4.0 && wrapped_prose(left) && lw > rw)
        || (right_clean && !left_clean && rw >= 4.0 && wrapped_prose(right) && rw > lw)
}

/// Whether some internal gutter recurs at the same x across at least two of
/// `rows` — i.e. the side is itself a table grid (one or more aligned columns)
/// rather than a single column that merely happens to contain one wide gap.
/// The projection fallback uses this to tell a genuine grid from an
/// accidental short alignment inside running prose.
fn has_aligned_internal_gutter(rows: &[Vec<Span>]) -> bool {
    let mut gutters: Vec<f64> = Vec::new();
    for v in rows {
        let size = v.iter().map(|s| s.size).fold(0.0f64, f64::max).max(0.1);
        for p in v.windows(2) {
            let gap = p[1].x - (p[0].x + p[0].advance);
            if gap >= 0.8 * size {
                gutters.push(0.5 * (p[0].x + p[0].advance + p[1].x));
            }
        }
    }
    gutters
        .iter()
        .any(|a| gutters.iter().filter(|b| (**b - *a).abs() <= 6.0).count() >= 2)
}

/// Same gate as [`prose_beside_grid`], but additionally requires the non-clean
/// side to be a *real* grid — an internal gutter aligned across at least two
/// rows. The running-gutter pass has the row-split evidence that the projection
/// fallback lacks, so without this a coincidental gap inside prose could be
/// mistaken for a column and split a sentence in half.
fn projection_prose_beside_grid(left: &[Vec<Span>], right: &[Vec<Span>]) -> bool {
    let (left_clean, right_clean) = (rows_are_clean(left), rows_are_clean(right));
    let (lw, rw) = (avg_words_per_row(left), avg_words_per_row(right));
    (left_clean
        && !right_clean
        && has_aligned_internal_gutter(right)
        && lw >= 4.0
        && wrapped_prose(left)
        && lw > rw)
        || (right_clean
            && !left_clean
            && has_aligned_internal_gutter(left)
            && rw >= 4.0
            && wrapped_prose(right)
            && rw > lw)
}

/// Recover a two-column block whose inter-column gutter is *narrower* than the
/// `1.2em` a standalone row split needs and which therefore never seeds the
/// running-gutter pass: a prose column facing a multi-column grid with only a
/// few points of white between them (common in tightly-typeset two-column
/// academic pages). Works from the vertical projection instead: find an x where
/// a contiguous run of rows has no span straddling it, then split the whole run
/// there. Returns `(start_index, end_index, left_rows, right_rows)`.
fn projection_columns_region(
    lines: &[Vec<Span>],
) -> Option<(usize, usize, Vec<Vec<Span>>, Vec<Vec<Span>>)> {
    if lines.len() < 6 {
        return None;
    }

    /// A span straddles `gx` when it covers that x position — content on both
    /// sides of `gx` with no gutter there.
    fn straddles(line: &[Span], gx: f64) -> bool {
        line.iter().any(|s| s.x + 0.5 < gx && s.x + s.advance - 0.5 > gx)
    }

    // Candidate gutters: the midpoint of every inter-span gap at least 0.5em
    // wide (white narrower than a word space cannot separate columns), plus
    // each line's own left and right content edge — a column whose rows never
    // merged across the gutter shows it only as those facing edges.
    let mut cands: Vec<f64> = Vec::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let size = line.iter().map(|s| s.size).fold(0.0f64, f64::max).max(0.1);
        for w in line.windows(2) {
            let a_end = w[0].x + w[0].advance;
            let gap = w[1].x - a_end;
            if gap >= 0.5 * size {
                cands.push((a_end + w[1].x) / 2.0);
            }
        }
        cands.push(line.iter().map(|s| s.x).fold(f64::INFINITY, f64::min));
        cands.push(line.iter().map(|s| s.x + s.advance).fold(f64::NEG_INFINITY, f64::max));
    }
    cands.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    cands.dedup_by(|a, b| (*a - *b).abs() < 3.0);

    let mut best: Option<(usize, usize, Vec<Vec<Span>>, Vec<Vec<Span>>)> = None;
    for &gx in &cands {
        let mut i = 0;
        while i < lines.len() {
            if lines[i].is_empty() || straddles(&lines[i], gx) {
                i += 1;
                continue;
            }
            let start = i;
            while i < lines.len() && !lines[i].is_empty() && !straddles(&lines[i], gx) {
                i += 1;
            }
            let end = i - 1;
            // A real block spans several rows; a shorter "run" is usually an
            // accidental short alignment inside running prose.
            if end - start + 1 < 6 {
                continue;
            }
            // Snap the gutter to the centre of the white actually available
            // across the run (the candidate came from a single row, and column
            // edges are ragged).
            let (mut left_end, mut right_start) = (f64::NEG_INFINITY, f64::INFINITY);
            for line in &lines[start..=end] {
                for s in line {
                    if s.x >= gx {
                        right_start = right_start.min(s.x);
                    } else {
                        left_end = left_end.max(s.x + s.advance);
                    }
                }
            }
            if !(left_end < right_start) {
                continue;
            }
            let snap = 0.5 * (left_end + right_start);
            if lines[start..=end].iter().any(|l| straddles(l, snap)) {
                continue;
            }
            let mut left: Vec<Vec<Span>> = Vec::new();
            let mut right: Vec<Vec<Span>> = Vec::new();
            for line in &lines[start..=end] {
                let (l, r): (Vec<Span>, Vec<Span>) = line
                    .iter()
                    .cloned()
                    .partition(|s| s.x + s.advance <= snap + 0.5);
                if !l.is_empty() {
                    left.push(l);
                }
                if !r.is_empty() {
                    right.push(r);
                }
            }
            if left.len() < 3 || right.len() < 3 {
                continue;
            }
            if avg_words_per_row(&left) < 2.5 || avg_words_per_row(&right) < 2.5 {
                continue;
            }
            if !projection_prose_beside_grid(&left, &right) {
                continue;
            }
            // Longest run wins; a tie keeps the earlier (leftmost) gutter, the
            // page gutter rather than an internal grid one.
            let score = end - start + 1;
            if best.as_ref().map_or(true, |b| score > b.1 - b.0 + 1) {
                best = Some((start, end, left, right));
            }
        }
    }
    best
}

/// A page region recovered as 3+ independent vertical columns by
/// [`multi_column_projection`].
struct MultiColumnRegion {
    /// First and last line index (into the page's `lines`) of the region.
    start: usize,
    end: usize,
    /// One line-stream per column, left to right.
    columns: Vec<Vec<Vec<Span>>>,
}

/// Recover a page region of 3+ narrow columns.
///
/// Every other reading-order detector models a *single* gutter
/// (`page_two_columns`, `detect_column_bands`) or a prose-vs-grid pair
/// (`projection_columns_region`). A magazine/newsletter page of three or more
/// prose columns separated by only ~0.7em of white defeats all of them: the
/// line builder fuses each row's columns into one visual line (word spaces are
/// ~0.25em, so a 0.7em gutter is far below any hard-break threshold) and the
/// whole page then weaves column-by-column row-by-row. This works from the
/// vertical projection instead: a gutter is an x covered by no span on any row
/// of the region, wide enough to be real white and narrow enough to sit between
/// columns. Rows above/below the region are left in place as full-width
/// streams, so a page header/footer keeps its order.
///
/// Deliberately conservative: it requires its own set of at least 2 gutters
/// (>=3 columns), each column to read as wrapped multi-word prose, and the
/// region to span several rows. A page it cannot read this way returns `None`
/// and falls back to the single linear stream exactly as before.
fn multi_column_projection(lines: &[Vec<Span>]) -> Option<MultiColumnRegion> {
    const MIN_REGION_ROWS: usize = 8;
    const MIN_GAP_EM: f64 = 0.4;
    const MIN_COL_WIDTH_EM: f64 = 3.0;
    if lines.len() < MIN_REGION_ROWS {
        return None;
    }

    // Candidate gutters: the midpoint of every inter-span gap at least
    // `MIN_GAP_EM` wide. A word space (~0.25em) never seeds one.
    let mut cands: Vec<f64> = Vec::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        for w in line.windows(2) {
            let a_end = w[0].x + w[0].advance;
            let gap = w[1].x - a_end;
            let size = w[1].size.max(w[0].size).max(0.1);
            if gap >= MIN_GAP_EM * size {
                cands.push(0.5 * (a_end + w[1].x));
            }
        }
    }
    cands.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    cands.dedup_by(|a, b| (*a - *b).abs() < 3.0);
    if cands.len() < 2 {
        return None;
    }

    let n = lines.len();
    let c = cands.len();
    // For each candidate: is no span on this row *covering* it (so the row is
    // compatible with a column split there), and does a real gap sit on it.
    let mut compat: Vec<Vec<bool>> = vec![vec![false; n]; c];
    let mut has_gap: Vec<Vec<bool>> = vec![vec![false; n]; c];
    for (g, &gx) in cands.iter().enumerate() {
        for (i, line) in lines.iter().enumerate() {
            if line.is_empty() {
                compat[g][i] = true;
                continue;
            }
            let mut covered = false;
            let mut gap_here = false;
            for (k, s) in line.iter().enumerate() {
                if s.x + 0.5 < gx && s.x + s.advance - 0.5 > gx {
                    covered = true;
                    break;
                }
                if let Some(t) = line.get(k + 1) {
                    let a_end = s.x + s.advance;
                    let gap = t.x - a_end;
                    let size = t.size.max(s.size).max(0.1);
                    if gap >= MIN_GAP_EM * size && a_end < gx && t.x > gx {
                        gap_here = true;
                    }
                }
            }
            compat[g][i] = !covered;
            has_gap[g][i] = gap_here;
        }
    }

    // Longest contiguous window on which at least two gutters are compatible
    // on every row and each has a real gap on most rows.
    let mut best: Option<(usize, usize, Vec<usize>)> = None;
    for s in 0..n {
        let mut all_ok = vec![true; c];
        let mut gaps = vec![0usize; c];
        let mut e = s;
        while e < n {
            for g in 0..c {
                if all_ok[g] && !compat[g][e] {
                    all_ok[g] = false;
                }
                if has_gap[g][e] {
                    gaps[g] += 1;
                }
            }
            let len = e - s + 1;
            let active: Vec<usize> = (0..c)
                .filter(|&g| all_ok[g] && (gaps[g] as f64) >= 0.6 * len as f64)
                .collect();
            if active.len() >= 2
                && best.as_ref().map_or(true, |b| len > b.1 - b.0 + 1)
            {
                best = Some((s, e, active));
            }
            if all_ok.iter().filter(|x| **x).count() < 2 {
                break;
            }
            e += 1;
        }
    }
    let (start, end, active) = best?;
    if end - start + 1 < MIN_REGION_ROWS {
        return None;
    }

    let body = body_size_for(lines);
    let min_col_w = MIN_COL_WIDTH_EM * body;
    let mut left_edge = f64::INFINITY;
    let mut right_edge = f64::NEG_INFINITY;
    for line in &lines[start..=end] {
        for sp in line {
            left_edge = left_edge.min(sp.x);
            right_edge = right_edge.max(sp.x + sp.advance);
        }
    }
    if !left_edge.is_finite() || right_edge - left_edge < 3.0 * min_col_w {
        return None;
    }

    // Keep the widest set of gutters that each leaves a real column on either
    // side and is separated from the next by at least one column width.
    let mut gutters: Vec<f64> = active.iter().map(|&g| cands[g]).collect();
    gutters.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    gutters.dedup_by(|a, b| (*a - *b).abs() < min_col_w);
    let mut selected: Vec<f64> = Vec::new();
    for gx in gutters {
        if gx - left_edge < min_col_w || right_edge - gx < min_col_w {
            continue;
        }
        if selected.last().map_or(true, |&last| gx - last >= min_col_w) {
            selected.push(gx);
        }
    }
    if selected.len() < 2 {
        return None;
    }

    let ncols = selected.len() + 1;
    let mut columns: Vec<Vec<Vec<Span>>> = vec![Vec::new(); ncols];
    for line in &lines[start..=end] {
        let mut buckets: Vec<Vec<Span>> = vec![Vec::new(); ncols];
        for sp in line {
            let center = sp.x + 0.5 * sp.advance;
            let mut col = 0usize;
            while col < selected.len() && center >= selected[col] {
                col += 1;
            }
            buckets[col].push(sp.clone());
        }
        for (ci, bucket) in buckets.into_iter().enumerate() {
            if !bucket.is_empty() {
                columns[ci].push(bucket);
            }
        }
    }
    for col in &columns {
        if col.len() < 3 || avg_words_per_row(col) < 2.5 {
            return None;
        }
    }
    if !columns.iter().any(|col| wrapped_prose(col)) {
        return None;
    }
    Some(MultiColumnRegion { start, end, columns })
}

/// Segment `lines` into a sequence of column bands. Unlike `page_two_columns`
/// (which needs ONE gutter consistent across the *entire* page), this walks
/// the page top-to-bottom and detects each contiguous two-column region on
/// its own: a run of rows sharing a consistent internal gutter (via
/// `split_row_columns`) — plus rows that plainly fall entirely to one side of
/// that gutter (a label with no counterpart on the facing side) — becomes a
/// `Columns` band once it has at least 3 confirmed splits and passes the same
/// prose/clean gates `page_two_columns_rows` uses (average >= 2.5 real words
/// per side, no internal wide gutter inside either half). Any row that
/// neither extends the current run nor falls unambiguously to one side ends
/// the run: a confirmed run is emitted as `Columns`, otherwise its rows are
/// restored to their original single-line form and folded back into the
/// surrounding `Full` block.
pub fn detect_column_bands(lines: &[Vec<Span>]) -> Vec<ColumnBand> {
    enum RunItem {
        Split {
            left: Vec<Span>,
            right: Vec<Span>,
            gutter: f64,
        },
        LeftOnly(Vec<Span>),
        RightOnly(Vec<Span>),
    }

    fn flush(run: &mut Vec<RunItem>, pending_full: &mut Vec<Vec<Span>>, bands: &mut Vec<ColumnBand>) {
        if run.is_empty() {
            return;
        }
        let gutters: Vec<f64> = run
            .iter()
            .filter_map(|it| match it {
                RunItem::Split { gutter, .. } => Some(*gutter),
                _ => None,
            })
            .collect();
        let left_splits: Vec<Vec<Span>> = run
            .iter()
            .filter_map(|it| match it {
                RunItem::Split { left, .. } => Some(left.clone()),
                _ => None,
            })
            .collect();
        let right_splits: Vec<Vec<Span>> = run
            .iter()
            .filter_map(|it| match it {
                RunItem::Split { right, .. } => Some(right.clone()),
                _ => None,
            })
            .collect();
        // A clean half is a single column; an unclean half contains its own
        // internal gutter — i.e. it is itself a multi-column table grid. When
        // one half is a flowing, multi-word prose column *and* the facing half
        // is such a grid, the established gutter still separates two
        // independent regions (body text beside a data table) and the band must
        // be kept. Merging those rows back instead weaves the table's cells
        // into the prose line-by-line, and the smaller table type then reads as
        // a LaTeX superscript of the prose (`model $properly^{Guardrails...}$`).
        // Both halves unclean means a single wide grid was cut in two, not a
        // prose/table split — leave those rows to the table detector.
        let left_clean = rows_are_clean(&left_splits);
        let right_clean = rows_are_clean(&right_splits);
        // A column block normally needs three crossing rows to confirm its
        // gutter. Two crossing rows can still describe a real corridor when
        // they are *consecutive* and each is a genuine *fusion* of two regions
        // that do not share a baseline:
        //
        //  * the target's numbered list has items 4 and 5 on the same visual
        //    line as the adjacent callout-box title ("Accès" / "bailleur") only
        //    because the line builder tolerates a half-em baseline difference
        //    (measured here: 2.28pt and 1.44pt), so emitting the list items
        //    first and the title after them is the true reading order;
        //  * a form label with its right-aligned value, or a table's
        //    model/score row, has both halves on *exactly* the same baseline
        //    (measured: 0.00pt) — it is one physical line whose row order must
        //    be preserved.
        //
        // Requiring a measured baseline gap on both crossing rows keeps the
        // latter row-wise, and requiring the crossings to be adjacent rejects a
        // single-column form whose wide gaps merely happen to align with a
        // one-sided row between them. Three crossings remain the default.
        let split_indices: Vec<usize> = run
            .iter()
            .enumerate()
            .filter(|(_, it)| matches!(it, RunItem::Split { .. }))
            .map(|(i, _)| i)
            .collect();
        let fused_corridor = split_indices.len() == 2
            && split_indices[1] == split_indices[0] + 1
            && split_indices.iter().all(|&i| match &run[i] {
                RunItem::Split { left, right, .. } => {
                    let ly = left.first().map(|s| s.y).unwrap_or(0.0);
                    let ry = right.first().map(|s| s.y).unwrap_or(0.0);
                    (ly - ry).abs() >= 1.0
                }
                _ => false,
            });
        let corridor_confirmed = gutters.len() >= 3 || fused_corridor;
        let ok = corridor_confirmed
            && avg_words_per_row(&left_splits) >= 2.5
            && avg_words_per_row(&right_splits) >= 2.5
            && ((left_clean && right_clean)
                || prose_beside_grid(&left_splits, &right_splits));

        if ok {
            if !pending_full.is_empty() {
                bands.push(ColumnBand::Full(std::mem::take(pending_full)));
            }
            let mut left = Vec::new();
            let mut right = Vec::new();
            for it in run.drain(..) {
                match it {
                    RunItem::Split { left: l, right: r, .. } => {
                        left.push(l);
                        right.push(r);
                    }
                    RunItem::LeftOnly(l) => left.push(l),
                    RunItem::RightOnly(r) => right.push(r),
                }
            }
            bands.push(ColumnBand::Columns { left, right });
        } else {
            for it in run.drain(..) {
                match it {
                    RunItem::Split { left: l, right: r, .. } => {
                        // Not a real column block after all: this was one
                        // physical row, so restore it whole rather than
                        // leaking the speculative split into the plain text.
                        let mut combined = l;
                        combined.extend(r);
                        combined.sort_by(|a, b| a.x.partial_cmp(&b.x).unwrap_or(std::cmp::Ordering::Equal));
                        pending_full.push(combined);
                    }
                    RunItem::LeftOnly(l) => pending_full.push(l),
                    RunItem::RightOnly(r) => pending_full.push(r),
                }
            }
        }
    }

    let mut bands: Vec<ColumnBand> = Vec::new();
    let mut pending_full: Vec<Vec<Span>> = Vec::new();
    let mut run: Vec<RunItem> = Vec::new();

    for line in lines {
        if line.is_empty() {
            continue;
        }
        let cur_gutters: Vec<f64> = run
            .iter()
            .filter_map(|it| match it {
                RunItem::Split { gutter, .. } => Some(*gutter),
                _ => None,
            })
            .collect();
        let med = run_median_gutter(&cur_gutters);
        let mut absorbed = false;

        if let Some((left, right)) = split_row_columns(line) {
            let l_end = left.iter().map(|s| s.x + s.advance).fold(f64::NEG_INFINITY, f64::max);
            let r_start = right.iter().map(|s| s.x).fold(f64::INFINITY, f64::min);
            let gutter = (l_end + r_start) / 2.0;
            let consistent = med.map_or(true, |m| (gutter - m).abs() <= (0.15 * m.abs()).max(6.0));
            if consistent {
                run.push(RunItem::Split { left, right, gutter });
                absorbed = true;
            }
        }

        // A justified column block can have a gutter too narrow for
        // `split_row_columns`'s standalone threshold. Once the run has an
        // established median gutter, split any remaining crossing row against
        // it (see `split_row_at_gutter`).
        if !absorbed {
            if let Some(m) = med {
                if let Some((left, right)) = split_row_at_gutter(line, m) {
                    let l_end = left.iter().map(|s| s.x + s.advance).fold(f64::NEG_INFINITY, f64::max);
                    let r_start = right.iter().map(|s| s.x).fold(f64::INFINITY, f64::min);
                    let gutter = (l_end + r_start) / 2.0;
                    if (gutter - m).abs() <= (0.15 * m.abs()).max(6.0) {
                        run.push(RunItem::Split { left, right, gutter });
                        absorbed = true;
                    }
                }
            }
        }

        if !absorbed {
            if let Some(m) = med {
                // Only a row vertically adjacent to the run can extend it. A
                // later full-width heading (or any short line) that happens to
                // sit entirely on one side of the gutter begins a new block —
                // absorbing it would render it *before* the facing column.
                let contiguous = run.last().map_or(true, |it| {
                    let last_y = match it {
                        RunItem::Split { left, .. } => left[0].y,
                        RunItem::LeftOnly(l) | RunItem::RightOnly(l) => l[0].y,
                    };
                    let gap = last_y - line[0].y;
                    gap >= -1.0 && gap <= 2.5 * line[0].size.max(0.1)
                });
                if contiguous {
                    let x0 = line.iter().map(|s| s.x).fold(f64::INFINITY, f64::min);
                    let x1 = line.iter().map(|s| s.x + s.advance).fold(f64::NEG_INFINITY, f64::max);
                    if x1 <= m {
                        run.push(RunItem::LeftOnly(line.clone()));
                        absorbed = true;
                    } else if x0 >= m {
                        run.push(RunItem::RightOnly(line.clone()));
                        absorbed = true;
                    }
                }
            }
        }

        if !absorbed {
            flush(&mut run, &mut pending_full, &mut bands);
            pending_full.push(line.clone());
        }
    }
    flush(&mut run, &mut pending_full, &mut bands);
    if !pending_full.is_empty() {
        bands.push(ColumnBand::Full(pending_full));
    }

    // The running-gutter pass can only seed a column block from a row whose
    // inter-column gap clears the standalone `1.2em` threshold. When no row on
    // the page does — a narrow page gutter beside a wider internal table gutter
    // — no band is found and the two regions fall back to one interleaved
    // stream. Recover such a block from the vertical projection instead.
    if !bands.iter().any(|b| matches!(b, ColumnBand::Columns { .. })) {
        if let Some((start, end, left, right)) = projection_columns_region(lines) {
            let mut rebuilt: Vec<ColumnBand> = Vec::new();
            if start > 0 {
                rebuilt.push(ColumnBand::Full(lines[..start].to_vec()));
            }
            rebuilt.push(ColumnBand::Columns { left, right });
            if end + 1 < lines.len() {
                rebuilt.push(ColumnBand::Full(lines[end + 1..].to_vec()));
            }
            return rebuilt;
        }
    }

    bands
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
/// stream fully, then its right stream fully, each starting its own
/// paragraph/list context (mirroring `render_human_order`'s stream loop) so
/// column content recovered mid-page doesn't inherit spacing or list state
/// from the unrelated column next to it.
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
///
/// A span's own text can embed a trailing space (a producer drawing the bold
/// run `"2 "` as one glyph run, rather than emitting the space as its own
/// span). CommonMark rejects a closing delimiter that is preceded by
/// whitespace — `**2 **` is *not* bold — and the orphaned `**` then consumes
/// the next `**…**` run on the line. So any whitespace sitting at the end of
/// the open run is re-emitted *after* the closing delimiters instead.
fn close_style(out: &mut String, st: InlineStyle) {
    if st == InlineStyle::default() {
        return;
    }
    let kept = out.trim_end_matches(' ').len();
    let trailing_space = out[kept..].to_string();
    out.truncate(kept);
    if st.bold {
        out.push_str("**");
    }
    if st.italic {
        out.push('*');
    }
    if st.underline {
        out.push_str("</u>");
    }
    out.push_str(&trailing_space);
}

/// Style of the next span after `after` that carries visible text, skipping
/// empty and whitespace-only spans; `None` once the line ends. Used to keep one
/// emphasis run open across an ordinary inter-word space: `**Mistral 7B**` is
/// valid CommonMark, so the delimiters must not be closed and reopened at every
/// word.
fn next_visible_style(line: &[Span], after: usize) -> Option<InlineStyle> {
    line.get(after + 1..)?
        .iter()
        .find(|s| !s.text.is_empty() && !s.text.chars().all(|c| c == ' '))
        .map(InlineStyle::of)
}

/// Render one visual line's spans to text with inline `**bold**` / `*italic*` /
/// `<u>underline</u>` emphasis. The spatial spacing rules are identical to the
/// legacy plain-text renderer (`render_cluster`), so a line of unstyled spans
/// produces byte-identical output; only runs whose style is non-plain gain
/// emphasis delimiters.
pub(crate) fn render_spans(line: &[Span]) -> String {
    let mut out = String::new();
    let mut prev_x: Option<f64> = None;
    let mut prev_word_advance = 0.0f64;
    let mut cur = InlineStyle::default();

    for (i, span) in line.iter().enumerate() {
        if span.text.is_empty() {
            continue;
        }
        let size = span.size.max(0.1);
        let space_adv = 0.25 * size;
        let is_space = span.text.chars().all(|c| c == ' ');

        if let Some(px) = prev_x {
            let gap = span.x - px;
            // `gap` is start-to-start; the *whitespace* between the two spans is
            // what it is after the previous span's own advance. Comparing the
            // raw start-to-start distance against a size threshold (as the
            // hard-break branch used to) makes any word run wider than ~2.5 em
            // look like a new column and emits a spurious newline mid-sentence.
            let gap = gap - prev_word_advance;
            if is_space {
                // A whitespace-only run whose origin sits behind the previous
                // run's right edge adds no visible whitespace. Producers draw
                // such zero-advance "spacer" spaces (often with a huge negative
                // `TJ` kern) purely to position the next run, and the spacer
                // then overlaps the following word at the same x. Treating it
                // as a separator split `Env`+`elo` at the spacer's own x; skip
                // it so the next run measures its gap against the real word end.
                if gap < -0.05 * size {
                    continue;
                }
            }
            if !is_space {
                if gap > 2.5 * size {
                    // A distinct column / element on the same row: break the
                    // line AND close any open emphasis first.
                    close_style(&mut out, cur);
                    cur = InlineStyle::default();
                    if !out.is_empty() && !out.ends_with('\n') {
                        out.push('\n');
                    }
                } else if gap > 0.65 * space_adv {
                    // Ordinary inter-word space. A space *inside* emphasis is
                    // valid CommonMark (`**Mistral 7B**`), so if the styled run
                    // simply continues with the same non-plain style, keep `cur`
                    // open and emit the space inside the delimiters. Only close
                    // (and emit the space outside) when the run ends or changes.
                    if cur == InlineStyle::default() || InlineStyle::of(span) != cur {
                        close_style(&mut out, cur);
                        cur = InlineStyle::default();
                    }
                    if !out.is_empty() && !out.ends_with(' ') && !out.ends_with('\n') {
                        out.push(' ');
                    }
                }
            }
        }

        if is_space {
            // A space glyph is never emphasized on its own, but it may sit in
            // the *interior* of an emphasis run when the next visible span keeps
            // the same non-plain style (`**Mistral 7B**` is valid CommonMark).
            // Only close the style when the run ends or changes; otherwise keep
            // it open and emit the space inside the delimiters.
            if !out.is_empty() && !out.ends_with(' ') && !out.ends_with('\n') {
                let continues =
                    cur != InlineStyle::default() && next_visible_style(line, i) == Some(cur);
                if !continues {
                    close_style(&mut out, cur);
                    cur = InlineStyle::default();
                }
                out.push(' ');
            }
        } else {
            let st = InlineStyle::of(span);
            if st != cur {
                // Opening a new delimiter. A span's own text can also *begin*
                // with a space (a producer run like `" www.caf.fr"`); an
                // opening delimiter followed by whitespace (`** x**`) is not
                // emphasis either, so flush that leading space outside the
                // delimiter. `close_style` already moved any trailing space of
                // the previous run outside its closing delimiter.
                let lead = span.text.len() - span.text.trim_start_matches(' ').len();
                close_style(&mut out, cur);
                if lead > 0 && !out.is_empty() && !out.ends_with(' ') && !out.ends_with('\n') {
                    out.push(' ');
                }
                open_style(&mut out, st);
                cur = st;
                out.push_str(&span.text[lead..]);
            } else {
                // Same style stays open: the whole run is emphasized and any
                // embedded whitespace is interior, so it is kept verbatim.
                out.push_str(&span.text);
            }
        }

        prev_x = Some(span.x);
        prev_word_advance = span.word_advance;
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
        push_line(&mut out, line, &mut prev_line_y, &mut list_state, body_size);
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
            push_line(&mut out, line, &mut prev_y, &mut list_state, body_size);
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

/// Robustly estimate the body (regular prose) font size for a page.
///
/// The median is a poor anchor for heading detection: when a page carries many
/// mid-size headings (outlines, structured reports), the heading sizes pull the
/// median up to the heading size, so `size >= body * 1.25` no longer fires and
/// the headings hide themselves. Instead take the most frequent size class —
/// the mode — and, when several classes tie for frequency, the *smallest* of
/// them. Body text is the smallest regular size class and headings are larger,
/// so this keeps the heading threshold anchored on the body and never lets the
/// headings inflate it.
///
/// A nominal size never arrives as one exact number: glyph advances and the
/// producer's own text-matrix rounding make a single 10pt body emit as
/// 9.8/9.9/10.0/10.1/10.2. Quantizing to a tenth of a point (the previous
/// approach) left those in separate bins, so on a page whose small
/// table/caption text is perfectly uniform (every 7pt cell line identical) that
/// one bin could out-vote the spread-out prose and drag `body_size` down to the
/// small text's size. Every real body line then measured `>= 1.3x body` and was
/// mistaken for a heading. Cluster near-identical sizes first, then vote, so the
/// body and the genuinely smaller table text stay separate classes but the
/// body's own metric jitter does not split its vote.
fn estimate_body_size(sizes: &[f64]) -> f64 {
    if sizes.is_empty() {
        return 10.0;
    }
    let mut sorted: Vec<f64> = sizes
        .iter()
        .copied()
        .filter(|s| s.is_finite() && *s > 0.0)
        .collect();
    if sorted.is_empty() {
        return 10.0;
    }
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    // Greedy 1-D clustering: a size joins the current cluster while it is within
    // `CLUSTER_TOL` of that cluster's anchor (its smallest member), so jitter
    // chains cannot drift the anchor upward past a genuinely distinct size. 5%
    // keeps a 7pt table separate from a 10pt body (30% apart) while merging the
    // ~2-4% jitter one nominal size carries.
    const CLUSTER_TOL: f64 = 0.05;
    let mut clusters: Vec<(f64, usize)> = Vec::new();
    for &s in &sorted {
        match clusters.last_mut() {
            Some((anchor, count)) if s <= *anchor * (1.0 + CLUSTER_TOL) => *count += 1,
            _ => clusters.push((s, 1)),
        }
    }

    // Body is the size class with the most lines. Clusters are ascending, so
    // keeping the first on an equal count preserves the old smallest-on-tie rule.
    let (mut best, mut best_count) = clusters[0];
    for &(anchor, count) in &clusters[1..] {
        if count > best_count {
            best = anchor;
            best_count = count;
        }
    }
    best.max(1.0)
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

    /// Raise the counter at `depth` to `ordinal` when the source marker runs
    /// ahead of it, so the next item continues from the author's numbering.
    ///
    /// The counter alone restarts at 1 every time a `Body` line ends a list
    /// run, which silently renumbered an author-numbered document's sections
    /// (`2.`, `3.`, `4.` …) into a wall of `1.`. Adopting the marker only when
    /// it is *ahead* keeps the auto-numbered-producer case working: a source
    /// that repeats the same `1.` on every item never exceeds the counter, so
    /// it still numbers consecutively.
    fn adopt_ordinal(&mut self, depth: u8, ordinal: usize) {
        let d = depth as usize;
        if self.counters.len() <= d {
            self.counters.resize(d + 1, 0);
        }
        if ordinal > self.counters[d] {
            self.counters[d] = ordinal;
        }
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

    let marker = &line[idx];
    let mut skip = idx + 1;
    // A marker is separated from its item text by whitespace: either the
    // marker span itself carries a trailing space, or the next span is a
    // pure-space span. `"20."` immediately followed by `"0"` — the integer and
    // fractional parts of a decimal number kerned into two spans — has
    // neither, and is a value, not an ordered-list marker.
    let mut separated = marker.text.ends_with(|c: char| c.is_whitespace());
    if skip < line.len() && line[skip].text.chars().all(|c| c == ' ') {
        separated = true;
        skip += 1;
    }
    if skip >= line.len() {
        return None; // marker with no item text
    }
    if !separated {
        // No explicit space span: require a real horizontal gap. Kerning a
        // decimal apart places the two spans flush (~0 pt apart), while a
        // genuine list space leaves roughly a quarter-em or more. The same
        // applies to a bullet: a negative amount reaches this layer as glyph
        // runs, so `-218,48` is the lone marker-shaped span `-` followed
        // flush by the digits. Without a gap it is a sign, not a Markdown
        // bullet — treating it as one strips the minus and shifts the value
        // into a list, which silently turns every credit-note amount positive.
        let gap = line[skip].x - (marker.x + marker.advance);
        if gap <= 0.1 * marker.size.max(1.0) {
            return None;
        }
    }
    Some((is_ordered, skip))
}

/// The explicit index carried by a *bracketed* ordered marker (`[3]`), or
/// `None` for any other marker shape.
///
/// A bracketed numeric marker is an author-supplied citation index, not an
/// auto-numbered list bullet. Its number must be rendered verbatim rather than
/// re-derived from `ListRunState`: the run counter restarts at 1 every time a
/// wrapped continuation line (a `Body` line) ends the Markdown list run between
/// two markers, so a reference list `[1] [2] [3] …` collapsed to `1. 1. 1. …`
/// and the citations lost their identity. `[N]` is unambiguous — the digits
/// are the index — so prefer them. Dot/paren markers (`1.`, `1)`) are not
/// always authoritative (some producers emit the same number on every item),
/// so `explicit_dot_ordinal` is only adopted when it runs ahead of the counter.
fn explicit_bracketed_ordinal(line: &[Span]) -> Option<usize> {
    let marker = line.iter().find(|s| !s.text.trim().is_empty())?.text.trim();
    let inner = marker.strip_prefix('[')?.strip_suffix(']')?;
    if inner.is_empty() || !inner.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    inner.parse().ok()
}

/// The numeric value carried by a *dot/paren* ordered marker (`2.`, `2)`,
/// `(2)`), or `None` for any other marker shape.
///
/// Unlike a bracketed citation index this is not always authoritative — some
/// producers stamp the same `1.` on every item — so the caller only adopts it
/// when it runs ahead of the synthetic counter (see `ListRunState::adopt_ordinal`).
fn explicit_dot_ordinal(line: &[Span]) -> Option<usize> {
    let marker = line.iter().find(|s| !s.text.trim().is_empty())?.text.trim();
    let digits = if marker.starts_with('(') && marker.ends_with(')') && marker.len() <= 5 {
        &marker[1..marker.len().saturating_sub(1)]
    } else if (marker.ends_with('.') || marker.ends_with(')')) && marker.len() <= 5 {
        &marker[..marker.len() - 1]
    } else {
        return None;
    };
    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
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
        // Always advance the run counter so a following dot/paren marker keeps
        // counting on from here, but let an explicit `[N]` citation index win,
        // and let a dot/paren marker's own number win whenever it runs ahead of
        // the counter (see `adopt_ordinal`).
        let ordinal = list_state.next_ordinal(depth);
        let ordinal = if ordered {
            if let Some(n) = explicit_bracketed_ordinal(line) {
                n
            } else if let Some(n) = explicit_dot_ordinal(line).filter(|&n| n > ordinal) {
                list_state.adopt_ordinal(depth, n);
                n
            } else {
                ordinal
            }
        } else {
            ordinal
        };
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
        // A prefix/suffix pair only represents ONE wrapper spanning the whole
        // string if the closing marker doesn't recur inside it. Without this
        // check, two independently-styled runs concatenated with a space —
        // e.g. `<u>Date</u> <u>:</u>` — look exactly like one outer `<u>...
        // </u>` wrap (prefix "<u>" + suffix "</u>"), so naively stripping
        // them removed the FIRST run's own `<u>` and the SECOND run's own
        // `</u>`, leaving the middle `</u> <u>` pair dangling unmatched
        // (`Date</u> <u>:` — invalid Markdown/HTML). Bail out instead of
        // guessing when the marker isn't unique to the true outer edges.
        if let Some(inner) = t.strip_prefix("<u>").and_then(|r| r.strip_suffix("</u>")) {
            if inner.contains("</u>") {
                break;
            }
            t = inner.trim();
            continue;
        }
        if let Some(inner) = t.strip_prefix("**").and_then(|r| r.strip_suffix("**")) {
            if inner.contains("**") {
                break;
            }
            t = inner.trim();
            continue;
        }
        if let Some(inner) = t.strip_prefix('*').and_then(|r| r.strip_suffix('*')) {
            if inner.contains('*') {
                break;
            }
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
fn join_paragraph_text(text: &mut String, next: &str) {
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
            word_advance: text.len() as f64 * 6.0,
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
    fn long_word_run_is_not_a_line_break() {
        // "Hello" is 30pt wide; "world" starts 33pt later, i.e. one normal
        // space after Hello's advance. The raw start-to-start distance is
        // 33 > 2.5 * size, but the *whitespace* gap is only 3pt, so the two
        // words belong on one line.
        let line = vec![
            span("Hello", 100.0, (false, false, false)),
            span("world", 133.0, (false, false, false)),
        ];
        assert_eq!(render_spans(&line), "Hello world");
    }

    #[test]
    fn wide_column_gutter_still_breaks_the_line() {
        // Second run starts 40pt after the first run's right edge: a genuine
        // column/element boundary must still become a hard newline.
        let line = vec![
            span("Hello", 100.0, (false, false, false)),
            span("world", 170.0, (false, false, false)),
        ];
        assert_eq!(render_spans(&line), "Hello\nworld");
    }

    #[test]
    fn unstyled_line_has_no_markers() {
        let line = words_spaced(&[("Hello", (false, false, false)), ("world", (false, false, false))]);
        assert_eq!(render_spans(&line), "Hello world");
    }

    #[test]
    fn bold_run_stays_open_across_words() {
        // A style run spanning several words must render as ONE emphasis span:
        // `**Bold text**`, not `**Bold** **text**`. (This test previously
        // asserted the fragmented form; that was the bug this change fixes.)
        let line = words_spaced(&[
            ("Bold", (true, false, false)),
            ("text", (true, false, false)),
        ]);
        assert_eq!(render_spans(&line), "**Bold text**");
    }

    #[test]
    fn adjacent_bold_spans_across_a_gap_merge() {
        // No explicit space span: the words are separated only by their x-gap.
        // "Word1" is 5*6=30pt wide and starts at 100; "Word2" starts at 133, so
        // the whitespace gap is 3pt (> 0.65 * space_adv, < 2.5 * size).
        let line = vec![
            span("Word1", 100.0, (true, false, false)),
            span("Word2", 133.0, (true, false, false)),
        ];
        assert_eq!(render_spans(&line), "**Word1 Word2**");
    }

    #[test]
    fn adjacent_bold_spans_across_a_space_span_merge() {
        // Same run, but the space arrives as its own whitespace-only span.
        let line = words_spaced(&[
            ("Word1", (true, false, false)),
            ("Word2", (true, false, false)),
        ]);
        assert_eq!(render_spans(&line), "**Word1 Word2**");
    }

    #[test]
    fn bold_then_unstyled_separates_cleanly() {
        let line = words_spaced(&[
            ("Bold", (true, false, false)),
            ("normal", (false, false, false)),
        ]);
        assert_eq!(render_spans(&line), "**Bold** normal");
    }

    #[test]
    fn bold_then_italic_separates_cleanly() {
        let line = words_spaced(&[
            ("Bold", (true, false, false)),
            ("Italic", (false, true, false)),
        ]);
        assert_eq!(render_spans(&line), "**Bold** *Italic*");
    }

    #[test]
    fn column_break_closes_style_and_breaks_line() {
        // Second run starts 40pt after the first run's right edge: a genuine
        // column/element boundary must still become a hard newline and close
        // the open emphasis before it.
        let line = vec![
            span("Hello", 100.0, (true, false, false)),
            span("world", 170.0, (true, false, false)),
        ];
        assert_eq!(render_spans(&line), "**Hello**\n**world**");
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

    #[test]
    fn embedded_trailing_space_is_moved_outside_bold_delimiters() {
        // Regression for SeConnecterAMonComptePartenaire.pdf: the producer draws
        // the bold run "2 " (digit + trailing space) as ONE glyph run, so the
        // space is not its own span and the `is_space` branch never fires. It
        // used to be pushed inside the emphasis, producing the invalid `**2 **`
        // (a closing delimiter preceded by whitespace), which also swallowed the
        // following `**mail**` run. The space must land after the closing `**`.
        let line = vec![
            span("dans ", 100.0, (false, false, false)),
            span("2 ", 130.0, (true, false, false)),
            span("mail", 142.0, (false, false, false)),
        ];
        let rendered = render_spans(&line);
        assert_eq!(rendered, "dans **2** mail");
        assert!(
            !rendered.contains("2 **"),
            "closing delimiter must not be preceded by a space: {rendered:?}"
        );
    }

    #[test]
    fn embedded_trailing_space_is_moved_out_before_a_different_style() {
        // Same pathology when the next span switches style: the bold run must
        // close flush against "2" and the space live between the two runs.
        let line = vec![
            span("dans ", 100.0, (false, false, false)),
            span("2 ", 130.0, (true, false, false)),
            span("mail", 142.0, (false, true, false)),
        ];
        let rendered = render_spans(&line);
        assert_eq!(rendered, "dans **2** *mail*");
        assert!(!rendered.contains("2 **"), "{rendered:?}");
    }

    #[test]
    fn embedded_leading_space_is_moved_outside_bold_delimiters() {
        // Mirror case: a span whose own text begins with a space (`" 2"`).
        // An opening delimiter followed by whitespace (`** 2**`) is not
        // emphasis either; the space belongs before the opening `**`.
        let line = vec![
            span(" 2", 100.0, (true, false, false)),
            span("mail", 120.0, (false, false, false)),
        ];
        let rendered = render_spans(&line);
        assert_eq!(rendered, "**2** mail");
        assert!(
            !rendered.starts_with("** "),
            "opening delimiter must not be followed by a space: {rendered:?}"
        );
    }

    #[test]
    fn projection_columns_keep_gutter_straddling_span() {
        // Regression for fnfe_Facture_FR_MINIMUM.pdf: `detect_projection_two_columns`
        // assigned each column half with two independent half-open filters
        // (`x + advance <= gx` and `x >= gx`). A span whose box *straddles* the
        // page gutter satisfied neither test, so its text was dropped from BOTH
        // columns — silently deleting the middle `0` of `120 000,00 €` (and the
        // `d` of `Fiducial`) from the structured `blocks` channel while the
        // Markdown, rendered via the band path, kept it. Every span must land in
        // exactly one side.
        fn mk(text: &str, x: f64, adv: f64, y: f64) -> Span {
            Span {
                text: text.to_string(),
                x,
                y,
                size: 10.0,
                advance: adv,
                word_advance: adv,
                is_bold: false,
                is_italic: false,
                is_underline: false,
                is_vertical: false,
            }
        }
        let mut lines: Vec<Vec<Span>> = Vec::new();
        for i in 0..4 {
            let y = 700.0 - 10.0 * i as f64;
            // Left column [10,65]; right column [100,160]; the "Z" span
            // [66,86] straddles the resulting gx = 82.5.
            lines.push(vec![
                mk("L", 10.0, 55.0, y),
                mk("Z", 66.0, 20.0, y),
                mk("R", 100.0, 60.0, y),
            ]);
        }
        // Left-only and right-only rows give the projection enough support.
        lines.push(vec![mk("L", 10.0, 55.0, 660.0)]);
        lines.push(vec![mk("R", 100.0, 60.0, 650.0)]);

        let pc = detect_projection_two_columns(&lines)
            .expect("a consistent two-column projection must be detected");
        let kept: String = pc
            .left
            .iter()
            .chain(pc.right.iter())
            .flat_map(|row| row.iter())
            .map(|s| s.text.as_str())
            .collect();
        assert_eq!(
            kept.matches('Z').count(),
            4,
            "a gutter-straddling span must be kept whole in one column: {:?}",
            pc.left
                .iter()
                .map(|r| r.iter().map(|s| s.text.clone()).collect::<Vec<_>>())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn projection_columns_keep_unsplittable_straddling_line() {
        // A whole line emitted as ONE span (PDF producers commonly draw
        // `TVA Intracommunautaire : FR23391284650` as a single TJ run)
        // crosses the column gutter, so the total-complement partition puts
        // the entire span on one side and leaves the other half empty. The
        // `both halves non-empty` guard then sends the row to the top/bottom
        // fallback, which only accepts rows at the block's edge — so a
        // mid-block row vanished from BOTH reading-order streams (and from
        // the `blocks` channel) while the Markdown, rendered through the
        // separate band path, kept it. Unlike the multi-span straddler
        // covered by `projection_columns_keep_gutter_straddling_span`, no
        // split of this row can produce two non-empty halves, so it must be
        // kept whole on the side its box leans to.
        fn mk(text: &str, x: f64, adv: f64, y: f64) -> Span {
            Span {
                text: text.to_string(),
                x,
                y,
                size: 10.0,
                advance: adv,
                word_advance: adv,
                is_bold: false,
                is_italic: false,
                is_underline: false,
                is_vertical: false,
            }
        }
        // Left column [10,65], right column [100,160] -> gx = 82.5.
        // The left-only / right-only rows set col_top=690, col_bottom=660 so
        // the straddler at y=675 is strictly *inside* the column block.
        let lines: Vec<Vec<Span>> = vec![
            vec![mk("L", 10.0, 55.0, 700.0), mk("R", 100.0, 60.0, 700.0)],
            vec![mk("A", 10.0, 55.0, 690.0)],
            vec![mk("B", 100.0, 60.0, 680.0)],
            vec![mk("TVA Intracommunautaire : FR23391284650", 10.0, 130.0, 675.0)],
            vec![mk("C", 10.0, 55.0, 670.0)],
            vec![mk("D", 100.0, 60.0, 660.0)],
        ];
        let pc = detect_projection_two_columns(&lines)
            .expect("a consistent two-column projection must be detected");
        let kept: String = pc
            .left
            .iter()
            .chain(pc.right.iter())
            .flat_map(|row| row.iter())
            .map(|s| s.text.as_str())
            .collect();
        assert!(
            kept.contains("TVA Intracommunautaire"),
            "a single-span line crossing the gutter must not vanish: left={:?} right={:?} top={:?} bottom={:?}",
            pc.left.iter().map(|r| r.iter().map(|s| s.text.clone()).collect::<Vec<_>>()).collect::<Vec<_>>(),
            pc.right.iter().map(|r| r.iter().map(|s| s.text.clone()).collect::<Vec<_>>()).collect::<Vec<_>>(),
            pc.top_full.iter().map(|r| r.iter().map(|s| s.text.clone()).collect::<Vec<_>>()).collect::<Vec<_>>(),
            pc.bottom_full.iter().map(|r| r.iter().map(|s| s.text.clone()).collect::<Vec<_>>()).collect::<Vec<_>>(),
        );
    }

    #[test]
    fn projection_columns_reject_centered_words_split_across_runs() {
        // Regression for the FR "Statut EI et régime micro entreprise" slide
        // deck: every visual line is a centered single-column sentence, but the
        // producer emits each word as several `Tj` runs (`Le statut et le
        // r|égim|e`). `detect_projection_two_columns` dropped the straddling run
        // from *both* halves, measured that run's own width as a white gutter,
        // and read the whole slide as two columns — emitting every line's tail
        // after every line's head (`# Le statut et le r` … `# égime`). A
        // straddler that bridges `l_end`..`r_start` is filler, not a gutter.
        fn mk(text: &str, x: f64, adv: f64, y: f64) -> Span {
            Span {
                text: text.to_string(),
                x,
                y,
                size: 10.0,
                advance: adv,
                word_advance: adv,
                is_bold: false,
                is_italic: false,
                is_underline: false,
                is_vertical: false,
            }
        }
        let mut lines: Vec<Vec<Span>> = Vec::new();
        for i in 0..4 {
            let y = 700.0 - 10.0 * i as f64;
            lines.push(vec![
                mk("Le statut et le r", 10.0, 55.0, y),
                mk("égim", 65.0, 30.0, y),
                mk("e ", 95.0, 65.0, y),
            ]);
        }
        // A head-only and a tail-only row give the projection its usual support.
        lines.push(vec![mk("Le statut et le r", 10.0, 55.0, 660.0)]);
        lines.push(vec![mk("e ", 95.0, 65.0, 650.0)]);

        assert!(
            detect_projection_two_columns(&lines).is_none(),
            "a straddling run that bridges the gap must not be read as a column gutter"
        );
        let streams = page_read_order(&lines);
        assert_eq!(streams.len(), 1, "centered prose must stay one column");
        let joined: String = streams[0]
            .iter()
            .flat_map(|l| l.iter())
            .map(|s| s.text.as_str())
            .collect();
        assert!(
            joined.contains("Le statut et le régime"),
            "the words must stay intact in reading order, got {joined:?}"
        );
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
            word_advance: text.len() as f64 * size * 0.6,
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

    #[test]
    fn uniform_small_table_text_does_not_become_the_body_size() {
        // Regression: a page whose small (7pt) table/caption cells are perfectly
        // uniform while the real 10pt prose carries ordinary metric jitter
        // (9.8/9.9/10.0/10.1/10.2). The old per-0.1pt mode saw 16 identical 7pt
        // lines beat each 4-line prose bin and returned 6.97; every real body
        // line then measured >= 1.3 * body and was emitted as a `##` heading
        // (observed on scratch/samples/mistral-pdf-tests.pdf page 6).
        let mut lines: Vec<Vec<Span>> = Vec::new();
        for _ in 0..16 {
            lines.push(one_span_line("cell", 7.0, false));
        }
        for i in 0..20 {
            let size = [9.8, 9.9, 10.0, 10.1, 10.2][i % 5];
            lines.push(one_span_line(&format!("Body line {i}"), size, false));
        }

        let body = body_size_for(&lines);
        assert!(
            body >= 9.5,
            "body size must follow the 10pt prose cluster, not the 7pt table: got {body}"
        );

        // The prose line itself must stay ordinary body text, not a heading.
        let prose = one_span_line("Our work demonstrates that models compress knowledge", 10.0, false);
        assert_eq!(
            detect_heading_level(&prose, body),
            None,
            "a body-sized prose line must not be promoted to a heading"
        );
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
    fn kerned_decimal_is_not_an_ordered_list_marker() {
        // A tax rate "20.0" reaches this layer as the marker-shaped span "20."
        // immediately followed (flush, negative kern) by "0". That is one
        // decimal number, not an ordered-list item; treating it as one both
        // corrupts the value and renumbers it to "1.".
        let line = vec![
            word("20.", 100.0, BODY, false),
            word("0", 118.0, BODY, false), // flush against "20."'s advance (100 + 18)
        ];
        assert!(
            detect_list_marker(&line).is_none(),
            "the integer part of a kerned decimal must not become a list marker"
        );
    }

    #[test]
    fn negative_amount_minus_is_not_an_unordered_list_marker() {
        // A credit-note amount reaches this layer as glyph runs: `-218,48`
        // arrives as the lone span `-` immediately followed (flush, ~0 pt gap)
        // by the digits. That is a numeric sign, not a Markdown bullet;
        // treating it as one strips the minus and turns the credit positive.
        let line = vec![
            word("-", 100.0, BODY, false),
            word("218,48", 106.0, BODY, false), // flush against "-"'s advance (100 + 6)
            word(" ", 142.0, BODY, false),
            word("€", 148.0, BODY, false),
        ];
        assert!(
            detect_list_marker(&line).is_none(),
            "a negative amount's sign must not become an unordered-list bullet"
        );
    }

    #[test]
    fn dash_bullet_with_a_real_space_gap_is_still_detected() {
        // No explicit space span, but a genuine positional gap: a real dash
        // bullet must survive the negative-sign guard.
        let line = vec![
            word("-", 100.0, BODY, false),
            word("Item text", 112.0, BODY, false),
        ];
        let (ordered, _) = detect_list_marker(&line).expect("real bullet with a gap");
        assert!(!ordered);
    }

    #[test]
    fn ordered_marker_with_a_real_space_gap_is_still_detected() {
        // No explicit space span, but a genuine positional gap: a real ordered
        // marker must survive the kerned-decimal guard.
        let line = vec![
            word("1.", 100.0, BODY, false),
            word("Item text", 130.0, BODY, false),
        ];
        let (ordered, _) = detect_list_marker(&line).expect("real marker with a gap");
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
    fn bracketed_citation_markers_keep_their_own_number_across_continuation_lines() {
        // A paper's reference list reaches this layer as "[1] Ainslie …",
        // "… continuation body line", "[2] Austin …". The continuation `Body`
        // line ends the Markdown list run, so the synthetic run counter
        // restarted at 1 for every item and the whole bibliography rendered as
        // "1. 1. 1. …". The explicit bracket index must survive.
        let bracketed = |n: usize, text: &str| -> Vec<Span> {
            let marker = format!("[{n}]");
            vec![
                word(&marker, 100.0, BODY, false),
                word(" ", 100.0 + marker.len() as f64 * BODY * 0.6, BODY, false),
                word(text, 120.0, BODY, false),
            ]
        };
        let lines: Vec<Vec<Span>> = vec![
            bracketed(1, "Joshua Ainslie, James Lee-Thorp"),
            one_span_line("Sumit Sanghai. Gqa: Training generalized multi-query", BODY, false),
            bracketed(2, "Jacob Austin, Augustus Odena"),
            one_span_line("language models. arXiv preprint arXiv:2108.07732, 2021.", BODY, false),
            bracketed(11, "Michael Collins"),
        ];
        let md = render_cluster(&lines);
        assert!(md.contains("1. Joshua Ainslie"), "{md}");
        assert!(md.contains("2. Jacob Austin"), "{md}");
        assert!(md.contains("11. Michael Collins"), "{md}");
        assert!(!md.contains("1. Jacob Austin"), "{md}");
    }

    #[test]
    fn dot_numbered_sections_keep_their_own_number_across_body_lines() {
        // A contract's numbered sections each carry their own literal number
        // (`1.`, `2.`, `3.` …) and are separated by ordinary body lines. Every
        // body line ends the Markdown list run, so the synthetic counter
        // restarted at 1 and all the sections rendered as "1." — the real
        // section numbering the document carries was lost (observed on
        // scratch/samples/text_style_complex.pdf, where sections 2–5 became
        // "1."). A dot marker that runs ahead of the counter must win.
        let lines: Vec<Vec<Span>> = vec![
            bulleted_line("1."),
            one_span_line("Body between one and two.", BODY, false),
            bulleted_line("2."),
            one_span_line("Body between two and three.", BODY, false),
            bulleted_line("3."),
            one_span_line("Body between three and four.", BODY, false),
            bulleted_line("5."),
        ];
        let md = render_cluster(&lines);
        assert!(md.contains("1. Item text"), "{md}");
        assert!(md.contains("2. Item text"), "{md}");
        assert!(md.contains("3. Item text"), "{md}");
        assert!(md.contains("5. Item text"), "{md}");
        assert_eq!(md.matches("1. Item text").count(), 1, "later sections must not collapse to 1.:\n{md}");
    }

    #[test]
    fn repeated_one_marker_still_numbers_consecutively() {
        // The complementary case the counter exists for: a producer that stamps
        // the same "1." on every item must still emit 1, 2, 3 (adopting a
        // literal number only when it runs *ahead* never fires here).
        let lines = vec![bulleted_line("1."), bulleted_line("1."), bulleted_line("1.")];
        let md = render_cluster(&lines);
        assert!(md.contains("1. Item text"), "{md}");
        assert!(md.contains("2. Item text"), "{md}");
        assert!(md.contains("3. Item text"), "{md}");
    }

    #[test]
    fn explicit_dot_ordinal_parses_the_three_marker_shapes() {
        assert_eq!(explicit_dot_ordinal(&bulleted_line("2.")), Some(2));
        assert_eq!(explicit_dot_ordinal(&bulleted_line("3)")), Some(3));
        assert_eq!(explicit_dot_ordinal(&bulleted_line("(4)")), Some(4));
        assert_eq!(explicit_dot_ordinal(&bulleted_line("-")), None);
        assert_eq!(explicit_dot_ordinal(&bulleted_line("[7]")), None);
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
    fn negative_amount_is_not_classified_as_a_list_block() {
        // Iteration 14 (fnfe_Avoir_FR_type381_BASIC.pdf): a credit-note amount
        // rendered as the single line `-20,48 €` was classified `kind:"list"`
        // by the bare `t.starts_with('-')` test in `build_doc_blocks`, even
        // though the markdown path (`detect_list_marker`) already rejects the
        // same sign because it has no separating whitespace. The two channels
        // must agree: a negative amount is a value, not a bullet.
        fn line(text: &str, x: f64, y: f64) -> Vec<Span> {
            vec![Span {
                text: text.to_string(),
                x,
                y,
                size: 10.0,
                advance: text.len() as f64 * 6.0,
                word_advance: text.len() as f64 * 6.0,
                is_bold: false,
                is_italic: false,
                is_underline: false,
                is_vertical: false,
            }]
        }
        let lines = vec![line("Description", 100.0, 700.0), line("-20,48 €", 200.0, 660.0)];
        let blocks = build_doc_blocks(&lines, 842.0);
        let neg_block = blocks
            .iter()
            .find(|b| b.text.contains("20,48"))
            .expect("negative amount must survive into the blocks channel");
        assert_eq!(
            neg_block.kind, "body",
            "a negative amount is a value, not a bullet list item: {neg_block:?}"
        );
    }

    #[test]
    fn dash_bullet_line_is_still_classified_as_a_list_block() {
        // The other direction: a real `- item` (dash then whitespace) must
        // remain a list block after the negative-sign guard.
        fn line(text: &str, x: f64, y: f64) -> Vec<Span> {
            vec![Span {
                text: text.to_string(),
                x,
                y,
                size: 10.0,
                advance: text.len() as f64 * 6.0,
                word_advance: text.len() as f64 * 6.0,
                is_bold: false,
                is_italic: false,
                is_underline: false,
                is_vertical: false,
            }]
        }
        let lines = vec![line("Description", 100.0, 700.0), line("- Item text", 100.0, 660.0)];
        let blocks = build_doc_blocks(&lines, 842.0);
        let list_block = blocks
            .iter()
            .find(|b| b.text.contains("Item text"))
            .expect("bullet line must survive into the blocks channel");
        assert_eq!(
            list_block.kind, "list",
            "a real dash bullet separated by whitespace must stay a list: {list_block:?}"
        );
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
    fn non_overlapping_adjacent_columns_do_not_merge() {
        // Regression (iteration 11, akretion_invoice_EN16931.pdf): the
        // `DEVISE : EURO (EUR)` cell in the page's right column and the
        // `NOM & DESIGNATION, Période` table header starting ~140pt to its
        // left are vertically adjacent body lines (pitch gap = 2pt, inside
        // the 1.8*10 cap) whose horizontal extents share nothing at all. The
        // first-merge exemption from the left-edge check used to fuse them
        // into one block `DEVISE : EURO (EUR) NOM & DESIGNATION, Période`,
        // even though the markdown channel — a separate code path — keeps
        // them apart. They must remain two blocks.
        let right_col = body_block(350.0, 700.0, "DEVISE : EURO (EUR)");
        let left_col = body_block(100.0, 688.0, "NOM & DESIGNATION, Période");
        let merged = merge_paragraph_lines(vec![right_col, left_col]);
        assert_eq!(
            merged.len(),
            2,
            "adjacent lines from different columns with no horizontal overlap must not merge: {:?}",
            merged.iter().map(|b| b.text.as_str()).collect::<Vec<_>>()
        );
        assert_eq!(merged[0].text, "DEVISE : EURO (EUR)");
        assert_eq!(merged[1].text, "NOM & DESIGNATION, Période");
    }

    #[test]
    fn short_first_line_that_overlaps_its_continuation_still_merges() {
        // The overlap guard must not defeat the first-line-indent exemption:
        // a short indented first line still overlaps the wider flush
        // continuation, so the two remain one paragraph.
        let blocks = vec![
            body_block(120.0, 700.0, "A short opener"),
            body_block(100.0, 688.0, "a much wider second line"),
        ];
        let merged = merge_paragraph_lines(blocks);
        assert_eq!(merged.len(), 1, "overlapping lines must still merge");
        assert_eq!(merged[0].text, "A short opener a much wider second line");
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

    #[test]
    fn real_compound_hyphen_is_kept_on_merge() {
        // A clitic tail (inversion) is not a line-wrap fragment.
        let mut text = "Comment va-".to_string();
        join_paragraph_text(&mut text, "t-il ?");
        assert_eq!(text, "Comment va-t-il ?");
        // A hyphenated compound whose second element is a whole word.
        let mut text = "un non-".to_string();
        join_paragraph_text(&mut text, "professionnel ici");
        assert_eq!(text, "un non-professionnel ici");
        // A capitalised continuation is never a line-wrap fragment either.
        let mut text = "la ville de".to_string();
        join_paragraph_text(&mut text, "Paris");
        assert_eq!(text, "la ville de Paris");
    }

    // -- page_two_columns_rows: column membership of unpaired lines -----------

    fn col_span(text: &str, x: f64, y: f64) -> Span {
        Span {
            text: text.to_string(),
            x,
            y,
            size: 10.0,
            advance: text.len() as f64 * 6.0,
            word_advance: text.len() as f64 * 6.0,
            is_bold: false,
            is_italic: false,
            is_underline: false,
            is_vertical: false,
        }
    }

    fn two_col_row(y: f64, left: &[(&str, f64)], right: &[(&str, f64)]) -> Vec<Span> {
        let mut v: Vec<Span> = left.iter().map(|(t, x)| col_span(t, *x, y)).collect();
        v.extend(right.iter().map(|(t, x)| col_span(t, *x, y)));
        v
    }

    #[test]
    fn unpaired_left_line_inside_column_span_stays_in_left_column() {
        // A two-column page whose left column is one short line taller than the
        // right: that line's baseline coincides with no right-column line, so it
        // can never be paired with a `split_row_columns` row. It must still be
        // routed to the left column, between the shared rows above it and the
        // footer rows below — not dumped into `bottom_full`, which renders after
        // the entire right column (the original bug: "intercontinentaux." jumped
        // to the end of the page, past the whole English paragraph).
        let lines = vec![
            two_col_row(228.0,
                &[("Le", 50.0), ("tarif", 70.0), ("réservé", 108.0)],
                &[("The", 300.0), ("fare", 320.0), ("applies", 348.0)]),
            two_col_row(221.0,
                &[("les", 50.0), ("dates", 70.0), ("du", 108.0)],
                &[("the", 300.0), ("dates", 320.0), ("below", 358.0)]),
            two_col_row(214.0,
                &[("pour", 50.0), ("les", 82.0), ("vols", 108.0)],
                &[("for", 300.0), ("the", 326.0), ("flights", 352.0)]),
            two_col_row(180.0,
                &[("La", 50.0), ("Première", 70.0), ("cabins", 130.0)],
                &[("equivalent", 300.0), ("in", 360.0), ("currency", 376.0)]),
            vec![col_span("intercontinentaux.", 50.0, 174.0)],
            two_col_row(130.0,
                &[("Pour", 50.0), ("plus", 70.0), ("d'information,", 100.0)],
                &[("For", 300.0), ("more", 320.0), ("information,", 348.0)]),
            two_col_row(123.0,
                &[("réservations", 50.0), ("en", 128.0), ("cliquant", 148.0)],
                &[("France", 300.0), ("web", 344.0), ("site", 370.0)]),
        ];
        let pc = page_two_columns_rows(&lines).expect("two consistent columns must be detected");
        let bottom: Vec<String> = pc.bottom_full.iter().map(|l| render_line_text(l)).collect();
        assert!(bottom.is_empty(), "no unpaired column line may be dumped to the footer: {bottom:?}");
        assert!(pc.top_full.is_empty(), "nothing sits above the column block");
        let left: Vec<String> = pc.left.iter().map(|l| render_line_text(l)).collect();
        let idx = left
            .iter()
            .position(|t| t.contains("intercontinentaux."))
            .expect("the taller left column's tail line must stay in the left stream");
        assert!(
            left[..idx].iter().any(|t| t.contains("vols")),
            "tail line must follow the left column body: {left:?}"
        );
        assert!(
            left[idx + 1..].iter().any(|t| t.contains("réservations")),
            "tail line must precede the left column footer: {left:?}"
        );
    }

    #[test]
    fn split_hard_breaks_subtracts_previous_span_advance() {
        // `render_spans` measures inter-span whitespace as
        // `next.x - prev.x - prev.advance`; `split_hard_breaks` must use the
        // same residual so the pre-split only cuts where a render would hard
        // break. Here "Nougat de l'" is one span whose own 78pt advance carries
        // the next span's start 78pt to the right — far past the 2.5em (25pt)
        // hard-break threshold if measured start-to-start, but the actual
        // whitespace between them is nil. Measuring the raw distance split the
        // product description into separate lines ("Nougat de l'" / "Abbaye" /
        // " 250g"); the residual gap must keep it one line.
        let line = vec![
            col_span("Nougat de l'", 50.0, 300.0),
            col_span("Abbaye", 128.0, 300.0),
            col_span("250g", 170.0, 300.0),
        ];
        let segs = split_hard_breaks(&line);
        let rendered: Vec<String> = segs.iter().map(|s| render_spans(s)).collect();
        assert_eq!(
            segs.len(),
            1,
            "no spurious mid-line break: {rendered:?}"
        );
        // The whole-row render agrees: it emits no hard newline, so the
        // pre-split must not either.
        assert!(
            !render_spans(&line).contains('\n'),
            "render_spans keeps the row on one line"
        );
        assert!(render_spans(&line).contains("Abbaye 250g"));
    }

    #[test]
    fn split_line_segments_subtracts_previous_span_advance() {
        // Same residual-gap rule as `render_spans` and `split_hard_breaks`: the
        // whitespace between two spans is `next.x - prev.x - prev.advance`. A
        // 78pt span followed flush by the next span therefore has no gap at
        // all, even though its own advance carries the next start 78pt to the
        // right — far past the 2.5em (25pt) / 20pt segment threshold if the raw
        // start-to-start distance is measured. The raw form carved one visual
        // line into spurious "columns" here.
        let line = vec![
            col_span("Nougat de l'", 50.0, 300.0),
            col_span("Abbaye", 128.0, 300.0),
            col_span("250g", 170.0, 300.0),
        ];
        let segs = split_line_segments(&line);
        assert_eq!(
            segs.len(),
            1,
            "flush spans must stay one segment: {:?}",
            segs.iter().map(|s| render_spans(s)).collect::<Vec<_>>()
        );

        // A genuine 30pt empty gutter (previous span ends 24pt in, next starts
        // 30pt after that) must still split into two segments.
        let gutter = vec![
            col_span("left", 50.0, 300.0),
            col_span("right", 104.0, 300.0),
        ];
        assert_eq!(
            split_line_segments(&gutter).len(),
            2,
            "a real column gutter must still split"
        );
    }
}

#[cfg(test)]
mod column_band_tests {
    use super::*;

    fn sp(text: &str, x: f64, y: f64) -> Span {
        Span {
            text: text.to_string(),
            x,
            y,
            size: 10.0,
            advance: text.len() as f64 * 6.0,
            word_advance: text.len() as f64 * 6.0,
            is_bold: false,
            is_italic: false,
            is_underline: false,
            is_vertical: false,
        }
    }

    /// One row of a mixed prose/table two-column block. The four lower-case
    /// left-column words end at x=146; the table label starts at 176 (a 30pt
    /// page gutter) and its value at 215 (a 15pt internal table gutter). The
    /// page gutter is wider than the table's own gutter, so
    /// `split_row_columns` cuts at the page gutter; `clean(right)` is false
    /// because the right half is itself a two-column grid. The all-lowercase
    /// left rows read as a wrapped prose column (each row continues the next).
    fn prose_beside_grid_row(y: f64, val: &str) -> Vec<Span> {
        vec![
            sp("lora", 50.0, y),
            sp("ipsu", 74.0, y),
            sp("dolo", 98.0, y),
            sp("sita", 122.0, y),
            sp("Ra", 176.0, y),
            sp("Rb", 188.0, y),
            sp(val, 215.0, y),
        ]
    }

    #[test]
    fn prose_beside_a_multicolumn_grid_keeps_the_column_band() {
        let lines = vec![
            prose_beside_grid_row(300.0, "V1"),
            prose_beside_grid_row(290.0, "V2"),
            prose_beside_grid_row(280.0, "V3"),
            prose_beside_grid_row(270.0, "V4"),
            // A caption row whose page gutter is only 10pt (< 1.2em): the
            // standalone detector misses it and only the established-gutter
            // fallback (`split_row_at_gutter`) can keep it in the block.
            vec![
                sp("lora", 50.0, 260.0),
                sp("ipsu", 74.0, 260.0),
                sp("dolo", 98.0, 260.0),
                sp("sita", 122.0, 260.0),
                sp("Ca", 156.0, 260.0),
                sp("Cb", 168.0, 260.0),
                sp("Cc", 180.0, 260.0),
            ],
            // A full-width heading far below must start its own block.
            vec![sp("Heading", 50.0, 150.0)],
        ];
        let bands = detect_column_bands(&lines);
        assert_eq!(bands.len(), 2, "one Columns band, then the Full heading band");
        match &bands[0] {
            ColumnBand::Columns { left, right } => {
                let lt: Vec<String> = left.iter().map(|l| render_line_text(l)).collect();
                let rt: Vec<String> = right.iter().map(|l| render_line_text(l)).collect();
                assert_eq!(lt.len(), 5, "all five prose rows stay in the left stream: {lt:?}");
                assert_eq!(rt.len(), 5, "all five grid/caption rows stay in the right stream: {rt:?}");
                assert!(
                    lt.iter().all(|t| !t.contains("Ra") && !t.contains("V")),
                    "no table cell may be woven into the prose: {lt:?}"
                );
                assert!(
                    rt.iter().all(|t| !t.contains("lora")
                        && !t.contains("ipsu")
                        && !t.contains("dolo")
                        && !t.contains("sita")),
                    "no prose may be woven into the grid stream: {rt:?}"
                );
                assert!(rt[0].contains("Ra") && rt[0].contains("V1"), "header + row 1: {rt:?}");
            }
            ColumnBand::Full(_) => panic!("mixed prose/table block was merged instead of split"),
        }
        match &bands[1] {
            ColumnBand::Full(rows) => assert_eq!(render_line_text(&rows[0]), "Heading"),
            ColumnBand::Columns { .. } => panic!("a full-width heading must not be a column"),
        }
    }

    #[test]
    fn column_streams_keep_a_paragraph_break() {
        // The left stream ends with an unterminated lowercase word; the right
        // stream starts lowercase. Without a blank line between the streams the
        // paragraph reflow joins them into one sentence (column weld).
        let left = vec![
            vec![sp("alpha", 50.0, 300.0), sp("beta", 80.0, 300.0)],
            vec![sp("the", 50.0, 290.0)],
        ];
        let right = vec![vec![sp("gamma", 300.0, 300.0)]];
        let bands = vec![ColumnBand::Columns { left, right }];
        let mut out = String::new();
        let mut prev = None;
        let mut ls = ListRunState::default();
        push_band_lines(&mut out, &bands, &mut prev, &mut ls, 10.0);
        assert!(
            out.contains("\n\n"),
            "no blank line between column streams: {out:?}"
        );
        let reflowed = crate::reflow::reflow_markdown(&out);
        assert!(
            !reflowed.contains("the gamma"),
            "left/right column streams were welded by reflow: {reflowed:?}"
        );
    }

    #[test]
    fn a_grid_cut_in_two_is_not_a_prose_column_band() {
        // Three table columns at x=50/120/200. `split_row_columns` cuts at the
        // widest (rightmost) gap, leaving the left half still holding two
        // columns and the right half a single short cell. This is one grid, not
        // a prose/table split, so it must not become a `Columns` band (which
        // would transpose the table).
        let row = |y: f64| {
            vec![
                sp("aaaa", 50.0, y),
                sp("bbbb", 120.0, y),
                sp("cccc", 200.0, y),
            ]
        };
        let lines = vec![row(300.0), row(290.0), row(280.0), row(270.0)];
        let bands = detect_column_bands(&lines);
        assert!(
            bands.iter().all(|b| !matches!(b, ColumnBand::Columns { .. })),
            "a multi-column grid must not be read as prose columns: {:?} bands",
            bands.len()
        );
    }

    #[test]
    fn wordy_label_value_grid_is_not_transposed_into_columns() {
        // A label/value table whose long label cells give the left half a high
        // word count — a word-count-only test would call it "prose" — but the
        // cells are independent (each ends with a bracket; none continues into
        // the next). It must keep its row order rather than be split into a
        // left label stream and a right value stream.
        let row = |y: f64, val: &str| {
            vec![
                sp("Alpha", 50.0, y),
                sp("beta", 80.0, y),
                sp("gamma", 104.0, y),
                sp("delta)", 134.0, y),
                sp(val, 200.0, y),
                sp("5,5%", 240.0, y),
                sp("zz", 280.0, y),
            ]
        };
        let lines = vec![
            row(300.0, "1,91"),
            row(290.0, "2,91"),
            row(280.0, "3,91"),
            row(270.0, "4,91"),
        ];
        let bands = detect_column_bands(&lines);
        assert!(
            bands.iter().all(|b| !matches!(b, ColumnBand::Columns { .. })),
            "a label/value grid must not be transposed into columns: {:?} bands",
            bands.len()
        );
    }

    /// A narrow page gutter (10pt — below the `1.2em` a standalone row split
    /// needs) between a prose column (right edge 170) and a facing grid whose
    /// own internal gutter is wider (30pt). No row can seed the running-gutter
    /// pass, so only the vertical-projection fallback keeps the streams apart.
    fn narrow_gutter_prose_beside_grid() -> Vec<Vec<Span>> {
        let prose = |y: f64| {
            vec![
                sp("lorem", 50.0, y),
                sp("ipsum", 74.0, y),
                sp("dolor", 98.0, y),
                sp("sitam", 122.0, y),
                sp("amet", 146.0, y),
            ]
        };
        let grid = |y: f64, label: &str, v1: &str, v2: &str| {
            vec![sp(label, 180.0, y), sp(v1, 258.0, y), sp(v2, 292.0, y)]
        };
        vec![
            // Full-width caption above the block, straddling the gutter.
            vec![sp("abcdefghijklmnopqrst", 100.0, 320.0)],
            prose(300.0),
            prose(290.0),
            grid(295.0, "Alphaone", "1047", "7.2"),
            prose(280.0),
            grid(285.0, "Betaxtwo", "1031", "6.8"),
            prose(270.0),
            grid(275.0, "Gammathr", "1012", "6.6"),
            prose(260.0),
            grid(265.0, "Deltarfou", "1041", "6.5"),
            prose(250.0),
            // Full-width paragraph below the block, straddling the gutter.
            vec![sp("abcdefghijklmnopqrst", 100.0, 200.0)],
        ]
    }

    #[test]
    fn narrow_gutter_prose_beside_grid_is_recovered_by_projection() {
        let lines = narrow_gutter_prose_beside_grid();
        let bands = detect_column_bands(&lines);
        assert_eq!(bands.len(), 3, "leading Full, Columns, trailing Full");
        match &bands[1] {
            ColumnBand::Columns { left, right } => {
                let lt: Vec<String> = left.iter().map(|l| render_line_text(l)).collect();
                let rt: Vec<String> = right.iter().map(|l| render_line_text(l)).collect();
                assert_eq!(lt.len(), 6, "all six prose rows stay left: {lt:?}");
                assert_eq!(rt.len(), 4, "all four grid rows stay right: {rt:?}");
                assert!(
                    lt.iter().all(|t| t.contains("lorem") && !t.contains("Alpha")),
                    "no grid cell may be woven into the prose: {lt:?}"
                );
                assert!(
                    rt.iter().all(|t| !t.contains("lorem") && !t.contains("ipsum")),
                    "no prose may be woven into the grid: {rt:?}"
                );
            }
            ColumnBand::Full(_) => panic!("narrow-gutter prose/grid block was merged"),
        }
        match &bands[0] {
            ColumnBand::Full(rows) => {
                assert!(render_line_text(&rows[0]).contains("abcdefghij"))
            }
            ColumnBand::Columns { .. } => panic!("full-width caption must not be a column"),
        }
    }

    #[test]
    fn single_column_prose_is_not_split_by_projection_fallback() {
        // Every row crosses the middle of the text column, so no vertical
        // corridor exists and the fallback must not invent a column band.
        let row = |y: f64, words: &[&str]| {
            words
                .iter()
                .enumerate()
                .map(|(i, w)| sp(w, 50.0 + i as f64 * 26.0, y))
                .collect()
        };
        let lines = vec![
            row(300.0, &["alpha", "beta", "gamma", "delta", "epsilon"]),
            row(290.0, &["alpha", "beta", "gamma", "delta", "epsilon"]),
            row(280.0, &["alpha", "beta", "gamma", "delta", "epsilon"]),
            row(270.0, &["alpha", "beta", "gamma", "delta", "epsilon"]),
            row(260.0, &["alpha", "beta", "gamma", "delta", "epsilon"]),
            row(250.0, &["alpha", "beta", "gamma", "delta", "epsilon"]),
        ];
        let bands = detect_column_bands(&lines);
        assert!(
            bands.iter().all(|b| !matches!(b, ColumnBand::Columns { .. })),
            "single-column prose must not be split into columns: {:?} bands",
            bands.len()
        );
    }

    /// Regression for `mustang_attributeBasedXMP_EN16931.pdf`: a single-column
    /// invoice page with a right-aligned amount column made the vertical-
    /// projection fallback read the page as two columns, so every amount was
    /// emitted after the whole left stream and the `blocks` channel moved a
    /// row's total (`275,00`) and a table header's last cell (`Teilzahlung`) to
    /// the end of the page. The projection's "left half" is not one prose
    /// column but a grid of table cells (label + description + quantity…), so
    /// the reading-order gate must reject it; the low-level projection detector
    /// itself stays ungated.
    #[test]
    fn cell_grid_half_is_not_a_page_column() {
        // One physical row: a label at x=50, a middle cell at x=150 (an
        // internal grid gutter), and an amount pinned to right edge 400 whose
        // left edge moves with its own length.
        let row = |y: f64, a: &str, b: &str, amount: &str| {
            vec![
                sp(a, 50.0, y),
                sp(b, 150.0, y),
                sp(amount, 400.0 - amount.len() as f64 * 6.0, y),
            ]
        };
        let lines = vec![
            row(300.0, "Positionssumme", "Basis", "473,00"),
            row(290.0, "Gesamtbetrag", "zu", "0,00"),
            row(280.0, "Abschlag", "netto", "-0,00"),
            row(270.0, "Rechnungssumme", "ohne", "473,00"),
            row(260.0, "Zahlbetrag", "frei", "529,87"),
        ];
        assert!(
            detect_projection_two_columns(&lines).is_some(),
            "the projection corridor is real; the missing reading-order gate is the bug"
        );
        let pc = page_two_columns(&lines).expect("projection fallback still detects it");
        assert!(
            !columns_are_viable_prose(&pc),
            "a half that is a grid of cells must not read as one prose column"
        );
    }

    /// The shape gate must not reject a genuine two-column page whose lines are
    /// each a single `Tj` run (one span per visual line) — the case the
    /// projection fallback exists for, and the shape the structural benchmark's
    /// `twocol_*` documents use.
    #[test]
    fn single_span_prose_columns_are_still_detected() {
        let row = |y: f64, l: &str, r: &str| vec![sp(l, 50.0, y), sp(r, 350.0, y)];
        let lines = vec![
            row(300.0, "Left line A.", "Right line A."),
            row(290.0, "Left line B.", "Right line B."),
            row(280.0, "Left line C.", "Right line C."),
            row(270.0, "Left line D.", "Right line D."),
        ];
        let pc = page_two_columns(&lines)
            .expect("a genuine two-column page must still be detected");
        assert_eq!(pc.left.len(), 4, "{:?}", pc.left);
        assert_eq!(pc.right.len(), 4, "{:?}", pc.right);
        assert!(
            columns_are_viable_prose(&pc),
            "a genuine two-column page must survive the reading-order gate"
        );
    }

    /// A visual line the glyph/line builder produced by fusing a left list item
    /// with a right callout-box heading that sits on a *different* baseline.
    /// This mirrors the target's real geometry (the marker line and the box
    /// line are 1.44pt/2.28pt apart, close enough for `build_lines`'s half-em
    /// tolerance to fuse them into one `lines` entry).
    fn fused_list_row(
        left_y: f64,
        marker: &str,
        left_words: [&str; 3],
        right_y: f64,
        right_words: [&str; 3],
    ) -> Vec<Span> {
        let mut v = vec![sp(marker, 50.0, left_y)];
        let mut x = 70.0;
        for w in left_words {
            v.push(sp(w, x, left_y));
            x += w.len() as f64 * 6.0 + 8.0;
        }
        let mut xr = 250.0;
        for w in right_words {
            v.push(sp(w, xr, right_y));
            xr += w.len() as f64 * 6.0 + 6.0;
        }
        v
    }

    /// Regression for the target `SeConnecterAMonComptePartenaire.pdf`. The
    /// numbered list's items 4 and 5 are each fused onto one visual line with
    /// the adjacent callout-box heading "Accès à l'Espace / bailleur", whose two
    /// lines sit a couple of points off the list baseline. The reading-order
    /// pass split the fused rows at the gutter but then *rejected* the block
    /// (only two crossing rows) and restored each fused row whole, so the single
    /// linear stream wove item 4 → "Accès" → item 5 → "bailleur" together. With
    /// the fix the list stays contiguous and the box heading is emitted as its
    /// own block after item 5.
    #[test]
    fn fused_list_item_and_callout_heading_keep_the_list_contiguous() {
        let lines = vec![
            vec![sp("1. Renseignez vos identifiants", 50.0, 300.0)],
            vec![sp("2. Renseignez les informations", 50.0, 290.0)],
            vec![sp("3. Personnalisez votre mot de passe", 50.0, 280.0)],
            fused_list_row(270.0, "4.", ["Acceptez", "les", "règles"], 268.0, ["Accès", "à", "l’Espace"]),
            fused_list_row(260.0, "5.", ["Acceptez", "les", "conditions"], 258.0, ["bailleur", "du", "compte"]),
            vec![sp(" ", 50.0, 250.0)],
            vec![sp("Vous êtes un bailleur physique et vous gérez", 50.0, 240.0)],
        ];
        let bands = detect_column_bands(&lines);
        assert_eq!(bands.len(), 3, "Full list preamble, Columns block, Full heading");
        match &bands[1] {
            ColumnBand::Columns { left, right } => {
                let lt: Vec<String> = left.iter().map(|l| render_line_text(l)).collect();
                let rt: Vec<String> = right.iter().map(|l| render_line_text(l)).collect();
                assert!(lt[0].contains("4."), "list item 4 stays left: {lt:?}");
                assert!(lt[1].contains("5."), "list item 5 stays left: {lt:?}");
                assert!(
                    lt.iter().all(|t| !t.contains("Accès") && !t.contains("bailleur")),
                    "no box heading may be woven into the list: {lt:?}"
                );
                assert_eq!(rt.len(), 2, "the two box lines stay together right: {rt:?}");
                assert!(rt[0].contains("Accès") && rt[1].contains("bailleur"), "{rt:?}");
            }
            ColumnBand::Full(_) => panic!("the fused list/box block was merged instead of split"),
        }
        // And the flattened page order must read 1..5 before the box heading.
        let streams = page_read_order(&lines);
        let seq: Vec<String> = streams.iter().flatten().map(|l| render_line_text(l)).collect();
        let pos = |needle: &str| {
            seq.iter()
                .position(|t| t.contains(needle))
                .unwrap_or_else(|| panic!("missing {needle:?} in {seq:?}"))
        };
        assert!(
            pos("1.") < pos("2.") && pos("2.") < pos("3.") && pos("3.") < pos("4.") && pos("4.") < pos("5."),
            "the numbered list must stay contiguous: {seq:?}"
        );
        assert!(
            pos("5.") < pos("Accès") && pos("Accès") < pos("bailleur"),
            "the box heading must follow the list as one block: {seq:?}"
        );
    }

    /// Counter-case for the fix: a form/table row is one physical line, so its
    /// label and right-aligned value share a baseline *exactly* (measured
    /// 0.00pt on the `mustang_zugferd_2p1_EXTENDED` and `mistral_7b`
    /// fixtures). Two adjacent such rows must keep their row-wise order, not be
    /// transposed into "all labels, then all values".
    fn same_baseline_pair(y: f64, label: &str, value: [&str; 3]) -> Vec<Span> {
        let mut v = vec![
            sp(label, 50.0, y),
            sp("Referenz", 130.0, y),
            sp("Nr", 180.0, y),
        ];
        let mut x = 250.0;
        for w in value {
            v.push(sp(w, x, y));
            x += w.len() as f64 * 6.0 + 6.0;
        }
        v
    }

    #[test]
    fn same_baseline_label_value_rows_are_not_transposed_into_columns() {
        let lines = vec![
            same_baseline_pair(300.0, "Bestellung", [":", "B123456789", "vom"]),
            same_baseline_pair(290.0, "Weitere", [":", "A456123", "Art"]),
            vec![sp("Ende", 50.0, 280.0)],
        ];
        let bands = detect_column_bands(&lines);
        assert!(
            bands.iter().all(|b| !matches!(b, ColumnBand::Columns { .. })),
            "same-baseline label/value rows must keep their row order: {} bands",
            bands.len()
        );
    }

    /// Counter-case for the fix: when one-sided rows separate the two crossing
    /// rows (the `text_style_complex` single-column training form), the wide
    /// gaps merely happen to align. The corridor must not be trusted even
    /// though each crossing row is itself a fused pair on different baselines.
    #[test]
    fn non_adjacent_crossings_are_not_a_column_band() {
        let lines = vec![
            fused_list_row(300.0, "1.", ["A", "B", "C"], 298.0, ["D", "E", "F"]),
            vec![sp("intervening line one", 50.0, 290.0)],
            vec![sp("intervening line two", 50.0, 280.0)],
            fused_list_row(270.0, "2.", ["G", "H", "I"], 268.0, ["J", "K", "L"]),
            vec![sp("Ende", 50.0, 260.0)],
        ];
        let bands = detect_column_bands(&lines);
        assert!(
            bands.iter().all(|b| !matches!(b, ColumnBand::Columns { .. })),
            "crossings separated by one-sided rows must not form a column band: {} bands",
            bands.len()
        );
    }

    // -- multi_column_projection (3+ narrow columns) --------------------------

    fn mc_span(text: &str, x: f64, y: f64) -> Span {
        Span {
            text: text.to_string(),
            x,
            y,
            size: 10.0,
            advance: text.len() as f64 * 6.0,
            word_advance: text.len() as f64 * 6.0,
            is_bold: false,
            is_italic: false,
            is_underline: false,
            is_vertical: false,
        }
    }

    /// Lay `words` left-to-right from `x` with 3pt inter-word gaps, returning
    /// the spans and the x just past the last word.
    fn mc_fragment(words: &[&str], mut x: f64, y: f64) -> (Vec<Span>, f64) {
        let mut v = Vec::new();
        for w in words {
            v.push(mc_span(w, x, y));
            x += w.len() as f64 * 6.0 + 3.0;
        }
        (v, x)
    }

    /// One fused visual row of a 3-column page: three prose fragments sharing a
    /// baseline, separated by ~7pt gutters (well below the 2.5em hard break and
    /// the 1.2em `split_row_columns` seed). Every fragment starts lowercase and
    /// ends without a terminator, so each column reads as wrapped prose.
    fn mc_three_col_row(y: f64) -> Vec<Span> {
        let (mut l, x1) = mc_fragment(&["mot", "deux", "trois"], 50.0, y);
        let (mut m, x2) = mc_fragment(&["autre", "texte", "ici"], x1 + 7.0, y);
        let (mut r, _) = mc_fragment(&["encore", "des", "mots"], x2 + 7.0, y);
        l.append(&mut m);
        l.append(&mut r);
        l
    }

    #[test]
    fn three_narrow_columns_are_recovered_not_woven() {
        // A full-width header whose single span covers both gutters must stay
        // its own stream and must not be absorbed into the column region.
        let mut lines = vec![vec![mc_span("HHHHHHHHHHHHHHHHHHHHHHHHHHHHHHHH", 50.0, 800.0)]];
        for i in 0..10 {
            lines.push(mc_three_col_row(780.0 - i as f64 * 12.0));
        }
        // The projection finds the two gutters directly.
        let region = multi_column_projection(&lines).expect("3 columns recovered");
        assert_eq!(region.columns.len(), 3, "must recover three columns");
        // Each column is a single column's text, in reading order.
        let first: String = region.columns[0]
            .iter()
            .flat_map(|l| l.iter().map(|s| s.text.as_str()))
            .collect();
        assert!(first.starts_with("mot"), "left column leads: {first}");
        assert!(!first.contains("autre"), "no middle column text leaks in: {first}");
        let middle: String = region.columns[1]
            .iter()
            .flat_map(|l| l.iter().map(|s| s.text.as_str()))
            .collect();
        assert!(middle.starts_with("autre"), "middle column follows: {middle}");
        // The region excludes the full-width header.
        assert_eq!(region.start, 1, "header must not join the column region");
    }

    /// A single-column page of ordinary prose must not be carved into columns
    /// by the projection: there is no recurring 0.4em+ gutter to find.
    #[test]
    fn single_column_prose_is_not_split_by_projection() {
        let mut lines = Vec::new();
        for i in 0..12 {
            let (v, _) = mc_fragment(&["une", "ligne", "de", "prose", "ordinaire"], 50.0, 780.0 - i as f64 * 12.0);
            lines.push(v);
        }
        assert!(
            multi_column_projection(&lines).is_none(),
            "single-column prose must not project into columns"
        );
        assert_eq!(page_read_order(&lines).len(), 1);
    }
}
