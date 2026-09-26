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
