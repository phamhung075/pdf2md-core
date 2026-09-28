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

mod projection;
use projection::*;
mod geometry;
use geometry::*;

use crate::cpdf_textpage::{CharInfo, Rect, TextLine, TextWord};
use crate::layout::glyph_stream::Span;

/// Default maximum tilt we are willing to scan for (degrees). Text clustered
/// into lines already tolerates ~10° of intra-line rotation, so scanning a
/// little wider than that comfortably captures skewed scans.
pub const DEFAULT_MAX_SKEW_DEG: f64 = 15.0;

/// Angular resolution of the Hough vote histogram (degrees). Wider than this
/// is enough because votes are averaged within a bin for the final estimate.
pub const COARSE_STEP_DEG: f64 = 0.5;

/// Fine angular step used to refine the coarse projection peak (degrees).
const FINE_STEP_DEG: f64 = 0.1;

/// Minimum anchor points before a projection estimate is trusted. Small clouds
/// (a dozen glyph runs) can align accidentally at a wrong angle, so below this
/// we defer to the axis-aligned hypothesis rather than risk a bad deskew.
const MIN_ANGLE_POINTS: usize = 16;

/// Base relative energy gain the best angle must show over the axis-aligned
/// hypothesis (0°) before it is trusted as a real tilt.
///
/// The projection energy is an auto-ranged histogram sum-of-squares, so on a
/// small or sparse cloud it drifts by a few percent as the bin origin and count
/// move with the projection range. That noise alone can lift an already-aligned
/// page's best angle a hair past 0° and deskew it wrongly, scattering rows that
/// were exactly aligned. A genuine tilt collapses whole rows at the tilt angle,
/// so its peak beats 0° by a wide factor — a few-degree tilt scores several
/// times the aligned energy — not by a few percent. This base margin is the
/// floor of that gap; the angle-proportional term below scales it.
const MIN_ENERGY_GAIN: f64 = 0.02;

/// Additional required energy gain per degree squared of the claimed tilt.
///
/// A genuine tilt of `θ` displaces a row by up to `page_width · sin θ`, so the
/// 0° hypothesis loses alignment in proportion to `sin² θ`: the energy gain a
/// real tilt produces grows with `θ²`, not linearly. A sparse cloud can
/// otherwise yield a *large* spurious angle at the same tiny gain as an aligned
/// page, and a large rotation is the most damaging kind of false positive. The
/// flat `MIN_ENERGY_GAIN` floor was compared against that noise (a few percent)
/// and let a 13.5° "tilt" through on a 3.9 % gain. Requiring the gain to grow
/// with `θ²` keeps small, plausible corrections while rejecting a large angle
/// the aligned hypothesis already explains. Calibrated on the corpus: a 4.5°
/// candidate at a 7 % gain is kept, a 13.5° candidate at 3.9 % is rejected.
const MIN_ENERGY_GAIN_PER_DEG2: f64 = 0.0015;

/// Tilts below this magnitude are treated as noise and left uncorrected
/// (degrees). This both avoids needless coordinate movement on clean docs and
/// leaves the common no-op path free of any cloning.
pub const MIN_SKEW_TO_CORRECT_DEG: f64 = 0.5;

/// A joining segment shorter than this contributes no vote: sub-pixel glyph
/// gaps (kerning noise) are not a reliable direction measurement.
const MIN_VOTE_LENGTH_PT: f64 = 0.5;

/// Estimates the dominant page-skew angle (degrees) from the text-line glyph
/// origins. Returns `0.0` when the page is (or is effectively) axis-aligned or
/// when there is not enough text to measure.
pub fn estimate_skew_angle_deg(lines: &[TextLine]) -> f64 {
    estimate_skew_angle_deg_with(lines, DEFAULT_MAX_SKEW_DEG, COARSE_STEP_DEG)
}

/// Estimation with explicit scan bounds, used for tuning and tests.
pub fn estimate_skew_angle_deg_with(
    lines: &[TextLine],
    max_deg: f64,
    coarse_step_deg: f64,
) -> f64 {
    let votes = collect_direction_votes(lines, max_deg);
    if votes.is_empty() {
        return 0.0;
    }
    let nbins = ((2.0 * max_deg) / coarse_step_deg).ceil() as usize + 1;
    let mut hist = vec![0.0f64; nbins];
    for &(angle_deg, weight) in &votes {
        let idx = ((angle_deg + max_deg) / coarse_step_deg).round() as isize;
        if idx >= 0 && (idx as usize) < nbins {
            hist[idx as usize] += weight;
        }
    }
    let mut best_i = 0;
    let mut best_w = hist[0];
    for (i, &w) in hist.iter().enumerate() {
        if w > best_w {
            best_w = w;
            best_i = i;
        }
    }
    // Refine: length-weighted mean of the votes around the dominant bin so the
    // answer is sub-bin-accurate instead of snapping to a histogram edge.
    let lo = (best_i as f64 - 1.0) * coarse_step_deg - max_deg;
    let hi = (best_i as f64 + 1.0) * coarse_step_deg - max_deg;
    let (mut sum, mut wsum) = (0.0, 0.0);
    for &(angle_deg, weight) in &votes {
        if angle_deg >= lo && angle_deg <= hi {
            sum += angle_deg * weight;
            wsum += weight;
        }
    }
    if wsum <= 0.0 {
        return (best_i as f64 * coarse_step_deg - max_deg).clamp(-max_deg, max_deg);
    }
    (sum / wsum).clamp(-max_deg, max_deg)
}

