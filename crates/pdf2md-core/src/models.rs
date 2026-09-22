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

    /// True when a cell holds a bare value (number, currency amount,
    /// percentage) rather than a textual label. Used to tell a genuine header
    /// row from the first row of a *headerless* label/value grid (the totals
    /// block of an invoice, a key/value summary), whose promotion to the GFM
    /// header would demote a data row and lose it from the record set.
    fn is_value_cell(cell: &str) -> bool {
        let t = cell
            .trim()
            .trim_matches(|c: char| matches!(c, '€' | '$' | '£' | ' ' | '\u{00a0}'));
        if t.is_empty() {
            return false;
        }
        let mut has_digit = false;
        for ch in t.chars() {
            if ch.is_ascii_digit() {
                has_digit = true;
            } else if !matches!(ch, '.' | ',' | '-' | '+' | '%' | '/' | '\'' | ' ' | '\u{00a0}') {
                return false;
            }
        }
        has_digit
    }

    /// Whether `rows[0]` is a real header. It is **not** when the first row
    /// already looks like its own data: a textual label plus a value column
    /// (`Total HT | 1250,00 €`) that stays a value column for every other row.
    /// A conventional header (`Désignation | Qté | PU HT | TVA | Total HT`) has
    /// no value cell in row 0 and is kept; so is an all-numeric first row, so
    /// the existing cell-count contract for headerless numeric grids is
    /// unchanged.
    pub(crate) fn first_row_is_header(rows: &[Vec<String>]) -> bool {
        if rows.len() < 2 {
            return true;
        }
        let num_cols = rows.iter().map(|r| r.len()).max().unwrap_or(0);
        let has_label = (0..num_cols).any(|c| {
            let v = rows[0].get(c).map(|s| s.as_str()).unwrap_or("").trim();
            !v.is_empty() && !Self::is_value_cell(v)
        });
        let has_value_column = (0..num_cols).any(|c| {
            Self::is_value_cell(rows[0].get(c).map(|s| s.as_str()).unwrap_or(""))
                && rows.iter().all(|r| {
                    let v = r.get(c).map(|s| s.as_str()).unwrap_or("").trim();
                    v.is_empty() || Self::is_value_cell(v)
                })
        });
        !(has_label && has_value_column)
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

        // Row 0 is the header only for a table that actually has one. A
        // headerless label/value grid (invoice totals: `Total HT | 1250,00 €`)
        // is emitted with a synthesized empty header so all of its rows stay
        // data rows, matching what vision/ground truth read.
        let has_header = Self::first_row_is_header(&self.rows);

        // Header row (row 0 or synthesized)
        md.push('|');
        for c in 0..num_cols {
            let val = if has_header {
                self.rows[0].get(c).map(|s| s.as_str()).unwrap_or("")
            } else {
                ""
            };
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

        // Data rows. For a headerless grid row 0 is data, so it is emitted too.
        let data_start = if has_header { 1 } else { 0 };
        for row in self.rows.iter().skip(data_start) {
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

/// How placed images are handled during a conversion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MediaMode {
    /// Neither extract nor emit images (the default: fastest, smallest output,
    /// and the policy the CLI and services ship with).
    None,
    /// Extract image placements into the JSON `media` side-channel, but never
    /// inline a `data:` URI into the markdown. Use this when the caller saves
    /// the images itself (e.g. the Obsidian plugin's `attachments/` folder).
    Reference,
    /// Extract image placements and inline them as self-contained `data:` URI
    /// images in the markdown (explicit opt-in: large output).
    Embed,
}

impl Default for MediaMode {
    fn default() -> Self {
        MediaMode::None
    }
}

/// Conversion and parsing options.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversionOptions {
    pub detect_tables: bool,
    pub detect_headings: bool,
    pub min_words_per_page: usize,
    /// How placed images are handled: [`MediaMode::None`] (default),
    /// [`MediaMode::Reference`] (JSON `media` list only) or
    /// [`MediaMode::Embed`] (inline `data:` URIs).
    pub media_mode: MediaMode,
    /// Rebuild reading order with zones/columns/furniture handling on the
    /// geometry path (fallback is byte-identical for simple single-column
    /// pages).
    pub detect_layout: bool,
    /// Detect pure-vector figure regions (charts/diagrams/logos drawn with
    /// paths, no raster) and cut them out as standalone clipped PDFs.
    pub detect_vectors: bool,
    /// Synthesize a LaTeX math AST from 2D glyph geometry and emit built-up
    /// fractions (`$\frac{a}{b}$`) and simple super/subscripts (`$x^{2}$`,
    /// `$a_{i}$`) as inline LaTeX before export. On by default; set to `false`
    /// for byte-identical plain extraction (no `$...$` synthesis).
    pub detect_math: bool,
    /// Maximum pixel dimension (width or height) for a raw/PNG-reconstructed
    /// raster before it is downscaled proportionally. Applies to the
    /// non-JPEG decode path (`decode_xobject_bytes`) at extraction time —
    /// JPEG streams are kept as a byte-for-byte passthrough since re-encoding
    /// them needs a JPEG codec that the default (non-`vision`) build does not
    /// link. The embed loop may additionally downscale/JPEG-recompress an
    /// already-encoded image to fit `max_media_bytes_per_doc` (see there).
    /// Bounds per-image payload size and markdown-render latency for scanned
    /// pages.
    pub max_image_dimension: u32,
    /// Maximum total bytes of base64-encoded image data inlined into the
    /// markdown across the whole document (`MediaMode::Embed`).
    ///
    /// When an image's full-size payload would exceed the remaining budget it
    /// is first adaptively shrunk to fit: progressively downscaled (aspect
    /// preserved, never below a ~400 px longest edge) re-measuring the base64
    /// size at each step, and under the `vision` feature also recompressed to
    /// JPEG over a descending quality ladder (80→40). Only if even that floor
    /// is still over budget is the image replaced with the short text
    /// placeholder, so a scan-heavy document can never blow the emitted
    /// markdown up to megabytes (which both slows rendering and can overflow a
    /// downstream LLM prompt/token limit). The JSON `media` side-channel
    /// always keeps the full-fidelity image; only the inline markdown copy is
    /// shrunk or omitted. The shrink is deterministic and bounded (a handful
    /// of encode attempts, no unbounded search).
    pub max_media_bytes_per_doc: usize,
}

impl Default for ConversionOptions {
    fn default() -> Self {
        Self {
            detect_tables: true,
            detect_headings: true,
            min_words_per_page: 5,
            media_mode: MediaMode::None,
            detect_layout: true,
            detect_vectors: false,
            detect_math: true,
            max_image_dimension: 1536,
            max_media_bytes_per_doc: 512 * 1024,
        }
    }
}

/// Conversion result summary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversionResult {
    pub markdown: String,
    pub total_pages: usize,
    pub total_words: usize,
    /// Count of pages whose own word count is below
    /// `ConversionOptions::min_words_per_page`. A document-wide `total_words`
    /// total can be nonzero while most individual pages are still near-empty
    /// (a few real pages carrying an otherwise-scanned document); callers use
    /// this vs. `total_pages` as a ratio to catch that case.
    pub pages_below_word_floor: usize,
    pub tables_detected: usize,
    pub duration_us: u64,
    /// True when at least one page's extraction hit a hard work bound (Form
    /// XObject `Do` invocation count, shared operator/decoded-byte budget, or
    /// form recursion depth) and stopped descending early. The conversion still
    /// succeeds with the text decoded so far — the flag makes the truncation
    /// observable instead of silent, so a caller can choose to rescue the
    /// document. `false` for every document that stays inside the budgets.
    #[serde(default, skip_serializing_if = "is_false")]
    pub budget_exhausted: bool,
    /// Extracted image placements (base64 payloads) when `detect_media`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub media: Vec<crate::media::MediaItem>,
    /// Structured reading-order blocks for pages handled by the geometry
    /// layout engine.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocks: Vec<crate::layout::DocBlock>,
    /// True when the text layer could not be decoded into readable words and
    /// the document should be routed through the OCR/Vision rescue pipeline.
    /// When set, `markdown` still holds a well-formed (non-empty) status
    /// document instead of being empty, so a downstream caller never has to
    /// special-case a 0-byte result. `false` for every ordinary conversion.
    #[serde(default, skip_serializing_if = "is_false")]
    pub needs_vision_rescue: bool,
    /// Why rescue is needed, when [`ConversionResult::needs_vision_rescue`] is
    /// set. Structured counterpart to the legacy error string so the gateway/
    /// worker gets an unambiguous signal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rescue_reason: Option<RescueReason>,
}

