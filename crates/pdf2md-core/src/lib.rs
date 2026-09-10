//! pdf2md-core — High-performance native Rust core engine for sub-millisecond
//! PDF-to-Markdown extraction and 2D spatial canvas table reconstruction.
//!
//! Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
//! SPDX-License-Identifier: BSL-1.1
//! Licensed under the Business Source License 1.1 (BSL-1.1).

pub mod cpdf_textpage;
pub mod ffi;
pub mod glyph_data;
pub mod layout;
pub mod media;
pub mod models;
#[cfg(feature = "vision")]
pub mod phash;
mod time;
pub mod text_extract;

pub use cpdf_textpage::{
    CharInfo, ClusterConfig, Matrix3x3, PdfTextState, Rect, SpatialClusterer, TextBlock, TextLine,
    TextWord,
};
pub use ffi::{
    pdf2md_convert, pdf2md_convert_ex, pdf2md_free_string, pdf2md_is_digital, pdf2md_version,
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
    BoundingBox, CanvasTable, ColumnAlignment, ConversionOptions, ConversionResult, TextSpan,
};

use crate::time::MonoClock;

/// Probes whether raw PDF bytes contain a digital text stream without full rendering.
pub fn is_digital_pdf_bytes(bytes: &[u8]) -> bool {
    if bytes.len() < 32 || !bytes.starts_with(b"%PDF-") {
        return false;
    }

    // Structural check: parse and look for a real text layer. Unlike a raw
    // string scan, this handles content streams that are FlateDecode-compressed
    // (where "BT"/"Tj" never appear in the raw bytes) — e.g. PDFCreator/
    // Ghostscript tickets and most modern PDFs. A page is "digital" if it
    // references any font or issues any text-show operator.
    if let Ok(doc) = lopdf::Document::load_mem(bytes) {
        for (_page_num, page_id) in doc.get_pages() {
            if doc.get_page_fonts(page_id).map_or(false, |f| !f.is_empty()) {
                return true;
            }
            if let Ok(content) = doc.get_and_decode_page_content(page_id) {
                if content
                    .operations
                    .iter()
                    .any(|op| matches!(op.operator.as_str(), "Tj" | "TJ" | "'" | "\""))
                {
                    return true;
                }
            }
        }
        return false;
    }

    // Fallback (failed parse / truncated input): raw markers.
    let text_markers: &[&[u8]] = &[b"BT\n", b"BT\r", b"BT ", b"/Font", b"Tj", b"TJ"];
    let mut matches = 0;
    for marker in text_markers {
        if bytes.windows(marker.len()).any(|w| w == *marker) {
            matches += 1;
        }
    }
    matches >= 2
}

