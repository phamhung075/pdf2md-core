// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Drawn-rule frame tables.
//!
//! A table region can be enclosed by drawn rules: an outer left/right pair plus
//! at least two interior vertical rules that span most of the region height.
//! When that geometry is present it is a far stronger column statement than
//! word-start alignment, because the rules separate columns that text-ruler
//! scanning merges:
//!
//! * a sidebar (small label/value boxes) sitting to the right of the frame's
//!   right rule is *outside* the table and must fall back to the prose flow;
//! * a large vertical gap inside the frame (an empty ruled area, e.g. the
//!   white space before a totals block) does not end the table — rows continue
//!   until the frame's own bottom;
//! * a multi-line header cell is joined per column and emitted as one header
//!   row, never as loose headings;
//! * two numbers separated by a drawn column rule are never joined into one
//!   cell.
//!
//! This model derives its column boundaries directly from the drawn rules,
//! reusing the ledger's column-assignment primitive (`ledger::column_of`), and
//! takes the band from the *interior* rules only, so the outer border's
//! full-page extent cannot annex rows below the table.

mod detect;
mod model;
mod rows;
mod strip;
use model::{build_frame_table, frame_is_cell_grid, hit_folds_values_only, is_grid_frame};
use detect::find_frames;
use rows::build_row;
use strip::{classify_excluded_strip, StripKind};

use super::rulers::{line_words, TableHit};
use crate::layout::glyph_stream::Span;
use crate::models::{CELL_LINE_BREAK, CELL_LINE_BREAK_PENDING};

/// Maximum leading lines joined into the header band.
const MAX_HEADER_LINES: usize = 3;
/// A leading line must sit within this many text sizes of the line below it to
/// belong to the same header band (the same 2.2×size pitch the ruler scanner
/// uses for an adjacent row).
const HEADER_GAP_SIZE_MULT: f64 = 2.2;
/// A header cell holds at most this many words.
const HEADER_MAX_CELL_WORDS: usize = 5;
/// A header line must populate at least this many frame columns.
const MIN_HEADER_COLS: usize = 2;
/// A frame table needs at least this many in-band lines (header + data).
const MIN_FRAME_ROWS: usize = 2;
/// ... at least this many rows below the header ...
const MIN_BODY_ROWS: usize = 1;
/// ... of which at least this many must populate two columns.
const MIN_TABULAR_ROWS: usize = 1;
/// Word-centre slack at the frame's outer rule.
const EDGE_EPS_PT: f64 = 0.5;
/// A frame must cover at least this fraction of an existing hit's row band
/// before it may rebuild that hit.
const FRAME_HIT_COVERAGE: f64 = 0.5;
/// A frame must span at least this fraction of an existing hit's width. The
/// shortfall is normally a sidebar outside the right rule (the intended
/// exclusion); a frame covering much less than the hit would cut off a real
/// content column and push it out to prose.
const FRAME_MIN_WIDTH_FRAC: f64 = 0.85;
/// A frame that re-cuts away an excluded *prose* strip may be narrower, but it
/// must still enclose most of the table: a frame covering less than this would
/// push whole table rows (labels and values) out as loose prose.
const FRAME_MIN_WIDTH_FRAC_PROSE: f64 = 0.6;
/// A frame wider than this many columns is a dense layout grid, not the
/// header-bounded table this model targets.
const MAX_FRAME_COLS: usize = 12;
/// A frame that no generic hit covers may still build its own table, but only
/// when it is a substantial grid: at least this many columns. Narrow frames are
/// deliberately excluded — a producer drawing a box around a few columns of
/// prose would otherwise be turned into a table, and every ordinary table is
/// already found by the generic passes, so creation is needed only for the wide
/// drawn grids (section tables, multi-column forms).
const MIN_GRID_COLS: usize = 7;
/// ... at least this many in-band lines ...
const MIN_GRID_ROWS: usize = 4;
/// ... and at least this many body rows that populate two columns.
const MIN_GRID_TABULAR_ROWS: usize = 3;

/// Diagnostic tracer: prints when TABLE_TRACE env is set (same switch as the
/// ruler scanner's trace). The message is built lazily so an untraced run pays
/// nothing for the per-rule formatting.
fn t(msg: impl FnOnce() -> String) {
    if std::env::var("TABLE_TRACE").is_ok() {
        eprintln!("[rf] {}", msg());
    }
}

