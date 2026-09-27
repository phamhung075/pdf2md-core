//! pdf2md-core — High-performance native Rust core engine for sub-millisecond
//! PDF-to-Markdown extraction and 2D spatial canvas table reconstruction.
//!
//! Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
//! SPDX-License-Identifier: BSL-1.1
//! Licensed under the Business Source License 1.1 (BSL-1.1).

mod pdf_load;
pub use pdf_load::*;
mod text_layer;
pub use text_layer::*;
mod canvas;
pub use canvas::*;
mod furniture;
use furniture::*;
mod running_lines;
use running_lines::*;
mod urls;
use urls::*;
mod page_select;
use page_select::*;
mod glyph_counts;
mod convert_annotations;
mod convert_media;
mod convert;
pub use convert::*;

pub mod cpdf_textpage;
pub mod ffi;
pub mod glyph_data;
pub mod layout;
pub mod media;
pub mod models;
#[cfg(feature = "vision")]
pub mod phash;
pub mod reflow;
mod time;
pub mod text_extract;

pub use cpdf_textpage::{
    CharInfo, ClusterConfig, Matrix3x3, PdfTextState, Rect, SpatialClusterer, TextBlock, TextLine,
    TextWord,
};
pub use ffi::{
    pdf2md_convert, pdf2md_convert_ex, pdf2md_convert_ex2, pdf2md_convert_ex3,
    pdf2md_free_string, pdf2md_is_digital, pdf2md_version,
};
pub use layout::{
    analyze_char_stream, analyze_layout, analyze_pages_parallel, correct_skew, deskew_lines,
    deskew_spans, estimate_skew_angle_deg, estimate_skew_angle_deg_from_spans, extract_tables,
    math_inline_for_line, render_math, render_math_line, spans_from_textline, synthesize_block_text,
    synthesize_line_expr, synthesize_spans_math, AstNode, DocumentStatistics, LatexExpr, LayoutAST,
    LineSegment, MIN_SKEW_TO_CORRECT_DEG, ModernLayoutEngine, TableCell, XyCutOptions,
};
pub use media::{extract_page_media, extract_page_vector_figures, MediaItem, MediaKind};
pub use models::{
    BoundingBox, CanvasTable, ColumnAlignment, ConversionOptions, ConversionResult, MediaMode,
    RescueReason, TextSpan,
};

use crate::time::MonoClock;

/// Largest decompressed size any single stream may reach during load or object
/// stream recovery. A tiny Flate stream can inflate without bound (a
/// "decompression bomb"); object/xref streams in real documents are tiny
/// dictionaries, so 16 MiB is orders of magnitude above any legitimate value.
pub(crate) const MAX_DECOMPRESSED_STREAM: usize = 16 << 20;
/// Total decompression budget for all object streams recovered in one document.
const MAX_OBJSTM_TOTAL: usize = 64 << 20;
/// Upper bound on how many object streams are examined in one document.
const MAX_OBJSTM_STREAMS: usize = 256;
/// Upper bound on the pages one conversion parses, matching the Go worker's
/// `maxSanePageCount` (5000). A crafted document can hold many thousands of
/// tiny page objects in one small upload; past this the conversion fails
/// explicitly instead of grinding through them for minutes.
const MAX_PAGES: usize = 5000;

/// Loads a PDF with lopdf, transparently repairing a classic cross-reference
/// table whose `startxref` pointer and/or per-object byte offsets are stale.
///
/// Some real-world producers emit a file whose `startxref` points a byte or two
/// past the `xref` keyword and whose trailing object offsets drift, while every
/// object body is intact. lopdf trusts the declared offsets verbatim and fails
/// the whole load with `invalid file trailer`, even though MuPDF, browsers and
/// every other reader open the file. This is a fallback only: a well-formed
/// document is loaded on the first, unmodified attempt.
///
/// All loads decode object/xref streams with [`lopdf::LoadOptions::max_decompressed_size`]
/// so a crafted object stream cannot allocate unbounded memory before our code
/// runs; a stream over the cap is skipped by lopdf instead of failing the load.
/// A PDF the empty user password cannot open: `lopdf` loads it but leaves an
/// `/Encrypt` entry in the trailer and no usable pages/objects. This is not a
/// scanned document — vision rescue cannot read it either — so it must be
/// reported as an encryption failure rather than routed to OCR.
pub const ENCRYPTED_PDF_ERROR: &str = "encrypted PDF: password required";

#[cfg(test)]
mod regression_tests;
#[cfg(test)]
mod regression_tests_common;
#[cfg(test)]
mod regression_tests_common2;
#[cfg(test)]
mod regression_tests_part2;
#[cfg(test)]
mod regression_tests_part3;
#[cfg(test)]
mod regression_tests_part4;
