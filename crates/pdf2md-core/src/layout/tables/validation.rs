// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Table validation heuristics: TOC dot leader rejection, bullet markers, stopword flow, and data tokens.

/// A cell or token that marks a table's data even though it carries no digit:
/// a voting tick, a round marker, or a short status word. Checklist / voting
/// matrices (`x` cells) and survey grids (`oui`/`non`) have no numbers at all,
/// so `has_data_tokens` used to call them header/prose and the grid was dropped
/// to loose text.
pub(crate) fn is_marker_token(tok: &str) -> bool {
    let t = tok.trim();
    if t.is_empty() {
        return false;
    }
    if matches!(
        t,
        "x" | "X"
            | "o"
            | "O"
            | "✓"
            | "✗"
            | "✘"
            | "×"
            | "✩"
            | "★"
            | "●"
            | "○"
            | "◀"
            | "▶"
            | "♦"
            | "√"
            | "₹"
            | "â̜"
    ) {
        return true;
    }
    matches!(t.to_ascii_lowercase().as_str(), "oui" | "non" | "yes" | "ok")
}

/// Whether a cell carries a marker/validation glyph anywhere — e.g. the
/// checklist cell `"✓ : validation rules"`.
fn contains_marker_glyph(s: &str) -> bool {
    s.contains("â̜")
        || s.chars().any(|ch| {
            matches!(ch, '✓' | '✗' | '✘' | '×' | '✩' | '★' | '●' | '○' | '◀' | '▶' | '♦' | '√' | '₹')
        })
}

/// Whether a cell renders a measurement / numeric range with a unit, e.g.
/// `"2 ÷ 10 bar"`, `"16 ÷ 100"`, `"- 20°C ÷ 80°C"`, `"230 V"`. Such cells are
/// data columns even though they hold three or four whitespace tokens, so
/// `is_tabular_rows`'s short-column test must accept them.
fn looks_like_measurement(c: &str) -> bool {
    if !c.chars().any(|ch| ch.is_ascii_digit()) {
        return false;
    }
    // Range/comparison glyphs are a strong measurement signal whenever the
    // cell also holds a digit.
    if c.chars().any(|ch| matches!(ch, '÷' | '±' | '×' | '≤' | '≥' | '~')) {
        return true;
    }
    // A degree sign is a measurement only when it is attached to a digit
    // ("35°", "20°C"). French prose abbreviates *numéro* as "n°": a producer
    // that draws it as "n ° 101621" gives a standalone degree glyph far from
    // any digit, which must not flag the bibliography cell as a measurement.
    if c.char_indices().any(|(i, ch)| {
        ch == '°'
            && (c[..i].chars().last().map_or(false, |b| b.is_ascii_digit())
                || c[i + ch.len_utf8()..]
                    .chars()
                    .next()
                    .map_or(false, |a| a.is_ascii_digit()))
    }) {
        return true;
    }
    const UNITS: &[&str] = &[
        "bar", "kpa", "mpa", "pa", "mbar", "rpm", "hz", "khz", "mhz", "ghz", "kw", "kva",
        "m³", "cm³", "mm²", "cm²", "m²", "µm", "μm", "kg", "mg", "ml", "cl", "da", "dan",
    ];
    // Multi-character technical units may appear as standalone whitespace
    // tokens ("2 ÷ 10 bar", "3000 rpm").
    if c.split_whitespace().any(|t| {
        let tl = t
            .trim_matches(|ch: char| !ch.is_alphanumeric() && ch != '°' && ch != '/')
            .to_lowercase();
        UNITS.contains(&tl.as_str())
    }) {
        return true;
    }
    // Single-character units (v/w/a/m/l/g/s) must never match an isolated
    // lowercase prose word ("a", "l", "m" are ordinary French words). Accept
    // them only when they are attached directly to the digit ("230V", "10m",
    // "5A") or are an uppercase V/W/A directly following a number ("230 V").
    let attached = c
        .to_lowercase()
        .chars()
        .collect::<Vec<char>>()
        .windows(2)
        .any(|w| w[0].is_ascii_digit() && matches!(w[1], 'v' | 'w' | 'a' | 'm' | 'l' | 'g' | 's'));
    if attached {
        return true;
    }
    c.split_whitespace().collect::<Vec<&str>>().windows(2).any(|w| {
        matches!(w[1], "V" | "W" | "A")
            && w[0]
                .chars()
                .last()
                .map(|ch| ch.is_ascii_digit())
                .unwrap_or(false)
    })
}

