// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! 2D Spatial & Structural Table Reconstruction (Bordered & Borderless).

pub mod bordered;
pub mod borderless;
pub mod consolidation;
pub mod rulers;
pub mod validation;

pub use bordered::{extract_bordered_tables, extract_tables, recover_borderless_tables, recover_tables_with_grid};
pub use borderless::extract_borderless_tables;
pub use rulers::{find_gap_tables, find_tables, scan_aligned_grids, table_rulers, RowInfo, TableHit, WordTok};

use crate::layout::glyph_stream::Span;
use crate::layout::reading_order::render_spans;
use crate::models::CanvasTable;

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
fn de_overlap_tables(tables: &[TableHit]) -> Vec<TableHit> {
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

/// Render a page's visual lines to text, replacing detected table blocks with
/// GFM pipe tables. Non-table lines use the exact same rules as `render_cluster`.
pub fn render_with_tables(lines: &[Vec<Span>], tables: &[TableHit]) -> String {
    let mut out = String::new();
    let mut prev_line_y: Option<f64> = None;
    let tables = de_overlap_tables(tables);
    let mut t = 0usize;

    let push_line = |out: &mut String, line: &[Span], prev_line_y: &mut Option<f64>| {
        let size = line[0].size.max(0.1);
        if let Some(py) = *prev_line_y {
            if py - line[0].y > 2.0 * size {
                out.push('\n');
            }
        }
        out.push_str(render_spans(line).trim_end());
        out.push('\n');
        *prev_line_y = Some(line[0].y);
    };

    let mut i = 0usize;
    while i < lines.len() {
        // Advance past any table whose region has already been consumed (e.g. a
        // nested/overlapping candidate de-overlapped above, or a table that a
        // previous atomic emission jumped beyond).
        while t < tables.len() && tables[t].start < i && tables[t].end < i {
            t += 1;
        }
        // A table block starting exactly at this line: emit GFM instead of
        // the plain rows.
        if t < tables.len() && tables[t].start == i {
            let hit = &tables[t];
            if hit.end >= lines.len() {
                // Defensive: never index past the last line.
                t += 1;
                continue;
            }
            // Blank line before the table (markdown block separation).
            if !out.is_empty() && !out.ends_with("\n\n") {
                out.push('\n');
            }
            let table = CanvasTable::new(
                hit.rows.clone(),
                hit.bbox.clone(),
            );
            out.push_str(&table.to_markdown());
            if !out.ends_with('\n') {
                out.push('\n');
            }
            out.push('\n'); // blank line after the table
            prev_line_y = Some(lines[hit.end][0].y);
            t += 1;
            i = hit.end + 1; // skip the table's own lines
            continue;
        }
        push_line(&mut out, &lines[i], &mut prev_line_y);
        i += 1;
    }

    out.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::BoundingBox;

    fn span(text: &str, x: f64, y: f64, size: f64, advance: f64) -> Span {
        Span {
            text: text.to_string(),
            x,
            y,
            size,
            advance,
            is_bold: false,
            is_italic: false,
            is_underline: false,
            is_vertical: false,
        }
    }

    fn line(text: &str, y: f64) -> Vec<Span> {
        vec![span(text, 50.0, y, 10.0, 8.0)]
    }

    fn table(start: usize, end: usize) -> TableHit {
        TableHit {
            start,
            end,
            rows: vec![
                vec!["h1".to_string(), "h2".to_string()],
                vec!["a".to_string(), "b".to_string()],
                vec!["c".to_string(), "d".to_string()],
            ],
            bbox: BoundingBox::new(10.0, 10.0, 300.0, 600.0),
        }
    }

    /// Count GFM pipe-table blocks in the rendered text by counting separator
    /// rows (`| --- | --- |`), which appear exactly once per emitted table.
    fn table_blocks(md: &str) -> usize {
        md.lines()
            .filter(|l| l.trim_start().starts_with('|') && l.contains("---"))
            .count()
    }

    #[test]
    fn render_with_tables_non_overlapping() {
        // Two disjoint tables followed by a prose line: both tables render.
        let lines = vec![
            line("r0", 800.0),
            line("r1", 780.0),
            line("r2", 760.0),
            line("r3", 740.0),
            line("r4", 720.0),
            line("r5", 700.0),
        ];
        // Table A occupies lines 0..=0; table B occupies lines 2..=2.
        let tables = vec![table(0, 0), table(2, 2)];
        let md = render_with_tables(&lines, &tables);
        assert_eq!(table_blocks(&md), 2, "both disjoint tables must render:\n{md}");
        // The prose lines in between remain.
        assert!(md.contains("r1"));
        assert!(md.contains("r3"));
        assert!(md.contains("r4"));
        assert!(md.contains("r5"));
    }

    #[test]
    fn render_with_tables_nested_hit_renders_outer_once() {
        let lines = vec![line("r0", 800.0), line("r1", 780.0), line("r2", 760.0)];
        // Outer [1..=2] contains inner [2..=2]: the outer is emitted, the inner
        // is a duplicate of the same region and must be dropped (not double).
        let tables = vec![table(1, 2), table(2, 2)];
        let md = render_with_tables(&lines, &tables);
        assert_eq!(table_blocks(&md), 1, "nested candidates collapse to one table:\n{md}");
    }

    #[test]
    fn render_with_tables_overlap_recovers_trailing_rows_as_text() {
        let lines = vec![
            line("r0", 800.0),
            line("r1", 780.0),
            line("r2", 760.0),
            line("r3", 740.0),
        ];
        // Overlapping candidates [1..=2] and [2..=3]: the first is emitted as a
        // table; the row the second claimed must not be silently lost.
        let tables = vec![table(1, 2), table(2, 3)];
        let md = render_with_tables(&lines, &tables);
        assert_eq!(table_blocks(&md), 1, "overlap collapses to the first table:\n{md}");
        assert!(
            md.contains("r3"),
            "trailing row of the overlapped candidate must survive as text:\n{md}"
        );
    }

    #[test]
    fn render_with_tables_same_start_renders_first() {
        let lines = vec![line("r0", 800.0), line("r1", 780.0), line("r2", 760.0)];
        // Two candidates sharing a start: the wider/earliest wins; duplicates
        // must not produce a second (staled) table.
        let tables = vec![table(0, 1), table(0, 2)];
        let md = render_with_tables(&lines, &tables);
        assert_eq!(table_blocks(&md), 1, "same-start candidates render once:\n{md}");
    }

    #[test]
    fn de_overlap_keeps_only_line_disjoint_hits() {
        let tables = vec![table(0, 2), table(2, 4), table(5, 5)];
        assert_eq!(
            de_overlap_tables(&tables).len(),
            2,
            "[0..=2] and [2..=4] share a line and must collapse; [5..=5] survives"
        );
    }
}
