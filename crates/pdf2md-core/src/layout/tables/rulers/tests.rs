// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Stage 3 & Stage 3b ruler scanning, grid alignment, and column corridor analysis.

    use super::*;

    fn sp(text: &str, x: f64, advance: f64) -> Span {
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

    fn sp_at(text: &str, x: f64, y: f64, advance: f64) -> Span {
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

    fn sz(text: &str, x: f64, y: f64, adv: f64, size: f64) -> Span {
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
    fn split_amount(num: &str, x: f64, num_adv: f64, y: f64, size: f64) -> Vec<Span> {
        vec![
            sz(num, x, y, num_adv, size),
            sz(" ", x + num_adv, y, 1.0, size),
            sz("€", x + num_adv + 1.0, y, 6.5, size),
        ]
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

    fn wt(text: &str, x0: f64, x1: f64) -> WordTok {
        WordTok { text: text.to_string(), x0, x1 }
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