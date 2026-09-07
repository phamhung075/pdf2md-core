// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Stage 3 & Stage 3b ruler scanning, grid alignment, and column corridor analysis.

use crate::layout::glyph_stream::Span;
use crate::layout::tables::consolidation::{bucket, bucket_words, consolidate_table_rows, merge_complementary_columns};
use crate::layout::tables::validation::is_tabular_rows;
use crate::models::BoundingBox;

/// A detected table: line range [start, end] (inclusive) plus cell rows.
#[derive(Debug, Clone)]
pub struct TableHit {
    pub start: usize,
    pub end: usize,
    pub rows: Vec<Vec<String>>,
    pub bbox: BoundingBox,
}

/// One decoded word with its device start x and end x (the column-start ruler).
#[derive(Debug, Clone)]
pub struct WordTok {
    pub text: String,
    pub x0: f64,
    pub x1: f64,
}

/// Word tokens + sorted start positions per visual line.
#[derive(Debug, Clone)]
pub struct RowInfo {
    pub words: Vec<WordTok>,
    pub starts: Vec<f64>,
    pub size: f64,
}

/// Split one visual line into words (same gap rules as the renderer).
pub fn line_words(line: &[Span]) -> Vec<WordTok> {
    let mut out: Vec<WordTok> = Vec::new();
    let mut text = String::new();
    let mut x0: Option<f64> = None;
    let mut prev_x: Option<f64> = None;
    let mut prev_advance = 0.0f64;
    let mut last_end = 0.0f64;

    let flush = |out: &mut Vec<WordTok>, text: &mut String, x0: &mut Option<f64>, last_end: f64| {
        if let Some(s) = x0.take() {
            if !text.trim().is_empty() {
                out.push(WordTok {
                    text: std::mem::take(text).trim().to_string(),
                    x0: s,
                    x1: last_end,
                });
            }
            text.clear();
        }
    };

    for sp in line {
        if sp.text.is_empty() {
            continue;
        }
        let size = sp.size.max(0.1);
        let space_adv = 0.25 * size;
        let is_space = sp.text.chars().all(|c| c == ' ');
        if let Some(px) = prev_x {
            let gap = sp.x - px;
            let word_break = is_space
                || (sp.text != " " && (gap > 2.5 * size || gap - prev_advance > 0.65 * space_adv));
            if word_break {
                flush(&mut out, &mut text, &mut x0, last_end);
            }
        } else if is_space {
            continue;
        }
        if x0.is_none() {
            x0 = Some(sp.x);
        }
        text.push_str(sp.text.trim());
        prev_x = Some(sp.x);
        prev_advance = sp.advance;
        last_end = sp.x + sp.advance;
    }
    flush(&mut out, &mut text, &mut x0, last_end);
    out
}

/// Table column rulers in rows lo..=hi (inclusive): word-start x positions
/// that appear in at least 2 rows within tolerance, spaced by at least min_gutter.
pub fn table_rulers(info: &[RowInfo], tol: f64, lo: usize, hi: usize, min_gutter: f64) -> Vec<f64> {
    let num_rows = hi - lo + 1;
    if num_rows < 2 {
        return Vec::new();
    }
    let mut starts_with_row: Vec<(f64, usize)> = Vec::new();
    for ri in lo..=hi {
        for &s in &info[ri].starts {
            starts_with_row.push((s, ri));
        }
    }
    starts_with_row.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    if starts_with_row.is_empty() {
        return Vec::new();
    }

    struct Clust {
        sum_x: f64,
        count: usize,
        rows: std::collections::HashSet<usize>,
    }
    let mut clusters: Vec<Clust> = Vec::new();
    for (x, ri) in starts_with_row {
        let mut matched = false;
        if let Some(last) = clusters.last_mut() {
            let mean = last.sum_x / last.count as f64;
            if (x - mean).abs() <= tol {
                last.sum_x += x;
                last.count += 1;
                last.rows.insert(ri);
                matched = true;
            }
        }
        if !matched {
            let mut rows = std::collections::HashSet::new();
            rows.insert(ri);
            clusters.push(Clust {
                sum_x: x,
                count: 1,
                rows,
            });
        }
    }

    let rulers: Vec<f64> = clusters
        .into_iter()
        .filter(|c| c.rows.len() >= 2)
        .map(|c| c.sum_x / c.count as f64)
        .collect();

    let mut merged: Vec<f64> = Vec::new();
    for r in rulers {
        match merged.last_mut() {
            Some(prev) if r - *prev < min_gutter => {}
            _ => merged.push(r),
        }
    }
    merged
}

