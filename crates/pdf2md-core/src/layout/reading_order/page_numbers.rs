// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Page-number footer detection for the rendered Markdown.
//!
//! The reading-order and doc-block renderers drop numeric footers; the table
//! renderer historically did not, so a page counter on a page that also carried
//! a table leaked through as a stray line. Two predicates live here: the broad
//! [`is_page_number_line`] used by the plain renderers, and the stricter
//! [`is_bare_page_number_line`] the table renderer applies so a leaked counter
//! is removed without also deleting machine-readable digit runs (barcode, MICR,
//! OCR strings) that happen to sit in the footer band.

use crate::layout::glyph_stream::Span;

/// Fraction of the page height, measured from the bottom, that counts as the
/// footer band. A page counter is drawn below this line.
const PAGE_NUMBER_BOTTOM_BAND: f64 = 0.055;

/// Longest run accepted as a bare page number. A one- or two-digit number alone
/// at the page bottom is a page counter; three or more digits is
/// indistinguishable from a data value (a table cell, code or amount emitted on
/// its own line) and is kept, because deleting real content is worse than
/// leaving the rare three-digit page number in place.
const BARE_PAGE_NUMBER_MAX_DIGITS: usize = 2;

/// Smallest font size, as a fraction of the page's body size, at which a bare
/// numeric footer can be a page number rather than a tiny machine-readable run
/// (barcode / MICR / OCR digit string), which must never be dropped.
const BARE_PAGE_NUMBER_MIN_SIZE_EM: f64 = 0.5;

/// Decide whether a visual line is a page-number footer (numeric-only, in the
/// bottom band of the page).
pub fn is_page_number_line(spans: &[Span], page_height: f64) -> bool {
    let y0 = spans.iter().map(|s| s.y).fold(f64::INFINITY, f64::min);
    let text: String = spans.iter().map(|s| s.text.as_str()).collect();
    let t = text.trim();
    if t.is_empty() {
        return false;
    }
    if y0 < page_height * PAGE_NUMBER_BOTTOM_BAND {
        let all_num = t
            .chars()
            .all(|c| c.is_ascii_digit() || c.is_whitespace() || c == '/' || c == '-' || c == '.');
        return all_num && t.len() <= 12;
    }
    false
}

/// Whether `spans` is a *bare* page-number footer: one one- or two-digit number
/// alone on the line, in the bottom band, set at a plausible footer size.
///
/// Deliberately stricter than [`is_page_number_line`], which accepts any short
/// numeric/slash/dot run (including tiny barcode and MICR runs). The table
/// renderer uses this shape so a leaked page counter is dropped without also
/// deleting a machine-readable digit line or a three-digit data value.
pub fn is_bare_page_number_line(spans: &[Span], page_height: f64, body_size: f64) -> bool {
    let text: String = spans.iter().map(|s| s.text.as_str()).collect();
    let t = text.trim();
    if t.is_empty()
        || t.len() > BARE_PAGE_NUMBER_MAX_DIGITS
        || !t.chars().all(|c| c.is_ascii_digit())
    {
        return false;
    }
    let y0 = spans.iter().map(|s| s.y).fold(f64::INFINITY, f64::min);
    if y0 >= page_height * PAGE_NUMBER_BOTTOM_BAND {
        return false;
    }
    let size = spans.iter().map(|s| s.size).fold(0.0f64, f64::max);
    size >= BARE_PAGE_NUMBER_MIN_SIZE_EM * body_size
}

#[cfg(test)]
mod tests {
    use super::{is_bare_page_number_line, is_page_number_line, Span};

    const PAGE_HEIGHT: f64 = 792.0;
    const BODY: f64 = 10.0;

    fn sp(text: &str, y: f64, size: f64) -> Span {
        Span {
            text: text.to_string(),
            x: 300.0,
            y,
            size,
            advance: text.len() as f64 * size * 0.6,
            word_advance: text.len() as f64 * size * 0.6,
            is_bold: false,
            is_italic: false,
            is_underline: false,
            is_vertical: false,
        }
    }

    #[test]
    fn bare_body_sized_footer_is_a_page_number() {
        let line = vec![sp("2", 20.0, BODY)];
        assert!(is_bare_page_number_line(&line, PAGE_HEIGHT, BODY));
    }

    #[test]
    fn tiny_machine_readable_run_is_not_a_page_number() {
        // A barcode / MICR digit string is set far below body size.
        let line = vec![sp("39", 20.0, 1.0)];
        assert!(!is_bare_page_number_line(&line, PAGE_HEIGHT, BODY));
    }

    #[test]
    fn long_numeric_value_is_not_a_bare_page_number() {
        for t in ["123", "12345", "123456789", "021 5", "3/10"] {
            let line = vec![sp(t, 20.0, BODY)];
            assert!(
                !is_bare_page_number_line(&line, PAGE_HEIGHT, BODY),
                "must not treat {t:?} as a bare page number"
            );
        }
    }

    #[test]
    fn numeric_line_away_from_the_bottom_band_is_not_a_page_number() {
        let line = vec![sp("2", 400.0, BODY)];
        assert!(!is_bare_page_number_line(&line, PAGE_HEIGHT, BODY));
        assert!(!is_page_number_line(&line, PAGE_HEIGHT));
    }
}
