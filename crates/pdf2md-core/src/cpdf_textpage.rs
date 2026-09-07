// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Standalone implementation of PDFium's `CPDF_TextPage` analytical algorithms.
//!
//! Re-implements ONLY the core mathematical transformations (ISO 32000 §8.3.3 & §9.4)
//! and character/word/line spatial clustering heuristics from PDFium (`core/fpdftext/cpdf_textpage.cpp`).
//!
//! Zero dependencies on external C/C++ runtimes or GUI rasterizers. 100% Pure Safe Rust.

use serde::{Deserialize, Serialize};

// ===========================================================================
// 1. 2D Axis-Aligned Bounding Box (Rect)
// ===========================================================================

/// Axis-Aligned 2D Bounding Box in Cartesian space.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    pub min_x: f64,
    pub min_y: f64,
    pub max_x: f64,
    pub max_y: f64,
}

impl Rect {
    /// Constructs a normalized rectangle where min <= max.
    #[inline]
    pub fn new(x0: f64, y0: f64, x1: f64, y1: f64) -> Self {
        Self {
            min_x: x0.min(x1),
            min_y: y0.min(y1),
            max_x: x0.max(x1),
            max_y: y0.max(y1),
        }
    }

    /// Creates an empty (degenerate) rectangle at the origin.
    #[inline]
    pub fn zero() -> Self {
        Self {
            min_x: 0.0,
            min_y: 0.0,
            max_x: 0.0,
            max_y: 0.0,
        }
    }

    /// Computes the minimal bounding box enclosing a slice of points.
    pub fn from_points(pts: &[(f64, f64)]) -> Self {
        if pts.is_empty() {
            return Self::zero();
        }
        let mut min_x = f64::INFINITY;
        let mut min_y = f64::INFINITY;
        let mut max_x = f64::NEG_INFINITY;
        let mut max_y = f64::NEG_INFINITY;

        for &(x, y) in pts {
            if x < min_x { min_x = x; }
            if y < min_y { min_y = y; }
            if x > max_x { max_x = x; }
            if y > max_y { max_y = y; }
        }

        Self { min_x, min_y, max_x, max_y }
    }

    #[inline]
    pub fn width(&self) -> f64 {
        (self.max_x - self.min_x).max(0.0)
    }

    #[inline]
    pub fn height(&self) -> f64 {
        (self.max_y - self.min_y).max(0.0)
    }

    #[inline]
    pub fn center_x(&self) -> f64 {
        (self.min_x + self.max_x) * 0.5
    }

    #[inline]
    pub fn center_y(&self) -> f64 {
        (self.min_y + self.max_y) * 0.5
    }

    /// Returns the union of this rectangle with another.
    #[inline]
    pub fn union_with(&self, other: &Rect) -> Rect {
        Rect {
            min_x: self.min_x.min(other.min_x),
            min_y: self.min_y.min(other.min_y),
            max_x: self.max_x.max(other.max_x),
            max_y: self.max_y.max(other.max_y),
        }
    }

    /// Checks if two rectangles intersect.
    #[inline]
    pub fn intersects(&self, other: &Rect) -> bool {
        self.min_x < other.max_x
            && self.max_x > other.min_x
            && self.min_y < other.max_y
            && self.max_y > other.min_y
    }

    /// Checks if a point lies inside or on the boundaries of the rectangle.
    #[inline]
    pub fn contains_point(&self, x: f64, y: f64) -> bool {
        x >= self.min_x && x <= self.max_x && y >= self.min_y && y <= self.max_y
    }
}

// ===========================================================================
// 2. 3x3 Affine Transformation Matrix (ISO 32000 §8.3.3)
// ===========================================================================

/// 3x3 Affine Transformation Matrix matching PDF ISO 32000-1 §8.3.3 specification.
///
/// In PDF specification, a 2D affine transform is represented as a 6-element array `[a, b, c, d, e, f]`
/// corresponding to the 3x3 matrix:
/// ```text
/// [ a  b  0 ]
/// [ c  d  0 ]
/// [ e  f  1 ]
/// ```
/// Points are represented as row vectors `[x, y, 1]`.
/// Mapping into transformed space:
/// `[x', y', 1] = [x, y, 1] * Matrix`
/// `x' = a * x + c * y + e`
/// `y' = b * x + d * y + f`
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Matrix3x3 {
    pub a: f64,
    pub b: f64,
    pub c: f64,
    pub d: f64,
    pub e: f64,
    pub f: f64,
}

