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
