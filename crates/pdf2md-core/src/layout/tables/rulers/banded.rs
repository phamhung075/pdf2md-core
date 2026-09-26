// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Stage 3 & Stage 3b ruler scanning, grid alignment, and column corridor analysis.

use super::*;

/// Map each line of an isolated column stream back to its index in the page's
/// own `lines`, by baseline-y containment. A merged visual line holds spans
/// from both columns whose baselines can differ by a few points, so the stream
/// line's y is matched against the *range* of y values an original line spans
/// rather than an exact y key.
pub(super) fn stream_index_map(stream: &[Vec<Span>], ranges: &[(f64, f64, usize)]) -> Vec<usize> {
    stream
        .iter()
        .map(|l| {
            let q = l.iter().map(|s| s.y).fold(f64::INFINITY, f64::min);
            ranges
                .iter()
                .filter(|&&(lo, hi, _)| q >= lo - 0.75 && q <= hi + 0.75)
                .min_by(|a, b| {
                    let da = (q - 0.5 * (a.0 + a.1)).abs();
                    let db = (q - 0.5 * (b.0 + b.1)).abs();
                    da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
                })
                .map(|&(_, _, i)| i)
                .unwrap_or(0)
        })
        .collect()
}

/// [`scan_aligned_grids_opts`] with the page split into column bands first, so a
/// side table's rows are never compared against a neighboring column's prose.
///
/// A page can interleave two independent regions on one visual line: a narrow
/// parameter/score table set in the right margin at the same baseline as the
/// main column's prose. Scanning the merged lines lets the seed-and-grow window
/// annex prose rows, which then trips the `flowing` veto and drops the whole
/// table to plain text. `detect_column_bands` already separates those regions,
/// so each column stream is scanned in isolation and every hit is mapped back to
/// the page's own line indices (the ones `render_with_tables` splices against).
///
/// A page with no genuine column band keeps the original whole-page scan, so
/// single-column table detection stays byte-identical.
pub(super) fn scan_aligned_grids_banded(
    lines: &[Vec<Span>],
    tol_mult: f64,
    covered: &[TableHit],
    wide_ok: bool,
) -> Vec<TableHit> {
    if lines.len() < 2 {
        return Vec::new();
    }
    let bands = detect_column_bands_for_tables(lines);
    if bands.iter().all(|b| matches!(b, ColumnBand::Full(_))) {
        return scan_aligned_grids_opts(lines, tol_mult, covered, wide_ok);
    }

    // Baseline-y ranges of each page line, for mapping a cloned stream line
    // back to the page line it came from.
    let mut ranges: Vec<(f64, f64, usize)> = Vec::with_capacity(lines.len());
    for (i, l) in lines.iter().enumerate() {
        if l.is_empty() {
            continue;
        }
        let lo = l.iter().map(|s| s.y).fold(f64::INFINITY, f64::min);
        let hi = l.iter().map(|s| s.y).fold(f64::NEG_INFINITY, f64::max);
        ranges.push((lo, hi, i));
    }

    // `scan_aligned_grids_opts` only reads `start`/`end` from `covered`; empty
    // rows and a zero bbox are enough to translate the page-level covered
    // ranges into a stream's local indices.
    let dummy = |start: usize, end: usize| TableHit {
        start,
        end,
        rows: Vec::new(),
        bbox: BoundingBox::new(0.0, 0.0, 0.0, 0.0),
    };

    let mut hits: Vec<TableHit> = Vec::new();
    for band in bands {
        let streams: Vec<Vec<Vec<Span>>> = match band {
            ColumnBand::Full(rows) => vec![rows],
            ColumnBand::Columns { left, right } => vec![left, right],
            ColumnBand::Stacks(columns) => columns,
        };
        for stream in streams {
            if stream.len() < 2 {
                continue;
            }
            let index_map = stream_index_map(&stream, &ranges);
            let mut local_covered: Vec<TableHit> = Vec::new();
            let mut run: Option<usize> = None;
            for (local, &orig) in index_map.iter().enumerate() {
                let cov = covered.iter().any(|h| h.start <= orig && orig <= h.end);
                match (run, cov) {
                    (None, true) => run = Some(local),
                    (Some(s), false) => {
                        local_covered.push(dummy(s, local - 1));
                        run = None;
                    }
                    _ => {}
                }
            }
            if let Some(s) = run {
                local_covered.push(dummy(s, index_map.len() - 1));
            }

            for sh in scan_aligned_grids_opts(&stream, tol_mult, &local_covered, wide_ok) {
                let start = index_map[sh.start];
                let end = index_map[sh.end];
                if start <= end {
                    hits.push(TableHit {
                        start,
                        end,
                        rows: sh.rows,
                        bbox: sh.bbox,
                    });
                }
            }
        }
    }
    hits
}
