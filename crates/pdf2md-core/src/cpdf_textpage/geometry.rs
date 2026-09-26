// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).
//
// ATTRIBUTION — Apache-2.0 / BSD-3-Clause NOTICE (required by Apache License 2.0 §4)
// This file is a Rust port/derivative of PDFium's `CPDF_TextPage` analytic
// algorithms (PDFium file `core/fpdftext/cpdf_textpage.cpp`), substantially
// rewritten in safe Rust (renamed structures, adjusted thresholds, new data
// model). The PDFium-derived algorithmic portions remain licensed under the
// Apache License 2.0 and the BSD-3-Clause notice below; the original Rust
// additions are licensed under BSL-1.1. Any modified source must retain this
// attribution and the upstream copyright
// (also reproduced in THIRD_PARTY_LICENSES / NOTICE):
//
// Copyright 2014 The PDFium Authors
//
// Redistribution and use in source and binary forms, with or without
// modification, are permitted provided that the following conditions are met:
//
// * Redistributions of source code must retain the above copyright
//   notice, this list of conditions and the following disclaimer.
// * Redistributions in binary form must reproduce the above
//   copyright notice, this list of conditions and the following disclaimer
//   in the documentation and/or other materials provided with the
//   distribution.
// * Neither the name of Google Inc. nor the names of its
//   contributors may be used to endorse or promote products derived from
//   this software without specific prior written permission.
//
// THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS
// "AS IS" AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT
// LIMITED TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR
// A PARTICULAR PURPOSE ARE DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT
// OWNER OR CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL,
// SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT
// LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES; LOSS OF USE,
// DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY
// THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT
// (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE
// OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
//
// The PDFium-derived portions are available under the Apache License, Version
// 2.0 (http://www.apache.org/licenses/LICENSE-2.0). See THIRD_PARTY_LICENSES
// for the full license text.

//! Standalone Rust implementation of PDFium's `CPDF_TextPage` analytical algorithms.
//!
//! Re-implements the core mathematical transformations (ISO 32000 §8.3.3 & §9.4)
//! and character/word/line spatial clustering heuristics from PDFium
//! (`core/fpdftext/cpdf_textpage.cpp`). This is a derivative work of PDFium and
//! is distributed with the required Apache-2.0 / BSD-3-Clause attribution above.
//!
//! Zero dependencies on external C/C++ runtimes or GUI rasterizers. 100% Pure Safe Rust.

use super::*;

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
