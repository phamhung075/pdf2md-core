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
pub use glyph_stream::{build_lines, extract_page_glyphs, page_height_of, Span};
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
mod table_detection_tests {
    use super::*;
    use lopdf::{Dictionary, Document, Object};
    use crate::layout::glyph_stream::resolve_font_style;

    /// Build one visual line from (start_x, text) word spans on a shared
    /// baseline `y` (page coordinates: larger y = higher on the page).
    fn row(y: f64, words: &[(f64, &str)]) -> Vec<Span> {
        words
            .iter()
            .map(|(x, t)| Span {
                text: t.to_string(),
                x: *x,
                y,
                size: 10.0,
                advance: 0.0,
                is_bold: false,
                is_italic: false,
                is_underline: false,
                is_vertical: false,
            })
            .collect()
    }

    fn page(rows: Vec<Vec<Span>>) -> Vec<Vec<Span>> {
        rows
    }

    /// A left-aligned word at `x0` (fixed start, like `row`'s words) next to
    /// a right-aligned word whose `x0` is back-computed from `right_x1` so
    /// its *end* (`x + advance`) lands exactly there — unlike `row`, `advance`
    /// is a real synthetic character width so `x1` is meaningful, letting a
    /// test line up a numeric column by its right edge instead of its start.
    fn left_and_right_aligned_row(y: f64, left_x0: f64, left: &str, right_x1: f64, right: &str) -> Vec<Span> {
        let size = 10.0;
        let right_advance = right.chars().count() as f64 * size * 0.55;
        vec![
            Span {
                text: left.to_string(),
                x: left_x0,
                y,
                size,
                advance: left.chars().count() as f64 * size * 0.55,
                is_bold: false,
                is_italic: false,
                is_underline: false,
                is_vertical: false,
            },
            Span {
                text: right.to_string(),
                x: right_x1 - right_advance,
                y,
                size,
                advance: right_advance,
                is_bold: false,
                is_italic: false,
                is_underline: false,
                is_vertical: false,
            },
        ]
    }

    #[test]
    fn right_aligned_numeric_column_is_recovered_by_its_end_not_its_start() {
        // Three amounts of different digit counts — "9,20 €", "145,50 €",
        // "1 200,00 €" — so their x0 (word start) scatters across the page,
        // but they all right-align to the same column edge (x1 = 250.0). A
        // start-only ruler scan finds no common x0 here and drops the whole
        // column; only the end-based (x1) clustering recovers it.
        let lines = page(vec![
            left_and_right_aligned_row(700.0, 50.0, "Frais", 250.0, "9,20 €"),
            left_and_right_aligned_row(690.0, 50.0, "Abonnement", 250.0, "145,50 €"),
            left_and_right_aligned_row(680.0, 50.0, "Consommation", 250.0, "1 200,00 €"),
        ]);

        // Every row's amount has a distinct x0 — the exact case a start-only
        // scan cannot cluster.
        let x0s: std::collections::HashSet<i64> = lines
            .iter()
            .map(|l| (l[1].x * 100.0).round() as i64)
            .collect();
        assert_eq!(x0s.len(), 3, "fixture must actually vary x0 across rows");

        let hits = find_tables(&lines);
        assert_eq!(hits.len(), 1, "right-aligned amount column must be recovered as a table");
        let rows = &hits[0].rows;
        assert_eq!(rows.len(), 3, "all 3 rows expected: {rows:?}");
        assert_eq!(rows[0][0].trim(), "Frais");
        assert_eq!(rows[0][1].trim(), "9,20 €");
        assert_eq!(rows[1][1].trim(), "145,50 €");
        assert_eq!(rows[2][1].trim(), "1 200,00 €");
    }

    #[test]
    fn aligned_two_column_grid_is_detected() {
        let lines = page(vec![
            row(700.0, &[(50.0, "Nom"), (300.0, "DUPONT")]),
            row(690.0, &[(50.0, "Prenom"), (300.0, "Marie")]),
            row(680.0, &[(50.0, "Date"), (300.0, "1985")]),
            row(670.0, &[(50.0, "Pays"), (300.0, "France")]),
        ]);
        let hits = find_tables(&lines);
        assert_eq!(hits.len(), 1, "aligned 2-col grid must be recovered");
        let rows: Vec<Vec<String>> = hits[0].rows.clone();
        assert_eq!(rows.len(), 4, "4 rows expected: {rows:?}");
        assert_eq!(rows[0][0].as_str(), "Nom");
        assert_eq!(rows[0][1].as_str(), "DUPONT");
    }