/// 2D spatial canvas clustering: groups text spans into aligned rows and columns.
pub fn reconstruct_canvas_tables(spans: &[TextSpan]) -> Vec<CanvasTable> {
    if spans.len() < 4 {
        return Vec::new();
    }

    // Sort spans top-to-bottom, left-to-right (Y inverted in standard PDF: larger Y is higher)
    let mut sorted_spans = spans.to_vec();
    sorted_spans.sort_by(|a, b| {
        b.bbox
            .y0
            .partial_cmp(&a.bbox.y0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                a.bbox
                    .x0
                    .partial_cmp(&b.bbox.x0)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    });

    // Group spans into candidate rows by vertical alignment threshold (3.0 points)
    let y_tolerance = 3.0;
    let mut rows: Vec<Vec<TextSpan>> = Vec::new();
    let mut current_row: Vec<TextSpan> = Vec::new();
    let mut current_y = sorted_spans[0].bbox.y0;

    for span in sorted_spans {
        if (span.bbox.y0 - current_y).abs() <= y_tolerance {
            current_row.push(span);
        } else {
            if !current_row.is_empty() {
                rows.push(current_row);
            }
            current_y = span.bbox.y0;
            current_row = vec![span];
        }
    }
    if !current_row.is_empty() {
        rows.push(current_row);
    }

    // Filter candidate tables: at least 2 rows and multiple columns
    let multi_col_rows: Vec<&Vec<TextSpan>> = rows.iter().filter(|r| r.len() >= 2).collect();
    if multi_col_rows.len() < 2 {
        return Vec::new();
    }

    // Extract table rows as string matrices
    let mut table_rows: Vec<Vec<String>> = Vec::new();
    for row in multi_col_rows {
        let mut cols: Vec<String> = Vec::new();
        for span in row {
            cols.push(span.text.clone());
        }
        table_rows.push(cols);
    }

    let min_x = spans
        .iter()
        .map(|s| s.bbox.x0)
        .fold(f64::INFINITY, f64::min);
    let max_x = spans
        .iter()
        .map(|s| s.bbox.x1)
        .fold(f64::NEG_INFINITY, f64::max);
    let min_y = spans
        .iter()
        .map(|s| s.bbox.y0)
        .fold(f64::INFINITY, f64::min);
    let max_y = spans
        .iter()
        .map(|s| s.bbox.y1)
        .fold(f64::NEG_INFINITY, f64::max);

    vec![CanvasTable::new(
        table_rows,
        BoundingBox::new(min_x, min_y, max_x, max_y),
    )]
}

/// Best-effort device page box ([x0, y0, x1, y1]) from the page /MediaBox,
/// used to classify full-page background images.
fn page_media_box(doc: &lopdf::Document, page_id: lopdf::ObjectId) -> Option<(f64, f64, f64, f64)> {
    let dict = doc.get_dictionary(page_id).ok()?;
    let rotate = dict
        .get(b"Rotate")
        .ok()
        .and_then(|o| o.as_i64().ok())
        .unwrap_or(0);
    let rotate = ((rotate % 360) + 360) % 360;
    let mb = dict.get(b"MediaBox").ok()?.as_array().ok()?;
    let g = |i: usize| -> Option<f64> {
        mb.get(i)
            .and_then(|o| o.as_float().ok().map(|f| f as f64))
            .or_else(|| mb.get(i).and_then(|o| o.as_i64().ok().map(|v| v as f64)))
    };
    let w = (g(2)? - g(0)?).abs();
    let h = (g(3)? - g(1)?).abs();
    if rotate == 90 || rotate == 270 {
        Some((0.0, 0.0, h, w))
    } else {
        Some((0.0, 0.0, w, h))
    }
}

/// Minimum on-page width fraction for an embedded image. A placement that is
/// genuinely tiny on the page (a barcode, a signature, an inline stamp) still
/// gets this floor so it is not rendered as a sub-pixel sliver.
const MIN_EMBED_WIDTH_FRAC: f64 = 0.05;

/// The on-page width of a media placement as a fraction of the page's width,
/// clamped to `[MIN_EMBED_WIDTH_FRAC, 1.0]`. This reproduces how big the image
/// actually was on the page, so a high-pixel-density placement that occupies
/// only part of the page is no longer blown up to its full pixel width by the
/// markdown reader. Returns `None` when the page has no measurable MediaBox (or
/// when the placement has no on-page width), letting the caller fall back to
/// the natural size.
fn embed_width_frac(
    m: &crate::media::MediaItem,
    page_bbox: Option<(f64, f64, f64, f64)>,
) -> Option<f64> {
    let (px0, _, px1, _) = page_bbox?;
    let pw = (px1 - px0).abs();
    if pw <= 1.0 {
        return None;
    }
    let w = (m.x1 - m.x0).abs();
    if w <= f64::EPSILON {
        return None;
    }
    Some((w / pw).clamp(MIN_EMBED_WIDTH_FRAC, 1.0))
}

/// Tag blocks whose text repeats near the top/bottom of >= 3 pages as running
/// headers/footers. Operates purely on the structured block list.
fn tag_running_furniture(blocks: &mut [layout::DocBlock]) {
    use std::collections::HashMap;
    let mut page_span: HashMap<usize, (f64, f64)> = HashMap::new();
    for b in blocks.iter() {
        let e = page_span
            .entry(b.page)
            .or_insert((f64::INFINITY, f64::NEG_INFINITY));
        e.0 = e.0.min(b.y0.min(b.y1));
        e.1 = e.1.max(b.y0.max(b.y1));
    }
    let norm = |t: &str| -> String {
        t.chars()
            .filter(|c| c.is_alphanumeric() || c.is_whitespace())
            .collect::<String>()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase()
    };
    let mut header_counts: HashMap<String, usize> = HashMap::new();
    let mut footer_counts: HashMap<String, usize> = HashMap::new();
    for b in blocks.iter() {
        if b.kind != "body" && b.kind != "list" && b.kind != "heading" {
            continue;
        }
        if b.text.split_whitespace().count() > 14 {
            continue; // paragraphs aren't furniture
        }
        let n = norm(&b.text);
        if n.is_empty() {
            continue;
        }
        if let Some((lo, hi)) = page_span.get(&b.page) {
            let span = (hi - lo).abs().max(1.0);
            let top_frac = ((b.y0.max(b.y1)) - lo) / span;
            let bottom_frac = ((b.y0.min(b.y1)) - lo) / span;
            if top_frac > 0.9 {
                *header_counts.entry(n).or_insert(0) += 1;
            } else if bottom_frac < 0.1 {
                *footer_counts.entry(n).or_insert(0) += 1;
            }
        }
    }
    for b in blocks.iter_mut() {
        if b.kind != "body" && b.kind != "list" && b.kind != "heading" {
            continue;
        }
        if b.text.split_whitespace().count() > 14 {
            continue;
        }
        let n = norm(&b.text);
        if let Some((lo, hi)) = page_span.get(&b.page) {
            let span = (hi - lo).abs().max(1.0);
            let top_frac = ((b.y0.max(b.y1)) - lo) / span;
            let bottom_frac = ((b.y0.min(b.y1)) - lo) / span;
            if top_frac > 0.9 && header_counts.get(&n).copied().unwrap_or(0) >= 3 {
                b.kind = "header".to_string();
            } else if bottom_frac < 0.1 && footer_counts.get(&n).copied().unwrap_or(0) >= 3 {
                b.kind = "footer".to_string();
            }
        }
    }
}

/// Formats raw URLs as markdown links [url](url) and unglues leading footnote digits (e.g. 1https:// -> 1 [https://..](..)).
fn format_urls_and_footnotes(text: &str) -> String {
    let mut out = String::new();
    for (i, line) in text.lines().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let mut processed_line = line.to_string();

        // 1. Unglue leading footnote numbers:
        // E.g. "1https://" -> "1 https://"
        // E.g. "4Since" -> "4 Since" (digit at start of line followed by capital letter)
        if let Some(first_char) = processed_line.chars().next() {
            if first_char.is_ascii_digit() {
                let digit_end = processed_line.find(|c: char| !c.is_ascii_digit()).unwrap_or(0);
                if digit_end > 0 && digit_end < processed_line.len() {
                    let rem = &processed_line[digit_end..];
                    if rem.starts_with("http://") || rem.starts_with("https://") {
                        processed_line = format!("{} {}", &processed_line[..digit_end], rem);
                    } else if rem.chars().next().map_or(false, |c| c.is_ascii_uppercase()) {
                        processed_line = format!("{} {}", &processed_line[..digit_end], rem);
                    }
                }
            }
        }

        // 2. Detect URLs and format as markdown links: [url](url)
        let mut result = String::new();
        let mut cursor = 0;
        while let Some(start_idx) = processed_line[cursor..]
            .find("http://")
            .or_else(|| processed_line[cursor..].find("https://"))
        {
            let abs_start = cursor + start_idx;
            result.push_str(&processed_line[cursor..abs_start]);

            let is_already_linked = (abs_start > 0 && processed_line.as_bytes()[abs_start - 1] == b'(')
                || (abs_start > 0 && processed_line.as_bytes()[abs_start - 1] == b'<');

            let url_sub = &processed_line[abs_start..];
            let end_offset = url_sub
                .find(|c: char| {
                    c.is_whitespace()
                        || c == ')'
                        || c == '>'
                        || c == '<'
                        || c == '\"'
                        || c == '\''
                })
                .unwrap_or(url_sub.len());

            let raw_url = &url_sub[..end_offset];
            let trimmed_len = raw_url
                .trim_end_matches(|c: char| c == '.' || c == ',' || c == ';' || c == ':')
                .len();
            let trailing_punct = &raw_url[trimmed_len..];
            let clean_url = &raw_url[..trimmed_len];

            if !is_already_linked && !clean_url.is_empty() {
                result.push_str(&format!("[{0}]({0})", clean_url));
            } else {
                result.push_str(clean_url);
            }
            result.push_str(trailing_punct);
            cursor = abs_start + end_offset;
        }
        result.push_str(&processed_line[cursor..]);
        out.push_str(&result);
    }
    out
}

