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
use crate::models::CanvasTable;

/// Render a page's visual lines to text, replacing detected table blocks with
/// GFM pipe tables. Non-table lines use the exact same rules as `render_cluster`.
pub fn render_with_tables(lines: &[Vec<Span>], tables: &[TableHit]) -> String {
    let mut out = String::new();
    let mut prev_line_y: Option<f64> = None;
    let mut t = 0usize;

    let push_line = |out: &mut String, line: &[Span], prev_line_y: &mut Option<f64>| {
        let size = line[0].size.max(0.1);
        if let Some(py) = *prev_line_y {
            if py - line[0].y > 2.0 * size {
                out.push('\n');
            }
        }
        let mut line_text = String::new();
        let mut prev_x: Option<f64> = None;
        let mut prev_advance = 0.0f64;
        for span in line {
            if span.text.is_empty() {
                continue;
            }
            if let Some(px) = prev_x {
                let gap = span.x - px;
                let space_adv = 0.25 * size;
                if span.text != " " {
                    if gap > 2.5 * size {
                        if !line_text.is_empty() && !line_text.ends_with('\n') {
                            line_text.push('\n');
                        }
                    } else if gap - prev_advance > 0.65 * space_adv {
                        if !line_text.is_empty()
                            && !line_text.ends_with(' ')
                            && !line_text.ends_with('\n')
                        {
                            line_text.push(' ');
                        }
                    }
                }
            }
            if span.text == " " {
                if !line_text.is_empty() && !line_text.ends_with(' ') && !line_text.ends_with('\n')
                {
                    line_text.push(' ');
                }
            } else {
                line_text.push_str(&span.text);
            }
            prev_x = Some(span.x);
            prev_advance = span.advance;
        }
        out.push_str(line_text.trim_end());
        out.push('\n');
        *prev_line_y = Some(line[0].y);
    };

    let mut i = 0usize;
    while i < lines.len() {
        // A table block starting exactly at this line: emit GFM instead of
        // the plain rows.
        if t < tables.len() && tables[t].start == i {
            let hit = &tables[t];
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
        // Defensive: a line strictly inside a table range (should not happen
        // because tables are emitted atomically above).
        if t < tables.len() && i > tables[t].start && i <= tables[t].end {
            i += 1;
            continue;
        }
        push_line(&mut out, &lines[i], &mut prev_line_y);
        i += 1;
    }

    out.trim_end().to_string()
}
