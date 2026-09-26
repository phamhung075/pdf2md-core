// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Stage 3 & Stage 3b ruler scanning, grid alignment, and column corridor analysis.

use super::*;
use super::tests_common::*;

    /// A single word drawn as several `TJ` runs (kerned glyph chunks) must stay
    /// one token. `line_words` used to compare the *raw* start-to-start span
    /// distance against the 2.5em column threshold, so a multi-run word whose
    /// runs are cumulatively wider than 2.5em fragmented on a phantom gutter:
    /// the real invoice fixture's "Number" (`N`+`umbe`+`r`) became
    /// `["Numbe", "r"]` because `umbe` alone is ~4em wide, and the table
    /// bucketer then dropped the stray `r` into the next column.
    #[test]
    fn line_words_keeps_multispan_word_across_wide_runs() {
        let line = vec![sp("N", 0.0, 7.0), sp("umbe", 7.0, 40.0), sp("r", 47.0, 4.0)];
        let words: Vec<String> = line_words(&line).into_iter().map(|w| w.text).collect();
        assert_eq!(words, vec!["Number"], "multi-span word was fragmented: {words:?}");
    }

    /// A genuine column gutter — real whitespace wider than 2.5em *after* the
    /// previous run's own advance — must still split into separate tokens.
    #[test]
    fn line_words_still_splits_on_real_gutter() {
        let line = vec![sp("left", 0.0, 18.0), sp("right", 60.0, 25.0)];
        let words: Vec<String> = line_words(&line).into_iter().map(|w| w.text).collect();
        assert_eq!(words, vec!["left", "right"], "real gutter was not split: {words:?}");
    }

    /// Two left-aligned label cells in a totals column ("Gesamtbetrag der
    /// Zuschläge" / "…Abschläge") have the same rendered width, so their right
    /// edges fall on the same x. The end-ruler pass used to promote that shared
    /// text edge to a column boundary because neither row *straddled* it (the
    /// words end exactly on it). That split the label column, stranded the unit
    /// "EUR" in a phantom middle column, and made `consolidate_table_rows`
    /// fold four distinct totals rows into a single `<br>`-joined row. The end
    /// pass must only accept a shared right edge when the word ending there is
    /// a separate value cell, not the tail of a leading label.
    #[test]
    fn shared_label_right_edge_does_not_invent_a_column() {
        let label = |t: &str, y: f64, adv: f64| sp_at(t, 287.56, y, adv);
        let amount = |t: &str, x: f64, y: f64, adv: f64| sp_at(t, x, y, adv);
        let lines: Vec<Vec<Span>> = vec![
            vec![label("Positionssumme", 405.20, 83.98), amount("473,00", 508.03, 405.20, 36.00)],
            vec![label("Gesamtbetrag der Zuschläge", 392.38, 155.96), amount("0,00", 520.03, 392.38, 24.00)],
            vec![label("Gesamtbetrag der Abschläge", 379.56, 155.96), amount("-0,00", 514.03, 379.56, 30.00)],
            vec![label("Rechnungssumme ohne USt.", 366.74, 143.96), amount("473,00", 508.03, 366.74, 36.00)],
            vec![
                label("Steuerbetrag in", 353.92, 95.97),
                amount("EUR", 454.05, 353.92, 24.00),
                amount("56,87", 508.03, 353.92, 36.00),
            ],
            vec![label("Bruttosumme", 341.10, 65.98), amount("529,87", 508.03, 341.10, 36.00)],
            vec![label("Erhaltene Anzahlungen", 326.03, 125.96), amount("-0,00", 514.03, 326.03, 30.00)],
            vec![label("Zahlbetrag", 313.21, 59.98), amount("529,87", 508.03, 313.21, 36.00)],
        ];
        let hits = find_tables(&lines);
        let hit = hits
            .iter()
            .find(|h| h.rows.iter().any(|r| r.iter().any(|c| c.contains("Positionssumme"))))
            .expect("totals table was not detected");
        assert_eq!(
            hit.rows.iter().map(|r| r.len()).max(),
            Some(2),
            "phantom middle column appeared: {:?}",
            hit.rows
        );
        assert_eq!(hit.rows.len(), 8, "distinct totals rows were merged: {:?}", hit.rows);
        assert_eq!(hit.rows[5][0], "Bruttosumme", "row 5 folded into row 4: {:?}", hit.rows);
        assert!(
            hit.rows.iter().all(|r| !r.iter().any(|c| c.contains("<br>"))),
            "rows were folded together: {:?}",
            hit.rows
        );
    }

    /// The very same totals block, but asserting the *cell contents* the
    /// previous test left unchecked: the unit "EUR" of the label "Steuerbetrag
    /// in EUR" renders at x=454, beyond the midpoint (398) between the label
    /// ruler (288) and the amount ruler (508), so the plain midpoint bucketer
    /// filed it into the amount column and produced "EUR 56,87". The unit is
    /// part of the label cell and must stay there. Fails before the
    /// content-aware bucketing fix, passes after.
    #[test]
    fn stranded_unit_word_stays_in_its_label_cell() {
        let label = |t: &str, y: f64, adv: f64| sp_at(t, 287.56, y, adv);
        let amount = |t: &str, x: f64, y: f64, adv: f64| sp_at(t, x, y, adv);
        let lines: Vec<Vec<Span>> = vec![
            vec![label("Positionssumme", 405.20, 83.98), amount("473,00", 508.03, 405.20, 36.00)],
            vec![label("Gesamtbetrag der Zuschläge", 392.38, 155.96), amount("0,00", 520.03, 392.38, 24.00)],
            vec![label("Gesamtbetrag der Abschläge", 379.56, 155.96), amount("-0,00", 514.03, 379.56, 30.00)],
            vec![label("Rechnungssumme ohne USt.", 366.74, 143.96), amount("473,00", 508.03, 366.74, 36.00)],
            vec![
                label("Steuerbetrag in", 353.92, 95.97),
                amount("EUR", 454.05, 353.92, 24.00),
                amount("56,87", 508.03, 353.92, 36.00),
            ],
            vec![label("Bruttosumme", 341.10, 65.98), amount("529,87", 508.03, 341.10, 36.00)],
            vec![label("Erhaltene Anzahlungen", 326.03, 125.96), amount("-0,00", 514.03, 326.03, 30.00)],
            vec![label("Zahlbetrag", 313.21, 59.98), amount("529,87", 508.03, 313.21, 36.00)],
        ];
        let hits = find_tables(&lines);
        let hit = hits
            .iter()
            .find(|h| h.rows.iter().any(|r| r.iter().any(|c| c.contains("Positionssumme"))))
            .expect("totals table was not detected");
        let row = hit
            .rows
            .iter()
            .find(|r| r.iter().any(|c| c.contains("Steuerbetrag")))
            .expect("Steuerbetrag row missing");
        assert_eq!(
            row[0], "Steuerbetrag in EUR",
            "label's unit was stranded in the amount column: {:?}",
            hit.rows
        );
        assert_eq!(
            row[1], "56,87",
            "amount cell absorbed the label's unit: {:?}",
            hit.rows
        );
    }

    /// The line-item grid of `fnfe_Facture_FR_BASIC.pdf`: the quantity is drawn
    /// as "20 Unit(s)" with only an ordinary word space between the number and
    /// the unit. The unit's word start (and the number's right edge) used to be
    /// promoted to a column boundary, splitting one quantity cell into two
    /// adjacent cells separated by a word space. The window-level `flowing`
    /// veto then read that split as prose and dropped the *entire* invoice
    /// table to plain text. Neither the number/unit start nor the number's end
    /// may become a boundary. Fails before the tight-pair ruler fix, passes
    /// after.
    #[test]
    fn quantity_number_and_unit_are_one_cell() {
        let cell = |t: &str, x: f64, y: f64, adv: f64| sp_at(t, x, y, adv);
        let lines: Vec<Vec<Span>> = vec![
            vec![
                cell("Nougat de l'Abbaye 250g", 34.0, 486.0, 100.0),
                cell("20", 358.0, 486.0, 8.0),
                cell("Unit(s)", 370.0, 486.0, 22.0),
                cell("4,55 €", 432.0, 486.0, 25.0),
                cell("10%", 476.0, 486.0, 18.0),
                cell("81,90 €", 532.0, 486.0, 25.0),
            ],
            vec![
                cell("Biscuits aux raisins 300g", 34.0, 468.0, 100.0),
                cell("15", 358.0, 468.0, 8.0),
                cell("Unit(s)", 370.0, 468.0, 22.0),
                cell("3,20 €", 432.0, 468.0, 25.0),
                cell("48,00 €", 532.0, 468.0, 25.0),
            ],
            vec![
                cell("Huile d'olive à l'ancienne", 34.0, 450.0, 100.0),
                cell("25", 358.0, 450.0, 8.0),
                cell("Liter(s)", 370.0, 450.0, 22.0),
                cell("19,80 €", 427.0, 450.0, 30.0),
                cell("495,00 €", 527.0, 450.0, 30.0),
            ],
        ];
        let hits = find_tables(&lines);
        let cells: Vec<&str> = hits
            .iter()
            .flat_map(|h| h.rows.iter().flatten())
            .map(|c| c.as_str())
            .collect();
        assert!(
            cells.iter().any(|c| c.contains("20 Unit(s)")),
            "quantity number and unit must stay in one cell, got {cells:?}"
        );
        assert!(
            !cells.iter().any(|c| c.trim() == "20"),
            "the number must not become its own phantom column, got {cells:?}"
        );
    }

    /// The ranking grid of `018a81a4_tjvclassements2013_0_page0`: a tight rank
    /// number ("1", "2", "3") is drawn only a couple of points before the
    /// athlete surname ("CAPIN", "LEBRUN", "PERIOU"). `start_ruler_is_separate_cell`
    /// rejected that surname start because the start-to-start distance was under
    /// 2em, so the rank fused with the name into one cell ("1 CAPIN" /
    /// "2 LEBRUN"). A digit followed by a non-unit word is a real column
    /// boundary, so the two must be separate cells.
    #[test]
    fn rank_number_and_name_are_separate_cells() {
        let cell = |t: &str, x: f64, y: f64, adv: f64| sp_at(t, x, y, adv);
        let lines: Vec<Vec<Span>> = vec![
            vec![
                cell("1", 34.0, 500.0, 5.0),
                cell("CAPIN", 46.0, 500.0, 33.0),
                cell("Christophe", 100.0, 500.0, 52.0),
                cell("Leucémie Espoir", 200.0, 500.0, 82.0),
                cell("172", 340.0, 500.0, 18.0),
            ],
            vec![
                cell("2", 34.0, 480.0, 5.0),
                cell("LEBRUN", 46.0, 480.0, 40.0),
                cell("Tony", 100.0, 480.0, 24.0),
                cell("VS Plabennec", 200.0, 480.0, 70.0),
                cell("159", 340.0, 480.0, 18.0),
            ],
            vec![
                cell("3", 34.0, 460.0, 5.0),
                cell("PERIOU", 46.0, 460.0, 40.0),
                cell("Mathieu", 100.0, 460.0, 40.0),
                cell("Cotes d'armor cyclisme", 200.0, 460.0, 120.0),
                cell("136", 340.0, 460.0, 18.0),
            ],
        ];
        let hits = find_tables(&lines);
        let rank_row = hits
            .iter()
            .flat_map(|h| h.rows.iter())
            .find(|r| r.iter().any(|c| c.contains("CAPIN")))
            .expect("ranking grid was not detected");
        let capin = rank_row
            .iter()
            .position(|c| c.contains("CAPIN"))
            .expect("CAPIN cell missing");
        assert_eq!(
            rank_row[capin], "CAPIN",
            "surname fused with the rank number: {rank_row:?}"
        );
        assert_eq!(
            rank_row[capin - 1],
            "1",
            "rank number fused with the surname: {rank_row:?}"
        );
    }

    /// The other side of the digit-aware tight-pair rule: a genuine
    /// number+unit pair must stay one cell, not split.
    ///
    /// `quantity_number_and_unit_are_one_cell` already pins the observable
    /// *tight* "20 Unit(s)" case, but the fallback `start-to-start >= 2em`
    /// test already fuses that one, so it cannot detect a regression in the
    /// `is_unit_like` branch itself. This test adds the unit-level decision the
    /// branch actually makes — after a digit, a unit word is not a column
    /// start, while a name is (see `rank_number_and_name_are_separate_cells`)
    /// — at the non-tight geometry where the fallback alone would accept the
    /// unit start and split the pair. It also keeps an end-to-end fusion
    /// assertion on the emitted cells.
    #[test]
    fn number_and_unit_stay_one_cell() {
        let cell = |t: &str, x: f64, y: f64, adv: f64| sp_at(t, x, y, adv);
        let lines: Vec<Vec<Span>> = vec![
            vec![
                cell("Farine T55", 34.0, 486.0, 60.0),
                cell("20", 358.0, 486.0, 8.0),
                cell("kg", 370.0, 486.0, 12.0),
                cell("4,55 €", 432.0, 486.0, 25.0),
            ],
            vec![
                cell("Sucre blanc", 34.0, 468.0, 60.0),
                cell("15", 358.0, 468.0, 8.0),
                cell("kg", 370.0, 468.0, 12.0),
                cell("3,20 €", 432.0, 468.0, 25.0),
            ],
            vec![
                cell("Huile d'olive", 34.0, 450.0, 60.0),
                cell("25", 358.0, 450.0, 8.0),
                cell("kg", 370.0, 450.0, 12.0),
                cell("19,80 €", 432.0, 450.0, 25.0),
            ],
        ];
        let hits = find_tables(&lines);
        let cells: Vec<&str> = hits
            .iter()
            .flat_map(|h| h.rows.iter().flatten())
            .map(|c| c.as_str())
            .collect();
        assert!(
            cells.iter().any(|c| c.contains("20 kg")),
            "number and unit must stay in one cell, got {cells:?}"
        );
        assert!(
            !cells.iter().any(|c| c.trim() == "kg"),
            "the unit must not become its own phantom column, got {cells:?}"
        );

        // The unit-level decision the `is_unit_like` branch makes, at a
        // non-tight geometry the old fallback would have split: 358 -> 380 is
        // a full 2.2em at size 10, yet the residual gap (10pt) is under the
        // 11pt column gutter, so only the digit branch decides.
        let row = |a: &str, b: &str| {
            let line = vec![sp_at(a, 358.0, 500.0, 12.0), sp_at(b, 380.0, 500.0, 12.0)];
            let words = line_words(&line);
            let starts = words.iter().map(|w| w.x0).collect();
            let ends = words.iter().map(|w| w.x1).collect();
            RowInfo { words, starts, ends, size: 10.0 }
        };
        assert!(380.0 - 358.0 >= 2.0 * 10.0, "pair must trip the old fallback");
        assert!(
            !start_ruler_is_separate_cell(&row("20", "kg"), 380.0, 1.0, 11.0),
            "a unit after a digit must not become its own column start"
        );
        assert!(
            start_ruler_is_separate_cell(&row("1", "CAPIN"), 380.0, 1.0, 11.0),
            "a name after a digit must become its own column start"
        );
    }

    /// A sparse bilingual key/value grid: a French header row, its English twin,
    /// then one or two data rows where one data cell is blank (the receipt
    /// number) and another holds a long ticket number. All four columns are
    /// short, but the label column averages 2.25 tokens/cell — short
    /// "Nom"/"Name" cells beside a 4-token full name and a 3-token
    /// "(Adulte / Adult)" qualifier — which sat just past `is_tabular_rows`'s
    /// old 2.2 short-column bar, so the whole grid was dropped to flat
    /// paragraph text. The bilingual header doubling the row count is what
    /// pushes the mean over; the grid must still reconstruct as a table with
    /// its blank cell preserved.
    #[test]
    fn sparse_bilingual_key_value_grid_is_tabular() {
        let cell = |t: &str, x: f64, y: f64, adv: f64| sp_at(t, x, y, adv);
        let lines: Vec<Vec<Span>> = vec![
            vec![
                cell("Nom", 35.0, 600.0, 16.0),
                cell("NUMÉRO DE REÇU", 180.0, 600.0, 82.0),
                cell("Numéro de billet associé", 300.0, 600.0, 104.0),
                cell("Mode de paiement", 420.0, 600.0, 84.0),
            ],
            vec![
                cell("Name", 35.0, 586.0, 26.0),
                cell("RECEIPT NUMBER", 180.0, 586.0, 90.0),
                cell("Associated ticket number", 300.0, 586.0, 112.0),
                cell("Form of payment", 420.0, 586.0, 86.0),
            ],
            vec![
                cell("ALPHA BETA GAMMA MR", 35.0, 572.0, 108.0),
                cell("9990001112223", 300.0, 572.0, 72.0),
                cell("Carte Master/Eurocard", 420.0, 572.0, 104.0),
            ],
            vec![
                cell("(Adulte / Adult)", 35.0, 558.0, 70.0),
                cell("Card Master/Eurocard", 420.0, 558.0, 96.0),
            ],
        ];
        let hits = find_tables(&lines);
        let hit = hits
            .iter()
            .find(|h| {
                h.rows
                    .iter()
                    .flatten()
                    .any(|c| c.contains("NUMÉRO DE REÇU"))
            })
            .expect("sparse bilingual key/value grid was not reconstructed as a table");
        let width = hit.rows.iter().map(|r| r.len()).max().unwrap_or(0);
        assert_eq!(
            width, 4,
            "grid lost one of its four columns: {:?}",
            hit.rows
        );
        let row = hit
            .rows
            .iter()
            .find(|r| r.iter().any(|c| c.contains("ALPHA BETA GAMMA MR")))
            .expect("passenger name row missing");
        assert_eq!(
            row[2], "9990001112223",
            "ticket-number value was stranded out of its column: {:?}",
            hit.rows
        );
        assert_eq!(
            row[1], "",
            "the blank receipt-number cell was filled or shifted: {:?}",
            hit.rows
        );
    }

    /// A 2-column label/value grid whose *last* value wraps onto one more
    /// visual line ("RENDU DROITS NON" then "ACQUITTÉS" below it). Because the
    /// wrapped tail is the final line of the table, there is no future row for
    /// the single-match continuation branch to point at, so `has_future_match`
    /// was always false and the growth loop broke before the tail: the cell
    /// value was split in two and the tail was emitted as a stray paragraph
    /// after the table (vision against the FNFE "Facture DOM" invoice renders
    /// the cell as "RENDU DROITS NON ACQUITTÉS"). The tail must be annexed
    /// into the same row's value cell.
    #[test]
    fn wrapped_final_value_cell_is_annexed_into_its_last_row() {
        let cell = |t: &str, x: f64, y: f64, adv: f64| sp_at(t, x, y, adv);
        let lines: Vec<Vec<Span>> = vec![
            vec![cell("Votre référence", 34.1, 560.0, 70.0), cell("BC543", 154.6, 560.0, 28.0)],
            vec![cell("Réf. marché", 34.1, 543.0, 55.0), cell("WELCOME_PACK_2017", 154.6, 543.0, 105.0)],
            vec![cell("N° TVA client", 34.1, 526.0, 60.0), cell("FR90343434346", 154.6, 526.0, 78.0)],
            vec![cell("Incoterms", 34.1, 509.0, 45.0), cell("RENDU DROITS NON", 154.6, 509.0, 92.0)],
            // Wrapped tail of the Incoterms value: single cell, one row pitch down.
            vec![cell("ACQUITTÉS", 154.6, 497.0, 44.0)],
        ];
        let hits = find_tables(&lines);
        let hit = hits
            .iter()
            .find(|h| h.rows.iter().any(|r| r.iter().any(|c| c.contains("Incoterms"))))
            .expect("label/value grid was not detected");
        let incoterms = hit
            .rows
            .iter()
            .find(|r| r.iter().any(|c| c.contains("Incoterms")))
            .expect("Incoterms row missing");
        assert!(
            incoterms.iter().any(|c| c.contains("ACQUITTÉS")),
            "wrapped tail of the final value cell was dropped from the table: {:?}",
            hit.rows
        );
    }

    /// The exact regression shape from the prior attempt: a line
    /// `["81,90" @ x, " " @ x+advance, "€" @ x+advance+space]` must tokenize to
    /// the single word `"81,90 €"`. Left split, the symbol becomes a phantom
    /// last column whose near-zero gap to the number reads as prose and makes
    /// the window-level `flowing` veto reject the whole table.
    #[test]
    fn trailing_currency_symbol_is_folded_into_its_number() {
        let line = vec![sp("81,90", 100.0, 28.0), sp(" ", 128.0, 2.5), sp("€", 130.5, 9.0)];
        let words = line_words(&line);
        assert_eq!(
            words.len(),
            1,
            "a trailing currency symbol must not become its own token: {words:?}"
        );
        assert_eq!(words[0].text, "81,90 €");
        assert!((words[0].x0 - 100.0).abs() < 1e-6, "x0 moved: {}", words[0].x0);
        assert!((words[0].x1 - 139.5).abs() < 1e-6, "x1 wrong: {}", words[0].x1);
    }

    /// A bare percent unit is folded the same way ("10" + "%" → "10%").
    #[test]
    fn trailing_percent_symbol_is_folded_into_its_number() {
        let line = vec![sp("10", 200.0, 11.0), sp(" ", 211.0, 2.0), sp("%", 213.0, 8.0)];
        let words = line_words(&line);
        assert_eq!(words.len(), 1, "percent unit split off: {words:?}");
        assert_eq!(words[0].text, "10%");
    }

    /// A currency symbol separated from the preceding token by a real column
    /// gutter is a genuinely different cell and must NOT be folded in.
    #[test]
    fn currency_symbol_after_a_gutter_is_not_folded() {
        let line = vec![sp("Total", 100.0, 30.0), sp("€", 200.0, 9.0)];
        let words: Vec<String> = line_words(&line).into_iter().map(|w| w.text).collect();
        assert_eq!(words, vec!["Total", "€"], "a separate symbol cell was fused: {words:?}");
    }
