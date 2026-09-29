// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Frame-to-table construction and the creation/override predicates that read
//! the drawn geometry. See [`super`] for the frame model and its policy.

use super::rows::{build_row, cell_is_value, is_header_like, populated};
use super::super::rulers::{line_words, TableHit};
use super::{
    line_size, t, Frame, EDGE_EPS_PT, HEADER_GAP_SIZE_MULT, MAX_HEADER_LINES, MIN_BODY_ROWS, MIN_FRAME_ROWS,
    MIN_GRID_COLS, MIN_GRID_ROWS, MIN_GRID_TABULAR_ROWS, MIN_TABULAR_ROWS,
};
use crate::layout::glyph_stream::Span;
use crate::models::{BoundingBox, CELL_LINE_BREAK, CELL_LINE_BREAK_PENDING};

/// Build one table from a frame, or `None` when the frame carries no table.
pub(super) fn build_frame_table(lines: &[Vec<Span>], frame: &Frame) -> Option<TableHit> {
    let ncols = frame.ncols();
    let boundaries = &frame.boundaries;

    // Every in-band line carrying an in-frame word.
    let mut data: Vec<(usize, Vec<String>)> = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if line.is_empty() {
            continue;
        }
        let y = line[0].y;
        if !(frame.lo..=frame.hi).contains(&y) {
            continue;
        }
        let row = build_row(&line_words(line), line_size(line), boundaries);
        if row.iter().all(|c| c.trim().is_empty()) {
            continue;
        }
        data.push((i, row));
    }
    if data.is_empty() {
        t(|| "reject: no data rows".to_string());
        return None;
    }

    // The header band is the leading run of label-like lines. It may sit inside
    // the band (a header row below the top rule) or, when the top rule runs
    // under the header, immediately above it; join either into one row so a
    // multi-line header cell is not emitted as headings or as data.
    let mut header: Vec<(usize, Vec<String>)> = Vec::new();
    let mut body_start = 0;
    while body_start < data.len() && header.len() < MAX_HEADER_LINES {
        if !is_header_like(&data[body_start].1) {
            break;
        }
        header.push(data[body_start].clone());
        body_start += 1;
    }
    if header.is_empty() {
        if let Some(&(first, _)) = data.first() {
            let mut prev_y = lines[first][0].y;
            let mut j = first;
            while header.len() < MAX_HEADER_LINES && j > 0 {
                j -= 1;
                let line = &lines[j];
                if line.is_empty() {
                    break;
                }
                let y = line[0].y;
                let size = line.iter().map(|s| s.size).fold(0.0, f64::max).max(1.0);
                let gap = y - prev_y;
                if !(gap > 0.0 && gap <= HEADER_GAP_SIZE_MULT * size) {
                    break;
                }
                let row = build_row(&line_words(line), line_size(line), boundaries);
                if !is_header_like(&row) {
                    break;
                }
                header.push((j, row));
                prev_y = y;
            }
            header.reverse();
        }
    }

    let body: &[(usize, Vec<String>)] = &data[body_start..];
    if body.len() < MIN_BODY_ROWS {
        t(|| "reject: header consumed every row".to_string());
        return None;
    }
    // A real table is at least a header plus a body, or two data rows.
    if header.len() + body.len() < MIN_FRAME_ROWS {
        t(|| "reject: single-row table".to_string());
        return None;
    }
    let tabular = body.iter().filter(|(_, r)| populated(r) >= 2).count();
    if tabular < MIN_TABULAR_ROWS {
        t(|| format!("reject: tabular={tabular}"));
        return None;
    }

    let mut rows: Vec<Vec<String>> = Vec::new();
    if header.is_empty() {
        rows.extend(body.iter().map(|(_, r)| r.clone()));
    } else {
        let mut joined = vec![String::new(); ncols];
        for (_, row) in &header {
            for (c, cell) in row.iter().enumerate() {
                if cell.trim().is_empty() {
                    continue;
                }
                if !joined[c].is_empty() {
                    joined[c].push(CELL_LINE_BREAK_PENDING);
                }
                joined[c].push_str(cell);
            }
        }
        rows.push(joined);
        rows.extend(body.iter().map(|(_, r)| r.clone()));
    }

    // Drop leading columns that are empty in every row (no value and no header):
    // they carry nothing and shift nothing. An interior or trailing empty column
    // is kept — a value must stay under its drawn header even when the column is
    // sparse (the Syndicat case).
    let mut drop = 0usize;
    while drop + 1 < ncols && rows.iter().all(|r| r.get(drop).is_none_or(|c| c.trim().is_empty())) {
        drop += 1;
    }
    if drop > 0 {
        for r in rows.iter_mut() {
            r.drain(0..drop);
        }
    }
    let ncols = ncols - drop;
    let boundaries: Vec<f64> = boundaries[drop..].to_vec();

    let start = header
        .iter()
        .map(|(i, _)| *i)
        .min()
        .unwrap_or_else(|| body[0].0);
    let end = body.last()?.0;

    // The renderer skips every line in `start..=end` and recovers only words
    // outside the table's x-band as side content. A word *inside* the band on a
    // consumed line that did not become a row would be silently dropped, so
    // refuse the re-cut when one exists (an out-of-band line interleaved with
    // the frame, e.g. a heading whose centre happens to sit under the frame).
    for (i, line) in lines.iter().enumerate().take(end + 1).skip(start) {
        if data.iter().any(|(j, _)| *j == i) || header.iter().any(|(j, _)| *j == i) {
            continue;
        }
        let in_frame = line_words(line).iter().any(|w| {
            let c = 0.5 * (w.x0 + w.x1);
            c > boundaries[0] + EDGE_EPS_PT && c < boundaries[ncols] - EDGE_EPS_PT
        });
        if in_frame {
            t(|| format!("reject: line {i} inside the band range has in-frame words"));
            return None;
        }
    }

    // bbox spans the frame's own x-range and the included lines' y-range.
    let (mut y0, mut y1) = (f64::INFINITY, f64::NEG_INFINITY);
    for line in &lines[start..=end] {
        for sp in line {
            y0 = y0.min(sp.y);
            y1 = y1.max(sp.y);
        }
    }
    if !y0.is_finite() {
        return None;
    }
    t(|| {
        format!(
            "frame start={start} end={end} rows={} body_start={body_start} hdr={}",
            rows.len(),
            header.len()
        )
    });
    Some(TableHit {
        start,
        end,
        rows,
        bbox: BoundingBox::new(boundaries[0], y0, boundaries[ncols], y1),
    })
}