/// Detects whether cell strings contain data tokens (numbers, currency, flight codes, dates, times)
/// rather than generic column headers.
pub fn has_data_tokens(cells: &[String]) -> bool {
    cells.iter().any(|c| {
        let s = c.trim();
        if s.is_empty() {
            return false;
        }
        if s.contains('€') || s.contains('$') || s.contains('%') || s.contains("EUR") || s.contains("USD") || s.contains('₹') {
            return true;
        }
        // A marker/validation cell: a lone tick/round token, or a cell whose
        // every token is one ("✓ : validation rules" is handled by the glyph
        // test, "x x x" by the all-tokens test below).
        if is_marker_token(s) || contains_marker_glyph(s) {
            return true;
        }
        if s.split_whitespace().all(|t| is_marker_token(t)) {
            return true;
        }
        for tok in s.split_whitespace() {
            let t = tok.trim_matches(|ch: char| !ch.is_alphanumeric());
            if t.is_empty() {
                continue;
            }
            // Time format e.g. "10:05", "09:35"
            if t.contains(':') && t.chars().any(|ch| ch.is_ascii_digit()) {
                return true;
            }
            // Pure number or number with decimal/comma/percent: "44.4%", "12,50", "4096", "32"
            let stripped = t.replace([',', '.', '-', '+', '%'], "");
            if !stripped.is_empty() && stripped.chars().all(|ch| ch.is_ascii_digit()) {
                return true;
            }
            // Flight code / alphanum date e.g. "AA100", "28MAR"
            if t.len() >= 4
                && t.chars().any(|ch| ch.is_ascii_digit())
                && t.chars().any(|ch| ch.is_ascii_alphabetic())
            {
                return true;
            }
        }
        false
    })
}

