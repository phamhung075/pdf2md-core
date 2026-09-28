//! pdf2md-core — High-performance native Rust core engine for sub-millisecond
//! PDF-to-Markdown extraction and 2D spatial canvas table reconstruction.
//!
//! Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
//! SPDX-License-Identifier: BSL-1.1
//! Licensed under the Business Source License 1.1 (BSL-1.1).

use super::*;
use super::regression_tests_common::*;
use super::regression_tests_common2::*;
    
        

    /// Regression for the `enedis_facture_hta` integration bug: a page whose
    /// retained *last* block is a lone short heading (`# 8`) while the repeated
    /// letterhead before it (in the footer band) is dropped must keep its
    /// trailing blank-line page separator. Without it the next page's first
    /// block — a table row — is welded on with zero characters (`# 8|   | …`),
    /// which is not a valid GFM table start.
    #[test]
    fn collapse_repeated_furniture_blocks_keeps_page_boundary_before_next_page_table() {
        let letterhead = "Enedis, SA a directoire et a conseil de surveillance\n\
                          Tour Enedis 92079 Paris La Defense Cedex - RCS de NANTERRE 444608442";
        let table =
            "|   | Page « Détails des éléments |\n| --- | --- |\n|   | facturés hors taxes » |";
        // Each page chunk ends with `\n\n` — the separator page assembly always
        // appends (see the `chunk.push_str("\n\n")` at page build time).
        let mut pages: Vec<(u32, String)> = vec![
            (
                1,
                format!("{letterhead}\n\nintro one\n\nbody one\n\nbody two\n\nbody three\n\n# 1\n\n"),
            ),
            // Page 2 ends with the retained lone heading, immediately after the
            // dropped furniture: exactly the enedis shape.
            (
                2,
                format!("header two\n\nintro two\n\nbody two a\n\nbody two b\n\n{letterhead}\n\n# 8\n\n"),
            ),
            // Page 3 begins with the table block (no blank line before it).
            (
                3,
                format!("header three\n\nintro three\n\nbody three a\n\nbody three b\n\n{letterhead}\n\n{table}\n\n"),
            ),
            (
                4,
                format!("header four\n\nintro four\n\nbody four a\n\nbody four b\n\n{letterhead}\n\n# 10\n\n"),
            ),
        ];

        collapse_repeated_furniture_blocks(&mut pages);

        let all = pages
            .iter()
            .map(|(_, s)| s.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        // (a) Letterhead dedup still holds: first occurrence kept, later dropped.
        assert_eq!(
            all.matches("Enedis, SA a directoire").count(),
            1,
            "letterhead dedup lost: {all}"
        );
        // (b) The page ending in the lone heading keeps a newline boundary.
        assert!(
            pages[1].1.ends_with('\n'),
            "rebuilt page must keep a boundary: {:?}",
            pages[1].1
        );
        // (c) Production concatenates pages with `push_str` and no separator:
        // the heading must never be glued to the following table row.
        let mut out = String::new();
        for (_, s) in &pages {
            out.push_str(s);
        }
        assert!(
            !out.contains("# 8|"),
            "heading welded to following table row: {out}"
        );
        assert!(
            out.contains("# 8\n"),
            "heading must be separated from the table by a newline: {out}"
        );
    }

    /// A near-identical footer whose only varying line is a real data value is
    /// not furniture and must survive on every page.
    #[test]
    fn collapse_repeated_furniture_blocks_keeps_distinct_data_blocks() {
        let footer = |amount: &str| {
            format!("Releve de compte\n| Total | {amount} |\n| --- | --- |\n| Reglement | 08/08/2024 |")
        };
        let mut pages: Vec<(u32, String)> = (1..=4)
            .map(|i| {
                (
                    i,
                    format!(
                        "lead {i}\n\na\n\nb\n\nc\n\n{}",
                        footer(&format!("1 234,5{i}"))
                    ),
                )
            })
            .collect();
        collapse_repeated_furniture_blocks(&mut pages);
        let all = pages.iter().map(|(_, s)| s.as_str()).collect::<Vec<_>>().join("\n");
        for i in 1..=4 {
            assert!(
                all.contains(&format!("1 234,5{i}")),
                "per-page amount {i} must survive: {all}"
            );
        }
    }

    /// A repeated block that sits in the middle of a page (outside the
    /// header/footer bands) is body content and must not be collapsed.
    #[test]
    fn collapse_repeated_furniture_blocks_keeps_mid_page_repeats() {
        let mid = "repeated middle paragraph";
        let mut pages: Vec<(u32, String)> = (1..=4)
            .map(|i| {
                (
                    i,
                    format!("first {i}\n\nsecond {i}\n\n{mid}\n\nfourth {i}\n\nfifth {i}"),
                )
            })
            .collect();
        collapse_repeated_furniture_blocks(&mut pages);
        let all = pages.iter().map(|(_, s)| s.as_str()).collect::<Vec<_>>().join("\n");
        assert_eq!(
            all.matches(mid).count(),
            4,
            "mid-page repeated block must survive on every page: {all}"
        );
    }

    /// A wholly-repeated sparse page (<= 4 blocks) is content, not furniture.
    #[test]
    fn collapse_repeated_furniture_blocks_keeps_sparse_repeated_pages() {
        let body = "TICKET DE CAISSE\nArticle un 5,00\nArticle deux 7,50\nTOTAL 12,50";
        let mut pages: Vec<(u32, String)> = (1..=3).map(|i| (i, body.to_string())).collect();
        collapse_repeated_furniture_blocks(&mut pages);
        let all = pages.iter().map(|(_, s)| s.as_str()).collect::<Vec<_>>().join("\n");
        assert_eq!(
            all.matches("TICKET DE CAISSE").count(),
            3,
            "sparse repeated page must survive: {all}"
        );
    }

    /// A ratio inside a table row and as a standalone footer-style line must
    /// both survive: it is not a corroborated page counter.
    #[test]
    fn synthetic_ratio_cell_and_standalone_survive() {
        let pages: Vec<String> = (1..=4)
            .map(|i| {
                [
                    tm_text(72.0, 815.0, &format!("RATIO-SHEET-{i}")),
                    tm_text(72.0, 720.0, "Ratio"),
                    tm_text(72.0, 705.0, "3/4"),
                    tm_text(72.0, 300.0, &format!("unique prose line number {i} for this sheet only")),
                    tm_text(72.0, 90.0, "3/4"),
                ]
                .join("\n")
            })
            .collect();
        let md = convert_synth(&pages);
        assert_eq!(count_occurrences(&md, "3/4"), 8, "ratio cells/footers lost:\n{md}");
        for i in 1..=4 {
            assert!(md.contains(&format!("RATIO-SHEET-{i}")), "page {i} header lost:\n{md}");
        }
    }

    /// `Sous-total page N: <amount>` data rows must survive: the label carries
    /// a varying amount, not a plain counter.
    #[test]
    fn synthetic_subtotal_rows_survive() {
        let amounts = ["120,50", "98,00", "75,25", "61,10"];
        let pages: Vec<String> = (0..4)
            .map(|i| {
                [
                    tm_text(72.0, 815.0, &format!("RELEVE-{} EN-TETE COURANT", i + 1)),
                    tm_text(72.0, 500.0, &format!("ligne de donnees propre a la page {}", i + 1)),
                    tm_text(72.0, 95.0, &format!("Sous-total page {}: {}", i + 1, amounts[i])),
                ]
                .join("\n")
            })
            .collect();
        let md = convert_synth(&pages);
        for a in amounts {
            assert!(md.contains(a), "amount {a} lost:\n{md}");
        }
        assert_eq!(count_occurrences(&md, "Sous-total"), 4, "subtotal rows lost:\n{md}");
    }

    /// An identical amount row repeated in the footer band on every page is a
    /// real data row, not running furniture: it must survive on every page.
    #[test]
    fn synthetic_identical_footer_amount_row_survives() {
        let pages: Vec<String> = (1..=4)
            .map(|i| {
                [
                    tm_text(72.0, 815.0, "RELEVE MENSUEL"),
                    tm_text(72.0, 500.0, &format!("operation unique de la page {i}")),
                    tm_text(72.0, 80.0, "Frais de dossier: 45,00"),
                ]
                .join("\n")
            })
            .collect();
        let md = convert_synth(&pages);
        assert_eq!(
            count_occurrences(&md, "45,00"),
            4,
            "identical footer amount row suppressed as furniture:\n{md}"
        );
        assert_eq!(
            count_occurrences(&md, "Frais de dossier"),
            4,
            "identical footer amount label suppressed as furniture:\n{md}"
        );
    }

    /// A column header repeated at the top of the next page's table is the same
    /// table continuing: the header survives once and every page's data rows
    /// follow it, in page order.
    #[test]
    fn synthetic_repeated_montant_header_survives() {
        let pages: Vec<String> = (1..=3)
            .map(|i| {
                [
                    tm_text(72.0, 780.0, "Montant"),
                    tm_text(300.0, 780.0, "Detail"),
                    tm_text(72.0, 765.0, &format!("poste-{i}A")),
                    tm_text(300.0, 765.0, "100,00"),
                    tm_text(72.0, 750.0, &format!("poste-{i}B")),
                    tm_text(300.0, 750.0, "200,00"),
                ]
                .join("\n")
            })
            .collect();
        let md = convert_synth(&pages);
        assert_eq!(count_occurrences(&md, "Montant"), 1, "repeated header kept:\n{md}");
        assert_eq!(
            count_occurrences(&md, "| --- | --- |"),
            1,
            "cross-page table not merged:\n{md}"
        );
        let a1 = md.find("poste-1A").expect("page 1 row");
        let a2 = md.find("poste-2A").expect("page 2 row");
        let a3 = md.find("poste-3A").expect("page 3 row");
        assert!(a1 < a2 && a2 < a3, "rows out of page order:\n{md}");
    }

    /// Distinct postal codes must never be folded into one furniture key.
    #[test]
    fn synthetic_postal_codes_survive() {
        let pages: Vec<String> = (1..=4)
            .map(|i| {
                let city = if i % 2 == 1 { "75001 PARIS" } else { "69001 LYON" };
                [
                    tm_text(72.0, 800.0, &format!("Adresse: {city}")),
                    tm_text(72.0, 780.0, &format!("dossier numero {i}0000001")),
                    tm_text(72.0, 300.0, &format!("texte metier distinct page {i}")),
                ]
                .join("\n")
            })
            .collect();
        let md = convert_synth(&pages);
        assert_eq!(count_occurrences(&md, "75001 PARIS"), 2, "postal code lost:\n{md}");
        assert_eq!(count_occurrences(&md, "69001 LYON"), 2, "postal code lost:\n{md}");
    }

    /// `Page 2/7`, `2 / 7`, `2/7` counters that vary across pages are dropped.
    #[test]
    fn synthetic_page_counters_are_removed() {
        let forms = ["Page 2/7", "2 / 7", "2/7", "Page 5/7"];
        let pages: Vec<String> = (1..=4)
            .map(|i| {
                [
                    tm_text(72.0, 815.0, forms[i - 1]),
                    tm_text(72.0, 300.0, &format!("contenu reel de la page {i} a conserver")),
                    tm_text(72.0, 90.0, forms[i % 4]),
                ]
                .join("\n")
            })
            .collect();
        let md = convert_synth(&pages);
        for f in forms {
            assert!(!md.contains(f), "counter {f} survived:\n{md}");
        }
        assert_eq!(count_occurrences(&md, "contenu reel"), 4, "body lost:\n{md}");
    }

    /// A legal footer whose only varying field is a page number is dropped,
    /// while a body line that merely says "corps page N" survives.
    #[test]
    fn synthetic_page_varying_legal_footer_removed_body_kept() {
        let pages: Vec<String> = (1..=4)
            .map(|i| {
                [
                    tm_text(72.0, 700.0, &format!("corps page {i}")),
                    tm_text(72.0, 80.0, &format!("Societe Exemple SAS - RCS 123 456 789 - page {i}")),
                ]
                .join("\n")
            })
            .collect();
        let md = convert_synth(&pages);
        assert_eq!(count_occurrences(&md, "RCS"), 1, "page-varying footer kept:\n{md}");
        assert_eq!(count_occurrences(&md, "corps page"), 4, "unique body line dropped:\n{md}");
    }

    /// A ticket whose lines repeat identically on every page must survive.
    #[test]
    fn synthetic_repeated_page_ticket_survives() {
        let body = ["TICKET DE CAISSE", "Article un 5,00", "Article deux 7,50", "TOTAL 12,50"];
        let page = td_text(&body, false);
        let pages = vec![page; 3];
        let md = convert_synth(&pages);
        assert_eq!(
            count_occurrences(&md, "TICKET DE CAISSE"),
            3,
            "repeated ticket content emptied:\n{md}"
        );
        for b in body {
            assert_eq!(count_occurrences(&md, b), 3, "line {b} lost:\n{md}");
        }
    }

    /// A horizontally-positioned `Tj`+`Td` prose page must keep every line.
    #[test]
    fn synthetic_td_prose_page_keeps_content() {
        let body = [
            "Le contenu de cette page est dispose",
            "par fragments successifs avec des",
            "deplacements horizontaux puis verticaux",
            "afin de tester le routage vers",
            "le moteur de mise en page du document",
        ];
        let pages = vec![td_text(&body, true); 3];
        let md = convert_synth(&pages);
        for b in body {
            assert_eq!(count_occurrences(&md, b), 3, "prose line {b} lost:\n{md}");
        }
    }

    /// A `'`-only letter (one `Tm`, zero leading) must keep word boundaries
    /// instead of being merged into a single fragment.
    #[test]
    fn synthetic_quote_show_letter_keeps_words() {
        let body = [
            "Objet: votre demande de dossier",
            "Madame, Monsieur,",
            "Nous accusons reception de votre courrier",
            "et vous remercions de votre confiance.",
        ];
        let pages = vec![quote_text(&body); 2];
        let md = convert_synth(&pages);
        assert!(!md.contains("dossierMadame"), "quote fragments merged:\n{md}");
        for b in body {
            assert!(md.contains(b), "letter line lost: {b}\n{md}");
        }
    }

    /// A table header row immediately preceding data rows without a ruler
    /// must be annexed into the markdown table header rather than emitted
    /// as loose prose.
    #[test]
    fn synthetic_preceding_header_row_annexation() {
        let page = [
            tm_text(40.0, 500.0, "Code"),
            tm_text(140.0, 500.0, "Description"),
            tm_text(300.0, 500.0, "Montant"),
            tm_text(40.0, 485.0, "A10"),
            tm_text(140.0, 485.0, "Prestation de service"),
            tm_text(300.0, 485.0, "150.00"),
            tm_text(40.0, 470.0, "B20"),
            tm_text(140.0, 470.0, "Fourniture materiel"),
            tm_text(300.0, 470.0, "230.00"),
            tm_text(40.0, 455.0, "C30"),
            tm_text(140.0, 455.0, "Frais de deplacement"),
            tm_text(300.0, 455.0, "45.00"),
        ]
        .join("\n");
        let md = convert_synth(&[page]);
        assert!(
            md.contains("| Code | Description | Montant |") || md.contains("|Code|Description|Montant|"),
            "header row was not annexed into markdown table; md was:\n{md}"
        );
        assert!(md.contains("| A10 |") || md.contains("|A10|"), "row A10 missing from table:\n{md}");
    }

    /// Multi-word cells across columns must bucket properly without cross-column corruption.
    #[test]
    fn synthetic_multi_word_cell_midpoint_bucketing() {
        let page = [
            tm_text(40.0, 500.0, "Compte principal"),
            tm_text(180.0, 500.0, "Assistance sur site"),
            tm_text(340.0, 500.0, "1 250,00 EUR"),
            tm_text(40.0, 485.0, "Compte secondaire"),
            tm_text(180.0, 485.0, "Formation utilisateurs"),
            tm_text(340.0, 485.0, "840,00 EUR"),
            tm_text(40.0, 470.0, "Compte tertiaire"),
            tm_text(180.0, 470.0, "Support annuel"),
            tm_text(340.0, 470.0, "3 100,00 EUR"),
        ]
        .join("\n");
        let md = convert_synth(&[page]);
        assert!(md.contains("Assistance sur site"), "missing Assistance sur site:\n{md}");
        assert!(md.contains("1 250,00 EUR"), "missing amount 1 250,00 EUR:\n{md}");
        assert!(!md.contains("Compte 1 250,00"), "cell bucket leaked across columns:\n{md}");
    }

    /// Stopword-dense French account labels beside numeric amount columns must survive
    /// as a table and not be dropped as flowing prose.
    #[test]
    fn synthetic_numeric_stopword_bypass_on_invoice_grids() {
        let page = [
            tm_text(40.0, 500.0, "011"),
            tm_text(90.0, 500.0, "Charges a caractere general"),
            tm_text(320.0, 500.0, "35 799.00 EUR"),
            tm_text(40.0, 485.0, "12"),
            tm_text(90.0, 485.0, "Virement de la section de fonctionnement"),
            tm_text(320.0, 485.0, "7 100.00 EUR"),
            tm_text(40.0, 470.0, "20"),
            tm_text(90.0, 470.0, "Dotations fonds divers et reserve"),
            tm_text(320.0, 470.0, "16 024.00 EUR"),
            tm_text(40.0, 455.0, "21"),
            tm_text(90.0, 455.0, "Immobilisations en cours"),
            tm_text(320.0, 455.0, "0.00 EUR"),
        ]
        .join("\n");
        let md = convert_synth(&[page]);
        assert!(
            md.lines().any(|l| l.trim().starts_with('|') && l.contains("Virement de la section")),
            "stopword-dense grid was collapsed into prose:\n{md}"
        );
    }

    // ---- Detached right-aligned amount: the wide-gap exception --------------

    /// A label and its lone right-aligned monetary value share one PDF baseline;
    /// the wide-gap hard break must keep them on one Markdown line. Uses only
    /// synthetic placeholder text.
    #[test]
    fn synthetic_right_aligned_amount_stays_with_its_label() {
        let page = [
            tm_text(72.0, 600.0, "Total a payer"),
            tm_text(430.0, 600.0, "1 234,56"),
        ]
        .join("\n");
        let md = convert_synth(&[page]);
        assert!(
            md.lines()
                .any(|l| l.contains("Total a payer") && l.contains("1 234,56")),
            "right-aligned amount was detached from its label:\n{md}"
        );
    }

    /// Two genuine prose columns must still be split: the exception is limited
    /// to a lone monetary right segment.
    #[test]
    fn synthetic_two_prose_columns_still_split() {
        let page = [
            tm_text(72.0, 600.0, "Premiere colonne de texte."),
            tm_text(320.0, 600.0, "Deuxieme colonne de texte."),
            tm_text(72.0, 584.0, "Suite de la premiere colonne."),
            tm_text(320.0, 584.0, "Suite de la deuxieme colonne."),
            tm_text(72.0, 568.0, "Fin de la premiere colonne."),
            tm_text(320.0, 568.0, "Fin de la deuxieme colonne."),
        ]
        .join("\n");
        let md = convert_synth(&[page]);
        assert!(
            !md.lines()
                .any(|l| l.contains("premiere colonne") && l.contains("deuxieme colonne")),
            "two prose columns were welded onto one line:\n{md}"
        );
        assert!(md.contains("premiere colonne") && md.contains("deuxieme colonne"));
    }

    /// A bare non-amount number (a page reference) is not a monetary token, so
    /// the right margin alone must not fuse it to the label.
    #[test]
    fn synthetic_bare_page_reference_number_still_splits() {
        let page = [
            tm_text(72.0, 600.0, "Article"),
            tm_text(540.0, 600.0, "12"),
        ]
        .join("\n");
        let md = convert_synth(&[page]);
        assert!(
            !md.lines()
                .any(|l| l.contains("Article") && l.contains("12")),
            "bare page reference was joined to its label:\n{md}"
        );
    }

    /// A two-column table-ish row of amounts has no letter on the left, so the
    /// exception does not apply and the columns stay split.
    #[test]
    fn synthetic_two_amount_columns_still_split() {
        let page = [
            tm_text(72.0, 600.0, "45,00"),
            tm_text(430.0, 600.0, "120,00"),
        ]
        .join("\n");
        let md = convert_synth(&[page]);
        assert!(
            !md.lines()
                .any(|l| l.contains("45,00") && l.contains("120,00")),
            "two amount columns were welded onto one line:\n{md}"
        );
    }

    /// A label followed by *two* wide-separated amount columns: the right
    /// remainder is not a single value, so the exception must not fire and the
    /// row must split exactly as it did before the exception existed.
    #[test]
    fn synthetic_label_with_two_amount_columns_still_splits() {
        let page = [
            tm_text(72.0, 600.0, "Total a payer"),
            tm_text(300.0, 600.0, "1 234,56"),
            tm_text(480.0, 600.0, "9 876,54"),
        ]
        .join("\n");
        let md = convert_synth(&[page]);
        assert!(
            !md.lines()
                .any(|l| l.contains("1 234,56") && l.contains("9 876,54")),
            "two amount columns were welded together:\n{md}"
        );
        assert!(
            !md.lines()
                .any(|l| l.contains("Total a payer") && l.contains("1 234,56")),
            "the label swallowed a two-column amount row:\n{md}"
        );
    }

    /// A French/ISO date (`dd.mm.yyyy`) carries a second separator, so the
    /// monetary-amount shape rejects it and the wide gap still breaks.
    #[test]
    fn synthetic_label_with_date_still_splits() {
        let page = [
            tm_text(72.0, 600.0, "Echeance"),
            tm_text(430.0, 600.0, "12.01.2026"),
        ]
        .join("\n");
        let md = convert_synth(&[page]);
        assert!(
            !md.lines()
                .any(|l| l.contains("Echeance") && l.contains("12.01.2026")),
            "date was kept on its label line:\n{md}"
        );
    }

    /// A 2-decimal percentage is not money: the `%` is rejected by the shape.
    #[test]
    fn synthetic_label_with_percentage_still_splits() {
        let page = [
            tm_text(72.0, 600.0, "Taux"),
            tm_text(430.0, 600.0, "12,50 %"),
        ]
        .join("\n");
        let md = convert_synth(&[page]);
        assert!(
            !md.lines()
                .any(|l| l.contains("Taux") && l.contains("12,50")),
            "percentage was kept on its label line:\n{md}"
        );
    }

    /// A bare 4-digit reference has no mandatory decimal part, so it is not a
    /// monetary amount and stays detached from the label.
    #[test]
    fn synthetic_label_with_four_digit_reference_still_splits() {
        let page = [
            tm_text(72.0, 600.0, "Reference"),
            tm_text(430.0, 600.0, "1234"),
        ]
        .join("\n");
        let md = convert_synth(&[page]);
        assert!(
            !md.lines()
                .any(|l| l.contains("Reference") && l.contains("1234")),
            "four-digit reference was joined to its label:\n{md}"
        );
    }

    /// A label whose last token ends in a bare 1..=3-digit number could be read
    /// as the leading thousands group of the right-side amount once joined
    /// ("12 345,67"), so the exception must not fire and the gap still splits.
    #[test]
    fn synthetic_label_with_trailing_number_still_splits() {
        let page = [
            tm_text(72.0, 600.0, "Label 12"),
            tm_text(430.0, 600.0, "345,67"),
        ]
        .join("\n");
        let md = convert_synth(&[page]);
        assert!(
            !md.lines()
                .any(|l| l.contains("Label 12") && l.contains("345,67")),
            "a trailing label number was welded to the amount:\n{md}"
        );
    }

    /// Same fusion risk when the label's last token is an alphanumeric code
    /// ending in digits ("AB12 345,67"), so the gap still splits.
    #[test]
    fn synthetic_label_with_trailing_alphanumeric_code_still_splits() {
        let page = [
            tm_text(72.0, 600.0, "Code AB12"),
            tm_text(430.0, 600.0, "345,67"),
        ]
        .join("\n");
        let md = convert_synth(&[page]);
        assert!(
            !md.lines()
                .any(|l| l.contains("Code AB12") && l.contains("345,67")),
            "a trailing code was welded to the amount:\n{md}"
        );
    }

    /// A label without any trailing digit run carries no grouping ambiguity, so
    /// the lone right-aligned amount is still kept on its line.
    #[test]
    fn synthetic_label_without_trailing_digit_keeps_amount() {
        let page = [
            tm_text(72.0, 600.0, "Total TTC"),
            tm_text(430.0, 600.0, "345,67"),
        ]
        .join("\n");
        let md = convert_synth(&[page]);
        assert!(
            md.lines()
                .any(|l| l.contains("Total TTC") && l.contains("345,67")),
            "amount was detached from a digit-free label:\n{md}"
        );
    }
