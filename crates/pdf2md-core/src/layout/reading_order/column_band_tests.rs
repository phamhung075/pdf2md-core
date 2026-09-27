// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Reading order recovery, multi-column stream separation, and structured DocBlock generation.

use super::*;
use super::column_band_tests_common::*;

    #[test]
    fn prose_beside_a_multicolumn_grid_keeps_the_column_band() {
        let lines = vec![
            prose_beside_grid_row(300.0, "V1"),
            prose_beside_grid_row(290.0, "V2"),
            prose_beside_grid_row(280.0, "V3"),
            prose_beside_grid_row(270.0, "V4"),
            // A caption row whose page gutter is only 10pt (< 1.2em): the
            // standalone detector misses it and only the established-gutter
            // fallback (`split_row_at_gutter`) can keep it in the block.
            vec![
                sp("lora", 50.0, 260.0),
                sp("ipsu", 74.0, 260.0),
                sp("dolo", 98.0, 260.0),
                sp("sita", 122.0, 260.0),
                sp("Ca", 156.0, 260.0),
                sp("Cb", 168.0, 260.0),
                sp("Cc", 180.0, 260.0),
            ],
            // A full-width heading far below must start its own block.
            vec![sp("Heading", 50.0, 150.0)],
        ];
        let bands = detect_column_bands(&lines);
        assert_eq!(bands.len(), 2, "one Columns band, then the Full heading band");
        match &bands[0] {
            ColumnBand::Columns { left, right } => {
                let lt: Vec<String> = left.iter().map(|l| render_line_text(l, None)).collect();
                let rt: Vec<String> = right.iter().map(|l| render_line_text(l, None)).collect();
                assert_eq!(lt.len(), 5, "all five prose rows stay in the left stream: {lt:?}");
                assert_eq!(rt.len(), 5, "all five grid/caption rows stay in the right stream: {rt:?}");
                assert!(
                    lt.iter().all(|t| !t.contains("Ra") && !t.contains("V")),
                    "no table cell may be woven into the prose: {lt:?}"
                );
                assert!(
                    rt.iter().all(|t| !t.contains("lora")
                        && !t.contains("ipsu")
                        && !t.contains("dolo")
                        && !t.contains("sita")),
                    "no prose may be woven into the grid stream: {rt:?}"
                );
                assert!(rt[0].contains("Ra") && rt[0].contains("V1"), "header + row 1: {rt:?}");
            }
            ColumnBand::Full(_) => panic!("mixed prose/table block was merged instead of split"),
            ColumnBand::Stacks(_) => panic!("mixed prose/table block was merged instead of split"),
        }
        match &bands[1] {
            ColumnBand::Full(rows) => assert_eq!(render_line_text(&rows[0], None), "Heading"),
            ColumnBand::Columns { .. } | ColumnBand::Stacks(_) => {
                panic!("a full-width heading must not be a column")
            }
        }
    }

    #[test]
    fn flush_body_beside_narrow_side_column_is_split() {
        // A body paragraph justified flush against a narrow side column leaves
        // no row with a `1.2em` intra-line gap, so the running-gutter pass
        // cannot seed the block and the two regions used to be woven together
        // line-by-line (`complex_p5`/`complex_p6`). The vertical projection
        // proves the straddle-free corridor; two clean wrapped text columns on
        // either side of a real (if narrow) white gutter must still split.
        //
        // The left column has a fixed right edge (x=120) and the side column a
        // fixed left edge (x=130): a 10pt corridor, below the 12pt (`1.2em`)
        // the standalone row split needs.
        let word = |t: &str, x: f64, y: f64| Span {
            text: t.to_string(),
            x,
            y,
            size: 10.0,
            advance: 22.0,
            word_advance: 22.0,
            is_bold: false,
            is_italic: false,
            is_underline: false,
            is_vertical: false,
        };
        let row = |y: f64, left: [&str; 3], right: [&str; 3]| {
            vec![
                word(left[0], 50.0, y),
                word(left[1], 74.0, y),
                word(left[2], 98.0, y),
                word(right[0], 130.0, y),
                word(right[1], 154.0, y),
                word(right[2], 178.0, y),
            ]
        };
        let lines = vec![
            row(300.0, ["alpha", "beta", "gamma"], ["caption", "one", "here"]),
            row(290.0, ["delta", "epsilon", "zeta"], ["more", "caption", "words"]),
            row(280.0, ["eta", "theta", "iota"], ["still", "the", "side"]),
            row(270.0, ["kappa", "lambda", "mu"], ["column", "keeps", "going"]),
            row(260.0, ["nu", "xi", "omicron"], ["and", "then", "ends"]),
            row(250.0, ["pi", "rho", "sigma"], ["with", "a", "line"]),
        ];
        let bands = detect_column_bands(&lines);
        let cols = bands.iter().find_map(|b| match b {
            ColumnBand::Columns { left, right } => Some((left, right)),
            _ => None,
        });
        let (left, right) = cols.unwrap_or_else(|| {
            panic!(
                "flush body + side column was not split into a Columns band ({} bands)",
                bands.len()
            )
        });
        assert_eq!(left.len(), 6, "body rows must stay in the left stream");
        assert_eq!(right.len(), 6, "side-column rows must stay in the right stream");
        let lt: Vec<String> = left.iter().map(|l| render_line_text(l, None)).collect();
        let rt: Vec<String> = right.iter().map(|l| render_line_text(l, None)).collect();
        assert!(
            lt[0].contains("alpha") && !lt[0].contains("caption"),
            "side column woven into the body: {lt:?}"
        );
        assert!(
            rt[0].contains("caption") && !rt[0].contains("alpha"),
            "body woven into the side column: {rt:?}"
        );
    }

    #[test]
    fn single_full_width_prose_column_stays_full() {
        // The counterpart guard: an ordinary full-width paragraph column has no
        // content on one side of any candidate gutter and must stay a single
        // Full band.
        let row = |y: f64, text: &str| {
            let mut v = Vec::new();
            let mut x = 50.0;
            for w in text.split_whitespace() {
                v.push(sp(w, x, y));
                x += w.len() as f64 * 6.0 + 4.0;
            }
            v
        };
        let lines = vec![
            row(300.0, "the quick brown fox jumps over the lazy dog"),
            row(290.0, "and then it runs away into the deep dark wood"),
            row(280.0, "where nobody can find it again for a long time"),
            row(270.0, "so we sit and wait for the morning to arrive"),
            row(260.0, "the end of the story is not yet written down"),
            row(250.0, "but we will know it when we see the sun rise"),
        ];
        let bands = detect_column_bands(&lines);
        assert!(
            bands.iter().all(|b| !matches!(b, ColumnBand::Columns { .. })),
            "single-column prose must not be split ({} bands)",
            bands.len()
        );
    }

    #[test]
    fn column_streams_keep_a_paragraph_break() {
        // The left stream ends with an unterminated lowercase word; the right
        // stream starts lowercase. Without a blank line between the streams the
        // paragraph reflow joins them into one sentence (column weld).
        let left = vec![
            vec![sp("alpha", 50.0, 300.0), sp("beta", 80.0, 300.0)],
            vec![sp("the", 50.0, 290.0)],
        ];
        let right = vec![vec![sp("gamma", 300.0, 300.0)]];
        let bands = vec![ColumnBand::Columns { left, right }];
        let mut out = String::new();
        let mut prev = None;
        let mut ls = ListRunState::default();
        push_band_lines(&mut out, &bands, &mut prev, &mut ls, 10.0, None);
        assert!(
            out.contains("\n\n"),
            "no blank line between column streams: {out:?}"
        );
        let reflowed = crate::reflow::reflow_markdown(&out);
        assert!(
            !reflowed.contains("the gamma"),
            "left/right column streams were welded by reflow: {reflowed:?}"
        );
    }

    #[test]
    fn a_grid_cut_in_two_is_not_a_prose_column_band() {
        // Three table columns at x=50/120/200. `split_row_columns` cuts at the
        // widest (rightmost) gap, leaving the left half still holding two
        // columns and the right half a single short cell. This is one grid, not
        // a prose/table split, so it must not become a `Columns` band (which
        // would transpose the table).
        let row = |y: f64| {
            vec![
                sp("aaaa", 50.0, y),
                sp("bbbb", 120.0, y),
                sp("cccc", 200.0, y),
            ]
        };
        let lines = vec![row(300.0), row(290.0), row(280.0), row(270.0)];
        let bands = detect_column_bands(&lines);
        assert!(
            bands.iter().all(|b| !matches!(b, ColumnBand::Columns { .. })),
            "a multi-column grid must not be read as prose columns: {:?} bands",
            bands.len()
        );
    }

    #[test]
    fn wordy_label_value_grid_is_not_transposed_into_columns() {
        // A label/value table whose long label cells give the left half a high
        // word count — a word-count-only test would call it "prose" — but the
        // cells are independent (each ends with a bracket; none continues into
        // the next). It must keep its row order rather than be split into a
        // left label stream and a right value stream.
        let row = |y: f64, val: &str| {
            vec![
                sp("Alpha", 50.0, y),
                sp("beta", 80.0, y),
                sp("gamma", 104.0, y),
                sp("delta)", 134.0, y),
                sp(val, 200.0, y),
                sp("5,5%", 240.0, y),
                sp("zz", 280.0, y),
            ]
        };
        let lines = vec![
            row(300.0, "1,91"),
            row(290.0, "2,91"),
            row(280.0, "3,91"),
            row(270.0, "4,91"),
        ];
        let bands = detect_column_bands(&lines);
        assert!(
            bands.iter().all(|b| !matches!(b, ColumnBand::Columns { .. })),
            "a label/value grid must not be transposed into columns: {:?} bands",
            bands.len()
        );
    }

    #[test]
    fn narrow_gutter_prose_beside_grid_is_recovered_by_projection() {
        let lines = narrow_gutter_prose_beside_grid();
        let bands = detect_column_bands(&lines);
        assert_eq!(bands.len(), 3, "leading Full, Columns, trailing Full");
        match &bands[1] {
            ColumnBand::Columns { left, right } => {
                let lt: Vec<String> = left.iter().map(|l| render_line_text(l, None)).collect();
                let rt: Vec<String> = right.iter().map(|l| render_line_text(l, None)).collect();
                assert_eq!(lt.len(), 6, "all six prose rows stay left: {lt:?}");
                assert_eq!(rt.len(), 4, "all four grid rows stay right: {rt:?}");
                assert!(
                    lt.iter().all(|t| t.contains("lorem") && !t.contains("Alpha")),
                    "no grid cell may be woven into the prose: {lt:?}"
                );
                assert!(
                    rt.iter().all(|t| !t.contains("lorem") && !t.contains("ipsum")),
                    "no prose may be woven into the grid: {rt:?}"
                );
            }
            ColumnBand::Full(_) => panic!("narrow-gutter prose/grid block was merged"),
            ColumnBand::Stacks(_) => panic!("narrow-gutter prose/grid block was merged"),
        }
        match &bands[0] {
            ColumnBand::Full(rows) => {
                assert!(render_line_text(&rows[0], None).contains("abcdefghij"))
            }
            ColumnBand::Columns { .. } | ColumnBand::Stacks(_) => {
                panic!("full-width caption must not be a column")
            }
        }
    }

    #[test]
    fn single_column_prose_is_not_split_by_projection_fallback() {
        // Every row crosses the middle of the text column, so no vertical
        // corridor exists and the fallback must not invent a column band.
        let row = |y: f64, words: &[&str]| {
            words
                .iter()
                .enumerate()
                .map(|(i, w)| sp(w, 50.0 + i as f64 * 26.0, y))
                .collect()
        };
        let lines = vec![
            row(300.0, &["alpha", "beta", "gamma", "delta", "epsilon"]),
            row(290.0, &["alpha", "beta", "gamma", "delta", "epsilon"]),
            row(280.0, &["alpha", "beta", "gamma", "delta", "epsilon"]),
            row(270.0, &["alpha", "beta", "gamma", "delta", "epsilon"]),
            row(260.0, &["alpha", "beta", "gamma", "delta", "epsilon"]),
            row(250.0, &["alpha", "beta", "gamma", "delta", "epsilon"]),
        ];
        let bands = detect_column_bands(&lines);
        assert!(
            bands.iter().all(|b| !matches!(b, ColumnBand::Columns { .. })),
            "single-column prose must not be split into columns: {:?} bands",
            bands.len()
        );
    }

    /// Regression for the bilingual (French/English side-by-side) Air France
    /// e-ticket. The page holds *two* independent two-column blocks. The first
    /// is drawn one span per word, so the running-gutter pass seeds a `Columns`
    /// band from it. The second draws every visual line as a single `Tj` run:
    /// `split_row_columns` needs >=5 spans and `avg_words_per_row` counts spans,
    /// so the running-gutter pass cannot see it — and because the page already
    /// produced a `Columns` band, the old projection fallback (gated on "no
    /// `Columns` band at all") was skipped, leaving the two languages woven row
    /// by row. The projection is now applied to every leftover `Full` band, and
    /// the two-clean-columns gate measures words rather than spans.
    #[test]
    fn bilingual_single_run_columns_split_beside_an_existing_column_band() {
        let mut lines: Vec<Vec<Span>> = Vec::new();
        // First block: one span per word, so `split_row_columns` seeds it.
        for i in 0..4 {
            let y = 780.0 - i as f64 * 10.0;
            let mut row = Vec::new();
            for k in 0..4 {
                row.push(sp(&format!("c{i}{k}"), 50.0 + k as f64 * 24.0, y));
            }
            for k in 0..4 {
                row.push(sp(&format!("v{i}{k}"), 350.0 + k as f64 * 24.0, y));
            }
            lines.push(row);
        }
        // Bilingual block: one run per line, a wider facing gutter. French
        // continuations start lowercase, English entries start capitalized —
        // `wrapped_prose` is true for the left column only, which the gate now
        // accepts.
        let fr = [
            "Le texte francais commence ici",
            "et continue sur la ligne suivante",
            "encore une suite de mots ici",
            "puis la fin du paragraphe",
            "une autre phrase commence",
            "et sa continuation finale",
        ];
        let en = [
            "Site internet Air France",
            "Air France website section",
            "Par telephone au zero neuf",
            "By phone at zero nine",
            "Dans un point de vente",
            "At an Air France point of sale",
        ];
        for (i, (l, r)) in fr.iter().zip(en.iter()).enumerate() {
            let y = 620.0 - i as f64 * 10.0;
            lines.push(vec![sp(l, 50.0, y), sp(r, 420.0, y)]);
        }

        let bands = detect_column_bands(&lines);
        let cols: Vec<(&Vec<Vec<Span>>, &Vec<Vec<Span>>)> = bands
            .iter()
            .filter_map(|b| match b {
                ColumnBand::Columns { left, right } => Some((left, right)),
                _ => None,
            })
            .collect();
        assert_eq!(
            cols.len(),
            2,
            "both two-column blocks must be recovered ({} bands)",
            bands.len()
        );
        let (left, right) = cols[1];
        assert_eq!(left.len(), 6, "the French lines must stay left: {left:?}");
        assert_eq!(right.len(), 6, "the English lines must stay right: {right:?}");
        let lt: Vec<String> = left.iter().map(|l| render_line_text(l, None)).collect();
        let rt: Vec<String> = right.iter().map(|l| render_line_text(l, None)).collect();
        assert!(
            lt[0].contains("Le texte francais") && !lt[0].contains("Site internet"),
            "English woven into the French column: {lt:?}"
        );
        assert!(
            rt[0].contains("Site internet") && !rt[0].contains("Le texte francais"),
            "French woven into the English column: {rt:?}"
        );
    }

    /// Regression for the `enedis_hp_hc` false positive the bilingual
    /// relaxation introduced: running furniture/table fragments where a
    /// recurring vertical gap cuts one flowing block in two. Both halves are
    /// clean, wordy and wrap-continue, so the word-based two-clean-columns gate
    /// alone admits them; only the starting-edge spread tells them apart from a
    /// real column — the left half's rows start at wildly different x (measured
    /// 100-360pt on enedis against 12-22pt on the bilingual e-ticket). The
    /// projection must leave these rows whole.
    #[test]
    fn high_start_spread_fragments_are_not_a_text_column() {
        // The left span migrates 50 -> 250 (200pt spread); the right span stays
        // put. Every row reads as wrapped prose, so nothing but the spread
        // rejects the split.
        let left = [
            "alpha beta gamma delta",
            "et continue ici encore",
            "encore une suite de mots",
            "puis la fin du passage",
            "une autre phrase commence",
            "et sa continuation finale",
        ];
        let right = [
            "premier element de la ligne",
            "deuxieme element suivant ici",
            "troisieme element de la suite",
            "quatrieme element encore la",
            "cinquieme element de la liste",
            "sixieme element final ici",
        ];
        let mut lines: Vec<Vec<Span>> = Vec::new();
        for i in 0..6 {
            let y = 700.0 - i as f64 * 10.0;
            lines.push(vec![
                sp(left[i], 50.0 + i as f64 * 40.0, y),
                sp(right[i], 500.0, y),
            ]);
        }
        let bands = detect_column_bands(&lines);
        assert!(
            bands.iter().all(|b| !matches!(b, ColumnBand::Columns { .. })),
            "high-spread fragments must not be split into columns: {} bands",
            bands.len()
        );
    }