/// Finds the start and end indices of the markdown table enclosing `pos` in `text`.
/// If `pos` is inside or on a table row, returns `(table_start, table_end)` where
/// `table_end` is the end index of the last row of the table.
/// If `pos` is not in a table, returns `(pos, pos)`.
fn find_table_boundaries(text: &str, pos: usize) -> (usize, usize) {
    let is_table_line = |line: &str| -> bool {
        let t = line.trim();
        t.starts_with('|') && t.ends_with('|')
    };

    let line_start = text[..pos].rfind('\n').map(|p| p + 1).unwrap_or(0);
    let line_end = text[pos..].find('\n').map(|p| pos + p).unwrap_or(text.len());
    let current_line = &text[line_start..line_end];

    if !is_table_line(current_line) {
        return (pos, pos);
    }

    // Scan backwards for table start
    let mut t_start = line_start;
    let mut cur = line_start;
    while cur > 0 {
        let prev_start = text[..cur - 1].rfind('\n').map(|p| p + 1).unwrap_or(0);
        let prev_line = &text[prev_start..cur - 1];
        if is_table_line(prev_line) {
            t_start = prev_start;
            cur = prev_start;
        } else {
            break;
        }
    }

    // Scan forward for table end
    let mut t_end = line_end;
    cur = line_end;
    while cur < text.len() {
        let next_start = cur + 1;
        if next_start >= text.len() {
            break;
        }
        let next_end = text[next_start..].find('\n').map(|p| next_start + p).unwrap_or(text.len());
        let next_line = &text[next_start..next_end];
        if is_table_line(next_line) {
            t_end = next_end;
            cur = next_end;
        } else {
            break;
        }
    }

    (t_start, t_end)
}