    #[test]
    fn toc_dot_leader_rows_are_rejected() {
        let lines = page(vec![
            row(700.0, &[(50.0, "1.1"), (300.0, "....."), (420.0, "5")]),
            row(690.0, &[(50.0, "1.2"), (300.0, "....."), (420.0, "6")]),
            row(680.0, &[(50.0, "2.1"), (300.0, "....."), (420.0, "9")]),
            row(670.0, &[(50.0, "2.2"), (300.0, "....."), (420.0, "12")]),
        ]);
        assert!(
            find_tables(&lines).is_empty(),
            "TOC dot leaders must not be tabled"
        );
        assert!(
            find_gap_tables(&lines, &[]).is_empty(),
            "TOC dot leaders must not be tabled (3b)"
        );
    }

    #[test]
    fn aligned_long_prose_columns_are_rejected() {
        let lines = page(vec![
            row(
                700.0,
                &[
                    (50.0, "The quick brown fox jumps over the lazy dog"),
                    (300.0, "second column of equally long words lives"),
                ],
            ),
            row(
                690.0,
                &[
                    (50.0, "Another quite long sentence to fill the first column"),
                    (300.0, "and the right hand column keeps on flowing too"),
                ],
            ),
            row(
                680.0,
                &[
                    (50.0, "A third verbose paragraph placed under the two above"),
                    (300.0, "with prose that keeps reading like a document"),
                ],
            ),
        ]);
        assert!(
            find_tables(&lines).is_empty(),
            "aligned prose must not be tabled"
        );
    }

    #[test]
    fn bullet_marker_column_is_rejected() {
        let lines = page(vec![
            row(700.0, &[(50.0, "-"), (80.0, "option one")]),
            row(690.0, &[(50.0, "-"), (80.0, "option two")]),
            row(680.0, &[(50.0, "-"), (80.0, "option three")]),
            row(670.0, &[(50.0, "-"), (80.0, "option four")]),
        ]);
        assert!(
            find_tables(&lines).is_empty(),
            "bullet marker column must not be tabled"
        );
    }

    #[test]
    fn jittered_grid_is_recovered_by_stage3b_only() {
        let lines = page(vec![
            row(700.0, &[(50.0, "A1"), (300.0, "B1")]),
            row(690.0, &[(50.0, "A2"), (300.8, "B2")]),
            row(680.0, &[(50.0, "A3"), (299.6, "B3")]),
            row(670.0, &[(50.0, "A4"), (300.4, "B4")]),
        ]);
        assert!(
            find_tables(&lines).is_empty(),
            "strict pass must miss the jittered grid"
        );
        let gap = find_gap_tables(&lines, &[]);
        assert_eq!(gap.len(), 1, "Stage-3b must recover the jittered grid");
        let rows: Vec<Vec<String>> = gap[0].rows.clone();
        assert!(rows[0].iter().any(|c| c == "B1"), "cells wrong: {rows:?}");
    }

    #[test]
    fn docblock_bounding_box_has_positive_height_and_preserves_style() {
        let spans = vec![Span {
            text: "Bold Section Heading".to_string(),
            x: 100.0,
            y: 750.0,
            size: 20.0,
            advance: 150.0,
            is_bold: true,
            is_italic: false,
            is_underline: false,
            is_vertical: false,
        }];
        let lines = build_lines(&spans);
        let blocks = build_doc_blocks(&lines, 842.0);
        assert_eq!(blocks.len(), 1);
        let block = &blocks[0];
        assert_eq!(block.text, "**Bold Section Heading**", "Bold run must be wrapped in **emphasis**");
        assert!(block.is_bold, "Block must retain bold flag");
        assert!(!block.is_italic, "Block must retain italic flag");
        assert!(
            block.y1 > block.y0,
            "Block height must be positive: y0={}, y1={}",
            block.y0,
            block.y1
        );
        assert_eq!(block.y1 - block.y0, 20.0, "Block height should match typographic size");
    }

