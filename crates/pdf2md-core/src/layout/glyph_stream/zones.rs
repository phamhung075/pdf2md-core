// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! PDF content-stream glyph extraction, font metric resolution, and 2D span aggregation.

use super::*;

/// Append one `table` zone to `blocks` per recovered canvas grid, and drop the
/// reading-order fragments (body/list/caption) that fall entirely inside a
/// recovered table bbox so the zone view shows the table as a single region
/// rather than scattered cells. The table zone carries a compact summary in its
/// `text` field (row × column count and a leading non-empty cell).
pub(super) fn append_table_zones(blocks: &mut Vec<crate::layout::reading_order::DocBlock>, hits: &[crate::layout::tables::TableHit]) {
    use crate::layout::reading_order::DocBlock;
    let original_len = blocks.len();
    let mut contained = vec![false; original_len];

    for hit in hits {
        let rows = hit.rows.len();
        let cols = hit.rows.iter().map(|r| r.len()).max().unwrap_or(0);
        // Only a genuine grid (>= 2 columns and >= 2 rows) rendered as a pipe
        // table. A single-column / single-row Hit is a layout wrapper whose
        // cells are ordinary flowing text; suppressing the blocks it contains
        // would delete real paragraphs and emit a fake `table` zone (F7/R4).
        if cols < 2 || rows < 2 {
            continue;
        }
        let tx0 = hit.bbox.x0.min(hit.bbox.x1);
        let tx1 = hit.bbox.x0.max(hit.bbox.x1);
        let ty0 = hit.bbox.y0.min(hit.bbox.y1);
        let ty1 = hit.bbox.y0.max(hit.bbox.y1);
        let label = hit
            .rows
            .first()
            .and_then(|r| r.iter().find(|c| !c.trim().is_empty()))
            .cloned()
            .unwrap_or_default();
        let text = format!("table · {} rows × {} cols · {}", rows, cols, label);
        blocks.push(DocBlock {
            page: 0,
            kind: "table".to_string(),
            x0: tx0,
            y0: ty0,
            x1: tx1,
            y1: ty1,
            text,
            is_bold: false,
            is_italic: false,
            is_underline: false,
        });
        for (i, b) in blocks.iter().take(original_len).enumerate() {
            if contained[i] {
                continue;
            }
            // `hit.bbox` is built from span *baselines* (`min_y`/`max_y` of
            // `sp.y` in `find_tables`), while `build_doc_blocks` pads every
            // block by half its font size above/below its baseline band. A
            // strict bbox containment test therefore never matches a fragment
            // sitting on the table's first or last row — it always overhangs
            // the bbox by ~0.5em — so those cell fragments (and any
            // cross-column paragraph merge inside the grid) leak into the zone
            // list beside the table zone instead of being collapsed into it.
            // Compare the fragment's centre, which is padding-independent,
            // against the baseline bbox; the small epsilon absorbs the
            // half-em overhang on a single-row fragment whose centre lands
            // exactly on the bbox edge.
            const EDGE_EPS: f64 = 0.5;
            let cx = 0.5 * (b.x0 + b.x1);
            let cy = 0.5 * (b.y0 + b.y1);
            let inside = cx >= tx0 - EDGE_EPS
                && cx <= tx1 + EDGE_EPS
                && cy >= ty0 - EDGE_EPS
                && cy <= ty1 + EDGE_EPS;
            if inside && matches!(b.kind.as_str(), "body" | "list" | "caption") {
                contained[i] = true;
            }
        }
    }

    if contained.iter().any(|c| *c) {
        let mut idx = 0;
        blocks.retain(|_| {
            let keep = if idx < original_len {
                !contained[idx]
            } else {
                true // newly pushed "table" zones are always kept
            };
            idx += 1;
            keep
        });
    }
}

// ---------------------------------------------------------------------------
// Underline (thin horizontal rule) detection
// ---------------------------------------------------------------------------

/// Record a thin horizontal rule between device points `p` and `q` as a
/// candidate underline segment `(y, x0, x1)`.
pub(super) fn record_horiz_seg(out: &mut Vec<(f64, f64, f64)>, p: (f64, f64), q: (f64, f64)) {
    let (x0, y0) = p;
    let (x1, y1) = q;
    if (y1 - y0).abs() < 0.6 {
        let w = (x1 - x0).abs();
        if w >= 1.5 {
            out.push((y0, x0.min(x1), x0.max(x1)));
        }
    }
}

