// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Reading order recovery, multi-column stream separation, and structured DocBlock generation.

    use super::*;

    fn sp(text: &str, x: f64, y: f64) -> Span {
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
    fn prose_beside_grid_row(y: f64, val: &str) -> Vec<Span> {
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
                let lt: Vec<String> = left.iter().map(|l| render_line_text(l)).collect();
                let rt: Vec<String> = right.iter().map(|l| render_line_text(l)).collect();
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
            ColumnBand::Full(rows) => assert_eq!(render_line_text(&rows[0]), "Heading"),
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
        let lt: Vec<String> = left.iter().map(|l| render_line_text(l)).collect();
        let rt: Vec<String> = right.iter().map(|l| render_line_text(l)).collect();
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
        push_band_lines(&mut out, &bands, &mut prev, &mut ls, 10.0);
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

    /// A narrow page gutter (10pt — below the `1.2em` a standalone row split
    /// needs) between a prose column (right edge 170) and a facing grid whose
    /// own internal gutter is wider (30pt). No row can seed the running-gutter
    /// pass, so only the vertical-projection fallback keeps the streams apart.
    fn narrow_gutter_prose_beside_grid() -> Vec<Vec<Span>> {
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

    #[test]
    fn narrow_gutter_prose_beside_grid_is_recovered_by_projection() {
        let lines = narrow_gutter_prose_beside_grid();
        let bands = detect_column_bands(&lines);
        assert_eq!(bands.len(), 3, "leading Full, Columns, trailing Full");
        match &bands[1] {
            ColumnBand::Columns { left, right } => {
                let lt: Vec<String> = left.iter().map(|l| render_line_text(l)).collect();
                let rt: Vec<String> = right.iter().map(|l| render_line_text(l)).collect();
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
                assert!(render_line_text(&rows[0]).contains("abcdefghij"))
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
        let lt: Vec<String> = left.iter().map(|l| render_line_text(l)).collect();
        let rt: Vec<String> = right.iter().map(|l| render_line_text(l)).collect();
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

    /// Regression for the bilingual e-ticket header whose columns are
    /// *staggered*: a French heading and its English translation sit one above
    /// the other in a narrow column, a neighbouring pair sits alongside, and
    /// their baselines interleave. Merging runs by baseline welds the columns
    /// into spurious rows, and the old top-to-bottom order threaded the English
    /// half of the first pair ("PART ONE") between the second pair's French and
    /// English halves. The staggered columns must be recovered and emitted
    /// left-to-right, each pair together, French first.
    #[test]
    fn staggered_bilingual_heading_pairs_keep_each_pair_together() {
        let lines = vec![
            vec![sp("Preface line above the block", 37.0, 700.0)],
            vec![
                sp("SECTION ONE", 37.0, 665.3),
                sp("LABEL TWO", 217.0, 666.1),
            ],
            vec![
                sp("TRANSLATION TWO", 217.0, 653.0),
                sp("SECTION THREE", 434.0, 657.7),
            ],
            vec![
                sp("PART ONE", 37.0, 651.2),
                sp("PART THREE", 454.0, 647.7),
            ],
            // A bridging row: one wide run across the first two columns. It
            // must stay after the block rather than join a column.
            vec![sp("A note that spans the first two columns", 37.0, 620.0)],
        ];

        // Flatten the page's streams, splitting each visual line back into its
        // segments so a merged row's columns are compared individually. On the
        // unfixed renderer the merged order is SECTION ONE, LABEL TWO,
        // TRANSLATION TWO, SECTION THREE, PART ONE, PART THREE — the English
        // half of the first pair lands between the second pair's halves.
        let seq: Vec<String> = page_read_order(&lines)
            .iter()
            .flatten()
            .flat_map(|l| {
                render_line_text(l)
                    .split('\n')
                    .map(|s| s.trim().to_string())
                    .collect::<Vec<_>>()
            })
            .collect();
        let pos = |n: &str| {
            seq.iter()
                .position(|t| t == n)
                .unwrap_or_else(|| panic!("missing {n:?} in {seq:?}"))
        };
        assert!(pos("SECTION ONE") < pos("PART ONE"), "first pair split: {seq:?}");
        assert!(pos("PART ONE") < pos("LABEL TWO"), "first pair not kept together: {seq:?}");
        assert!(pos("LABEL TWO") < pos("TRANSLATION TWO"), "label pair split: {seq:?}");
        assert!(
            pos("TRANSLATION TWO") < pos("SECTION THREE"),
            "label pair not kept together: {seq:?}"
        );
        assert!(pos("SECTION THREE") < pos("PART THREE"), "second pair split: {seq:?}");
        assert!(
            pos("PART THREE") < pos("A note that spans the first two columns"),
            "the bridging note must stay after the block: {seq:?}"
        );
    }

    /// Regression for `mustang_attributeBasedXMP_EN16931.pdf`: a single-column
    /// invoice page with a right-aligned amount column made the vertical-
    /// projection fallback read the page as two columns, so every amount was
    /// emitted after the whole left stream and the `blocks` channel moved a
    /// row's total (`275,00`) and a table header's last cell (`Teilzahlung`) to
    /// the end of the page. The projection's "left half" is not one prose
    /// column but a grid of table cells (label + description + quantity…), so
    /// the reading-order gate must reject it; the low-level projection detector
    /// itself stays ungated.
    #[test]
    fn cell_grid_half_is_not_a_page_column() {
        // One physical row: a label at x=50, a middle cell at x=150 (an
        // internal grid gutter), and an amount pinned to right edge 400 whose
        // left edge moves with its own length.
        let row = |y: f64, a: &str, b: &str, amount: &str| {
            vec![
                sp(a, 50.0, y),
                sp(b, 150.0, y),
                sp(amount, 400.0 - amount.len() as f64 * 6.0, y),
            ]
        };
        let lines = vec![
            row(300.0, "Positionssumme", "Basis", "473,00"),
            row(290.0, "Gesamtbetrag", "zu", "0,00"),
            row(280.0, "Abschlag", "netto", "-0,00"),
            row(270.0, "Rechnungssumme", "ohne", "473,00"),
            row(260.0, "Zahlbetrag", "frei", "529,87"),
        ];
        assert!(
            detect_projection_two_columns(&lines).is_some(),
            "the projection corridor is real; the missing reading-order gate is the bug"
        );
        let pc = page_two_columns(&lines).expect("projection fallback still detects it");
        assert!(
            !columns_are_viable_prose(&pc),
            "a half that is a grid of cells must not read as one prose column"
        );
    }

    /// The shape gate must not reject a genuine two-column page whose lines are
    /// each a single `Tj` run (one span per visual line) — the case the
    /// projection fallback exists for, and the shape the structural benchmark's
    /// `twocol_*` documents use.
    #[test]
    fn single_span_prose_columns_are_still_detected() {
        let row = |y: f64, l: &str, r: &str| vec![sp(l, 50.0, y), sp(r, 350.0, y)];
        let lines = vec![
            row(300.0, "Left line A.", "Right line A."),
            row(290.0, "Left line B.", "Right line B."),
            row(280.0, "Left line C.", "Right line C."),
            row(270.0, "Left line D.", "Right line D."),
        ];
        let pc = page_two_columns(&lines)
            .expect("a genuine two-column page must still be detected");
        assert_eq!(pc.left.len(), 4, "{:?}", pc.left);
        assert_eq!(pc.right.len(), 4, "{:?}", pc.right);
        assert!(
            columns_are_viable_prose(&pc),
            "a genuine two-column page must survive the reading-order gate"
        );
    }

    /// A visual line the glyph/line builder produced by fusing a left list item
    /// with a right callout-box heading that sits on a *different* baseline.
    /// This mirrors the target's real geometry (the marker line and the box
    /// line are 1.44pt/2.28pt apart, close enough for `build_lines`'s half-em
    /// tolerance to fuse them into one `lines` entry).
    fn fused_list_row(
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

    /// Regression for the target `SeConnecterAMonComptePartenaire.pdf`. The
    /// numbered list's items 4 and 5 are each fused onto one visual line with
    /// the adjacent callout-box heading "Accès à l'Espace / bailleur", whose two
    /// lines sit a couple of points off the list baseline. The reading-order
    /// pass split the fused rows at the gutter but then *rejected* the block
    /// (only two crossing rows) and restored each fused row whole, so the single
    /// linear stream wove item 4 → "Accès" → item 5 → "bailleur" together. With
    /// the fix the list stays contiguous and the box heading is emitted as its
    /// own block after item 5.
    #[test]
    fn fused_list_item_and_callout_heading_keep_the_list_contiguous() {
        let lines = vec![
            vec![sp("1. Renseignez vos identifiants", 50.0, 300.0)],
            vec![sp("2. Renseignez les informations", 50.0, 290.0)],
            vec![sp("3. Personnalisez votre mot de passe", 50.0, 280.0)],
            fused_list_row(270.0, "4.", ["Acceptez", "les", "règles"], 268.0, ["Accès", "à", "l’Espace"]),
            fused_list_row(260.0, "5.", ["Acceptez", "les", "conditions"], 258.0, ["bailleur", "du", "compte"]),
            vec![sp(" ", 50.0, 250.0)],
            vec![sp("Vous êtes un bailleur physique et vous gérez", 50.0, 240.0)],
        ];
        let bands = detect_column_bands(&lines);
        assert_eq!(bands.len(), 3, "Full list preamble, Columns block, Full heading");
        match &bands[1] {
            ColumnBand::Columns { left, right } => {
                let lt: Vec<String> = left.iter().map(|l| render_line_text(l)).collect();
                let rt: Vec<String> = right.iter().map(|l| render_line_text(l)).collect();
                assert!(lt[0].contains("4."), "list item 4 stays left: {lt:?}");
                assert!(lt[1].contains("5."), "list item 5 stays left: {lt:?}");
                assert!(
                    lt.iter().all(|t| !t.contains("Accès") && !t.contains("bailleur")),
                    "no box heading may be woven into the list: {lt:?}"
                );
                assert_eq!(rt.len(), 2, "the two box lines stay together right: {rt:?}");
                assert!(rt[0].contains("Accès") && rt[1].contains("bailleur"), "{rt:?}");
            }
            ColumnBand::Full(_) => panic!("the fused list/box block was merged instead of split"),
            ColumnBand::Stacks(_) => {
                panic!("the fused list/box block was merged instead of split")
            }
        }
        // And the flattened page order must read 1..5 before the box heading.
        let streams = page_read_order(&lines);
        let seq: Vec<String> = streams.iter().flatten().map(|l| render_line_text(l)).collect();
        let pos = |needle: &str| {
            seq.iter()
                .position(|t| t.contains(needle))
                .unwrap_or_else(|| panic!("missing {needle:?} in {seq:?}"))
        };
        assert!(
            pos("1.") < pos("2.") && pos("2.") < pos("3.") && pos("3.") < pos("4.") && pos("4.") < pos("5."),
            "the numbered list must stay contiguous: {seq:?}"
        );
        assert!(
            pos("5.") < pos("Accès") && pos("Accès") < pos("bailleur"),
            "the box heading must follow the list as one block: {seq:?}"
        );
    }

    /// Counter-case for the fix: a form/table row is one physical line, so its
    /// label and right-aligned value share a baseline *exactly* (measured
    /// 0.00pt on the `mustang_zugferd_2p1_EXTENDED` and `mistral_7b`
    /// fixtures). Two adjacent such rows must keep their row-wise order, not be
    /// transposed into "all labels, then all values".
    fn same_baseline_pair(y: f64, label: &str, value: [&str; 3]) -> Vec<Span> {
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

    #[test]
    fn same_baseline_label_value_rows_are_not_transposed_into_columns() {
        let lines = vec![
            same_baseline_pair(300.0, "Bestellung", [":", "B123456789", "vom"]),
            same_baseline_pair(290.0, "Weitere", [":", "A456123", "Art"]),
            vec![sp("Ende", 50.0, 280.0)],
        ];
        let bands = detect_column_bands(&lines);
        assert!(
            bands.iter().all(|b| !matches!(b, ColumnBand::Columns { .. })),
            "same-baseline label/value rows must keep their row order: {} bands",
            bands.len()
        );
    }

    /// Counter-case for the fix: when one-sided rows separate the two crossing
    /// rows (the `text_style_complex` single-column training form), the wide
    /// gaps merely happen to align. The corridor must not be trusted even
    /// though each crossing row is itself a fused pair on different baselines.
    #[test]
    fn non_adjacent_crossings_are_not_a_column_band() {
        let lines = vec![
            fused_list_row(300.0, "1.", ["A", "B", "C"], 298.0, ["D", "E", "F"]),
            vec![sp("intervening line one", 50.0, 290.0)],
            vec![sp("intervening line two", 50.0, 280.0)],
            fused_list_row(270.0, "2.", ["G", "H", "I"], 268.0, ["J", "K", "L"]),
            vec![sp("Ende", 50.0, 260.0)],
        ];
        let bands = detect_column_bands(&lines);
        assert!(
            bands.iter().all(|b| !matches!(b, ColumnBand::Columns { .. })),
            "crossings separated by one-sided rows must not form a column band: {} bands",
            bands.len()
        );
    }

    // -- multi_column_projection (3+ narrow columns) --------------------------

    fn mc_span(text: &str, x: f64, y: f64) -> Span {
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
    fn mc_fragment(words: &[&str], mut x: f64, y: f64) -> (Vec<Span>, f64) {
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
    fn mc_three_col_row(y: f64) -> Vec<Span> {
        let (mut l, x1) = mc_fragment(&["mot", "deux", "trois"], 50.0, y);
        let (mut m, x2) = mc_fragment(&["autre", "texte", "ici"], x1 + 7.0, y);
        let (mut r, _) = mc_fragment(&["encore", "des", "mots"], x2 + 7.0, y);
        l.append(&mut m);
        l.append(&mut r);
        l
    }

    #[test]
    fn three_narrow_columns_are_recovered_not_woven() {
        // A full-width header whose single span covers both gutters must stay
        // its own stream and must not be absorbed into the column region.
        let mut lines = vec![vec![mc_span("HHHHHHHHHHHHHHHHHHHHHHHHHHHHHHHH", 50.0, 800.0)]];
        for i in 0..10 {
            lines.push(mc_three_col_row(780.0 - i as f64 * 12.0));
        }
        // The projection finds the two gutters directly.
        let region = multi_column_projection(&lines).expect("3 columns recovered");
        assert_eq!(region.columns.len(), 3, "must recover three columns");
        // Each column is a single column's text, in reading order.
        let first: String = region.columns[0]
            .iter()
            .flat_map(|l| l.iter().map(|s| s.text.as_str()))
            .collect();
        assert!(first.starts_with("mot"), "left column leads: {first}");
        assert!(!first.contains("autre"), "no middle column text leaks in: {first}");
        let middle: String = region.columns[1]
            .iter()
            .flat_map(|l| l.iter().map(|s| s.text.as_str()))
            .collect();
        assert!(middle.starts_with("autre"), "middle column follows: {middle}");
        // The region excludes the full-width header.
        assert_eq!(region.start, 1, "header must not join the column region");
    }

    /// A single-column page of ordinary prose must not be carved into columns
    /// by the projection: there is no recurring 0.4em+ gutter to find.
    #[test]
    fn single_column_prose_is_not_split_by_projection() {
        let mut lines = Vec::new();
        for i in 0..12 {
            let (v, _) = mc_fragment(&["une", "ligne", "de", "prose", "ordinaire"], 50.0, 780.0 - i as f64 * 12.0);
            lines.push(v);
        }
        assert!(
            multi_column_projection(&lines).is_none(),
            "single-column prose must not project into columns"
        );
        assert_eq!(page_read_order(&lines).len(), 1);
    }

    /// Regression for the Air France e-ticket receipt block. The last visual
    /// line of the two-column block carries only a short facing segment on the
    /// left ("réservations en cliquant ici"), so the *midpoint* of the white in
    /// front of the right column recedes far left of the established gutter —
    /// while the right column still begins at its usual margin. The
    /// midpoint-only consistency filter discarded that split, `col_bottom`
    /// stopped one row above it, and the line was flushed as `bottom_full`
    /// after the entire opposite column, cutting both the French and the
    /// English sentence in half.
    #[test]
    fn short_final_row_stays_in_its_columns() {
        let full_left = ["Le", "tarif", "reserve", "est", "valable", "pour", "un", "billet"];
        let right = ["France", "web", "site", "by", "clicking"];
        let place = |words: &[&str], mut x: f64, y: f64| -> (Vec<Span>, f64) {
            let mut v = Vec::new();
            for w in words {
                v.push(sp(w, x, y));
                x += w.len() as f64 * 6.0 + 6.0;
            }
            (v, x)
        };
        let mut lines: Vec<Vec<Span>> = Vec::new();
        for i in 0..4 {
            let y = 600.0 - i as f64 * 10.0;
            let (mut row, _) = place(&full_left, 50.0, y); // left text ends at x=308
            let (mut r, _) = place(&right, 450.0, y);
            row.append(&mut r);
            lines.push(row);
        }
        let y = 560.0;
        let (mut row, _) = place(&["Vos", "en", "ici"], 50.0, y); // left text ends at x=110
        let (mut r, _) = place(&right, 450.0, y);
        row.append(&mut r);
        lines.push(row);

        let pc = page_two_columns(&lines).expect("a two-column page must be detected");
        assert!(
            pc.bottom_full.is_empty(),
            "the final row leaked past the columns: {:?}",
            pc.bottom_full
        );
        assert_eq!(pc.left.len(), 5, "left column lost its final line: {:?}", pc.left);
        assert_eq!(pc.right.len(), 5, "right column lost its final line: {:?}", pc.right);
        let tail_left: String = pc.left.last().unwrap().iter().map(|s| s.text.as_str()).collect();
        let tail_right: String = pc.right.last().unwrap().iter().map(|s| s.text.as_str()).collect();
        assert!(tail_left.contains("Vos") && tail_left.contains("ici"), "French tail not on the left: {tail_left:?}");
        assert!(
            tail_right.contains("France") && tail_right.contains("clicking"),
            "English tail not on the right: {tail_right:?}"
        );
        assert!(
            !tail_right.contains("Vos"),
            "the short French tail was woven into the right column: {tail_right:?}"
        );
    }