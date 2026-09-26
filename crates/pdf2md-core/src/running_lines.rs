//! pdf2md-core — High-performance native Rust core engine for sub-millisecond
//! PDF-to-Markdown extraction and 2D spatial canvas table reconstruction.
//!
//! Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
//! SPDX-License-Identifier: BSL-1.1
//! Licensed under the Business Source License 1.1 (BSL-1.1).

use super::*;

/// Suppress running headers/footers in the assembled per-page markdown for
/// documents whose pages carry no structured blocks (the string-walker path),
/// where `tag_running_furniture` has nothing to inspect.
///
/// Only a page's first/last few non-empty lines are candidates, and a line is
/// dropped only when its key recurs on at least three pages (keeping its first
/// occurrence). A page whose whole body is short repeats (a ticket printed
/// identically on every page) is skipped entirely so its content survives.
/// Table rows, images, headings and letterless data lines are never candidates.
pub(super) fn strip_running_lines(page_md: &mut [(u32, String)]) {
    use std::collections::{HashMap, HashSet};
    const BAND: usize = 3;
    const MIN_PAGES: u32 = 3;
    if page_md.len() < 2 {
        return;
    }
    let is_candidate = |l: &str| -> bool {
        let t = l.trim();
        !t.is_empty()
            && t.chars().any(|c| c.is_alphabetic())
            && !carries_data_value(t)
            && !t.starts_with('|')
            && !t.starts_with('<')
            && !t.starts_with('#')
            && !t.starts_with("![")
            && t.split_whitespace().count() <= 14
    };
    // A wholly-repeated sparse page (fewer than 2*BAND non-empty lines) is
    // content, not furniture: every line would fall in a band and be dropped.
    let sparse = |chunk: &str| chunk.lines().filter(|l| !l.trim().is_empty()).count() <= 2 * BAND;

    let mut first_seen: HashMap<String, u32> = HashMap::new();
    let mut page_count: HashMap<String, u32> = HashMap::new();
    for (page, chunk) in page_md.iter() {
        if sparse(chunk) {
            continue;
        }
        let lines: Vec<&str> = chunk.lines().filter(|l| !l.trim().is_empty()).collect();
        let n = lines.len();
        let mut seen_here: HashSet<String> = HashSet::new();
        for (i, l) in lines.iter().enumerate() {
            if !is_candidate(l) || (i >= BAND && i + BAND < n) {
                continue;
            }
            let key = furniture_line_key(l);
            if key.is_empty() {
                continue;
            }
            first_seen.entry(key.clone()).or_insert(*page);
            if seen_here.insert(key.clone()) {
                *page_count.entry(key).or_insert(0) += 1;
            }
        }
    }
    let repeated: HashSet<String> = page_count
        .into_iter()
        .filter(|(_, c)| *c >= MIN_PAGES)
        .map(|(k, _)| k)
        .collect();

    for (page, chunk) in page_md.iter_mut() {
        if sparse(chunk) {
            continue;
        }
        let n = chunk.lines().filter(|l| !l.trim().is_empty()).count();
        let mut out = String::with_capacity(chunk.len());
        let mut idx = 0usize;
        for l in chunk.lines() {
            let drop = if l.trim().is_empty() {
                false
            } else {
                let here = idx;
                idx += 1;
                if is_candidate(l) && (here < BAND || here + BAND >= n) {
                    let key = furniture_line_key(l);
                    repeated.contains(&key)
                        && first_seen.get(&key).copied().unwrap_or(*page) < *page
                } else {
                    false
                }
            };
            if !drop {
                out.push_str(l);
                out.push('\n');
            }
        }
        if out.ends_with('\n') {
            out.pop();
        }
        *chunk = out;
    }
}

/// Page-number aware variant of [`furniture_line_key`] used for cross-page
/// *block* furniture. The line-level key masks only a page token immediately
/// following `page`/`p`; a footer such as `| … | Page : 12/17 |` has a
/// separator (`:`) between the marker and the number, so the number would
/// otherwise survive as data and the two blocks would not compare equal. This
/// variant additionally masks a bare `N` / `N/M` page token adjacent to a page
/// marker (skipping at most one separator token) and any `N/M` fraction on a
/// line that carries a page marker. Everything else defers to
/// [`furniture_line_key`], so amounts, dates and long identifiers still make
/// two blocks distinct.
pub(super) fn furniture_block_line_key(line: &str) -> String {
    let toks: Vec<&str> = line.split_whitespace().collect();
    let strip = |t: &str| -> String {
        t.chars()
            .filter(|c| c.is_alphanumeric())
            .collect::<String>()
            .to_lowercase()
    };
    let is_marker = |t: &str| {
        let s = strip(t);
        s == "page" || s == "p"
    };
    let has_marker = toks.iter().any(|t| is_marker(t));
    let is_page_run = |t: &str| -> bool {
        let core = t.trim_matches(|c: char| !c.is_alphanumeric());
        if core.is_empty() {
            return false;
        }
        let parts: Vec<&str> = core.split('/').collect();
        parts.len() <= 2
            && parts.iter().all(|p| {
                !p.is_empty() && p.len() <= 3 && p.chars().all(|c| c.is_ascii_digit())
            })
    };
    let is_sep = |t: &str| t.chars().all(|c| !c.is_alphanumeric());
    let masked: Vec<String> = toks
        .iter()
        .enumerate()
        .map(|(i, tok)| {
            if !has_marker || !is_page_run(tok) {
                return (*tok).to_string();
            }
            if tok.contains('/') {
                return "#/#".to_string();
            }
            let prev_marker = i > 0 && is_marker(toks[i - 1]);
            let sep_then_marker = i > 1 && is_sep(toks[i - 1]) && is_marker(toks[i - 2]);
            if prev_marker || sep_then_marker {
                "#".to_string()
            } else {
                (*tok).to_string()
            }
        })
        .collect();
    furniture_line_key(&masked.join(" "))
}

