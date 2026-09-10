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

/// Relative energy gain the best angle must show over the axis-aligned
/// hypothesis (0°) before it is trusted as a real tilt. A genuine tilt makes
/// the projection sharply peaky only at the tilt angle, so the peak beats 0°
/// by a wide margin; a coincidental alignment does not.
const MIN_ENERGY_GAIN: f64 = 0.02;

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
    let e_best = projection_energy(points, bin, coarse);
    let e_zero = projection_energy(points, bin, 0.0);
    if e_zero > 0.0 && e_best <= e_zero * (1.0 + MIN_ENERGY_GAIN) {
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

/// Collects Hough line-direction votes: for each pair of consecutive glyphs on
/// a line, the angle of the joining segment (the text baseline direction),
/// weighted by the segment length. Long, reliable baselines dominate; a stray
/// glyph is a single low-weight vote. Near-vertical segments (rotated text) are
/// discarded.
fn collect_direction_votes(lines: &[TextLine], max_deg: f64) -> Vec<(f64, f64)> {
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
fn projection_energy(points: &[(f64, f64)], bin: f64, angle_deg: f64) -> f64 {
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
fn best_angle_by_projection(points: &[(f64, f64)], bin: f64, lo: f64, hi: f64, step: f64) -> f64 {
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
fn median_span_size(spans: &[Span]) -> f64 {
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

fn centroid_of_span_origins(spans: &[Span]) -> (f64, f64) {
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

fn centroid_of_origins(lines: &[TextLine]) -> (f64, f64) {
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

fn deskew_line(line: &TextLine, rot: &impl Fn(f64, f64) -> (f64, f64)) -> TextLine {
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
fn rotate_rect(rect: &Rect, rot: &impl Fn(f64, f64) -> (f64, f64)) -> Rect {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Rotate a local point about the given pivot by `angle_rad` (CCW in
    /// y-up space).
    fn rot_pt(x: f64, y: f64, angle_rad: f64, pivot: (f64, f64)) -> (f64, f64) {
        let (s, c) = angle_rad.sin_cos();
        let (dx, dy) = (x - pivot.0, y - pivot.1);
        (c * dx - s * dy + pivot.0, s * dx + c * dy + pivot.1)
    }

    fn make_page(tilt_deg: f64, rows: usize, cols: usize) -> Vec<TextLine> {
        // Simulate a tabular page: `cols` aligned columns and `rows` lines,
        // each line's glyphs laid along the tilted baseline.
        let mut lines = Vec::new();
        let column_xs: Vec<f64> = (0..cols).map(|c| 60.0 + c as f64 * 120.0).collect();
        for r in 0..rows {
            let y = 700.0 - r as f64 * 18.0;
            let mut chars = Vec::new();
            for (ci, &cx) in column_xs.iter().enumerate() {
                for (i, ch) in format!("C{}{}", ci, r).chars().enumerate() {
                    // Position this glyph in the tilted frame at x = cx + i*8
                    let (ox, oy) =
                        rot_pt(cx + i as f64 * 8.0, y, tilt_deg.to_radians(), (300.0, 500.0));
                    let bbox = Rect::new(ox - 3.0, oy - 5.0, ox + 3.0, oy + 5.0);
                    chars.push(CharInfo {
                        unicode: ch,
                        bbox,
                        font_size: 10.0,
                        matrix: crate::cpdf_textpage::Matrix3x3::rotation(tilt_deg.to_radians()),
                        origin: (ox, oy),
                        advance_width: 8.0,
                        is_bold: false,
                        is_italic: false,
                    });
                }
            }
            let mut line_bbox = chars[0].bbox;
            for c in &chars[1..] {
                line_bbox = line_bbox.union_with(&c.bbox);
            }
            let baseline = chars.iter().map(|c| c.origin.1).sum::<f64>() / chars.len() as f64;
            let text_str: String = chars.iter().map(|c| c.unicode).collect();
            lines.push(TextLine {
                chars,
                words: Vec::new(),
                baseline,
                line_bbox,
                text: text_str,
            });
        }
        lines
    }

    /// Rotate the four corners of an axis-aligned rect about `pivot` and take
    /// the enclosing AABB — matches how a skewed page records a glyph box.
    fn rot_rect(rect: &Rect, angle_rad: f64, pivot: (f64, f64)) -> Rect {
        let (a, b) = rot_pt(rect.min_x, rect.min_y, angle_rad, pivot);
        let (c, d) = rot_pt(rect.max_x, rect.min_y, angle_rad, pivot);
        let (e, f) = rot_pt(rect.max_x, rect.max_y, angle_rad, pivot);
        let (g, h) = rot_pt(rect.min_x, rect.max_y, angle_rad, pivot);
        Rect {
            min_x: a.min(c).min(e).min(g),
            min_y: b.min(d).min(f).min(h),
            max_x: a.max(c).max(e).max(g),
            max_y: b.max(d).max(f).max(h),
        }
    }

    /// A single-column page of two paragraphs, each two long lines, tilted by
    /// `tilt_deg`. Line baselines are 14pt apart within a paragraph and 34pt
    /// between paragraphs (so only a horizontal XY-Cut valley can separate
    /// them), and lines are ~500pt wide so a small tilt makes each line box
    /// ~27pt tall — enough to smear the paragraph valley.
    fn make_paragraph_page(tilt_deg: f64) -> Vec<TextLine> {
        let pivot = (280.0, 500.0);
        let baselines = [700.0, 686.0, 652.0, 638.0]; // 2-para, 2-line each
        let mut lines = Vec::new();
        for (li, &y) in baselines.iter().enumerate() {
            let mut chars = Vec::new();
            for x_i in 0..62usize {
                let x = 30.0 + x_i as f64 * 8.0;
                let (ox, oy) = rot_pt(x, y, tilt_deg.to_radians(), pivot);
                let aligned_box = Rect::new(x - 3.0, y - 5.0, x + 3.0, y + 5.0);
                let bbox = rot_rect(&aligned_box, tilt_deg.to_radians(), pivot);
                chars.push(CharInfo {
                    unicode: 'a',
                    bbox,
                    font_size: 10.0,
                    matrix: crate::cpdf_textpage::Matrix3x3::rotation(tilt_deg.to_radians()),
                    origin: (ox, oy),
                    advance_width: 8.0,
                    is_bold: false,
                    is_italic: false,
                });
            }
            let mut line_bbox = chars[0].bbox;
            for c in &chars[1..] {
                line_bbox = line_bbox.union_with(&c.bbox);
            }
            let baseline = chars.iter().map(|c| c.origin.1).sum::<f64>() / chars.len() as f64;
            let text_str = format!("line {li}");
            lines.push(TextLine {
                chars,
                words: Vec::new(),
                baseline,
                line_bbox,
                text: text_str,
            });
        }
        lines
    }

    #[test]
    fn estimate_detects_known_tilt() {
        for &tilt in &[3.0, -4.0, 6.0] {
            let lines = make_page(tilt, 6, 3);
            let est = estimate_skew_angle_deg(&lines);
            assert!(
                (est - tilt).abs() <= 0.6,
                "tilt {tilt} deg estimated as {est} deg"
            );
        }
    }

    /// A page of `rows` visual rows × `cols` spans per row, laid on a baseline
    /// tilted by `tilt_deg` about `pivot`. Spans are wide enough apart that the
    /// tilt creates a discardable within-row baseline drift.
    fn make_span_page(tilt_deg: f64, rows: usize, cols: usize) -> Vec<Span> {
        let pivot = (150.0, 500.0);
        let xs: Vec<f64> = (0..cols).map(|c| 50.0 + c as f64 * 100.0).collect();
        let mut spans = Vec::new();
        for r in 0..rows {
            let y = 700.0 - r as f64 * 30.0;
            for (ci, &x) in xs.iter().enumerate() {
                let (rx, ry) = rot_pt(x, y, tilt_deg.to_radians(), pivot);
                spans.push(Span {
                    text: format!("{ci}"),
                    x: rx,
                    y: ry,
                    size: 10.0,
                    advance: 20.0,
                    is_bold: false,
                    is_italic: false,
                    is_underline: false,
                    is_vertical: false,
                });
            }
        }
        spans
    }

    #[test]
    fn span_estimator_detects_known_tilt() {
        for &tilt in &[3.0, -4.0] {
            let spans = make_span_page(tilt, 6, 4);
            let est = estimate_skew_angle_deg_from_spans(&spans, DEFAULT_MAX_SKEW_DEG, COARSE_STEP_DEG);
            assert!(
                (est - tilt).abs() <= 0.8,
                "span tilt {tilt} deg estimated as {est} deg"
            );
        }
    }

    #[test]
    fn span_aligned_page_is_sub_threshold() {
        let spans = make_span_page(0.0, 6, 4);
        let est = estimate_skew_angle_deg_from_spans(&spans, DEFAULT_MAX_SKEW_DEG, COARSE_STEP_DEG);
        assert!(
            est.abs() < MIN_SKEW_TO_CORRECT_DEG,
            "aligned span page must not be flagged, got {est}"
        );
    }

    #[test]
    fn deskew_spans_restores_row_alignment_for_build_lines() {
        use crate::layout::glyph_stream::build_lines;
        let rows = 4;
        let cols = 6;
        let tilted = make_span_page(3.0, rows, cols);

        // Tilt smears the baseline: some visual line is fragmented by
        // build_lines, which clusters spans by a tight baseline-y tolerance.
        let raw_lines = build_lines(&tilted);
        assert!(
            raw_lines.iter().any(|l| l.len() < cols),
            "a tilted page must fragment at least one row"
        );

        let skew = estimate_skew_angle_deg_from_spans(&tilted, DEFAULT_MAX_SKEW_DEG, COARSE_STEP_DEG);
        let deskewed = deskew_spans(&tilted, skew);
        let lines = build_lines(&deskewed);
        assert_eq!(lines.len(), rows, "deskewed page must recover one line per row");
        assert!(
            lines.iter().all(|l| l.len() == cols),
            "every deskewed line must hold all column spans"
        );
    }

    #[test]
    fn aligned_page_reports_zero() {
        let lines = make_page(0.0, 6, 3);
        assert_eq!(estimate_skew_angle_deg(&lines), 0.0);
    }

    #[test]
    fn deskew_reduces_per_line_height() {
        let lines = make_page(3.0, 6, 3);
        let skew = estimate_skew_angle_deg(&lines);
        let deskewed = deskew_lines(&lines, skew);
        for (before, after) in lines.iter().zip(deskewed.iter()) {
            // A tilted line's AABB picks up height = glyph_height + width*sin(tilt).
            // After deskew it should collapse back toward the glyph height (~10).
            assert!(
                after.line_bbox.height() <= before.line_bbox.height(),
                "deskew {skew} did not reduce line height: before={} after={}",
                before.line_bbox.height(),
                after.line_bbox.height()
            );
            assert!(
                after.line_bbox.height() < 16.0,
                "deskewed line still too tall: {}",
                after.line_bbox.height()
            );
        }
    }

    #[test]
    fn xy_cut_recovers_paragraph_split_on_skewed_page() {
        use crate::layout::xy_cut::recursive_xy_cut;
        use crate::layout::xy_cut::XyCutOptions;

        // Two paragraphs of two long lines each, tilted 2°. The long lines make
        // each tilted box ~27pt tall, so the 34pt inter-paragraph gap collapses
        // to ~7pt — below the 14pt cut threshold — and naive XY-Cut merges the
        // whole page into one block.
        let lines = make_paragraph_page(2.0);
        let options = XyCutOptions::default();
        let median_fs = 10.0;

        let raw_blocks = recursive_xy_cut(&lines, &options, median_fs);
        assert_eq!(
            raw_blocks.len(),
            1,
            "tilted page must resist naive XY-Cut segmentation ({} blocks)",
            raw_blocks.len()
        );

        // After skew correction, the inter-paragraph valley reappears and the
        // page splits into its two paragraphs.
        let skew = estimate_skew_angle_deg(&lines);
        assert!(
            skew.abs() >= MIN_SKEW_TO_CORRECT_DEG,
            "estimator must flag the 2deg tilt, got {skew}"
        );
        let deskewed = deskew_lines(&lines, skew);
        let blocks = recursive_xy_cut(&deskewed, &options, median_fs);
        assert!(
            blocks.len() >= 2,
            "deskewed page must split into its paragraphs, got {}",
            blocks.len()
        );
    }

    #[test]
    fn correction_is_noop_for_aligned_page_of_lines() {
        let lines = make_page(0.0, 5, 3);
        let (out, angle) = correct_skew(&lines);
        assert_eq!(angle, 0.0);
        assert_eq!(out.len(), lines.len());
        // No-op path should leave bounding boxes byte-identical.
        for (a, b) in out.iter().zip(lines.iter()) {
            assert_eq!(a.line_bbox, b.line_bbox);
        }
    }
}