/// Converts PDF byte slice to clean Markdown with 2D spatial table reconstruction.
pub fn convert_pdf_bytes_to_markdown(
    bytes: &[u8],
    options: &ConversionOptions,
) -> Result<ConversionResult, String> {
    let t0 = MonoClock::now();

    // Try parsing with lopdf
    let doc =
        lopdf::Document::load_mem(bytes).map_err(|e| format!("lopdf parsing error: {}", e))?;

    let mut full_markdown = String::new();
    let total_pages = doc.get_pages().len();
    let mut total_words = 0;
    let mut any_text_ops = false;
    let mut any_fonts = false;
    let mut tables_detected = 0usize;
    let mut media_items: Vec<media::MediaItem> = Vec::new();
    let mut block_items: Vec<layout::DocBlock> = Vec::new();
    // Per-page markdown chunks (page number, content) so running headers and
    // footers can be suppressed after the doc-level furniture pass.
    let mut page_md: Vec<(u32, String)> = Vec::new();
    // True when the document carries a tagged /StructTreeRoot. Computed once so
    // the (untagged) common case pays no per-page structure scan.
    let has_struct_tree = layout::has_struct_tree(&doc);

    for (page_num, page_id) in doc.get_pages() {
        // Prefer our own multilingual decoder (correct WinAnsi/Differences/
        // ToUnicode handling — see text_extract.rs) and only fall back to
        // lopdf's extractor when the page content cannot be parsed at all.
        let page_text = {
            let geo = text_extract::extract_page_text_report(
                &doc,
                page_num,
                options.detect_tables,
                options.detect_layout,
                options.detect_math,
            );
            let geo = match geo {
                Ok(pt) => pt,
                Err(_) => text_extract::PageText {
                    text: doc.extract_text(&[page_num]).unwrap_or_default(),
                    text_ops_seen: true,
                    has_fonts: doc.get_page_fonts(page_id).map_or(false, |f| !f.is_empty()),
                    tables: 0,
                    blocks: Vec::new(),
                },
            };
            // When the document is tagged (/StructTreeRoot, PDF/UA, Word/
            // LibreOffice/Acrobat exports) the structure tree is ground truth for
            // reading order and semantic roles (H1..H6, P, Table, Figure, …). Use
            // it — but ONLY when it covers >= 85% of the words the geometry
            // fast-path produces — otherwise a partial/decorative structure tree
            // (common in invoice generators) would drop content and regress the
            // tuned geometric engine.
            if has_struct_tree {
                if let Some(tagged) = layout::extract_tagged_page(&doc, page_id, options.detect_math) {
                    let geo_words = geo.text.split_whitespace().count();
                    if tagged.words >= 5 && tagged.words * 100 >= 85 * geo_words {
                        text_extract::PageText {
                            text: tagged.text,
                            text_ops_seen: true,
                            has_fonts: true,
                            tables: tagged.tables,
                            blocks: tagged.blocks,
                        }
                    } else {
                        geo
                    }
                } else {
                    geo
                }
            } else {
                geo
            }
        };
        any_text_ops |= page_text.text_ops_seen;
        any_fonts |= page_text.has_fonts;
        tables_detected += page_text.tables;
        let text = page_text.text.clone();
        let mut page_blocks: Vec<layout::DocBlock> = page_text.blocks;
        for b in &mut page_blocks {
            b.page = page_num as usize;
        }

        let page_media: Vec<media::MediaItem> = if options.detect_media {
            let page_bbox = page_media_box(&doc, page_id);
            let mut m = media::extract_page_media(&doc, page_id, page_num as usize, page_bbox);
            if options.detect_vectors {
                m.extend(media::extract_page_vector_figures(
                    &doc,
                    page_id,
                    page_num as usize,
                    page_bbox,
                ));
            }
            m
        } else {
            Vec::new()
        };

        // Figure + caption adjacency: when this page has structured layout
        // blocks, mark the short text line attached to a content image as a
        // caption, and give the figure itself a role in the block list at its
        // reading position (right before the text that follows it).
        if options.detect_layout && !page_media.is_empty() {
            let content: Vec<&media::MediaItem> =
                page_media.iter().filter(|m| !m.decorative).collect();
            if !content.is_empty() && !page_blocks.is_empty() {
                for m in &content {
                    let my = (m.y0 + m.y1) / 2.0;
                    let best = page_blocks
                        .iter_mut()
                        .filter(|b| b.kind != "figure")
                        .min_by(|a, b2| {
                            let da = ((a.y0 + a.y1) / 2.0 - my).abs();
                            let db = ((b2.y0 + b2.y1) / 2.0 - my).abs();
                            da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
                        });
                    if let Some(b) = best {
                        let gap = (my - (b.y0 + b.y1) / 2.0).abs();
                        let size_hint = (b.y1 - b.y0).abs().max(8.0);
                        if gap <= 2.5 * size_hint && b.text.split_whitespace().count() <= 9 {
                            b.kind = "caption".to_string();
                        }
                    }
                }
                // Insert figure blocks immediately before the caption line (or
                // the first text block that sits below the figure).
                let mut figures: Vec<layout::DocBlock> = content
                    .iter()
                    .map(|m| layout::DocBlock {
                        page: page_num as usize,
                        kind: "figure".to_string(),
                        x0: m.x0.min(m.x1),
                        y0: m.y0.min(m.y1),
                        x1: m.x0.max(m.x1),
                        y1: m.y0.max(m.y1),
                        text: format!("[{}]", m.kind.as_str()),
                        is_bold: false,
                        is_italic: false,
                        is_underline: false,
                    })
                    .collect();
                // Highest figure first.
                figures.sort_by(|a, b| {
                    let ay = (a.y0 + a.y1) / 2.0;
                    let by = (b.y0 + b.y1) / 2.0;
                    by.partial_cmp(&ay).unwrap_or(std::cmp::Ordering::Equal)
                });
                for fig in figures {
                    // Place after the last text block above it (reading order).
                    let insert_at = page_blocks
                        .iter()
                        .position(|b| (b.y0 + b.y1) / 2.0 <= (fig.y0 + fig.y1) / 2.0)
                        .unwrap_or(page_blocks.len());
                    page_blocks.insert(insert_at, fig);
                }
            }
        }

        // Words are counted from the text layer only, so corpus text-word
        // baselines are unaffected by image embedding.
        let words: Vec<&str> = text.split_whitespace().collect();
        total_words += words.len();
        for m in &page_media {
            media_items.push(m.clone());
        }

        let mut chunk = String::new();
        if options.detect_headings
            && (text.starts_with("# ")
                || text.lines().next().map_or(false, |l| {
                    l.len() < 60 && l.chars().all(|c| c.is_alphanumeric() || c.is_whitespace())
                }))
        {
            chunk.push_str(&format!("\n## Page {}\n\n", page_num));
        }

        let mut processed_text = format_urls_and_footnotes(&text);

        if options.embed_media {
            // Page-relative sizing: reproduce the on-page footprint so a high-DPI
            // placement that covers only part of the page does not render at its
            // full pixel width (which is much larger than it was on the page).
            let page_bbox = page_media_box(&doc, page_id);
            let mut imgs: Vec<&media::MediaItem> = page_media
                .iter()
                .filter(|m| {
                    !m.decorative
                        && !m.data_b64.is_empty()
                        && (m.format == "image/jpeg" || m.format == "image/png")
                })
                .collect();
            // Sort ascending by y (lowest y first) so bottom-most images are inserted first,
            // preserving string character offsets for images higher up on the page.
            imgs.sort_by(|a, b| {
                let ay = (a.y0 + a.y1) / 2.0;
                let by = (b.y0 + b.y1) / 2.0;
                ay.partial_cmp(&by).unwrap_or(std::cmp::Ordering::Equal)
            });

            let mut remaining_imgs = Vec::new();
            for m in imgs {
                let my = (m.y0 + m.y1) / 2.0;
                let img_md = match embed_width_frac(m, page_bbox) {
                    Some(frac) => format!(
                        "<img alt=\"{}\" src=\"data:{};base64,{}\" width=\"{}\" />\n\n",
                        m.kind.as_str(),
                        m.format,
                        m.data_b64,
                        // Emit as a percent of the reader's content width, which
                        // matches the fraction of the page the image occupied.
                        format!("{}%", (frac * 100.0).round() as u32),
                    ),
                    None => format!(
                        "![{}](data:{};base64,{})\n\n",
                        m.kind.as_str(),
                        m.format,
                        m.data_b64
                    ),
                };

                // Find the first block strictly below the image
                let next_block = page_blocks
                    .iter()
                    .find(|b| b.kind != "figure" && (b.y0 + b.y1) / 2.0 <= my);

                let mut inserted = false;
                if let Some(b) = next_block {
                    let search_key = b
                        .text
                        .lines()
                        .map(|l| l.trim())
                        .find(|l| l.len() >= 4)
                        .unwrap_or_else(|| b.text.lines().next().unwrap_or(&b.text).trim());
                    if !search_key.is_empty() {
                        if let Some(mut pos) = processed_text.find(search_key) {
                            let (t_start, t_end) = find_table_boundaries(&processed_text, pos);
                            if t_start != t_end {
                                pos = t_end;
                            }
                            let mut prefix = String::new();
                            if !processed_text[..pos].ends_with("\n\n") {
                                if processed_text[..pos].ends_with('\n') {
                                    prefix.push('\n');
                                } else {
                                    prefix.push_str("\n\n");
                                }
                            }
                            let full_img = format!("{}{}", prefix, img_md);
                            processed_text.insert_str(pos, &full_img);
                            inserted = true;
                        }
                    }
                }
                if !inserted {
                    remaining_imgs.push(img_md);
                }
            }

            chunk.push_str(&processed_text);
            chunk.push_str("\n\n");
            remaining_imgs.reverse();
            for img_md in remaining_imgs {
                chunk.push_str(&img_md);
            }
        } else {
            chunk.push_str(&processed_text);
            chunk.push_str("\n\n");
        }
        page_md.push((page_num, chunk));
        block_items.extend(page_blocks);
    }

    // When nothing was decoded, give an actionable reason instead of a silent
    // empty markdown:
    //  * text-show operators but no readable text -> glyph-encoded (Type3) doc;
    //  * no fonts and no text-show operators at all -> scanned/image page.
    if total_words == 0 {
        if any_text_ops {
            return Err(
                "Document text layer is glyph-encoded (e.g. Type3/outlined) with no Unicode mapping; \
                 no readable text found — route through the OCR/Vision pipeline (vision-LLM rescue)."
                    .to_string(),
            );
        }
        if !any_fonts {
            return Err(
                "Document lacks a digital text layer or is scanned (images only); \
                 route through the OCR/Vision pipeline (vision-LLM rescue)."
                    .to_string(),
            );
        }
        return Err(
            "Document references fonts but no readable text was found (scanned or outlined); \
             route through the OCR/Vision pipeline (vision-LLM rescue)."
                .to_string(),
        );
    }

    // Document-level furniture pass: a short line repeated at the top band of
    // many pages is a running header; one repeated at the bottom band is a
    // running footer. Tag them in the block list, and suppress their repeated
    // occurrences in the emitted markdown (keep the first / page-1 line).
    if options.detect_layout && !block_items.is_empty() {
        tag_running_furniture(&mut block_items);
        // Keep a running header/footer only on its first page; strip the
        // repeated occurrences from every later page of the markdown.
        let mut furniture: Vec<(String, u32)> = Vec::new();
        for b in block_items
            .iter()
            .filter(|b| b.kind == "header" || b.kind == "footer")
        {
            let t = b.text.trim();
            if t.len() <= 1 {
                continue;
            }
            match furniture.iter_mut().find(|(ft, _)| ft == t) {
                Some((_, first)) => *first = (*first).min(b.page as u32),
                None => furniture.push((t.to_string(), b.page as u32)),
            }
        }
        for (page, chunk) in page_md.iter_mut() {
            for (line_text, first_page) in furniture.iter() {
                if *page <= *first_page {
                    continue; // keep on the first page it occurs
                }
                let mut out = String::with_capacity(chunk.len());
                for ln in chunk.lines() {
                    if ln.trim() == line_text {
                        continue;
                    }
                    out.push_str(ln);
                    out.push('\n');
                }
                if out.ends_with('\n') {
                    out.pop();
                }
                *chunk = out;
            }
        }
    }
    for (_p, chunk) in page_md {
        full_markdown.push_str(&chunk);
    }

    let duration_us = t0.elapsed_us();

    Ok(ConversionResult {
        markdown: full_markdown,
        total_pages,
        total_words,
        tables_detected,
        duration_us,
        media: media_items,
        blocks: block_items,
    })
}

