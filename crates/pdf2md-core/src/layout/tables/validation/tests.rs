// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Table validation heuristics: TOC dot leader rejection, bullet markers, stopword flow, and data tokens.

    use super::*;

    /// A French account/amount grid's labels are stopword-dense ("Virement de
    /// la section …", "Dotations fonds divers réserve"), so the prose stopword
    /// detector used to discard it even though the amount column is a
    /// structural table signal no paragraph has. A dedicated numeric column
    /// must override the stopword veto and keep the grid.
    #[test]
    fn numeric_amount_column_overrides_french_stopwords() {
        let rows: Vec<Vec<String>> = vec![
            vec!["011".into(), "Charges à caractère général".into(), "35 799.00 €".into()],
            vec!["12".into(), "Virement de la section de fon".into(), "7 100.00 €".into()],
            vec!["20".into(), "Dotations fonds divers réserve".into(), "16 024.00 €".into()],
            vec!["21".into(), "Immobilisations en cours".into(), "0.00 €".into()],
        ];
        assert!(
            is_tabular_rows(&rows),
            "a numeric amount column must override the stopword veto: {rows:?}"
        );
    }

    /// The numeric-column escape must not accept prose: with no dedicated
    /// amount column the stopword share still vetoes the grid.
    #[test]
    fn stopword_dense_grid_without_numeric_column_is_rejected() {
        let rows: Vec<Vec<String>> = vec![
            vec!["La commune de la".into(), "section des travaux".into()],
            vec!["Le projet de la".into(), "ville et de la".into()],
            vec!["Les élus de la".into(), "commune sont".into()],
            vec!["Une partie de la".into(), "voie est".into()],
        ];
        assert!(!is_tabular_rows(&rows), "stopword-dense prose was accepted: {rows:?}");
    }

    /// A municipal voting matrix (`03bb566c_PV-CM-11-06-2020_3`): a descriptive
    /// first column of full names and seven `x` marker columns. It carries no
    /// digit, date or currency, so `has_data_tokens` used to be false and
    /// `is_tabular_rows` rejected the grid solely because the name column's
    /// mean exceeded 2 words/cell. Marker columns are a structural table signal
    /// and must keep the grid.
    #[test]
    fn voting_matrix_with_marker_columns_and_names_is_tabular() {
        let rows: Vec<Vec<String>> = vec![
            vec![
                "Membre".into(), "Point 1".into(), "Point 2".into(), "Point 3".into(),
                "Point 4".into(), "Point 5".into(), "Point 6".into(), "Point 7".into(),
            ],
            vec![
                "Isabelle Cazaubon (Adjointe)".into(), "x".into(), "x".into(), "x".into(),
                "".into(), "x".into(), "x".into(), "x".into(),
            ],
            vec![
                "Bertrand Caubraque".into(), "x".into(), "".into(), "x".into(),
                "x".into(), "".into(), "x".into(), "x".into(),
            ],
            vec![
                "Marie Dupont (Maire)".into(), "x".into(), "x".into(), "".into(),
                "x".into(), "x".into(), "".into(), "x".into(),
            ],
        ];
        assert!(
            has_data_tokens(&rows[1]),
            "an 'x' marker row must count as data: {:?}",
            rows[1]
        );
        assert!(
            is_tabular_rows(&rows),
            "a voting matrix with marker columns must be tabular: {rows:?}"
        );
    }

    /// A technical specification grid (`175bfc79_8155_pim_0`): range+unit cells
    /// like "2 ÷ 10 bar" or "- 20°C ÷ 80°C" hold three or four whitespace
    /// tokens, so no column passed the old 2.2-token short-column test and the
    /// grid was rejected as prose. Measurement-like columns are data columns.
    #[test]
    fn technical_spec_range_and_unit_column_is_tabular() {
        let rows: Vec<Vec<String>> = vec![
            vec!["Pression maximale de travail".into(), "2 ÷ 10 bar".into()],
            vec!["Température minimale de service".into(), "- 20°C ÷ 80°C".into()],
            vec!["Débit nominal de la pompe".into(), "16 ÷ 100".into()],
            vec!["Tension d'alimentation électrique".into(), "230 V".into()],
        ];
        assert!(
            is_tabular_rows(&rows),
            "a range/unit specification grid must be tabular: {rows:?}"
        );
    }

    /// Short status words ("oui", "non") and lone symbols are data markers too.
    /// A bare hyphen / plus, or the French abbreviation "no", is not a marker:
    /// ordinary prose lines carry them, so they must not flag a whole column.
    #[test]
    fn short_status_words_are_data_markers() {
        assert!(has_data_tokens(&["oui".to_string()]));
        assert!(has_data_tokens(&["Non".to_string()]));
        assert!(has_data_tokens(&["✓ : validation rules".to_string()]));
        assert!(has_data_tokens(&["x x x".to_string()]));
        assert!(!is_marker_token("-"));
        assert!(!is_marker_token("+"));
        assert!(!is_marker_token("no"));
        assert!(!has_data_tokens(&["-".to_string()]));
        assert!(!has_data_tokens(&["+".to_string()]));
        assert!(!has_data_tokens(&["Nom du membre".to_string()]));
        // A prose cell that merely contains a status word is not a marker.
        assert!(!has_data_tokens(&["Note non applicable".to_string()]));
    }

    /// A French bibliography / prose column ("Paris, 1994, 182 p.", "no 6",
    /// "à la ville") holds digits but no technical unit. The old `UNITS` list
    /// contained single-letter words ("a", "l", "m") and short words ("min"),
    /// so ordinary prose tokens matched, the whole column was flagged as a
    /// `measurement_col`, and the stopword veto was bypassed. Only attached
    /// single-character units or an uppercase V/W/A after a number may match.
    #[test]
    fn french_prose_words_are_not_measurements() {
        assert!(!looks_like_measurement("Paris, 1994, 182 p. à la ville"));
        assert!(!looks_like_measurement("no 6"));
        assert!(!looks_like_measurement("min"));
        assert!(looks_like_measurement("230 V"));
        assert!(looks_like_measurement("10m"));
        assert!(looks_like_measurement("2 ÷ 10 bar"));
    }

    /// `00094916_Addictionssansdrogues_13`: a 3-column bibliography of running
    /// prose. The cell "Document Toxibase n ° 101621" carries the French
    /// *numéro* abbreviation with a standalone degree glyph; treating any `°`
    /// as a measurement flagged the column as `measurement_col`, bypassed the
    /// prose rejection, and emitted the bibliography as a GFM table. A degree
    /// sign only counts when a digit abuts it.
    #[test]
    fn french_bibliography_with_numero_degree_is_not_tabular() {
        let rows: Vec<Vec<String>> = vec![
            vec![
                "Réduction des risques, Paris, 1997, 8 p.".into(),
                "".into(),
                "net http://www.redpsy.com/infopsy/cyberdepen-".into(),
            ],
            vec!["".into(), "Psychologues, 1997, (144), 45-48".into(), "".into()],
            vec!["".into(), "".into(), "dance2.html, 10 p.".into()],
            vec![
                "GRÉCO ; GROUPE RECHERCHES ÉTUDES".into(),
                "Document Toxibase n ° 101621".into(),
                "".into(),
            ],
        ];
        assert!(!looks_like_measurement("Document Toxibase n ° 101621"));
        assert!(
            !is_tabular_rows(&rows),
            "a running bibliography was accepted as a table: {rows:?}"
        );
    }