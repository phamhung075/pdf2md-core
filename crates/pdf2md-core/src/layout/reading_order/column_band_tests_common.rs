// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Reading order recovery, multi-column stream separation, and structured DocBlock generation.

use super::*;


    pub(super) fn sp(text: &str, x: f64, y: f64) -> Span {
        Span {
            text: text.to_string(),
            x,
            y,
            size: 10.0,
            advance: text.len() as f64 * 6.0,
            word_advance: text.len() as f64 * 6.0,
            is_bold: false,
            is_italic: false,
            is_underline: false,
            is_vertical: false,
        }
    }

    /// One row of a mixed prose/table two-column block. The four lower-case
    /// left-column words end at x=146; the table label starts at 176 (a 30pt
    /// page gutter) and its value at 215 (a 15pt internal table gutter). The
    /// page gutter is wider than the table's own gutter, so
    /// `split_row_columns` cuts at the page gutter; `clean(right)` is false
    /// because the right half is itself a two-column grid. The all-lowercase
    /// left rows read as a wrapped prose column (each row continues the next).
    pub(super) fn prose_beside_grid_row(y: f64, val: &str) -> Vec<Span> {
        vec![
            sp("lora", 50.0, y),
            sp("ipsu", 74.0, y),
            sp("dolo", 98.0, y),
            sp("sita", 122.0, y),
            sp("Ra", 176.0, y),
            sp("Rb", 188.0, y),
            sp(val, 215.0, y),
        ]
    }

    /// A narrow page gutter (10pt — below the `1.2em` a standalone row split
    /// needs) between a prose column (right edge 170) and a facing grid whose
    /// own internal gutter is wider (30pt). No row can seed the running-gutter
    /// pass, so only the vertical-projection fallback keeps the streams apart.
    pub(super) fn narrow_gutter_prose_beside_grid() -> Vec<Vec<Span>> {
        let prose = |y: f64| {
            vec![
                sp("lorem", 50.0, y),
                sp("ipsum", 74.0, y),
                sp("dolor", 98.0, y),
                sp("sitam", 122.0, y),
                sp("amet", 146.0, y),
            ]
        };
        let grid = |y: f64, label: &str, v1: &str, v2: &str| {
            vec![sp(label, 180.0, y), sp(v1, 258.0, y), sp(v2, 292.0, y)]
        };
        vec![
            // Full-width caption above the block, straddling the gutter.
            vec![sp("abcdefghijklmnopqrst", 100.0, 320.0)],
            prose(300.0),
            prose(290.0),
            grid(295.0, "Alphaone", "1047", "7.2"),
            prose(280.0),
            grid(285.0, "Betaxtwo", "1031", "6.8"),
            prose(270.0),
            grid(275.0, "Gammathr", "1012", "6.6"),
            prose(260.0),
            grid(265.0, "Deltarfou", "1041", "6.5"),
            prose(250.0),
            // Full-width paragraph below the block, straddling the gutter.
            vec![sp("abcdefghijklmnopqrst", 100.0, 200.0)],
        ]
    }

    /// A visual line the glyph/line builder produced by fusing a left list item
    /// with a right callout-box heading that sits on a *different* baseline.
    /// This mirrors the target's real geometry (the marker line and the box
    /// line are 1.44pt/2.28pt apart, close enough for `build_lines`'s half-em
    /// tolerance to fuse them into one `lines` entry).
    pub(super) fn fused_list_row(
        left_y: f64,
        marker: &str,
        left_words: [&str; 3],
        right_y: f64,
        right_words: [&str; 3],
    ) -> Vec<Span> {
        let mut v = vec![sp(marker, 50.0, left_y)];
        let mut x = 70.0;
        for w in left_words {
            v.push(sp(w, x, left_y));
            x += w.len() as f64 * 6.0 + 8.0;
        }
        let mut xr = 250.0;
        for w in right_words {
            v.push(sp(w, xr, right_y));
            xr += w.len() as f64 * 6.0 + 6.0;
        }
        v
    }

    /// Counter-case for the fix: a form/table row is one physical line, so its
    /// label and right-aligned value share a baseline *exactly* (measured
    /// 0.00pt on the `mustang_zugferd_2p1_EXTENDED` and `mistral_7b`
    /// fixtures). Two adjacent such rows must keep their row-wise order, not be
    /// transposed into "all labels, then all values".
    pub(super) fn same_baseline_pair(y: f64, label: &str, value: [&str; 3]) -> Vec<Span> {
        let mut v = vec![
            sp(label, 50.0, y),
            sp("Referenz", 130.0, y),
            sp("Nr", 180.0, y),
        ];
        let mut x = 250.0;
        for w in value {
            v.push(sp(w, x, y));
            x += w.len() as f64 * 6.0 + 6.0;
        }
        v
    }

    // -- multi_column_projection (3+ narrow columns) --------------------------

    pub(super) fn mc_span(text: &str, x: f64, y: f64) -> Span {
        Span {
            text: text.to_string(),
            x,
            y,
            size: 10.0,
            advance: text.len() as f64 * 6.0,
            word_advance: text.len() as f64 * 6.0,
            is_bold: false,
            is_italic: false,
            is_underline: false,
            is_vertical: false,
        }
    }

    /// Lay `words` left-to-right from `x` with 3pt inter-word gaps, returning
    /// the spans and the x just past the last word.
    pub(super) fn mc_fragment(words: &[&str], mut x: f64, y: f64) -> (Vec<Span>, f64) {
        let mut v = Vec::new();
        for w in words {
            v.push(mc_span(w, x, y));
            x += w.len() as f64 * 6.0 + 3.0;
        }
        (v, x)
    }

    /// One fused visual row of a 3-column page: three prose fragments sharing a
    /// baseline, separated by ~7pt gutters (well below the 2.5em hard break and
    /// the 1.2em `split_row_columns` seed). Every fragment starts lowercase and
    /// ends without a terminator, so each column reads as wrapped prose.
    pub(super) fn mc_three_col_row(y: f64) -> Vec<Span> {
        let (mut l, x1) = mc_fragment(&["mot", "deux", "trois"], 50.0, y);
        let (mut m, x2) = mc_fragment(&["autre", "texte", "ici"], x1 + 7.0, y);
        let (mut r, _) = mc_fragment(&["encore", "des", "mots"], x2 + 7.0, y);
        l.append(&mut m);
        l.append(&mut r);
        l
    }
