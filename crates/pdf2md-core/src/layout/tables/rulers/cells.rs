// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Stage 3 & Stage 3b ruler scanning, grid alignment, and column corridor analysis.

use super::*;

/// Whether `words` (in left-to-right order) shows any column-like internal
/// spacing of its own — i.e. at least one gap between consecutive words is at
/// least `min_gutter`. A row where every word sits within ordinary
/// word-spacing of the next reads as one flowing sentence, not separate
/// cells.
pub(super) fn row_has_internal_gutter(words: &[WordTok], min_gutter: f64) -> bool {
    words.windows(2).any(|p| p[1].x0 - p[0].x1 >= min_gutter)
}

/// Whether the word of `row` ending on `x` is a *separate value cell* rather
/// than part of the row's leading (label) cell. The leading cell is the run of
/// words from the row's start up to the first column-like gutter; a right edge
/// inside that run is just the label column's ragged extent. Only a word at or
/// after the first gutter — a standalone value, or the last token of a
/// multi-word value such as "1 200,00 €" — can testify that `x` is a
/// right-aligned column edge. A row that is one contiguous cell throughout has
/// no separate value cell at all.
pub(super) fn end_ruler_is_separate_cell(row: &RowInfo, x: f64, tol: f64, min_gutter: f64) -> bool {
    let first_gutter = row
        .words
        .windows(2)
        .position(|p| p[1].x0 - p[0].x1 >= min_gutter)
        .map(|i| i + 1)
        .unwrap_or(row.words.len());
    if first_gutter >= row.words.len() {
        return false;
    }
    for (i, w) in row.words.iter().enumerate() {
        if (w.x1 - x).abs() <= tol {
            if i < first_gutter {
                return false;
            }
            // The word must also *end* its cell: either it is the row's last
            // word, or the next word is separated by a column-like gutter.
            // Otherwise this right edge sits between two words of one cell
            // ("20 Unit(s)": the edge of "20" a word space before "Unit(s)"),
            // which must not become a column boundary.
            if i + 1 < row.words.len() && row.words[i + 1].x0 - w.x1 < min_gutter {
                return false;
            }
            return true;
        }
    }
    false
}

/// Whether `text` is a measurement/currency unit token — "Unit(s)", "Liter",
/// "kg", "€", "%" and the like. Used to keep a tight number+unit pair ("20
/// Unit(s)") inside one cell while letting a tight rank+name pair ("1 CAPIN")
/// split into two columns.
pub(super) fn is_unit_like(text: &str) -> bool {
    let t = text.trim().to_lowercase();
    let clean = t.trim_matches(|c: char| !c.is_alphanumeric() && c != '%' && c != '€' && c != '$' && c != '£' && c != '°');
    if clean.is_empty() {
        return false;
    }
    let base = clean.replace("(s)", "").replace('(', "").replace(')', "");
    const KNOWN_UNITS: &[&str] = &[
        "unit", "units", "liter", "liters", "litre", "litres",
        "kg", "g", "mg", "t", "m", "cm", "mm", "km", "l", "ml", "cl", "dl",
        "s", "sec", "min", "h", "hr", "hrs",
        "eur", "usd", "gbp", "chf", "%", "€", "$", "£",
        "pcs", "pce", "pces", "stk", "un", "ex", "ct", "boite", "bte", "colis", "paq", "pkg",
        "bar", "kpa", "mpa", "pa", "mbar", "rpm", "hz", "khz", "mhz", "ghz",
        "kw", "kwh", "w", "v", "a", "kva", "var",
        "m³", "cm³", "mm²", "cm²", "m²", "µm", "μm", "da", "dan",
    ];
    KNOWN_UNITS.contains(&base.as_str()) || KNOWN_UNITS.contains(&clean)
}

/// Whether the word of `row` starting on `x` is a *separate cell* rather than
/// an interior word of a multi-word cell. Only a word at the row's own start
/// (index 0) or one separated from the preceding word by a column-like gutter
/// can testify that `x` is a real column boundary; a word that is merely the
/// second token of one cell ("20 Unit(s)") must not become one.
pub(super) fn start_ruler_is_separate_cell(row: &RowInfo, x: f64, tol: f64, min_gutter: f64) -> bool {
    for (i, w) in row.words.iter().enumerate() {
        if (w.x0 - x).abs() <= tol {
            if i == 0 {
                return true;
            }
            let prev = &row.words[i - 1];
            if w.x0 - prev.x1 >= min_gutter {
                return true;
            }
            // When the previous token carries a digit, the pair is either a
            // number+unit cell ("20 Unit(s)"), which must stay one cell, or a
            // tight rank/id followed by a name ("1 CAPIN"), which is a real
            // column boundary. Only a unit word keeps the pair together; any
            // other token after the digit starts a new column.
            if prev.text.chars().any(|c| c.is_ascii_digit()) {
                return !is_unit_like(&w.text);
            }
            // The token starts only an ordinary word space after the previous
            // one. Treat it as an interior word of the same cell (not a column
            // boundary) only when the two tokens form a *tight* pair — a
            // number+unit cell like "20 Unit(s)" (both tokens start within
            // ~2em). A distant label whose last word merely abuts the next
            // column is left as its own start.
            return w.x0 - prev.x0 >= 2.0 * row.size.max(0.1);
        }
    }
    false
}

