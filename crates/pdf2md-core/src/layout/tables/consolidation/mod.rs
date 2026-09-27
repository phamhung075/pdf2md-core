// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Table cell bucketing, multi-line row consolidation, and complementary column merging.

use crate::layout::glyph_stream::Span;
use crate::layout::reading_order::is_monetary_amount;
use crate::layout::tables::rulers::{RowInfo, WordTok};
use crate::layout::tables::validation::has_data_tokens;
use crate::models::{CanvasTable, CELL_LINE_BREAK, CELL_LINE_BREAK_PENDING};

/// Join one matrix cell's same-line `words` into its text.
///
/// Adjacent words are joined with a single space exactly as before, except
/// when *both* are complete monetary amounts and the x-gap between them
/// reaches the table's `min_gutter`: those are two visual columns that the
/// ruler pass filed into one cell (a single sparse ruler), so an in-cell line
/// break is inserted and the two amounts read as separate lines of the cell.
///
/// The break is inserted in its deferred form, [`CELL_LINE_BREAK_PENDING`],
/// which every structural pass already reads as the whitespace it replaces; the
/// Markdown renderer swaps it for [`CELL_LINE_BREAK`] at emission. This keeps
/// the join purely a render-time change. Every other pair — including a
/// space-grouped amount split into several tokens by a sub-`min_gutter`
/// thousands gap — keeps today's space.
fn join_cell_words(words: &[&WordTok], min_gutter: f64) -> String {
    let mut out = String::new();
    let mut prev: Option<&WordTok> = None;
    for w in words {
        if let Some(p) = prev {
            let separate_columns =
                is_monetary_amount(&p.text) && is_monetary_amount(&w.text) && w.x0 - p.x1 >= min_gutter;
            if separate_columns {
                out.push(CELL_LINE_BREAK_PENDING);
            } else {
                out.push(' ');
            }
        }
        out.push_str(&w.text);
        prev = Some(w);
    }
    out
}

/// Assign every word of row `ri` to a column, using the ruler midpoints.
///
/// A word whose start lies within an ordinary word space of the previous
/// word's end belongs to the *same cell*, so it inherits that word's column
/// instead of being tested against the midpoint on its own. PDF producers
/// routinely draw one multi-word cell as several runs whose starts straddle
/// the midpoint boundary — the model-name-plus-version cell "WizardLM 13B
/// v1.2", the benchmark annotation "6.84 ± 0.07", the header "MT-Bench" —
/// and the pure per-word midpoint test then cuts that cell across two
/// columns. A genuine column boundary is always at least a column gutter
/// wide (`table_rulers_opts`'s `min_gutter`, ~1.1em), far wider than the
/// `0.65em` word space tested here, so an intra-cell word space can only
/// ever join two words of the same cell; it can never fuse two real columns.
///
/// This is deliberately used only for the *emitted* cells
/// (`bucket_rows_content_aware`). The `flowing`/`multi_col` validation passes
/// keep the plain per-word midpoint `bucket`/`bucket_words`, because
/// collapsing a dense grid's tight columns there would disarm the `flowing`
/// prose veto and annex whole pages of prose into a spurious table.
fn assign_columns(info: &[RowInfo], ri: usize, rulers: &[f64]) -> Vec<usize> {
    let ncol = rulers.len();
    let bounds: Vec<f64> = rulers.windows(2).map(|p| (p[0] + p[1]) / 2.0).collect();
    let words = &info[ri].words;
    let size = info[ri].size.max(0.1);
    let mut out = Vec::with_capacity(words.len());
    for (wi, w) in words.iter().enumerate() {
        let mut col = bounds.iter().position(|&b| w.x0 < b).unwrap_or(ncol - 1);
        if wi > 0 && w.x0 - words[wi - 1].x1 < 0.65 * size {
            col = out[wi - 1];
        }
        out.push(col);
    }
    out
}

/// Bucket words of row `ri` into columns separated by the ruler midpoints (as WordTok references).
pub fn bucket_words<'a>(info: &'a [RowInfo], ri: usize, rulers: &[f64]) -> Vec<Vec<&'a WordTok>> {
    let ncol = rulers.len();
    let bounds: Vec<f64> = rulers.windows(2).map(|p| (p[0] + p[1]) / 2.0).collect();
    let mut cells: Vec<Vec<&'a WordTok>> = vec![Vec::new(); ncol];
    for w in &info[ri].words {
        let col = bounds.iter().position(|&b| w.x0 < b).unwrap_or(ncol - 1);
        cells[col].push(w);
    }
    cells
}

