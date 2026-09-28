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
use detect::find_frames;

use super::ledger::column_of;
use super::rulers::{line_words, TableHit, WordTok};
use crate::layout::glyph_stream::Span;
use crate::models::{BoundingBox, CELL_LINE_BREAK, CELL_LINE_BREAK_PENDING};

/// Maximum leading lines joined into the header band.
const MAX_HEADER_LINES: usize = 2;
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
/// A frame wider than this many columns is a dense layout grid, not the
/// header-bounded table this model targets.
const MAX_FRAME_COLS: usize = 12;

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

/// Is `cell` a bare value (number / amount / percent) rather than a label?
fn cell_is_value(cell: &str) -> bool {
    let t = cell
        .trim()
        .trim_matches(|c: char| matches!(c, '€' | '$' | '£' | '\u{00a0}'));
    if t.is_empty() {
        return false;
    }
    let mut has_digit = false;
    for ch in t.chars() {
        if ch.is_ascii_digit() {
            has_digit = true;
        } else if !matches!(
            ch,
            '.' | ',' | '-' | '+' | '%' | '/' | '\'' | ' ' | '\u{2028}'
        ) {
            return false;
        }
    }
    has_digit
}

/// Assign a line's words to the frame's columns. A word that lies *entirely*
/// outside the frame's outer rules belongs to the surrounding flow (a sidebar,
/// the page margin) and is dropped; a word that straddles an outer rule is kept
/// in the nearest column, because `render_with_tables` treats any span
/// overlapping the table's bbox as table content — dropping it here would
/// silently delete it from both the table and the recovered side flow.
fn build_row(words: &[WordTok], boundaries: &[f64]) -> Vec<String> {
    let ncols = boundaries.len() - 1;
    let left = boundaries[0];
    let right = boundaries[boundaries.len() - 1];
    // `column_of` takes the *interior* boundaries only; the outer rules are
    // explicit edges.
    let interior = &boundaries[1..boundaries.len() - 1];
    let mut cells = vec![String::new(); ncols];
    for w in words {
        if w.x1 <= left + EDGE_EPS_PT || w.x0 >= right - EDGE_EPS_PT {
            continue;
        }
        // A producer frequently draws two right-aligned numbers in adjacent
        // spans whose advances abut, so `line_words` folds them into one token
        // ("##.#### ####.##"). The drawn rule between them is the authority: a
        // token crossing an interior rule is split at its whitespace and each
        // part bucketed by its own interpolated centre.
        if interior
            .iter()
            .any(|&b| b > w.x0 + EDGE_EPS_PT && b < w.x1 - EDGE_EPS_PT)
            && w.text.contains(' ')
        {
            for (col, part) in split_crossing_token(w, interior) {
                push_cell(&mut cells, col, &part);
            }
            continue;
        }
        let center = 0.5 * (w.x0 + w.x1);
        let c = column_of(center.clamp(left, right), interior).min(ncols - 1);
        push_cell(&mut cells, c, w.text.as_str());
    }
    cells
}

/// Split a word token that crosses an interior rule at its whitespace, assigning
/// each whitespace-separated part a column from its x-position interpolated
/// linearly across the token (digit runs are near-uniform width, so the split
/// lands on the rule). A part past the frame's right rule is dropped.
fn split_crossing_token(w: &WordTok, interior: &[f64]) -> Vec<(usize, String)> {
    let text = w.text.as_str();
    let char_count = text.chars().count().max(1) as f64;
    let total = (w.x1 - w.x0).max(0.1);
    let mut out = Vec::new();
    let mut search_from = 0usize;
    for part in text.split_whitespace() {
        let Some(idx) = text[search_from..].find(part).map(|i| i + search_from) else {
            search_from += part.len();
            continue;
        };
        let c0 = text[..idx].chars().count() as f64;
        let c1 = c0 + part.chars().count() as f64;
        let x0 = w.x0 + total * (c0 / char_count);
        let x1 = w.x0 + total * (c1 / char_count);
        search_from = idx + part.len();
        let col = column_of(0.5 * (x0 + x1), interior);
        out.push((col, part.to_string()));
    }
    out
}

/// Append `text` to `cells[col]`, space-separating multiple words in one cell.
fn push_cell(cells: &mut [String], col: usize, text: &str) {
    if col >= cells.len() || text.trim().is_empty() {
        return;
    }
    if !cells[col].is_empty() {
        cells[col].push(' ');
    }
    cells[col].push_str(text.trim());
}

/// Number of non-empty cells.
fn populated(cells: &[String]) -> usize {
    cells.iter().filter(|c| !c.trim().is_empty()).count()
}

