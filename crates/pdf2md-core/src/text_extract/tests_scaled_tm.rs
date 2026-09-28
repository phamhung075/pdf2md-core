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
use super::tests_common::*;

    #[test]
    fn scaled_tm_baseline_moves_start_new_lines() {
        // A producer that writes `/F1 1 Tf` plus a scaled `Tm` per line inside
        // one `BT`/`ET`: the scale makes one text-space unit ten device points.
        // Only the identity `Tm` used to start a line, so every visual line
        // fused into a single run.
        let doc = content_doc(
            b"BT /F1 1 Tf\n\
              10 0 0 10 50 800 Tm (PREMIERE LIGNE) Tj\n\
              10 0 0 10 50 780 Tm (DEUXIEME LIGNE) Tj ET",
        );
        let page = extract_page_text_report(&doc, 1, false, false, false).expect("page text");
        assert_eq!(page.text, "PREMIERE LIGNE\nDEUXIEME LIGNE");
    }

    #[test]
    fn scaled_tm_same_baseline_restores_the_word_gap() {
        // Two objects on one baseline: a left label and its right-aligned
        // amount. The scaled `Tm` carries the amount's device origin, so the
        // walker inserts the separator the strings do not contain instead of
        // welding the label to its amount.
        let doc = content_doc(
            b"BT /F1 1 Tf\n\
              10 0 0 10 50 800 Tm (Montant disponible :) Tj\n\
              10 0 0 10 300 800 Tm (1 000,00) Tj ET",
        );
        let page = extract_page_text_report(&doc, 1, false, false, false).expect("page text");
        assert_eq!(page.text, "Montant disponible : 1 000,00");
    }