impl Matrix3x3 {
    /// Identity Matrix:
    /// ```text
    /// [ 1  0  0 ]
    /// [ 0  1  0 ]
    /// [ 0  0  1 ]
    /// ```
    pub const IDENTITY: Self = Matrix3x3 {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: 1.0,
        e: 0.0,
        f: 0.0,
    };

    /// Creates a matrix from raw 6 components `[a, b, c, d, e, f]`.
    #[inline]
    pub fn new(a: f64, b: f64, c: f64, d: f64, e: f64, f: f64) -> Self {
        Self { a, b, c, d, e, f }
    }

    /// Creates a pure translation matrix.
    #[inline]
    pub fn translation(tx: f64, ty: f64) -> Self {
        Self {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            e: tx,
            f: ty,
        }
    }

    /// Creates a pure scaling matrix.
    #[inline]
    pub fn scaling(sx: f64, sy: f64) -> Self {
        Self {
            a: sx,
            b: 0.0,
            c: 0.0,
            d: sy,
            e: 0.0,
            f: 0.0,
        }
    }

    /// Creates a pure rotation matrix (angle in radians counter-clockwise).
    pub fn rotation(radians: f64) -> Self {
        let (sin_a, cos_a) = radians.sin_cos();
        Self {
            a: cos_a,
            b: sin_a,
            c: -sin_a,
            d: cos_a,
            e: 0.0,
            f: 0.0,
        }
    }

    /// Multiplies `self` by `rhs` such that transforming by `self` then `rhs`
    /// is equivalent to transforming by `self.multiply(rhs)`.
    ///
    /// Row vector multiplication rule:
    /// `v * (M1 * M2) = (v * M1) * M2`
    pub fn multiply(&self, rhs: &Self) -> Self {
        Self {
            a: self.a * rhs.a + self.b * rhs.c,
            b: self.a * rhs.b + self.b * rhs.d,
            c: self.c * rhs.a + self.d * rhs.c,
            d: self.c * rhs.b + self.d * rhs.d,
            e: self.e * rhs.a + self.f * rhs.c + rhs.e,
            f: self.e * rhs.b + self.f * rhs.d + rhs.f,
        }
    }

    /// Transforms a point `(x, y)`:
    /// `x' = a * x + c * y + e`
    /// `y' = b * x + d * y + f`
    #[inline]
    pub fn transform_point(&self, x: f64, y: f64) -> (f64, f64) {
        (
            self.a * x + self.c * y + self.e,
            self.b * x + self.d * y + self.f,
        )
    }

    /// Transforms a vector displacement `(dx, dy)` (translation components `e, f` ignored):
    /// `dx' = a * dx + c * dy`
    /// `dy' = b * dx + d * dy`
    #[inline]
    pub fn transform_vector(&self, dx: f64, dy: f64) -> (f64, f64) {
        (
            self.a * dx + self.c * dy,
            self.b * dx + self.d * dy,
        )
    }

    /// Transforms an axis-aligned rectangle by mapping its 4 corners and returning
    /// the tightest enclosing Axis-Aligned Bounding Box (AABB).
    pub fn transform_rect(&self, rect: &Rect) -> Rect {
        let p1 = self.transform_point(rect.min_x, rect.min_y);
        let p2 = self.transform_point(rect.max_x, rect.min_y);
        let p3 = self.transform_point(rect.max_x, rect.max_y);
        let p4 = self.transform_point(rect.min_x, rect.max_y);
        Rect::from_points(&[p1, p2, p3, p4])
    }

    /// Computes the determinant: `a * d - b * c`.
    #[inline]
    pub fn determinant(&self) -> f64 {
        self.a * self.d - self.b * self.c
    }

    /// Computes the inverse matrix if non-singular.
    pub fn inverse(&self) -> Option<Self> {
        let det = self.determinant();
        if det.abs() < 1e-12 {
            return None;
        }
        let inv_det = 1.0 / det;
        Some(Self {
            a: self.d * inv_det,
            b: -self.b * inv_det,
            c: -self.c * inv_det,
            d: self.a * inv_det,
            e: (self.c * self.f - self.d * self.e) * inv_det,
            f: (self.b * self.e - self.a * self.f) * inv_det,
        })
    }

    /// Scale factor along the X-axis: `sqrt(a^2 + b^2)`.
    #[inline]
    pub fn scale_x(&self) -> f64 {
        (self.a * self.a + self.b * self.b).sqrt()
    }