/// Is `row` the shape of a header band line: a short label in several columns
/// and no bare value cell?
fn is_header_like(row: &[String]) -> bool {
    populated(row) >= MIN_HEADER_COLS
        && !row.iter().any(|c| cell_is_value(c))
        && row
            .iter()
            .all(|c| c.is_empty() || c.split_whitespace().count() <= HEADER_MAX_CELL_WORDS)
}

/// Build one table from a frame, or `None` when the frame carries no table.
///
/// `hit` is the existing table the frame re-cuts. Rows above the hit's own
/// start are not absorbed (an address block sitting in the frame's upper band
/// is not table content); only a short label-like header immediately above the
/// hit is annexed.
fn build_frame_table(lines: &[Vec<Span>], frame: &Frame, hit: &TableHit) -> Option<TableHit> {
    let ncols = frame.ncols();
    let boundaries = &frame.boundaries;

    // Every in-band line at or below the hit's start carrying an in-frame word.
    let mut data: Vec<(usize, Vec<String>)> = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if i < hit.start || line.is_empty() {
            continue;
        }
        let y = line[0].y;
        if !(frame.lo..=frame.hi).contains(&y) {
            continue;
        }
        let row = build_row(&line_words(line), boundaries);
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
                let row = build_row(&line_words(line), boundaries);
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

    let start = header
        .iter()
        .map(|(i, _)| *i)
        .min()
        .unwrap_or_else(|| body[0].0);
    let end = body.last()?.0;

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
        return false;
    }
    let left = frame.boundaries[0];
    let right = frame.boundaries[frame.ncols()];
    // The frame must not chop off the hit's left content ...
    if left > hit.bbox.x0 + EDGE_EPS_PT {
        return false;
    }
    // ... and must account for most of its width (the rest may be a sidebar
    // outside the right rule, which the frame deliberately excludes).
    let hit_width = (hit.bbox.x1 - hit.bbox.x0).max(0.0);
    (right - left) >= FRAME_MIN_WIDTH_FRAC * hit_width
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
        let row = build_row(&line_words(line), &frame.boundaries);
        if row.iter().any(|c| !c.trim().is_empty()) {
            visual_lines += 1;
        }
    }
    hit.rows.len() < visual_lines
}

/// Rebuild each drawn-rule frame that genuinely encloses an existing generic
/// hit, replacing that hit with the rule-column table. A frame that covers no
/// hit is left alone: this pass refines a table the geometry already found, it
/// never turns prose into a table.
///
/// The frame is only *re-cut* (same column count as the hit): the drawn rules
/// then fix the boundaries, exclude a sidebar past the right rule, extend the
/// rows across an empty ruled gap, and join a header the text-ruler pass left
/// above the grid. A frame with more columns than the hit would be a
/// re-columnization of the whole page region, which on multi-column statements
/// annexes a neighbouring box and drops real rows; that broader re-columnization
/// is deliberately out of scope here.
pub fn apply_ruled_frame_model(
    lines: &[Vec<Span>],
    hits: Vec<TableHit>,
    vertical_rules: &[(f64, f64, f64)],
) -> Vec<TableHit> {
    let frames = find_frames(lines, vertical_rules);
    if frames.is_empty() {
        return hits;
    }
    let mut out: Vec<TableHit> = hits.clone();
    let mut added: Vec<TableHit> = Vec::new();
    for frame in &frames {
        if frame.ncols() > MAX_FRAME_COLS {
            continue;
        }
        let covered: Vec<&TableHit> = hits.iter().filter(|h| frame_covers_hit(lines, frame, h)).collect();
        // Exactly one existing table must be enclosed, with the frame's own
        // column count. A frame that would span or merge several separate grids
        // (a multi-column statement page, a dense layout) is a false positive:
        // re-columnizing it annexes neighbouring boxes and drops real rows.
        if covered.len() != 1 || hit_ncols(covered[0]) != frame.ncols() {
            continue;
        }
        // A table the generic pass already folded (wrapped description cells)
        // keeps its row structure; the line-per-row re-cut would split every
        // continuation into its own row.
        if hit_folds_lines(lines, frame, covered[0]) {
            continue;
        }
        let Some(hit) = build_frame_table(lines, frame, covered[0]) else {
            continue;
        };
        // The rebuilt region must not overlap any other hit: replacing it would
        // delete that neighbour's table boundary without folding its rows in.
        let touching = hits
            .iter()
            .filter(|h| h.start <= hit.end && h.end >= hit.start)
            .count();
        if touching != 1 {
            continue;
        }
        out.retain(|h| h.end < hit.start || h.start > hit.end);
        added.push(hit);
    }
    out.extend(added);
    out.sort_by(|a, b| a.start.cmp(&b.start));
    out
}

#[cfg(test)]
#[path = "ruled_frame/tests.rs"]
mod tests;