    #[test]
    fn resolve_font_style_detects_bold_and_italic() {
        let doc = Document::new();
        let mut normal_dict = Dictionary::new();
        normal_dict.set(b"BaseFont", Object::Name(b"Helvetica".to_vec()));
        assert_eq!(resolve_font_style(&doc, &normal_dict), (false, false));

        let mut bold_dict = Dictionary::new();
        bold_dict.set(b"BaseFont", Object::Name(b"Helvetica-Bold".to_vec()));
        assert_eq!(resolve_font_style(&doc, &bold_dict), (true, false));

        let mut italic_dict = Dictionary::new();
        italic_dict.set(b"BaseFont", Object::Name(b"TimesNewRoman,Italic".to_vec()));
        assert_eq!(resolve_font_style(&doc, &italic_dict), (false, true));

        let mut bold_italic_dict = Dictionary::new();
        bold_italic_dict.set(b"BaseFont", Object::Name(b"Arial-BoldItalicMT".to_vec()));
        assert_eq!(resolve_font_style(&doc, &bold_italic_dict), (true, true));
    }
}

#[cfg(test)]
mod modern_layout_and_table_tests {
    use super::*;
    use crate::cpdf_textpage::{Matrix3x3, Rect, TextWord};
    use crate::models::ColumnAlignment;

    fn make_word(text: &str, min_x: f64, max_x: f64, min_y: f64, max_y: f64, is_bold: bool, fs: f64) -> TextWord {
        let bbox = Rect::new(min_x, min_y, max_x, max_y);
        let count = text.chars().count();
        let char_w = if count == 0 { 0.0 } else { (max_x - min_x) / count as f64 };
        let mut chars = Vec::new();
        for (i, ch) in text.chars().enumerate() {
            chars.push(CharInfo {
                unicode: ch,
                bbox: Rect::new(min_x + i as f64 * char_w, min_y, min_x + (i + 1) as f64 * char_w, max_y),
                font_size: fs,
                matrix: Matrix3x3::IDENTITY,
                origin: (min_x + i as f64 * char_w, min_y),
                advance_width: char_w,
                is_bold,
                is_italic: false,
            });
        }
        TextWord {
            text: text.to_string(),
            word_bbox: bbox,
            chars,
        }
    }

    fn make_line(words: Vec<TextWord>) -> TextLine {
        let mut line_bbox = words[0].word_bbox;
        let mut text = String::new();
        let mut chars = Vec::new();

        for (i, w) in words.iter().enumerate() {
            if i > 0 {
                text.push(' ');
            }
            text.push_str(&w.text);
            line_bbox = line_bbox.union_with(&w.word_bbox);
            chars.extend(w.chars.clone());
        }

        TextLine {
            line_bbox,
            words,
            text,
            chars,
            baseline: line_bbox.min_y,
        }
    }

    #[test]
    fn test_borderless_financial_balance_sheet() {
        let row0 = make_line(vec![
            make_word("Asset Category", 50.0, 150.0, 700.0, 712.0, true, 10.0),
            make_word("Q1 2025", 220.0, 270.0, 700.0, 712.0, true, 10.0),
            make_word("Q2 2025", 320.0, 370.0, 700.0, 712.0, true, 10.0),
            make_word("Q3 2025", 420.0, 470.0, 700.0, 712.0, true, 10.0),
        ]);

        let row1 = make_line(vec![
            make_word("Cash & Equivalents", 50.0, 160.0, 670.0, 682.0, false, 10.0),
            make_word("$12,450.00", 200.0, 270.0, 670.0, 682.0, false, 10.0),
            make_word("$14,200.50", 300.0, 370.0, 670.0, 682.0, false, 10.0),
            make_word("$15,800.00", 400.0, 470.0, 670.0, 682.0, false, 10.0),
        ]);

        let row2 = make_line(vec![
            make_word("Accounts Receivable", 50.0, 170.0, 650.0, 662.0, false, 10.0),
            make_word("$8,320.00", 208.0, 270.0, 650.0, 662.0, false, 10.0),
            make_word("$7,910.25", 308.0, 370.0, 650.0, 662.0, false, 10.0),
            make_word("$9,150.75", 406.0, 470.0, 650.0, 662.0, false, 10.0),
        ]);

        let row3 = make_line(vec![
            make_word("Total Assets", 50.0, 130.0, 630.0, 642.0, true, 10.0),
            make_word("$20,770.00", 198.0, 270.0, 630.0, 642.0, true, 10.0),
            make_word("$22,110.75", 296.0, 370.0, 630.0, 642.0, true, 10.0),
            make_word("$24,950.75", 396.0, 470.0, 630.0, 642.0, true, 10.0),
        ]);

        let lines = vec![row0, row1, row2, row3];
        let tables = extract_tables(&lines, &[]);

        assert_eq!(tables.len(), 1, "Must detect exactly 1 borderless canvas table");
        let table = &tables[0];
        assert_eq!(table.rows.len(), 4, "Must extract 4 rows");
        assert!(table.rows.iter().all(|r| r.len() == 4), "Every row must have exactly 4 columns");

        assert_eq!(table.rows[0], vec!["Asset Category", "Q1 2025", "Q2 2025", "Q3 2025"]);
        assert_eq!(table.rows[1], vec!["Cash & Equivalents", "$12,450.00", "$14,200.50", "$15,800.00"]);
        assert_eq!(table.rows[2], vec!["Accounts Receivable", "$8,320.00", "$7,910.25", "$9,150.75"]);
        assert_eq!(table.rows[3], vec!["Total Assets", "$20,770.00", "$22,110.75", "$24,950.75"]);

        let aligns = table.alignments.as_ref().expect("Alignments must be computed");
        assert_eq!(aligns[0], ColumnAlignment::Left, "Col 0 must be left-aligned");
        assert_eq!(aligns[1], ColumnAlignment::Right, "Col 1 must be right-aligned");
        assert_eq!(aligns[2], ColumnAlignment::Right, "Col 2 must be right-aligned");
        assert_eq!(aligns[3], ColumnAlignment::Right, "Col 3 must be right-aligned");

        let md = table.to_markdown();
        assert!(md.contains("| :--- | ---: | ---: | ---: |"), "Markdown must have right-aligned separator: {}", md);
        assert!(md.contains("$24,950.75"), "Must preserve currency and decimals: {}", md);
    }