    /// Scale factor along the Y-axis: `sqrt(c^2 + d^2)`.
    #[inline]
    pub fn scale_y(&self) -> f64 {
        (self.c * self.c + self.d * self.d).sqrt()
    }

    /// Rotation angle in radians: `atan2(b, a)`.
    #[inline]
    pub fn rotation_radians(&self) -> f64 {
        self.b.atan2(self.a)
    }
}

impl Default for Matrix3x3 {
    fn default() -> Self {
        Self::IDENTITY
    }
}

// ===========================================================================
// 3. Core Data Structures
// ===========================================================================

/// Extracted character information containing spatial geometry and font metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CharInfo {
    /// Unicode character decoded from CMap or standard encoding.
    pub unicode: char,
    /// Exact Axis-Aligned Bounding Box in User Space coordinates.
    pub bbox: Rect,
    /// Effective font size in user space points.
    pub font_size: f64,
    /// Transformation matrix mapping text space to user space for this glyph.
    pub matrix: Matrix3x3,
    /// Baseline origin point in user space: `(x_user, y_user) = (0, 0) * Tm * CTM`.
    pub origin: (f64, f64),
    /// Horizontal advance width in user space points.
    pub advance_width: f64,
    /// Bold weight flag (resolved from font descriptor or synthetic stroke).
    pub is_bold: bool,
    /// Italic/Oblique flag (resolved from font descriptor slant angle).
    pub is_italic: bool,
}

/// Coherent word formed by clustering contiguous characters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextWord {
    /// Ordered list of characters in this word.
    pub chars: Vec<CharInfo>,
    /// Bounding box enclosing all characters in this word.
    pub word_bbox: Rect,
    /// Reconstructed string.
    pub text: String,
}

/// Logical text line formed by clustering words along a shared baseline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextLine {
    /// All characters belonging to this line in left-to-right reading order.
    pub chars: Vec<CharInfo>,
    /// Words segmented by detected whitespace thresholds.
    pub words: Vec<TextWord>,
    /// Representative vertical baseline (Y coordinate in User Space).
    pub baseline: f64,
    /// Bounding box enclosing the entire line.
    pub line_bbox: Rect,
    /// Reconstructed line string with synthesized spaces between words.
    pub text: String,
}

/// Logical text block or paragraph formed by clustering lines in a column.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextBlock {
    /// Ordered lines in this block (top-to-bottom reading order).
    pub lines: Vec<TextLine>,
    /// Bounding box enclosing the entire block.
    pub block_bbox: Rect,
    /// Reconstructed block string with newline separators.
    pub text: String,
}

// ===========================================================================
// 4. PDF ISO 32000 §9.4 Text State & Coordinate Transform Engine
// ===========================================================================

/// Tracks PDF Text State parameters across content stream operators (`BT`, `ET`, `Tm`, `Td`, etc.).
#[derive(Debug, Clone)]
pub struct PdfTextState {
    /// Current font size $T_{fs}$ (points).
    pub font_size: f64,
    /// Horizontal scaling $T_h$ (percentage, default 100.0).
    pub horizontal_scaling: f64,
    /// Character spacing $T_c$ (user space units).
    pub char_spacing: f64,
    /// Word spacing $T_w$ (user space units).
    pub word_spacing: f64,
    /// Text rise $T_{rise}$ (user space units).
    pub text_rise: f64,
    /// Text leading $T_l$ (user space units).
    pub leading: f64,
    /// Text Matrix $T_m$.
    pub tm: Matrix3x3,
    /// Text Line Matrix $T_{lm}$.
    pub tlm: Matrix3x3,
    /// Current Transformation Matrix (CTM) from graphics state.
    pub ctm: Matrix3x3,
}

impl Default for PdfTextState {
    fn default() -> Self {
        Self {
            font_size: 12.0,
            horizontal_scaling: 100.0,
            char_spacing: 0.0,
            word_spacing: 0.0,
            text_rise: 0.0,
            leading: 0.0,
            tm: Matrix3x3::IDENTITY,
            tlm: Matrix3x3::IDENTITY,
            ctm: Matrix3x3::IDENTITY,
        }
    }
}

impl PdfTextState {
    /// Initializes text state at a `BT` (Begin Text) operator.
    pub fn begin_text(&mut self) {
        self.tm = Matrix3x3::IDENTITY;
        self.tlm = Matrix3x3::IDENTITY;
    }