/// Bucket words of row `ri` into columns separated by the ruler midpoints.
pub fn bucket(info: &[RowInfo], ri: usize, rulers: &[f64]) -> Vec<String> {
    let ncol = rulers.len();
    let bounds: Vec<f64> = rulers.windows(2).map(|p| (p[0] + p[1]) / 2.0).collect();
    let mut cells: Vec<Vec<String>> = vec![Vec::new(); ncol];
    for w in &info[ri].words {
        let col = bounds.iter().position(|&b| w.x0 < b).unwrap_or(ncol - 1);
        cells[col].push(w.text.clone());
    }
    cells.into_iter().map(|c| c.join(" ")).collect()
}

/// Bucket every row of a window with [`bucket`], then pull back words that the
/// pure midpoint rule stranded in the following column.
///
/// `bucket` assigns a word to whichever column's ruler midpoint contains the
/// word's *start*. That is right for the two ordinary shapes — a left-aligned
/// cell beginning at its column ruler, and a right-aligned numeric value whose
/// (varying) start still falls inside its own column — but it mis-files the
/// tail of the *previous* column's multi-word label when that tail happens to
/// start beyond the midpoint. In the totals block of the ZUGFeRD "EN16931
/// Rabatte" invoice, "Steuerbetrag in EUR" renders with "EUR" at x=454, past
/// the midpoint (398) between the label ruler (288) and the amount ruler (508),
/// so the amount cell came out as "EUR 21,30" instead of "Steuerbetrag in EUR"
/// + "21,30" (the vision transcription and the raw span geometry both read the
/// unit as part of the label).
///
/// A word is rescued only when every one of these holds:
///   * it is the leading token of a *value* cell whose remaining tokens are all
///     numeric — the "<unit> <amount>" shape ("EUR 21,30"). This keeps the
///     rescue from re-filing header words or wrapped multi-word cells in
///     unrelated grids;
///   * it starts at least a full alignment tolerance before its own column's
///     start ruler, so a value merely jittering around the ruler is untouched;
///   * it begins to the right of the previous column's content edge and is
///     geometrically closer to that edge than to this column's ruler, so a long
///     value whose start dips left of the ruler is not moved.
///
/// `min_gutter` is the window's own minimum column gutter (`min_gutter_for` of
/// its largest font), the same value the ruler pass used. It is threaded into
/// [`join_cell_words`] so two complete monetary amounts separated by at least
/// that gutter — i.e. two real columns the sparse ruler filed into one cell —
/// are emitted on separate in-cell lines.
pub fn bucket_rows_content_aware(
    info: &[RowInfo],
    win_rows: &[usize],
    rulers: &[f64],
    tol: f64,
    min_gutter: f64,
) -> Vec<Vec<String>> {
    let ncol = rulers.len();
    let bounds: Vec<f64> = rulers.windows(2).map(|p| (p[0] + p[1]) / 2.0).collect();
    let assign = |w: &WordTok| bounds.iter().position(|&b| w.x0 < b).unwrap_or(ncol - 1);
    let has_digit = |t: &str| t.chars().any(|c| c.is_ascii_digit());

    // Pass 1: the right edge of the content each column actually holds, using
    // the ordinary midpoint assignment. This is what a rescued word is compared
    // against — per-row extents are not enough, because the label that "EUR"
    // continues ("…Zuschläge") reaches farther right on a different row.
    let mut extents = vec![f64::NEG_INFINITY; ncol];
    for &ri in win_rows {
        for w in &info[ri].words {
            let col = assign(w);
            extents[col] = extents[col].max(w.x1);
        }
    }

    win_rows
        .iter()
        .map(|&ri| {
            let words = &info[ri].words;
            // Same word-space-aware assignment `bucket`/`bucket_words` use, so
            // the emitted cells agree with the cells the validation passes saw.
            let assigned: Vec<usize> = assign_columns(info, ri, rulers);
            let is_leading_unit = |wi: usize| {
                let col = assigned[wi];
                if has_digit(&words[wi].text) || assigned[..wi].contains(&col) {
                    return false;
                }
                let mut have_number = false;
                for (j, &c) in assigned.iter().enumerate() {
                    if j == wi || c != col {
                        continue;
                    }
                    if !has_digit(&words[j].text) {
                        return false;
                    }
                    have_number = true;
                }
                have_number
            };

            let mut cells: Vec<Vec<&WordTok>> = vec![Vec::new(); ncol];
            for (wi, w) in words.iter().enumerate() {
                let mut col = assigned[wi];
                while col > 0 && is_leading_unit(wi) {
                    let ruler = rulers[col];
                    if w.x0 >= ruler - tol {
                        break;
                    }
                    let left_extent = extents[..col]
                        .iter()
                        .cloned()
                        .fold(f64::NEG_INFINITY, f64::max);
                    if left_extent.is_finite()
                        && w.x0 > left_extent
                        && (w.x0 - left_extent) < (ruler - w.x0)
                    {
                        col -= 1;
                    } else {
                        break;
                    }
                }
                cells[col].push(w);
            }
            cells
                .into_iter()
                .map(|c| join_cell_words(&c, min_gutter))
                .collect()
        })
        .collect()
}