/// How a row's words relate to a candidate ruler `x` — see
/// [`classify_straddle`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Straddle {
    /// No word crosses `x`.
    None,
    /// Only isolated single-cell words cross `x`, and each merely *contains*
    /// this one ruler (it does not span a second candidate). The cell is that
    /// column's own content, however wide, so it is not a fragmentation signal.
    Contained,
    /// Only isolated single-cell words cross `x`, but at least one spans two or
    /// more candidate rulers — a merged/spanning cell that does threaten to
    /// fragment the grid.
    Spanning,
    /// At least one crossing word is an interior token of a multi-word cell:
    /// the ruler is an interior word position, not a column boundary.
    Veto,
}

/// Classify whether row `row` straddles ruler `x`.
///
/// A row with no column-like spacing of its own (see `row_has_internal_gutter`)
/// is exempt: it is typically a wrapped continuation line of a multi-line
/// description with no aligned cells of its own, so it cannot testify about
/// where the table's real columns are — letting it veto rulers that every
/// numeric row agrees on would silently drop the whole table to plain text.
///
/// A word that is its own cell (separated from both neighbours by a column
/// gutter: `word_may_define_center`) and merely *encompasses* `x` is that
/// column's content, not a boundary straddle — the wide header "Dossard"
/// drawn over the narrower bib values "263" must not delete the bib ruler.
/// Only when such a cell genuinely spans multiple candidate rulers is it a
/// fragmentation signal.
pub(super) fn classify_straddle(
    row: &RowInfo,
    rulers: &[f64],
    x: f64,
    tol: f64,
    min_gutter: f64,
) -> Straddle {
    if !row_has_internal_gutter(&row.words, min_gutter) {
        return Straddle::None;
    }
    let mut kind = Straddle::None;
    for (i, w) in row.words.iter().enumerate() {
        if !(w.x0 < x - tol && w.x1 > x + tol) {
            continue;
        }
        // A word that is its own cell and centred on `x` is the very evidence
        // for a centre-alignment ruler; it must not veto the ruler it defines.
        let centered = (0.5 * (w.x0 + w.x1) - x).abs() <= tol;
        if centered && word_may_define_center(row, i, min_gutter) {
            continue;
        }
        if word_may_define_center(row, i, min_gutter) {
            let crossed = rulers
                .iter()
                .filter(|&&r| w.x0 < r - tol && w.x1 > r + tol)
                .count();
            kind = kind.max(if crossed >= 2 {
                Straddle::Spanning
            } else {
                Straddle::Contained
            });
            continue;
        }
        return Straddle::Veto;
    }
    kind
}

/// Whether row `row` straddles ruler `x` in a way that should veto `x` as a
/// column boundary. An isolated cell that merely contains the ruler
/// ([`Straddle::Contained`]) does not veto; one spanning several rulers, or an
/// interior word of a multi-word cell, does.
pub(super) fn row_straddles(row: &RowInfo, rulers: &[f64], x: f64, tol: f64, min_gutter: f64) -> bool {
    matches!(
        classify_straddle(row, rulers, x, tol, min_gutter),
        Straddle::Spanning | Straddle::Veto
    )
}