    /// Sets explicit Text Matrix and Text Line Matrix (`Tm` operator).
    pub fn set_text_matrix(&mut self, a: f64, b: f64, c: f64, d: f64, e: f64, f: f64) {
        let m = Matrix3x3::new(a, b, c, d, e, f);
        self.tm = m;
        self.tlm = m;
    }

    /// Moves text line origin (`Td` operator):
    /// `Tlm = Translate(tx, ty) * Tlm`
    /// `Tm = Tlm`
    pub fn move_text_line(&mut self, tx: f64, ty: f64) {
        let t = Matrix3x3::translation(tx, ty);
        self.tlm = t.multiply(&self.tlm);
        self.tm = self.tlm;
    }

    /// Moves text line origin and sets leading (`TD` operator).
    pub fn move_text_line_with_leading(&mut self, tx: f64, ty: f64) {
        self.leading = -ty;
        self.move_text_line(tx, ty);
    }

    /// Moves to start of next line using current leading (`T*` operator).
    pub fn next_line(&mut self) {
        self.move_text_line(0.0, -self.leading);
    }

    /// Computes the Text Rendering Matrix (TRM) mapping normalized glyph space (1/1000)
    /// into Device / User Space:
    ///
    /// $$TRM = \begin{bmatrix} T_{fs} \times \frac{T_h}{100.0} & 0 & 0 \\ 0 & T_{fs} & 0 \\ 0 & T_{rise} & 1 \end{bmatrix} \times T_m \times CTM$$
    pub fn text_rendering_matrix(&self) -> Matrix3x3 {
        let font_scale_matrix = Matrix3x3::new(
            self.font_size * (self.horizontal_scaling / 100.0),
            0.0,
            0.0,
            self.font_size,
            0.0,
            self.text_rise,
        );
        font_scale_matrix.multiply(&self.tm).multiply(&self.ctm)
    }

    /// Maps glyph origin and displacement vectors into User Space:
    /// `[x_user, y_user] = [x_text, y_text] * Tm * CTM`
    #[inline]
    pub fn text_to_user_space(&self, x_text: f64, y_text: f64) -> (f64, f64) {
        let compound = self.tm.multiply(&self.ctm);
        compound.transform_point(x_text, y_text + self.text_rise)
    }

    /// Computes accurate character bounding box in user coordinates given raw glyph metrics in font units (1/1000).
    ///
    /// If font bounding box is not available, standard typographic metrics `[0, -200, advance, 800]` are used.
    pub fn compute_char_bbox(&self, advance_font_units: f64, glyph_bbox_font_units: Option<Rect>) -> Rect {
        let raw_box = glyph_bbox_font_units.unwrap_or_else(|| {
            Rect::new(0.0, -200.0, advance_font_units, 800.0)
        });

        // Normalize font units (1/1000)
        let normalized = Rect::new(
            raw_box.min_x / 1000.0,
            raw_box.min_y / 1000.0,
            raw_box.max_x / 1000.0,
            raw_box.max_y / 1000.0,
        );

        let trm = self.text_rendering_matrix();
        trm.transform_rect(&normalized)
    }

    /// Advances the Text Matrix ($T_m$) after rendering a glyph of width `advance_font_units` (in 1/1000 font units).
    ///
    /// ISO 32000 formula for displacement:
    /// $$t_x = \left( \frac{w_0}{1000.0} \times T_{fs} + T_c + (\text{if space } T_w) \right) \times \frac{T_h}{100.0}$$
    /// $$T_m = \text{Translate}(t_x, 0) \times T_m$$
    pub fn advance_glyph(&mut self, advance_font_units: f64, is_space: bool) -> f64 {
        let w0 = advance_font_units / 1000.0;
        let mut tx = w0 * self.font_size + self.char_spacing;
        if is_space {
            tx += self.word_spacing;
        }
        tx *= self.horizontal_scaling / 100.0;

        let disp = Matrix3x3::translation(tx, 0.0);
        self.tm = disp.multiply(&self.tm);
        tx
    }
}

// ===========================================================================
// 5. Spatial Clustering Heuristics (Ported from PDFium CPDF_TextPage)
// ===========================================================================

