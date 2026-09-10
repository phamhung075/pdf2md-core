// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Core domain models, bounding box primitives, and conversion options/results.

use serde::{Deserialize, Serialize};

/// 2D Bounding Box in PDF coordinate space (points).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BoundingBox {
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
}

impl BoundingBox {
    pub fn new(x0: f64, y0: f64, x1: f64, y1: f64) -> Self {
        Self { x0, y0, x1, y1 }
    }

    pub fn intersects(&self, other: &BoundingBox) -> bool {
        self.x0 < other.x1 && self.x1 > other.x0 && self.y0 < other.y1 && self.y1 > other.y0
    }

    pub fn width(&self) -> f64 {
        (self.x1 - self.x0).abs()
    }

    pub fn height(&self) -> f64 {
        (self.y1 - self.y0).abs()
    }
}

/// Extracted text span with spatial coordinates.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TextSpan {
    pub text: String,
    pub bbox: BoundingBox,
    pub font_size: f64,
    pub is_bold: bool,
    #[serde(default)]
    pub is_italic: bool,
    #[serde(default)]
    pub is_underline: bool,
    pub page_number: usize,
}

/// Column text alignment for tabular data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ColumnAlignment {
    Left,
    Center,
    Right,
}

/// Reconstructed 2D table grid from text positions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CanvasTable {
    pub rows: Vec<Vec<String>>,
    pub bbox: BoundingBox,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alignments: Option<Vec<ColumnAlignment>>,
}

impl CanvasTable {
    pub fn new(rows: Vec<Vec<String>>, bbox: BoundingBox) -> Self {
        Self {
            rows,
            bbox,
            alignments: None,
        }
    }

    pub fn with_alignments(
        rows: Vec<Vec<String>>,
        bbox: BoundingBox,
        alignments: Vec<ColumnAlignment>,
    ) -> Self {
        Self {
            rows,
            bbox,
            alignments: Some(alignments),
        }
    }

    /// Escapes a cell for GFM pipe tables: `|` and `\` must be backslash
    /// escaped, newlines flattened (a cell must stay on one physical row).
    fn md_cell(raw: &str) -> String {
        let v = raw.trim();
        if v.is_empty() {
            return " ".to_string();
        }
        let mut s = String::with_capacity(v.len() + 4);
        for ch in v.chars() {
            match ch {
                '|' => s.push_str("\\|"),
                '\\' => s.push_str("\\\\"),
                '\n' | '\r' => s.push(' '),
                _ => s.push(ch),
            }
        }
        s
    }

    /// Renders the reconstructed table into standard GitHub Flavored Markdown (GFM) pipe table.
    pub fn to_markdown(&self) -> String {
        if self.rows.is_empty() {
            return String::new();
        }

        let num_cols = self.rows.iter().map(|r| r.len()).max().unwrap_or(0);
        if num_cols == 0 {
            return String::new();
        }

        let mut md = String::new();

        // Header row (row 0 or synthesized)
        let header = &self.rows[0];
        md.push('|');
        for c in 0..num_cols {
            let val = header.get(c).map(|s| s.as_str()).unwrap_or("");
            md.push_str(&format!(" {} |", Self::md_cell(val)));
        }
        md.push('\n');

        // Separator row
        md.push('|');
        for c in 0..num_cols {
            if let Some(ref aligns) = self.alignments {
                match aligns.get(c) {
                    Some(ColumnAlignment::Right) => md.push_str(" ---: |"),
                    Some(ColumnAlignment::Center) => md.push_str(" :---: |"),
                    Some(ColumnAlignment::Left) => md.push_str(" :--- |"),
                    None => md.push_str(" --- |"),
                }
            } else {
                md.push_str(" --- |");
            }
        }
        md.push('\n');

        // Data rows
        for row in self.rows.iter().skip(1) {
            md.push('|');
            for c in 0..num_cols {
                let val = row.get(c).map(|s| s.as_str()).unwrap_or("");
                md.push_str(&format!(" {} |", Self::md_cell(val)));
            }
            md.push('\n');
        }

        md
    }
}

/// Conversion and parsing options.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversionOptions {
    pub detect_tables: bool,
    pub detect_headings: bool,
    pub min_words_per_page: usize,
    /// Extract placed images and return them as base64 media items.
    pub detect_media: bool,
    /// Rebuild reading order with zones/columns/furniture handling on the
    /// geometry path (fallback is byte-identical for simple single-column
    /// pages).
    pub detect_layout: bool,
    /// Embed extracted, non-decorative images into the markdown itself as
    /// self-contained data-URI lines (placed top-to-bottom per page). When
    /// false, images are only returned in the `media` JSON list.
    pub embed_media: bool,
    /// Detect pure-vector figure regions (charts/diagrams/logos drawn with
    /// paths, no raster) and cut them out as standalone clipped PDFs.
    pub detect_vectors: bool,
    /// Synthesize a LaTeX math AST from 2D glyph geometry and emit built-up
    /// fractions (`$\frac{a}{b}$`) and simple super/subscripts (`$x^{2}$`,
    /// `$a_{i}$`) as inline LaTeX before export. On by default; set to `false`
    /// for byte-identical plain extraction (no `$...$` synthesis).
    pub detect_math: bool,
}

impl Default for ConversionOptions {
    fn default() -> Self {
        Self {
            detect_tables: true,
            detect_headings: true,
            min_words_per_page: 5,
            detect_media: true,
            detect_layout: true,
            embed_media: true,
            detect_vectors: false,
            detect_math: true,
        }
    }
}

/// Conversion result summary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversionResult {
    pub markdown: String,
    pub total_pages: usize,
    pub total_words: usize,
    pub tables_detected: usize,
    pub duration_us: u64,
    /// Extracted image placements (base64 payloads) when `detect_media`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub media: Vec<crate::media::MediaItem>,
    /// Structured reading-order blocks for pages handled by the geometry
    /// layout engine.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocks: Vec<crate::layout::DocBlock>,
}
