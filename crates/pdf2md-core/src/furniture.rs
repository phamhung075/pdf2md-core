//! pdf2md-core — High-performance native Rust core engine for sub-millisecond
//! PDF-to-Markdown extraction and 2D spatial canvas table reconstruction.
//!
//! Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
//! SPDX-License-Identifier: BSL-1.1
//! Licensed under the Business Source License 1.1 (BSL-1.1).

use super::*;

/// True when `c` is the kind of punctuation that separates a running footer's
/// fields ("… - page 2", "| page 2", "· page 2"). Used to tell a page-varying
/// footer apart from body prose that merely contains the word "page".
pub(super) fn is_furniture_separator(c: char) -> bool {
    matches!(
        c,
        '-' | '\u{2013}' | '\u{2014}' | '\u{00b7}' | '|' | '\u{2022}' | '\u{00a9}' | '\u{00ae}' | ',' | ';' | ':'
    )
}

/// Mask a bare/short page-number token (`2`, `2/7`) that directly follows a
/// `page`/`p.` marker in a running header/footer. Anything longer than three
/// digits, or carrying any other character (amounts, dates, postal codes,
/// IBAN/SIRET runs), is rejected so two different values never compare equal.
pub(super) fn mask_page_token(tok: &str) -> Option<String> {
    let trimmed = tok.trim_matches(|c: char| ".,;:*_()[]{}".contains(c));
    let parts: Vec<&str> = trimmed.split('/').collect();
    if parts.is_empty() || parts.len() > 2 {
        return None;
    }
    if !parts
        .iter()
        .all(|p| !p.is_empty() && p.len() <= 3 && p.chars().all(|c| c.is_ascii_digit()))
    {
        return None;
    }
    Some(parts.iter().map(|_| "#").collect::<Vec<_>>().join("/"))
}

/// Whether a short line carries a real data value that must never be treated
/// as running furniture: a decimal amount (`\d+[.,]\d{2}`) or a long digit run
/// (>= 4 contiguous digits). Identical fee/total rows often repeat in the
/// footer band of every page; suppressing them as a "running footer" deletes
/// real numbers. Short 3-digit identifier groups ("RCS 123 456 789") are not
/// protected, so a page-varying legal footer is still suppressed.
pub(super) fn carries_data_value(t: &str) -> bool {
    let chars: Vec<char> = t.chars().collect();
    let mut i = 0usize;
    while i < chars.len() {
        if !chars[i].is_ascii_digit() {
            i += 1;
            continue;
        }
        let start = i;
        while i < chars.len() && chars[i].is_ascii_digit() {
            i += 1;
        }
        if i - start >= 4 {
            return true;
        }
        if i < chars.len() && (chars[i] == '.' || chars[i] == ',') {
            let mut j = i + 1;
            let mut decimals = 0usize;
            while j < chars.len() && chars[j].is_ascii_digit() {
                j += 1;
                decimals += 1;
            }
            if decimals == 2 {
                return true;
            }
        }
    }
    false
}

/// Normalize a short line for running-furniture comparison: lowercase, drop
/// punctuation, and collapse whitespace. A page number that directly follows a
/// `page`/`p.` marker *set off by a separator* ("… - page 2") is masked to `#`,
/// so a page-varying legal footer compares equal across pages; a bare number in
/// body prose ("corps page 2") is kept verbatim, as are amounts, dates and
/// postal/account runs, so unique per-page content is never treated as furniture.
pub(super) fn furniture_line_key(t: &str) -> String {
    let toks: Vec<&str> = t.split_whitespace().collect();
    let mut out: Vec<String> = Vec::with_capacity(toks.len());
    let mut i = 0;
    while i < toks.len() {
        let tok = toks[i];
        let stripped: String = tok
            .chars()
            .filter(|c| c.is_alphanumeric())
            .collect::<String>()
            .to_lowercase();
        let marker = stripped == "page" || stripped == "p";
        let leading_sep = tok
            .chars()
            .take_while(|c| !c.is_alphanumeric())
            .any(is_furniture_separator);
        let prev_sep = i > 0 && toks[i - 1].chars().any(is_furniture_separator);
        if marker && (prev_sep || leading_sep || tok.ends_with('.')) {
            if let Some(masked) = toks.get(i + 1).and_then(|n| mask_page_token(n)) {
                if !stripped.is_empty() {
                    out.push(stripped);
                }
                out.push(masked);
                i += 2;
                continue;
            }
        }
        if !stripped.is_empty() {
            out.push(stripped);
        }
        i += 1;
    }
    out.join(" ")
}

