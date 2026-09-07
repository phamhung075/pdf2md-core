// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Recursive XY-Cut++ spatial layout partitioning & valley projection profiles.

use serde::{Deserialize, Serialize};
use crate::cpdf_textpage::{Rect, TextBlock, TextLine};
use crate::models::{BoundingBox, CanvasTable, ColumnAlignment};

/// 2D vector line segment from PDF graphics path (points).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LineSegment {
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
}

impl LineSegment {
    pub fn new(x0: f64, y0: f64, x1: f64, y1: f64) -> Self {
        Self { x0, y0, x1, y1 }
    }

    #[inline]
    pub fn is_horizontal(&self, tolerance: f64) -> bool {
        (self.y0 - self.y1).abs() <= tolerance && (self.x0 - self.x1).abs() > tolerance
    }

    #[inline]
    pub fn is_vertical(&self, tolerance: f64) -> bool {
        (self.x0 - self.x1).abs() <= tolerance && (self.y0 - self.y1).abs() > tolerance
    }

    #[inline]
    pub fn length(&self) -> f64 {
        let dx = self.x1 - self.x0;
        let dy = self.y1 - self.y0;
        (dx * dx + dy * dy).sqrt()
    }

    #[inline]
    pub fn bbox(&self) -> Rect {
        Rect::new(self.x0, self.y0, self.x1, self.y1)
    }
}

/// A cell in a reconstructed table grid with merged span tracking.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TableCell {
    pub text: String,
    pub bbox: Rect,
    pub colspan: usize,
    pub rowspan: usize,
    pub alignment: ColumnAlignment,
}

/// Reconstructed structured table prior to serialization.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StructuredTable {
    pub rows: Vec<Vec<TableCell>>,
    pub column_alignments: Vec<ColumnAlignment>,
    pub bbox: Rect,
}

impl StructuredTable {
    pub fn to_canvas_table(&self) -> CanvasTable {
        let num_cols = self.column_alignments.len();
        let mut rows_matrix: Vec<Vec<String>> = Vec::with_capacity(self.rows.len());

        for row in &self.rows {
            let mut row_strings = vec![String::new(); num_cols];
            let mut col_idx = 0;
            for cell in row {
                if col_idx >= num_cols {
                    break;
                }
                row_strings[col_idx] = cell.text.clone();
                col_idx += cell.colspan.max(1);
            }
            rows_matrix.push(row_strings);
        }

        CanvasTable {
            rows: rows_matrix,
            bbox: BoundingBox::new(self.bbox.min_x, self.bbox.min_y, self.bbox.max_x, self.bbox.max_y),
            alignments: Some(self.column_alignments.clone()),
        }
    }
}

#[derive(Debug, Clone)]
pub struct XyCutOptions {
    /// Minimum horizontal valley height (pt) to partition into horizontal bands.
    pub min_horizontal_gap: f64,
    /// Minimum vertical valley width (pt) to partition into column regions.
    pub min_vertical_gap: f64,
}