/// Merges complementary adjacent columns (e.g. where an indented cell value never co-occurs with the column header,
/// the content falls geometrically inside the text span of column c and they never co-occur on the same line).
pub fn merge_complementary_columns(
    mut table_rows: Vec<Vec<String>>,
    win_rows: &[usize],
    info: &[RowInfo],
    rulers: &[f64],
    tol: f64,
) -> (Vec<Vec<String>>, Vec<f64>) {
    if table_rows.is_empty() || table_rows[0].len() < 2 {
        return (table_rows, rulers.to_vec());
    }
    let mut current_rulers = rulers.to_vec();
    let mut c = 0;
    while c + 1 < current_rulers.len() {
        let r0 = current_rulers[c];
        let r1 = current_rulers[c + 1];

        // Find the maximum right edge (x1) of words assigned to column c
        let bounds: Vec<f64> = current_rulers.windows(2).map(|p| (p[0] + p[1]) / 2.0).collect();
        let ncol = current_rulers.len();
        let mut max_x1_c = r0;
        for &ri in win_rows {
            for w in &info[ri].words {
                let col = bounds.iter().position(|&b| w.x0 < b).unwrap_or(ncol - 1);
                if col == c {
                    max_x1_c = max_x1_c.max(w.x1);
                }
            }
        }

        // Two adjacent columns are complementary if they never co-occur on the
        // same visual line AND one of:
        // 1) the columns' geographic extents say they are really one cell:
        //    - if r1 is the *right edge* of column c (an end-derived ruler,
        //      i.e. a word ends exactly there), they merge as soon as the text
        //      reaches r1;
        //    - if r1 is the *start* of the next column (a start-derived ruler),
        //      the text must genuinely extend past r1 by more than `tol`,
        //      otherwise the word merely touches the next column's start and
        //      the columns are distinct. This margin is essential once run
        //      advances are correct, because tightly-packed header cells end
        //      within a fraction of a point of the next ruler.
        // 2) the two rulers are a tight pair (much closer to each other than to
        //    the next ruler) and only one of them has a header cell — absorbs
        //    a header column whose value is indented into a second ruler.
        let never_cooccur = table_rows
            .iter()
            .all(|r| r[c].trim().is_empty() || r[c + 1].trim().is_empty());
        let r1_is_start = win_rows.iter().any(|&ri| {
            info[ri]
                .starts
                .iter()
                .any(|&s| (s - r1).abs() <= tol * 1.5)
        });
        let extends_into = if r1_is_start {
            max_x1_c - r1 > tol
        } else {
            max_x1_c + tol >= r1
        };
        let next_ruler_gap = if c + 2 < current_rulers.len() {
            current_rulers[c + 2] - r1
        } else {
            100.0
        };
        let r_gap = r1 - r0;
        let is_tight_pair = r_gap < next_ruler_gap * 0.75;
        let one_has_no_header = !table_rows.is_empty()
            && (table_rows[0][c].trim().is_empty()
                != table_rows[0][c + 1].trim().is_empty());

        let should_merge =
            never_cooccur && (extends_into || (is_tight_pair && one_has_no_header));

        if should_merge {
            for r in table_rows.iter_mut() {
                if r[c].trim().is_empty() {
                    r[c] = std::mem::take(&mut r[c + 1]);
                }
                r.remove(c + 1);
            }
            current_rulers.remove(c + 1);
            continue;
        }
        c += 1;
    }
    (table_rows, current_rulers)
}

