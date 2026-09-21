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
pub mod reflow;
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

/// True when the loaded document is still encrypted after loading: the trailer
/// carries an `/Encrypt` dictionary and lopdf did not decrypt it (on a
/// successful empty-password authentication lopdf removes the trailer entry and
/// records `encryption_state`). This is deliberately *not* conditioned on the
/// page tree being empty: an encrypted document whose pages happen to remain
/// parseable must still be reported as encrypted rather than silently converted
/// or routed to OCR, which cannot read it either.
fn encrypted_undecrypted(doc: &lopdf::Document) -> bool {
    doc.trailer.get(b"Encrypt").is_ok() && !doc.was_encrypted()
}

fn load_pdf_document(bytes: &[u8]) -> Result<lopdf::Document, String> {
    let doc = load_pdf_document_repaired(bytes)?;
    if encrypted_undecrypted(&doc) {
        return Err(ENCRYPTED_PDF_ERROR.to_string());
    }
    Ok(doc)
}

/// True when `bytes` is a PDF that the empty user password cannot open (the
/// trailer keeps an `/Encrypt` entry after loading). `is_digital_pdf_bytes`
/// reports such a document as "not digital"; the CLI calls this first so it can
/// surface the distinct [`ENCRYPTED_PDF_ERROR`] message instead of the generic
/// scanned-image one.
pub fn pdf_password_required(bytes: &[u8]) -> bool {
    if bytes.len() < 32 || !bytes.starts_with(b"%PDF-") {
        return false;
    }
    match load_pdf_document_repaired(bytes) {
        Ok(doc) => encrypted_undecrypted(&doc),
        Err(_) => false,
    }
}

fn load_pdf_document_repaired(bytes: &[u8]) -> Result<lopdf::Document, String> {
    match load_bounded(bytes) {
        Ok(doc) => Ok(recover_object_streams(doc)),
        Err(first_err) => {
            // Recovery 1: some producers pad the file with bytes after `%%EOF`
            // (fixed-size host buffers), which pushes the marker outside the
            // last-512-byte window lopdf scans for `startxref` and makes the
            // otherwise-valid classic xref unreadable (`invalid start value`).
            if let Some(trimmed) = truncate_after_last_eof(bytes) {
                if let Ok(doc) = load_bounded(&trimmed) {
                    return Ok(recover_object_streams(doc));
                }
                if let Some(repaired) = repair_classic_xref(&trimmed) {
                    if let Ok(doc) = load_bounded(&repaired) {
                        return Ok(recover_object_streams(doc));
                    }
                }
            }
            // Recovery 2: the existing repair for a classic xref whose declared
            // offsets drifted.
            if let Some(repaired) = repair_classic_xref(bytes) {
                if let Ok(doc) = load_bounded(&repaired) {
                    return Ok(recover_object_streams(doc));
                }
            }
            Err(format!("lopdf parsing error: {}", first_err))
        }
    }
}

/// [`lopdf::Document::load_mem`] with the decompression-bomb cap applied.
fn load_bounded(bytes: &[u8]) -> Result<lopdf::Document, lopdf::Error> {
    lopdf::Document::load_mem_with_options(
        bytes,
        lopdf::LoadOptions::with_max_decompressed_size(MAX_DECOMPRESSED_STREAM),
    )
}

/// Returns `bytes` truncated just past the last `%%EOF` marker, or `None` when
/// there is nothing to trim (well-formed file) or no marker at all.
fn truncate_after_last_eof(bytes: &[u8]) -> Option<Vec<u8>> {
    let eof = find_last(bytes, b"%%EOF")?;
    let end = eof + b"%%EOF".len();
    if end >= bytes.len() {
        return None;
    }
    Some(bytes[..end].to_vec())
}

/// lopdf expands `/Type /ObjStm` object streams by parsing each embedded object
/// with a parser that does not skip `%` comments. A number of real producers
/// separate the embedded objects with `% N G` comment lines (ORNIKAR CGV), so
/// every compressed object fails to parse and the page tree root disappears —
/// `get_pages()` returns empty even though the file is valid. When that happens,
/// decompress each object stream, blank those line-leading comments in place,
/// and re-parse it ourselves, folding the recovered objects into the document.
/// Only invoked when the normal load produced no pages. Each stream is decoded
/// under a size cap, with a total budget and a stream cap for the pass, so a
/// decompression bomb is skipped instead of exhausting memory.
fn recover_object_streams(mut doc: lopdf::Document) -> lopdf::Document {
    if !doc.get_pages().is_empty() {
        return doc;
    }
    let streams: Vec<lopdf::ObjectId> = doc
        .objects
        .iter()
        .filter_map(|(id, object)| match object {
            lopdf::Object::Stream(stream)
                if crate::text_extract::get_name(&stream.dict, b"Type") == Some(b"ObjStm") =>
            {
                Some(*id)
            }
            _ => None,
        })
        .take(MAX_OBJSTM_STREAMS)
        .collect();
    if streams.is_empty() {
        return doc;
    }
    let mut recovered: Vec<(lopdf::ObjectId, lopdf::Object)> = Vec::new();
    // Bound the whole recovery pass: a document can hold many object streams, so
    // cap both how much each one may expand and how much is decoded in total. A
    // stream over its budget is skipped rather than failing the conversion.
    let mut budget = MAX_OBJSTM_TOTAL;
    for id in streams {
        if budget == 0 {
            break;
        }
        let Some(lopdf::Object::Stream(stream)) = doc.objects.get_mut(&id) else {
            continue;
        };
        let Ok(mut content) =
            stream.decompressed_content_with_limit(budget.min(MAX_DECOMPRESSED_STREAM))
        else {
            continue;
        };
        budget = budget.saturating_sub(content.len());
        if !blank_line_comments(&mut content) {
            continue;
        }
        // Re-parse the now comment-free bytes directly: drop the filter so
        // `ObjectStream` does not try to decompress the plain content again.
        stream.dict.remove(b"Filter");
        stream.dict.remove(b"DecodeParms");
        stream.set_content(content);
        if let Ok(object_stream) = lopdf::ObjectStream::new(stream) {
            recovered.extend(object_stream.objects);
        }
    }
    for (id, object) in recovered {
        doc.objects.entry(id).or_insert(object);
    }
    doc
}

/// Blank PDF comments (`%` to end of line) that start a line, preserving the
/// byte length so nothing else has to be re-offset. Returns whether anything
/// changed.
fn blank_line_comments(content: &mut [u8]) -> bool {
    let mut at_line_start = true;
    let mut changed = false;
    let mut i = 0;
    while i < content.len() {
        let byte = content[i];
        if at_line_start && byte == b'%' {
            while i < content.len() && content[i] != b'\n' && content[i] != b'\r' {
                content[i] = b' ';
                i += 1;
            }
            changed = true;
            continue;
        }
        at_line_start = matches!(byte, b'\n' | b'\r' | b' ');
        i += 1;
    }
    changed
}

/// Rebuilds a classic cross-reference table from the actual `N G obj` headers
/// when the declared offsets disagree with them, and corrects the trailing
/// `startxref` value. Returns `None` for files with no classic table, or whose
/// entries do not have the fixed 20-byte layout — we only rewrite a table we
/// fully understand, so a genuine parse failure is still reported unchanged.
fn repair_classic_xref(bytes: &[u8]) -> Option<Vec<u8>> {
    let xref_pos = find_last_xref_keyword(bytes)?;

    // Each subsection is "<first> <count>\n" followed by `count` fixed-width
    // entries; a well-formed file has one, incremental updates may chain more.
    let mut entries: Vec<(u32, usize)> = Vec::new();
    let mut p = skip_ws(bytes, xref_pos + 4);
    while !bytes[p..].starts_with(b"trailer") {
        let (first, after_first) = parse_uint(bytes, p)?;
        let (count, after_count) = parse_uint(bytes, skip_ws(bytes, after_first))?;
        p = skip_ws(bytes, after_count);
        for i in 0..count {
            let e = p.checked_add((i as usize).checked_mul(20)?)?;
            let entry = bytes.get(e..e.checked_add(20)?)?;
            if entry[10] != b' ' || entry[16] != b' ' || !matches!(entry[17], b'n' | b'f') {
                return None;
            }
            if entry[17] == b'n' {
                entries.push((first.checked_add(i)?, e));
            }
        }
        p = p.checked_add((count as usize).checked_mul(20)?)?;
        p = skip_ws(bytes, p);
    }
    if entries.is_empty() {
        return None;
    }

    let positions = scan_object_offsets(bytes);
    let mut out = bytes.to_vec();
    let mut changed = false;
    for (num, entry_pos) in &entries {
        let true_off = *positions.get(num)?;
        let field = format!("{:010}", true_off);
        if out[*entry_pos..*entry_pos + 10] != *field.as_bytes() {
            out[*entry_pos..*entry_pos + 10].copy_from_slice(field.as_bytes());
            changed = true;
        }
    }

    // Point the last `startxref` at the keyword we actually found.
    let sx = find_last(bytes, b"startxref")?;
    let digits_start = skip_ws(bytes, sx + b"startxref".len());
    let (_, digits_end) = parse_uint(bytes, digits_start)?;
    let corrected = xref_pos.to_string();
    if out[digits_start..digits_end] != *corrected.as_bytes() {
        out.splice(digits_start..digits_end, corrected.into_bytes());
        changed = true;
    }

    changed.then_some(out)
}

fn find_last(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).rposition(|w| w == needle)
}

fn find_last_xref_keyword(bytes: &[u8]) -> Option<usize> {
    let mut before = bytes.len();
    while before >= 4 {
        let pos = bytes[..before].windows(4).rposition(|w| w == b"xref")?;
        let at_line_start = pos == 0 || matches!(bytes[pos - 1], b'\n' | b'\r');
        let followed_by_ws = bytes.get(pos + 4).map_or(false, |b| b.is_ascii_whitespace());
        if at_line_start && followed_by_ws {
            return Some(pos);
        }
        before = pos;
    }
    None
}

fn skip_ws(bytes: &[u8], mut p: usize) -> usize {
    while p < bytes.len() && bytes[p].is_ascii_whitespace() {
        p += 1;
    }
    p
}

fn parse_uint(bytes: &[u8], start: usize) -> Option<(u32, usize)> {
    let mut p = start;
    while p < bytes.len() && bytes[p].is_ascii_digit() {
        p += 1;
    }
    if p == start {
        return None;
    }
    let value = std::str::from_utf8(&bytes[start..p]).ok()?.parse().ok()?;
    Some((value, p))
}

/// Maps every object number to the byte offset of its `N G obj` header by
/// scanning the file body, so a stale xref entry can be pointed at the truth.
fn scan_object_offsets(bytes: &[u8]) -> std::collections::HashMap<u32, usize> {
    let mut found = std::collections::HashMap::new();
    let mut from = 0;
    while let Some(rel) = find_from(bytes, b" obj", from) {
        if let Some((num, start)) = parse_object_header(bytes, rel) {
            found.entry(num).or_insert(start);
        }
        from = rel + 4;
    }
    found
}

fn find_from(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if from >= hay.len() {
        return None;
    }
    hay[from..].windows(needle.len()).position(|w| w == needle).map(|p| from + p)
}

/// Parses the `<num> <gen>` immediately preceding the ` obj` at `space`, and
/// returns `(num, offset_of_num)` only when the header starts at a line
/// boundary (so a byte sequence inside a stream is not mistaken for one).
fn parse_object_header(bytes: &[u8], space: usize) -> Option<(u32, usize)> {
    if bytes.get(space + 1..space + 4)? != b"obj" {
        return None;
    }
    let mut k = space;
    let gen_end = k;
    while k > 0 && bytes[k - 1].is_ascii_digit() {
        k -= 1;
    }
    if k == gen_end || k == 0 || bytes[k - 1] != b' ' {
        return None;
    }
    k -= 1;
    let num_end = k;
    while k > 0 && bytes[k - 1].is_ascii_digit() {
        k -= 1;
    }
    if k == num_end || (k > 0 && !matches!(bytes[k - 1], b'\n' | b'\r')) {
        return None;
    }
    let num = std::str::from_utf8(&bytes[k..num_end]).ok()?.parse().ok()?;
    Some((num, k))
}

/// True when a page (or a Form XObject it draws, recursively) references fonts
/// or issues a text-show operator. This is what makes a page "digital" for
/// routing; it must resolve inherited `/Resources` and descend into forms.
fn page_has_text_layer(doc: &lopdf::Document, page_id: lopdf::ObjectId) -> bool {
    let chain = text_extract::resource_dicts(doc, page_id);
    let mut fonts = std::collections::BTreeMap::new();
    text_extract::collect_fonts(doc, &chain, &mut fonts);
    if !fonts.is_empty() {
        return true;
    }
    let Ok(content) = text_extract::decode_page_content(doc, page_id) else {
        return false;
    };
    let mut budget = text_extract::WalkerBudget::new();
    let found = content_has_text_layer(
        doc,
        &chain,
        &content.operations,
        &mut Vec::new(),
        0,
        &mut budget,
    );
    // The probe shares the walker's work bounds; a page whose probe was
    // truncated must not fail silently (it routes to rescue on an incomplete
    // scan).
    if budget.exhausted && std::env::var_os("PDF2MD_DEBUG").is_some() {
        eprintln!("pdf2md: digital-text-layer probe hit a work bound; result may be incomplete");
    }
    found
}

