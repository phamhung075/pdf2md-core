// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Side-by-side table grouping and side-content recovery.
//!
//! Two drawn frames side by side (left table, right table) share visual lines
//! but occupy disjoint x-ranges. [`group_tables`] clusters such hits so
//! `render_with_tables` emits them left to right, and [`spans_outside_active`]
//! recovers the words each table does not own.

use super::TableHit;
use crate::layout::glyph_stream::Span;

/// Collapse duplicate / overlapping table candidates for a page into a
/// non-overlapping, line-disjoint list sorted by `start`.
///
/// `find_tables` ∪ `find_gap_tables` are produced independently and are only
/// sorted by `start`; nothing de-overlaps them. Nested, partially-overlapping,
/// or same-start hits therefore stall the render loop (it advances `i` past the
/// first table's `end` but never advances `t`), silently dropping **every**
/// remaining table on that page as plain text. Keep a hit only when its `start`
/// is at or beyond the first line not already claimed by a kept table, so each
/// underlying region is emitted (as a GFM table) at most once.
///
/// Superseded by [`group_tables`] (which additionally keeps side-by-side
/// frames); retained as the reference semantics its unit test pins.
#[cfg(test)]
pub(super) fn de_overlap_tables(tables: &[TableHit]) -> Vec<TableHit> {
    let mut sorted: Vec<TableHit> = tables.to_vec();
    sorted.sort_by(|a, b| a.start.cmp(&b.start).then(a.end.cmp(&b.end)));
    let mut out: Vec<TableHit> = Vec::new();
    let mut covered = 0usize;
    for h in sorted {
        if h.start >= covered {
            let end = h.end;
            out.push(h);
            covered = end + 1;
        }
    }
    out
}

/// Spans of `line` at index `idx` outside every *active* table (one whose own
/// line range contains `idx`). A side table shares its visual lines with a
/// neighboring column's prose: everything outside its own x-band must still be
/// rendered, or that column's text vanishes. A side-by-side pair's tables have
/// overlapping line ranges, so a line that only one of them spans must not have
/// its words claimed by the other's x-band.
pub(super) fn spans_outside_active(line: &[Span], idx: usize, active: &[&TableHit]) -> Vec<Span> {
    line.iter()
        .filter(|s| {
            let end = s.x + s.advance;
            !active.iter().any(|h| {
                h.start <= idx
                    && idx <= h.end
                    && s.x < h.bbox.x1 - 0.5
                    && end > h.bbox.x0 + 0.5
            })
        })
        .cloned()
        .collect()
}

/// A set of table hits emitted together at one reading position. Two frames
/// drawn side by side (left table, right table) share visual lines but occupy
/// disjoint x-ranges; they are one group and are emitted left to right.
pub(super) struct TableGroup {
    pub(super) hits: Vec<TableHit>,
    pub(super) start: usize,
    pub(super) end: usize,
}

/// Cluster line-overlapping, x-disjoint hits into side-by-side groups, then
/// drop groups that share a line with an earlier (kept) group, exactly as
/// [`de_overlap_tables`] does for single hits. A group of one is the ordinary
/// case and renders byte-identically to before.
pub(super) fn group_tables(tables: &[TableHit]) -> Vec<TableGroup> {
    let mut sorted = tables.to_vec();
    sorted.sort_by(|a, b| {
        a.start
            .cmp(&b.start)
            .then(a.bbox.x0.partial_cmp(&b.bbox.x0).unwrap_or(std::cmp::Ordering::Equal))
    });
    let mut groups: Vec<TableGroup> = Vec::new();
    for h in sorted {
        let mut placed = false;
        for g in groups.iter_mut().rev() {
            let line_overlap = h.start <= g.end && h.end >= g.start;
            let x_disjoint = g
                .hits
                .iter()
                .all(|m| h.bbox.x1 <= m.bbox.x0 || h.bbox.x0 >= m.bbox.x1);
            if line_overlap && x_disjoint {
                g.start = g.start.min(h.start);
                g.end = g.end.max(h.end);
                g.hits.push(h.clone());
                placed = true;
                break;
            }
        }
        if !placed {
            groups.push(TableGroup {
                start: h.start,
                end: h.end,
                hits: vec![h],
            });
        }
    }
    groups.sort_by(|a, b| a.start.cmp(&b.start).then(a.end.cmp(&b.end)));
    let mut out: Vec<TableGroup> = Vec::new();
    let mut covered = 0usize;
    for g in groups {
        if g.start >= covered {
            covered = g.end + 1;
            out.push(g);
        }
    }
    out
}
