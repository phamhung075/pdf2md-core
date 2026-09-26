//! pdf2md-core — High-performance native Rust core engine for sub-millisecond
//! PDF-to-Markdown extraction and 2D spatial canvas table reconstruction.
//!
//! Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
//! SPDX-License-Identifier: BSL-1.1
//! Licensed under the Business Source License 1.1 (BSL-1.1).

use super::*;

/// 2D spatial canvas clustering: groups text spans into aligned rows and columns.
pub fn reconstruct_canvas_tables(spans: &[TextSpan]) -> Vec<CanvasTable> {
    if spans.len() < 4 {
        return Vec::new();
    }

    // Sort spans top-to-bottom, left-to-right (Y inverted in standard PDF: larger Y is higher)
    let mut sorted_spans = spans.to_vec();
    sorted_spans.sort_by(|a, b| {
        b.bbox
            .y0
            .partial_cmp(&a.bbox.y0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                a.bbox
                    .x0
                    .partial_cmp(&b.bbox.x0)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    });

    // Group spans into candidate rows by vertical alignment threshold (3.0 points)
    let y_tolerance = 3.0;
    let mut rows: Vec<Vec<TextSpan>> = Vec::new();
    let mut current_row: Vec<TextSpan> = Vec::new();
    let mut current_y = sorted_spans[0].bbox.y0;

    for span in sorted_spans {
        if (span.bbox.y0 - current_y).abs() <= y_tolerance {
            current_row.push(span);
        } else {
            if !current_row.is_empty() {
                rows.push(current_row);
            }
            current_y = span.bbox.y0;
            current_row = vec![span];
        }
    }
    if !current_row.is_empty() {
        rows.push(current_row);
    }

    // Filter candidate tables: at least 2 rows and multiple columns
    let multi_col_rows: Vec<&Vec<TextSpan>> = rows.iter().filter(|r| r.len() >= 2).collect();
    if multi_col_rows.len() < 2 {
        return Vec::new();
    }

    // Extract table rows as string matrices
    let mut table_rows: Vec<Vec<String>> = Vec::new();
    for row in multi_col_rows {
        let mut cols: Vec<String> = Vec::new();
        for span in row {
            cols.push(span.text.clone());
        }
        table_rows.push(cols);
    }

    let min_x = spans
        .iter()
        .map(|s| s.bbox.x0)
        .fold(f64::INFINITY, f64::min);
    let max_x = spans
        .iter()
        .map(|s| s.bbox.x1)
        .fold(f64::NEG_INFINITY, f64::max);
    let min_y = spans
        .iter()
        .map(|s| s.bbox.y0)
        .fold(f64::INFINITY, f64::min);
    let max_y = spans
        .iter()
        .map(|s| s.bbox.y1)
        .fold(f64::NEG_INFINITY, f64::max);

    vec![CanvasTable::new(
        table_rows,
        BoundingBox::new(min_x, min_y, max_x, max_y),
    )]
}

/// Best-effort device page box ([x0, y0, x1, y1]) from the page /MediaBox,
/// used to classify full-page background images.
pub(super) fn page_media_box(doc: &lopdf::Document, page_id: lopdf::ObjectId) -> Option<(f64, f64, f64, f64)> {
    let dict = doc.get_dictionary(page_id).ok()?;
    let rotate = dict
        .get(b"Rotate")
        .ok()
        .and_then(|o| o.as_i64().ok())
        .unwrap_or(0);
    let rotate = ((rotate % 360) + 360) % 360;
    let mb = dict.get(b"MediaBox").ok()?.as_array().ok()?;
    let g = |i: usize| -> Option<f64> {
        mb.get(i)
            .and_then(|o| o.as_float().ok().map(|f| f as f64))
            .or_else(|| mb.get(i).and_then(|o| o.as_i64().ok().map(|v| v as f64)))
    };
    let w = (g(2)? - g(0)?).abs();
    let h = (g(3)? - g(1)?).abs();
    if rotate == 90 || rotate == 270 {
        Some((0.0, 0.0, h, w))
    } else {
        Some((0.0, 0.0, w, h))
    }
}

/// Minimum on-page width fraction for an embedded image. A placement that is
/// genuinely tiny on the page (a barcode, a signature, an inline stamp) still
/// gets this floor so it is not rendered as a sub-pixel sliver.
pub(super) const MIN_EMBED_WIDTH_FRAC: f64 = 0.05;

/// The on-page width of a media placement as a fraction of the page's width,
/// clamped to `[MIN_EMBED_WIDTH_FRAC, 1.0]`. This reproduces how big the image
/// actually was on the page, so a high-pixel-density placement that occupies
/// only part of the page is no longer blown up to its full pixel width by the
/// markdown reader. Returns `None` when the page has no measurable MediaBox (or
/// when the placement has no on-page width), letting the caller fall back to
/// the natural size.
pub(super) fn embed_width_frac(
    m: &crate::media::MediaItem,
    page_bbox: Option<(f64, f64, f64, f64)>,
) -> Option<f64> {
    let (px0, _, px1, _) = page_bbox?;
    let pw = (px1 - px0).abs();
    if pw <= 1.0 {
        return None;
    }
    let w = (m.x1 - m.x0).abs();
    if w <= f64::EPSILON {
        return None;
    }
    Some((w / pw).clamp(MIN_EMBED_WIDTH_FRAC, 1.0))
}

/// Whether a block adjacent to a content image is that image's caption.
///
/// A short line close to the image is the classic caption. Academic papers,
/// however, use descriptive multi-line captions that easily exceed nine words,
/// so a `Figure`/`Fig.` prefixed paragraph within a looser gap is also a
/// caption (`**Figure 1: ...** The number of operations ...`).
pub(super) fn is_caption_block(text: &str, gap: f64, size_hint: f64) -> bool {
    let caption_prefix = text.starts_with("Figure ")
        || text.starts_with("**Figure ")
        || text.starts_with("Fig. ")
        || text.starts_with("**Fig. ");
    (gap <= 2.5 * size_hint && text.split_whitespace().count() <= 9)
        || (caption_prefix && gap <= 4.0 * size_hint)
}
