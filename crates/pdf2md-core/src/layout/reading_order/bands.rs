// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Reading order recovery, multi-column stream separation, and structured DocBlock generation.

use super::*;

/// Segment `lines` into a sequence of column bands. Unlike `page_two_columns`
/// (which needs ONE gutter consistent across the *entire* page), this walks
/// the page top-to-bottom and detects each contiguous two-column region on
/// its own: a run of rows sharing a consistent internal gutter (via
/// `split_row_columns`) — plus rows that plainly fall entirely to one side of
/// that gutter (a label with no counterpart on the facing side) — becomes a
/// `Columns` band once it has at least 3 confirmed splits and passes the same
/// prose/clean gates `page_two_columns_rows` uses (average >= 2.5 real words
/// per side, no internal wide gutter inside either half). Any row that
/// neither extends the current run nor falls unambiguously to one side ends
/// the run: a confirmed run is emitted as `Columns`, otherwise its rows are
/// restored to their original single-line form and folded back into the
/// surrounding `Full` block.
pub fn detect_column_bands(lines: &[Vec<Span>]) -> Vec<ColumnBand> {
    detect_column_bands_opts(lines, true)
}

/// [`detect_column_bands`] without the staggered-stack pass, for the table
/// scanner. Re-banding a page's lines into staggered columns is a *rendering*
/// decision: the scanner works on the page's own geometry, and splitting a
/// wrapped-cell table into columns along its cell gutters hides the grid from
/// the banded aligned-grid scan.
pub(crate) fn detect_column_bands_for_tables(lines: &[Vec<Span>]) -> Vec<ColumnBand> {
    detect_column_bands_opts(lines, false)
}