impl Default for XyCutOptions {
    fn default() -> Self {
        Self {
            min_horizontal_gap: 14.0,
            min_vertical_gap: 18.0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct DocumentStatistics {
    pub median_font_size: f64,
    pub dominant_is_bold: bool,
}

/// Computes running statistical metrics per page/document without hardcoded point thresholds.
pub fn compute_document_statistics(lines: &[TextLine]) -> DocumentStatistics {
    let mut font_sizes = Vec::new();
    let mut bold_count = 0usize;
    let mut total_count = 0usize;

    for l in lines {
        for ch in &l.chars {
            font_sizes.push(ch.font_size);
            if ch.is_bold {
                bold_count += 1;
            }
            total_count += 1;
        }
    }

    font_sizes.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median = if font_sizes.is_empty() {
        12.0
    } else {
        font_sizes[font_sizes.len() / 2]
    };

    DocumentStatistics {
        median_font_size: median,
        dominant_is_bold: bold_count > total_count / 2,
    }
}

/// Recursive XY-Cut++ spatial layout partitioning.
///
/// Recursively computes dynamic projection profiles across Y (horizontal valleys)
/// and X (vertical column gutters) to naturally partition mixed-column documents
/// (e.g. 1-col title -> 2-col body -> 1-col references) into homogeneous semantic blocks
/// in strict topological reading order (Top-to-Bottom, Left-to-Right).
pub fn recursive_xy_cut(lines: &[TextLine], options: &XyCutOptions, median_fs: f64) -> Vec<TextBlock> {
    let indices: Vec<usize> = (0..lines.len()).collect();
    xy_cut_sub(lines, &indices, options, median_fs)
}

fn xy_cut_sub(lines: &[TextLine], indices: &[usize], options: &XyCutOptions, median_fs: f64) -> Vec<TextBlock> {
    if indices.is_empty() {
        return Vec::new();
    }
    if indices.len() == 1 {
        let l = &lines[indices[0]];
        return vec![TextBlock {
            lines: vec![l.clone()],
            block_bbox: l.line_bbox,
            text: l.text.clone(),
        }];
    }

    // 1. Horizontal Projection Profile (Y-axis histogram cuts)
    // Sort lines by descending top Y (PDF coordinates: larger Y is higher on page)
    let mut sorted_by_y = indices.to_vec();
    sorted_by_y.sort_by(|&a, &b| {
        lines[b].line_bbox.max_y.partial_cmp(&lines[a].line_bbox.max_y).unwrap_or(std::cmp::Ordering::Equal)
    });

    let mut h_cuts = Vec::new();
    let mut running_bottom = lines[sorted_by_y[0]].line_bbox.min_y;

    for i in 1..sorted_by_y.len() {
        let next_top = lines[sorted_by_y[i]].line_bbox.max_y;
        let gap = running_bottom - next_top;
        if gap >= options.min_horizontal_gap {
            h_cuts.push(i);
            running_bottom = lines[sorted_by_y[i]].line_bbox.min_y;
        } else {
            running_bottom = running_bottom.min(lines[sorted_by_y[i]].line_bbox.min_y);
        }
    }

    if !h_cuts.is_empty() {
        let mut blocks = Vec::new();
        let mut prev_idx = 0;
        for cut in h_cuts {
            let slice = &sorted_by_y[prev_idx..cut];
            blocks.extend(xy_cut_sub(lines, slice, options, median_fs));
            prev_idx = cut;
        }
        let slice = &sorted_by_y[prev_idx..];
        blocks.extend(xy_cut_sub(lines, slice, options, median_fs));
        return blocks;
    }

    // 2. Vertical Projection Profile (X-axis column cuts within horizontal band)
    let mut sorted_by_x = indices.to_vec();
    sorted_by_x.sort_by(|&a, &b| {
        lines[a].line_bbox.min_x.partial_cmp(&lines[b].line_bbox.min_x).unwrap_or(std::cmp::Ordering::Equal)
    });

    let mut v_cuts = Vec::new();
    let mut running_right = lines[sorted_by_x[0]].line_bbox.max_x;

    for i in 1..sorted_by_x.len() {
        let next_left = lines[sorted_by_x[i]].line_bbox.min_x;
        let gap = next_left - running_right;
        if gap >= options.min_vertical_gap {
            v_cuts.push(i);
            running_right = lines[sorted_by_x[i]].line_bbox.max_x;
        } else {
            running_right = running_right.max(lines[sorted_by_x[i]].line_bbox.max_x);
        }
    }

    if !v_cuts.is_empty() {
        let mut blocks = Vec::new();
        let mut prev_idx = 0;
        for cut in v_cuts {
            let slice = &sorted_by_x[prev_idx..cut];
            blocks.extend(xy_cut_sub(lines, slice, options, median_fs));
            prev_idx = cut;
        }
        let slice = &sorted_by_x[prev_idx..];
        blocks.extend(xy_cut_sub(lines, slice, options, median_fs));
        return blocks;
    }

    // 3. Leaf Block (Homogeneous slice)
    let mut block_lines: Vec<TextLine> = indices.iter().map(|&i| lines[i].clone()).collect();
    block_lines.sort_by(|a, b| {
        b.line_bbox.max_y.partial_cmp(&a.line_bbox.max_y).unwrap_or(std::cmp::Ordering::Equal)
    });

    let mut block_bbox = block_lines[0].line_bbox;
    let mut text = String::new();
    for (idx, l) in block_lines.iter().enumerate() {
        block_bbox = block_bbox.union_with(&l.line_bbox);
        if idx > 0 {
            text.push(' ');
        }
        text.push_str(&l.text);
    }

    vec![TextBlock {
        lines: block_lines,
        block_bbox,
        text,
    }]
}
