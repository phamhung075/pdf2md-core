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
                    word_advance: 20.0,
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