/// Structured reason a document needs the vision-rescue route. Emitted on
/// [`ConversionResult`] alongside the human-readable status markdown so a
/// caller can branch on the cause instead of parsing prose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RescueReason {
    /// Text-show operators were present but zero readable words were decoded:
    /// glyph-encoded/outlined fonts (e.g. Type3) or a missing/broken ToUnicode
    /// CMap on a Type0/CID font. The document is not a scan, so only vision
    /// rescue can recover it.
    GlyphEncoded,
    /// Fonts were referenced but no readable text decoded.
    UnreadableFonts,
    /// No fonts and no text-show operators: a scanned/image-only document.
    ScannedImage,
}

/// `skip_serializing_if` helper: omit the `budget_exhausted` key when false, so
/// the JSON of an ordinary conversion is byte-identical to before.
fn is_false(b: &bool) -> bool {
    !*b
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(rows: Vec<Vec<&str>>) -> CanvasTable {
        CanvasTable::new(
            rows.into_iter()
                .map(|r| r.into_iter().map(|s| s.to_string()).collect())
                .collect(),
            BoundingBox::new(0.0, 0.0, 0.0, 0.0),
        )
    }

    /// Invoice totals block: every row is a label/value pair, so row 0 is data,
    /// not a header. Regression for the `Total HT` row being swallowed by the
    /// synthesized GFM header.
    #[test]
    fn headerless_label_value_grid_keeps_first_row_as_data() {
        let t = table(vec![
            vec!["Total HT", "1250,00 €"],
            vec!["Total TVA", "0,00 €"],
            vec!["Total TTC", "1250,00 €"],
        ]);
        let md = t.to_markdown();
        let first = md.lines().next().unwrap();
        assert!(
            first.chars().all(|c| c == '|' || c == ' '),
            "expected an empty header, got {first:?} in\n{md}"
        );
        assert!(md.contains("| Total HT | 1250,00 € |"), "Totals row lost:\n{md}");
        assert!(md.contains("| Total TVA | 0,00 € |"), "{md}");
        assert!(md.contains("| Total TTC | 1250,00 € |"), "{md}");
    }

    /// A conventional header (no value cell in row 0) is still emitted as the
    /// header, so ordinary tables are untouched.
    #[test]
    fn real_header_row_is_preserved() {
        let t = table(vec![
            vec!["Désignation", "Qté", "PU HT", "TVA", "Total HT"],
            vec!["Prestation de conseil", "1", "800,00 €", "-", "800,00 €"],
        ]);
        let md = t.to_markdown();
        assert!(
            md.starts_with("| Désignation | Qté | PU HT | TVA | Total HT |\n"),
            "{md}"
        );
    }

    /// All-numeric first rows keep the existing row-0-as-header contract that
    /// the structural benchmark's `table_borderless_numbers` scores against.
    #[test]
    fn all_numeric_first_row_keeps_header_contract() {
        let t = table(vec![
            vec!["1200.00", "45.50", "7.20"],
            vec!["443.10", "12.00", "9.90"],
        ]);
        let md = t.to_markdown();
        assert!(md.starts_with("| 1200.00 | 45.50 | 7.20 |\n"), "{md}");
    }

    /// `budget_exhausted` is an additive JSON field: omitted when false so an
    /// ordinary conversion's JSON is unchanged, present when true.
    #[test]
    fn budget_exhausted_is_serialised_only_when_true() {
        let mut r = ConversionResult {
            markdown: String::new(),
            total_pages: 1,
            total_words: 0,
            pages_below_word_floor: 0,
            tables_detected: 0,
            duration_us: 0,
            budget_exhausted: false,
            media: Vec::new(),
            blocks: Vec::new(),
            needs_vision_rescue: false,
            rescue_reason: None,
        };
        let v = serde_json::to_value(&r).expect("serialise");
        assert!(
            v.get("budget_exhausted").is_none(),
            "false must be skipped: {v}"
        );
        r.budget_exhausted = true;
        let v = serde_json::to_value(&r).expect("serialise");
        assert_eq!(v.get("budget_exhausted"), Some(&serde_json::Value::Bool(true)));
    }
}