/// Does `hit` carry only *value* folds, i.e. every in-cell line break joins
/// numeric/short value parts rather than prose? A description that wraps is a
/// genuine fold the re-cut must not split; a value the generic pass folded out
/// of two different drawn columns is not.
pub(super) fn hit_folds_values_only(hit: &TableHit) -> bool {
    let mut folded = false;
    for cell in hit.rows.iter().flatten() {
        if !cell.contains(CELL_LINE_BREAK) && !cell.contains(CELL_LINE_BREAK_PENDING) {
            continue;
        }
        folded = true;
        let normalized = cell.replace(CELL_LINE_BREAK_PENDING, CELL_LINE_BREAK);
        let parts: Vec<&str> = normalized
            .split(CELL_LINE_BREAK)
            .filter(|p| !p.trim().is_empty())
            .collect();
        if parts.len() < 2 || !parts.iter().all(|p| cell_is_value(p)) {
            return false;
        }
    }
    folded
}

/// Do the frame's boundaries come from drawn cell rectangles? A grid whose
/// producer boxes every cell states its columns through those stacked edges; a
/// table drawn with long column rules does not. Used only to decide whether the
/// drawn columns may override a `<br>` the generic pass folded across them.
pub(super) fn frame_is_cell_grid(frame: &Frame, cell_rects: &[(f64, f64, f64, f64)]) -> bool {
    const TOL: f64 = 2.0;
    frame.boundaries.iter().all(|&b| {
        cell_rects
            .iter()
            .any(|&(x0, _, x1, _)| (b - x0).abs() <= TOL || (b - x1).abs() <= TOL)
    })
}

/// Does `frame` describe a genuine multi-column grid on its own, with enough
/// tabular body rows to be a table? Used only to *create* a table the generic
/// word-gap clustering never saw; a frame that is merely a box (too few
/// columns) or a prose strip is refused here.
pub(super) fn is_grid_frame(lines: &[Vec<Span>], frame: &Frame) -> bool {
    if frame.ncols() < MIN_GRID_COLS {
        return false;
    }
    let mut rows = 0usize;
    let mut tabular = 0usize;
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let y = line[0].y;
        if !(frame.lo..=frame.hi).contains(&y) {
            continue;
        }
        let row = build_row(&line_words(line), line_size(line), &frame.boundaries);
        if row.iter().all(|c| c.trim().is_empty()) {
            continue;
        }
        rows += 1;
        if populated(&row) >= 2 {
            tabular += 1;
        }
    }
    rows >= MIN_GRID_ROWS && tabular >= MIN_GRID_TABULAR_ROWS
}

