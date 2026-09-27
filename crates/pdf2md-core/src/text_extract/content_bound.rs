// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Pre-decode bounds for a page content stream.
//!
//! `lopdf::content::Content::decode` materialises one `Operation` per operator
//! with no bound of its own, at roughly 1 KB of heap per operator. A 35 KB PDF
//! whose content stream packs one `(word) Tj` per glyph expands to an 800 000
//! operator list that costs ~865 MB per decode (and the page is decoded more
//! than once), reaching 1.87 GB RSS for the whole conversion and aborting under
//! any address-space limit. The existing operator budgets (`MAX_WALKER_OPS_TOTAL`
//! / `MAX_FORM_OPS_TOTAL`) are applied *after* decode, so they cannot stop the
//! allocation.
//!
//! [`bound_page_content`] scans the raw decoded bytes for the text-show
//! operators (`Tj`, `TJ`, `'`, `"`) and the total operator count, and truncates
//! the slice just past the operator that reaches a cap. `Content::decode` parses
//! the prefix (lopdf's `many0(operation)` discards a partial trailing
//! operation), so the engine still returns a partial page rather than failing,
//! and the caller reports `budget_exhausted`. Real documents are far below both
//! caps — measured over the 811-document corpus the largest single-page counts
//! are 4 521 text-show operators and 226 850 operators — so no real page is
//! touched.

/// Maximum text-show operators (`Tj`/`TJ`/`'`/`"`) decoded for one page.
///
/// Corpus maximum is 4 521, so this leaves ~14x headroom and still stops the
/// glyph-amplification repro after a bounded prefix. This is the "glyph budget"
/// counterpart of [`MAX_WALKER_OPS_TOTAL`](super::MAX_WALKER_OPS_TOTAL); the
/// glyph engine additionally caps its accumulated spans at
/// [`MAX_GLYPH_SPANS_PER_PAGE`](crate::layout::glyph_stream).
pub(crate) const MAX_PAGE_GLYPHS: usize = 64_000;

/// Maximum operators (text-show and everything else) decoded for one page.
///
/// Corpus maximum is 226 850, so this is a memory backstop for a
/// path/state-operator bomb rather than a content limit; a single decode at
/// this cap stays well inside the 400 MB target.
pub(crate) const MAX_PAGE_CONTENT_OPS: usize = 300_000;

/// Truncate `data` in place when it holds more than [`MAX_PAGE_CONTENT_OPS`]
/// operators or more than [`MAX_PAGE_GLYPHS`] text-show operators. Returns
/// `true` when the stream was truncated.
pub(crate) fn bound_page_content(data: &mut Vec<u8>) -> bool {
    match operator_cut(data, MAX_PAGE_CONTENT_OPS, MAX_PAGE_GLYPHS) {
        Some(cut) => {
            data.truncate(cut);
            true
        }
        None => false,
    }
}

/// Byte offset just past the operator that reaches `max_ops` or `max_glyphs`,
/// or `None` when the whole stream is within budget.
///
/// The scan follows the PDF content-stream grammar far enough to never count a
/// byte inside a literal `(...)` string, a `<...>` hex string, a `%` comment,
/// a `/Name`, a number or an inline image (`BI ... ID <binary> EI`) as an
/// operator. Anything it cannot classify is skipped conservatively.
pub(crate) fn operator_cut(data: &[u8], max_ops: usize, max_glyphs: usize) -> Option<usize> {
    let n = data.len();
    let mut i = 0usize;
    let mut ops = 0usize;
    let mut glyphs = 0usize;
    while i < n {
        let b = data[i];
        match b {
            b'\0' | b'\t' | b'\n' | b'\x0c' | b'\r' | b' ' => i += 1,
            b'%' => {
                while i < n && data[i] != b'\n' && data[i] != b'\r' {
                    i += 1;
                }
            }
            b'(' => i = skip_literal_string(data, i),
            b'<' => {
                if data.get(i + 1) == Some(&b'<') {
                    i += 2;
                } else {
                    i = skip_hex_string(data, i);
                }
            }
            b'>' | b'[' | b']' | b'{' | b'}' => i += 1,
            b'/' => {
                i += 1;
                while i < n && !is_ws(data[i]) && !is_delim(data[i]) {
                    i += 1;
                }
            }
            b'+' | b'-' | b'.' | b'0'..=b'9' => i = skip_number(data, i),
            b'A'..=b'Z' | b'a'..=b'z' | b'*' => {
                let start = i;
                while i < n && (data[i].is_ascii_alphanumeric() || data[i] == b'*') {
                    i += 1;
                }
                ops += 1;
                if is_glyph_operator(&data[start..i]) {
                    glyphs += 1;
                }
                if &data[start..i] == b"ID" {
                    i = skip_inline_image(data, i);
                }
                if ops >= max_ops || glyphs >= max_glyphs {
                    return Some(i);
                }
            }
            b'\'' | b'"' => {
                ops += 1;
                glyphs += 1;
                i += 1;
                if ops >= max_ops || glyphs >= max_glyphs {
                    return Some(i);
                }
            }
            _ => i += 1,
        }
    }
    None
}