    #[test]
    fn test_bordered_invoice_summary_with_merged_headers() {
        let vector_lines = vec![
            LineSegment::new(50.0, 500.0, 450.0, 500.0),
            LineSegment::new(50.0, 450.0, 450.0, 450.0),
            LineSegment::new(50.0, 400.0, 450.0, 400.0),
            LineSegment::new(50.0, 350.0, 450.0, 350.0),
            LineSegment::new(50.0, 350.0, 50.0, 500.0),
            LineSegment::new(200.0, 350.0, 200.0, 450.0),
            LineSegment::new(320.0, 350.0, 320.0, 500.0),
            LineSegment::new(450.0, 350.0, 450.0, 500.0),
        ];

        let header_line = make_line(vec![
            make_word("Item Description & SKU", 60.0, 250.0, 465.0, 480.0, true, 11.0),
            make_word("Amount", 340.0, 390.0, 465.0, 480.0, true, 11.0),
        ]);

        let data1_line = make_line(vec![
            make_word("High-Performance Core", 60.0, 180.0, 415.0, 430.0, false, 10.0),
            make_word("SKU-881", 210.0, 260.0, 415.0, 430.0, false, 10.0),
            make_word("$1,200.00", 350.0, 410.0, 415.0, 430.0, false, 10.0),
        ]);

        let data2_line = make_line(vec![
            make_word("Obsidian Plugin WASM", 60.0, 180.0, 365.0, 380.0, false, 10.0),
            make_word("SKU-992", 210.0, 260.0, 365.0, 380.0, false, 10.0),
            make_word("$850.00", 360.0, 410.0, 365.0, 380.0, false, 10.0),
        ]);

        let lines = vec![header_line, data1_line, data2_line];
        let tables = extract_tables(&lines, &vector_lines);

        assert_eq!(tables.len(), 1, "Must extract 1 bordered table via vector intersections");
        let table = &tables[0];
        assert_eq!(table.rows.len(), 3, "Table must have 3 rows");
        assert_eq!(table.rows[0].len(), 3, "Row 0 must normalize to 3 columns");
        assert_eq!(table.rows[0][0], "Item Description & SKU");
        assert_eq!(table.rows[0][1], "", "Spanned column must be empty for GFM alignment");
        assert_eq!(table.rows[0][2], "Amount");

        assert_eq!(table.rows[1], vec!["High-Performance Core", "SKU-881", "$1,200.00"]);
        assert_eq!(table.rows[2], vec!["Obsidian Plugin WASM", "SKU-992", "$850.00"]);

        let md = table.to_markdown();
        assert!(md.contains("| Item Description & SKU |") && md.contains("| Amount |"), "Markdown must render merged header: {}", md);
        assert!(md.contains("$1,200.00"), "Numbers must be preserved: {}", md);
    }

