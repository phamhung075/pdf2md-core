// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Cross-page table continuation.
//!
//! When a table ends page N and the same-header table starts page N+1, the two
//! are one logical table split by the page break. This pass emits them as one
//! Markdown table: the continuation rows are appended under the first table and
//! the repeated header/separator pair is dropped from the next page's chunk,
//! which then keeps only its non-table content (address/letterhead furniture and
//! footers). A chain continues across any number of pages whose first table
//! repeats the same header.
//!
//! The pass runs on the assembled per-page Markdown chunks, after the
//! document-level furniture pass and before the per-page reflow. Per-page
//! extraction results (the geometry blocks and their page numbers) are not
//! touched; only the emitted document text is rejoined.

/// One GFM table inside a page chunk.
struct TableSpan {
    /// Index of the header line.
    start: usize,
    /// Index of the last body line. Always `> start` (the separator sits between).
    end: usize,
}

/// Header + alignment separator: the two lines every rendered GFM table opens
/// with, before any body row.
const TABLE_PREFIX_LINES: usize = 2;

/// A table a later page's first table may continue.
struct OpenTable {
    /// Page whose chunk currently holds the open table.
    page: usize,
    /// Normalised header cells of the open table.
    header: Vec<String>,
    /// Index, in `pages[page]`, of the open table's last body line.
    last_row: usize,
}

/// Join same-header tables across page boundaries into one table.
///
/// Every line is preserved except the repeated header and separator lines of a
/// continuation table; no other line is dropped or reordered.
pub(crate) fn merge_continuation_tables(page_md: &mut [(u32, String)]) {
    if page_md.len() < 2 {
        return;
    }
    let mut pages: Vec<Vec<String>> = page_md
        .iter()
        .map(|(_, chunk)| chunk.split('\n').map(str::to_string).collect())
        .collect();

    let mut open: Option<OpenTable> = None;
    for p in 0..pages.len() {
        let tables = find_tables(&pages[p]);
        let Some(first) = tables.first() else {
            // A page with no table ends the chain: the open table is no longer
            // "the previous page's table" for the page after this one.
            open = None;
            continue;
        };

        // Merge only when the continuation carries at least one body row and
        // the two headers normalise equal (which implies equal column count).
        let merge_target = match open.as_ref() {
            Some(op)
                if first.end >= first.start + TABLE_PREFIX_LINES
                    && headers_equal(&op.header, &pages[p][first.start]) =>
            {
                Some((op.page, op.last_row + 1))
            }
            _ => None,
        };

        let mut merged = false;
        if let Some((carrier, at)) = merge_target {
            let body: Vec<String> =
                pages[p][first.start + TABLE_PREFIX_LINES..=first.end].to_vec();
            let moved = body.len();
            pages[p].drain(first.start..=first.end);
            pages[carrier].splice(at..at, body);
            if let Some(op) = open.as_mut() {
                op.last_row += moved;
            }
            merged = true;
        }

        // Re-detect after any removal. A continuation page that still carries
        // its own trailing table becomes the new open table; a fully consumed
        // page leaves the merged table open so the chain can continue.
        let remaining = find_tables(&pages[p]);
        if merged {
            if let Some(last) = remaining.last() {
                open = Some(OpenTable {
                    page: p,
                    header: header_cells(&pages[p][last.start]),
                    last_row: last.end,
                });
            }
        } else {
            let last = tables.last().expect("checked non-empty above");
            open = Some(OpenTable {
                page: p,
                header: header_cells(&pages[p][last.start]),
                last_row: last.end,
            });
        }
    }

    for ((_, chunk), lines) in page_md.iter_mut().zip(pages) {
        *chunk = lines.join("\n");
    }
}

/// Every GFM table in a page's lines, in order.
fn find_tables(lines: &[String]) -> Vec<TableSpan> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < lines.len() {
        if !is_table_row(&lines[i]) {
            i += 1;
            continue;
        }
        let start = i;
        let mut has_separator = false;
        while i < lines.len() && is_table_row(&lines[i]) {
            has_separator |= is_separator_row(&lines[i]);
            i += 1;
        }
        if has_separator && i > start + 1 {
            out.push(TableSpan { start, end: i - 1 });
        }
    }
    out
}

/// A rendered GFM table row: starts and ends with an unescaped pipe.
fn is_table_row(line: &str) -> bool {
    let t = line.trim();
    t.starts_with('|') && t.ends_with('|')
}

/// A GFM alignment separator row (`| --- | :---: |`).
fn is_separator_row(line: &str) -> bool {
    let cells = split_row(line);
    !cells.is_empty()
        && cells.iter().all(|c| {
            let c = c.trim();
            !c.is_empty() && c.contains('-') && c.chars().all(|ch| ch == '-' || ch == ':')
        })
}

/// Split a table row into its cell strings, ignoring escaped pipes (`\|`).
fn split_row(line: &str) -> Vec<String> {
    let mut cells: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut escaped = false;
    for ch in line.trim().chars() {
        if escaped {
            cur.push('\\');
            cur.push(ch);
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if ch == '|' {
            cells.push(std::mem::take(&mut cur));
        } else {
            cur.push(ch);
        }
    }
    cells.push(cur);
    if cells.len() >= 2 {
        cells.remove(0);
        cells.pop();
    }
    cells
}

/// Normalised header cells of a table's first line.
fn header_cells(line: &str) -> Vec<String> {
    split_row(line)
        .iter()
        .map(|c| strip_br(c).trim().to_lowercase())
        .collect()
}

/// Two headers match when they have the same column count and the same cells
/// after trimming, case-folding and dropping in-cell `<br>` breaks. An
/// all-empty header (a headerless label/value grid) never matches: it would
/// make every same-column grid on consecutive pages look like one table.
fn headers_equal(open: &[String], line: &str) -> bool {
    if !open.iter().any(|c| !c.is_empty()) {
        return false;
    }
    open == header_cells(line).as_slice()
}

/// Drop HTML `<br>` tags (any casing, slash optional) from a header cell.
fn strip_br(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0usize;
    while i < s.len() {
        if bytes[i] == b'<' && starts_html_br(&bytes[i..]) {
            if let Some(end) = s[i..].find('>') {
                i += end + 1;
                continue;
            }
        }
        let ch = s[i..].chars().next().expect("i is a char boundary");
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// True when `rest` opens with `<br` followed by a tag-ending or whitespace byte
/// (`<br>`, `<br/>`, `<br />`), so a cell word like `<brown>` is not eaten.
fn starts_html_br(rest: &[u8]) -> bool {
    rest.len() >= 4
        && rest[0].eq_ignore_ascii_case(&b'<')
        && rest[1].eq_ignore_ascii_case(&b'b')
        && rest[2].eq_ignore_ascii_case(&b'r')
        && matches!(rest[3], b'>' | b'/' | b' ' | b'\t')
}

#[cfg(test)]
mod tests;
