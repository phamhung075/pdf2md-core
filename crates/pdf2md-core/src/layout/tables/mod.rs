// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! 2D Spatial & Structural Table Reconstruction (Bordered & Borderless).

pub mod bordered;
pub mod borderless;
pub mod consolidation;
pub mod key_value;
pub mod ledger;
pub mod ledger_columns;
pub mod ledger_rows;
pub mod ruled_frame;
pub mod rulers;
pub mod validation;

pub use bordered::{extract_bordered_tables, extract_tables, recover_borderless_tables, recover_tables_with_grid};
pub use borderless::extract_borderless_tables;
pub use key_value::append_key_value_boxes;
pub use ledger::{apply_ledger_model, apply_ledger_model_with_rules};
pub use ruled_frame::apply_ruled_frame_model;
pub use rulers::{find_gap_tables, find_tables, scan_aligned_grids, table_rulers, RowInfo, TableHit, WordTok};

mod groups;
use groups::{group_tables, spans_outside_active};
#[cfg(test)]
use groups::de_overlap_tables;

use crate::layout::glyph_stream::Span;
use crate::layout::reading_order::{
    body_size_for, detect_column_bands, is_bare_page_number_line, push_band_lines, ListRunState,
};
use crate::models::CanvasTable;

/// Baseline-to-baseline line pitch as a multiple of the body font size.
const LINE_PITCH_MULT: f64 = 1.2;
/// Neighboring content whose first line sits within this many line pitches of
/// the line above the table continues that prose (side-by-side column); content
/// starting further down is a separate sidebar emitted after the table.
const SIDE_CONTINUATION_PITCH_MULT: f64 = 3.0;
/// A block of neighboring content must have at least this many lines to count
/// as a sidebar eligible for the table-first order; a handful of side amounts
/// beside individual rows keeps the original order.
const MIN_SIDEBAR_LINES: usize = 5;