    #[test]
    fn test_recursive_xy_cut_mixed_columns() {
        let title = make_line(vec![
            make_word("High-Performance Document Layout", 50.0, 450.0, 750.0, 770.0, true, 18.0),
        ]);

        let col1_l1 = make_line(vec![
            make_word("Left column paragraph one sentence.", 50.0, 220.0, 580.0, 592.0, false, 10.0),
        ]);
        let col1_l2 = make_line(vec![
            make_word("Left column paragraph second line.", 50.0, 215.0, 560.0, 572.0, false, 10.0),
        ]);

        let col2_l1 = make_line(vec![
            make_word("Right column first sentence here.", 300.0, 470.0, 578.0, 590.0, false, 10.0),
        ]);
        let col2_l2 = make_line(vec![
            make_word("Right column second sentence here.", 300.0, 468.0, 558.0, 570.0, false, 10.0),
        ]);

        let footer = make_line(vec![
            make_word("Copyright 2026 Dai Hung PHAM. All rights reserved.", 50.0, 450.0, 350.0, 362.0, false, 9.0),
        ]);

        let lines = vec![title, col1_l1, col1_l2, col2_l1, col2_l2, footer];
        let ast = analyze_layout(&lines);

        assert_eq!(ast.nodes.len(), 4, "Should detect Title, Left Col, Right Col, Footer: {:?}", ast.nodes);
        match &ast.nodes[0] {
            AstNode::Heading { level, text } => {
                assert_eq!(*level, 1);
                assert!(text.contains("High-Performance Document Layout"));
            }
            other => panic!("Expected Heading, got {:?}", other),
        }

        match &ast.nodes[1] {
            AstNode::Paragraph { text } => {
                assert!(text.contains("Left column paragraph"), "Left column must come first: {}", text);
            }
            other => panic!("Expected Left column Paragraph, got {:?}", other),
        }

        match &ast.nodes[2] {
            AstNode::Paragraph { text } => {
                assert!(text.contains("Right column first sentence"), "Right column must come second: {}", text);
            }
            other => panic!("Expected Right column Paragraph, got {:?}", other),
        }

        match &ast.nodes[3] {
            AstNode::Paragraph { text } => {
                assert!(text.contains("Copyright 2026"), "Footer must come last: {}", text);
            }
            other => panic!("Expected Footer Paragraph, got {:?}", other),
        }
    }

    #[test]
    fn test_statistical_heading_and_list_classifier() {
        let h1 = make_line(vec![
            make_word("Primary Document Heading", 50.0, 300.0, 700.0, 720.0, true, 18.0),
        ]);
        let h2 = make_line(vec![
            make_word("Secondary Section Heading", 50.0, 250.0, 650.0, 665.0, false, 14.0),
        ]);
        let item1 = make_line(vec![
            make_word("-", 50.0, 55.0, 600.0, 610.0, false, 10.0),
            make_word("First unordered feature item", 65.0, 250.0, 600.0, 610.0, false, 10.0),
        ]);
        let item2 = make_line(vec![
            make_word("-", 70.0, 75.0, 580.0, 590.0, false, 10.0),
            make_word("Nested sub-item with extra detail", 85.0, 270.0, 580.0, 590.0, false, 10.0),
        ]);
        let ordered1 = make_line(vec![
            make_word("1.", 50.0, 60.0, 540.0, 550.0, false, 10.0),
            make_word("First step in the procedure", 70.0, 250.0, 540.0, 550.0, false, 10.0),
        ]);
        let task1 = make_line(vec![
            make_word("[ ]", 50.0, 65.0, 500.0, 510.0, false, 10.0),
            make_word("Pending verification task", 75.0, 250.0, 500.0, 510.0, false, 10.0),
        ]);

        let body1 = make_line(vec![
            make_word("Standard paragraph text line for baseline metrics.", 50.0, 350.0, 450.0, 460.0, false, 10.0),
        ]);
        let body2 = make_line(vec![
            make_word("Second paragraph text line with normal font size.", 50.0, 340.0, 435.0, 445.0, false, 10.0),
        ]);

        let lines = vec![h1, h2, item1, item2, ordered1, task1, body1, body2];
        let ast = analyze_layout(&lines);
        let md = ast.to_markdown();

        assert!(md.contains("# Primary Document Heading"), "Must format H1: {}", md);
        assert!(md.contains("## Secondary Section Heading"), "Must format H2: {}", md);
        assert!(md.contains("- First unordered feature item"), "Must format unordered list: {}", md);
        assert!(md.contains("  - Nested sub-item with extra detail"), "Must format indented list: {}", md);
        assert!(md.contains("1. First step in the procedure"), "Must format ordered list: {}", md);
        assert!(md.contains("- [ ] Pending verification task"), "Must format task list: {}", md);
    }
}