fn is_ws(b: u8) -> bool {
    matches!(b, b'\0' | b'\t' | b'\n' | b'\x0c' | b'\r' | b' ')
}

fn is_delim(b: u8) -> bool {
    matches!(
        b,
        b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
    )
}

/// The four text-show operators; `'` and `"` are handled by the caller.
fn is_glyph_operator(tok: &[u8]) -> bool {
    tok == b"Tj" || tok == b"TJ"
}

/// Index just past a literal `(...)` string opened at `start`.
fn skip_literal_string(data: &[u8], start: usize) -> usize {
    let n = data.len();
    let mut depth = 1usize;
    let mut i = start + 1;
    while i < n {
        match data[i] {
            b'\\' => i += 2,
            b'(' => {
                depth += 1;
                i += 1;
            }
            b')' => {
                depth -= 1;
                i += 1;
                if depth == 0 {
                    return i;
                }
            }
            _ => i += 1,
        }
    }
    n
}

/// Index just past a `<...>` hex string opened at `start`.
fn skip_hex_string(data: &[u8], start: usize) -> usize {
    let n = data.len();
    let mut i = start + 1;
    while i < n && data[i] != b'>' {
        i += 1;
    }
    if i < n {
        i + 1
    } else {
        n
    }
}

/// Index just past a numeric token starting at `start`.
fn skip_number(data: &[u8], start: usize) -> usize {
    let n = data.len();
    let mut i = start;
    while i < n && matches!(data[i], b'+' | b'-' | b'.' | b'e' | b'E' | b'0'..=b'9') {
        i += 1;
    }
    i
}

/// Index just past the `EI` that closes an inline image. `start` points just
/// past the `ID` keyword; per the spec one whitespace byte separates `ID` from
/// the binary data, which runs until a whitespace-delimited `EI`. A missing
/// `EI` returns end-of-buffer (the bytes are binary, not operators).
fn skip_inline_image(data: &[u8], start: usize) -> usize {
    let n = data.len();
    let mut i = start;
    if i < n && data[i].is_ascii_whitespace() {
        i += 1;
    }
    while i + 1 < n {
        if data[i] == b'E'
            && data[i + 1] == b'I'
            && (i == 0 || data[i - 1].is_ascii_whitespace())
            && (i + 2 >= n || data[i + 2].is_ascii_whitespace() || is_delim(data[i + 2]))
        {
            return i + 2;
        }
        i += 1;
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glyph_cap_truncates_just_past_the_operator() {
        // 10 identical ops; a glyph cap of 3 keeps the third `Tj` and the text
        // before it.
        let data = b"(word) Tj (word) Tj (word) Tj (word) Tj".to_vec();
        let cut = operator_cut(&data, 1_000, 3).expect("cap must trip");
        let kept = &data[..cut];
        assert!(kept.ends_with(b"Tj"));
        assert_eq!(kept.windows(2).filter(|w| *w == b"Tj").count(), 3);
    }

    #[test]
    fn strings_names_and_inline_images_do_not_count() {
        // `Tj` inside a literal string and a hex string is data, not an operator.
        let data = b"(Tj) <5478> /Tj 12 Tj".to_vec();
        assert!(operator_cut(&data, 1_000, 100).is_none());
        // `BI ... ID <binary with bogus operators> EI` hides binary.
        let mut data = Vec::new();
        data.extend_from_slice(b"BI /W 4 ID Tj Tj Tj binary EI ");
        data.extend_from_slice(b"Tj");
        // One real glyph op (`Tj` after EI); cap of 2 must not trip.
        assert!(operator_cut(&data, 1_000, 2).is_none());
    }

    #[test]
    fn op_cap_truncates_on_total_operators() {
        let mut data = Vec::new();
        for _ in 0..50 {
            data.extend_from_slice(b"q ");
        }
        let cut = operator_cut(&data, 10, 1_000).expect("op cap must trip");
        assert_eq!(data[..cut].windows(1).filter(|w| *w == b"q").count(), 10);
    }
}