/// Collapse cross-page running furniture expressed as *multi-line blocks* — the
/// case the line-based passes cannot see because a letterhead or footer is a
/// running title plus a status table (`| Enedis-NOI-CF_110E | Page : 1/17 |`,
/// `| 4.0 | 08/08/2024 |`, then the document title).
///
/// Operates on the assembled per-page markdown before reflow. It is
/// deliberately conservative:
///  * only a page's first/last `BLOCK_BAND` blocks are candidates, so repeated
///    body prose in the middle of a page is never touched;
///  * a page with too few blocks to have a real body (`<= 2 * BLOCK_BAND`) is
///    skipped entirely, so a wholly-repeated sparse page keeps its content;
///  * a candidate block must recur on at least `BLOCK_MIN_PAGES` distinct pages
///    under a key that normalizes whitespace/punctuation and masks only
///    page-number tokens, so a per-page amount, date, identifier or body value
///    keeps two blocks distinct;
///  * the block is bounded in size and must carry running-title text, so large
///    data tables and image lines are never candidates;
///  * only the first occurrence survives, and a page is never emptied.
pub(super) fn collapse_repeated_furniture_blocks(page_md: &mut [(u32, String)]) {
    use std::collections::{HashMap, HashSet};
    const BLOCK_BAND: usize = 2;
    const BLOCK_MIN_PAGES: u32 = 3;
    const BLOCK_MAX_LINES: usize = 12;
    const BLOCK_MAX_CHARS: usize = 600;

    if page_md.len() < BLOCK_MIN_PAGES as usize {
        return;
    }

    // Split a chunk into blank-line-separated blocks; whitespace-only lines
    // separate, they do not form a block.
    let split_blocks = |chunk: &str| -> Vec<String> {
        let mut blocks: Vec<String> = Vec::new();
        let mut cur: Vec<&str> = Vec::new();
        for line in chunk.lines() {
            if line.trim().is_empty() {
                if !cur.is_empty() {
                    blocks.push(cur.join("\n"));
                    cur.clear();
                }
            } else {
                cur.push(line);
            }
        }
        if !cur.is_empty() {
            blocks.push(cur.join("\n"));
        }
        blocks
    };

    let is_candidate = |b: &str| -> bool {
        if b.lines().count() > BLOCK_MAX_LINES || b.len() > BLOCK_MAX_CHARS {
            return false;
        }
        if b.trim_start().starts_with("![") || b.trim_start().starts_with("<img") {
            return false;
        }
        // Require at least one line with two or more alphabetic words, so a
        // pure numeric table cannot be collapsed as running furniture.
        b.lines().any(|l| {
            l.split_whitespace()
                .filter(|w| w.chars().any(|c| c.is_alphabetic()))
                .count()
                >= 2
        })
    };

    let key_of = |b: &str| -> String {
        b.lines()
            .map(furniture_block_line_key)
            .collect::<Vec<_>>()
            .join("\n")
    };

    let pages: Vec<Vec<String>> = page_md.iter().map(|(_, c)| split_blocks(c)).collect();

    let mut first_page: HashMap<String, u32> = HashMap::new();
    let mut page_count: HashMap<String, u32> = HashMap::new();
    for (i, blocks) in pages.iter().enumerate() {
        // A page with too few blocks to carry a body is content, not furniture.
        if blocks.len() <= 2 * BLOCK_BAND {
            continue;
        }
        let page = page_md[i].0;
        let n = blocks.len();
        let mut seen_here: HashSet<String> = HashSet::new();
        for (bi, b) in blocks.iter().enumerate() {
            if !is_candidate(b) || !(bi < BLOCK_BAND || bi + BLOCK_BAND >= n) {
                continue;
            }
            let key = key_of(b);
            if key.chars().filter(|c| c.is_alphanumeric()).count() < 8 {
                continue;
            }
            first_page.entry(key.clone()).or_insert(page);
            if seen_here.insert(key.clone()) {
                *page_count.entry(key).or_insert(0) += 1;
            }
        }
    }
    let repeated: HashSet<String> = page_count
        .into_iter()
        .filter(|(_, c)| *c >= BLOCK_MIN_PAGES)
        .map(|(k, _)| k)
        .collect();
    if repeated.is_empty() {
        return;
    }

    for (i, (page, chunk)) in page_md.iter_mut().enumerate() {
        let blocks = &pages[i];
        if blocks.len() <= 2 * BLOCK_BAND {
            continue;
        }
        let n = blocks.len();
        let page_no = *page;
        let mut kept: Vec<&str> = Vec::with_capacity(n);
        let (mut dropped_any, mut dropped_first, mut dropped_last) = (false, false, false);
        for (bi, b) in blocks.iter().enumerate() {
            let in_band = bi < BLOCK_BAND || bi + BLOCK_BAND >= n;
            let drop = in_band && is_candidate(b) && {
                let key = key_of(b);
                repeated.contains(&key)
                    && first_page.get(&key).copied().unwrap_or(page_no) < page_no
            };
            if drop {
                dropped_any = true;
                dropped_first |= bi == 0;
                dropped_last |= bi + 1 == n;
            } else {
                kept.push(b.as_str());
            }
        }
        // Never empty a page: if every block was furniture, keep the page as-is.
        if dropped_any && !kept.is_empty() {
            // `split_blocks` discards blank lines, so rebuilding from `kept`
            // alone loses the trailing separator page assembly always appends
            // (pages are concatenated with no separator). Capture it so a
            // retained last block keeps its page boundary.
            let trail = {
                let t = chunk.trim_end_matches(|c: char| c == '\n' || c == '\r');
                chunk[t.len()..].to_string()
            };
            let mut rebuilt = kept.join("\n\n");
            // Keep a leading boundary when the header was dropped, so this
            // page's first block is not welded onto the previous page.
            if dropped_first {
                rebuilt.insert_str(0, "\n\n");
            }
            if dropped_last {
                // The trailing footer itself was furniture: restore the
                // blank-line boundary it used to provide.
                rebuilt.push_str("\n\n");
            } else {
                // Otherwise preserve exactly the separator the chunk had, so a
                // retained last block (e.g. a lone `# 8`) is not welded onto
                // the next page's first block (e.g. a table row) with zero
                // characters between them.
                rebuilt.push_str(&trail);
            }
            *chunk = rebuilt;
        }
    }
}

