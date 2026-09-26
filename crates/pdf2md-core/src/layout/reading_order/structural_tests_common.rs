// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Reading order recovery, multi-column stream separation, and structured DocBlock generation.

use super::*;


    pub(super) const BODY: f64 = 10.0;

    pub(super) fn word(text: &str, x: f64, size: f64, bold: bool) -> Span {
        Span {
            text: text.to_string(),
            x,
            y: 700.0,
            size,
            advance: text.len() as f64 * size * 0.6,
            word_advance: text.len() as f64 * size * 0.6,
            is_bold: bold,
            is_italic: false,
            is_underline: false,
            is_vertical: false,
        }
    }

    pub(super) fn one_span_line(text: &str, size: f64, bold: bool) -> Vec<Span> {
        vec![word(text, 100.0, size, bold)]
    }

    // -- detect_list_marker ---------------------------------------------------

    pub(super) fn bulleted_line(marker: &str) -> Vec<Span> {
        vec![
            word(marker, 100.0, BODY, false),
            word(" ", 100.0 + marker.len() as f64 * 6.0, BODY, false),
            word("Item text", 120.0, BODY, false),
        ]
    }
