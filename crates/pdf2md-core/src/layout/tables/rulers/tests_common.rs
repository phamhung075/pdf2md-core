// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Stage 3 & Stage 3b ruler scanning, grid alignment, and column corridor analysis.

use super::*;


    pub(super) fn sp(text: &str, x: f64, advance: f64) -> Span {
        Span {
            text: text.to_string(),
            x,
            y: 100.0,
            size: 10.0,
            advance,
            word_advance: advance,
            is_bold: false,
            is_italic: false,
            is_underline: false,
            is_vertical: false,
        }
    }

    pub(super) fn sp_at(text: &str, x: f64, y: f64, advance: f64) -> Span {
        Span {
            text: text.to_string(),
            x,
            y,
            size: 10.0,
            advance,
            word_advance: advance,
            is_bold: false,
            is_italic: false,
            is_underline: false,
            is_vertical: false,
        }
    }

    pub(super) fn sz(text: &str, x: f64, y: f64, adv: f64, size: f64) -> Span {
        Span {
            text: text.to_string(),
            x,
            y,
            size,
            advance: adv,
            word_advance: adv,
            is_bold: false,
            is_italic: false,
            is_underline: false,
            is_vertical: false,
        }
    }

    /// A value drawn as two runs — the number, an explicit space span, then the
    /// currency symbol — exactly as the FNFE invoice generators emit it. Before
    /// `merge_value_symbol_tokens` this became two tokens: the number, and a
    /// `"€"` whose x0 sits exactly on the number's right edge.
    pub(super) fn split_amount(num: &str, x: f64, num_adv: f64, y: f64, size: f64) -> Vec<Span> {
        vec![
            sz(num, x, y, num_adv, size),
            sz(" ", x + num_adv, y, 1.0, size),
            sz("€", x + num_adv + 1.0, y, 6.5, size),
        ]
    }

    pub(super) fn wt(text: &str, x0: f64, x1: f64) -> WordTok {
        WordTok { text: text.to_string(), x0, x1 }
    }