/// A candidate frame: its ascending column boundaries and the y-band the
/// interior rules agree on.
struct Frame {
    boundaries: Vec<f64>,
    lo: f64,
    hi: f64,
}

impl Frame {
    fn ncols(&self) -> usize {
        self.boundaries.len() - 1
    }
}

/// Largest font size on a visual line (>= 1.0), used to judge whether two
/// words are separated by a word space or by a column gutter.
fn line_size(line: &[Span]) -> f64 {
    line.iter().map(|s| s.size).fold(0.0, f64::max).max(1.0)
}

/// The y-band covered by a hit's own lines.
fn hit_band(lines: &[Vec<Span>], hit: &TableHit) -> (f64, f64) {
    let mut lo = f64::INFINITY;
    let mut hi = f64::NEG_INFINITY;
    for line in &lines[hit.start.min(lines.len().saturating_sub(1))..=hit.end.min(lines.len().saturating_sub(1))] {
        for sp in line {
            lo = lo.min(sp.y);
            hi = hi.max(sp.y);
        }
    }
    (lo, hi)
}

/// Does `frame` genuinely enclose the table `hit`? The frame's band must cover
/// the hit's rows, and its left rule must not cut into the hit's content: the
/// failure this guards is a spurious "frame" whose left edge sits in the middle
/// of a wider table, which would silently drop every word to its left.
fn frame_covers_hit(lines: &[Vec<Span>], frame: &Frame, hit: &TableHit) -> bool {
    let (hlo, hhi) = hit_band(lines, hit);
    if !hlo.is_finite() || hhi < hlo {
        return false;
    }
    let overlap = frame.hi.min(hhi) - frame.lo.max(hlo);
    if overlap < FRAME_HIT_COVERAGE * (hhi - hlo) {
        t(|| format!("cover reject: overlap {overlap:.1} vs {}", FRAME_HIT_COVERAGE * (hhi - hlo)));
        return false;
    }
    let left = frame.boundaries[0];
    let right = frame.boundaries[frame.ncols()];
    let hit_width = (hit.bbox.x1 - hit.bbox.x0).max(0.0);
    // The frame must cover most of the hit and not chop off its left content.
    // When either holds less, the strip the frame excludes is only allowed to
    // become prose if it actually reads as a text column; otherwise the frame
    // would cut a real column out of the table. An excluded *prose* strip is
    // recovered by the renderer (`spans_outside`), so no word is dropped.
    let left_ok = left <= hit.bbox.x0 + EDGE_EPS_PT;
    let width_ok = (right - left) >= FRAME_MIN_WIDTH_FRAC * hit_width;
    if left_ok && width_ok {
        return true;
    }
    match classify_excluded_strip(lines, frame) {
        // A prose strip may be excluded, but the frame must still enclose most
        // of the table; a much narrower frame would push whole rows out as
        // loose prose. An empty strip loses nothing regardless of width.
        StripKind::Prose if (right - left) >= FRAME_MIN_WIDTH_FRAC_PROSE * hit_width => {
            t(|| {
                format!(
                    "cover: prose strip allows left_ok={left_ok} width_ok={width_ok} ({:.1} vs {:.1})",
                    right - left,
                    hit_width
                )
            });
            true
        }
        StripKind::Empty => {
            t(|| {
                format!(
                    "cover: empty strip allows left_ok={left_ok} width_ok={width_ok} ({:.1} vs {:.1})",
                    right - left,
                    hit_width
                )
            });
            true
        }
        StripKind::Prose | StripKind::Other => {
            t(|| format!("cover reject: left_ok={left_ok} width_ok={width_ok} and strip not usable"));
            false
        }
    }
}

/// The column count a generic hit already shows.
fn hit_ncols(hit: &TableHit) -> usize {
    hit.rows.iter().map(|r| r.len()).max().unwrap_or(0)
}

