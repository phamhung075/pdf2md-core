// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Table cell bucketing, multi-line row consolidation, and complementary column merging.

use crate::layout::glyph_stream::Span;
use crate::layout::tables::rulers::{RowInfo, WordTok};
use crate::layout::tables::validation::has_data_tokens;

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

/// Merges complementary adjacent columns (e.g. where an indented cell value never co-occurs with the column header,
/// the content falls geometrically inside the text span of column c and they never co-occur on the same line).
pub fn merge_complementary_columns(
    mut table_rows: Vec<Vec<String>>,
    win_rows: &[usize],
    info: &[RowInfo],
    rulers: &[f64],
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

        // Two adjacent columns are complementary if:
        // 1) Ruler r1 falls within the text extent of column c (centered or indented content),
        // 2) AND they never co-occur on the same visual line
        let never_cooccur = table_rows.iter().all(|r| {
            r[c].trim().is_empty() || r[c + 1].trim().is_empty()
        });

        let next_ruler_gap = if c + 2 < current_rulers.len() {
            current_rulers[c + 2] - r1
        } else {
            100.0
        };
        let r_gap = r1 - r0;
        let is_tight_pair = r_gap < next_ruler_gap * 0.75;
        let one_has_no_header = !table_rows.is_empty()
            && (table_rows[0][c].trim().is_empty() != table_rows[0][c + 1].trim().is_empty());

        if never_cooccur && (r1 <= max_x1_c || (is_tight_pair && one_has_no_header)) {
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
    let mut header_rows_count = 1usize;
    if num_cols >= 3 && table_rows.len() > 2 {
        let prev_line = win_rows[0];
        let curr_line = win_rows[1];
        let gap = lines[prev_line][0].y - lines[curr_line][0].y;
        let candidate_row = &table_rows[1];
        let filled = candidate_row.iter().filter(|c| !c.trim().is_empty()).count();
        if gap <= row_break_threshold && !has_data_tokens(candidate_row) && filled >= 2 {
            header_rows_count = 2;
        }
    }

    let mut consolidated: Vec<Vec<String>> = Vec::new();
    if header_rows_count > 1 {
        let mut merged_header = table_rows[0].clone();
        for h in 1..header_rows_count {
            for c in 0..num_cols {
                let s_prev = merged_header[c].trim();
                let s_curr = table_rows[h][c].trim();
                if s_prev.is_empty() {
                    merged_header[c] = s_curr.to_string();
                } else if !s_curr.is_empty() && s_curr != s_prev {
                    merged_header[c] = format!("{}<br>{}", s_prev, s_curr);
                }
            }
        }
        consolidated.push(merged_header);
    } else {
        consolidated.push(table_rows[0].clone());
    }

    // 3. Data rows consolidation
    let mut cur_row: Option<Vec<String>> = None;
    let mut prev_y = lines[win_rows[header_rows_count - 1]][0].y;

    for r_idx in header_rows_count..table_rows.len() {
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
                    } else if !curr_has_col0 && row_has_col0 {
                        true
                    } else if first_col_curr == first_col_row && filled_count >= 2 && filled_count >= curr_filled {
                        false
                    } else {
                        filled_count < curr_filled
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
                        curr[c] = format!("{}<br>{}", s_prev, s_curr);
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