/// Flush the current path as consecutive line segments into `out`, keeping only
/// thin horizontal rules. `close` wraps the last point back to the path start.
pub(super) fn flush_path_segs(
    path: &mut Vec<(f64, f64)>,
    start: Option<(f64, f64)>,
    out: &mut Vec<(f64, f64, f64)>,
    close: bool,
) {
    // A genuine underline is a *thin* horizontal rule. A taller path is a
    // rectangle border — e.g. the hyperlink annotation box a producer draws with
    // `m`/`l` around a link — whose top and bottom edges would otherwise each be
    // recorded as an "underline". On the FR "Statut EI" ACRE slide the URL link
    // box's top edge sat a couple of points below the *previous* text line's
    // baseline, so it underlined " au plus tard dans les 45 jours suiv" even
    // though only `l'URSSAF` and the URL are underlined on the page. Reject a
    // path whose overall vertical extent is not that of a rule, using the same
    // < 2pt threshold the thin-filled-`re` branch already applies.
    let mut ymin = f64::INFINITY;
    let mut ymax = f64::NEG_INFINITY;
    for &(_, y) in path.iter() {
        ymin = ymin.min(y);
        ymax = ymax.max(y);
    }
    if close {
        if let Some((_, y)) = start {
            ymin = ymin.min(y);
            ymax = ymax.max(y);
        }
    }
    if !path.is_empty() && ymax - ymin < 2.0 {
        for w in path.windows(2) {
            record_horiz_seg(out, w[0], w[1]);
        }
        if close {
            if let Some(s) = start {
                if let Some(&last) = path.last() {
                    record_horiz_seg(out, last, s);
                }
            }
        }
    }
    path.clear();
}

/// Mark the spans of each (non-table) visual line that sit directly above a
/// thin horizontal rule as underlined. A rule qualifies when its device `y` sits
/// a small distance below the span's baseline (PDF y-axis up) and it horizontally
/// overlaps the span.
pub(super) fn mark_underlines(
    lines: &mut [Vec<Span>],
    segs: &[(f64, f64, f64)],
    covered_lines: &std::collections::HashSet<usize>,
) {
    for (li, line) in lines.iter_mut().enumerate() {
        if covered_lines.contains(&li) {
            continue;
        }
        for span in line.iter_mut() {
            if span.is_underline || span.is_vertical {
                continue;
            }
            let size = span.size.max(0.1);
            let baseline = span.y;
            let sx0 = span.x;
            let sx1 = span.x + span.advance;
            let span_w = (sx1 - sx0).max(0.1);
            for &(sy, rx0, rx1) in segs {
                let d = baseline - sy; // positive when the rule is below the baseline
                if !(0.05 * size..=0.5 * size).contains(&d) {
                    continue;
                }
                let overlap = (sx1.min(rx1) - sx0.max(rx0)).max(0.0);
                // A rule that hugs the span horizontally, or a rule of about the
                // same width as the span that overlaps it.
                let wide_enough = overlap / span_w >= 0.5;
                let similar_width = (rx1 - rx0).abs() - span_w <= 0.4 * size;
                if wide_enough || (similar_width && overlap > 0.0) {
                    span.is_underline = true;
                    break;
                }
            }
        }
    }
}

/// Append extracted vertical margin text to the page body text.
///
/// Vertical runs are side furniture (arXiv side stamps, running headers): when
/// layout analysis / human reading order is enabled they must stay out of the
/// body prose, or the stamp is injected mid-paragraph. The caller still emits
/// the margin blocks for zone inspectors either way. Without layout analysis
/// the legacy append behavior is kept.
pub(super) fn append_vertical_text(text: &mut String, vertical_text: &str, detect_layout: bool) {
    if vertical_text.is_empty() || detect_layout {
        return;
    }
    if !text.is_empty() {
        if text.ends_with('\n') {
            text.push('\n');
        } else {
            text.push_str("\n\n");
        }
    }
    text.push_str(vertical_text);
}