/// Does the hit already fold a wrapped cell, i.e. carry an in-cell line break?
/// The re-cut builds one row per visual line; it cannot reproduce the generic
/// pass's continuation fold, so a folded table must be left to the pass that
/// produced it (bank/telecom statement descriptions wrap over several lines and
/// used to stay `<br>`-joined in one row). The generic ruler pass emits the
/// literal `<br>`; the ledger uses the deferred sentinel — both count.
fn hit_has_continuation_joins(hit: &TableHit) -> bool {
    hit.rows
        .iter()
        .flatten()
        .any(|c| c.contains(CELL_LINE_BREAK) || c.contains(CELL_LINE_BREAK_PENDING))
}

/// Would re-cutting `hit` split a row the generic pass folded? A fold joins two
/// visual lines into one logical row; the check is that the hit shows fewer rows
/// than the in-frame visual lines it spans. Folds join with `<br>` *or* with a
/// plain space (a cell ending in `/` or `-`), so the row count — not just the
/// in-cell separator — is the reliable test.
fn hit_folds_lines(lines: &[Vec<Span>], frame: &Frame, hit: &TableHit) -> bool {
    if hit_has_continuation_joins(hit) {
        return true;
    }
    let end = hit.end.min(lines.len().saturating_sub(1));
    let mut visual_lines = 0usize;
    for line in &lines[hit.start.min(end)..=end] {
        if line.is_empty() {
            continue;
        }
        let row = build_row(&line_words(line), line_size(line), &frame.boundaries);
        if row.iter().any(|c| !c.trim().is_empty()) {
            visual_lines += 1;
        }
    }
    hit.rows.len() < visual_lines
}

/// Is each frame one of a side-by-side pair (overlapping band, disjoint x)?
/// A sibling is what lets a wide generic fragment "cover" a half-grid: the two
/// drawn grids each rebuild one half. A lone long-rule frame that merely spans
/// stacked statement fragments has no sibling and must not re-columnize them.
fn side_by_side_flags(frames: &[Frame]) -> Vec<bool> {
    frames
        .iter()
        .enumerate()
        .map(|(i, f)| {
            frames.iter().enumerate().any(|(j, g)| {
                if i == j {
                    return false;
                }
                let band_overlap = f.lo <= g.hi && g.lo <= f.hi;
                let x_disjoint = f.boundaries[0] >= g.boundaries[g.ncols()] - EDGE_EPS_PT
                    || g.boundaries[0] >= f.boundaries[f.ncols()] - EDGE_EPS_PT;
                band_overlap && x_disjoint
            })
        })
        .collect()
}

