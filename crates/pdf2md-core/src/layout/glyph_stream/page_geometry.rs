// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! PDF content-stream glyph extraction, font metric resolution, and 2D span aggregation.

use super::*;

/// Height of the page media box in device points (best effort), accounting for page rotation.
pub fn page_height_of(doc: &Document, page_id: ObjectId) -> Option<f64> {
    let (_, h) = page_initial_transform(doc, page_id);
    Some(h)
}

/// Resolve an inheritable page attribute (`/Rotate`, `/MediaBox`, `/CropBox`)
/// by climbing the `/Pages` parent chain, cycle- and depth-bounded. PDF viewers
/// inherit these from ancestors, but the glyph engine previously read only the
/// page dictionary; a page that inherits its `/Rotate` or box from `/Pages`
/// therefore got the wrong orientation/geometry.
pub(super) fn inherited_page_object<'a>(
    doc: &'a Document,
    page_id: ObjectId,
    key: &[u8],
) -> Option<&'a Object> {
    let mut seen: Vec<ObjectId> = Vec::new();
    let mut cur = Some(page_id);
    while let Some(id) = cur {
        if seen.len() >= 32 || seen.contains(&id) {
            break;
        }
        seen.push(id);
        let Ok(dict) = doc.get_dictionary(id) else { break };
        if let Ok(o) = dict.get(key) {
            return deref(doc, o);
        }
        cur = dict.get(b"Parent").ok().and_then(|o| o.as_reference().ok());
    }
    None
}

/// Initial coordinate transformation matrix and display height for a page,
/// taking into account the page /Rotate attribute (0, 90, 180, 270 degrees clockwise)
/// and /MediaBox / /CropBox boundaries. All three are inherited from the
/// `/Pages` ancestors when absent on the page itself.
pub(crate) fn page_initial_transform(doc: &Document, page_id: ObjectId) -> (Mtx, f64) {
    let rotate = inherited_page_object(doc, page_id, b"Rotate")
        .and_then(|o| o.as_i64().ok())
        .unwrap_or(0);
    let rotate = ((rotate % 360) + 360) % 360;

    let box_obj = inherited_page_object(doc, page_id, b"CropBox")
        .or_else(|| inherited_page_object(doc, page_id, b"MediaBox"))
        .and_then(|o| o.as_array().ok());

    let (x0, y0, x1, y1) = if let Some(b) = box_obj {
        let g = |i: usize| -> f64 {
            b.get(i)
                .and_then(|o| deref(doc, o))
                .and_then(|o| o.as_float().ok().map(|f| f as f64).or_else(|| o.as_i64().ok().map(|v| v as f64)))
                .unwrap_or(0.0)
        };
        (g(0), g(1), g(2), g(3))
    } else {
        (0.0, 0.0, 595.0, 842.0)
    };

    let w = (x1 - x0).abs();
    let h = (y1 - y0).abs();

    match rotate {
        90 => (Mtx::from_parts(0.0, -1.0, 1.0, 0.0, -y0, x1), w),
        180 => (Mtx::from_parts(-1.0, 0.0, 0.0, -1.0, x1, y1), h),
        270 => (Mtx::from_parts(0.0, 1.0, -1.0, 0.0, y1, -x0), w),
        _ => (Mtx::ID, h),
    }
}

/// Group consecutive spans into runs by device baseline y, preserving
/// stream order, then merge adjacent same-visual-line runs and sort lines
/// top-to-bottom. This is the line model used both for the plain text renderer
/// and for table detection.
pub fn build_lines(spans: &[Span]) -> Vec<Vec<Span>> {
    if spans.is_empty() {
        return Vec::new();
    }

    // Group consecutive spans into runs by device baseline y, preserving
    // stream order.
    let mut runs: Vec<Vec<Span>> = Vec::new();
    for span in spans {
        let eps = 0.5 * span.size.max(0.1);
        match runs.last_mut() {
            Some(last) if (span.y - last.last().unwrap().y).abs() <= eps => last.push(span.clone()),
            _ => runs.push(vec![span.clone()]),
        }
    }

    // Sort all runs top-to-bottom by device baseline y.
    runs.sort_by(|a, b| {
        b[0].y
            .partial_cmp(&a[0].y)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    // Merge runs that sit on the same visual baseline y (after sorting by y,
    // runs from different columns on the same row are now adjacent).
    let mut lines: Vec<Vec<Span>> = Vec::new();
    for run in runs {
        let eps = 0.5 * run[0].size.max(0.1);
        match lines.last_mut() {
            Some(last) if (run[0].y - last[0].y).abs() <= eps => {
                last.extend(run);
            }
            _ => lines.push(run),
        }
    }

    // Within each visual line, sort spans left-to-right by x, using stable sort
    // to preserve stream order for glyphs sharing the same x position.
    for line in &mut lines {
        line.sort_by(|a, b| {
            a.x.partial_cmp(&b.x).unwrap_or(std::cmp::Ordering::Equal)
        });

        // Deduplicate overstrike / shadow spans (faux-bolding):
        // In many PDFs (invoices, pay slips, forms), bold text is created by drawing identical
        // glyphs twice at the same or micro-shifted coordinates (dx <= 0.75 pt or 0.2 * size).
        let mut deduped: Vec<Span> = Vec::with_capacity(line.len());
        for span in line.drain(..) {
            let is_duplicate = if let Some(prev) = deduped.last_mut() {
                if prev.text == span.text {
                    let dx = (span.x - prev.x).abs();
                    let dy = (span.y - prev.y).abs();
                    let max_dx = (0.2 * span.size).max(0.75);
                    if dx <= max_dx && dy <= 0.75 {
                        prev.is_bold = true;
                        true
                    } else {
                        false
                    }
                } else {
                    false
                }
            } else {
                false
            };
            if !is_duplicate {
                deduped.push(span);
            }
        }
        *line = deduped;
    }

    lines
}
