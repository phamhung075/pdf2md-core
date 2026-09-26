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

/// Collects Hough line-direction votes: for each pair of consecutive glyphs on
/// a line, the angle of the joining segment (the text baseline direction),
/// weighted by the segment length. Long, reliable baselines dominate; a stray
/// glyph is a single low-weight vote. Near-vertical segments (rotated text) are
/// discarded.
pub(super) fn collect_direction_votes(lines: &[TextLine], max_deg: f64) -> Vec<(f64, f64)> {
    let mut votes = Vec::new();
    for line in lines {
        for pair in line.chars.windows(2) {
            let (x0, y0) = (pair[0].origin.0, pair[0].origin.1);
            let (x1, y1) = (pair[1].origin.0, pair[1].origin.1);
            let (dx, dy) = (x1 - x0, y1 - y0);
            let len = (dx * dx + dy * dy).sqrt();
            if len < MIN_VOTE_LENGTH_PT {
                continue;
            }
            let ang = dy.atan2(dx).to_degrees();
            // Fold the reading direction onto the near-horizontal band so a
            // right-to-left run (dx < 0) still votes for its baseline tilt.
            let a = if ang > 90.0 {
                ang - 180.0
            } else if ang < -90.0 {
                ang + 180.0
            } else {
                ang
            };
            if a.abs() <= max_deg {
                votes.push((a, len));
            }
        }
    }
    votes
}

/// Scores a projection of a point cloud onto the axis perpendicular to a
/// baseline tilted `angle_deg`. Uses the sum of squared histogram bin counts
/// (Radon-style row-alignment energy): when rows align each collapses into a
/// few tall bins (high energy); when they misalign they smear (low energy).
pub(super) fn projection_energy(points: &[(f64, f64)], bin: f64, angle_deg: f64) -> f64 {
    let theta = angle_deg.to_radians();
    let (sin_t, cos_t) = theta.sin_cos();
    if bin <= 0.0 {
        return 0.0;
    }
    let mut proj = Vec::with_capacity(points.len());
    let mut min_p = f64::INFINITY;
    let mut max_p = f64::NEG_INFINITY;
    for &(x, y) in points {
        let p = -sin_t * x + cos_t * y;
        proj.push(p);
        if p < min_p {
            min_p = p;
        }
        if p > max_p {
            max_p = p;
        }
    }
    let range = max_p - min_p;
    if range <= f64::EPSILON {
        return 0.0;
    }
    let bins = ((range / bin).ceil() as usize).clamp(1, 512);
    let mut hist = vec![0usize; bins];
    for &p in &proj {
        let mut b = ((p - min_p) / bin) as usize;
        if b >= bins {
            b = bins - 1;
        }
        hist[b] += 1;
    }
    let mut energy = 0.0;
    for &c in &hist {
        energy += (c as f64) * (c as f64);
    }
    energy
}

/// Brute-force scan over `[lo, hi]` at `step` degrees for the angle maximizing
/// projection energy. Both endpoints are inclusive.
///
/// The energy profile is flat across the small band where the residual
/// misalignment is smaller than a histogram bin, so a naive argmax would pin an
/// arbitrary edge of that plateau (a spurious nonzero angle on an axis-aligned
/// page). We instead take the mid-point of the near-maximal plateau, which is
/// symmetric around the true tilt: exactly `0` for an aligned page and the true
/// angle for a tilted one.
pub(super) fn best_angle_by_projection(points: &[(f64, f64)], bin: f64, lo: f64, hi: f64, step: f64) -> f64 {
    let mut angles: Vec<(f64, f64)> = Vec::new();
    let mut max_e = f64::NEG_INFINITY;
    let mut a = lo;
    loop {
        let e = projection_energy(points, bin, a);
        angles.push((a, e));
        if e > max_e {
            max_e = e;
        }
        if a >= hi {
            break;
        }
        a = (a + step).min(hi);
    }
    let tol = (max_e.abs() * 1e-9).max(1e-9);
    let mut lo_a = f64::INFINITY;
    let mut hi_a = f64::NEG_INFINITY;
    for &(angle, e) in &angles {
        if e >= max_e - tol {
            lo_a = lo_a.min(angle);
            hi_a = hi_a.max(angle);
        }
    }
    (lo_a + hi_a) * 0.5
}

/// Median span font size — drives the projection histogram bin so it adapts to
/// fine print and titles alike without a hard-coded point constant.
pub(super) fn median_span_size(spans: &[Span]) -> f64 {
    let mut n = 0;
    let mut avg = 0.0;
    for s in spans {
        avg += s.size;
        n += 1;
    }
    if n == 0 {
        return 10.0;
    }
    avg / n as f64
}