/// True when a candidate grid is a genuine data table rather than one of the
/// alignment look-alikes. Cells are supplied per row (empty strings allowed).
///
/// Rejections:
///   * any column that is a dot-leader column (>= half of its cells are
///     leader runs like "......") — TOC / index rows;
///   * a constant short first column across rows (bullet / radio markers);
///   * grids where no column is short (<= ~1.6 words/cell on average) and
///     where the overall cell verbosity looks like prose (>= 6 words/cell).
pub fn is_tabular_rows(rows: &[Vec<String>]) -> bool {
    let is_leader = |c: &str| -> bool {
        let t = c.trim();
        if t.is_empty() {
            return false;
        }
        let n = t.chars().count();
        let filler = t
            .chars()
            .filter(|ch| matches!(ch, '.' | '·' | '•' | '_' | ' '))
            .count();
        (t.starts_with('.') || t.starts_with('·') || t.starts_with('•')) && filler * 2 >= n
    };

    let data: Vec<&Vec<String>> = rows
        .iter()
        .filter(|r| r.iter().any(|c| !c.trim().is_empty()))
        .collect();
    if data.len() < 2 {
        return false;
    }
    let cols = data.iter().map(|r| r.len()).max().unwrap_or(0);
    if cols < 2 {
        return false;
    }
    let non_empty: Vec<usize> = (0..cols)
        .map(|k| {
            data.iter()
                .filter(|r| r.get(k).map_or(false, |c| !c.trim().is_empty()))
                .count()
        })
        .collect();
    let eff: Vec<usize> = (0..cols).filter(|&k| non_empty[k] > 0).collect();
    if eff.len() < 2 {
        return false;
    }

    // Dot-leader column (TOC dotted rows).
    for &k in &eff {
        let cells: Vec<&str> = data
            .iter()
            .filter_map(|r| {
                let c = r.get(k).map_or("", |c| c.as_str()).trim();
                if c.is_empty() {
                    None
                } else {
                    Some(c)
                }
            })
            .collect();
        if cells.is_empty() {
            continue;
        }
        let leaders = cells.iter().filter(|c| is_leader(c)).count();
        if leaders * 2 >= cells.len() {
            return false;
        }
    }

    // Constant short first column or bullet list (markers like "•", "-", "*", "o").
    if eff[0] == 0 {
        let first: Vec<&str> = data
            .iter()
            .map(|r| r[0].trim())
            .filter(|c| !c.is_empty())
            .collect();
        if !first.is_empty()
            && first.iter().all(|c| *c == first[0])
            && first[0].chars().count() <= 2
        {
            return false;
        }
        if !first.is_empty()
            && first.iter().all(|c| c.starts_with('•') || c.starts_with('-') || c.starts_with('*') || c.starts_with('·'))
        {
            return false;
        }
    }

    // A data grid must contain at least one short column (codes, amounts,
    // dates, labels); grids whose every cell is long prose are aligned body
    // text, not tables.
    let mut short_col = false;
    let mut total_tokens = 0usize;
    let mut total_cells = 0usize;
    for &k in &eff {
        let cells: Vec<&str> = data
            .iter()
            .filter_map(|r| {
                let c = r.get(k).map_or("", |c| c.as_str()).trim();
                if c.is_empty() {
                    None
                } else {
                    Some(c)
                }
            })
            .collect();
        if cells.is_empty() {
            continue;
        }
        let tokens: usize = cells.iter().map(|c| c.split_whitespace().count()).sum();
        total_tokens += tokens;
        total_cells += cells.len();
        let mean = tokens as f64 / cells.len() as f64;
        // Technical spec cells carry a value, its range and a unit in one cell
        // ("2 ÷ 10 bar", "- 20°C ÷ 80°C"): three or four whitespace tokens, so
        // the old 2.2-token threshold found no short column and rejected the
        // whole grid as prose. Any column whose cells are measurement-like (a
        // digit plus a unit/range glyph or unit word) is a data column, not
        // prose, even when a few tokens long.
        //
        // A sparse bilingual key-value grid (a French header row, its English
        // twin, then one or two data rows) inflates every column's mean the
        // same way: the label column holds short cells ("Nom" / "Name") beside
        // a 4-token full-name value and the 3-token "(Adulte / Adult)"
        // qualifier, averaging 2.25 tokens/cell — just past the old 2.2 bar,
        // even though no cell is a sentence. Genuine prose/URL columns sit at
        // 2.5 and above (see
        // `french_bibliography_with_numero_degree_is_not_tabular`, whose URL
        // column is exactly 2.5, and
        // `stopword_dense_grid_without_numeric_column_is_rejected`, whose
        // shortest column is 2.75), so 2.4 admits the bilingual grid while
        // still rejecting those.
        if mean <= 2.4 || (mean <= 4.5 && cells.iter().any(|c| looks_like_measurement(c))) {
            short_col = true;
        }
    }
    if !short_col {
        return false;
    }
    // A wide grid (>= 4 columns) where > 65% of cells are empty is typical of
    // accidental alignments across wrapped sentences.
    if cols >= 4 {
        let total_possible = data.len() * cols;
        let filled_count: usize = data
            .iter()
            .map(|r| r.iter().filter(|c| !c.trim().is_empty()).count())
            .sum();
        let density = filled_count as f64 / total_possible as f64;
        if density < 0.35 {
            return false;
        }
    }

    // Prose stopword detector: flowing sentences contain a high percentage of
    // grammatical function words (articles, prepositions, conjunctions, pronouns)
    // which rarely appear in tabular data cells.
    const STOPWORDS: &[&str] = &[
        "the", "of", "and", "to", "in", "a", "is", "that", "for", "with", "as", "by", "on",
        "from", "we", "our", "are", "it", "an", "at", "be", "this", "which", "or", "can",
        "all", "also", "was", "were", "been", "have", "has", "had", "not", "but", "they",
        "their", "under", "both", "more", "into", "than",
        "le", "la", "les", "de", "du", "des", "et", "en", "dans", "pour", "sur", "une", "un",
        "par", "est", "sont", "avec", "qui", "que",
    ];
    let mut stopword_count = 0usize;
    let mut total_words_count = 0usize;
    for r in &data {
        for c in r.iter() {
            let replaced = c.replace("<br>", " ");
            let tokens: Vec<&str> = replaced.split_whitespace().collect();
            for (i, word) in tokens.iter().enumerate() {
                let clean = word.trim_matches(|ch: char| !ch.is_alphabetic()).to_lowercase();
                if !clean.is_empty() {
                    total_words_count += 1;
                    if STOPWORDS.contains(&clean.as_str()) {
                        // A function word immediately adjacent to a numeric/date
                        // token is part of a structured label (date-range span
                        // "du 17/05/25 au ...", amount "de 19/05/2026"), not
                        // prose. Excluding it keeps genuine bill grids whose
                        // description cells carry date spans from being
                        // misclassified as flowing paragraphs.
                        let numtok = |t: &&str| {
                            t.trim_matches(|ch: char| !ch.is_ascii_alphanumeric())
                                .chars()
                                .any(|ch| ch.is_ascii_digit())
                        };
                        let next_num = tokens.get(i + 1).map_or(false, numtok);
                        let prev_num = i
                            .checked_sub(1)
                            .and_then(|j| tokens.get(j))
                            .map_or(false, numtok);
                        if !next_num && !prev_num {
                            stopword_count += 1;
                        }
                    }
                }
            }
        }
    }
    // A column of bare numeric/currency amounts is a structural table signal
    // no prose paragraph has. French account/amount grids carry stopword-dense
    // labels ("Virement de la section …", "Dotations fonds divers réserve"),
    // so the prose detector above discards genuine tables. When a column holds
    // >= 2 values and is at least 3/4 numeric, the stopword share is not
    // evidence of flowing prose.
    let is_amount_cell = |c: &str| -> bool {
        let t = c
            .trim()
            .trim_matches(|ch: char| matches!(ch, '€' | '$' | '£' | ' ' | '\u{00a0}'));
        if t.is_empty() {
            return false;
        }
        // Section / outline numbers like "4.2." or "1." are not amounts.
        if t.ends_with('.') || t.ends_with(':') || t.ends_with(';') {
            return false;
        }
        let dot_count = t.chars().filter(|&ch| ch == '.').count();
        if dot_count > 1 {
            let parts: Vec<&str> = t.split('.').collect();
            let is_thousands = parts.iter().skip(1).all(|p| p.len() == 3 && p.chars().all(|ch| ch.is_ascii_digit()));
            if !is_thousands {
                return false;
            }
        }
        let mut digit = false;
        for ch in t.chars() {
            if ch.is_ascii_digit() {
                digit = true;
            } else if !matches!(ch, '.' | ',' | '-' | '+' | '%' | '/' | '\'' | ' ' | '\u{00a0}') {
                return false;
            }
        }
        digit
    };
    let numeric_col = (0..cols).any(|k| {
        let cells: Vec<&str> = data
            .iter()
            .filter_map(|r| {
                let c = r.get(k).map_or("", |c| c.as_str()).trim();
                if c.is_empty() {
                    None
                } else {
                    Some(c)
                }
            })
            .collect();
        cells.len() >= 2 && cells.iter().filter(|c| is_amount_cell(c)).count() * 4 >= cells.len() * 3
    });
    // A column of measurement/range cells ("2 ÷ 10 bar", "- 20°C ÷ 80°C") is
    // the same structural table signal as an amount column: a prose paragraph
    // has no such column, so the stopword share of a technical specification's
    // descriptive labels must not veto the grid.
    let measurement_col = (0..cols).any(|k| {
        let cells: Vec<&str> = data
            .iter()
            .filter_map(|r| {
                let c = r.get(k).map_or("", |c| c.as_str()).trim();
                if c.is_empty() {
                    None
                } else {
                    Some(c)
                }
            })
            .collect();
        cells.len() >= 2
            && cells.iter().filter(|c| looks_like_measurement(c)).count() * 2 >= cells.len()
    });
    if !numeric_col
        && !measurement_col
        && total_words_count >= 12
        && stopword_count * 100 / total_words_count >= 20
    {
        return false;
    }

    // A 2-row candidate cannot hold verbose prose sentences in any cell (>= 5 words).
    if data.len() == 2 && data.iter().any(|r| r.iter().any(|c| c.replace("<br>", " ").split_whitespace().count() >= 5)) {
        return false;
    }

    // Multi-row grids (>= 3 rows) without any data token (numbers, codes, dates,
    // currency, markers) are flowing prose paragraphs unless they carry at least
    // one genuine data column. A descriptive leading column (a full name or
    // label, mean > 2 words) no longer condemns the whole grid: a trailing
    // column of marker tokens ("x"/"oui") or short values is a structural table
    // signal no prose paragraph has. Only a grid whose every column is verbose
    // prose is rejected.
    if data.len() >= 3 && !data.iter().any(|r| has_data_tokens(r)) {
        let mut has_short_data_col = false;
        let mut max_col_mean = 0.0f64;
        for &k in &eff {
            let cells: Vec<&str> = data
                .iter()
                .filter_map(|r| {
                    let c = r.get(k).map_or("", |c| c.as_str()).trim();
                    if c.is_empty() {
                        None
                    } else {
                        Some(c)
                    }
                })
                .collect();
            if cells.is_empty() {
                continue;
            }
            let tok: usize = cells.iter().map(|c| c.split_whitespace().count()).sum();
            let mean = tok as f64 / cells.len() as f64;
            max_col_mean = max_col_mean.max(mean);
            let marker_col = cells.iter().filter(|c| is_marker_token(c)).count() * 2 >= cells.len();
            let numeric_col = cells.iter().filter(|c| is_amount_cell(c)).count() * 2 >= cells.len();
            if k > 0 && (mean <= 2.0 || marker_col || numeric_col) {
                has_short_data_col = true;
            }
        }
        if !has_short_data_col && max_col_mean > 2.0 {
            return false;
        }
    }

    let overall = total_tokens as f64 / total_cells as f64;
    overall < 6.0
}

#[cfg(test)]
mod tests {
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
}
