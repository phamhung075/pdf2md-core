// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Borderless canvas tables via spatial density projection and whitespace valleys.

use crate::cpdf_textpage::{Rect, TextLine, TextWord};
use crate::layout::xy_cut::compute_document_statistics;
use crate::models::{BoundingBox, CanvasTable, ColumnAlignment};

pub fn extract_borderless_tables(lines: &[TextLine]) -> Vec<CanvasTable> {
    if lines.len() < 2 {
        return Vec::new();
    }

    let mut sorted_lines = lines.to_vec();
    sorted_lines.sort_by(|a, b| {
        b.line_bbox.max_y.partial_cmp(&a.line_bbox.max_y).unwrap_or(std::cmp::Ordering::Equal)
    });

    let median_fs = compute_document_statistics(&sorted_lines).median_font_size;
    let col_split_gap = (median_fs * 1.2).max(10.0);

    let line_cells: Vec<Vec<(Rect, String)>> = sorted_lines
        .iter()
        .map(|l| line_to_cells(l, col_split_gap))
        .collect();

    let mut detected_tables = Vec::new();
    let mut line_idx = 0;

    while line_idx < sorted_lines.len() {
        if line_cells[line_idx].len() >= 2 {
            let mut run_end = line_idx + 1;
            while run_end < sorted_lines.len() {
                let cur_cells = &line_cells[run_end];
                if cur_cells.is_empty() {
                    break;
                }
                let prev_y = sorted_lines[run_end - 1].line_bbox.min_y;
                let cur_top = sorted_lines[run_end].line_bbox.max_y;
                let v_gap = prev_y - cur_top;
                if v_gap > (median_fs * 2.5).max(24.0) || v_gap < -8.0 {
                    break;
                }

                if cur_cells.len() >= 2 {
                    run_end += 1;
                } else {
                    break;
                }
            }

            if run_end - line_idx >= 2 {
                let slice = &line_cells[line_idx..run_end];
                if let Some(table) = recover_borderless_projection_table(slice) {
                    detected_tables.push(table);
                    line_idx = run_end;
                    continue;
                }
            }
        }
        line_idx += 1;
    }

    detected_tables
}

pub fn recover_borderless_projection_table(slice: &[Vec<(Rect, String)>]) -> Option<CanvasTable> {
    if slice.len() < 2 {
        return None;
    }

    // 1. Collect all cell bounding intervals [min_x, max_x] across all rows
    let mut intervals: Vec<(f64, f64)> = Vec::new();
    for row in slice {
        for (rect, _) in row {
            intervals.push((rect.min_x, rect.max_x));
        }
    }
    if intervals.is_empty() {
        return None;
    }

    intervals.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

    // 2. Merge overlapping / near-touching intervals to detect candidate column bands
    // Valleys with gap >= 8.0 pt define column gutters
    let mut columns: Vec<(f64, f64)> = Vec::new();
    for (start, end) in intervals {
        if let Some(last) = columns.last_mut() {
            if start <= last.1 + 8.0 {
                last.1 = last.1.max(end);
                continue;
            }
        }
        columns.push((start, end));
    }

    if columns.len() < 2 {
        return None;
    }

    let num_cols = columns.len();
    let mut rows_matrix: Vec<Vec<String>> = Vec::with_capacity(slice.len());
    let mut col_left_coords: Vec<Vec<f64>> = vec![Vec::new(); num_cols];
    let mut col_right_coords: Vec<Vec<f64>> = vec![Vec::new(); num_cols];
    let mut col_texts: Vec<Vec<String>> = vec![Vec::new(); num_cols];
    let mut table_bbox = slice[0][0].0;

    for row in slice {
        let mut row_strings = vec![String::new(); num_cols];
        for (rect, text) in row {
            table_bbox = table_bbox.union_with(rect);
            let cx = rect.center_x();

            // Find closest matching column
            let best_col = columns
                .iter()
                .enumerate()
                .min_by(|(_, c1), (_, c2)| {
                    let d1 = if cx < c1.0 { c1.0 - cx } else if cx > c1.1 { cx - c1.1 } else { 0.0 };
                    let d2 = if cx < c2.0 { c2.0 - cx } else if cx > c2.1 { cx - c2.1 } else { 0.0 };
                    d1.partial_cmp(&d2).unwrap_or(std::cmp::Ordering::Equal)
                })
                .map(|(idx, _)| idx)
                .unwrap_or(0);

            let spans_next = best_col + 1 < num_cols && rect.max_x >= columns[best_col + 1].1 - 4.0;

            if row_strings[best_col].is_empty() {
                row_strings[best_col] = text.clone();
            } else {
                row_strings[best_col].push(' ');
                row_strings[best_col].push_str(text);
            }

            col_left_coords[best_col].push(rect.min_x);
            col_right_coords[best_col].push(rect.max_x);
            col_texts[best_col].push(text.clone());

            if spans_next {
                // Span handled by leaving best_col + 1 empty
            }
        }
        rows_matrix.push(row_strings);
    }

    // 3. False-positive validation
    let col0_is_bullets = col_texts[0].iter().all(|t| {
        let s = t.trim();
        s == "-" || s == "*" || s == "•" || s == "+" || (s.ends_with('.') && s.len() <= 3)
    });
    if col0_is_bullets && col_texts[0].len() == slice.len() {
        return None;
    }

    // 4. Alignment Detection
    let mut col_alignments = Vec::with_capacity(num_cols);
    for c in 0..num_cols {
        let left_dev = std_deviation(&col_left_coords[c]);
        let right_dev = std_deviation(&col_right_coords[c]);
        let numeric_count = col_texts[c].iter().filter(|t| is_numeric_cell(t)).count();
        let is_numeric = numeric_count > 0 && numeric_count >= col_texts[c].len() / 2;

        if is_numeric && right_dev <= left_dev + 3.0 {
            col_alignments.push(ColumnAlignment::Right);
        } else if left_dev <= 6.0 {
            col_alignments.push(ColumnAlignment::Left);
        } else {
            col_alignments.push(ColumnAlignment::Center);
        }
    }

    Some(CanvasTable {
        rows: rows_matrix,
        bbox: BoundingBox::new(table_bbox.min_x, table_bbox.min_y, table_bbox.max_x, table_bbox.max_y),
        alignments: Some(col_alignments),
    })
}