/// Parse a *pure* page-counter line — `Page N`, `Page N/M`, `N/M`, `N / M`,
/// `N sur M` — alone on its line. A bare `N` counts only with the `Page`
/// marker. Returns `(N, M)`.
pub(super) fn parse_page_counter(t: &str) -> Option<(u32, Option<u32>)> {
    let lower = t.trim().to_lowercase();
    let (body, marked) = match lower.strip_prefix("page") {
        Some(rest) => (rest, true),
        None => (lower.as_str(), false),
    };
    let compact: String = body.chars().filter(|c| !c.is_whitespace()).collect();
    if compact.is_empty() {
        return None;
    }
    if let Some((a, b)) = compact.split_once('/') {
        if a.is_empty() || b.is_empty() || b.contains('/') {
            return None;
        }
        return Some((a.parse().ok()?, Some(b.parse().ok()?)));
    }
    if let Some((a, b)) = compact.split_once("sur") {
        if a.is_empty() || b.is_empty() {
            return None;
        }
        return Some((a.parse().ok()?, Some(b.parse().ok()?)));
    }
    if marked {
        return compact.parse::<u32>().ok().map(|n| (n, None));
    }
    None
}

/// Drop pure page-counter lines that are *corroborated* as counters: they sit
/// in a page's top/bottom band, their `N` differs across pages, and their `M`
/// is consistent (all equal, or equal to the page count). A standalone ratio
/// cell (`3/4` repeated unchanged on every page) and a `1/2` mid-page are
/// therefore kept.
pub(super) fn suppress_page_counters(page_md: &mut [(u32, String)]) {
    use std::collections::HashSet;
    const BAND: usize = 3;
    if page_md.len() < 2 {
        return;
    }
    let mut ns: HashSet<u32> = HashSet::new();
    let mut ms: Vec<u32> = Vec::new();
    let mut pages_with: HashSet<u32> = HashSet::new();
    for (page, chunk) in page_md.iter() {
        let lines: Vec<&str> = chunk.lines().filter(|l| !l.trim().is_empty()).collect();
        let n = lines.len();
        for (i, l) in lines.iter().enumerate() {
            if i >= BAND && i + BAND < n {
                continue;
            }
            if let Some((cn, cm)) = parse_page_counter(l) {
                ns.insert(cn);
                if let Some(m) = cm {
                    ms.push(m);
                }
                pages_with.insert(*page);
            }
        }
    }
    if pages_with.len() < 2 || ns.len() < 2 {
        return;
    }
    let m_ok = ms.is_empty() || ms.iter().all(|m| *m == ms[0]);
    if !m_ok {
        return;
    }
    for (_page, chunk) in page_md.iter_mut() {
        let lines: Vec<&str> = chunk.lines().filter(|l| !l.trim().is_empty()).collect();
        let n = lines.len();
        let mut out = String::with_capacity(chunk.len());
        let mut idx = 0usize;
        for l in chunk.lines() {
            let drop = if l.trim().is_empty() {
                false
            } else {
                let here = idx;
                idx += 1;
                (here < BAND || here + BAND >= n) && parse_page_counter(l).is_some()
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

/// Document-level running-furniture pass.
///
/// A short line repeated at the top band of many pages is a running header; one
/// repeated at the bottom band is a running footer. Tag them in the block list,
/// and suppress their repeated occurrences in the emitted markdown (keep the
/// first / page-1 line). Pure page counters are corroborated at the document
/// level (they vary across pages and share a page count) before any are
/// dropped, and cross-page furniture expressed as multi-line blocks is
/// collapsed as whole blocks before reflow.
pub(super) fn apply_furniture(
    options: &ConversionOptions,
    page_md: &mut [(u32, String)],
    block_items: &mut Vec<layout::DocBlock>,
) {
    if !options.detect_layout {
        return;
    }
    if !block_items.is_empty() {
        tag_running_furniture(block_items.as_mut_slice());
        // Keep a running header/footer only on its first page; strip the
        // repeated occurrences from every later page of the markdown. Matching
        // is on the normalized key (which masks only a page-number tail set off
        // by a separator), so a page-varying footer is suppressed while body
        // prose and data values are kept.
        let mut furniture: Vec<(String, u32)> = Vec::new();
        for b in block_items
            .iter()
            .filter(|b| b.kind == "header" || b.kind == "footer")
        {
            let t = b.text.trim();
            if t.len() <= 1 {
                continue;
            }
            let key = furniture_line_key(t);
            match furniture.iter_mut().find(|(fk, _)| *fk == key) {
                Some((_, first)) => *first = (*first).min(b.page as u32),
                None => furniture.push((key, b.page as u32)),
            }
        }
        for (page, chunk) in page_md.iter_mut() {
            let mut out = String::with_capacity(chunk.len());
            for ln in chunk.lines() {
                let drop = !ln.trim().is_empty()
                    && !ln.trim().starts_with('|')
                    && furniture.iter().any(|(key, first_page)| {
                        *page > *first_page && furniture_line_key(ln.trim()) == *key
                    });
                if !drop {
                    out.push_str(ln);
                    out.push('\n');
                }
            }
            if out.ends_with('\n') {
                out.pop();
            }
            *chunk = out;
        }
    } else {
        strip_running_lines(page_md);
    }
    suppress_page_counters(page_md);
    collapse_repeated_furniture_blocks(page_md);
}