/// Detect grid tables on a page's visual lines.
pub fn scan_aligned_grids(lines: &[Vec<Span>], tol_mult: f64, covered: &[TableHit]) -> Vec<TableHit> {
    if lines.len() < 3 {
        return Vec::new();
    }

    let info: Vec<RowInfo> = lines
        .iter()
        .map(|l| {
            let words = line_words(l);
            let mut starts: Vec<f64> = words.iter().map(|w| w.x0).collect();
            starts.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            RowInfo {
                words,
                starts,
                size: l[0].size.max(0.1),
            }
        })
        .collect();

    let tol = tol_mult
        * info
            .iter()
            .map(|r| (0.06 * r.size).clamp(0.5, 1.2))
            .fold(0.0f64, f64::max);

    // Vertical bands of consecutive rows that could sit in one grid (tight line pitch,
    // no huge paragraph gap inside). Rows already claimed by a previous pass are excluded.
    let mut bands: Vec<Vec<usize>> = Vec::new();
    for (i, r) in info.iter().enumerate() {
        if r.words.is_empty() {
            continue;
        }
        if covered.iter().any(|h| h.start <= i && i <= h.end) {
            continue;
        }
        match bands.last_mut() {
            Some(band) => {
                let prev = *band.last().unwrap();
                let gap = lines[prev][0].y - lines[i][0].y;
                let scale = info[prev].size.max(r.size);
                let line_pitch = 1.2 * scale;
                if gap >= 0.0 && (gap <= 3.8 * line_pitch || gap <= 38.0) {
                    band.push(i);
                } else {
                    bands.push(vec![i]);
                }
            }
            None => bands.push(vec![i]),
        }
    }

    let mut hits: Vec<TableHit> = Vec::new();
    for band in bands {
        if band.len() < 2 {
            continue;
        }
        let mut lo = 0usize;
        while lo < band.len() {
            if lo + 1 >= band.len() {
                break;
            }
            let max_size = band[lo..=lo + 1]
                .iter()
                .map(|&i| info[i].size)
                .fold(0.0f64, f64::max);
            let min_gutter = (1.1 * max_size).max(6.0);
            let mut hi = lo + 1;
            let mut rulers = table_rulers(&info, tol, band[lo], band[hi], min_gutter);
            // Verify that the seed rows (lo and hi) both match at least 2 column rulers
            let match_lo = rulers.iter().filter(|&&r| info[band[lo]].starts.iter().any(|&s| (r - s).abs() <= tol * 1.5)).count();
            let match_hi = rulers.iter().filter(|&&r| info[band[hi]].starts.iter().any(|&s| (r - s).abs() <= tol * 1.5)).count();
            if rulers.len() < 2 || match_lo < 2 || match_hi < 2 {
                lo += 1;
                continue;
            }
            while hi + 1 < band.len() && rulers.len() >= 2 {
                let next_ri = band[hi + 1];
                let next_r = table_rulers(&info, tol, band[lo], next_ri, min_gutter);
                let next_match = next_r.iter().filter(|&&r| info[next_ri].starts.iter().any(|&s| (r - s).abs() <= tol * 1.5)).count();
                if next_r.len() >= 2 && next_match >= 2 {
                    hi += 1;
                    rulers = next_r;
                    continue;
                }
                // Single-column continuation row (e.g. wrapped airport name in a multi-line cell)
                if next_match == 1 {
                    let straddles = info[next_ri].words.iter().any(|w| {
                        rulers[1..].iter().any(|&r| w.x0 < r - tol && w.x1 > r + tol)
                    });
                    if !straddles {
                        let has_future_match = (hi + 2..band.len().min(hi + 4)).any(|fut_idx| {
                            let fut_ri = band[fut_idx];
                            rulers.iter().filter(|&&r| info[fut_ri].starts.iter().any(|&s| (r - s).abs() <= tol * 1.5)).count() >= 2
                        });
                        if has_future_match {
                            hi += 1;
                            continue;
                        }
                    }
                }
                break;
            }
            if rulers.len() < 2 {
                lo += 1;
                continue;
            }
            let win_rows: Vec<usize> = band[lo..=hi].to_vec();

            // No indivisible word can straddle an interior column ruler.
            let has_straddling_word = win_rows.iter().any(|&ri| {
                info[ri].words.iter().any(|w| {
                    rulers[1..].iter().any(|&r| w.x0 < r - tol && w.x1 > r + tol)
                })
            });
            if has_straddling_word {
                lo += 1;
                continue;
            }

            let multi_col_rows = win_rows
                .iter()
                .filter(|&&i| {
                    let row_cells = bucket(&info, i, &rulers);
                    row_cells.iter().filter(|c| !c.trim().is_empty()).count() >= 2
                })
                .count();
            if multi_col_rows < 2 {
                lo += 1;
                continue;
            }
            if hi - lo + 1 >= 2 && rulers.len() >= 2 {
                let max_size = win_rows
                    .iter()
                    .map(|&i| info[i].size)
                    .fold(0.0f64, f64::max);
                let min_gutter = (1.1 * max_size).max(6.0);
                let ok_gutter = rulers.windows(2).all(|p| p[1] - p[0] >= min_gutter);
                if ok_gutter {
                    let mid1 = (rulers[0] + rulers[1]) / 2.0;
                    let mut distinct: std::collections::HashSet<String> =
                        std::collections::HashSet::new();
                    for &i in &win_rows {
                        let first: String = info[i]
                            .words
                            .iter()
                            .filter(|w| w.x0 < mid1)
                            .map(|w| w.text.as_str())
                            .collect::<Vec<_>>()
                            .join(" ");
                        if !first.trim().is_empty() {
                            distinct.insert(first);
                        }
                    }
                    if distinct.len() >= 2 {
                        let flowing_rows = win_rows
                            .iter()
                            .filter(|&&ri| {
                                let cells = bucket_words(&info, ri, &rulers);
                                let size = info[ri].size;
                                for c in 0..cells.len().saturating_sub(1) {
                                    if !cells[c].is_empty() && !cells[c + 1].is_empty() {
                                        let last_w = cells[c].last().unwrap();
                                        let first_next_w = cells[c + 1].first().unwrap();
                                        let gap = first_next_w.x0 - last_w.x1;
                                        if gap < 0.65 * size {
                                            return true;
                                        }
                                    }
                                }
                                false
                            })
                            .count();
                        if flowing_rows >= 2 && flowing_rows * 2 >= multi_col_rows {
                            lo += 1;
                            continue;
                        }

                        let table_rows: Vec<Vec<String>> = win_rows
                            .iter()
                            .map(|&i| bucket(&info, i, &rulers))
                            .collect();
                        let (table_rows, rulers) =
                            merge_complementary_columns(table_rows, &win_rows, &info, &rulers);
                        // Drop fully-empty edge columns.
                        let ncol = rulers.len();
                        let mut c0 = 0usize;
                        let mut c1 = ncol;
                        while c0 < c1 && table_rows.iter().all(|r| r[c0].trim().is_empty()) {
                            c0 += 1;
                        }
                        while c1 > c0 && table_rows.iter().all(|r| r[c1 - 1].trim().is_empty()) {
                            c1 -= 1;
                        }
                        if c1 - c0 >= 2 {
                            let rows2: Vec<Vec<String>> =
                                table_rows.iter().map(|r| r[c0..c1].to_vec()).collect();
                            if !is_tabular_rows(&rows2) {
                                lo += 1;
                                continue;
                            }
                            let consolidated_rows =
                                consolidate_table_rows(rows2, &win_rows, lines, &info);
                            let mut min_x = f64::INFINITY;
                            let mut max_x = f64::NEG_INFINITY;
                            let mut min_y = f64::INFINITY;
                            let mut max_y = f64::NEG_INFINITY;
                            for &i in &win_rows {
                                for sp in &lines[i] {
                                    min_x = min_x.min(sp.x);
                                    max_x = max_x.max(sp.x + sp.advance);
                                    min_y = min_y.min(sp.y);
                                    max_y = max_y.max(sp.y);
                                }
                            }
                            hits.push(TableHit {
                                start: win_rows[0],
                                end: *win_rows.last().unwrap(),
                                rows: consolidated_rows,
                                bbox: BoundingBox::new(min_x, min_y, max_x, max_y),
                            });
                            lo = hi + 1;
                            continue;
                        }
                    }
                }
            }
            lo += 1;
        }
    }
    hits
}

/// Stage-3 table recovery: strict ruler alignment (the default pass).
pub fn find_tables(lines: &[Vec<Span>]) -> Vec<TableHit> {
    scan_aligned_grids(lines, 1.0, &[])
}

/// Stage-3b recovery pass: the same grid scan with a wider alignment
/// tolerance, over rows the strict pass did not claim (jittered tables).
pub fn find_gap_tables(lines: &[Vec<Span>], covered: &[TableHit]) -> Vec<TableHit> {
    scan_aligned_grids(lines, 2.0, covered)
}