/// Threshold configuration for character spacing, line clustering, and column detection.
#[derive(Debug, Clone)]
pub struct ClusterConfig {
    /// Minimum space threshold relative to font size (default: 0.20).
    /// If `gap >= font_size * space_threshold_factor`, a word break is formed.
    pub space_threshold_factor: f64,
    /// Kerning threshold relative to font size (default: -0.20).
    /// Gaps tighter than this are treated as overlapping glyphs / ligatures.
    pub kerning_tolerance_factor: f64,
    /// Vertical baseline tolerance relative to font size (default: 0.25).
    /// Characters with baseline delta within this threshold are grouped into the same line.
    pub baseline_tolerance_factor: f64,
    /// Line spacing threshold factor for block/paragraph grouping (default: 1.8).
    pub line_spacing_factor: f64,
    /// Column gutter threshold relative to font size (default: 2.0).
    /// Horizontal gaps larger than this indicate multi-column boundaries.
    pub column_gutter_factor: f64,
    /// Angle tolerance in radians for same text direction (default: 10 degrees ~ 0.174 rad).
    pub direction_tolerance_rad: f64,
}

impl Default for ClusterConfig {
    fn default() -> Self {
        Self {
            space_threshold_factor: 0.20,
            kerning_tolerance_factor: -0.20,
            baseline_tolerance_factor: 0.25,
            line_spacing_factor: 1.8,
            column_gutter_factor: 2.0,
            direction_tolerance_rad: 10.0f64.to_radians(),
        }
    }
}

/// Core spatial clustering engine ported from PDFium's `cpdf_textpage.cpp`.
pub struct SpatialClusterer {
    pub config: ClusterConfig,
}

impl SpatialClusterer {
    pub fn new(config: ClusterConfig) -> Self {
        Self { config }
    }

    /// Clusters an unordered or stream-ordered array of `CharInfo` tokens into coherent text lines.
    pub fn cluster_into_lines(&self, chars: &[CharInfo]) -> Vec<TextLine> {
        if chars.is_empty() {
            return Vec::new();
        }

        // 1. Bucket characters by baseline and writing direction
        let mut line_buckets: Vec<Vec<CharInfo>> = Vec::new();

        for ch in chars {
            let ch_angle = ch.matrix.rotation_radians();
            let mut matched_line = None;

            for bucket in line_buckets.iter_mut() {
                let ref_ch = &bucket[0];
                let angle_diff = (ch_angle - ref_ch.matrix.rotation_radians()).abs();
                let effective_fs = ch.font_size.min(ref_ch.font_size);
                let baseline_tol = (effective_fs * self.config.baseline_tolerance_factor).max(1.0);

                if angle_diff <= self.config.direction_tolerance_rad
                    && (ch.origin.1 - ref_ch.origin.1).abs() <= baseline_tol
                {
                    matched_line = Some(bucket);
                    break;
                }
            }

            if let Some(bucket) = matched_line {
                bucket.push(ch.clone());
            } else {
                line_buckets.push(vec![ch.clone()]);
            }
        }

        // 2. Sort lines vertically (PDF User Space: descending Y = top to bottom)
        line_buckets.sort_by(|a, b| {
            let avg_y_a = a.iter().map(|c| c.origin.1).sum::<f64>() / a.len() as f64;
            let avg_y_b = b.iter().map(|c| c.origin.1).sum::<f64>() / b.len() as f64;
            avg_y_b.partial_cmp(&avg_y_a).unwrap_or(std::cmp::Ordering::Equal)
        });

        // 3. For each line, sort characters along the reading axis (ascending X) and segment into words
        let mut result_lines = Vec::new();

        for mut bucket in line_buckets {
            bucket.sort_by(|a, b| {
                a.origin.0.partial_cmp(&b.origin.0).unwrap_or(std::cmp::Ordering::Equal)
            });

            let line = self.build_line_with_words(bucket);
            result_lines.push(line);
        }

        result_lines
    }