/// Render a page's visual lines to text, replacing detected table blocks with
/// GFM pipe tables. Non-table lines use the exact same rules as `render_cluster`.
///
/// Bare page-number footers are dropped from the non-table segments (as the
/// plain renderer already does); the body-size anchor is still estimated from
/// the *unfiltered* line list, so removing a footer cannot reclassify a heading.
pub fn render_with_tables(
    lines: &[Vec<Span>],
    tables: &[TableHit],
    page_height: f64,
    page_width: Option<f64>,
) -> String {
    let mut out = String::new();
    let mut prev_line_y: Option<f64> = None;
    let groups = group_tables(tables);
    let mut t = 0usize;
    let body_size = body_size_for(lines);
    let is_footer = |l: &Vec<Span>| is_bare_page_number_line(l, page_height, body_size);
    let mut list_state = ListRunState::default();

    let mut i = 0usize;
    while i < lines.len() {
        // Advance past any group whose region has already been consumed (e.g. a
        // nested/overlapping candidate de-overlapped above, or a group that a
        // previous atomic emission jumped beyond).
        while t < groups.len() && groups[t].start < i && groups[t].end < i {
            t += 1;
        }
        // A table block starting exactly at this line: emit GFM instead of
        // the plain rows.
        if t < groups.len() && groups[t].start == i {
            let group = &groups[t];
            if group.end >= lines.len() {
                // Defensive: never index past the last line.
                t += 1;
                continue;
            }
            // Emit left to right. A side-by-side pair shares visual lines; the
            // group is emitted at the leftmost member's start.
            let mut active: Vec<&TableHit> = group.hits.iter().collect();
            active.sort_by(|a, b| {
                a.bbox.x0
                    .partial_cmp(&b.bbox.x0)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            // A side table shares its visual lines with a neighboring column's
            // prose. When that neighbor runs beside the table's own first line
            // (a genuine side-by-side layout) it continues the prose above the
            // table, so it is emitted first as before. When it instead begins
            // *below* the table's top — a sidebar to the right whose first box
            // starts a few rows down — the table must be emitted first, at its
            // frame's position, or it is pushed below the sidebar.
            let side_content: Vec<Vec<Span>> = (i..=group.end)
                .map(|li| spans_outside_active(&lines[li], li, &active))
                .map(|l| if is_footer(&l) { Vec::new() } else { l })
                .collect();
            // Neighboring content that begins on or just below the table's first
            // line continues the prose above the table (a side-by-side column),
            // so it must keep the original order: paragraph first, table after.
            // A *substantial* block that starts several rows down is a separate
            // sidebar to the right; the table is emitted first, at its frame's
            // position. A couple of side amounts beside individual rows is not a
            // sidebar and keeps the original order.
            let first_side = side_content.iter().position(|l| !l.is_empty());
            let side_lines = side_content.iter().filter(|l| !l.is_empty()).count();
            let side_below = match first_side {
                // Content that begins on the table's own first line is a
                // side-by-side column, not a sidebar below it: the large gap
                // above the table must not be read as "starts several rows
                // down" (the line before a ruled table is often a heading or a
                // separate block far above).
                Some(off) if i > 0 && off > 0 => {
                    let prev_y = lines[i - 1].first().map(|s| s.y);
                    let side_y = lines[i + off].first().map(|s| s.y).or(prev_y);
                    match (prev_y, side_y) {
                        (Some(py), Some(sy)) => {
                            let pitch = (body_size * LINE_PITCH_MULT).max(1.0);
                            py - sy > SIDE_CONTINUATION_PITCH_MULT * pitch
                        }
                        _ => false,
                    }
                }
                _ => false,
            };
            let side_first = !(side_below && side_lines >= MIN_SIDEBAR_LINES);
            let had_side = side_lines > 0;
            let emit_side = |out: &mut String,
                             prev_line_y: &mut Option<f64>,
                             list_state: &mut ListRunState| {
                if had_side {
                    let bands = detect_column_bands(&side_content);
                    push_band_lines(out, &bands, prev_line_y, list_state, body_size, page_width);
                }
            };
            let emit_table = |out: &mut String, hit: &TableHit| {
                // Blank line before the table (markdown block separation).
                if !out.is_empty() && !out.ends_with("\n\n") {
                    out.push('\n');
                }
                let table = CanvasTable::new(hit.rows.clone(), hit.bbox.clone());
                out.push_str(&table.to_markdown());
                if !out.ends_with('\n') {
                    out.push('\n');
                }
                out.push('\n'); // blank line after the table
            };
            if side_first {
                emit_side(&mut out, &mut prev_line_y, &mut list_state);
                for hit in &active {
                    emit_table(&mut out, hit);
                }
                prev_line_y = Some(lines[group.end][0].y);
            } else {
                for hit in &active {
                    emit_table(&mut out, hit);
                }
                emit_side(&mut out, &mut prev_line_y, &mut list_state);
                // A sidebar emitted after the table is a separate block: the
                // prose that follows must not join onto its last line.
                prev_line_y = if had_side { None } else { Some(lines[group.end][0].y) };
            }
            list_state = ListRunState::default(); // a table interrupts any list run
            t += 1;
            i = group.end + 1; // skip the table's own lines
            continue;
        }
        // Recover column reading order for the contiguous run of non-table
        // lines up to the next table start (or end of page) as one unit,
        // instead of walking it strictly line-by-line: a page can carry a
        // small table (e.g. a 2-cell reference block) while its surrounding
        // header/footer still uses a two-column seller/buyer style layout
        // that a naive top-to-bottom walk would weave together.
        let seg_end = if t < groups.len() {
            groups[t].start.min(lines.len())
        } else {
            lines.len()
        };
        let seg = &lines[i..seg_end];
        // Only pay for a filtered copy when the segment actually carries a
        // footer; the common case renders the slice directly.
        let filtered;
        let seg = if seg.iter().any(is_footer) {
            filtered = seg
                .iter()
                .filter(|l| !is_footer(l))
                .cloned()
                .collect::<Vec<Vec<Span>>>();
            &filtered[..]
        } else {
            seg
        };
        let bands = detect_column_bands(seg);
        push_band_lines(&mut out, &bands, &mut prev_line_y, &mut list_state, body_size, page_width);
        i = seg_end;
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
            word_advance: advance,
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
        let md = render_with_tables(&lines, &tables, 792.0, None);
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
        let md = render_with_tables(&lines, &tables, 792.0, None);
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
        let md = render_with_tables(&lines, &tables, 792.0, None);
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
        let md = render_with_tables(&lines, &tables, 792.0, None);
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

    /// A side table shares its visual lines with the main column's prose. When
    /// the table is spliced, everything outside its own x-band must still be
    /// rendered, or the neighboring column's text vanishes from the page.
    #[test]
    fn render_with_tables_preserves_prose_beside_side_table() {
        let lines = vec![
            vec![
                span("left prose one", 40.0, 700.0, 10.0, 100.0),
                span("dim", 300.0, 700.0, 10.0, 20.0),
                span("4096", 360.0, 700.0, 10.0, 25.0),
            ],
            vec![
                span("left prose two", 40.0, 688.0, 10.0, 100.0),
                span("n_layers", 300.0, 688.0, 10.0, 45.0),
                span("32", 360.0, 688.0, 10.0, 12.0),
            ],
        ];
        let hit = TableHit {
            start: 0,
            end: 1,
            rows: vec![
                vec!["dim".to_string(), "4096".to_string()],
                vec!["n_layers".to_string(), "32".to_string()],
            ],
            bbox: BoundingBox::new(300.0, 680.0, 400.0, 710.0),
        };
        let md = render_with_tables(&lines, &[hit], 792.0, None);
        assert!(
            md.contains("left prose one") && md.contains("left prose two"),
            "prose beside the side table was dropped:\n{md}"
        );
        assert_eq!(table_blocks(&md), 1, "side table was not rendered:\n{md}");
    }

    #[test]
    fn bare_page_number_footer_is_dropped_beside_a_table() {
        // A page that carries a table must not leak its bare footer number as a
        // stray line (the plain renderer already drops it).
        let lines = vec![
            line("Intro prose", 700.0),
            line("molecule count", 690.0),
            line("2", 20.0),
        ];
        let tables = vec![table(1, 1)];
        let md = render_with_tables(&lines, &tables, 792.0, None);
        assert!(md.contains("Intro prose"), "intro lost:\n{md}");
        assert_eq!(table_blocks(&md), 1, "table not rendered:\n{md}");
        assert!(
            !md.lines().any(|l| l.trim() == "2"),
            "bare page number leaked into output:\n{md}"
        );
    }

    /// A side column that begins on the table's own first line is a side-by-side
    /// column, not a sidebar below it: a heading far above the table must not
    /// push its prose after the table (the frame re-cut's left strip).
    #[test]
    fn side_prose_at_the_table_top_is_emitted_before_it() {
        let mut lines = vec![line("heading high above", 780.0)];
        for i in 0..6 {
            let y = 700.0 - i as f64 * 12.0;
            lines.push(vec![
                span(&format!("left line {i}"), 40.0, y, 10.0, 60.0),
                span(&format!("c{i}"), 300.0, y, 10.0, 15.0),
                span(&format!("{i}"), 360.0, y, 10.0, 8.0),
            ]);
        }
        let rows: Vec<Vec<String>> = (0..6).map(|i| vec![format!("c{i}"), format!("{i}")]).collect();
        let hit = TableHit {
            start: 1,
            end: 6,
            rows,
            bbox: BoundingBox::new(300.0, 630.0, 380.0, 710.0),
        };
        let md = render_with_tables(&lines, &[hit], 792.0, None);
        let prose = md.find("left line 0").expect("side prose lost");
        let table = md.find("| c0").expect("table lost");
        assert!(prose < table, "side prose must be emitted before the table:\n{md}");
    }

    /// Two side-by-side frames share visual lines but occupy disjoint x-ranges:
    /// both must be emitted, left one first, instead of one collapsing the
    /// other by line overlap.
    #[test]
    fn render_with_tables_emits_side_by_side_frames_left_first() {
        let lines = vec![
            line("r0", 800.0),
            line("r1", 780.0),
            line("r2", 760.0),
            line("r3", 740.0),
        ];
        let left = TableHit {
            start: 0,
            end: 2,
            rows: vec![vec!["LEFT".to_string()]],
            bbox: BoundingBox::new(10.0, 700.0, 200.0, 810.0),
        };
        let right = TableHit {
            start: 1,
            end: 3,
            rows: vec![vec!["RIGHT".to_string()]],
            bbox: BoundingBox::new(300.0, 700.0, 500.0, 810.0),
        };
        let md = render_with_tables(&lines, &[left, right], 792.0, None);
        assert_eq!(table_blocks(&md), 2, "a side-by-side pair must emit both tables:\n{md}");
        let l = md.find("LEFT").expect("left table lost");
        let r = md.find("RIGHT").expect("right table lost");
        assert!(l < r, "the left frame must be emitted first:\n{md}");
    }
}