pub(super) fn detect_column_bands_opts(lines: &[Vec<Span>], allow_stacks: bool) -> Vec<ColumnBand> {
    enum RunItem {
        Split {
            left: Vec<Span>,
            right: Vec<Span>,
            gutter: f64,
        },
        LeftOnly(Vec<Span>),
        RightOnly(Vec<Span>),
    }

    fn flush(run: &mut Vec<RunItem>, pending_full: &mut Vec<Vec<Span>>, bands: &mut Vec<ColumnBand>) {
        if run.is_empty() {
            return;
        }
        let gutters: Vec<f64> = run
            .iter()
            .filter_map(|it| match it {
                RunItem::Split { gutter, .. } => Some(*gutter),
                _ => None,
            })
            .collect();
        let left_splits: Vec<Vec<Span>> = run
            .iter()
            .filter_map(|it| match it {
                RunItem::Split { left, .. } => Some(left.clone()),
                _ => None,
            })
            .collect();
        let right_splits: Vec<Vec<Span>> = run
            .iter()
            .filter_map(|it| match it {
                RunItem::Split { right, .. } => Some(right.clone()),
                _ => None,
            })
            .collect();
        // A clean half is a single column; an unclean half contains its own
        // internal gutter — i.e. it is itself a multi-column table grid. When
        // one half is a flowing, multi-word prose column *and* the facing half
        // is such a grid, the established gutter still separates two
        // independent regions (body text beside a data table) and the band must
        // be kept. Merging those rows back instead weaves the table's cells
        // into the prose line-by-line, and the smaller table type then reads as
        // a LaTeX superscript of the prose (`model $properly^{Guardrails...}$`).
        // Both halves unclean means a single wide grid was cut in two, not a
        // prose/table split — leave those rows to the table detector.
        let left_clean = rows_are_clean(&left_splits);
        let right_clean = rows_are_clean(&right_splits);
        // A column block normally needs three crossing rows to confirm its
        // gutter. Two crossing rows can still describe a real corridor when
        // they are *consecutive* and each is a genuine *fusion* of two regions
        // that do not share a baseline:
        //
        //  * the target's numbered list has items 4 and 5 on the same visual
        //    line as the adjacent callout-box title ("Accès" / "bailleur") only
        //    because the line builder tolerates a half-em baseline difference
        //    (measured here: 2.28pt and 1.44pt), so emitting the list items
        //    first and the title after them is the true reading order;
        //  * a form label with its right-aligned value, or a table's
        //    model/score row, has both halves on *exactly* the same baseline
        //    (measured: 0.00pt) — it is one physical line whose row order must
        //    be preserved.
        //
        // Requiring a measured baseline gap on both crossing rows keeps the
        // latter row-wise, and requiring the crossings to be adjacent rejects a
        // single-column form whose wide gaps merely happen to align with a
        // one-sided row between them. Three crossings remain the default.
        let split_indices: Vec<usize> = run
            .iter()
            .enumerate()
            .filter(|(_, it)| matches!(it, RunItem::Split { .. }))
            .map(|(i, _)| i)
            .collect();
        let fused_corridor = split_indices.len() == 2
            && split_indices[1] == split_indices[0] + 1
            && split_indices.iter().all(|&i| match &run[i] {
                RunItem::Split { left, right, .. } => {
                    let ly = left.first().map(|s| s.y).unwrap_or(0.0);
                    let ry = right.first().map(|s| s.y).unwrap_or(0.0);
                    (ly - ry).abs() >= 1.0
                }
                _ => false,
            });
        let corridor_confirmed = gutters.len() >= 3 || fused_corridor;
        let ok = corridor_confirmed
            && avg_words_per_row(&left_splits) >= 2.5
            && avg_words_per_row(&right_splits) >= 2.5
            && ((left_clean && right_clean)
                || prose_beside_grid(&left_splits, &right_splits));

        if ok {
            if !pending_full.is_empty() {
                bands.push(ColumnBand::Full(std::mem::take(pending_full)));
            }
            let mut left = Vec::new();
            let mut right = Vec::new();
            for it in run.drain(..) {
                match it {
                    RunItem::Split { left: l, right: r, .. } => {
                        left.push(l);
                        right.push(r);
                    }
                    RunItem::LeftOnly(l) => left.push(l),
                    RunItem::RightOnly(r) => right.push(r),
                }
            }
            bands.push(ColumnBand::Columns { left, right });
        } else {
            for it in run.drain(..) {
                match it {
                    RunItem::Split { left: l, right: r, .. } => {
                        // Not a real column block after all: this was one
                        // physical row, so restore it whole rather than
                        // leaking the speculative split into the plain text.
                        let mut combined = l;
                        combined.extend(r);
                        combined.sort_by(|a, b| a.x.partial_cmp(&b.x).unwrap_or(std::cmp::Ordering::Equal));
                        pending_full.push(combined);
                    }
                    RunItem::LeftOnly(l) => pending_full.push(l),
                    RunItem::RightOnly(r) => pending_full.push(r),
                }
            }
        }
    }

    let mut bands: Vec<ColumnBand> = Vec::new();
    let mut pending_full: Vec<Vec<Span>> = Vec::new();
    let mut run: Vec<RunItem> = Vec::new();

    for line in lines {
        if line.is_empty() {
            continue;
        }
        let cur_gutters: Vec<f64> = run
            .iter()
            .filter_map(|it| match it {
                RunItem::Split { gutter, .. } => Some(*gutter),
                _ => None,
            })
            .collect();
        let med = run_median_gutter(&cur_gutters);
        let mut absorbed = false;

        if let Some((left, right)) = split_row_columns(line) {
            let l_end = left.iter().map(|s| s.x + s.advance).fold(f64::NEG_INFINITY, f64::max);
            let r_start = right.iter().map(|s| s.x).fold(f64::INFINITY, f64::min);
            let gutter = (l_end + r_start) / 2.0;
            let consistent = med.map_or(true, |m| (gutter - m).abs() <= (0.15 * m.abs()).max(6.0));
            if consistent {
                run.push(RunItem::Split { left, right, gutter });
                absorbed = true;
            }
        }

        // A justified column block can have a gutter too narrow for
        // `split_row_columns`'s standalone threshold. Once the run has an
        // established median gutter, split any remaining crossing row against
        // it (see `split_row_at_gutter`).
        if !absorbed {
            if let Some(m) = med {
                if let Some((left, right)) = split_row_at_gutter(line, m) {
                    let l_end = left.iter().map(|s| s.x + s.advance).fold(f64::NEG_INFINITY, f64::max);
                    let r_start = right.iter().map(|s| s.x).fold(f64::INFINITY, f64::min);
                    let gutter = (l_end + r_start) / 2.0;
                    if (gutter - m).abs() <= (0.15 * m.abs()).max(6.0) {
                        run.push(RunItem::Split { left, right, gutter });
                        absorbed = true;
                    }
                }
            }
        }

        if !absorbed {
            if let Some(m) = med {
                // Only a row vertically adjacent to the run can extend it. A
                // later full-width heading (or any short line) that happens to
                // sit entirely on one side of the gutter begins a new block —
                // absorbing it would render it *before* the facing column.
                let contiguous = run.last().map_or(true, |it| {
                    let last_y = match it {
                        RunItem::Split { left, .. } => left[0].y,
                        RunItem::LeftOnly(l) | RunItem::RightOnly(l) => l[0].y,
                    };
                    let gap = last_y - line[0].y;
                    gap >= -1.0 && gap <= 2.5 * line[0].size.max(0.1)
                });
                if contiguous {
                    let x0 = line.iter().map(|s| s.x).fold(f64::INFINITY, f64::min);
                    let x1 = line.iter().map(|s| s.x + s.advance).fold(f64::NEG_INFINITY, f64::max);
                    if x1 <= m {
                        run.push(RunItem::LeftOnly(line.clone()));
                        absorbed = true;
                    } else if x0 >= m {
                        run.push(RunItem::RightOnly(line.clone()));
                        absorbed = true;
                    }
                }
            }
        }

        if !absorbed {
            flush(&mut run, &mut pending_full, &mut bands);
            pending_full.push(line.clone());
        }
    }
    flush(&mut run, &mut pending_full, &mut bands);
    if !pending_full.is_empty() {
        bands.push(ColumnBand::Full(pending_full));
    }

    // The running-gutter pass can only seed a column block from a row whose
    // inter-column gap clears the standalone `1.2em` threshold. Whole classes of
    // real two-column blocks never clear that bar and still sit in a `Full`
    // band here — a narrow page gutter beside a wider internal table gutter, or
    // a bilingual layout (French and English side by side) whose every visual
    // line is a single `Tj` run. Recover those from the vertical projection.
    //
    // The projection is applied to *every* `Full` band, not only when the page
    // produced no `Columns` band at all. A page can hold one two-column block
    // the running-gutter pass already found (this document's page 2 baggage
    // notes) *and* another it could not (the AVANT/PENDANT/APRÈS bilingual
    // contact blocks, left in a `Full` band); skipping the projection because
    // *a* `Columns` band existed anywhere else left the second block woven
    // row by row.
    let mut out: Vec<ColumnBand> = Vec::new();
    for band in bands {
        match band {
            ColumnBand::Columns { .. } | ColumnBand::Stacks(_) => out.push(band),
            ColumnBand::Full(rows) => project_full_band(rows, allow_stacks, &mut out, 0),
        }
    }
    out
}

