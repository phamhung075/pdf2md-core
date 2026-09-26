// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Robust multilingual (FR / VI / EN) PDF text extraction for the fast path.
//!
//! This module replaces `lopdf::Document::extract_text` for the digital-PDF
//! fast path. `lopdf`'s extractor has two defects that corrupt accented Latin
//! text (French, Vietnamese, ...):
//!
//! 1. A `/Encoding` dictionary whose `/Differences` array contains `/.notdef`
//!    (extremely common in real-world generators, e.g. EDF / Enedis bills) is
//!    treated as an *error*, so the whole font silently falls back to lopdf's
//!    `STANDARD_ENCODING` table.
//! 2. That fallback table is corrupt for bytes >= 0xC0: it maps byte `0xE9`
//!    (`é`) to `Ø` and byte `0xE8` (`è`) to `Ł`, among others.
//!
//! We therefore resolve font encodings ourselves with correct data tables and
//! decode each text run accordingly:
//!   * `/ToUnicode` CMaps (bfchar / bfrange) — authoritative when present;
//!   * `/Encoding` by name (`WinAnsiEncoding`, `MacRomanEncoding`, ...);
//!   * `/Encoding` dictionaries with `/Differences` (`.notdef` and unknown
//!     glyph names are tolerated, AGL `uniXXXX` names supported);
//!   * a WinAnsi heuristic for non-symbolic simple fonts with no encoding
//!     information (how the overwhelming majority of producers write accented
//!     Latin text).
//!
//! Page content / font-structure parsing still comes from lopdf (public API
//! only). If content parsing fails, the caller falls back to lopdf's own
//! extractor.

use super::*;

/// Normalise decoded page text before it is rendered:
///
///   * fold the Latin presentation-form ligatures U+FB00–U+FB06 to their letter
///     sequences (the geometry engine already does this in `fold_ligatures`;
///     the string walker must too, since a `/ToUnicode` CMap may return the
///     ligature code point itself);
///   * drop soft hyphens (U+00AD) — invisible formatting, never text;
///   * turn a no-break space (U+00A0) or narrow no-break space (U+202F) into a
///     plain space when it sits next to a digit, the French thousands
///     separator (`1 234,56`). NBSPs elsewhere are left untouched;
///   * map a Microsoft Symbol/Wingdings private-use code point
///     (U+F020–U+F0FF) to its Unicode equivalent, and drop any other
///     private-use character (U+E000, U+F8FF, the supplementary planes): an
///     unmapped PUA glyph is mojibake, not text.
pub(crate) fn normalize_decoded_text(s: &str) -> String {
    if !s.chars().any(|c| {
        matches!(c, '\u{FB00}'..='\u{FB06}' | '\u{00AD}' | '\u{00A0}' | '\u{202F}')
            || is_private_use_char(c)
    }) {
        return s.to_string();
    }
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    for (i, &c) in chars.iter().enumerate() {
        match c {
            '\u{FB00}' => out.push_str("ff"),
            '\u{FB01}' => out.push_str("fi"),
            '\u{FB02}' => out.push_str("fl"),
            '\u{FB03}' => out.push_str("ffi"),
            '\u{FB04}' => out.push_str("ffl"),
            '\u{FB05}' | '\u{FB06}' => out.push_str("st"),
            '\u{00AD}' => {}
            '\u{00A0}' | '\u{202F}' => {
                let prev_digit = i > 0 && chars[i - 1].is_ascii_digit();
                let next_digit = chars.get(i + 1).map_or(false, |n| n.is_ascii_digit());
                if prev_digit || next_digit {
                    out.push(' ');
                } else {
                    out.push(c);
                }
            }
            c if is_private_use_char(c) => {
                // The font is unknown on this shared string chokepoint, so use
                // the corpus-verified/Symbol union: a mapped glyph survives; an
                // unmapped PUA character is dropped rather than emitted.
                if let Some(m) = crate::glyph_data::symbol_pua_to_char(c as u32) {
                    out.push(m);
                }
            }
            _ => out.push(c),
        }
    }
    out
}

/// True when the geometry output dropped a numeric value (>= 3 digits) that the
/// string walker decoded. Compared as a substring of the output's concatenated
/// digits so re-tokenisation (`1 234` → `1234`) is not mistaken for a loss.
///
/// The walker side must be tokenised the same way as the decoder: a grouped
/// value (`12 345`, `1 234,56`, `12.345,67`) is one number whose digits are
/// contiguous after the group separators are removed. Splitting on *every*
/// non-digit would check `12` and `345` independently, so a glyph page that
/// still carried those two fragments separately would pass the guard even
/// though the decoded value was gone (a corpus file 6-digit, a corpus file amount-dot-2dec).
pub(super) fn loses_digit_content(walker: &str, layout: &str) -> bool {
    let layout_digits: String = layout.chars().filter(|c| c.is_ascii_digit()).collect();
    let chars: Vec<char> = walker.chars().collect();
    let mut i = 0usize;
    while i < chars.len() {
        if !chars[i].is_ascii_digit() {
            i += 1;
            continue;
        }
        // Consume one numeric value: digits plus internal group/decimal
        // separators that are immediately followed by another digit.
        let mut digits = String::new();
        while i < chars.len() {
            let c = chars[i];
            if c.is_ascii_digit() {
                digits.push(c);
                i += 1;
            } else if is_group_separator(c)
                && chars.get(i + 1).map_or(false, |n| n.is_ascii_digit())
            {
                i += 1;
            } else {
                break;
            }
        }
        if digits.len() >= 3 && !layout_digits.contains(&digits) {
            return true;
        }
    }
    false
}

/// Separators that may join two digit groups of one numeric value.
pub(super) fn is_group_separator(c: char) -> bool {
    matches!(c, ' ' | '\u{00a0}' | '\u{202f}' | '\'' | '.' | ',')
}

/// The page-marker heuristic `convert_pdf_bytes_to_markdown` applies to a
/// page's first line. Computed here on the string-walker text so a newly
/// routed page keeps the marker the non-routed path emitted.
pub(super) fn first_line_looks_like_heading(text: &str) -> bool {
    text.starts_with("# ")
        || text.lines().next().map_or(false, |l| {
            l.len() < 60 && l.chars().all(|c| c.is_alphanumeric() || c.is_whitespace())
        })
}