/// Rebuild each drawn-rule frame that genuinely encloses an existing generic
/// hit, replacing that hit with the rule-column table. A frame that covers no
/// hit is left alone unless it is a substantial grid (see `is_grid_frame`).
///
/// The drawn rules fix the boundaries, exclude a sidebar past the right rule,
/// extend the rows across an empty ruled gap, and join a header the text-ruler
/// pass left above the grid. A frame with more columns than the hit is a
/// re-columnization: it is allowed for a dense cell grid and for one half of a
/// side-by-side pair, but refused for a lone long-rule frame over stacked
/// statement fragments (which would annex neighbouring boxes and rows).
pub fn apply_ruled_frame_model(
    lines: &[Vec<Span>],
    hits: Vec<TableHit>,
    vertical_rules: &[(f64, f64, f64)],
    cell_rects: &[(f64, f64, f64, f64)],
) -> Vec<TableHit> {
    // A grid whose producer draws every cell as its own bordered rectangle
    // states its columns through those stacked edges. They are folded into the
    // rule set here; a lone box contributes two short edges that cannot seed or
    // join a frame (see `find_frames`'s length/coverage gates).
    let mut rules: Vec<(f64, f64, f64)> = vertical_rules.to_vec();
    for &(x0, y0, x1, y1) in cell_rects {
        rules.push((x0, y0, y1));
        rules.push((x1, y0, y1));
    }
    let frames = find_frames(lines, &rules);
    if frames.is_empty() {
        return hits;
    }
    let side_by_side = side_by_side_flags(&frames);
    t(|| {
        format!(
            "hits_in={:?}",
            hits.iter()
                .map(|h| (
                    h.start,
                    h.end,
                    hit_ncols(h),
                    h.bbox.x0.round(),
                    h.bbox.x1.round(),
                    h.bbox.y0.round(),
                    h.bbox.y1.round()
                ))
                .collect::<Vec<_>>()
        )
    });
    let mut out: Vec<TableHit> = hits.clone();
    let mut added: Vec<TableHit> = Vec::new();
    for (fi, frame) in frames.iter().enumerate() {
        if frame.ncols() > MAX_FRAME_COLS {
            continue;
        }
        let covered: Vec<&TableHit> = hits.iter().filter(|h| frame_covers_hit(lines, frame, h)).collect();
        // The drawn rules are the authority on columns.
        //  * Exactly one covered hit: re-cut that region. More columns than the
        //    generic word-gap clustering found is the whole point; the same count
        //    is a straight re-cut. Fewer columns is allowed only when the
        //    excluded strip is a genuine prose column — an excluded value/label
        //    strip would merge real cells, so the generic table is kept.
        //  * Several covered hits: the generic pass split one drawn grid along
        //    word gaps, so each frame rebuilds its own half (side by side).
        //  * None: only a substantial grid builds a table on its own, never a
        //    box or a prose strip.
        let (fx0, fx1) = (frame.boundaries[0], frame.boundaries[frame.ncols()]);
        if covered.is_empty() {
            if !is_grid_frame(lines, frame) {
                t(|| format!("skip: frame {}cols covered=0", frame.ncols()));
                continue;
            }
        } else {
            // A frame with fewer columns than the widest hit it covers may only
            // shrink when the excluded strip is a genuine prose column; an
            // excluded value/label strip would merge real cells.
            let widest = covered.iter().map(|h| hit_ncols(h)).max().unwrap_or(0);
            if frame.ncols() < widest && classify_excluded_strip(lines, frame) != StripKind::Prose {
                t(|| format!("skip: frame {}cols fewer than hit {widest}", frame.ncols()));
                continue;
            }
            // A frame with *more* columns than every hit it covers re-states the
            // columns of the whole region. That is the intended fix only for a
            // grid whose producer boxes each cell (a dense cell grid, where the
            // generic pass folded values of different drawn columns together);
            // a long-rule frame that merely crosses several stacked statement
            // fragments must keep the generic/ledger row structure, or every
            // continuation line becomes its own row (F0402) and repeated headers
            // are annexed into the body (F0058).
            if frame.ncols() > widest
                && !frame_is_cell_grid(frame, cell_rects)
                && !side_by_side[fi]
            {
                t(|| format!("skip: frame {}cols wider than hit {widest} (not a cell grid)", frame.ncols()));
                continue;
            }
            // A table the generic pass already folded (wrapped description
            // cells) keeps its row structure; the line-per-row re-cut would
            // split every continuation into its own row. The one exception is a
            // dense drawn cell grid whose columns are *more numerous* than a
            // hit's and whose fold joins bare values: there the generic pass
            // folded values of different drawn columns into one cell, and the
            // drawn rules must override it. A prose fold always keeps the guard.
            let folds = covered.iter().any(|h| {
                let value_override = frame_is_cell_grid(frame, cell_rects)
                    && frame.ncols() > hit_ncols(h)
                    && hit_folds_values_only(h);
                !value_override && hit_folds_lines(lines, frame, h)
            });
            if folds {
                t(|| "skip: hit folds lines".to_string());
                continue;
            }
        }
        let Some(hit) = build_frame_table(lines, frame) else {
            t(|| "skip: build_frame_table returned None".to_string());
            continue;
        };
        // A single re-cut must not start below the hit it replaces (its header
        // would fall outside the new range) or end above it.
        if covered.len() == 1 {
            let h = covered[0];
            if hit.start > h.start || hit.end < h.end {
                t(|| "skip: re-cut would drop the hit's leading/trailing rows".to_string());
                continue;
            }
        }
        // The frame supersedes every generic fragment whose lines and x-band it
        // overlaps: keeping one would double a region the drawn rules already
        // rebuilt (and `group_tables` would then drop one arbitrarily).
        out.retain(|h| {
            let x_overlap = h.bbox.x0 < fx1 - EDGE_EPS_PT && h.bbox.x1 > fx0 + EDGE_EPS_PT;
            let line_overlap = h.start <= hit.end && h.end >= hit.start;
            !(x_overlap && line_overlap)
        });
        added.push(hit);
    }
    out.extend(added);
    out.sort_by(|a, b| a.start.cmp(&b.start));
    out
}

#[cfg(test)]
#[path = "ruled_frame/tests.rs"]
mod tests;
