// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Stage 3 & Stage 3b ruler scanning, grid alignment, and column corridor analysis.

use super::*;
use super::tests_common::*;

    /// Regression for the four fixtures the previous growth-boundary relaxation
    /// corrupted (`fnfe_Avoir_FR_type381_BASIC`, `fnfe_Facture_UE_*`,
    /// `mustang_validAvoir_FR_type380_BASICWL`): a two-row line-item grid whose
    /// totals/VAT block reuses the right-aligned amount edge, followed by a
    /// multi-line anchor-column label ("Taxe Base Montant Total HT" / "TVA
    /// collectée (vente)" / "Total taxes" / "Total TTC"). Once the currency
    /// symbols are folded the line-item grid is clean, so the only thing that
    /// used to stop the growth loop was the `flowing` veto — a totals row that
    /// shares the amount column's right edge could then be annexed, fabricating
    /// a single garbled product/totals row. An oversized title (20pt vs the
    /// table's 9pt) inflates the page-wide tolerance, which is exactly what let
    /// the totals row "match" the amount column in the 2×-tolerance gap pass.
    /// The totals block must never enter the line-item hit.
    #[test]
    fn totals_block_is_never_annexed_into_the_line_item_grid() {
        let mut lines: Vec<Vec<Span>> = Vec::new();
        // A 16pt title elsewhere on the page inflates the pass-wide tolerance
        // past the table's own 9pt scale — enough that a totals row 2.5pt off
        // the amount edge looks aligned in the 2×-tolerance gap pass.
        lines.push(vec![sz("AVOIR AV-2017-0005", 100.0, 700.0, 160.0, 16.0)]);

        // Two line items (9pt), each amount drawn as number + space + "€".
        let mut item_a = vec![
            sz("Nougat de l'Abbaye 250g", 34.1, 452.6, 100.8, 9.0),
            sz("5", 362.5, 452.6, 5.0, 9.0),
            sz("Unit(s)", 370.2, 452.6, 20.6, 9.0),
        ];
        item_a.extend(split_amount("4,55", 431.8, 17.5, 452.6, 9.0));
        item_a.push(sz("10%", 475.6, 452.6, 18.0, 9.0));
        item_a.extend(split_amount("-20,48", 528.8, 25.5, 452.6, 9.0));
        lines.push(item_a);

        let mut item_b = vec![
            sz("Huile d'olive à l'ancienne", 34.1, 434.4, 98.5, 9.0),
            sz("10", 357.5, 434.4, 10.0, 9.0),
            sz("Liter(s)", 370.2, 434.4, 21.8, 9.0),
        ];
        item_b.extend(split_amount("19,80", 426.8, 22.5, 434.4, 9.0));
        item_b.extend(split_amount("-198,00", 523.8, 30.5, 434.4, 9.0));
        lines.push(item_b);

        // Totals/VAT/Règlement block (7-10pt) reusing the amount right edge
        // (x≈561.8) and a multi-line anchor label.
        lines.push(vec![
            sz("Taxe", 88.8, 415.6, 15.4, 7.0),
            sz("Base", 188.3, 415.6, 16.8, 7.0),
            sz("Montant", 254.1, 415.6, 27.2, 7.0),
            sz("Total", 454.3, 415.6, 23.1, 7.0),
            sz("HT", 477.4, 415.6, 16.1, 7.0),
            sz("-218,48 €", 519.6, 415.6, 42.3, 7.0),
        ]);
        lines.push(vec![
            sz("TVA", 34.1, 402.0, 13.4, 7.0),
            sz("collectée", 47.0, 402.0, 29.3, 7.0),
            sz("(vente)", 76.4, 402.0, 23.6, 7.0),
            sz("20,0%", 100.0, 402.0, 21.7, 7.0),
            sz("-20,48 €", 203.2, 402.0, 25.7, 7.0),
            sz("-4,10 €", 279.3, 402.0, 21.8, 7.0),
        ]);
        lines.push(vec![
            sz("Total", 442.0, 395.5, 23.2, 10.0),
            sz("taxes", 465.2, 395.5, 28.2, 10.0),
            sz("-14,99 €", 525.1, 395.5, 36.8, 10.0),
        ]);
        lines.push(vec![
            sz("TVA", 34.1, 388.4, 13.4, 7.0),
            sz("collectée", 47.0, 388.4, 29.3, 7.0),
            sz("(vente)", 76.4, 388.4, 23.6, 7.0),
            sz("5,5%", 100.0, 388.4, 17.8, 7.0),
            sz("-198,00 €", 199.3, 388.4, 29.6, 7.0),
            sz("-10,89 €", 275.4, 388.4, 25.7, 7.0),
        ]);
        lines.push(vec![
            sz("Total TTC", 448.2, 378.2, 45.3, 10.0),
            sz("-233,47 €", 519.6, 378.2, 42.3, 10.0),
        ]);

        // The full detector: strict pass plus the 2×-tolerance stage-3b pass.
        let mut hits = find_tables(&lines);
        hits.extend(find_gap_tables(&lines, &hits));

        let item_hit = hits
            .iter()
            .find(|h| h.rows.iter().any(|r| r.iter().any(|c| c.contains("Nougat"))))
            .unwrap_or_else(|| panic!("line-item grid was not detected, hits={hits:?}"));
        let joined = item_hit
            .rows
            .iter()
            .flatten()
            .cloned()
            .collect::<Vec<_>>()
            .join(" | ");
        assert!(
            item_hit.rows.iter().any(|r| r.iter().any(|c| c.contains("Huile"))),
            "both line items must be in the grid: {joined}"
        );
        for forbidden in [
            "Taxe", "Base", "Montant", "Total", "taxes", "TTC", "TVA", "-218,48", "-14,99",
            "-233,47",
        ] {
            assert!(
                !joined.contains(forbidden),
                "totals/VAT label {forbidden:?} was annexed into the line-item hit: {joined}"
            );
        }
        assert_eq!(item_hit.rows.len(), 2, "totals rows merged into the grid: {:?}", item_hit.rows);
    }

    /// A narrow two-column table set in the page margin, sharing every visual
    /// line with the main column's prose. `find_tables` used to scan the merged
    /// lines, so the seed-and-grow window compared the table's rows against the
    /// prose and the `flowing` veto dropped the whole table to plain text. The
    /// detector must now isolate the right-hand column band and recover the
    /// table without annexing the prose.
    #[test]
    fn side_table_beside_prose_is_detected_without_the_prose() {
        let prose = |x: f64, y: f64, t: &str, adv: f64| sp_at(t, x, y, adv);
        let mut lines: Vec<Vec<Span>> = Vec::new();
        let params = [
            ("Parameter", "Value", "Unit"),
            ("dim", "4096", "params"),
            ("n_layers", "32", "layers"),
            ("head_dim", "128", "dim"),
            ("hidden_dim", "14336", "dim"),
            ("n_heads", "32", "heads"),
            ("vocab_size", "32000", "tokens"),
        ];
        for (i, (name, value, unit)) in params.iter().enumerate() {
            let y = 700.0 - 12.0 * i as f64;
            lines.push(vec![
                prose(40.0, y, "alpha", 28.0),
                prose(74.0, y, "beta", 24.0),
                prose(104.0, y, "gamma", 32.0),
                prose(142.0, y, "delta", 28.0),
                prose(300.0, y, name, 30.0),
                prose(360.0, y, value, 26.0),
                prose(410.0, y, unit, 30.0),
            ]);
        }
        let hits = find_tables(&lines);
        let hit = hits
            .iter()
            .find(|h| h.rows.iter().any(|r| r.iter().any(|c| c.contains("n_layers"))))
            .unwrap_or_else(|| panic!("side table was not detected, hits={hits:?}"));
        let joined = hit.rows.iter().flatten().cloned().collect::<Vec<_>>().join(" | ");
        assert!(
            joined.contains("4096") && joined.contains("32000"),
            "table cells missing: {joined}"
        );
        assert!(
            !joined.contains("alpha") && !joined.contains("delta"),
            "neighboring prose was annexed into the side table: {joined}"
        );
        assert!(
            hit.bbox.x0 >= 290.0,
            "hit must be confined to the right-hand column band: {:?}",
            hit.bbox
        );
    }

    /// A sparse value column (only one data row fills the last cell — the
    /// "± 0.07" of the Mistral benchmark grid) must not make the consolidator
    /// treat every following row as a wrapped continuation and fold the whole
    /// grid into one `<br>`-joined row.
    #[test]
    fn sparse_value_column_does_not_fold_rows_into_one() {
        let lines: Vec<Vec<Span>> = (0..5)
            .map(|i| vec![sp_at("x", 40.0, 700.0 - 12.0 * i as f64, 8.0)])
            .collect();
        let info: Vec<RowInfo> = lines
            .iter()
            .map(|l| RowInfo {
                words: line_words(l),
                starts: vec![40.0],
                ends: vec![48.0],
                size: 10.0,
            })
            .collect();
        let win_rows: Vec<usize> = (0..5).collect();
        let rows = vec![
            vec!["Model".into(), "MT".into(), "ELO".into()],
            vec!["A".into(), "1".into(), "2".into(), "+/- 0.07".into()],
            vec!["B".into(), "3".into(), "4".into(), "".into()],
            vec!["C".into(), "5".into(), "6".into(), "".into()],
            vec!["D".into(), "7".into(), "8".into(), "".into()],
        ];
        let out = consolidate_table_rows(rows, &win_rows, &lines, &info);
        assert_eq!(
            out.len(),
            5,
            "data rows were folded together: {out:?}"
        );
        for row in &out[1..] {
            assert!(
                !row.iter().any(|c| c.contains("<br>")),
                "distinct data rows were joined with <br>: {out:?}"
            );
        }
    }

    /// A sign drawn as its own run before the line's final amount must fold
    /// onto it, so the deduction keeps its sign instead of splitting into a
    /// lone "-" cell and a separate "93" cell.
    #[test]
    fn merge_sign_tokens_joins_trailing_signed_amount() {
        let words = vec![wt("revenu", 0.0, 30.0), wt("-", 50.0, 53.0), wt("93", 86.0, 100.0)];
        let out = merge_sign_tokens(words);
        assert_eq!(out.len(), 2, "sign was not folded: {out:?}");
        assert_eq!(out[1].text, "-93");
    }

    /// An inline hyphen inside a longer run ("022 735 - 3477 (service …)") is
    /// not a signed trailing amount and must be left as drawn.
    #[test]
    fn merge_sign_tokens_leaves_inline_hyphen_alone() {
        let words = vec![
            wt("022", 0.0, 20.0),
            wt("735", 22.0, 40.0),
            wt("-", 42.0, 45.0),
            wt("3477", 60.0, 80.0),
            wt("(service", 82.0, 120.0),
        ];
        let out = merge_sign_tokens(words);
        assert!(out.iter().any(|w| w.text == "-"), "inline hyphen was folded: {out:?}");
        assert!(out.iter().any(|w| w.text == "3477"));
    }

    /// A later pass must not build a band that bridges rows another pass already
    /// claimed: the resulting hit would report the whole contiguous line range
    /// while omitting those covered rows, so `de_overlap_tables` would keep it
    /// and silently drop the covered table's data.
    #[test]
    fn gap_band_does_not_bridge_a_covered_region() {
        let lines = vec![
            vec![sp_at("label", 0.0, 400.0, 40.0), sp_at("100", 200.0, 400.0, 25.0)],
            vec![sp_at("covered", 0.0, 388.0, 60.0), sp_at("200", 200.0, 388.0, 25.0)],
            vec![sp_at("covered2", 0.0, 376.0, 65.0), sp_at("300", 200.0, 376.0, 25.0)],
            vec![sp_at("tail", 0.0, 364.0, 30.0), sp_at("400", 200.0, 364.0, 25.0)],
        ];
        let covered = vec![TableHit {
            start: 1,
            end: 2,
            rows: Vec::new(),
            bbox: BoundingBox::new(0.0, 0.0, 0.0, 0.0),
        }];
        let hits = scan_aligned_grids_opts(&lines, 2.0, &covered, false);
        assert!(
            hits.iter().all(|h| h.end < 1 || h.start > 2),
            "a gap hit bridged the covered rows: {hits:?}"
        );
    }

    /// A numeric column whose values are *centred* in their cell shares neither
    /// a left nor a right edge across rows ("34155", "1146" and "5679" all
    /// differ in start and end), so the start and end ruler passes both see no
    /// column and the grid was emitted as loose text. The centre pass must
    /// recover it, keeping the left-aligned label column on its start ruler.
    #[test]
    fn centered_numeric_columns_are_recovered() {
        let word = |t: &str, center: f64, width: f64, y: f64| sp_at(t, center - width / 2.0, y, width);
        let row = |label: &str, v1: &str, v2: &str, v3: &str, y: f64| {
            vec![
                sp_at(label, 30.0, y, 40.0),
                word(v1, 150.0, 30.0, y),
                word(v2, 210.0, 30.0, y),
                word(v3, 270.0, 30.0, y),
            ]
        };
        let lines = vec![
            row("Alpha", "34155", "43354", "60219", 200.0),
            row("Beta", "1146", "2212", "3994", 186.0),
            row("Gamma", "5679", "7697", "9484", 172.0),
            row("Delta", "10335", "20335", "30335", 158.0),
        ];
        let hits = find_tables(&lines);
        assert_eq!(hits.len(), 1, "centred grid not recovered: {hits:?}");
        assert_eq!(hits[0].start, 0);
        assert_eq!(hits[0].end, 3);
        assert_eq!(
            hits[0].rows[0].len(),
            4,
            "centred grid collapsed into one cell: {:?}",
            hits[0].rows
        );
        assert!(
            hits[0].rows.iter().any(|r| r.contains(&"5679".to_string())),
            "centred value missing from cells: {:?}",
            hits[0].rows
        );
    }

    /// A left-aligned value column whose values happen to be equal-width also
    /// has a consistent centre. The centre pass must not add a redundant ruler
    /// beside the start ruler it already has, which would leave a phantom empty
    /// column in the emitted grid.
    #[test]
    fn left_aligned_equal_width_values_gain_no_centre_ruler() {
        let row = |label: &str, v: &str, y: f64| {
            vec![sp_at(label, 30.0, y, 40.0), sp_at(v, 150.0, y, 30.0)]
        };
        let lines = vec![
            row("Alpha", "34155", 200.0),
            row("Beta", "11467", 186.0),
            row("Gamma", "56790", 172.0),
            row("Delta", "10335", 158.0),
        ];
        let hits = find_tables(&lines);
        assert_eq!(hits.len(), 1, "left-aligned grid not recovered: {hits:?}");
        assert_eq!(
            hits[0].rows[0].len(),
            2,
            "a spurious centre ruler split the left-aligned column: {:?}",
            hits[0].rows
        );
    }

    /// C4 `12ac87e6_FM17-Scratch_GR_0`: the header cell "Dossard" (74..119) is
    /// wider than the bib numbers beneath it ("263" at 90..108). It therefore
    /// straddles the bib start ruler at 90 and the old `row_straddles` vetoed
    /// that ruler for the whole table, fusing every row's bib and name into
    /// "| 22 494 PUYMEGE |". A cell that is its own cell and merely contains
    /// the ruler (it does not span a second ruler) must not delete the boundary
    /// 52 rows agree on.
    #[test]
    fn wide_header_cell_does_not_veto_the_numeric_ruler() {
        let cell = |t: &str, x: f64, y: f64, adv: f64| sp_at(t, x, y, adv);
        let mut lines: Vec<Vec<Span>> = Vec::new();
        lines.push(vec![
            cell("Dossard", 74.0, 300.0, 45.0),
            cell("NOM", 130.0, 300.0, 30.0),
            cell("Prénom", 230.0, 300.0, 45.0),
        ]);
        let bibs = ["494", "478", "512", "333", "201"];
        let names = ["PUYMEGE", "LELIEVRE", "MARTIN", "DUPONT", "DURAND"];
        let firsts = ["Jérome", "Warren", "Alice", "Bob", "Carol"];
        for i in 0..5 {
            let y = 288.0 - 12.0 * i as f64;
            lines.push(vec![
                cell(bibs[i], 90.0, y, 18.0),
                cell(names[i], 130.0, y, 55.0),
                cell(firsts[i], 230.0, y, 40.0),
            ]);
        }
        let hits = find_tables(&lines);
        let hit = hits
            .iter()
            .find(|h| h.rows.iter().any(|r| r.iter().any(|c| c.contains("PUYMEGE"))))
            .unwrap_or_else(|| panic!("Dossard table not detected: {hits:?}"));
        let joined = hit.rows.iter().flatten().cloned().collect::<Vec<_>>().join(" | ");
        assert!(
            !joined.contains("494 PUYMEGE"),
            "bib and name were fused into one cell: {joined}"
        );
        assert!(
            hit.rows
                .iter()
                .any(|r| r.iter().any(|c| c.trim() == "494")),
            "the bib must be its own cell: {joined}"
        );
    }

    /// C3 `1c060490_Annexe_2012-I-13_fr_10`: a 10-column grid's header row has
    /// ten words, so the old hard `row.words.len() > 8` cap refused to annex it
    /// and the header was emitted as loose text. The cap must scale with the
    /// detected column count. The header is offset a fraction of a point from
    /// the data rulers so it cannot seed the grid itself and annexation is what
    /// recovers it.
    #[test]
    fn multi_column_header_over_eight_words_is_annexed() {
        let ncol = 10usize;
        let x_of = |c: usize| 50.0 + 62.0 * c as f64;
        let mut lines: Vec<Vec<Span>> = Vec::new();
        let header: Vec<Span> = (0..ncol)
            .map(|c| sp_at(&format!("Col{c}"), x_of(c) + 0.8, 300.0, 30.0))
            .collect();
        lines.push(header);
        for r in 0..4 {
            let y = 288.0 - 12.0 * r as f64;
            let row: Vec<Span> = (0..ncol)
                .map(|c| sp_at(&format!("{}", r * ncol + c + 1), x_of(c), y, 14.0))
                .collect();
            lines.push(row);
        }
        let hits = find_tables(&lines);
        let hit = hits
            .iter()
            .find(|h| h.rows.iter().any(|r| r.iter().any(|c| c.contains("Col1"))))
            .unwrap_or_else(|| panic!("wide table was not detected: {hits:?}"));
        assert!(
            hit.rows.len() >= 5,
            "the 10-word header row was not annexed: {:?}",
            hit.rows
        );
        assert!(
            hit.rows[0].iter().any(|c| c.contains("Col1"))
                && hit.rows[0].iter().any(|c| c.contains("Col9")),
            "wide header cells missing from row 0: {:?}",
            hit.rows[0]
        );
    }

    /// `complex_p5`: a two-cell table header whose right label is centred over
    /// the value column (`MT` starts ~1.4pt right of the `6.84` ruler) must
    /// still be annexed to the table. The pass-wide `tol` is under 1pt on that
    /// page, so the exact `tol * 1.5` test matched only one ruler and
    /// `Guardrails`/`MT Bench` was emitted as detached bold body text instead
    /// of the table's header row.
    #[test]
    fn centred_header_label_still_matches_a_value_column_ruler() {
        let header = vec![
            sp("Guardrails", 379.0, 66.0),
            sp("MT", 456.0, 14.0),
            sp("Bench", 472.0, 25.0),
        ];
        let words = line_words(&header);
        let starts = words.iter().map(|w| w.x0).collect();
        let ends = words.iter().map(|w| w.x1).collect();
        let info = vec![RowInfo {
            words,
            starts,
            ends,
            size: 10.0,
        }];
        let rulers = vec![387.52, 454.56, 472.50, 497.60];
        assert!(
            header_like_row(&info, 0, &rulers, 0.6, 6.0),
            "a header whose label is centred over the value column was rejected"
        );
    }
