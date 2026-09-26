//! pdf2md-core — High-performance native Rust core engine for sub-millisecond
//! PDF-to-Markdown extraction and 2D spatial canvas table reconstruction.
//!
//! Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
//! SPDX-License-Identifier: BSL-1.1
//! Licensed under the Business Source License 1.1 (BSL-1.1).


/// Formats raw URLs as markdown links [url](url) and unglues leading footnote digits (e.g. 1https:// -> 1 [https://..](..)).
pub(super) fn format_urls_and_footnotes(text: &str) -> String {
    let mut out = String::new();
    for (i, line) in text.lines().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let mut processed_line = line.to_string();

        // 1. Unglue leading footnote numbers:
        // E.g. "1https://" -> "1 https://"
        // E.g. "4Since" -> "4 Since" (digit at start of line followed by capital letter)
        if let Some(first_char) = processed_line.chars().next() {
            if first_char.is_ascii_digit() {
                let digit_end = processed_line.find(|c: char| !c.is_ascii_digit()).unwrap_or(0);
                if digit_end > 0 && digit_end < processed_line.len() {
                    let rem = &processed_line[digit_end..];
                    if rem.starts_with("http://") || rem.starts_with("https://") {
                        processed_line = format!("{} {}", &processed_line[..digit_end], rem);
                    } else if rem.chars().next().map_or(false, |c| c.is_ascii_uppercase()) {
                        processed_line = format!("{} {}", &processed_line[..digit_end], rem);
                    }
                }
            }
        }

        // 2. Detect URLs and format as markdown links: [url](url)
        let mut result = String::new();
        let mut cursor = 0;
        while let Some(start_idx) = processed_line[cursor..]
            .find("http://")
            .or_else(|| processed_line[cursor..].find("https://"))
        {
            let abs_start = cursor + start_idx;
            result.push_str(&processed_line[cursor..abs_start]);

            let is_already_linked = (abs_start > 0 && processed_line.as_bytes()[abs_start - 1] == b'(')
                || (abs_start > 0 && processed_line.as_bytes()[abs_start - 1] == b'<');

            let url_sub = &processed_line[abs_start..];
            let end_offset = url_sub
                .find(|c: char| {
                    c.is_whitespace()
                        || c == ')'
                        || c == '>'
                        || c == '<'
                        || c == '\"'
                        || c == '\''
                })
                .unwrap_or(url_sub.len());

            let raw_url = &url_sub[..end_offset];
            let trimmed_len = raw_url
                .trim_end_matches(|c: char| c == '.' || c == ',' || c == ';' || c == ':')
                .len();
            let trailing_punct = &raw_url[trimmed_len..];
            let clean_url = &raw_url[..trimmed_len];

            if !is_already_linked && !clean_url.is_empty() {
                result.push_str(&format!("[{0}]({0})", clean_url));
            } else {
                result.push_str(clean_url);
            }
            result.push_str(trailing_punct);
            cursor = abs_start + end_offset;
        }
        result.push_str(&processed_line[cursor..]);
        out.push_str(&result);
    }
    out
}

/// Finds the start and end indices of the markdown table enclosing `pos` in `text`.
/// If `pos` is inside or on a table row, returns `(table_start, table_end)` where
/// `table_end` is the end index of the last row of the table.
/// If `pos` is not in a table, returns `(pos, pos)`.
pub(super) fn find_table_boundaries(text: &str, pos: usize) -> (usize, usize) {
    let is_table_line = |line: &str| -> bool {
        let t = line.trim();
        t.starts_with('|') && t.ends_with('|')
    };

    let line_start = text[..pos].rfind('\n').map(|p| p + 1).unwrap_or(0);
    let line_end = text[pos..].find('\n').map(|p| pos + p).unwrap_or(text.len());
    let current_line = &text[line_start..line_end];

    if !is_table_line(current_line) {
        return (pos, pos);
    }

    // Scan backwards for table start
    let mut t_start = line_start;
    let mut cur = line_start;
    while cur > 0 {
        let prev_start = text[..cur - 1].rfind('\n').map(|p| p + 1).unwrap_or(0);
        let prev_line = &text[prev_start..cur - 1];
        if is_table_line(prev_line) {
            t_start = prev_start;
            cur = prev_start;
        } else {
            break;
        }
    }

    // Scan forward for table end
    let mut t_end = line_end;
    cur = line_end;
    while cur < text.len() {
        let next_start = cur + 1;
        if next_start >= text.len() {
            break;
        }
        let next_end = text[next_start..].find('\n').map(|p| next_start + p).unwrap_or(text.len());
        let next_line = &text[next_start..next_end];
        if is_table_line(next_line) {
            t_end = next_end;
            cur = next_end;
        } else {
            break;
        }
    }

    (t_start, t_end)
}