/// Tag blocks whose text repeats near the top/bottom of >= 3 pages as running
/// headers/footers. Operates purely on the structured block list.
pub(super) fn tag_running_furniture(blocks: &mut [layout::DocBlock]) {
    use std::collections::HashMap;
    let mut page_span: HashMap<usize, (f64, f64)> = HashMap::new();
    for b in blocks.iter() {
        let e = page_span
            .entry(b.page)
            .or_insert((f64::INFINITY, f64::NEG_INFINITY));
        e.0 = e.0.min(b.y0.min(b.y1));
        e.1 = e.1.max(b.y0.max(b.y1));
    }
    let norm = |t: &str| -> String { furniture_line_key(t) };
    let mut header_counts: HashMap<String, usize> = HashMap::new();
    let mut footer_counts: HashMap<String, usize> = HashMap::new();
    for b in blocks.iter() {
        if b.kind != "body" && b.kind != "list" && b.kind != "heading" {
            continue;
        }
        if b.text.split_whitespace().count() > 14 {
            continue; // paragraphs aren't furniture
        }
        // A line with no letters is a data value, not furniture; digit
        // normalization would otherwise collapse distinct numbers ("0.19" /
        // "0.29" → "#.##") into one bogus repeated header. A line that *does*
        // carry an amount or a long number is equally data: identical fee/total
        // rows repeat in the footer band of every page and must survive.
        if !b.text.chars().any(|c| c.is_alphabetic()) || carries_data_value(&b.text) {
            continue;
        }
        let n = norm(&b.text);
        if n.is_empty() {
            continue;
        }
        if let Some((lo, hi)) = page_span.get(&b.page) {
            let span = (hi - lo).abs().max(1.0);
            let top_frac = ((b.y0.max(b.y1)) - lo) / span;
            let bottom_frac = ((b.y0.min(b.y1)) - lo) / span;
            if top_frac > 0.9 {
                *header_counts.entry(n).or_insert(0) += 1;
            } else if bottom_frac < 0.1 {
                *footer_counts.entry(n).or_insert(0) += 1;
            }
        }
    }
    for b in blocks.iter_mut() {
        if b.kind != "body" && b.kind != "list" && b.kind != "heading" {
            continue;
        }
        if b.text.split_whitespace().count() > 14 {
            continue;
        }
        if !b.text.chars().any(|c| c.is_alphabetic()) || carries_data_value(&b.text) {
            continue;
        }
        let n = norm(&b.text);
        if let Some((lo, hi)) = page_span.get(&b.page) {
            let span = (hi - lo).abs().max(1.0);
            let top_frac = ((b.y0.max(b.y1)) - lo) / span;
            let bottom_frac = ((b.y0.min(b.y1)) - lo) / span;
            if top_frac > 0.9 && header_counts.get(&n).copied().unwrap_or(0) >= 3 {
                b.kind = "header".to_string();
            } else if bottom_frac < 0.1 && footer_counts.get(&n).copied().unwrap_or(0) >= 3 {
                b.kind = "footer".to_string();
            }
        }
    }
}
