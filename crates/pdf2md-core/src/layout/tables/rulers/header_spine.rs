// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Header-spine column derivation.
//!
//! A grid whose first row is a clean, gutter-separated label row ("header
//! spine") already states its own columns: each cell is one column, and a
//! second header line refines those labels vertically. The spine pass in
//! [`super::header_spine_legacy`] recognises a two-line header at the window
//! start; this module adds two narrow things around it:
//!
//! * the header group may sit one or two rows *above* the seed window (a rule
//!   or extra leading put it in the band but the seed opened on its wrapped
//!   second line), so the group is recovered and the window start is moved up
//!   before the spine pass runs;
//! * a header may be a **single** line directly above the value rows, which
//!   the two-line pass cannot accept on its own.
//!
//! Both are deliberately conservative: only the legacy pass may produce rulers,
//! key/value boxes, address blocks and prose stay out through its numeric gate,
//! and the caller's own grid hits survive whenever this pass declines.

use super::*;

/// Maximum words in one spine cell for it to read as a short column label
/// rather than a prose span.
pub(super) const SPINE_MAX_CELL_WORDS: usize = 4;
/// Minimum number of cells a row must show to define a table's columns.
pub(super) const SPINE_MIN_CELLS: usize = 3;
/// Minimum value columns (cells carrying a digit) in the data rows for a spine
/// to be a real table rather than a side-by-side text/address block.
pub(super) const SPINE_MIN_NUMERIC_COLS: usize = 2;
/// Maximum rows looked ahead when extending over a tall first cell.
pub(super) const SPINE_MAX_LOOKAHEAD: usize = 24;
/// Maximum rows the window may grow over a tall first cell. A wrapped cell has
/// a handful of continuation lines; a larger growth annexes neighbouring grids.
pub(super) const SPINE_MAX_EXTEND: usize = 8;
/// Largest seed window the spine pass will re-column.
pub(super) const SPINE_MAX_WINDOW_ROWS: usize = 8;
/// Alignment slack (in points) on top of the window tolerance when testing that
/// a wrapped sub-header cell falls inside one of the header's cells.
pub(super) const SUB_HEADER_SLACK_PT: f64 = 2.0;
/// Largest baseline gap (in text sizes) between a value row and the line above
/// it. Wider than the intra-cell pitch because a table often leaves extra
/// leading between the last wrapped description line and the next line item;
/// wider than this is a new section, not another row.
pub(super) const SPINE_ROW_GAP_PITCH_MULT: f64 = 3.0;
/// Extra vertical pitch (in text sizes) a header row sitting in the *same*
/// contiguous band as the line below it may still leave the seed window's top.
/// `grid_scan` splits a band when the gap exceeds `3.8 * 1.2 * size`, so a
/// header separated from its data by a rule or extra leading would otherwise
/// become its own band and be emitted as loose text.
const HEADER_BACKTRACK_PITCH_MULT: f64 = 3.8;

/// The cell boundaries of `words`: each cell is a maximal run of words whose
/// consecutive gaps stay below `min_gutter`. Returns `(start, end, text)`.
pub(super) fn row_cells(words: &[WordTok], min_gutter: f64) -> Vec<(f64, f64, String)> {
    let mut cells: Vec<(f64, f64, String)> = Vec::new();
    for (i, w) in words.iter().enumerate() {
        let new_cell = i == 0 || w.x0 - words[i - 1].x1 >= min_gutter;
        if new_cell {
            cells.push((w.x0, w.x1, w.text.clone()));
        } else if let Some(last) = cells.last_mut() {
            last.1 = w.x1;
            last.2.push(' ');
            last.2.push_str(&w.text);
        }
    }
    cells
}

/// Whether every cell is a short label with no digit — the shape of a header
/// spine row rather than a data row.
pub(super) fn cells_are_labels(cells: &[(f64, f64, String)]) -> bool {
    !cells.is_empty()
        && cells.iter().all(|(_, _, t)| {
            !t.trim().is_empty()
                && !t.chars().any(|c| c.is_ascii_digit())
                && t.split_whitespace().count() <= SPINE_MAX_CELL_WORDS
        })
}

/// Whether the row at band position `pos` (`band[pos]` is its line) is a short
/// label row — the shape of a header line rather than a line-item or totals row.
fn row_at_is_label(info: &[RowInfo], band: &[usize], pos: usize, min_gutter: f64) -> bool {
    let cells = row_cells(&info[band[pos]].words, min_gutter);
    cells.len() >= SPINE_MIN_CELLS && cells_are_labels(&cells)
}

/// Number of merged cells in the row at band position `pos`.
fn cell_count_at(info: &[RowInfo], band: &[usize], pos: usize, min_gutter: f64) -> usize {
    row_cells(&info[band[pos]].words, min_gutter).len()
}