#[cfg(test)]
mod regression_tests {
    use super::*;

    /// Split a GFM pipe-table row `| a | b |` into normalized cell strings.
    fn split_cells(line: &str) -> Vec<String> {
        let inner = line.trim();
        let inner = inner
            .strip_prefix('|')
            .unwrap_or(inner)
            .strip_suffix('|')
            .unwrap_or(inner);
        inner
            .split('|')
            .map(|c| c.trim().replace("\\|", "|").replace("\\\\", "\\"))
            .collect()
    }

    /// Remove the inline emphasis delimiters (`**`, `*`, `<u>`, `</u>`) the
    /// renderer now inserts, so substring assertions stay valid whether or not
    /// a run was detected as bold/italic/underline.
    fn strip_emphasis(s: &str) -> String {
        s.replace("**", "")
            .replace("<u>", "")
            .replace("</u>", "")
            .replace("*", "")
    }

    /// Parse every GFM pipe table (with a `|`-separator row) in the markdown
    /// into a grid of cells. Used by the structural assertions to recover the
    /// reconstructed tables instead of doing substring searches.
    fn parse_gfm_tables(md: &str) -> Vec<Vec<Vec<String>>> {
        let lines: Vec<&str> = md.lines().collect();
        let mut out = Vec::new();
        let mut i = 0;
        while i < lines.len() {
            let t = lines[i].trim();
            if t.starts_with('|') && t.ends_with('|') {
                let mut rows = Vec::new();
                while i < lines.len() {
                    let lt = lines[i].trim();
                    if !(lt.starts_with('|') && lt.ends_with('|')) {
                        break;
                    }
                    let cells = split_cells(lt);
                    let is_sep = cells.iter().all(|c| {
                        !c.is_empty() && c.chars().all(|ch| ch == '-' || ch == ':' || ch.is_whitespace())
                    });
                    if !is_sep {
                        rows.push(cells);
                    }
                    i += 1;
                }
                if !rows.is_empty() {
                    out.push(rows);
                }
            } else {
                i += 1;
            }
        }
        out
    }