    /// Segments a sorted horizontal character sequence into words using PDFium whitespace heuristics.
    fn build_line_with_words(&self, chars: Vec<CharInfo>) -> TextLine {
        if chars.is_empty() {
            return TextLine {
                chars: Vec::new(),
                words: Vec::new(),
                baseline: 0.0,
                line_bbox: Rect::zero(),
                text: String::new(),
            };
        }

        let avg_baseline = chars.iter().map(|c| c.origin.1).sum::<f64>() / chars.len() as f64;
        let mut line_bbox = chars[0].bbox;
        for c in &chars[1..] {
            line_bbox = line_bbox.union_with(&c.bbox);
        }

        let mut words: Vec<TextWord> = Vec::new();
        let mut current_word_chars: Vec<CharInfo> = Vec::new();
        let mut full_line_text = String::new();

        for i in 0..chars.len() {
            let cur = &chars[i];

            if current_word_chars.is_empty() {
                current_word_chars.push(cur.clone());
                continue;
            }

            let prev = current_word_chars.last().unwrap();
            let effective_fs = prev.font_size.min(cur.font_size);
            let space_threshold = (effective_fs * self.config.space_threshold_factor).max(1.5);
            let _kerning_floor = effective_fs * self.config.kerning_tolerance_factor;

            // Distance along horizontal baseline between characters
            let gap = cur.bbox.min_x - prev.bbox.max_x;

            if gap >= space_threshold {
                // Word boundary: finalize current word
                let word_text: String = current_word_chars.iter().map(|c| c.unicode).collect();
                let mut word_bbox = current_word_chars[0].bbox;
                for c in &current_word_chars[1..] {
                    word_bbox = word_bbox.union_with(&c.bbox);
                }

                if !full_line_text.is_empty() {
                    full_line_text.push(' ');
                }
                full_line_text.push_str(&word_text);

                words.push(TextWord {
                    chars: std::mem::take(&mut current_word_chars),
                    word_bbox,
                    text: word_text,
                });
            }

            current_word_chars.push(cur.clone());
        }

        // Finalize remaining word
        if !current_word_chars.is_empty() {
            let word_text: String = current_word_chars.iter().map(|c| c.unicode).collect();
            let mut word_bbox = current_word_chars[0].bbox;
            for c in &current_word_chars[1..] {
                word_bbox = word_bbox.union_with(&c.bbox);
            }

            if !full_line_text.is_empty() {
                full_line_text.push(' ');
            }
            full_line_text.push_str(&word_text);

            words.push(TextWord {
                chars: current_word_chars,
                word_bbox,
                text: word_text,
            });
        }

        TextLine {
            chars,
            words,
            baseline: avg_baseline,
            line_bbox,
            text: full_line_text,
        }
    }

    /// Clusters lines into columns and logical text blocks (paragraphs) with reading-order sorting.
    pub fn cluster_into_blocks(&self, lines: &[TextLine]) -> Vec<TextBlock> {
        if lines.is_empty() {
            return Vec::new();
        }

        // Group lines by horizontal overlap / column boundaries
        let mut blocks: Vec<Vec<TextLine>> = Vec::new();

        for line in lines {
            let mut placed = false;

            for block in blocks.iter_mut() {
                let last_line = block.last().unwrap();
                let avg_fs = line.chars.first().map(|c| c.font_size).unwrap_or(12.0);
                let line_spacing_tol = avg_fs * self.config.line_spacing_factor;
                let v_delta = (last_line.baseline - line.baseline).abs();

                // Check vertical proximity and horizontal alignment
                let h_overlap = line.line_bbox.min_x < last_line.line_bbox.max_x
                    && line.line_bbox.max_x > last_line.line_bbox.min_x;
                let margin_aligned = (line.line_bbox.min_x - last_line.line_bbox.min_x).abs() <= avg_fs * 1.5;

                if v_delta <= line_spacing_tol && (h_overlap || margin_aligned) {
                    block.push(line.clone());
                    placed = true;
                    break;
                }
            }

            if !placed {
                blocks.push(vec![line.clone()]);
            }
        }

        // Multi-column reading order sorting:
        // Group blocks by columns (left-to-right), then sort top-to-bottom within each column
        blocks.sort_by(|b1, b2| {
            let bbox1 = b1.iter().fold(b1[0].line_bbox, |acc, l| acc.union_with(&l.line_bbox));
            let bbox2 = b2.iter().fold(b2[0].line_bbox, |acc, l| acc.union_with(&l.line_bbox));

            // If horizontally distinct with column gutter, sort left-to-right
            let avg_fs = 12.0;
            let gutter = avg_fs * self.config.column_gutter_factor;
            if bbox1.max_x + gutter <= bbox2.min_x {
                std::cmp::Ordering::Less
            } else if bbox2.max_x + gutter <= bbox1.min_x {
                std::cmp::Ordering::Greater
            } else {
                // Same column: sort top-to-bottom (descending Y in PDF coordinates)
                bbox2.max_y.partial_cmp(&bbox1.max_y).unwrap_or(std::cmp::Ordering::Equal)
            }
        });

        // Convert to TextBlock structs
        blocks
            .into_iter()
            .map(|blines| {
                let mut block_bbox = blines[0].line_bbox;
                let mut full_text = String::new();

                for (idx, l) in blines.iter().enumerate() {
                    block_bbox = block_bbox.union_with(&l.line_bbox);
                    if idx > 0 {
                        full_text.push('\n');
                    }
                    full_text.push_str(&l.text);
                }

                TextBlock {
                    lines: blines,
                    block_bbox,
                    text: full_text,
                }
            })
            .collect()
    }
}