/// The topmost band position of the header group sitting immediately above the
/// window's first row, or `None` when no label row sits directly above it.
///
/// The scan stops at a row that is not a short label, at a gap beyond header
/// pitch (a section rule ends the header), and at a row with *fewer* cells than
/// the row below it: a wrapped header is widest on its first line ("Prix
/// Unitaire" over "HT"), so a one-cell row above a six-cell one is the section
/// title, not another header line.
///
/// The gap is measured in the *header's* own text size, not the row above it:
/// a section heading is set larger than the header, and letting its size raise
/// the pitch would annex it as another header line.
fn preceding_header_group(
    seed: usize,
    info: &[RowInfo],
    band: &[usize],
    lines: &[Vec<Span>],
    min_gutter: f64,
) -> Option<usize> {
    let mut header_lo: Option<usize> = None;
    let mut cur = seed;
    while cur > 0 {
        let prev = cur - 1;
        if !row_at_is_label(info, band, prev, min_gutter) {
            break;
        }
        if cell_count_at(info, band, prev, min_gutter)
            < cell_count_at(info, band, cur, min_gutter)
        {
            break;
        }
        let gap = lines[band[prev]][0].y - lines[band[cur]][0].y;
        let pitch = HEADER_BACKTRACK_PITCH_MULT * info[band[cur]].size.max(1.0);
        if !(gap > 0.0 && gap <= pitch) {
            break;
        }
        header_lo = Some(prev);
        cur = prev;
    }
    header_lo
}

/// Derive the window's columns from its header spine.
///
/// The header group above the window is recovered first — a seed that opened on
/// a wrapped header's second line still needs the full header row — and the
/// window start is moved up to it. Only the legacy pass produces rulers, so
/// every grid it accepts is the previous release's exact result and the caller
/// keeps its own grid whenever this pass declines.
///
/// Two vetoes keep the pass out of grids that belong to other models:
/// * a header that reads as an operations ledger (a date/value column beside an
///   amount column) is left to the ledger model;
/// * the single-line-header branch needs the caller's own rulers to already
///   form a tabular grid, so a header above prose is never promoted to a table.
#[allow(clippy::too_many_arguments)] // the pass geometry is threaded verbatim
pub(super) fn header_spine_refine(
    info: &[RowInfo],
    lines: &[Vec<Span>],
    band: &[usize],
    win_lo: &mut usize,
    hi: &mut usize,
    tol: f64,
    min_gutter: f64,
    generic_rulers: &[f64],
) -> Option<Vec<f64>> {
    let orig_win_lo = *win_lo;
    let orig_hi = *hi;
    if reads_as_ledger(info, band, orig_win_lo) {
        return None;
    }
    let generic_first_cells = generic_window_header_cells(
        info,
        lines,
        band,
        orig_win_lo,
        orig_hi,
        generic_rulers,
        tol,
        min_gutter,
    );
    // Only a window that opened on a wrapped header's *second* line needs the
    // group above it; a seed that opened on a data row is left to the generic
    // and ledger passes exactly as before.
    if row_at_is_label(info, band, orig_win_lo, min_gutter) {
        if let Some(header) = preceding_header_group(orig_win_lo, info, band, lines, min_gutter) {
        // A group that already starts the band has no row above it to measure a
        // gap against; only a group with a sibling row above needs the test.
        let fits = header == 0 || {
            let prev = band[header - 1];
            let gap = lines[prev][0].y - lines[band[header]][0].y;
            let pitch = HEADER_BACKTRACK_PITCH_MULT * info[band[header]].size.max(1.0);
            gap > 0.0 && gap <= pitch
        };
        if fits && !reads_as_ledger(info, band, header) {
            *win_lo = header;
            if let Some(rulers) = super::header_spine_legacy::legacy_spine_refine(
                info,
                lines,
                band,
                header,
                hi,
                tol,
                min_gutter,
                generic_first_cells,
                true,
            ) {
                return Some(rulers);
            }
            *win_lo = orig_win_lo;
            *hi = orig_hi;
        }
        }
    }
    super::header_spine_legacy::legacy_spine_refine(
        info,
        lines,
        band,
        orig_win_lo,
        hi,
        tol,
        min_gutter,
        generic_first_cells,
        false,
    )
}

/// Whether the row at band position `pos` reads as an operations-ledger header.
fn reads_as_ledger(info: &[RowInfo], band: &[usize], pos: usize) -> bool {
    let words = &info[band[pos]].words;
    crate::layout::tables::ledger::header_reads_as_ledger(words, info[band[pos]].size)
}

/// The caller's own rulers turn the window into a tabular grid whose header row
/// has this many cells, or `None` when they do not. The single-line branch may
/// then un-merge a header the generic pass collapsed, but never promote a
/// header that the generic pass rejected (or one the generic pass already read
/// as a full header row).
#[allow(clippy::too_many_arguments)] // the pass geometry is threaded verbatim
fn generic_window_header_cells(
    info: &[RowInfo],
    lines: &[Vec<Span>],
    band: &[usize],
    win_lo: usize,
    hi: usize,
    generic_rulers: &[f64],
    tol: f64,
    min_gutter: f64,
) -> Option<usize> {
    if generic_rulers.len() < 2 {
        return None;
    }
    let rows: Vec<usize> = band[win_lo..=hi].to_vec();
    let full = bucket_rows_content_aware(info, &rows, generic_rulers, tol, min_gutter);
    let (full, _) = merge_complementary_columns(full, &rows, info, generic_rulers, tol);
    let full = drop_empty_edge_columns(full);
    if !is_tabular_rows(&full)
        || trimmed_table(info, &rows, generic_rulers, lines, tol, min_gutter, false).is_none()
    {
        return None;
    }
    full.first().map(|r| r.len())
}