/// Rotates every text line back to the page axes by `-angle_deg` (the inverse
/// of the tilt the estimator found), so axis-aligned projection cuts work.
///
/// Only geometry is changed: `text` is preserved verbatim, as are font size,
/// style and advance metrics. The glyph `origin` points and every bounding box
/// are rotated about the page-text centroid and re-bounded, so XY-Cut sees
/// true horizontal/vertical valleys instead of tilted ones.
pub fn deskew_lines(lines: &[TextLine], angle_deg: f64) -> Vec<TextLine> {
    let theta = angle_deg.to_radians();
    let (sin_t, cos_t) = theta.sin_cos();
    let (cx, cy) = centroid_of_origins(lines);
    let rot = |x: f64, y: f64| -> (f64, f64) {
        let (dx, dy) = (x - cx, y - cy);
        (
            cos_t * dx + sin_t * dy + cx,
            -sin_t * dx + cos_t * dy + cy,
        )
    };
    lines.iter().map(|l| deskew_line(l, &rot)).collect()
}

/// Estimates the tilt and, when it exceeds [`MIN_SKEW_TO_CORRECT_DEG`], returns
/// a deskewed copy of `lines`; otherwise returns the (unchanged) input and
/// reports the (negligible) measured angle. Convenience wrapper for callers
/// that want a single call.
pub fn correct_skew(lines: &[TextLine]) -> (Vec<TextLine>, f64) {
    let angle = estimate_skew_angle_deg(lines);
    if angle.abs() >= MIN_SKEW_TO_CORRECT_DEG {
        (deskew_lines(lines, angle), angle)
    } else {
        (lines.to_vec(), angle)
    }
}

/// Estimates page skew (degrees) from a pre-line span cloud via a lightweight
/// Radon projection scan. Unlike [`estimate_skew_angle_deg`], this needs no
/// line grouping, so it works directly on the raw glyph spans emitted by the
/// text walker *before* [`build_lines`](crate::layout::glyph_stream::build_lines)
/// tries to cluster them. The projector integrates the span anchors over many
/// candidate baseline directions and picks the one whose perpendicular
/// projection is the sharpest (rows collapse into single histogram peaks).
pub fn estimate_skew_angle_deg_from_spans(
    spans: &[Span],
    max_deg: f64,
    coarse_step_deg: f64,
) -> f64 {
    if spans.is_empty() {
        return 0.0;
    }
    let points: Vec<(f64, f64)> = spans.iter().map(|s| (s.x, s.y)).collect();
    let bin = median_span_size(spans) * 0.5;
    estimate_skew_angle_deg_from_points(&points, bin, max_deg, coarse_step_deg)
}

/// Estimates page skew (degrees) from a layout-independent cloud of baseline
/// anchor points using a lightweight Radon projection scan. `bin_width` is the
/// projection histogram bin in points (typically ~half the median glyph size).
/// Returns `0.0` when there are too few anchors to trust, or when the best
/// angle does not beat the axis-aligned hypothesis (0°) by a decisive margin —
/// both guard against deskewing an already-fine page whose small/complex
/// span cloud merely aligns coincidentally at some wrong angle.
pub fn estimate_skew_angle_deg_from_points(
    points: &[(f64, f64)],
    bin_width: f64,
    max_deg: f64,
    coarse_step_deg: f64,
) -> f64 {
    if points.len() < MIN_ANGLE_POINTS {
        return 0.0;
    }
    let bin = if bin_width.is_finite() && bin_width > 0.0 {
        bin_width
    } else {
        4.0
    };
    let coarse = best_angle_by_projection(points, bin, -max_deg, max_deg, coarse_step_deg);
    // Confidence gate: a tilted page must beat the axis-aligned hypothesis
    // (0°) decisively, otherwise the peak is a spurious alignment of a small /
    // structured cloud and deskewing would rotate a page that is already fine.
    // The bar rises with the square of the claimed angle (see the constant), so
    // a large, implausible tilt needs far more evidence than a small one.
    let e_best = projection_energy(points, bin, coarse);
    let e_zero = projection_energy(points, bin, 0.0);
    let required_gain = MIN_ENERGY_GAIN + MIN_ENERGY_GAIN_PER_DEG2 * coarse * coarse;
    if e_zero > 0.0 && e_best <= e_zero * (1.0 + required_gain) {
        return 0.0;
    }
    if coarse.abs() >= max_deg - coarse_step_deg * 0.5 {
        return coarse.clamp(-max_deg, max_deg);
    }
    best_angle_by_projection(points, bin, coarse - coarse_step_deg, coarse + coarse_step_deg, FINE_STEP_DEG)
        .clamp(-max_deg, max_deg)
}

/// Rotates every span's baseline origin back to the page axes by `-angle_deg`.
/// `advance`, `size`, `text` and the style flags are untouched — a span on the
/// now-horizontal baseline simply keeps its run width and glyph height, so the
/// downstream line clustering and reading-order passes see axis-aligned rows.
pub fn deskew_spans(spans: &[Span], angle_deg: f64) -> Vec<Span> {
    if spans.is_empty() || angle_deg.abs() <= f64::EPSILON {
        return spans.to_vec();
    }
    let theta = angle_deg.to_radians();
    let (sin_t, cos_t) = theta.sin_cos();
    let (cx, cy) = centroid_of_span_origins(spans);
    spans
        .iter()
        .map(|s| {
            let (dx, dy) = (s.x - cx, s.y - cy);
            let mut n = s.clone();
            n.x = cos_t * dx + sin_t * dy + cx;
            n.y = -sin_t * dx + cos_t * dy + cy;
            n
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Estimation internals
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests;
