// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Table validation heuristics: TOC dot leader rejection, bullet markers, stopword flow, and data tokens.

/// Detects whether cell strings contain data tokens (numbers, currency, flight codes, dates, times)
/// rather than generic column headers.
pub fn has_data_tokens(cells: &[String]) -> bool {
    cells.iter().any(|c| {
        let s = c.trim();
        if s.is_empty() {
            return false;
        }
        if s.contains('€') || s.contains('$') || s.contains('%') || s.contains("EUR") || s.contains("USD") {
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
        if mean <= 2.2 {
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
    if total_words_count >= 12 && stopword_count * 100 / total_words_count >= 20 {
        return false;
    }

    // A 2-row candidate cannot hold verbose prose sentences in any cell (>= 5 words).
    if data.len() == 2 && data.iter().any(|r| r.iter().any(|c| c.replace("<br>", " ").split_whitespace().count() >= 5)) {
        return false;
    }

    // Multi-row grids (>= 3 rows) without any data token (numbers, codes, dates, currency)
    // are flowing prose paragraphs unless all columns are very short (<= 2 words/cell).
    if data.len() >= 3 && !data.iter().any(|r| has_data_tokens(r)) {
        let max_col_mean = (0..cols)
            .map(|k| {
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
                    0.0
                } else {
                    let tok: usize = cells.iter().map(|c| c.split_whitespace().count()).sum();
                    tok as f64 / cells.len() as f64
                }
            })
            .fold(0.0f64, f64::max);
        if max_col_mean > 2.0 {
            return false;
        }
    }

    let overall = total_tokens as f64 / total_cells as f64;
    overall < 6.0
}
