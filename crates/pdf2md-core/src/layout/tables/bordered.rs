// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Vector intersection graph for bordered tables (with merged cell / colspan support).

use crate::cpdf_textpage::{Rect, TextLine, TextWord};
use crate::layout::tables::borderless::{extract_borderless_tables, format_cell_words, is_numeric_cell};
use crate::layout::xy_cut::{LineSegment, StructuredTable, TableCell};
use crate::models::{CanvasTable, ColumnAlignment};

/// Production-grade table extraction engine with dual recovery:
/// - Engine A: Vector intersection graph for bordered tables (with merged cell / colspan support).
/// - Engine B: Vertical whitespace projection valleys & multi-token alignment for borderless canvas tables.
pub fn extract_tables(
    lines: &[TextLine],
    vector_lines: &[LineSegment],
) -> Vec<CanvasTable> {
    let mut all_tables = Vec::new();

    // 1. Engine A: Bordered tables via vector segment intersections
    let (bordered_tables, consumed_line_indices) = extract_bordered_tables(lines, vector_lines);
    all_tables.extend(bordered_tables);

    // 2. Engine B: Borderless canvas tables via whitespace projection valleys & alignment
    let remaining_lines: Vec<TextLine> = lines
        .iter()
        .enumerate()
        .filter(|(idx, _)| !consumed_line_indices.contains(idx))
        .map(|(_, l)| l.clone())
        .collect();

    let borderless_tables = extract_borderless_tables(&remaining_lines);
    all_tables.extend(borderless_tables);

    // Sort all tables in reading order (top-to-bottom: descending top-Y)
    all_tables.sort_by(|a, b| {
        let top_a = a.bbox.y0.max(a.bbox.y1);
        let top_b = b.bbox.y0.max(b.bbox.y1);
        top_b.partial_cmp(&top_a).unwrap_or(std::cmp::Ordering::Equal)
    });

    all_tables
}

/// Unified table recovery taking vector line segments if present.
pub fn recover_tables_with_grid(
    lines: &[TextLine],
    vector_lines: &[LineSegment],
) -> (Vec<CanvasTable>, Vec<TextLine>) {
    let tables = extract_tables(lines, vector_lines);
    if tables.is_empty() {
        return (Vec::new(), lines.to_vec());
    }

    let non_table_lines: Vec<TextLine> = lines
        .iter()
        .filter(|line| {
            let l_bbox = line.line_bbox;
            !tables.iter().any(|t| {
                let t_box = Rect::new(t.bbox.x0, t.bbox.y0, t.bbox.x1, t.bbox.y1);
                t_box.intersects(&l_bbox) || t_box.contains_point(l_bbox.center_x(), l_bbox.center_y())
            })
        })
        .cloned()
        .collect();

    (tables, non_table_lines)
}

/// Borderless table recovery using dynamic whitespace valleys and orthogonal baselines.
pub fn recover_borderless_tables(lines: &[TextLine]) -> (Vec<CanvasTable>, Vec<TextLine>) {
    recover_tables_with_grid(lines, &[])
}

