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

    /// Regression for the block-order panic: the old comparator mixed a
    /// per-pair column test with a same-column y comparison, which is not
    /// transitive — a wide block `A` bridges a narrow left block `B` and a
    /// right block `C`, giving `A<B`, `B<C`, `C<A`. On real pages that made
    /// `sort_by` panic with "comparison function does not correctly implement a
    /// total order". Thirty blocks is enough to reach the sort's consistency
    /// check; the fixed column partition must order them without panicking.
    #[test]
    fn test_block_ordering_is_total_order() {
        let clusterer = SpatialClusterer::new(ClusterConfig::default());
        let font_size = 12.0;

        let make_line = |text: &str, bbox: Rect, baseline: f64| -> TextLine {
            let chars = vec![CharInfo {
                unicode: 'x',
                bbox,
                font_size,
                matrix: Matrix3x3::translation(bbox.min_x, baseline),
                origin: (bbox.min_x, baseline),
                advance_width: bbox.width(),
                is_bold: false,
                is_italic: false,
            }];
            TextLine {
                chars,
                words: vec![],
                baseline,
                line_bbox: bbox,
                text: text.to_string(),
            }
        };

        // A overlaps B and C in x; B is clear of C by more than the gutter.
        // Distinct baselines keep every line its own block.
        let mut lines = Vec::new();
        for k in 0..10 {
            let off = k as f64 * 1000.0;
            lines.push(make_line("A", Rect::new(90.0, 510.0 + off, 200.0, 520.0 + off), 510.0 + off));
            lines.push(make_line("B", Rect::new(100.0, 400.0 + off, 120.0, 410.0 + off), 400.0 + off));
            lines.push(make_line("C", Rect::new(150.0, 590.0 + off, 250.0, 600.0 + off), 590.0 + off));
        }

        let blocks = clusterer.cluster_into_blocks(&lines);

        assert_eq!(blocks.len(), 30, "each crafted line must form its own block");

        // A single deterministic column results from A bridging the gutter, so
        // the total order is (descending max_y, then ascending min_x).
        for pair in blocks.windows(2) {
            let (a, b) = (&pair[0].block_bbox, &pair[1].block_bbox);
            assert!(
                a.max_y > b.max_y || (a.max_y == b.max_y && a.min_x <= b.min_x),
                "block order is not (desc max_y, asc min_x): {:?} then {:?}",
                a,
                b
            );
        }
    }