/// Whether a first-column cell is a compact row anchor — a single date/code
/// token such as "28MAR" or "08DEC" — rather than a descriptive label
/// ("Nombre de tests réalisés"). A multi-line record whose first-column anchor
/// sits on its second visual line is joined on this signal; a new logical data
/// row, which carries a fresh and typically multi-word first-column label, is
/// not.
fn is_compact_row_anchor(cell: &str) -> bool {
    let t = cell.trim();
    !t.is_empty()
        && t.split_whitespace().count() == 1
        && t.chars().any(|ch| ch.is_ascii_digit())
}

/// Consolidates multi-line table rows and multi-line headers into single logical rows.
/// Uses the table's own natural line pitch (median baseline difference) to cleanly distinguish
/// intra-row continuation lines from inter-row paragraph / row margins.
pub fn consolidate_table_rows(
    table_rows: Vec<Vec<String>>,
    win_rows: &[usize],
    lines: &[Vec<Span>],
    info: &[RowInfo],
) -> Vec<Vec<String>> {
    if table_rows.len() <= 1 {
        return table_rows;
    }
    let num_cols = table_rows[0].len();

    // 1. Calculate natural intra-line pitch from the table's own baseline differences
    let mut intra_gaps: Vec<f64> = Vec::new();
    for w in win_rows.windows(2) {
        let i0 = w[0];
        let i1 = w[1];
        let gap = lines[i0][0].y - lines[i1][0].y;
        let scale = info[i0].size.max(info[i1].size);
        if gap > 0.4 * scale && gap < 2.2 * scale {
            intra_gaps.push(gap);
        }
    }
    intra_gaps.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let natural_pitch = if !intra_gaps.is_empty() {
        intra_gaps[intra_gaps.len() / 2]
    } else {
        let avg_size = win_rows.iter().map(|&i| info[i].size).sum::<f64>() / win_rows.len() as f64;
        1.2 * avg_size
    };

    let row_break_threshold = 1.5 * natural_pitch;

    // 2. Header rows identification: row 0 is header.
    // In wide grids (>= 3 columns), check if row 1 is a bilingual/unit header continuation:
    //
    // Whether row 0 is a header at all uses the same rule the Markdown renderer
    // uses (`first_row_is_header`), so consolidation and rendering agree: a
    // headerless label/value grid (the billet's
    // `DAI HUNG PHAM - 1989 | 28.00 € | …`) keeps its first row as data and can
    // absorb its own continuation lines.
    let row0_is_header = CanvasTable::first_row_is_header(&table_rows);
    let mut header_rows_count = 1usize;
    if row0_is_header && num_cols >= 3 && table_rows.len() > 2 {
        let prev_line = win_rows[0];
        let curr_line = win_rows[1];
        let gap = lines[prev_line][0].y - lines[curr_line][0].y;
        let candidate_row = &table_rows[1];
        let filled = candidate_row.iter().filter(|c| !c.trim().is_empty()).count();
        // A header split across two visual rows often has only one cell in the
        // second row (e.g. "ELO Rating" under a "Model | MT-Bench" first row).
        // Merge it when the two rows are *complementary* — no column carries a
        // cell in both — so the header reads "Model | ELO Rating | MT-Bench"
        // instead of the lone cell becoming the first data row.
        let complementary = filled >= 1
            && (0..num_cols)
                .all(|c| table_rows[0][c].trim().is_empty() || candidate_row[c].trim().is_empty());
        if gap <= row_break_threshold && !has_data_tokens(candidate_row) && (filled >= 2 || complementary) {
            header_rows_count = 2;
        }
    }

    let mut consolidated: Vec<Vec<String>> = Vec::new();
    // The first data row index: past the header when row 0 is one, otherwise 0
    // itself. A headerless first row must stay open (`cur_row`) so its own
    // wrapped continuation lines merge into it instead of becoming blank-keyed
    // rows of their own (the billet's middle column).
    let data_start = if row0_is_header { header_rows_count } else { 0 };
    if header_rows_count > 1 {
        let mut merged_header = table_rows[0].clone();
        for h in 1..header_rows_count {
            for c in 0..num_cols {
                let s_prev = merged_header[c].trim();
                let s_curr = table_rows[h][c].trim();
                if s_prev.is_empty() {
                    merged_header[c] = s_curr.to_string();
                } else if !s_curr.is_empty() && s_curr != s_prev {
                    merged_header[c] = format!("{}{}{}", s_prev, CELL_LINE_BREAK, s_curr);
                }
            }
        }
        consolidated.push(merged_header);
    } else if row0_is_header {
        consolidated.push(table_rows[0].clone());
    }

    // 3. Data rows consolidation
    let mut cur_row: Option<Vec<String>> = if data_start == 0 {
        Some(table_rows[0].clone())
    } else {
        None
    };
    let mut prev_y = if data_start == 0 {
        lines[win_rows[0]][0].y
    } else {
        lines[win_rows[header_rows_count - 1]][0].y
    };

    for r_idx in data_start..table_rows.len() {
        if r_idx == 0 {
            continue; // already seeded as the open row above
        }
        let line_idx = win_rows[r_idx];
        let y = lines[line_idx][0].y;
        let gap = prev_y - y;

        let row_cells = &table_rows[r_idx];
        let filled_count = row_cells.iter().filter(|c| !c.trim().is_empty()).count();
        if filled_count == 0 {
            continue;
        }

        let is_continuation = match &cur_row {
            None => false,
            Some(curr) => {
                if gap > row_break_threshold {
                    false
                } else {
                    let curr_filled = curr.iter().filter(|c| !c.trim().is_empty()).count();
                    let first_col_curr = curr.iter().position(|c| !c.trim().is_empty());
                    let first_col_row = row_cells.iter().position(|c| !c.trim().is_empty());

                    let is_parenthetical = row_cells.iter().any(|c| c.trim().starts_with('('));
                    let curr_has_col0 = !curr[0].trim().is_empty();
                    let row_has_col0 = !row_cells[0].trim().is_empty();

                    if is_parenthetical {
                        true
                    } else if curr_has_col0 && !row_has_col0 {
                        true
                    } else if !curr_has_col0 && row_has_col0 && is_compact_row_anchor(&row_cells[0]) {
                        // A multi-line record may carry its first-column anchor
                        // on the *second* visual line: the itinerary's times
                        // line ("10:05 11:30", no first-column value) sits above
                        // its date/leg line ("28MAR …", which starts column 0).
                        // Joining is only safe when that fresh first cell is a
                        // compact date/code, not a descriptive label — a new
                        // logical data row ("Nombre de tests réalisés") must
                        // never be folded into the sparse anchorless line above
                        // it (C2 `036c761a`).
                        true
                    } else if first_col_curr == first_col_row && filled_count >= 2 && filled_count >= curr_filled {
                        false
                    } else {
                        // A continuation row carries no fresh first-column value
                        // of its own; a row that starts its own first cell is a
                        // new logical row even when it fills fewer columns than
                        // the row above — a sparse value column (e.g. the
                        // "± 0.07" of only one benchmark row) must not fold the
                        // whole grid into a single `<br>`-joined row.
                        !row_has_col0 && filled_count < curr_filled
                    }
                }
            }
        };

        if is_continuation {
            let curr = cur_row.as_mut().unwrap();
            for c in 0..num_cols {
                let s_prev = curr[c].trim();
                let s_curr = row_cells[c].trim();
                if s_prev.is_empty() {
                    curr[c] = s_curr.to_string();
                } else if !s_curr.is_empty() {
                    if s_prev.ends_with('/') || s_prev.ends_with('-') {
                        curr[c] = format!("{} {}", s_prev, s_curr);
                    } else {
                        curr[c] = format!("{}{}{}", s_prev, CELL_LINE_BREAK, s_curr);
                    }
                }
            }
        } else {
            if let Some(finished) = cur_row.take() {
                consolidated.push(finished);
            }
            cur_row = Some(row_cells.clone());
        }

        prev_y = y;
    }

    if let Some(finished) = cur_row {
        consolidated.push(finished);
    }

    consolidated
}

#[cfg(test)]
mod tests;