/// Depth cap for the recursive band projection. The recursion already
/// terminates (each peel passes strictly smaller row slices), but a pathological
/// band that peels one region at a time could nest once per row; at the cap the
/// remaining rows are emitted as a single `Full` band. 512 is far above any real
/// page's band depth.
const MAX_BAND_RECURSION_DEPTH: usize = 512;

/// Recursively peel two-column projection regions out of one `Full` band, then
/// (when `allow_stacks`) a staggered multi-column region.
///
/// [`projection_columns_region`] returns only the longest region, and a single
/// `Full` band can hold several independent two-column blocks (the bilingual
/// AVANT/PENDANT/APRÈS blocks share one band). Peel one region, then recurse on
/// the rows before and after it; a slice with no region stays a `Full` band.
/// The staggered pass runs only when the projection found nothing on this band,
/// and leaves its surroundings as plain `Full` rows rather than re-projecting
/// them.
pub(super) fn project_full_band(rows: Vec<Vec<Span>>, allow_stacks: bool, out: &mut Vec<ColumnBand>, depth: usize) {
    if depth >= MAX_BAND_RECURSION_DEPTH {
        out.push(ColumnBand::Full(rows));
        return;
    }
    if let Some((start, end, left, right)) = projection_columns_region(&rows) {
        if start > 0 {
            project_full_band(rows[..start].to_vec(), allow_stacks, out, depth + 1);
        }
        out.push(ColumnBand::Columns { left, right });
        if end + 1 < rows.len() {
            project_full_band(rows[end + 1..].to_vec(), allow_stacks, out, depth + 1);
        }
        return;
    }
    if allow_stacks {
        if let Some((start, end, columns)) = staggered_columns_region(&rows) {
            // The rows around the stack stay plain `Full` blocks. They are not
            // re-projected: the projection already failed on this whole band
            // (otherwise the branch above would have run), and re-running it on
            // a sub-slice can find a region the full band did not and reorder
            // the remainder.
            if start > 0 {
                out.push(ColumnBand::Full(rows[..start].to_vec()));
            }
            out.push(ColumnBand::Stacks(columns));
            if end + 1 < rows.len() {
                out.push(ColumnBand::Full(rows[end + 1..].to_vec()));
            }
            return;
        }
    }
    out.push(ColumnBand::Full(rows));
}
