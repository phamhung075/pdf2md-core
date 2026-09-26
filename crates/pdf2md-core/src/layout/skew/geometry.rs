// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Super-lightweight Hough/Radon skew estimation and deskewing.
//!
//! Both layout stages that assume axis-aligned geometry are hurt by a page
//! tilted even a degree or two (a skewed scan, a rotated label, an A4 page
//! photographed at a slight tilt):
//!
//! * [`recursive_xy_cut`](super::xy_cut::recursive_xy_cut) partitions by
//!   axis-aligned projection profiles — tilted line boxes overlap vertically,
//!   the valleys vanish and the page collapses into one giant block.
//! * [`build_lines`](super::glyph_stream::build_lines) clusters glyph spans by
//!   a tight baseline-`y` tolerance — tilted baselines fragment rows and smear
//!   column gutters.
//!
//! This module estimates that tilt and rotates the geometry back onto the page
//! axes *before* those stages. Both halves are intentionally cheap and operate
//! on sparse points (no rasterisation):
//!
//! 1. **Estimation** takes a sparse point cloud of glyph baseline origins.
//!      * For TextLines (already grouped), [`estimate_skew_angle_deg`] runs a
//!        Hough line-angle vote: the direction of each pair of consecutive
//!        glyphs on a line points at the baseline tilt, so each segment votes
//!        for its angle weighted by length; the dominant bin is the tilt. On an
//!        axis-aligned page every vote is exactly 0°, so it returns `0.0`.
//!      * For an ungrouped span cloud, [`estimate_skew_angle_deg_from_spans`]
//!        runs a Radon projection scan: it projects the anchors over many
//!        candidate baseline directions and takes the near-maximal plateau's
//!        mid-point, i.e. the angle whose perpendicular projection is sharpest
//!        (rows collapse into single histogram peaks).
//! 2. **Deskewing** rotates every anchor (and every bbox corner) by the
//!    inverse of the estimated angle about the page-text centroid. For spans,
//!    [`deskew_spans`] rotates only the baseline origin and keeps the run
//!    width/glyph height intact, so downstream clustering sees axis-aligned
//!    rows. The extracted `text` and style flags are never touched.
//!
//! On an axis-aligned page the estimate is sub-threshold, the caller skips the
//! (rare) clone/rebuild, and no coordinate is moved.

use super::*;

pub(super) fn centroid_of_span_origins(spans: &[Span]) -> (f64, f64) {
    let (mut sx, mut sy, mut n) = (0.0, 0.0, 0usize);
    for s in spans {
        sx += s.x;
        sy += s.y;
        n += 1;
    }
    if n == 0 {
        return (0.0, 0.0);
    }
    let inv = 1.0 / n as f64;
    (sx * inv, sy * inv)
}

// ---------------------------------------------------------------------------
// Deskew internals
// ---------------------------------------------------------------------------

pub(super) fn centroid_of_origins(lines: &[TextLine]) -> (f64, f64) {
    let (mut sx, mut sy, mut n) = (0.0, 0.0, 0usize);
    for l in lines {
        for c in &l.chars {
            sx += c.origin.0;
            sy += c.origin.1;
            n += 1;
        }
    }
    if n == 0 {
        return (0.0, 0.0);
    }
    let inv = 1.0 / n as f64;
    (sx * inv, sy * inv)
}

pub(super) fn deskew_line(line: &TextLine, rot: &impl Fn(f64, f64) -> (f64, f64)) -> TextLine {
    // Degenerate (empty) lines pass through untouched so we never fabricate an
    // INFINITY bound.
    if line.chars.is_empty() {
        return line.clone();
    }
    // Rebuild the char list with rotated origins & bounding boxes.
    let mut chars: Vec<CharInfo> = Vec::with_capacity(line.chars.len());
    let mut baseline_sum = 0.0;
    let mut min_x = f64::INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut max_y = f64::NEG_INFINITY;

    for c in &line.chars {
        let (ox, oy) = rot(c.origin.0, c.origin.1);
        let bbox = rotate_rect(&c.bbox, rot);
        baseline_sum += oy;
        min_x = min_x.min(bbox.min_x);
        min_y = min_y.min(bbox.min_y);
        max_x = max_x.max(bbox.max_x);
        max_y = max_y.max(bbox.max_y);
        let mut nc = c.clone();
        nc.origin = (ox, oy);
        nc.bbox = bbox;
        chars.push(nc);
    }

    // Rebuild words: rotate each word's own glyph clone and re-bound.
    let words: Vec<TextWord> = line
        .words
        .iter()
        .map(|w| {
            let mut wmin_x = f64::INFINITY;
            let mut wmin_y = f64::INFINITY;
            let mut wmax_x = f64::NEG_INFINITY;
            let mut wmax_y = f64::NEG_INFINITY;
            let wchars: Vec<CharInfo> = w
                .chars
                .iter()
                .map(|c| {
                    let (ox, oy) = rot(c.origin.0, c.origin.1);
                    let bbox = rotate_rect(&c.bbox, rot);
                    wmin_x = wmin_x.min(bbox.min_x);
                    wmin_y = wmin_y.min(bbox.min_y);
                    wmax_x = wmax_x.max(bbox.max_x);
                    wmax_y = wmax_y.max(bbox.max_y);
                    let mut nc = c.clone();
                    nc.origin = (ox, oy);
                    nc.bbox = bbox;
                    nc
                })
                .collect();
            let word_bbox = Rect {
                min_x: wmin_x,
                min_y: wmin_y,
                max_x: wmax_x,
                max_y: wmax_y,
            };
            TextWord {
                chars: wchars,
                word_bbox,
                text: w.text.clone(),
            }
        })
        .collect();

    let line_bbox = Rect {
        min_x,
        min_y,
        max_x,
        max_y,
    };
    let baseline = if chars.is_empty() {
        0.0
    } else {
        baseline_sum / chars.len() as f64
    };

    TextLine {
        chars,
        words,
        baseline,
        line_bbox,
        text: line.text.clone(),
    }
}

/// Rotates the four corners of an axis-aligned rectangle by `rot` and returns
/// the tightest enclosing axis-aligned rectangle. For the small angles this
/// module handles, the enclosure is only marginally larger than the true
/// rotated bounds — close enough for valley detection.
pub(super) fn rotate_rect(rect: &Rect, rot: &impl Fn(f64, f64) -> (f64, f64)) -> Rect {
    let (a, b) = rot(rect.min_x, rect.min_y);
    let (c, d) = rot(rect.max_x, rect.min_y);
    let (e, f) = rot(rect.max_x, rect.max_y);
    let (g, h) = rot(rect.min_x, rect.max_y);
    Rect {
        min_x: a.min(c).min(e).min(g),
        min_y: b.min(d).min(f).min(h),
        max_x: a.max(c).max(e).max(g),
        max_y: b.max(d).max(f).max(h),
    }
}