    #[test]
    fn test_billet_electronique_itinerary_table_cohesion() {
        let pdf_path = "../../../scratch/tests/fixtures/billet_electronique.pdf";
        if let Ok(bytes) = std::fs::read(pdf_path) {
            let res = convert_pdf_bytes_to_markdown(&bytes, &ConversionOptions::default()).unwrap();
            let raw_md = &res.markdown;
            let md = strip_emphasis(raw_md);

            // STRUCTURAL assertion, not "contains AF7331": the four flight legs
            // must be reconstructed as a single unified GFM table with the
            // documented 9-column bilingual header, and each leg must occupy
            // exactly one data row's Flight column.
            let tables = parse_gfm_tables(&md);
            let itinerary = tables
                .iter()
                .find(|t| t.first().map_or(false, |hdr| hdr.iter().any(|c| c.contains("Vol") && c.contains("Flight"))))
                .expect("itinerary table (9-col bilingual header) must be reconstructed");

            let num_cols = itinerary[0].len();
            assert_eq!(num_cols, 9, "itinerary must be a 9-column table");
            assert_eq!(itinerary.len() - 1, 4, "exactly one row per flight leg");
            for row in itinerary.iter().skip(1) {
                assert_eq!(row.len(), num_cols, "every flight row must have 9 cells");
                // The Flight cell may carry `<br>`-separated metadata; the leg
                // code is its leading token.
                let flight = row[3].split(['<', '\n']).next().unwrap_or("").trim();
                assert!(
                    matches!(flight, "AF7331" | "AF0258" | "AF0253" | "AF7342"),
                    "Flight column must carry a known leg code, got {:?}",
                    row[3]
                );
            }
            let legs: Vec<String> = itinerary
                .iter()
                .skip(1)
                .map(|r| r[3].split(['<', '\n']).next().unwrap_or("").trim().to_string())
                .collect();
            for want in ["AF7331", "AF0258", "AF0253", "AF7342"] {
                assert!(legs.iter().any(|l| l == want), "missing flight leg {}", want);
            }
        }
    }