fn content_has_text_layer(
    doc: &lopdf::Document,
    chain: &[&lopdf::Dictionary],
    ops: &[lopdf::content::Operation],
    form_path: &mut Vec<lopdf::ObjectId>,
    depth: usize,
    budget: &mut text_extract::WalkerBudget,
) -> bool {
    if depth >= text_extract::MAX_WALKER_FORM_DEPTH {
        budget.exhausted = true;
        return false;
    }
    // Scan this stream's operators for a text-show, charging the shared page
    // budget so a form DAG cannot multiply the work (a page whose budget is
    // exhausted is reported as needing rescue rather than walked forever).
    for op in ops {
        if budget.ops_left == 0 {
            budget.exhausted = true;
            return false;
        }
        budget.ops_left -= 1;
        if matches!(op.operator.as_str(), "Tj" | "TJ" | "'" | "\"") {
            return true;
        }
    }
    for op in ops {
        if op.operator != "Do" {
            continue;
        }
        if budget.do_left == 0 {
            budget.exhausted = true;
            break;
        }
        let Some(name) = op.operands.first().and_then(|o| o.as_name().ok()) else {
            continue;
        };
        let Some((id, form)) = text_extract::lookup_form(doc, chain, name) else {
            continue;
        };
        // A form already on the current path draws itself (directly or through
        // a chain); stop instead of recursing.
        if id.map_or(false, |id| form_path.contains(&id)) {
            continue;
        }
        let fchain = text_extract::form_resource_chain(doc, &form.dict, chain);
        let mut fonts = std::collections::BTreeMap::new();
        text_extract::collect_fonts(doc, &fchain, &mut fonts);
        if !fonts.is_empty() {
            return true;
        }
        let Ok(data) = form.get_plain_content_with_limit(16 << 20) else {
            continue;
        };
        if data.len() > budget.bytes_left {
            budget.exhausted = true;
            continue;
        }
        let Ok(fc) = lopdf::content::Content::decode(&data) else {
            continue;
        };
        budget.bytes_left = budget.bytes_left.saturating_sub(data.len());
        budget.ops_left = budget
            .ops_left
            .saturating_sub(text_extract::FORM_INVOCATION_OPS);
        if budget.ops_left == 0 {
            budget.exhausted = true;
        }
        budget.do_left -= 1;
        if let Some(id) = id {
            form_path.push(id);
        }
        let found = content_has_text_layer(
            doc,
            &fchain,
            &fc.operations,
            form_path,
            depth + 1,
            budget,
        );
        if id.is_some() {
            form_path.pop();
        }
        if found {
            return true;
        }
    }
    false
}

