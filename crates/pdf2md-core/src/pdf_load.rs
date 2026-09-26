//! pdf2md-core — High-performance native Rust core engine for sub-millisecond
//! PDF-to-Markdown extraction and 2D spatial canvas table reconstruction.
//!
//! Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
//! SPDX-License-Identifier: BSL-1.1
//! Licensed under the Business Source License 1.1 (BSL-1.1).

use super::*;

/// True when the loaded document is still encrypted after loading: the trailer
/// carries an `/Encrypt` dictionary and lopdf did not decrypt it (on a
/// successful empty-password authentication lopdf removes the trailer entry and
/// records `encryption_state`). This is deliberately *not* conditioned on the
/// page tree being empty: an encrypted document whose pages happen to remain
/// parseable must still be reported as encrypted rather than silently converted
/// or routed to OCR, which cannot read it either.
pub(super) fn encrypted_undecrypted(doc: &lopdf::Document) -> bool {
    doc.trailer.get(b"Encrypt").is_ok() && !doc.was_encrypted()
}

pub(super) fn load_pdf_document(bytes: &[u8]) -> Result<lopdf::Document, String> {
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

pub(super) fn load_pdf_document_repaired(bytes: &[u8]) -> Result<lopdf::Document, String> {
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
pub(super) fn load_bounded(bytes: &[u8]) -> Result<lopdf::Document, lopdf::Error> {
    lopdf::Document::load_mem_with_options(
        bytes,
        lopdf::LoadOptions::with_max_decompressed_size(MAX_DECOMPRESSED_STREAM),
    )
}

/// Returns `bytes` truncated just past the last `%%EOF` marker, or `None` when
/// there is nothing to trim (well-formed file) or no marker at all.
pub(super) fn truncate_after_last_eof(bytes: &[u8]) -> Option<Vec<u8>> {
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
pub(super) fn recover_object_streams(mut doc: lopdf::Document) -> lopdf::Document {
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
pub(super) fn blank_line_comments(content: &mut [u8]) -> bool {
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
pub(super) fn repair_classic_xref(bytes: &[u8]) -> Option<Vec<u8>> {
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

pub(super) fn find_last(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).rposition(|w| w == needle)
}

pub(super) fn find_last_xref_keyword(bytes: &[u8]) -> Option<usize> {
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

pub(super) fn skip_ws(bytes: &[u8], mut p: usize) -> usize {
    while p < bytes.len() && bytes[p].is_ascii_whitespace() {
        p += 1;
    }
    p
}

pub(super) fn parse_uint(bytes: &[u8], start: usize) -> Option<(u32, usize)> {
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
pub(super) fn scan_object_offsets(bytes: &[u8]) -> std::collections::HashMap<u32, usize> {
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

pub(super) fn find_from(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if from >= hay.len() {
        return None;
    }
    hay[from..].windows(needle.len()).position(|w| w == needle).map(|p| from + p)
}

/// Parses the `<num> <gen>` immediately preceding the ` obj` at `space`, and
/// returns `(num, offset_of_num)` only when the header starts at a line
/// boundary (so a byte sequence inside a stream is not mistaken for one).
pub(super) fn parse_object_header(bytes: &[u8], space: usize) -> Option<(u32, usize)> {
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