/// Whether the row at `row_idx` is a plausible table header that sits directly
/// above a detected grid and may be annexed to it.
///
/// A wrapped/multi-line table header is often set immediately above the first
/// data row yet contributes none of the `>= 2` shared rulers the seed needs —
/// its cells are multi-word and their left edges fall between the data
/// columns' rulers — so the seed starts at the first data row and the header
/// is emitted as loose text above the table, failing every `top_heading`
/// relation. Annexing it must not promote an unrelated caption or prose line,
/// so the row is accepted only when:
///   * it aligns with at least two of the table's own column rulers (a
///     left-margin title matches only column 0);
///   * no word straddles an interior ruler (a spanning cell would fragment the
///     emitted cells);
///   * it is a short label row, not a sentence: every bucketed cell holds
///     fewer than five words and the row does not exceed a column-proportional
///     word budget (a 10- or 15-column grid necessarily has 10+ header words).
pub(super) fn header_like_row(
    info: &[RowInfo],
    row_idx: usize,
    rulers: &[f64],
    tol: f64,
    min_gutter: f64,
) -> bool {
    if rulers.len() < 2 {
        return false;
    }
    let row = &info[row_idx];
    if row.words.is_empty() {
        return false;
    }
    // NOTE: no `row_has_internal_gutter` gate here. Many real multi-column
    // headers use compact spacing or short column labels whose consecutive
    // words are separated by less than `min_gutter`; the `matched >= 2`
    // test below already guarantees alignment with at least two columns.
    // The previous hard cap of eight words rejected every wide grid's header
    // out of hand (a 10- or 15-column table has at least that many header
    // tokens). Scale the budget with the detected column count instead.
    let max_header_words = (3 * rulers.len()).max(12);
    if row.words.len() > max_header_words {
        return false;
    }
    // Header cells are frequently centred over their column rather than
    // left-aligned on the data's start ruler, so a label such as the Mistral
    // table's `MT Bench` starts ~1.5pt right of the `6.84` value ruler below
    // it. The pass-wide `tol` is derived from the smallest font on the page
    // (often < 1pt), so the exact `tol * 1.5` test rejected the only row that
    // would have kept the header attached, leaving `Guardrails`/`MT Bench` as
    // detached bold body text. A floor of 2pt keeps a genuine two-cell header
    // matching while the no-straddle and short-cell gates below still reject
    // prose and captions.
    let match_tol = (tol * 1.5).max(2.0);
    let matched = rulers
        .iter()
        .filter(|&&r| row_matches_ruler(row, r, match_tol))
        .count();
    if matched < 2 {
        return false;
    }
    // A header row may legitimately carry a wide merged cell that spans
    // several columns ("ClGlt Dossard" over the rank/bib rulers), so a
    // `Spanning` crossing is tolerated here; only an interior word of a
    // multi-word cell (`Veto`) would truly fragment the emitted cells.
    if rulers[1..].iter().any(|&r| row_straddles(row, rulers, r, tol, min_gutter)) {
        return false;
    }
    bucket(info, row_idx, rulers)
        .iter()
        .all(|c| c.split_whitespace().count() < 5)
}

/// Start positions shared by at least two of `rows` (the column-start
/// candidates for a window), used to tell a genuine merged cell from a
/// fragmenting interior word-start.
pub(super) fn supported_starts(info: &[RowInfo], rows: &[usize], tol: f64) -> Vec<f64> {
    let mut pts: Vec<(f64, usize)> = Vec::new();
    for &ri in rows {
        for &s in &info[ri].starts {
            pts.push((s, ri));
        }
    }
    cluster_positions(pts, tol)
}

/// Like `row_straddles`, but tolerating a *wide merged-cell* word on a
/// continuation row. A word much wider than a normal column gutter, which
/// begins at a genuine column position (present in `col_starts`, i.e. shared
/// by at least two rows) and crosses the ruler comfortably inside its extent,
/// is a merged/spanning cell whose content legitimately covers several
/// columns; it must not veto a *start*-derived column boundary, because that
/// would delete real columns. Every other crossing word — a narrow word, a
/// word on a full header/data row, or a word that only grazes the ruler near
/// its start — is still an interior word-start/end and vetoes. This only
/// matters once run advances are measured correctly: with the old 1-em
/// advances every word was ~10pt wide, so a wrapped continuation word never
/// reached the next column and this distinction was invisible.
pub(super) fn row_straddles_wide_ok(
    info: &[RowInfo],
    col_starts: &[f64],
    ri: usize,
    x: f64,
    tol: f64,
    min_gutter: f64,
) -> bool {
    let row = &info[ri];
    if !row_has_internal_gutter(&row.words, min_gutter) {
        return false;
    }
    // Only a continuation row — one carrying fewer words than there are
    // column starts — can plausibly hold a merged cell that overflows several
    // columns. A full header/data row (as many words as columns) that crosses
    // a boundary is a genuine fragmentation signal and keeps vetoing.
    let row_is_continuation = row.words.len() < col_starts.len();
    row.words.iter().enumerate().any(|(i, w)| {
        let crosses = w.x0 < x - tol && w.x1 > x + tol;
        if !crosses {
            return false;
        }
        // See `row_straddles`: a separate cell centred on `x` defines a
        // centre-alignment ruler and does not veto it.
        if (0.5 * (w.x0 + w.x1) - x).abs() <= tol && word_may_define_center(row, i, min_gutter) {
            return false;
        }
        let wide = (w.x1 - w.x0) > 4.0 * min_gutter;
        if !wide {
            return true;
        }
        let begins_at_column = col_starts
            .iter()
            .any(|&s| (s - w.x0).abs() <= tol * 1.5);
        // The crossed ruler must sit comfortably *inside* the word, well clear
        // of both its start and its end. A ruler only a few points from the
        // word's start is an interior near-start (fragmentation); a merged
        // cell that continues a neighbouring column has the inner rulers deep
        // in its extent.
        let well_inside = (x - w.x0) > 2.0 * min_gutter && (w.x1 - x) > 2.0 * min_gutter;
        !(row_is_continuation && begins_at_column && well_inside)
    })
}
