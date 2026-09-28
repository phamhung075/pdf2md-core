// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! High-performance geometry-based text layout reconstruction and semantic AST pipeline.

pub mod ast;
pub mod glyph_stream;
pub mod latex_math;
pub mod reading_order;
pub mod semantic;
pub mod skew;
pub mod struct_tree;
pub mod tables;
pub mod xy_cut;

pub use ast::{AstNode, LayoutAST};
pub use glyph_stream::{
    build_lines, extract_page_glyphs, page_glyph_spans, page_height_of, PageSpans, Span,
};
pub(crate) use glyph_stream::{mtx_from, num, Mtx};
pub use latex_math::{
    detect_fractions, math_inline_for_line, render_math, render_math_line, spans_from_textline,
    synthesize_block_text, synthesize_line_expr, synthesize_spans_math, FractionHit, LatexExpr,
    RuleSeg, ScriptHit, ScriptKind,
};
pub use reading_order::{
    build_doc_blocks, is_page_number_line, page_read_order, page_two_columns, render_cluster,
    render_human_order, render_line_text, split_row_columns, DocBlock, PageColumns,
};
pub use semantic::{classify_semantic_blocks, detect_heading, detect_list_item};
pub use skew::{
    correct_skew, deskew_lines, deskew_spans, estimate_skew_angle_deg,
    estimate_skew_angle_deg_from_spans, estimate_skew_angle_deg_with, MIN_SKEW_TO_CORRECT_DEG,
};
pub use struct_tree::{extract_tagged_page, has_struct_tree, TaggedPage};
pub use tables::{
    extract_bordered_tables, extract_borderless_tables, extract_tables, find_gap_tables,
    find_tables, recover_borderless_tables, recover_tables_with_grid, render_with_tables,
    scan_aligned_grids, table_rulers, RowInfo, TableHit, WordTok,
};
pub use xy_cut::{
    compute_document_statistics, recursive_xy_cut, DocumentStatistics, LineSegment,
    StructuredTable, TableCell, XyCutOptions,
};

use crate::cpdf_textpage::{CharInfo, ClusterConfig, SpatialClusterer, TextLine};

/// Principal layout engine coordinating borderless table extraction, dynamic XY-Cut++,
/// and semantic role classification.
pub struct ModernLayoutEngine {
    pub xy_options: XyCutOptions,
    /// Synthesize LaTeX math AST for fraction / super-subscript patterns in the
    /// emitted node text (on by default; set `false` for plain output).
    pub detect_math: bool,
}

impl Default for ModernLayoutEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl ModernLayoutEngine {
    pub fn new() -> Self {
        Self {
            xy_options: XyCutOptions::default(),
            detect_math: true,
        }
    }

    pub fn with_options(xy_options: XyCutOptions) -> Self {
        Self {
            xy_options,
            detect_math: true,
        }
    }

    /// Enable LaTeX math AST synthesis (fractions + simple super/subscripts).
    pub fn with_math(self) -> Self {
        Self {
            detect_math: true,
            ..self
        }
    }

    /// Disable LaTeX math AST synthesis (plain text output).
    pub fn without_math(self) -> Self {
        Self {
            detect_math: false,
            ..self
        }
    }

    pub fn analyze_lines(&self, lines: &[TextLine]) -> LayoutAST {
        if lines.is_empty() {
            return LayoutAST::new(Vec::new());
        }

        // 1. Recover Borderless / Canvas Tables
        let (tables, remaining_lines) = recover_borderless_tables(lines);

        if remaining_lines.is_empty() {
            let nodes = tables.into_iter().map(AstNode::Table).collect();
            return LayoutAST::new(nodes);
        }

        // 2. Statistical Metric Extraction
        let stats = compute_document_statistics(&remaining_lines);

        // 2.5 Lightweight Hough skew correction.
        // XY-Cut partitions on axis-aligned projection profiles, so a page
        // tilted by even a couple of degrees smears the horizontal/vertical
        // valleys. Estimate the tilt from the glyph baselines (a Hough
        // line-angle vote over sparse points) and rotate the bounding boxes
        // back onto the page axes before cutting. On an already-aligned page
        // the estimator returns exactly 0° and the lines are moved through
        // untouched (no clone).
        let skew_deg = estimate_skew_angle_deg(&remaining_lines);
        let effective_lines: Vec<TextLine> = if skew_deg.abs() >= MIN_SKEW_TO_CORRECT_DEG {
            deskew_lines(&remaining_lines, skew_deg)
        } else {
            remaining_lines
        };

        // 3. Dynamic Recursive XY-Cut++ Segmentation
        let xy_options = if self.xy_options.min_horizontal_gap > 0.0 && self.xy_options.min_vertical_gap > 0.0 {
            self.xy_options.clone()
        } else {
            XyCutOptions {
                min_horizontal_gap: (stats.median_font_size * 1.3).max(12.0),
                min_vertical_gap: (stats.median_font_size * 1.8).max(18.0),
            }
        };
        let blocks = recursive_xy_cut(&effective_lines, &xy_options, stats.median_font_size);

        // 4. Semantic Hierarchy Classification
        let text_nodes = classify_semantic_blocks(&blocks, &stats, self.detect_math);

        // 5. Merge Tables and Text Blocks in reading order (top-to-bottom)
        let mut all_elements: Vec<(f64, AstNode)> = Vec::new();
        for t in tables {
            let y_top = t.bbox.y1.max(t.bbox.y0);
            all_elements.push((y_top, AstNode::Table(t)));
        }
        for (y_top, node) in text_nodes {
            all_elements.push((y_top, node));
        }

        all_elements.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

        let final_nodes = all_elements.into_iter().map(|(_, n)| n).collect();
        LayoutAST::new(final_nodes)
    }

    pub fn analyze_char_stream(&self, chars: &[CharInfo]) -> LayoutAST {
        let clusterer = SpatialClusterer::new(ClusterConfig::default());
        let lines = clusterer.cluster_into_lines(chars);
        self.analyze_lines(&lines)
    }
}

/// Principal layout analysis entrypoint:
/// Converts `TextLine` tokens emitted by `cpdf_textpage` into a hierarchical Semantic AST (`LayoutAST`).
pub fn analyze_layout(lines: &[TextLine]) -> LayoutAST {
    let engine = ModernLayoutEngine::new();
    engine.analyze_lines(lines)
}

/// Convenience entrypoint taking a raw `CharInfo` slice directly.
pub fn analyze_char_stream(chars: &[CharInfo]) -> LayoutAST {
    let engine = ModernLayoutEngine::new();
    engine.analyze_char_stream(chars)
}

/// Multi-page layout analysis leveraging Rayon for parallel batch processing across pages.
pub fn analyze_pages_parallel(pages_lines: &[Vec<TextLine>]) -> Vec<LayoutAST> {
    use rayon::prelude::*;
    pages_lines
        .par_iter()
        .map(|lines| analyze_layout(lines))
        .collect()
}

#[cfg(test)]
mod table_detection_tests;


#[cfg(test)]
mod modern_layout_and_table_tests;