    #[test]
    fn test_edf_facture_complex_extraction() {
        let pdf_path = "../../../scratch/samples/edf-facture-complex.pdf";
        if let Ok(bytes) = std::fs::read(pdf_path) {
            let res = convert_pdf_bytes_to_markdown(&bytes, &ConversionOptions::default()).unwrap();
            assert_eq!(res.total_pages, 10, "Must have 10 pages");
            let md = strip_emphasis(&res.markdown);

            // 1. Unified address & proper French text without artificial spaces
            assert!(md.contains("Mlle PALMA BRIGITTE"), "Must contain unified 'Mlle PALMA BRIGITTE'");
            assert!(!md.contains("Ml l e PALMA"), "Must NOT contain letter-split 'Ml l e PALMA'");
            assert!(!md.contains("BRI GI TTE"), "Must NOT contain letter-split 'BRI GI TTE'");
            assert!(md.contains("LE GALOIS"), "Must contain unified 'LE GALOIS'");
            assert!(!md.contains("LE GALOI S"), "Must NOT contain letter-split 'LE GALOI S'");
            assert!(md.contains("13014 MARSEILLE"), "Must contain unified '13014 MARSEILLE'");
            assert!(!md.contains("13014 MARSEI LLE"), "Must NOT contain letter-split '13014 MARSEI LLE'");

            // 2. Sidebar contact preservation
            assert!(md.contains("NOUS CONTACTER"), "Must extract 'NOUS CONTACTER' header");
            assert!(md.contains("5 002 674 443"), "Must extract client number");

            // 3. French diacritics & elision preservation
            assert!(md.contains("d'électricité") || md.contains("d’électricité"), "Must preserve elision in 'd'électricité'");
            assert!(md.contains("Médiateur"), "Must preserve French accent in 'Médiateur'");
            assert!(md.contains("Détail de la facture"), "Must preserve French accent in 'Détail de la facture'");

            // 4. Page 9 separated blocks
            assert!(md.contains("MA CONSO"), "Must contain 'MA CONSO'");
            assert!(md.contains("& MOI"), "Must contain '& MOI'");

            // 5. Margin text partitioned and not interleaved into prose
            assert!(!md.contains("Mademoiselle, 7 1 3 1 8"), "Vertical margin must not interleave into letter greeting");

            // 6. STRUCTURAL: the reconstructions must be well-formed GFM tables
            // (a uniform rectangle) — not merely contain the right substrings.
            let tables = parse_gfm_tables(&md);
            assert!(!tables.is_empty(), "facture must reconstruct at least one table");
            for t in &tables {
                let cols = t[0].len();
                assert!(cols >= 2, "a reconstructed table must have >= 2 columns");
                for row in t {
                    assert_eq!(row.len(), cols, "every row of a reconstructed table must share the header column count");
                }
            }
        }
    }

