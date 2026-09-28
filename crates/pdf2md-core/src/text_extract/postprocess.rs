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
///     unmapped PUA glyph is mojibake, not text;
///   * strip characters that carry no text and must never reach the output:
///     NUL and the other C0 controls (except tab/newline/carriage-return),
///     DEL and the C1 controls, bidi embedding/override/isolate controls,
///     the zero-width space, U+FEFF, and the Unicode noncharacters
///     (see [`STRIPPED_EXTRACTION_RANGES`]). Bidi marks and joiners that real
///     RTL and Persian/Indic/emoji text need (U+200C–U+200F) are kept;
///   * map a source U+2028 LINE SEPARATOR or U+2029 PARAGRAPH SEPARATOR to a
///     newline. U+2028 is also the engine's internal `CELL_LINE_BREAK_PENDING`
///     sentinel, so a decoded U+2028 must be rewritten here, before that pass
///     runs, or it would be emitted as a real `<br>`.
pub(crate) fn normalize_decoded_text(s: &str) -> String {
    if !s.chars().any(needs_decoded_text_normalization) {
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
            // A source line/paragraph separator becomes a real line break. It
            // must not survive: U+2028 is the in-cell deferred-break sentinel.
            '\u{2028}' | '\u{2029}' => out.push('\n'),
            c if is_stripped_extraction_char(c) => {}
            _ => out.push(c),
        }
    }
    out
}

/// True when [`normalize_decoded_text`] must rewrite `c` (fold, drop, replace,
/// or strip). Keeps the common all-plain-text case allocation-free.
fn needs_decoded_text_normalization(c: char) -> bool {
    matches!(
        c,
        '\u{FB00}'..='\u{FB06}'
            | '\u{00AD}'
            | '\u{00A0}'
            | '\u{202F}'
            | '\u{2028}'
            | '\u{2029}'
    ) || is_private_use_char(c)
        || is_stripped_extraction_char(c)
}

/// Code-point ranges (inclusive) removed from extracted text because they carry
/// no visible text and can corrupt downstream storage or reorder the rendered
/// output. C0 controls are stripped except tab/newline/carriage-return, which
/// keep the existing line handling. U+200E/U+200F (LRM/RLM) and U+200C/U+200D
/// (ZWNJ/ZWJ) are deliberately absent: they are legitimate in RTL and
/// Persian/Indic/emoji text.
const STRIPPED_EXTRACTION_RANGES: &[(char, char)] = &[
    ('\u{0000}', '\u{0008}'), // C0 controls below TAB
    ('\u{000B}', '\u{000C}'), // VT, FF
    ('\u{000E}', '\u{001F}'), // C0 controls above CR
    ('\u{007F}', '\u{009F}'), // DEL + C1 controls
    ('\u{200B}', '\u{200B}'), // zero-width space
    ('\u{202A}', '\u{202E}'), // bidi embedding / override
    ('\u{2066}', '\u{2069}'), // bidi isolates
    ('\u{FEFF}', '\u{FEFF}'), // BOM / zero-width no-break space
    ('\u{FDD0}', '\u{FDEF}'), // noncharacters (the nFFFE/nFFFF forms are below)
];

/// True when a decoded character must be removed from extracted text. Every
/// noncharacter (`U+nFFFE` / `U+nFFFF`, in any plane) is covered by the low-bit
/// test, not just the BMP ones.
fn is_stripped_extraction_char(c: char) -> bool {
    STRIPPED_EXTRACTION_RANGES
        .iter()
        .any(|&(lo, hi)| c >= lo && c <= hi)
        || (c as u32 & 0xFFFE) == 0xFFFE
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
///
/// A rendered ATX heading (`#`..`######`) already *is* a page title, so any
/// level counts as heading-like. Without this, a document whose title the
/// layout renderer promoted to `## ` lost its `## Page N` marker, because only
/// the H1 spelling `# ` was recognised.
pub(crate) fn first_line_looks_like_heading(text: &str) -> bool {
    let Some(line) = text.lines().next() else {
        return false;
    };
    let hashes = line.bytes().take_while(|&b| b == b'#').count();
    if (1..=6).contains(&hashes) && line.as_bytes().get(hashes).map_or(true, |&b| b == b' ') {
        return true;
    }
    line.len() < 60 && line.chars().all(|c| c.is_alphanumeric() || c.is_whitespace())
}