pub fn std_deviation(values: &[f64]) -> f64 {
    if values.len() < 2 {
        return 0.0;
    }
    let mean = values.iter().sum::<f64>() / values.len() as f64;
    let sum_sq = values.iter().map(|&v| (v - mean) * (v - mean)).sum::<f64>();
    (sum_sq / (values.len() - 1) as f64).sqrt()
}

pub fn format_cell_words(words: &[&TextWord]) -> String {
    if words.is_empty() {
        return String::new();
    }
    let mut s = String::new();
    for (i, w) in words.iter().enumerate() {
        let t = w.text.trim();
        if t.is_empty() {
            continue;
        }
        if i > 0 {
            let prev = words[i - 1].text.trim();
            let no_space = (prev == "$" || prev == "€" || prev == "£" || prev == "¥" || prev == "VND" || prev == "-")
                && t.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false);
            if !no_space && !s.is_empty() {
                s.push(' ');
            }
        }
        s.push_str(t);
    }
    s
}

pub fn is_numeric_cell(s: &str) -> bool {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return false;
    }
    let stripped = trimmed
        .replace('$', "")
        .replace('€', "")
        .replace('£', "")
        .replace('¥', "")
        .replace("VND", "")
        .replace(',', "")
        .replace('.', "")
        .replace('-', "")
        .replace('%', "")
        .replace('(', "")
        .replace(')', "");
    !stripped.is_empty() && stripped.chars().all(|c| c.is_ascii_digit() || c.is_whitespace())
}

pub fn line_to_cells(line: &TextLine, col_split_gap: f64) -> Vec<(Rect, String)> {
    if line.words.is_empty() {
        if !line.text.is_empty() {
            return vec![(line.line_bbox, line.text.clone())];
        }
        return Vec::new();
    }

    let mut cells = Vec::new();
    let mut current_cell_words: Vec<&TextWord> = vec![&line.words[0]];

    for w in &line.words[1..] {
        let prev = current_cell_words.last().unwrap();
        let gap = w.word_bbox.min_x - prev.word_bbox.max_x;
        if gap >= col_split_gap {
            let mut cell_bbox = current_cell_words[0].word_bbox;
            let cell_text = format_cell_words(&current_cell_words);
            for word in &current_cell_words[1..] {
                cell_bbox = cell_bbox.union_with(&word.word_bbox);
            }
            cells.push((cell_bbox, cell_text));
            current_cell_words = vec![w];
        } else {
            current_cell_words.push(w);
        }
    }

    if !current_cell_words.is_empty() {
        let mut cell_bbox = current_cell_words[0].word_bbox;
        let cell_text = format_cell_words(&current_cell_words);
        for word in &current_cell_words[1..] {
            cell_bbox = cell_bbox.union_with(&word.word_bbox);
        }
        cells.push((cell_bbox, cell_text));
    }

    cells
}