pub fn extract_bordered_tables(
    lines: &[TextLine],
    vector_lines: &[LineSegment],
) -> (Vec<CanvasTable>, Vec<usize>) {
    if vector_lines.is_empty() {
        return (Vec::new(), Vec::new());
    }

    // 1. Separate horizontal and vertical lines
    let mut h_segs: Vec<(f64, f64, f64)> = Vec::new(); // (min_x, max_x, y)
    let mut v_segs: Vec<(f64, f64, f64)> = Vec::new(); // (min_y, max_y, x)

    for seg in vector_lines {
        if seg.is_horizontal(1.5) && (seg.x0 - seg.x1).abs() >= 8.0 {
            h_segs.push((seg.x0.min(seg.x1), seg.x0.max(seg.x1), (seg.y0 + seg.y1) * 0.5));
        } else if seg.is_vertical(1.5) && (seg.y0 - seg.y1).abs() >= 8.0 {
            v_segs.push((seg.y0.min(seg.y1), seg.y0.max(seg.y1), (seg.x0 + seg.x1) * 0.5));
        }
    }

    if h_segs.len() < 2 || v_segs.len() < 2 {
        return (Vec::new(), Vec::new());
    }

    // 2. Merge collinear horizontal segments
    h_segs.sort_by(|a, b| a.2.partial_cmp(&b.2).unwrap_or(std::cmp::Ordering::Equal));
    let mut merged_h: Vec<(f64, f64, f64)> = Vec::new();
    for seg in h_segs {
        if let Some(last) = merged_h.last_mut() {
            if (last.2 - seg.2).abs() <= 2.0 && seg.0 <= last.1 + 3.0 {
                last.1 = last.1.max(seg.1);
                last.2 = (last.2 + seg.2) * 0.5;
                continue;
            }
        }
        merged_h.push(seg);
    }

    // 3. Merge collinear vertical segments
    v_segs.sort_by(|a, b| a.2.partial_cmp(&b.2).unwrap_or(std::cmp::Ordering::Equal));
    let mut merged_v: Vec<(f64, f64, f64)> = Vec::new();
    for seg in v_segs {
        if let Some(last) = merged_v.last_mut() {
            if (last.2 - seg.2).abs() <= 2.0 && seg.0 <= last.1 + 3.0 {
                last.1 = last.1.max(seg.1);
                last.2 = (last.2 + seg.2) * 0.5;
                continue;
            }
        }
        merged_v.push(seg);
    }

    if merged_h.len() < 2 || merged_v.len() < 2 {
        return (Vec::new(), Vec::new());
    }

    // 4. Distinct Y coordinates sorted descending (top to bottom)
    let mut y_coords: Vec<f64> = merged_h.iter().map(|s| s.2).collect();
    y_coords.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
    let mut unique_y: Vec<f64> = Vec::new();
    for y in y_coords {
        if !unique_y.iter().any(|&prev| (prev - y).abs() <= 2.5) {
            unique_y.push(y);
        }
    }

    // Distinct X coordinates sorted ascending (left to right)
    let mut x_coords: Vec<f64> = merged_v.iter().map(|s| s.2).collect();
    x_coords.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mut unique_x: Vec<f64> = Vec::new();
    for x in x_coords {
        if !unique_x.iter().any(|&prev| (prev - x).abs() <= 2.5) {
            unique_x.push(x);
        }
    }

    if unique_y.len() < 2 || unique_x.len() < 2 {
        return (Vec::new(), Vec::new());
    }

    let num_rows = unique_y.len() - 1;
    let num_cols = unique_x.len() - 1;

    let mut table_rows: Vec<Vec<TableCell>> = Vec::with_capacity(num_rows);
    let mut col_alignments = vec![ColumnAlignment::Left; num_cols];

    for r in 0..num_rows {
        let y_top = unique_y[r];
        let y_bottom = unique_y[r + 1];
        let mut row_cells: Vec<TableCell> = Vec::new();
        let mut c = 0;

        while c < num_cols {
            let x_start = unique_x[c];
            let mut span = 1;

            // Check if column boundary at c + span has an internal vertical line
            while c + span < num_cols {
                let check_x = unique_x[c + span];
                let has_v_div = merged_v.iter().any(|v| {
                    (v.2 - check_x).abs() <= 3.0
                        && v.0 <= y_bottom + (y_top - y_bottom) * 0.4
                        && v.1 >= y_top - (y_top - y_bottom) * 0.4
                });

                if has_v_div {
                    break;
                } else {
                    span += 1;
                }
            }

            let x_end = unique_x[c + span];
            let cell_bbox = Rect::new(x_start, y_bottom, x_end, y_top);

            // Collect words falling inside this cell
            let mut cell_words: Vec<&TextWord> = Vec::new();
            for l in lines {
                for w in &l.words {
                    let cx = w.word_bbox.center_x();
                    let cy = w.word_bbox.center_y();
                    if cell_bbox.contains_point(cx, cy)
                        || (cell_bbox.intersects(&w.word_bbox)
                            && w.word_bbox.min_x >= cell_bbox.min_x - 1.0
                            && w.word_bbox.max_x <= cell_bbox.max_x + 1.0)
                    {
                        cell_words.push(w);
                    }
                }
            }

            // Sort words reading order: descending Y, then ascending X
            cell_words.sort_by(|a, b| {
                let y_diff = (b.word_bbox.center_y() - a.word_bbox.center_y()).abs();
                if y_diff > 4.0 {
                    b.word_bbox.center_y().partial_cmp(&a.word_bbox.center_y()).unwrap_or(std::cmp::Ordering::Equal)
                } else {
                    a.word_bbox.min_x.partial_cmp(&b.word_bbox.min_x).unwrap_or(std::cmp::Ordering::Equal)
                }
            });

            let cell_text = format_cell_words(&cell_words);
            let align = if is_numeric_cell(&cell_text) {
                ColumnAlignment::Right
            } else {
                ColumnAlignment::Left
            };
            if align == ColumnAlignment::Right && span == 1 {
                col_alignments[c] = ColumnAlignment::Right;
            }

            row_cells.push(TableCell {
                text: cell_text,
                bbox: cell_bbox,
                colspan: span,
                rowspan: 1,
                alignment: align,
            });

            c += span;
        }

        table_rows.push(row_cells);
    }

    let table_bbox = Rect::new(
        unique_x[0],
        unique_y.last().copied().unwrap_or(0.0),
        unique_x.last().copied().unwrap_or(0.0),
        unique_y[0],
    );

    let structured = StructuredTable {
        rows: table_rows,
        column_alignments: col_alignments,
        bbox: table_bbox,
    };

    let canvas_table = structured.to_canvas_table();

    let mut consumed = Vec::new();
    for (idx, l) in lines.iter().enumerate() {
        if table_bbox.intersects(&l.line_bbox) || table_bbox.contains_point(l.line_bbox.center_x(), l.line_bbox.center_y()) {
            consumed.push(idx);
        }
    }

    (vec![canvas_table], consumed)
}
