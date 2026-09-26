// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Stage 3 & Stage 3b ruler scanning, grid alignment, and column corridor analysis.

use super::*;

/// Diagnostic tracer: prints when TABLE_TRACE env is set.
pub(super) fn t(msg: &str) {
    if std::env::var("TABLE_TRACE").is_ok() {
        eprintln!("[tbl] {}", msg);
    }
}

/// Reconstruct a line's text (for diagnostics).
pub(super) fn line_text(line: &[Span]) -> String {
    line.iter().map(|s| s.text.as_str()).collect()
}

/// Detect grid tables on a page's visual lines.
///
/// The page is split into column bands first (see `scan_aligned_grids_banded`):
/// a side table sharing baselines with a neighboring column's prose is scanned
/// in isolation instead of against that prose.
pub fn scan_aligned_grids(lines: &[Vec<Span>], tol_mult: f64, covered: &[TableHit]) -> Vec<TableHit> {
    // The public/lossy passes (including the stage-3b gap recovery) keep the
    // original strict merged-cell veto; only `find_tables`'s explicit
    // refinement pass opts into the relaxed one.
    scan_aligned_grids_banded(lines, tol_mult, covered, false)
}

/// Stage-3 table recovery: strict ruler alignment (the default pass).
pub fn find_tables(lines: &[Vec<Span>]) -> Vec<TableHit> {
    // The strict geometry is the primary detector. The relaxed pass (which
    // tolerates a wide merged/spanning cell crossing a genuine column
    // boundary — needed for dense bilingual grids whose continuation rows
    // overflow several columns) may only *refine* a table the strict pass
    // already found: if it overlaps a strict hit and resolves more columns,
    // its finer segmentation replaces that hit. It never invents a table the
    // strict geometry does not see, so it cannot turn ordinary address/prose
    // blocks into spurious grids.
    let mut hits = scan_aligned_grids_banded(lines, 1.0, &[], false);
    let wide_hits = scan_aligned_grids_banded(lines, 1.0, &[], true);
    for wh in wide_hits {
        let Some(pos) = hits
            .iter()
            .position(|h| h.start <= wh.end && wh.start <= h.end)
        else {
            continue;
        };
        let strict_cols = hits[pos].rows.first().map(|r| r.len()).unwrap_or(0);
        let wide_cols = wh.rows.first().map(|r| r.len()).unwrap_or(0);
        // Only refine a table that is already genuinely tabular. A two-column
        // strict hit is usually a clean label/value block; letting the relaxed
        // pass absorb surrounding address/prose lines into it produces a worse
        // grid, so small tables keep their strict segmentation.
        if wide_cols > strict_cols && strict_cols >= 3 {
            hits[pos] = wh;
        }
    }
    hits
}

/// Stage-3b recovery pass: the same grid scan with a wider alignment
/// tolerance, over rows the strict pass did not claim (jittered tables).
pub fn find_gap_tables(lines: &[Vec<Span>], covered: &[TableHit]) -> Vec<TableHit> {
    scan_aligned_grids_banded(lines, 2.0, covered, false)
}