    #[test]
    fn embed_width_frac_maps_page_fraction() {
        use crate::media::MediaKind;
        let page = Some((0.0, 0.0, 612.0, 792.0));
        let make = |x0: f64, x1: f64| media::MediaItem {
            page: 1,
            x0,
            y0: 0.0,
            x1,
            y1: 60.0,
            width: 600,
            height: 600,
            format: "image/jpeg".into(),
            kind: MediaKind::Photo,
            decorative: false,
            repeat: 1,
            data: Vec::new(),
            data_b64: String::new(),
        };

        // 100pt of a 612pt-wide page => the image covers 16.34% of the width.
        let frac = embed_width_frac(&make(0.0, 100.0), page).unwrap();
        assert!((frac - 100.0 / 612.0).abs() < 1e-9, "got {frac}");

        // A placement that is tiny on the page still gets the floor.
        assert_eq!(
            embed_width_frac(&make(0.0, 4.0), page),
            Some(MIN_EMBED_WIDTH_FRAC)
        );

        // Clamp anything that bleeds past the page edge to full width.
        assert_eq!(embed_width_frac(&make(0.0, 800.0), page), Some(1.0));

        // No measurable page box => fall back to natural size.
        assert_eq!(embed_width_frac(&make(0.0, 100.0), None), None);
    }

    #[test]
    fn embedded_images_are_resized_to_page_footprint() {
        let pdf_path = "../../../scratch/tests/fixtures/synth_logo_image.pdf";
        if let Ok(bytes) = std::fs::read(pdf_path) {
            let res = convert_pdf_bytes_to_markdown(&bytes, &ConversionOptions::default()).unwrap();
            let md = &res.markdown;

            // The fixture must actually embed a raster image so the assertion is
            // meaningful.
            assert!(
                md.contains("data:image/"),
                "fixture must embed a raster image, got: {md}"
            );

            // Every embedded image is an HTML <img> carrying a percent width equal
            // to its on-page footprint — never a bare markdown image that the
            // reader renders at the image's full pixel width.
            assert!(
                !md.contains("](data:image/"),
                "must not embed as a bare markdown image (renders at full pixel size)"
            );
            assert!(
                md.contains("width=") && md.contains("%\""),
                "embedded image must carry a percent width attribute"
            );
        }
    }
}