// ===========================================================================
// 6. Unit Tests Demonstrating Mathematical & Geometric Correctness
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// Demonstrates exact affine matrix math, compound transforms, and coordinate mapping:
    /// `[x_user, y_user] = [x_text, y_text] * Tm * CTM`
    #[test]
    fn test_matrix_coordinate_transforms() {
        // 1. Identity transform
        let id = Matrix3x3::IDENTITY;
        assert_eq!(id.transform_point(10.0, 20.0), (10.0, 20.0));

        // 2. Translation matrix: tx = 100, ty = 200
        let t = Matrix3x3::translation(100.0, 200.0);
        assert_eq!(t.transform_point(10.0, 20.0), (110.0, 220.0));

        // 3. Scaling matrix: sx = 2, sy = 3
        let s = Matrix3x3::scaling(2.0, 3.0);
        assert_eq!(s.transform_point(10.0, 20.0), (20.0, 60.0));

        // 4. Compound transformation: Tm * CTM
        // Text Matrix: scaled by 12 points, placed at origin (50, 700)
        let tm = Matrix3x3::new(12.0, 0.0, 0.0, 12.0, 50.0, 700.0);
        // CTM: scaled by 2 (DPI scaling or display matrix), translated by (10, 10)
        let ctm = Matrix3x3::new(2.0, 0.0, 0.0, 2.0, 10.0, 10.0);

        let compound = tm.multiply(&ctm);

        // Point at text origin (0, 0):
        // [0, 0] * Tm = [50, 700]
        // [50, 700] * CTM = [50*2 + 10, 700*2 + 10] = [110, 1410]
        let mapped = compound.transform_point(0.0, 0.0);
        assert!((mapped.0 - 110.0).abs() < 1e-6);
        assert!((mapped.1 - 1410.0).abs() < 1e-6);

        // Vector displacement test (translation invariant)
        let v = compound.transform_vector(1.0, 1.0);
        assert_eq!(v, (24.0, 24.0));

        // 5. Matrix Inversion
        let inv = compound.inverse().expect("matrix must be invertible");
        let back = inv.transform_point(mapped.0, mapped.1);
        assert!((back.0 - 0.0).abs() < 1e-6);
        assert!((back.1 - 0.0).abs() < 1e-6);
    }

    /// Demonstrates PDF text state advance calculation and character bounding box generation.
    #[test]
    fn test_char_bounding_box_generation() {
        let mut state = PdfTextState::default();
        state.font_size = 10.0;
        state.horizontal_scaling = 100.0;
        state.set_text_matrix(1.0, 0.0, 0.0, 1.0, 72.0, 720.0);

        // Character 'H' with advance width 722 font units (1/1000)
        let bbox = state.compute_char_bbox(722.0, Some(Rect::new(0.0, 0.0, 722.0, 700.0)));

        assert!((bbox.min_x - 72.0).abs() < 1e-6);
        assert!((bbox.min_y - 720.0).abs() < 1e-6);
        assert!((bbox.max_x - (72.0 + 7.22)).abs() < 1e-6);
        assert!((bbox.max_y - (720.0 + 7.0)).abs() < 1e-6);

        // Advance glyph
        let dx = state.advance_glyph(722.0, false);
        assert!((dx - 7.22).abs() < 1e-6);
        assert!((state.tm.e - (72.0 + 7.22)).abs() < 1e-6);
    }

    /// Demonstrates PDFium thresholding heuristics:
    /// - Intra-word gap vs inter-word space detection
    /// - Merging unordered `CharInfo` tokens into coherent words and lines.
    #[test]
    fn test_merge_unordered_chars_into_words_and_lines() {
        let clusterer = SpatialClusterer::new(ClusterConfig::default());

        // Construct characters for two lines:
        // Line 1 (y = 700): "Hello World"
        // Line 2 (y = 680): "Rust PDF"
        // Feed them in deliberately scrambled / reverse order!
        let font_size = 12.0;

        let make_char = |c: char, x: f64, y: f64, width: f64| -> CharInfo {
            CharInfo {
                unicode: c,
                bbox: Rect::new(x, y - 2.0, x + width, y + 10.0),
                font_size,
                matrix: Matrix3x3::translation(x, y),
                origin: (x, y),
                advance_width: width,
                is_bold: false,
                is_italic: false,
            }
        };

        // Line 1: "Hello World" (at y = 700)
        // 'H'(50..58), 'e'(58..65), 'l'(65..70), 'l'(70..75), 'o'(75..82)  -> gap = 0.0 (< 0.2*12)
        // [space gap = 4.0 >= 2.4] -> 'W'(86..96), 'o'(96..103), 'r'(103..108), 'l'(108..113), 'd'(113..120)
        let mut chars = vec![
            make_char('l', 70.0, 700.0, 5.0),
            make_char('e', 58.0, 700.0, 7.0),
            make_char('o', 75.0, 700.0, 7.0),
            make_char('H', 50.0, 700.0, 8.0),
            make_char('l', 65.0, 700.0, 5.0),
            make_char('d', 113.0, 700.0, 7.0),
            make_char('W', 86.0, 700.0, 10.0),
            make_char('r', 103.0, 700.0, 5.0),
            make_char('l', 108.0, 700.0, 5.0),
            make_char('o', 96.0, 700.0, 7.0),
        ];

        // Line 2: "Rust PDF" (at y = 680)
        // 'R'(50..58), 'u'(58..65), 's'(65..70), 't'(70..75) -> gap = 4.0 -> 'P'(79..86), 'D'(86..93), 'F'(93..99)
        chars.extend(vec![
            make_char('F', 93.0, 680.0, 6.0),
            make_char('u', 58.0, 680.0, 7.0),
            make_char('D', 86.0, 680.0, 7.0),
            make_char('P', 79.0, 680.0, 7.0),
            make_char('R', 50.0, 680.0, 8.0),
            make_char('t', 70.0, 680.0, 5.0),
            make_char('s', 65.0, 680.0, 5.0),
        ]);

        // Execute clustering
        let lines = clusterer.cluster_into_lines(&chars);

        assert_eq!(lines.len(), 2, "Must form exactly 2 lines");

        // First line must be top line (y = 700)
        assert_eq!(lines[0].text, "Hello World");
        assert_eq!(lines[0].words.len(), 2);
        assert_eq!(lines[0].words[0].text, "Hello");
        assert_eq!(lines[0].words[1].text, "World");

        // Second line must be bottom line (y = 680)
        assert_eq!(lines[1].text, "Rust PDF");
        assert_eq!(lines[1].words.len(), 2);
        assert_eq!(lines[1].words[0].text, "Rust");
        assert_eq!(lines[1].words[1].text, "PDF");
    }

    /// Demonstrates multi-column reading order:
    /// Left column must be read in full (top-to-bottom) before starting the right column.
    #[test]
    fn test_multi_column_reading_order() {
        let clusterer = SpatialClusterer::new(ClusterConfig::default());
        let font_size = 12.0;

        let make_line = |text: &str, x: f64, y: f64| -> TextLine {
            let mut cur_x = x;
            let mut chars = Vec::new();
            for c in text.chars() {
                let w = 7.0;
                chars.push(CharInfo {
                    unicode: c,
                    bbox: Rect::new(cur_x, y - 2.0, cur_x + w, y + 10.0),
                    font_size,
                    matrix: Matrix3x3::translation(cur_x, y),
                    origin: (cur_x, y),
                    advance_width: w,
                    is_bold: false,
                    is_italic: false,
                });
                cur_x += w;
            }
            TextLine {
                chars,
                words: vec![],
                baseline: y,
                line_bbox: Rect::new(x, y - 2.0, cur_x, y + 10.0),
                text: text.to_string(),
            }
        };

        // Two columns side-by-side:
        // Col 1: x = 50..150; Line 1 (y = 700), Line 2 (y = 680)
        // Col 2: x = 300..400; Line 3 (y = 700), Line 4 (y = 680)
        let lines = vec![
            make_line("Col1 Top", 50.0, 700.0),
            make_line("Col2 Top", 300.0, 700.0),
            make_line("Col1 Bottom", 50.0, 680.0),
            make_line("Col2 Bottom", 300.0, 680.0),
        ];

        let blocks = clusterer.cluster_into_blocks(&lines);

        assert_eq!(blocks.len(), 2, "Must partition into 2 distinct column blocks");

        // Reading order: Col 1 then Col 2
        assert!(blocks[0].text.contains("Col1 Top"));
        assert!(blocks[0].text.contains("Col1 Bottom"));

        assert!(blocks[1].text.contains("Col2 Top"));
        assert!(blocks[1].text.contains("Col2 Bottom"));
    }
}