/// Probes whether raw PDF bytes contain a digital text stream without full rendering.
pub fn is_digital_pdf_bytes(bytes: &[u8]) -> bool {
    if bytes.len() < 32 || !bytes.starts_with(b"%PDF-") {
        return false;
    }

    // Structural check: parse and look for a real text layer. Unlike a raw
    // string scan, this handles content streams that are FlateDecode-compressed
    // (where "BT"/"Tj" never appear in the raw bytes) — e.g. PDFCreator/
    // Ghostscript tickets and most modern PDFs. A page is "digital" if it
    // references any font (including one inherited from the /Pages tree) or
    // issues any text-show operator, descending recursively into Form XObjects.
    if let Ok(doc) = load_pdf_document(bytes) {
        for (_page_num, page_id) in doc.get_pages().into_iter().take(MAX_PAGES) {
            if page_has_text_layer(&doc, page_id) {
                return true;
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

/// Whether a block adjacent to a content image is that image's caption.
///
/// A short line close to the image is the classic caption. Academic papers,
/// however, use descriptive multi-line captions that easily exceed nine words,
/// so a `Figure`/`Fig.` prefixed paragraph within a looser gap is also a
/// caption (`**Figure 1: ...** The number of operations ...`).
fn is_caption_block(text: &str, gap: f64, size_hint: f64) -> bool {
    let caption_prefix = text.starts_with("Figure ")
        || text.starts_with("**Figure ")
        || text.starts_with("Fig. ")
        || text.starts_with("**Fig. ");
    (gap <= 2.5 * size_hint && text.split_whitespace().count() <= 9)
        || (caption_prefix && gap <= 4.0 * size_hint)
}

/// True when `c` is the kind of punctuation that separates a running footer's
/// fields ("… - page 2", "| page 2", "· page 2"). Used to tell a page-varying
/// footer apart from body prose that merely contains the word "page".
fn is_furniture_separator(c: char) -> bool {
    matches!(
        c,
        '-' | '\u{2013}' | '\u{2014}' | '\u{00b7}' | '|' | '\u{2022}' | '\u{00a9}' | '\u{00ae}' | ',' | ';' | ':'
    )
}

/// Mask a bare/short page-number token (`2`, `2/7`) that directly follows a
/// `page`/`p.` marker in a running header/footer. Anything longer than three
/// digits, or carrying any other character (amounts, dates, postal codes,
/// IBAN/SIRET runs), is rejected so two different values never compare equal.
fn mask_page_token(tok: &str) -> Option<String> {
    let trimmed = tok.trim_matches(|c: char| ".,;:*_()[]{}".contains(c));
    let parts: Vec<&str> = trimmed.split('/').collect();
    if parts.is_empty() || parts.len() > 2 {
        return None;
    }
    if !parts
        .iter()
        .all(|p| !p.is_empty() && p.len() <= 3 && p.chars().all(|c| c.is_ascii_digit()))
    {
        return None;
    }
    Some(parts.iter().map(|_| "#").collect::<Vec<_>>().join("/"))
}

/// Whether a short line carries a real data value that must never be treated
/// as running furniture: a decimal amount (`\d+[.,]\d{2}`) or a long digit run
/// (>= 4 contiguous digits). Identical fee/total rows often repeat in the
/// footer band of every page; suppressing them as a "running footer" deletes
/// real numbers. Short 3-digit identifier groups ("RCS 123 456 789") are not
/// protected, so a page-varying legal footer is still suppressed.
fn carries_data_value(t: &str) -> bool {
    let chars: Vec<char> = t.chars().collect();
    let mut i = 0usize;
    while i < chars.len() {
        if !chars[i].is_ascii_digit() {
            i += 1;
            continue;
        }
        let start = i;
        while i < chars.len() && chars[i].is_ascii_digit() {
            i += 1;
        }
        if i - start >= 4 {
            return true;
        }
        if i < chars.len() && (chars[i] == '.' || chars[i] == ',') {
            let mut j = i + 1;
            let mut decimals = 0usize;
            while j < chars.len() && chars[j].is_ascii_digit() {
                j += 1;
                decimals += 1;
            }
            if decimals == 2 {
                return true;
            }
        }
    }
    false
}

/// Normalize a short line for running-furniture comparison: lowercase, drop
/// punctuation, and collapse whitespace. A page number that directly follows a
/// `page`/`p.` marker *set off by a separator* ("… - page 2") is masked to `#`,
/// so a page-varying legal footer compares equal across pages; a bare number in
/// body prose ("corps page 2") is kept verbatim, as are amounts, dates and
/// postal/account runs, so unique per-page content is never treated as furniture.
fn furniture_line_key(t: &str) -> String {
    let toks: Vec<&str> = t.split_whitespace().collect();
    let mut out: Vec<String> = Vec::with_capacity(toks.len());
    let mut i = 0;
    while i < toks.len() {
        let tok = toks[i];
        let stripped: String = tok
            .chars()
            .filter(|c| c.is_alphanumeric())
            .collect::<String>()
            .to_lowercase();
        let marker = stripped == "page" || stripped == "p";
        let leading_sep = tok
            .chars()
            .take_while(|c| !c.is_alphanumeric())
            .any(is_furniture_separator);
        let prev_sep = i > 0 && toks[i - 1].chars().any(is_furniture_separator);
        if marker && (prev_sep || leading_sep || tok.ends_with('.')) {
            if let Some(masked) = toks.get(i + 1).and_then(|n| mask_page_token(n)) {
                if !stripped.is_empty() {
                    out.push(stripped);
                }
                out.push(masked);
                i += 2;
                continue;
            }
        }
        if !stripped.is_empty() {
            out.push(stripped);
        }
        i += 1;
    }
    out.join(" ")
}

/// Parse a *pure* page-counter line — `Page N`, `Page N/M`, `N/M`, `N / M`,
/// `N sur M` — alone on its line. A bare `N` counts only with the `Page`
/// marker. Returns `(N, M)`.
fn parse_page_counter(t: &str) -> Option<(u32, Option<u32>)> {
    let lower = t.trim().to_lowercase();
    let (body, marked) = match lower.strip_prefix("page") {
        Some(rest) => (rest, true),
        None => (lower.as_str(), false),
    };
    let compact: String = body.chars().filter(|c| !c.is_whitespace()).collect();
    if compact.is_empty() {
        return None;
    }
    if let Some((a, b)) = compact.split_once('/') {
        if a.is_empty() || b.is_empty() || b.contains('/') {
            return None;
        }
        return Some((a.parse().ok()?, Some(b.parse().ok()?)));
    }
    if let Some((a, b)) = compact.split_once("sur") {
        if a.is_empty() || b.is_empty() {
            return None;
        }
        return Some((a.parse().ok()?, Some(b.parse().ok()?)));
    }
    if marked {
        return compact.parse::<u32>().ok().map(|n| (n, None));
    }
    None
}

/// Drop pure page-counter lines that are *corroborated* as counters: they sit
/// in a page's top/bottom band, their `N` differs across pages, and their `M`
/// is consistent (all equal, or equal to the page count). A standalone ratio
/// cell (`3/4` repeated unchanged on every page) and a `1/2` mid-page are
/// therefore kept.
fn suppress_page_counters(page_md: &mut [(u32, String)]) {
    use std::collections::HashSet;
    const BAND: usize = 3;
    if page_md.len() < 2 {
        return;
    }
    let mut ns: HashSet<u32> = HashSet::new();
    let mut ms: Vec<u32> = Vec::new();
    let mut pages_with: HashSet<u32> = HashSet::new();
    for (page, chunk) in page_md.iter() {
        let lines: Vec<&str> = chunk.lines().filter(|l| !l.trim().is_empty()).collect();
        let n = lines.len();
        for (i, l) in lines.iter().enumerate() {
            if i >= BAND && i + BAND < n {
                continue;
            }
            if let Some((cn, cm)) = parse_page_counter(l) {
                ns.insert(cn);
                if let Some(m) = cm {
                    ms.push(m);
                }
                pages_with.insert(*page);
            }
        }
    }
    if pages_with.len() < 2 || ns.len() < 2 {
        return;
    }
    let m_ok = ms.is_empty() || ms.iter().all(|m| *m == ms[0]);
    if !m_ok {
        return;
    }
    for (_page, chunk) in page_md.iter_mut() {
        let lines: Vec<&str> = chunk.lines().filter(|l| !l.trim().is_empty()).collect();
        let n = lines.len();
        let mut out = String::with_capacity(chunk.len());
        let mut idx = 0usize;
        for l in chunk.lines() {
            let drop = if l.trim().is_empty() {
                false
            } else {
                let here = idx;
                idx += 1;
                (here < BAND || here + BAND >= n) && parse_page_counter(l).is_some()
            };
            if !drop {
                out.push_str(l);
                out.push('\n');
            }
        }
        if out.ends_with('\n') {
            out.pop();
        }
        *chunk = out;
    }
}

/// Suppress running headers/footers in the assembled per-page markdown for
/// documents whose pages carry no structured blocks (the string-walker path),
/// where `tag_running_furniture` has nothing to inspect.
///
/// Only a page's first/last few non-empty lines are candidates, and a line is
/// dropped only when its key recurs on at least three pages (keeping its first
/// occurrence). A page whose whole body is short repeats (a ticket printed
/// identically on every page) is skipped entirely so its content survives.
/// Table rows, images, headings and letterless data lines are never candidates.
fn strip_running_lines(page_md: &mut [(u32, String)]) {
    use std::collections::{HashMap, HashSet};
    const BAND: usize = 3;
    const MIN_PAGES: u32 = 3;
    if page_md.len() < 2 {
        return;
    }
    let is_candidate = |l: &str| -> bool {
        let t = l.trim();
        !t.is_empty()
            && t.chars().any(|c| c.is_alphabetic())
            && !carries_data_value(t)
            && !t.starts_with('|')
            && !t.starts_with('<')
            && !t.starts_with('#')
            && !t.starts_with("![")
            && t.split_whitespace().count() <= 14
    };
    // A wholly-repeated sparse page (fewer than 2*BAND non-empty lines) is
    // content, not furniture: every line would fall in a band and be dropped.
    let sparse = |chunk: &str| chunk.lines().filter(|l| !l.trim().is_empty()).count() <= 2 * BAND;

    let mut first_seen: HashMap<String, u32> = HashMap::new();
    let mut page_count: HashMap<String, u32> = HashMap::new();
    for (page, chunk) in page_md.iter() {
        if sparse(chunk) {
            continue;
        }
        let lines: Vec<&str> = chunk.lines().filter(|l| !l.trim().is_empty()).collect();
        let n = lines.len();
        let mut seen_here: HashSet<String> = HashSet::new();
        for (i, l) in lines.iter().enumerate() {
            if !is_candidate(l) || (i >= BAND && i + BAND < n) {
                continue;
            }
            let key = furniture_line_key(l);
            if key.is_empty() {
                continue;
            }
            first_seen.entry(key.clone()).or_insert(*page);
            if seen_here.insert(key.clone()) {
                *page_count.entry(key).or_insert(0) += 1;
            }
        }
    }
    let repeated: HashSet<String> = page_count
        .into_iter()
        .filter(|(_, c)| *c >= MIN_PAGES)
        .map(|(k, _)| k)
        .collect();

    for (page, chunk) in page_md.iter_mut() {
        if sparse(chunk) {
            continue;
        }
        let n = chunk.lines().filter(|l| !l.trim().is_empty()).count();
        let mut out = String::with_capacity(chunk.len());
        let mut idx = 0usize;
        for l in chunk.lines() {
            let drop = if l.trim().is_empty() {
                false
            } else {
                let here = idx;
                idx += 1;
                if is_candidate(l) && (here < BAND || here + BAND >= n) {
                    let key = furniture_line_key(l);
                    repeated.contains(&key)
                        && first_seen.get(&key).copied().unwrap_or(*page) < *page
                } else {
                    false
                }
            };
            if !drop {
                out.push_str(l);
                out.push('\n');
            }
        }
        if out.ends_with('\n') {
            out.pop();
        }
        *chunk = out;
    }
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
    let norm = |t: &str| -> String { furniture_line_key(t) };
    let mut header_counts: HashMap<String, usize> = HashMap::new();
    let mut footer_counts: HashMap<String, usize> = HashMap::new();
    for b in blocks.iter() {
        if b.kind != "body" && b.kind != "list" && b.kind != "heading" {
            continue;
        }
        if b.text.split_whitespace().count() > 14 {
            continue; // paragraphs aren't furniture
        }
        // A line with no letters is a data value, not furniture; digit
        // normalization would otherwise collapse distinct numbers ("0.19" /
        // "0.29" → "#.##") into one bogus repeated header. A line that *does*
        // carry an amount or a long number is equally data: identical fee/total
        // rows repeat in the footer band of every page and must survive.
        if !b.text.chars().any(|c| c.is_alphabetic()) || carries_data_value(&b.text) {
            continue;
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
        if !b.text.chars().any(|c| c.is_alphabetic()) || carries_data_value(&b.text) {
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

    // Try parsing with lopdf, falling back to a cross-reference repair for the
    // real-world producers whose `startxref`/object offsets have drifted.
    let doc = load_pdf_document(bytes)?;

    let mut full_markdown = String::new();
    let total_pages = doc.get_pages().len();
    if total_pages > MAX_PAGES {
        return Err(format!(
            "document has {total_pages} pages, over the {MAX_PAGES}-page limit"
        ));
    }
    let mut total_words = 0;
    // Pages whose own word count falls below `options.min_words_per_page`.
    // Unlike `total_words == 0` (checked once, document-wide, below), this
    // catches the case a whole-document zero-check misses entirely: a
    // mostly-scanned document where a handful of pages carry real text (a
    // cover sheet, a signed last page) so the document-level total is well
    // above zero, but most individual pages are near-empty. Callers (the
    // gateway's escalation decision) compare this against `total_pages` as a
    // ratio — a document-wide count alone can't distinguish "a few blank
    // pages in an otherwise fine document" from "mostly blank".
    let mut pages_below_word_floor = 0usize;
    let mut any_text_ops = false;
    let mut any_fonts = false;
    let mut tables_detected = 0usize;
    // True when any page's extraction hit a work bound and truncated.
    let mut budget_exhausted = false;
    let mut media_items: Vec<media::MediaItem> = Vec::new();
    // Running total of base64 bytes inlined into the markdown as `data:`
    // URIs so far, across every page. Once `options.max_media_bytes_per_doc`
    // is reached, further images are replaced with a text placeholder
    // instead of another data URI (see the `embed_media` loop below) — this
    // is what actually bounds the emitted markdown size for scan-heavy
    // documents; the per-image downscale in `media::extract_page_media`
    // only shrinks the common case.
    let mut media_budget_used: usize = 0;
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
        let (page_text, marker_hint) = {
            let geo = text_extract::extract_page_text_report_with_marker(
                &doc,
                page_num,
                options.detect_tables,
                options.detect_layout,
                options.detect_math,
            );
            let (geo, geo_hint) = match geo {
                Ok(pair) => pair,
                Err(_) => (
                    text_extract::PageText {
                        text: doc
                            .extract_text_with_limit(
                                &[page_num],
                                text_extract::MAX_PAGE_CONTENT_TOTAL,
                            )
                            .unwrap_or_default(),
                        text_ops_seen: true,
                        has_fonts: !text_extract::page_fonts(&doc, page_id).is_empty(),
                        tables: 0,
                        blocks: Vec::new(),
                        budget_exhausted: false,
                    },
                    None,
                ),
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
                        (
                            text_extract::PageText {
                                text: tagged.text,
                                text_ops_seen: true,
                                has_fonts: true,
                                tables: tagged.tables,
                                blocks: tagged.blocks,
                                budget_exhausted: false,
                            },
                            None,
                        )
                    } else {
                        (geo, geo_hint)
                    }
                } else {
                    (geo, geo_hint)
                }
            } else {
                (geo, geo_hint)
            }
        };
        any_text_ops |= page_text.text_ops_seen;
        any_fonts |= page_text.has_fonts;
        tables_detected += page_text.tables;
        budget_exhausted |= page_text.budget_exhausted;
        // Fold presentation-form ligatures, drop soft hyphens and normalise the
        // French digit-group no-break spaces, consistently for the string
        // walker and the geometry path (the geometry path already folds
        // ligatures, so this is a no-op there for those).
        let text = text_extract::normalize_decoded_text(&page_text.text);
        let mut page_blocks: Vec<layout::DocBlock> = page_text.blocks;
        for b in &mut page_blocks {
            b.page = page_num as usize;
            b.text = text_extract::normalize_decoded_text(&b.text);
        }

        let page_media: Vec<media::MediaItem> = if options.detect_media {
            let page_bbox = page_media_box(&doc, page_id);
            let mut m = media::extract_page_media(
                &doc,
                page_id,
                page_num as usize,
                page_bbox,
                options.max_image_dimension,
            );
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
                        if is_caption_block(&b.text, gap, size_hint) {
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
        if words.len() < options.min_words_per_page {
            pages_below_word_floor += 1;
        }
        for m in &page_media {
            media_items.push(m.clone());
        }

        let mut chunk = String::new();
        // A newly-routed page reports the marker decision from the string
        // walker (`marker_hint`), so routing does not move its page boundary.
        let heading_like = marker_hint.unwrap_or_else(|| {
            text.starts_with("# ")
                || text.lines().next().map_or(false, |l| {
                    l.len() < 60 && l.chars().all(|c| c.is_alphanumeric() || c.is_whitespace())
                })
        });
        if options.detect_headings && heading_like {
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
                let b64_len = m.data_b64.len();
                // Inline payload to splice into the markdown: either the
                // original data URI or an adaptive shrink when the full-size
                // copy would push the running total over budget. `None` is the
                // true last resort (omission placeholder).
                let mut inline: Option<(String, &str)> = None;
                if media_budget_used.saturating_add(b64_len) <= options.max_media_bytes_per_doc {
                    media_budget_used += b64_len;
                    inline = Some((m.data_b64.clone(), m.format.as_str()));
                } else {
                    // Over the per-document embed budget: before falling back
                    // to the omission placeholder, try progressively
                    // downscaling (and, under `vision`, JPEG-recompressing)
                    // the figure so it fits the *remaining* budget. Only this
                    // markdown copy is shrunk — `media_items`/`media` (the
                    // JSON side-channel, pushed above) keeps the full bytes.
                    let remaining =
                        options.max_media_bytes_per_doc.saturating_sub(media_budget_used);
                    if let Some((bytes, mime)) =
                        media::raster::shrink_encoded_image_to_fit(&m.data, &m.format, remaining)
                    {
                        let b64 = media::b64encode(&bytes);
                        media_budget_used += b64.len();
                        inline = Some((b64, mime));
                    }
                }
                let img_md = match inline {
                    Some((b64, mime)) => match embed_width_frac(m, page_bbox) {
                        Some(frac) => format!(
                            "<img alt=\"{}\" src=\"data:{};base64,{}\" width=\"{}\" />\n\n",
                            m.kind.as_str(),
                            mime,
                            b64,
                            // Emit as a percent of the reader's content width, which
                            // matches the fraction of the page the image occupied.
                            format!("{}%", (frac * 100.0).round() as u32),
                        ),
                        None => format!(
                            "![{}](data:{};base64,{})\n\n",
                            m.kind.as_str(),
                            mime,
                            b64
                        ),
                    },
                    // Even the pixel/quality floor does not fit: keep the
                    // figure's reading-order position (so surrounding text
                    // still makes sense) but drop the payload instead of
                    // inlining another multi-hundred-KB data URI.
                    None => format!(
                        "*[{} omitted: {}x{} {}, {} KB — over the {} KB per-document image budget]*\n\n",
                        m.kind.as_str(),
                        m.width,
                        m.height,
                        m.format,
                        b64_len / 1024,
                        options.max_media_bytes_per_doc / 1024,
                    ),
                };

                // Prefer the figure's caption as the anchor when one sits
                // below/near the image and horizontally overlaps it (or spans
                // the page): a descriptive caption is the true reading-order
                // neighbour, and inserting before it keeps the figure next to
                // its caption. Otherwise pick the nearest block immediately
                // below (max mid-y) that horizontally overlaps the image, so a
                // footnote or a different-column block is not used and the
                // figure is not displaced far from its caption.
                let mw = (m.x1 - m.x0).abs();
                let page_w = page_bbox.map_or(0.0, |(px0, _, px1, _)| (px1 - px0).abs());
                let caption_block = page_blocks
                    .iter()
                    .filter(|b| b.kind == "caption")
                    .filter(|b| {
                        let overlap = b.x1.min(m.x1) - b.x0.max(m.x0);
                        let full_width = page_w > 0.0 && (b.x1 - b.x0).abs() >= 0.9 * page_w;
                        overlap > 0.0 || full_width
                    })
                    .filter(|b| {
                        let size_hint = (b.y1 - b.y0).abs().max(8.0);
                        (b.y0 + b.y1) / 2.0 <= my + 4.0 * size_hint
                    })
                    .max_by(|a, b| {
                        let ay = (a.y0 + a.y1) / 2.0;
                        let by = (b.y0 + b.y1) / 2.0;
                        ay.partial_cmp(&by).unwrap_or(std::cmp::Ordering::Equal)
                    });
                let next_block = caption_block.or_else(|| {
                    page_blocks
                        .iter()
                        .filter(|b| b.kind != "figure")
                        .filter(|b| (b.y0 + b.y1) / 2.0 <= my)
                        .filter(|b| {
                            let bw = (b.x1 - b.x0).abs();
                            let overlap = b.x1.min(m.x1) - b.x0.max(m.x0);
                            overlap > 0.2 * mw.min(bw) || mw > 300.0
                        })
                        .max_by(|a, b| {
                            let ay = (a.y0 + a.y1) / 2.0;
                            let by = (b.y0 + b.y1) / 2.0;
                            ay.partial_cmp(&by).unwrap_or(std::cmp::Ordering::Equal)
                        })
                });

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
                            // `search_key` is plain block text (from
                            // build_doc_blocks) with no structural prefix, so a
                            // match can land mid-line — e.g. after the "# "/
                            // "- "/"1. " a heading/list line now carries. Snap
                            // back to the start of that line so the image is
                            // never spliced into the middle of a marker,
                            // orphaning it as a bare "#" with nothing after.
                            let line_start = processed_text[..pos].rfind('\n').map_or(0, |i| i + 1);
                            pos = line_start;
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
    //  * text-show operators seen but zero words decoded -> the text layer is
    //    unusable; glyph-encoded/outlined fonts, a missing or broken ToUnicode
    //    CMap, and a corrupt font program are all possible. The font subtype is
    //    not inspected here, so the message must not assert a specific one;
    //  * no fonts and no text-show operators at all -> scanned/image page.
    if total_words == 0 {
        if any_text_ops {
            return Err(
                "Document draws text (text-show operators present) but no readable words were decoded; \
                 the text layer may be glyph-encoded or outlined (e.g. Type3), may lack a usable Unicode \
                 mapping (e.g. a missing or broken ToUnicode CMap on a Type0/CID font), or its embedded \
                 font program may be corrupt — route through the OCR/Vision pipeline (vision-LLM rescue)."
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
        // repeated occurrences from every later page of the markdown. Matching
        // is on the normalized key (which masks only a page-number tail set off
        // by a separator), so a page-varying footer is suppressed while body
        // prose and data values are kept.
        let mut furniture: Vec<(String, u32)> = Vec::new();
        for b in block_items
            .iter()
            .filter(|b| b.kind == "header" || b.kind == "footer")
        {
            let t = b.text.trim();
            if t.len() <= 1 {
                continue;
            }
            let key = furniture_line_key(t);
            match furniture.iter_mut().find(|(fk, _)| *fk == key) {
                Some((_, first)) => *first = (*first).min(b.page as u32),
                None => furniture.push((key, b.page as u32)),
            }
        }
        for (page, chunk) in page_md.iter_mut() {
            let mut out = String::with_capacity(chunk.len());
            for ln in chunk.lines() {
                let drop = !ln.trim().is_empty()
                    && !ln.trim().starts_with('|')
                    && furniture.iter().any(|(key, first_page)| {
                        *page > *first_page && furniture_line_key(ln.trim()) == *key
                    });
                if !drop {
                    out.push_str(ln);
                    out.push('\n');
                }
            }
            if out.ends_with('\n') {
                out.pop();
            }
            *chunk = out;
        }
    } else if options.detect_layout {
        strip_running_lines(&mut page_md);
    }
    // Pure page counters are corroborated at the document level (they vary
    // across pages and share a page count) before any are dropped, so a
    // standalone ratio/data value survives.
    if options.detect_layout {
        suppress_page_counters(&mut page_md);
    }
    for (_p, chunk) in page_md {
        // Paragraph reflow: join the walker's one-line-per-PDF-line output
        // back into paragraphs. Run per page, after the line-based furniture
        // and page-counter passes, so a running header or a suppressed
        // counter is never welded into body prose.
        let reflowed = reflow::reflow_markdown(&chunk);
        full_markdown.push_str(&reflowed);
    }

    let duration_us = t0.elapsed_us();

    // The C ABI/JSON surface (ffi.rs) is frozen, so the Go worker cannot see
    // the flag through `ConversionResult`; make the truncation visible on the
    // debug channel instead of failing silently.
    if budget_exhausted && std::env::var_os("PDF2MD_DEBUG").is_some() {
        eprintln!("pdf2md: a page hit an extraction work bound; output may be truncated");
    }

    Ok(ConversionResult {
        markdown: full_markdown,
        total_pages,
        total_words,
        pages_below_word_floor,
        tables_detected,
        duration_us,
        budget_exhausted,
        media: media_items,
        blocks: block_items,
    })
}

#[cfg(test)]
mod regression_tests {
    use super::*;
    use lopdf::dictionary;

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

            // 2b. Regression test for a real bug: this document has zero
            // genuine built-up fractions, but before the is_fraction_bar /
            // detect_stacked_fractions fixes, a decorative rule under "NOUS
            // CONTACTER" (misread as a fraction bar between it and the
            // unrelated "N° client" line below) and a vertical reference-
            // number strip near "Nimes, le 19 mai 2026" (misread as a long
            // chain of stacked fractions, one digit per pseudo-fraction)
            // both produced spurious `\frac{...}{...}` output. The
            // `md.contains("NOUS CONTACTER")` check above alone would not
            // have caught this — that substring survives fine inside
            // `$\frac{NOUS CONTACTER}{...}$` too.
            assert!(
                !res.markdown.contains(r"\frac{"),
                "this document has no genuine built-up fractions; any \\frac{{}} is a false positive:\n{}",
                res.markdown
            );

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

    #[test]
    fn image_insertion_never_splits_a_structural_prefix() {
        // Regression test for a real bug R7 exposed: the figure-insertion pass
        // finds an insertion point by searching for a block's plain text (no
        // "#"/"-"/"1. " prefix) inside the already-rendered markdown. Once
        // headings/lists gained real Markdown prefixes, a match could land
        // mid-line — right after the "# " — splicing the image in between and
        // leaving an orphaned "# " with nothing after it, and the heading's own
        // text stranded, unprefixed, below the image. The fix snaps the
        // insertion point back to the start of the matched line.
        let pdf_path = "../../../scratch/tests/fixtures/billet_electronique.pdf";
        if let Ok(bytes) = std::fs::read(pdf_path) {
            let res = convert_pdf_bytes_to_markdown(&bytes, &ConversionOptions::default()).unwrap();
            let md = &res.markdown;
            for line in md.lines() {
                let trimmed = line.trim_end();
                let after_hashes = trimmed.trim_start_matches('#');
                if after_hashes.len() == trimmed.len() {
                    continue; // doesn't start with '#' — not a heading line
                }
                // A line starting with one or more '#' must be a real heading
                // marker (space then non-empty text), never a bare "#"/"##"
                // run with nothing — or only whitespace — after it.
                assert!(
                    after_hashes.starts_with(' ') && !after_hashes.trim().is_empty(),
                    "heading marker with no text after it: {trimmed:?}\nfull markdown:\n{md}"
                );
            }
        }
    }

    #[test]
    fn media_budget_replaces_data_uri_with_placeholder_once_exhausted() {
        let pdf_path = "../../../scratch/tests/fixtures/synth_logo_image.pdf";
        if let Ok(bytes) = std::fs::read(pdf_path) {
            let opts = ConversionOptions {
                max_media_bytes_per_doc: 1, // any real image blows this instantly
                ..Default::default()
            };
            let res = convert_pdf_bytes_to_markdown(&bytes, &opts).unwrap();
            let md = &res.markdown;

            assert!(
                !md.contains("data:image/"),
                "over-budget image must not be inlined as a data URI, got: {md}"
            );
            assert!(
                md.contains("omitted") && md.contains("per-document image budget"),
                "over-budget image must leave a placeholder, got: {md}"
            );
            // The JSON media side-channel is a separate opt-in payload and must
            // still carry the full image — only the inline markdown embed is
            // budget-capped.
            assert!(
                res.media.iter().any(|m| !m.data_b64.is_empty()),
                "media list must still report the full image out-of-band"
            );
        }
    }

    #[test]
    fn media_budget_default_still_embeds_a_normal_sized_image() {
        let pdf_path = "../../../scratch/tests/fixtures/synth_logo_image.pdf";
        if let Ok(bytes) = std::fs::read(pdf_path) {
            let res = convert_pdf_bytes_to_markdown(&bytes, &ConversionOptions::default()).unwrap();
            assert!(
                res.markdown.contains("data:image/"),
                "a small fixture image must not trip the default 512KB budget"
            );
        }
    }

    /// Build a one-page PDF with a single uncompressed `DeviceRGB` image
    /// XObject of deterministic pseudo-random noise. Noise makes the
    /// reconstructed PNG effectively incompressible (its size scales with
    /// pixel area), which is exactly the shape the adaptive embed-budget step
    /// has to handle. The image is painted at 250x200 pt on a 400x400 page so
    /// `classify_geometry` sees a non-decorative chart, not a full-page
    /// background.
    fn synthetic_noise_image_pdf(width: u32, height: u32) -> Vec<u8> {
        let mut samples = Vec::with_capacity(width as usize * height as usize * 3);
        let mut state: u32 = 0x1234_5678;
        for _ in 0..(width as usize * height as usize * 3) {
            // xorshift32 — deterministic and dependency-free.
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            samples.push((state & 0xff) as u8);
        }

        let mut doc = lopdf::Document::new();
        let mut img_dict = lopdf::Dictionary::new();
        img_dict.set(b"Type", lopdf::Object::Name(b"XObject".to_vec()));
        img_dict.set(b"Subtype", lopdf::Object::Name(b"Image".to_vec()));
        img_dict.set(b"Width", lopdf::Object::Integer(width as i64));
        img_dict.set(b"Height", lopdf::Object::Integer(height as i64));
        img_dict.set(b"ColorSpace", lopdf::Object::Name(b"DeviceRGB".to_vec()));
        img_dict.set(b"BitsPerComponent", lopdf::Object::Integer(8));
        let img_id = doc.add_object(lopdf::Object::Stream(lopdf::Stream::new(img_dict, samples)));

        // A minimal Helvetica text layer: without it `convert_pdf_bytes_to_markdown`
        // rejects the synthetic page as a scan before the embed loop runs.
        let mut font_dict = lopdf::Dictionary::new();
        font_dict.set(b"Type", lopdf::Object::Name(b"Font".to_vec()));
        font_dict.set(b"Subtype", lopdf::Object::Name(b"Type1".to_vec()));
        font_dict.set(b"BaseFont", lopdf::Object::Name(b"Helvetica".to_vec()));
        font_dict.set(b"Encoding", lopdf::Object::Name(b"WinAnsiEncoding".to_vec()));
        let font_id = doc.add_object(lopdf::Object::Dictionary(font_dict));

        let content = "BT /F1 12 Tf 60 360 Td (Synthetic sample text for the media budget test) Tj ET\n\
                       q 250 0 0 200 75 100 cm /Im0 Do Q\n";
        let content_id = doc.add_object(lopdf::Object::Stream(lopdf::Stream::new(
            lopdf::Dictionary::new(),
            content.as_bytes().to_vec(),
        )));

        let mut page_dict = lopdf::Dictionary::new();
        page_dict.set(b"Type", lopdf::Object::Name(b"Page".to_vec()));
        page_dict.set(
            b"MediaBox",
            lopdf::Object::Array(vec![
                lopdf::Object::Integer(0),
                lopdf::Object::Integer(0),
                lopdf::Object::Integer(400),
                lopdf::Object::Integer(400),
            ]),
        );
        let mut xobj = lopdf::Dictionary::new();
        xobj.set(b"Im0", lopdf::Object::Reference(img_id));
        let mut font_res = lopdf::Dictionary::new();
        font_res.set(b"F1", lopdf::Object::Reference(font_id));
        let mut res_dict = lopdf::Dictionary::new();
        res_dict.set(b"XObject", lopdf::Object::Dictionary(xobj));
        res_dict.set(b"Font", lopdf::Object::Dictionary(font_res));
        page_dict.set(b"Resources", lopdf::Object::Dictionary(res_dict));
        page_dict.set(b"Contents", lopdf::Object::Reference(content_id));
        let page_id = doc.add_object(lopdf::Object::Dictionary(page_dict));

        let mut pages_dict = lopdf::Dictionary::new();
        pages_dict.set(b"Type", lopdf::Object::Name(b"Pages".to_vec()));
        pages_dict.set(
            b"Kids",
            lopdf::Object::Array(vec![lopdf::Object::Reference(page_id)]),
        );
        pages_dict.set(b"Count", lopdf::Object::Integer(1));
        let pages_id = doc.add_object(lopdf::Object::Dictionary(pages_dict));

        let mut catalog_dict = lopdf::Dictionary::new();
        catalog_dict.set(b"Type", lopdf::Object::Name(b"Catalog".to_vec()));
        catalog_dict.set(b"Pages", lopdf::Object::Reference(pages_id));
        let catalog_id = doc.add_object(lopdf::Object::Dictionary(catalog_dict));
        doc.trailer.set(b"Root", lopdf::Object::Reference(catalog_id));

        let mut bytes = Vec::new();
        doc.save_to(&mut bytes).expect("save synthetic image pdf");
        bytes
    }

    /// Extract the base64 payload of the first inlined data URI.
    fn first_inline_b64_len(markdown: &str) -> Option<usize> {
        let start = markdown.find("base64,")? + "base64,".len();
        let end = markdown[start..]
            .find(|c| c == '"' || c == ')')
            .map(|i| start + i)
            .unwrap_or(markdown.len());
        Some(end - start)
    }

    #[test]
    fn media_budget_downscales_an_oversized_image_to_fit() {
        let bytes = synthetic_noise_image_pdf(1000, 1000);
        // The full-size PNG is several MB of base64; 1.5 MB can only be met
        // after the adaptive downscale, and sits well above the ~400 px floor
        // so the figure must be embedded rather than omitted.
        let opts = ConversionOptions {
            max_media_bytes_per_doc: 1_500_000,
            ..Default::default()
        };
        let res = convert_pdf_bytes_to_markdown(&bytes, &opts).unwrap();
        let md = &res.markdown;
        assert!(
            md.contains("data:image/"),
            "an oversized image must be adaptively downscaled and embedded, got: {md}"
        );
        // The default (non-`vision`) build has no JPEG codec, so its only
        // shrink path is a PNG downscale.
        #[cfg(not(feature = "vision"))]
        assert!(
            md.contains("data:image/png"),
            "the default build must re-encode the shrunk copy as PNG, got: {md}"
        );
        assert!(
            !md.contains("omitted"),
            "a downscaled-to-fit image must not fall back to the placeholder"
        );
        // The JSON side-channel keeps the full-fidelity bytes; only the inline
        // markdown copy is shrunk.
        let full = res
            .media
            .iter()
            .find(|m| !m.data_b64.is_empty())
            .expect("media side-channel must still carry the image");
        let inline = first_inline_b64_len(md).expect("inline data URI");
        assert!(
            inline <= opts.max_media_bytes_per_doc,
            "inlined payload ({inline}) must respect the budget"
        );
        assert!(
            inline < full.data_b64.len(),
            "inline copy ({inline}) must be the shrunk one while the side-channel keeps the full {} bytes",
            full.data_b64.len()
        );
    }

    #[test]
    fn media_budget_omits_when_the_downscale_floor_still_exceeds_budget() {
        let bytes = synthetic_noise_image_pdf(1000, 1000);
        // Far below what even the ~400 px floor can fit, so the omission
        // placeholder remains the true last resort.
        let opts = ConversionOptions {
            max_media_bytes_per_doc: 32 * 1024,
            ..Default::default()
        };
        let res = convert_pdf_bytes_to_markdown(&bytes, &opts).unwrap();
        let md = &res.markdown;
        assert!(
            !md.contains("data:image/"),
            "nothing should inline at a 32 KB budget, got: {md}"
        );
        assert!(
            md.contains("omitted") && md.contains("per-document image budget"),
            "a still-over-budget-at-the-floor image must leave the placeholder, got: {md}"
        );
    }

    #[cfg(feature = "vision")]
    #[test]
    fn media_budget_vision_jpeg_reencode_fits_where_png_downscale_cannot() {
        let bytes = synthetic_noise_image_pdf(1000, 1000);
        // Calibrated so the PNG downscale floor (~600 KB of base64 for this
        // noise image) still exceeds the budget while a JPEG re-encode at the
        // quality floor fits. Without the vision JPEG path this case would be
        // omitted.
        let budget = 300 * 1024;
        let opts = ConversionOptions {
            max_media_bytes_per_doc: budget,
            ..Default::default()
        };
        let res = convert_pdf_bytes_to_markdown(&bytes, &opts).unwrap();
        let md = &res.markdown;
        assert!(
            md.contains("data:image/jpeg"),
            "vision build must JPEG-recompress to fit, got: {md}"
        );
        assert!(
            !md.contains("omitted"),
            "JPEG re-encode should avoid the placeholder, got: {md}"
        );
        let inline = first_inline_b64_len(md).expect("inline data URI");
        assert!(
            inline <= budget,
            "inlined JPEG ({inline}) must respect the budget"
        );
        // Side-channel still advertises the original PNG, untouched.
        let full = res
            .media
            .iter()
            .find(|m| !m.data_b64.is_empty())
            .expect("media side-channel must still carry the image");
        assert_eq!(full.format, "image/png");
        assert!(
            inline < full.data_b64.len(),
            "inlined JPEG ({inline}) must be smaller than the full PNG ({})",
            full.data_b64.len()
        );
    }

    #[test]
    fn pages_below_word_floor_is_low_for_a_text_rich_document() {
        let pdf_path = "../../../scratch/samples/edf-facture-complex.pdf";
        if let Ok(bytes) = std::fs::read(pdf_path) {
            let res = convert_pdf_bytes_to_markdown(&bytes, &ConversionOptions::default()).unwrap();
            // A real 10-page invoice legitimately has a couple of sparse pages
            // (a mostly-blank separator, a footer-only page) below the 5-word
            // floor without the document being "mostly scanned" — the gateway's
            // escalation decision cares about the *ratio* (well under its ~30%
            // threshold here), not a strict zero.
            assert!(
                res.pages_below_word_floor <= 2,
                "expected at most 2 of 10 pages below the word floor, got {} (failures would indicate the \
                 per-page counter is over-firing, not that the document changed)",
                res.pages_below_word_floor
            );
        }
    }

    #[test]
    fn pages_below_word_floor_counts_pages_under_a_custom_threshold() {
        // This fixture's single page carries exactly 5 words of real text next
        // to an embedded logo image — below the default floor (5) it passes,
        // but a caller asking for a stricter per-page floor must see it counted.
        let pdf_path = "../../../scratch/tests/fixtures/synth_logo_image.pdf";
        if let Ok(bytes) = std::fs::read(pdf_path) {
            let opts = ConversionOptions {
                min_words_per_page: 10,
                ..Default::default()
            };
            let res = convert_pdf_bytes_to_markdown(&bytes, &opts).unwrap();
            assert_eq!(res.total_pages, 1);
            assert_eq!(
                res.pages_below_word_floor, 1,
                "the single page must be counted below a raised 10-word floor"
            );
        }
    }

    /// Builds a minimal PDF whose classic xref table points every object two
    /// bytes late and whose `startxref` is likewise stale — the drift the real
    /// FNFE/Factur-X French invoice fixtures ship. The object bodies are valid.
    fn drifted_xref_pdf() -> Vec<u8> {
        let objects: [&[u8]; 4] = [
            b"<< /Type /Catalog /Pages 2 0 R >>",
            b"<< /Type /Pages /Kids [ 3 0 R ] /Count 1 >>",
            b"<< /Type /Page /Parent 2 0 R /MediaBox [ 0 0 200 200 ] /Contents 4 0 R /Resources << >> >>",
            b"<< /Length 0 >>\nstream\n\nendstream",
        ];
        let mut body = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (i, obj) in objects.iter().enumerate() {
            offsets.push(body.len());
            body.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
            body.extend_from_slice(obj);
            body.extend_from_slice(b"\nendobj\n");
        }
        let xref_pos = body.len();
        let mut tail = format!("xref\n0 {}\n", objects.len() + 1).into_bytes();
        tail.extend_from_slice(b"0000000000 65535 f \n");
        for off in &offsets {
            // Deliberately stale: two bytes past the true object header.
            tail.extend_from_slice(format!("{:010} 00000 n \n", off + 2).as_bytes());
        }
        tail.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{}\n%%EOF\n",
                objects.len() + 1,
                xref_pos + 2
            )
            .as_bytes(),
        );
        body.extend_from_slice(&tail);
        body
    }

    #[test]
    fn stale_startxref_and_object_offsets_are_repaired() {
        let bytes = drifted_xref_pdf();
        // The raw bytes are genuinely unparseable without the repair: this is
        // the exact failure that made a whole FNFE FR invoice fail Tier A.
        assert!(lopdf::Document::load_mem(&bytes).is_err());
        let doc = load_pdf_document(&bytes).expect("repair must recover a drifted xref");
        assert_eq!(doc.get_pages().len(), 1);
    }

    /// Bug 4 regression: an academic caption is often a descriptive paragraph
    /// well over nine words. A `**Figure 1:` / `Figure` / `Fig.` prefixed block
    /// within a slightly looser gap must still be a caption, while an unrelated
    /// long paragraph whose first word merely starts with "Figure" must not.
    #[test]
    fn descriptive_figure_caption_with_many_words_is_recognized() {
        let text = "**Figure 1: Sliding Window Attention.** The number of \
                    operations in vanilla attention is quadratic in the sequence \
                    length, so the attention diagram spans the full page width.";
        assert!(
            text.split_whitespace().count() > 9,
            "test premise: the caption paragraph must exceed nine words"
        );
        // 3.0 * size_hint sits inside the extended 4.0 * size_hint caption gap.
        assert!(
            is_caption_block(text, 3.0 * 12.0, 12.0),
            "a long Figure-prefixed paragraph must be recognized as a caption"
        );
        // Beyond even the extended gap it is no longer treated as a caption.
        assert!(!is_caption_block(text, 4.5 * 12.0, 12.0));

        // A short line far away is not a caption either.
        assert!(!is_caption_block("unrelated body text", 3.0 * 12.0, 12.0));
    }

    /// Minimal Helvetica font dictionary shared by the synthetic builders.
    fn helvetica_font(doc: &mut lopdf::Document) -> lopdf::ObjectId {
        let mut font = lopdf::Dictionary::new();
        font.set(b"Type", lopdf::Object::Name(b"Font".to_vec()));
        font.set(b"Subtype", lopdf::Object::Name(b"Type1".to_vec()));
        font.set(b"BaseFont", lopdf::Object::Name(b"Helvetica".to_vec()));
        font.set(b"Encoding", lopdf::Object::Name(b"WinAnsiEncoding".to_vec()));
        doc.add_object(lopdf::Object::Dictionary(font))
    }

    fn finish_catalog(
        doc: &mut lopdf::Document,
        pages_id: lopdf::ObjectId,
    ) -> Vec<u8> {
        let mut catalog = lopdf::Dictionary::new();
        catalog.set(b"Type", lopdf::Object::Name(b"Catalog".to_vec()));
        catalog.set(b"Pages", lopdf::Object::Reference(pages_id));
        let catalog_id = doc.add_object(lopdf::Object::Dictionary(catalog));
        doc.trailer.set(b"Root", lopdf::Object::Reference(catalog_id));
        let mut bytes = Vec::new();
        doc.save_to(&mut bytes).expect("save synthetic pdf");
        bytes
    }

    /// Escape a synthetic literal string for a PDF content stream.
    fn pdf_string(s: &str) -> String {
        s.replace('\\', "\\\\")
            .replace('(', "\\(")
            .replace(')', "\\)")
    }

    /// Build a multi-page PDF from raw content streams (Helvetica/WinAnsi), so
    /// a test controls the exact `Tm`/`Td`/`Tj`/`'` operators. All content is
    /// synthetic placeholder text.
    fn synth_pages_pdf(pages: &[String]) -> Vec<u8> {
        let mut doc = lopdf::Document::new();
        let font_id = helvetica_font(&mut doc);
        let content_ids: Vec<lopdf::ObjectId> = pages
            .iter()
            .map(|c| {
                doc.add_object(lopdf::Object::Stream(lopdf::Stream::new(
                    lopdf::Dictionary::new(),
                    c.as_bytes().to_vec(),
                )))
            })
            .collect();
        let mut pages = lopdf::Dictionary::new();
        pages.set(b"Type", lopdf::Object::Name(b"Pages".to_vec()));
        pages.set(b"Kids", lopdf::Object::Array(Vec::new()));
        pages.set(b"Count", lopdf::Object::Integer(content_ids.len() as i64));
        let pages_id = doc.add_object(lopdf::Object::Dictionary(pages));
        let mut kids = Vec::new();
        for cid in content_ids {
            let mut fonts = lopdf::Dictionary::new();
            fonts.set(b"F1", lopdf::Object::Reference(font_id));
            let mut res = lopdf::Dictionary::new();
            res.set(b"Font", lopdf::Object::Dictionary(fonts));
            let mut page = lopdf::Dictionary::new();
            page.set(b"Type", lopdf::Object::Name(b"Page".to_vec()));
            page.set(b"Parent", lopdf::Object::Reference(pages_id));
            page.set(
                b"MediaBox",
                lopdf::Object::Array(vec![
                    lopdf::Object::Integer(0),
                    lopdf::Object::Integer(0),
                    lopdf::Object::Integer(595),
                    lopdf::Object::Integer(842),
                ]),
            );
            page.set(b"Contents", lopdf::Object::Reference(cid));
            page.set(b"Resources", lopdf::Object::Dictionary(res));
            let pid = doc.add_object(lopdf::Object::Dictionary(page));
            kids.push(lopdf::Object::Reference(pid));
        }
        doc.get_object_mut(pages_id)
            .unwrap()
            .as_dict_mut()
            .unwrap()
            .set(b"Kids", lopdf::Object::Array(kids));
        finish_catalog(&mut doc, pages_id)
    }

    /// A `Tm`-positioned text op.
    fn tm_text(x: f64, y: f64, s: &str) -> String {
        format!(
            "BT /F1 11 Tf 1 0 0 1 {x} {y} Tm ({}) Tj ET",
            pdf_string(s)
        )
    }

    /// A `Tj`+`Td` content stream; `horizontal` picks fragment placement vs the
    /// vertical-only line advances of ordinary prose.
    fn td_text(lines: &[&str], horizontal: bool) -> String {
        let mut parts = vec!["BT /F1 10 Tf".to_string()];
        parts.push(if horizontal {
            "300 800 Td".to_string()
        } else {
            "72 800 Td".to_string()
        });
        for (i, ln) in lines.iter().enumerate() {
            parts.push(format!("({}) Tj", pdf_string(ln)));
            if i + 1 != lines.len() {
                parts.push(if horizontal {
                    "-228 -16 Td".to_string()
                } else {
                    "0 -16 Td".to_string()
                });
            }
        }
        parts.push("ET".to_string());
        parts.join(" ")
    }

    /// A `'`-only content stream at one `Tm` (no `Td`, zero leading).
    fn quote_text(lines: &[&str]) -> String {
        let mut parts = vec!["BT /F1 12 Tf 1 0 0 1 72 800 Tm".to_string()];
        for ln in lines {
            parts.push(format!("({}) '", pdf_string(ln)));
        }
        parts.push("ET".to_string());
        parts.join(" ")
    }

    fn count_occurrences(haystack: &str, needle: &str) -> usize {
        haystack.matches(needle).count()
    }

    /// One page whose `/Resources` live inline on the `/Pages` parent, not on
    /// the page object (the QZP payslip shape). lopdf 0.44's
    /// `get_page_fonts` only collects inherited resources that are *indirect*
    /// references, so it returns zero fonts here; our own resolver must still
    /// decode the text instead of dropping every `Tj`.
    fn inherited_inline_resources_pdf() -> Vec<u8> {
        let mut doc = lopdf::Document::new();
        let font_id = helvetica_font(&mut doc);
        let content = b"BT /F1 12 Tf 40 120 Td (Inherited Resource Text) Tj ET".to_vec();
        let content_id = doc.add_object(lopdf::Object::Stream(lopdf::Stream::new(
            lopdf::Dictionary::new(),
            content,
        )));

        let mut pages = lopdf::Dictionary::new();
        pages.set(b"Type", lopdf::Object::Name(b"Pages".to_vec()));
        pages.set(b"Kids", lopdf::Object::Array(Vec::new()));
        pages.set(b"Count", lopdf::Object::Integer(1));
        let pages_id = doc.add_object(lopdf::Object::Dictionary(pages));

        let mut page = lopdf::Dictionary::new();
        page.set(b"Type", lopdf::Object::Name(b"Page".to_vec()));
        page.set(b"Parent", lopdf::Object::Reference(pages_id));
        page.set(
            b"MediaBox",
            lopdf::Object::Array(vec![
                lopdf::Object::Integer(0),
                lopdf::Object::Integer(0),
                lopdf::Object::Integer(595),
                lopdf::Object::Integer(842),
            ]),
        );
        page.set(b"Contents", lopdf::Object::Reference(content_id));
        let page_id = doc.add_object(lopdf::Object::Dictionary(page));

        let mut font_res = lopdf::Dictionary::new();
        font_res.set(b"F1", lopdf::Object::Reference(font_id));
        let mut resources = lopdf::Dictionary::new();
        resources.set(b"Font", lopdf::Object::Dictionary(font_res));
        // The Resources belong to the /Pages node only.
        let pages = doc
            .get_object_mut(pages_id)
            .unwrap()
            .as_dict_mut()
            .unwrap();
        pages.set(
            b"Kids",
            lopdf::Object::Array(vec![lopdf::Object::Reference(page_id)]),
        );
        pages.set(b"Resources", lopdf::Object::Dictionary(resources));
        finish_catalog(&mut doc, pages_id)
    }

    #[test]
    fn inline_resources_inherited_from_pages_are_resolved() {
        let bytes = inherited_inline_resources_pdf();
        // Documenting the root cause: the stock lopdf resolver misses the
        // inline ancestor dictionary, so without our fix no codec resolves.
        let stock = lopdf::Document::load_mem(&bytes).unwrap();
        let page_id = *stock.get_pages().values().next().unwrap();
        assert_eq!(
            stock.get_page_fonts(page_id).map(|f| f.len()).unwrap_or(0),
            0,
            "premise: lopdf must miss inline /Pages resources for this regression to matter"
        );

        assert!(is_digital_pdf_bytes(&bytes));
        let res = convert_pdf_bytes_to_markdown(&bytes, &ConversionOptions::default())
            .expect("text must decode once inherited resources resolve");
        assert!(
            res.markdown.contains("Inherited Resource Text"),
            "got: {}",
            res.markdown
        );
    }

    /// One page whose content is only `/Fm0 Do`, with the text and fonts inside
    /// the Form XObject — the Bouygues/payslip shape that was classified as
    /// scanned because neither detection nor extraction looked inside the form.
    fn form_xobject_text_pdf() -> Vec<u8> {
        let mut doc = lopdf::Document::new();
        let font_id = helvetica_font(&mut doc);

        let mut form_fonts = lopdf::Dictionary::new();
        form_fonts.set(b"F1", lopdf::Object::Reference(font_id));
        let mut form_res = lopdf::Dictionary::new();
        form_res.set(b"Font", lopdf::Object::Dictionary(form_fonts));
        let mut form_dict = lopdf::Dictionary::new();
        form_dict.set(b"Type", lopdf::Object::Name(b"XObject".to_vec()));
        form_dict.set(b"Subtype", lopdf::Object::Name(b"Form".to_vec()));
        form_dict.set(b"Resources", lopdf::Object::Dictionary(form_res));
        form_dict.set(
            b"BBox",
            lopdf::Object::Array(vec![
                lopdf::Object::Integer(0),
                lopdf::Object::Integer(0),
                lopdf::Object::Integer(595),
                lopdf::Object::Integer(842),
            ]),
        );
        let form_content = b"BT /F1 12 Tf 40 120 Td (Form XObject Text) Tj ET".to_vec();
        let form_id = doc.add_object(lopdf::Object::Stream(lopdf::Stream::new(
            form_dict,
            form_content,
        )));

        let page_content = b"q /Fm0 Do Q".to_vec();
        let content_id = doc.add_object(lopdf::Object::Stream(lopdf::Stream::new(
            lopdf::Dictionary::new(),
            page_content,
        )));

        let mut pages = lopdf::Dictionary::new();
        pages.set(b"Type", lopdf::Object::Name(b"Pages".to_vec()));
        pages.set(b"Kids", lopdf::Object::Array(Vec::new()));
        pages.set(b"Count", lopdf::Object::Integer(1));
        let pages_id = doc.add_object(lopdf::Object::Dictionary(pages));

        let mut xobjects = lopdf::Dictionary::new();
        xobjects.set(b"Fm0", lopdf::Object::Reference(form_id));
        let mut page_res = lopdf::Dictionary::new();
        page_res.set(b"XObject", lopdf::Object::Dictionary(xobjects));
        let mut page = lopdf::Dictionary::new();
        page.set(b"Type", lopdf::Object::Name(b"Page".to_vec()));
        page.set(b"Parent", lopdf::Object::Reference(pages_id));
        page.set(
            b"MediaBox",
            lopdf::Object::Array(vec![
                lopdf::Object::Integer(0),
                lopdf::Object::Integer(0),
                lopdf::Object::Integer(595),
                lopdf::Object::Integer(842),
            ]),
        );
        page.set(b"Resources", lopdf::Object::Dictionary(page_res));
        page.set(b"Contents", lopdf::Object::Reference(content_id));
        let page_id = doc.add_object(lopdf::Object::Dictionary(page));
        doc.get_object_mut(pages_id)
            .unwrap()
            .as_dict_mut()
            .unwrap()
            .set(
                b"Kids",
                lopdf::Object::Array(vec![lopdf::Object::Reference(page_id)]),
            );
        finish_catalog(&mut doc, pages_id)
    }

    #[test]
    fn text_inside_form_xobjects_is_detected_and_extracted() {
        let bytes = form_xobject_text_pdf();
        assert!(
            is_digital_pdf_bytes(&bytes),
            "a page whose only text lives in a Form XObject must classify as digital"
        );
        let res = convert_pdf_bytes_to_markdown(&bytes, &ConversionOptions::default())
            .expect("Form XObject text must be extracted");
        assert!(res.markdown.contains("Form XObject Text"), "got: {}", res.markdown);
    }

    #[test]
    fn trailing_bytes_after_eof_do_not_defeat_loading() {
        let mut padded = inherited_inline_resources_pdf();
        // Host buffers pad files with NULs; once the tail exceeds lopdf's
        // last-512-byte `startxref` window the classic xref is unreadable.
        padded.extend(std::iter::repeat(0u8).take(2048));
        assert!(
            lopdf::Document::load_mem(&padded).is_err(),
            "premise: the padded file must fail the stock loader"
        );
        let doc = load_pdf_document(&padded).expect("recovery must trim past %%EOF");
        assert_eq!(doc.get_pages().len(), 1);
        assert!(is_digital_pdf_bytes(&padded));
    }

    #[test]
    fn objstm_objects_separated_by_comments_are_recovered() {
        // lopdf parses ObjStm entries with a parser that does not skip `%`
        // comments; real producers (ORNIKAR CGV) prefix every embedded object
        // with `% N G`, so the whole page tree vanishes. The recovery blanks
        // those comments and re-parses.
        let index = b"3 0 4 36\n";
        let mut content = index.to_vec();
        content.extend_from_slice(b"% 3 0\n<< /Type /Pages /Count 0 >>\n");
        content.extend_from_slice(b"% 4 0\n<< /Type /Catalog >>\n");
        let mut dict = lopdf::Dictionary::new();
        dict.set(b"Type", lopdf::Object::Name(b"ObjStm".to_vec()));
        dict.set(b"N", lopdf::Object::Integer(2));
        dict.set(b"First", lopdf::Object::Integer(index.len() as i64));
        let mut doc = lopdf::Document::new();
        doc.add_object(lopdf::Object::Stream(lopdf::Stream::new(dict, content)));
        // Premise: without the recovery, the comment-prefixed objects are lost.
        assert!(!doc.objects.contains_key(&(3, 0)));
        let doc = recover_object_streams(doc);
        assert!(
            doc.objects.contains_key(&(3, 0)),
            "comment-separated ObjStm objects must be recovered"
        );
        assert!(doc.objects.contains_key(&(4, 0)));
    }

    /// One page that draws a Form XObject which draws *itself* and then shows
    /// text. The self-reference must not multiply the text or loop.
    fn self_referential_form_pdf() -> Vec<u8> {
        let mut doc = lopdf::Document::new();
        let font_id = helvetica_font(&mut doc);

        let form_content = b"q /Fm0 Do Q BT /F1 12 Tf 40 120 Td (Self Form Text) Tj ET".to_vec();
        let mut form_dict = lopdf::Dictionary::new();
        form_dict.set(b"Type", lopdf::Object::Name(b"XObject".to_vec()));
        form_dict.set(b"Subtype", lopdf::Object::Name(b"Form".to_vec()));
        form_dict.set(
            b"BBox",
            lopdf::Object::Array(vec![
                lopdf::Object::Integer(0),
                lopdf::Object::Integer(0),
                lopdf::Object::Integer(595),
                lopdf::Object::Integer(842),
            ]),
        );
        let form_id = doc.add_object(lopdf::Object::Stream(lopdf::Stream::new(
            form_dict,
            form_content,
        )));

        // The form's own resources map /Fm0 back to itself plus its font.
        let mut xobjects = lopdf::Dictionary::new();
        xobjects.set(b"Fm0", lopdf::Object::Reference(form_id));
        let mut fonts = lopdf::Dictionary::new();
        fonts.set(b"F1", lopdf::Object::Reference(font_id));
        let mut res = lopdf::Dictionary::new();
        res.set(b"XObject", lopdf::Object::Dictionary(xobjects));
        res.set(b"Font", lopdf::Object::Dictionary(fonts));
        doc.get_object_mut(form_id)
            .unwrap()
            .as_stream_mut()
            .unwrap()
            .dict
            .set(b"Resources", lopdf::Object::Dictionary(res));

        let content_id = doc.add_object(lopdf::Object::Stream(lopdf::Stream::new(
            lopdf::Dictionary::new(),
            b"q /Fm0 Do Q".to_vec(),
        )));
        let mut pages = lopdf::Dictionary::new();
        pages.set(b"Type", lopdf::Object::Name(b"Pages".to_vec()));
        pages.set(b"Kids", lopdf::Object::Array(Vec::new()));
        pages.set(b"Count", lopdf::Object::Integer(1));
        let pages_id = doc.add_object(lopdf::Object::Dictionary(pages));
        let mut page_res = lopdf::Dictionary::new();
        let mut page_xobjects = lopdf::Dictionary::new();
        page_xobjects.set(b"Fm0", lopdf::Object::Reference(form_id));
        page_res.set(b"XObject", lopdf::Object::Dictionary(page_xobjects));
        let mut page = lopdf::Dictionary::new();
        page.set(b"Type", lopdf::Object::Name(b"Page".to_vec()));
        page.set(b"Parent", lopdf::Object::Reference(pages_id));
        page.set(
            b"MediaBox",
            lopdf::Object::Array(vec![
                lopdf::Object::Integer(0),
                lopdf::Object::Integer(0),
                lopdf::Object::Integer(595),
                lopdf::Object::Integer(842),
            ]),
        );
        page.set(b"Resources", lopdf::Object::Dictionary(page_res));
        page.set(b"Contents", lopdf::Object::Reference(content_id));
        let page_id = doc.add_object(lopdf::Object::Dictionary(page));
        doc.get_object_mut(pages_id)
            .unwrap()
            .as_dict_mut()
            .unwrap()
            .set(
                b"Kids",
                lopdf::Object::Array(vec![lopdf::Object::Reference(page_id)]),
            );
        finish_catalog(&mut doc, pages_id)
    }

    #[test]
    fn self_referential_form_is_walked_once() {
        let bytes = self_referential_form_pdf();
        assert!(is_digital_pdf_bytes(&bytes));
        let res = convert_pdf_bytes_to_markdown(&bytes, &ConversionOptions::default())
            .expect("text after the self-reference must be extracted");
        assert_eq!(
            res.markdown.matches("Self Form Text").count(),
            1,
            "a form that draws itself must be entered once, got: {}",
            res.markdown
        );
    }

    #[test]
    fn object_stream_bomb_is_rejected_without_inflating() {
        use std::io::Write as _;

        // A payload far above the per-stream cap that still compresses to a few
        // KiB: the classic decompression-bomb shape. The first object is valid,
        // so an unbounded decoder would recover it and allocate the whole 20 MiB.
        let index = b"0 0 ";
        let mut payload = index.to_vec();
        payload.extend_from_slice(b"<< /Type /Pages /Kids [] /Count 1 >>");
        payload.resize(MAX_DECOMPRESSED_STREAM + (4 << 20), b'A');
        let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
        enc.write_all(&payload).unwrap();
        let compressed = enc.finish().unwrap();
        assert!(
            compressed.len() < 64 * 1024,
            "the bomb must be small on disk, got {} bytes",
            compressed.len()
        );

        let mut dict = lopdf::Dictionary::new();
        dict.set(b"Type", lopdf::Object::Name(b"ObjStm".to_vec()));
        dict.set(b"N", lopdf::Object::Integer(1));
        dict.set(b"First", lopdf::Object::Integer(index.len() as i64));
        dict.set(b"Filter", lopdf::Object::Name(b"FlateDecode".to_vec()));

        // Premise: the guard, not the data, is what keeps this finite.
        let probe = lopdf::Stream::new(dict.clone(), compressed.clone());
        assert!(
            probe
                .decompressed_content_with_limit(MAX_DECOMPRESSED_STREAM)
                .is_err(),
            "premise: the payload must exceed the per-stream cap"
        );

        let mut doc = lopdf::Document::new();
        let mut pages = lopdf::Dictionary::new();
        pages.set(b"Type", lopdf::Object::Name(b"Pages".to_vec()));
        pages.set(b"Kids", lopdf::Object::Array(Vec::new()));
        pages.set(b"Count", lopdf::Object::Integer(0));
        let pages_id = doc.add_object(lopdf::Object::Dictionary(pages));
        doc.add_object(lopdf::Object::Stream(lopdf::Stream::new(dict, compressed)));
        let bytes = finish_catalog(&mut doc, pages_id);

        let loaded = load_pdf_document(&bytes).expect("a bomb must not fail the load");
        assert!(
            loaded.get_pages().is_empty(),
            "the bomb object stream must be skipped, not expanded"
        );
        assert!(
            convert_pdf_bytes_to_markdown(&bytes, &ConversionOptions::default()).is_err(),
            "the bomb must fall through to the explicit no-text-layer error"
        );
    }

    /// A one-page PDF whose content stream inflates past the page-content cap
    /// and whose resources carry a font, so the digital probe short-circuits on
    /// the font and the conversion path itself has to reject the stream.
    fn content_stream_bomb_pdf() -> Vec<u8> {
        use std::io::Write as _;

        let payload = vec![b' '; text_extract::MAX_PAGE_CONTENT_STREAM + (4 << 20)];
        let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
        enc.write_all(&payload).unwrap();
        let compressed = enc.finish().unwrap();
        assert!(
            compressed.len() < 64 * 1024,
            "the bomb must be small on disk, got {} bytes",
            compressed.len()
        );

        let mut doc = lopdf::Document::new();
        let font_id = helvetica_font(&mut doc);
        let mut stream_dict = lopdf::Dictionary::new();
        stream_dict.set(b"Filter", lopdf::Object::Name(b"FlateDecode".to_vec()));
        let content_id = doc.add_object(lopdf::Object::Stream(lopdf::Stream::new(
            stream_dict,
            compressed,
        )));

        let mut fonts = lopdf::Dictionary::new();
        fonts.set(b"F1", lopdf::Object::Reference(font_id));
        let mut resources = lopdf::Dictionary::new();
        resources.set(b"Font", lopdf::Object::Dictionary(fonts));

        let mut pages = lopdf::Dictionary::new();
        pages.set(b"Type", lopdf::Object::Name(b"Pages".to_vec()));
        pages.set(b"Kids", lopdf::Object::Array(Vec::new()));
        pages.set(b"Count", lopdf::Object::Integer(1));
        let pages_id = doc.add_object(lopdf::Object::Dictionary(pages));

        let mut page = lopdf::Dictionary::new();
        page.set(b"Type", lopdf::Object::Name(b"Page".to_vec()));
        page.set(b"Parent", lopdf::Object::Reference(pages_id));
        page.set(
            b"MediaBox",
            lopdf::Object::Array(vec![
                lopdf::Object::Integer(0),
                lopdf::Object::Integer(0),
                lopdf::Object::Integer(612),
                lopdf::Object::Integer(792),
            ]),
        );
        page.set(b"Resources", lopdf::Object::Dictionary(resources));
        page.set(b"Contents", lopdf::Object::Reference(content_id));
        let page_id = doc.add_object(lopdf::Object::Dictionary(page));
        doc.get_object_mut(pages_id)
            .unwrap()
            .as_dict_mut()
            .unwrap()
            .set(
                b"Kids",
                lopdf::Object::Array(vec![lopdf::Object::Reference(page_id)]),
            );
        finish_catalog(&mut doc, pages_id)
    }

    #[test]
    fn content_stream_bomb_is_rejected_without_inflating() {
        let bytes = content_stream_bomb_pdf();
        // The font makes the probe report a digital layer without decoding the
        // bomb, so this exercises the conversion path, not the probe.
        assert!(is_digital_pdf_bytes(&bytes));

        let doc = load_pdf_document(&bytes).expect("a bomb must not fail the load");
        let page_id = *doc.get_pages().values().next().unwrap();
        let err = text_extract::decode_page_content(&doc, page_id)
            .expect_err("an over-cap page stream must be rejected");
        assert!(
            matches!(
                err,
                lopdf::Error::Decompress(lopdf::DecompressError::MemoryLimitExceeded { .. })
            ),
            "expected a limit error, got {err:?}"
        );

        let res = convert_pdf_bytes_to_markdown(&bytes, &ConversionOptions::default());
        assert!(
            res.is_err(),
            "an undecodable page must fail explicitly instead of emitting a prefix, got: {res:?}"
        );
    }

    #[test]
    fn image_xobject_bomb_is_rejected_before_allocating() {
        use std::io::Write as _;

        let doc = lopdf::Document::new();

        // Declared tiny, but the Flate stream inflates past the sample cap.
        let payload = vec![0u8; media::MAX_IMAGE_SAMPLES + (1 << 20)];
        let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
        enc.write_all(&payload).unwrap();
        let compressed = enc.finish().unwrap();

        let mut dict = lopdf::Dictionary::new();
        dict.set(b"Width", lopdf::Object::Integer(64));
        dict.set(b"Height", lopdf::Object::Integer(64));
        dict.set(b"BitsPerComponent", lopdf::Object::Integer(8));
        dict.set(b"ColorSpace", lopdf::Object::Name(b"DeviceGray".to_vec()));
        dict.set(b"Filter", lopdf::Object::Name(b"FlateDecode".to_vec()));
        let bomb = lopdf::Object::Stream(lopdf::Stream::new(dict, compressed));
        assert!(
            media::raster::decode_xobject_bytes(&doc, &bomb, u32::MAX).is_none(),
            "an inflating image stream must be rejected"
        );

        // A huge declared frame is rejected without touching the stream bytes.
        let mut dict = lopdf::Dictionary::new();
        dict.set(b"Width", lopdf::Object::Integer(100_000));
        dict.set(b"Height", lopdf::Object::Integer(100_000));
        dict.set(b"BitsPerComponent", lopdf::Object::Integer(8));
        dict.set(b"ColorSpace", lopdf::Object::Name(b"DeviceGray".to_vec()));
        let huge = lopdf::Object::Stream(lopdf::Stream::new(dict, vec![0u8; 16]));
        assert!(
            media::raster::decode_xobject_bytes(&doc, &huge, u32::MAX).is_none(),
            "a 100000x100000 declaration must be rejected"
        );

        // Just past the pixel cap at a legal dimension.
        let mut dict = lopdf::Dictionary::new();
        dict.set(b"Width", lopdf::Object::Integer(16_384));
        dict.set(b"Height", lopdf::Object::Integer(16_384));
        dict.set(b"BitsPerComponent", lopdf::Object::Integer(8));
        dict.set(b"ColorSpace", lopdf::Object::Name(b"DeviceGray".to_vec()));
        let wide = lopdf::Object::Stream(lopdf::Stream::new(dict, vec![0u8; 16]));
        assert!(
            media::raster::decode_xobject_bytes(&doc, &wide, u32::MAX).is_none(),
            "a 16384x16384 declaration must be rejected"
        );
    }

    #[test]
    fn page_count_over_limit_is_rejected() {
        let mut doc = lopdf::Document::new();
        let mut pages = lopdf::Dictionary::new();
        pages.set(b"Type", lopdf::Object::Name(b"Pages".to_vec()));
        pages.set(b"Count", lopdf::Object::Integer(MAX_PAGES as i64 + 1));
        let pages_id = doc.add_object(lopdf::Object::Dictionary(pages));

        let mut kids = Vec::with_capacity(MAX_PAGES + 1);
        for _ in 0..=MAX_PAGES {
            let mut page = lopdf::Dictionary::new();
            page.set(b"Type", lopdf::Object::Name(b"Page".to_vec()));
            page.set(b"Parent", lopdf::Object::Reference(pages_id));
            page.set(
                b"MediaBox",
                lopdf::Object::Array(vec![
                    lopdf::Object::Integer(0),
                    lopdf::Object::Integer(0),
                    lopdf::Object::Integer(612),
                    lopdf::Object::Integer(792),
                ]),
            );
            kids.push(lopdf::Object::Reference(
                doc.add_object(lopdf::Object::Dictionary(page)),
            ));
        }
        doc.get_object_mut(pages_id)
            .unwrap()
            .as_dict_mut()
            .unwrap()
            .set(b"Kids", lopdf::Object::Array(kids));
        let bytes = finish_catalog(&mut doc, pages_id);

        let err = convert_pdf_bytes_to_markdown(&bytes, &ConversionOptions::default())
            .expect_err("a document over the page cap must fail explicitly");
        assert!(err.contains("page limit"), "unexpected error: {err}");
    }

    /// The furniture key masks only a page number set off from a `page`/`p.`
    /// marker by a separator, so a page-varying footer matches across pages
    /// while body prose and longer identifiers stay distinct.
    #[test]
    fn furniture_key_masks_only_separated_page_tails() {
        assert_eq!(
            furniture_line_key("Societe Exemple SAS - page 2"),
            furniture_line_key("Societe Exemple SAS - page 3")
        );
        assert_eq!(
            furniture_line_key("Mentions legales | page 12"),
            furniture_line_key("Mentions legales | page 47")
        );
        assert_ne!(
            furniture_line_key("corps page 1"),
            furniture_line_key("corps page 2"),
            "a page word embedded in body prose must not be normalized"
        );
        assert_ne!(
            furniture_line_key("13004 MARSEILLE"),
            furniture_line_key("13009 MARSEILLE"),
            "distinct postal codes must not collapse to one key"
        );
        assert_ne!(
            furniture_line_key("page 2024"),
            furniture_line_key("page 2025"),
            "four-digit year-like tokens must not be masked"
        );
    }

    #[test]
    fn page_counter_line_detection() {
        assert_eq!(parse_page_counter("2/7"), Some((2, Some(7))));
        assert_eq!(parse_page_counter(" 2 / 7 "), Some((2, Some(7))));
        assert_eq!(parse_page_counter("Page 3/9"), Some((3, Some(9))));
        assert_eq!(parse_page_counter("2 sur 7"), Some((2, Some(7))));
        assert_eq!(parse_page_counter("Page 12"), Some((12, None)));
        assert_eq!(parse_page_counter("12"), None, "a bare number is not a counter");
        assert_eq!(parse_page_counter("13004 MARSEILLE"), None);
        assert_eq!(parse_page_counter("RCS 542 107 651"), None);
    }

    /// Only corroborated counters fall: a standalone ratio repeated unchanged
    /// on every page must survive.
    #[test]
    fn suppress_page_counters_drops_corroborated_only() {
        let mut pages: Vec<(u32, String)> = (1..=4)
            .map(|i| (i, format!("corps {i}\n3/4")))
            .collect();
        suppress_page_counters(&mut pages);
        let all = pages.iter().map(|(_, s)| s.as_str()).collect::<Vec<_>>().join("\n");
        assert_eq!(all.matches("3/4").count(), 4, "uncorroborated ratio dropped: {all}");

        let mut counters: Vec<(u32, String)> = (1..=4)
            .map(|i| (i, format!("corps {i}\nPage {}/4", i)))
            .collect();
        suppress_page_counters(&mut counters);
        let call = counters.iter().map(|(_, s)| s.as_str()).collect::<Vec<_>>().join("\n");
        for i in 1..=4 {
            assert!(!call.contains(&format!("Page {i}/4")), "counter survived: {call}");
        }
    }

    /// The line-based pass drops a repeated band header after page 1 only on
    /// dense pages; a sparse page whose whole body repeats (a ticket) survives.
    #[test]
    fn strip_running_lines_keeps_sparse_repeated_content() {
        let filler = "l1\nl2\nl3\nl4\nl5\nl6\nl7";
        let mut pages = vec![
            (1u32, format!("head band\n{filler}\nbody alpha\n")),
            (2u32, format!("head band\n{filler}\nbody beta\n")),
            (3u32, format!("head band\n{filler}\nbody gamma\n")),
        ];
        strip_running_lines(&mut pages);
        let all = pages.iter().map(|(_, s)| s.as_str()).collect::<Vec<_>>().join("\n");
        assert_eq!(all.matches("head band").count(), 1, "repeated header kept once: {all}");
        for unique in ["body alpha", "body beta", "body gamma"] {
            assert!(all.contains(unique), "unique line {unique} was dropped: {all}");
        }

        let ticket = "TICKET DE CAISSE\nArticle un 5,00\nArticle deux 7,50\nTOTAL 12,50";
        let mut sparse: Vec<(u32, String)> = (1..=3).map(|i| (i, ticket.to_string())).collect();
        strip_running_lines(&mut sparse);
        let kept = sparse.iter().map(|(_, s)| s.as_str()).collect::<Vec<_>>().join("\n");
        assert_eq!(
            kept.matches("TICKET DE CAISSE").count(),
            3,
            "sparse repeated content must survive: {kept}"
        );
    }

    // ---- Synthetic end-to-end cases for the layout batch (QA list) ----

    fn convert_synth(pages: &[String]) -> String {
        let bytes = synth_pages_pdf(pages);
        convert_pdf_bytes_to_markdown(&bytes, &ConversionOptions::default())
            .expect("synthetic pdf must convert")
            .markdown
    }

    /// A ratio inside a table row and as a standalone footer-style line must
    /// both survive: it is not a corroborated page counter.
    #[test]
    fn synthetic_ratio_cell_and_standalone_survive() {
        let pages: Vec<String> = (1..=4)
            .map(|i| {
                [
                    tm_text(72.0, 815.0, &format!("RATIO-SHEET-{i}")),
                    tm_text(72.0, 720.0, "Ratio"),
                    tm_text(72.0, 705.0, "3/4"),
                    tm_text(72.0, 300.0, &format!("unique prose line number {i} for this sheet only")),
                    tm_text(72.0, 90.0, "3/4"),
                ]
                .join("\n")
            })
            .collect();
        let md = convert_synth(&pages);
        assert_eq!(count_occurrences(&md, "3/4"), 8, "ratio cells/footers lost:\n{md}");
        for i in 1..=4 {
            assert!(md.contains(&format!("RATIO-SHEET-{i}")), "page {i} header lost:\n{md}");
        }
    }

    /// `Sous-total page N: <amount>` data rows must survive: the label carries
    /// a varying amount, not a plain counter.
    #[test]
    fn synthetic_subtotal_rows_survive() {
        let amounts = ["120,50", "98,00", "75,25", "61,10"];
        let pages: Vec<String> = (0..4)
            .map(|i| {
                [
                    tm_text(72.0, 815.0, &format!("RELEVE-{} EN-TETE COURANT", i + 1)),
                    tm_text(72.0, 500.0, &format!("ligne de donnees propre a la page {}", i + 1)),
                    tm_text(72.0, 95.0, &format!("Sous-total page {}: {}", i + 1, amounts[i])),
                ]
                .join("\n")
            })
            .collect();
        let md = convert_synth(&pages);
        for a in amounts {
            assert!(md.contains(a), "amount {a} lost:\n{md}");
        }
        assert_eq!(count_occurrences(&md, "Sous-total"), 4, "subtotal rows lost:\n{md}");
    }

    /// An identical amount row repeated in the footer band on every page is a
    /// real data row, not running furniture: it must survive on every page.
    #[test]
    fn synthetic_identical_footer_amount_row_survives() {
        let pages: Vec<String> = (1..=4)
            .map(|i| {
                [
                    tm_text(72.0, 815.0, "RELEVE MENSUEL"),
                    tm_text(72.0, 500.0, &format!("operation unique de la page {i}")),
                    tm_text(72.0, 80.0, "Frais de dossier: 45,00"),
                ]
                .join("\n")
            })
            .collect();
        let md = convert_synth(&pages);
        assert_eq!(
            count_occurrences(&md, "45,00"),
            4,
            "identical footer amount row suppressed as furniture:\n{md}"
        );
        assert_eq!(
            count_occurrences(&md, "Frais de dossier"),
            4,
            "identical footer amount label suppressed as furniture:\n{md}"
        );
    }

    /// A repeated column header inside table rows must survive.
    #[test]
    fn synthetic_repeated_montant_header_survives() {
        let pages: Vec<String> = (1..=3)
            .map(|i| {
                [
                    tm_text(72.0, 780.0, "Montant"),
                    tm_text(300.0, 780.0, "Detail"),
                    tm_text(72.0, 765.0, &format!("poste-{i}A")),
                    tm_text(300.0, 765.0, "100,00"),
                    tm_text(72.0, 750.0, &format!("poste-{i}B")),
                    tm_text(300.0, 750.0, "200,00"),
                ]
                .join("\n")
            })
            .collect();
        let md = convert_synth(&pages);
        assert_eq!(count_occurrences(&md, "Montant"), 3, "repeated header lost:\n{md}");
    }

    /// Distinct postal codes must never be folded into one furniture key.
    #[test]
    fn synthetic_postal_codes_survive() {
        let pages: Vec<String> = (1..=4)
            .map(|i| {
                let city = if i % 2 == 1 { "75001 PARIS" } else { "69001 LYON" };
                [
                    tm_text(72.0, 800.0, &format!("Adresse: {city}")),
                    tm_text(72.0, 780.0, &format!("dossier numero {i}0000001")),
                    tm_text(72.0, 300.0, &format!("texte metier distinct page {i}")),
                ]
                .join("\n")
            })
            .collect();
        let md = convert_synth(&pages);
        assert_eq!(count_occurrences(&md, "75001 PARIS"), 2, "postal code lost:\n{md}");
        assert_eq!(count_occurrences(&md, "69001 LYON"), 2, "postal code lost:\n{md}");
    }

    /// `Page 2/7`, `2 / 7`, `2/7` counters that vary across pages are dropped.
    #[test]
    fn synthetic_page_counters_are_removed() {
        let forms = ["Page 2/7", "2 / 7", "2/7", "Page 5/7"];
        let pages: Vec<String> = (1..=4)
            .map(|i| {
                [
                    tm_text(72.0, 815.0, forms[i - 1]),
                    tm_text(72.0, 300.0, &format!("contenu reel de la page {i} a conserver")),
                    tm_text(72.0, 90.0, forms[i % 4]),
                ]
                .join("\n")
            })
            .collect();
        let md = convert_synth(&pages);
        for f in forms {
            assert!(!md.contains(f), "counter {f} survived:\n{md}");
        }
        assert_eq!(count_occurrences(&md, "contenu reel"), 4, "body lost:\n{md}");
    }

    /// A legal footer whose only varying field is a page number is dropped,
    /// while a body line that merely says "corps page N" survives.
    #[test]
    fn synthetic_page_varying_legal_footer_removed_body_kept() {
        let pages: Vec<String> = (1..=4)
            .map(|i| {
                [
                    tm_text(72.0, 700.0, &format!("corps page {i}")),
                    tm_text(72.0, 80.0, &format!("Societe Exemple SAS - RCS 123 456 789 - page {i}")),
                ]
                .join("\n")
            })
            .collect();
        let md = convert_synth(&pages);
        assert_eq!(count_occurrences(&md, "RCS"), 1, "page-varying footer kept:\n{md}");
        assert_eq!(count_occurrences(&md, "corps page"), 4, "unique body line dropped:\n{md}");
    }

    /// A ticket whose lines repeat identically on every page must survive.
    #[test]
    fn synthetic_repeated_page_ticket_survives() {
        let body = ["TICKET DE CAISSE", "Article un 5,00", "Article deux 7,50", "TOTAL 12,50"];
        let page = td_text(&body, false);
        let pages = vec![page; 3];
        let md = convert_synth(&pages);
        assert_eq!(
            count_occurrences(&md, "TICKET DE CAISSE"),
            3,
            "repeated ticket content emptied:\n{md}"
        );
        for b in body {
            assert_eq!(count_occurrences(&md, b), 3, "line {b} lost:\n{md}");
        }
    }

    /// A horizontally-positioned `Tj`+`Td` prose page must keep every line.
    #[test]
    fn synthetic_td_prose_page_keeps_content() {
        let body = [
            "Le contenu de cette page est dispose",
            "par fragments successifs avec des",
            "deplacements horizontaux puis verticaux",
            "afin de tester le routage vers",
            "le moteur de mise en page du document",
        ];
        let pages = vec![td_text(&body, true); 3];
        let md = convert_synth(&pages);
        for b in body {
            assert_eq!(count_occurrences(&md, b), 3, "prose line {b} lost:\n{md}");
        }
    }

    /// A `'`-only letter (one `Tm`, zero leading) must keep word boundaries
    /// instead of being merged into a single fragment.
    #[test]
    fn synthetic_quote_show_letter_keeps_words() {
        let body = [
            "Objet: votre demande de dossier",
            "Madame, Monsieur,",
            "Nous accusons reception de votre courrier",
            "et vous remercions de votre confiance.",
        ];
        let pages = vec![quote_text(&body); 2];
        let md = convert_synth(&pages);
        assert!(!md.contains("dossierMadame"), "quote fragments merged:\n{md}");
        for b in body {
            assert!(md.contains(b), "letter line lost: {b}\n{md}");
        }
    }

    /// The layout path's overdraw dedup is intentional: it folds only an exact
    /// overstrike (same text, same baseline) and must never merge two distinct
    /// strings that happen to overlap.
    #[test]
    fn overdraw_dedup_folds_only_identical_spans() {
        use crate::layout::{build_lines, Span};
        let span = |text: &str, x: f64, y: f64| Span {
            text: text.to_string(),
            x,
            y,
            size: 10.0,
            advance: 20.0,
            word_advance: 20.0,
            is_bold: false,
            is_italic: false,
            is_underline: false,
            is_vertical: false,
        };
        let overstrike = build_lines(&[span("Montant", 100.0, 500.0), span("Montant", 100.2, 500.0)]);
        assert_eq!(
            overstrike.iter().map(|l| l.len()).sum::<usize>(),
            1,
            "an identical overstrike must collapse to one span"
        );
        let distinct = build_lines(&[
            span("ALPHA-COST", 100.0, 500.0),
            span("BRAVO-COST", 100.0, 500.4),
        ]);
        let texts: Vec<&str> = distinct.iter().flatten().map(|s| s.text.as_str()).collect();
        assert!(
            texts.contains(&"ALPHA-COST") && texts.contains(&"BRAVO-COST"),
            "distinct overlapping strings must both survive: {texts:?}"
        );
    }

    /// An encrypted PDF the empty user password cannot open leaves an
    /// `/Encrypt` trailer entry; that is an encryption failure, not a scanned
    /// document.
    #[test]
    fn encrypted_document_without_pages_is_reported() {
        let mut doc = lopdf::Document::with_version("1.4");
        doc.trailer
            .set("Encrypt", lopdf::Object::Dictionary(lopdf::Dictionary::new()));
        assert!(encrypted_undecrypted(&doc));
        assert_eq!(ENCRYPTED_PDF_ERROR, "encrypted PDF: password required");
    }

    #[test]
    fn unencrypted_empty_document_is_not_encrypted() {
        let doc = lopdf::Document::with_version("1.4");
        assert!(!encrypted_undecrypted(&doc));
    }

    /// End-to-end: a serialized PDF with an `/Encrypt` entry and no pages must
    /// come back as [`ENCRYPTED_PDF_ERROR`], so the gateway can report it
    /// instead of paying for a vision rescue that cannot read it either.
    #[test]
    fn encrypted_pdf_bytes_are_reported_as_encrypted() {
        let mut doc = lopdf::Document::with_version("1.4");
        let pages_id = doc.new_object_id();
        doc.objects.insert(
            pages_id,
            lopdf::Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => lopdf::Object::Array(vec![]),
                "Count" => 0,
            }),
        );
        let catalog_id = doc.new_object_id();
        doc.objects.insert(
            catalog_id,
            lopdf::Object::Dictionary(dictionary! {
                "Type" => "Catalog", "Pages" => pages_id,
            }),
        );
        doc.trailer.set("Root", catalog_id);
        doc.trailer.set(
            "Encrypt",
            lopdf::Object::Dictionary(dictionary! {
                "Filter" => "Standard", "V" => 1, "R" => 2,
                "O" => lopdf::Object::String(vec![0u8; 32], lopdf::StringFormat::Literal),
                "U" => lopdf::Object::String(vec![0u8; 32], lopdf::StringFormat::Literal),
                "P" => -1,
            }),
        );
        let mut bytes = Vec::new();
        doc.save_to(&mut bytes).expect("serialize encrypted stub");
        match load_pdf_document(&bytes) {
            Err(e) => assert_eq!(e, ENCRYPTED_PDF_ERROR),
            Ok(d) => panic!("expected an encryption error, got {} pages", d.get_pages().len()),
        }
    }

    /// The public probe the CLI uses must agree with the load error for an
    /// encrypted stub, and the stub must not look "digital" (otherwise the CLI
    /// pre-gate would print the scanned-image message before ever checking).
    #[test]
    fn pdf_password_required_probe_matches_the_conversion_error() {
        let mut doc = lopdf::Document::with_version("1.4");
        let pages_id = doc.new_object_id();
        doc.objects.insert(
            pages_id,
            lopdf::Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => lopdf::Object::Array(vec![]),
                "Count" => 0,
            }),
        );
        let catalog_id = doc.new_object_id();
        doc.objects.insert(
            catalog_id,
            lopdf::Object::Dictionary(dictionary! {
                "Type" => "Catalog", "Pages" => pages_id,
            }),
        );
        doc.trailer.set("Root", catalog_id);
        doc.trailer.set(
            "Encrypt",
            lopdf::Object::Dictionary(dictionary! {
                "Filter" => "Standard", "V" => 1, "R" => 2,
                "O" => lopdf::Object::String(vec![0u8; 32], lopdf::StringFormat::Literal),
                "U" => lopdf::Object::String(vec![0u8; 32], lopdf::StringFormat::Literal),
                "P" => -1,
            }),
        );
        let mut bytes = Vec::new();
        doc.save_to(&mut bytes).expect("serialize encrypted stub");

        assert!(pdf_password_required(&bytes));
        assert!(!is_digital_pdf_bytes(&bytes));
        let err = convert_pdf_bytes_to_markdown(&bytes, &ConversionOptions::default())
            .expect_err("encrypted bytes must fail conversion");
        assert_eq!(err, ENCRYPTED_PDF_ERROR);
    }

    /// A plain unencrypted document is never reported as password-protected.
    #[test]
    fn pdf_password_required_probe_is_false_for_a_plain_pdf() {
        let mut doc = lopdf::Document::with_version("1.4");
        let pages_id = doc.new_object_id();
        doc.objects.insert(
            pages_id,
            lopdf::Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => lopdf::Object::Array(vec![]),
                "Count" => 0,
            }),
        );
        let catalog_id = doc.new_object_id();
        doc.objects.insert(
            catalog_id,
            lopdf::Object::Dictionary(dictionary! {
                "Type" => "Catalog", "Pages" => pages_id,
            }),
        );
        doc.trailer.set("Root", catalog_id);
        let mut bytes = Vec::new();
        doc.save_to(&mut bytes).expect("serialize plain stub");
        assert!(!pdf_password_required(&bytes));
    }
}
