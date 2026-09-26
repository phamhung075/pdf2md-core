// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Robust multilingual (FR / VI / EN) PDF text extraction for the fast path.
//!
//! This module replaces `lopdf::Document::extract_text` for the digital-PDF
//! fast path. `lopdf`'s extractor has two defects that corrupt accented Latin
//! text (French, Vietnamese, ...):
//!
//! 1. A `/Encoding` dictionary whose `/Differences` array contains `/.notdef`
//!    (extremely common in real-world generators, e.g. EDF / Enedis bills) is
//!    treated as an *error*, so the whole font silently falls back to lopdf's
//!    `STANDARD_ENCODING` table.
//! 2. That fallback table is corrupt for bytes >= 0xC0: it maps byte `0xE9`
//!    (`é`) to `Ø` and byte `0xE8` (`è`) to `Ł`, among others.
//!
//! We therefore resolve font encodings ourselves with correct data tables and
//! decode each text run accordingly:
//!   * `/ToUnicode` CMaps (bfchar / bfrange) — authoritative when present;
//!   * `/Encoding` by name (`WinAnsiEncoding`, `MacRomanEncoding`, ...);
//!   * `/Encoding` dictionaries with `/Differences` (`.notdef` and unknown
//!     glyph names are tolerated, AGL `uniXXXX` names supported);
//!   * a WinAnsi heuristic for non-symbolic simple fonts with no encoding
//!     information (how the overwhelming majority of producers write accented
//!     Latin text).
//!
//! Page content / font-structure parsing still comes from lopdf (public API
//! only). If content parsing fails, the caller falls back to lopdf's own
//! extractor.

use super::*;

/// Decode a page's content streams under explicit decompression caps.
///
/// Bounded replacement for `Document::get_and_decode_page_content`, whose
/// single-stream decode is unbounded: a ~1 MiB Flate page stream can inflate to
/// gigabytes. A page over either cap is rejected with a decompression error —
/// the callers already treat an undecodable page as "no text" — never truncated
/// mid-operator into garbage. A stream that fails for any other reason keeps
/// lopdf's lenient fallback to its raw bytes, still within the page budget.
pub(crate) fn decode_page_content(
    doc: &Document,
    page_id: ObjectId,
) -> lopdf::Result<Content<Vec<Operation>>> {
    let limit_err = || {
        lopdf::Error::Decompress(lopdf::DecompressError::MemoryLimitExceeded {
            limit: MAX_PAGE_CONTENT_TOTAL,
        })
    };
    let mut data = Vec::new();
    for object_id in doc.get_page_contents(page_id) {
        let Ok(stream) = doc.get_object(object_id).and_then(Object::as_stream) else {
            continue;
        };
        let remaining = MAX_PAGE_CONTENT_TOTAL.saturating_sub(data.len());
        let budget = remaining.min(MAX_PAGE_CONTENT_STREAM);
        match stream.get_plain_content_with_limit(budget) {
            Ok(bytes) => data.extend_from_slice(&bytes),
            Err(lopdf::Error::Decompress(lopdf::DecompressError::MemoryLimitExceeded {
                ..
            })) => {
                return Err(limit_err());
            }
            Err(_) => {
                if stream.content.len() > budget {
                    return Err(limit_err());
                }
                data.extend_from_slice(&stream.content);
            }
        }
        data.push(b'\n');
    }
    Content::decode(&data)
}

pub(super) fn extract_page(
    doc: &Document,
    page_id: ObjectId,
    detect_tables: bool,
    detect_layout: bool,
    detect_math: bool,
) -> Result<(PageText, Option<bool>), String> {
    let chain = resource_dicts(doc, page_id);
    let mut fonts: BTreeMap<Vec<u8>, &Dictionary> = BTreeMap::new();
    collect_fonts(doc, &chain, &mut fonts);
    let has_fonts = !fonts.is_empty();

    let content: Content<Vec<Operation>> =
        decode_page_content(doc, page_id).map_err(|e| format!("{e}"))?;

    // Glyph-positioned pages (one glyph per text object with absolute
    // coordinates, e.g. LibreOffice forms) need a geometry engine — word
    // boundaries are encoded as inter-glyph gaps, not as space glyphs, and the
    // content stream is not in reading order. Delegate those to `layout`.
    //
    // Signature: `TJ` arrays only (no plain `Tj` strings) **and** per-glyph
    // `TD` positioning. Docs that use `Tm` with multi-string `TJ` arrays
    // (e.g. some Enedis bills) still extract correctly through the string
    // walker, so they must not be re-routed.
    //
    // Table recovery is the exception: Enedis-style notes place every cell
    // (and every word) at an absolute `Tm` position, so the geometry engine
    // can reconstruct their grids while the string walker flattens them. When
    // `detect_tables` is on we therefore also route `TJ`-only pages that
    // position with `Tm` (no `TD` needed) through the geometry engine, which
    // recovers reading order *and* Stage-3 grids. Without `detect_tables` the
    // string walker is kept so table-less callers stay byte-identical.
    // Page-stream-only signals. These decide the pre-existing *legacy*
    // geometry routing and must stay byte-for-byte equivalent for pages HEAD
    // already routed.
    let has_tj = content.operations.iter().any(|op| op.operator == "TJ");
    let has_tj_plain = content.operations.iter().any(|op| op.operator == "Tj");
    // `'` / `"` show a string and advance to the next line; some statement
    // generators position every fragment with them (plus `Td`) and no `Tj`/`TJ`.
    let has_quote = content
        .operations
        .iter()
        .any(|op| op.operator == "'" || op.operator == "\"");
    let has_td = content.operations.iter().any(|op| op.operator == "TD");
    // `Td` (lowercase) is the ordinary line/positioning operator. Some
    // producers (CAF payslips, Engie bills) position every fragment with
    // `Tj` + `Td` and no `Tm`/`TD`, so the old check missed them and they
    // stayed in the string walker — no table geometry and content-stream
    // reading order. Route those only when the `Td` moves are *fragment*
    // placements (most carry a horizontal component), not the vertical-only
    // line advances of ordinary one-BT-per-line prose (tickets, books), whose
    // string-walker output must stay byte-identical. The table-less path keeps
    // its exact legacy signature.
    let has_plain_td = {
        let (mut total, mut horizontal) = (0usize, 0usize);
        for op in &content.operations {
            if op.operator == "Td" {
                total += 1;
                if let Some(tx) = op.operands.first().and_then(|o| o.as_float().ok()) {
                    if tx.abs() > 0.01 {
                        horizontal += 1;
                    }
                }
            }
        }
        total >= 2 && horizontal * 2 >= total
    };
    let has_tm = content.operations.iter().any(|op| op.operator == "Tm");
    // Legacy geometry routing (a `TJ`/`Tj` page positioned with `TD`/`Tm`);
    // this is the pre-existing signature and is never re-checked below.
    let legacy_geometry = (has_tj || has_tj_plain) && (has_td || has_tm);

    // B1: text and positioning operators *inside Form XObjects* must count
    // too. A page whose content stream is just `/Fm0 Do` has none of the
    // page-stream flags above, so it went to the string walker, which recurses
    // into forms and produces text but never builds table geometry
    // (corpus files/corpus files/a corpus file). Keep these form-aware flags separate
    // from `legacy_geometry` so the newly routed pages still hit the
    // walker-vs-glyph digit-loss fallback below.
    let sig = crate::layout::glyph_stream::page_content_signals(doc, page_id);
    // The routing signal scan shares the same family of bounds as the walkers;
    // a page whose signal scan was truncated may have been misrouted, so carry
    // that fact into the page result too.
    let signals_exhausted = sig.budget_exhausted;
    let form_geometry =
        (sig.has_tj_array || sig.has_tj_plain) && (sig.has_td_upper || sig.has_tm);
    let form_plain_td = sig.td_total >= 2 && sig.td_horizontal * 2 >= sig.td_total;
    let route_to_layout = if detect_tables {
        // `Tj` positioned with `Td` (CAF payslips, Engie bills) and quote
        // show-ops (`'`/`"`). A quote page is routed only when it also carries
        // explicit `Td` fragment placements: a page that shows every string
        // with `'` at one `Tm` and zero leading is one-string-per-line prose
        // the geometry path would merge into a single run, so it stays on the
        // string walker.
        legacy_geometry
            || ((has_tj_plain || has_quote) && has_plain_td)
            || (form_geometry && !legacy_geometry)
            || ((sig.has_tj_plain || sig.has_quote) && form_plain_td)
    } else {
        has_tj && !has_tj_plain && has_td
    };
    // The `Tj`+`Td` / quote triggers were added by the layout batch. The
    // geometry engine can drop text held in rotated or Form-XObject content
    // that the string walker reaches, so those newly-routed pages are checked
    // against the walker before their output is trusted.
    let new_geometry = route_to_layout && !legacy_geometry && detect_tables;
    if route_to_layout {
        if new_geometry {
            let mut walker = String::new();
            let mut walker_ops = false;
            let mut form_path: Vec<ObjectId> = Vec::new();
            let mut budget = WalkerBudget::new();
            walk_content(
                doc,
                &chain,
                &content.operations,
                &mut walker,
                &mut walker_ops,
                &mut form_path,
                0,
                &mut budget,
            );
            let walker = walker.trim_end().to_string();
            let marker_hint = Some(first_line_looks_like_heading(&walker));
            match crate::layout::extract_page_glyphs(
                doc,
                page_id,
                detect_tables,
                detect_layout,
                detect_math,
            ) {
                Ok(pt) => {
                    // No table recovered, or a decoded digit run is missing:
                    // the walker is the lossless output, so keep it (tables
                    // are not worth unique content).
                    if pt.tables == 0 || loses_digit_content(&walker, &pt.text) {
                        return Ok((
                            PageText {
                                text: walker,
                                text_ops_seen: walker_ops,
                                has_fonts,
                                tables: 0,
                                blocks: Vec::new(),
                                budget_exhausted: budget.exhausted || signals_exhausted,
                            },
                            marker_hint,
                        ));
                    }
                    return Ok((
                        PageText {
                            budget_exhausted: pt.budget_exhausted || signals_exhausted,
                            ..pt
                        },
                        marker_hint,
                    ));
                }
                Err(e) => return Err(e),
            }
        }
        return match crate::layout::extract_page_glyphs(
            doc,
            page_id,
            detect_tables,
            detect_layout,
            detect_math,
        ) {
            Ok(pt) => {
                // The geometry engine classifies a page drawn under a rotated
                // CTM (e.g. landscape content rotated 90 degrees) as vertical
                // text; with layout analysis on it keeps those spans in
                // structured blocks only and leaves `text` empty, so the page
                // previously failed with "no readable words" even though its
                // `/ToUnicode` map is fine (a corpus file). The same drop can leave a
                // single stray word in `text` while the blocks hold the rest
                // (a corpus file). The string walker decodes the same fonts and is the
                // lossless output here, so fall back to it instead of reporting
                // a native failure or returning the fragment.
                let text_words = pt.text.split_whitespace().count();
                let block_words: usize = pt
                    .blocks
                    .iter()
                    .map(|b| b.text.split_whitespace().count())
                    .sum();
                if text_words >= 2 || (text_words > 0 && block_words < 5) {
                    return Ok((
                        PageText {
                            budget_exhausted: pt.budget_exhausted || signals_exhausted,
                            ..pt
                        },
                        None,
                    ));
                }
                // Keep the page-marker decision the geometry path made, so a
                // fallback page does not lose the `## Page N` marker the
                // non-fallback path would have emitted.
                let marker_hint = Some(first_line_looks_like_heading(&pt.text));
                let mut out = String::new();
                let mut ops_seen = false;
                let mut form_path: Vec<ObjectId> = Vec::new();
                let mut budget = WalkerBudget::new();
                walk_content(
                    doc,
                    &chain,
                    &content.operations,
                    &mut out,
                    &mut ops_seen,
                    &mut form_path,
                    0,
                    &mut budget,
                );
                let out = out.trim_end().to_string();
                if out.split_whitespace().count() <= text_words {
                    return Ok((
                        PageText {
                            budget_exhausted: pt.budget_exhausted || signals_exhausted,
                            ..pt
                        },
                        None,
                    ));
                }
                Ok((
                    PageText {
                        text: out,
                        text_ops_seen: ops_seen,
                        has_fonts,
                        tables: 0,
                        blocks: Vec::new(),
                        budget_exhausted: budget.exhausted || signals_exhausted,
                    },
                    marker_hint,
                ))
            }
            Err(e) => Err(e),
        };
    }

    let mut out = String::new();
    let mut text_ops_seen = false;
    let mut form_path: Vec<ObjectId> = Vec::new();
    let mut budget = WalkerBudget::new();
    walk_content(
        doc,
        &chain,
        &content.operations,
        &mut out,
        &mut text_ops_seen,
        &mut form_path,
        0,
        &mut budget,
    );

    Ok((
        PageText {
            text: out.trim_end().to_string(),
            text_ops_seen,
            has_fonts,
            tables: 0,
            blocks: Vec::new(),
            budget_exhausted: budget.exhausted || signals_exhausted,
        },
        None,
    ))
}

/// Reports the text for one page (1-based page numbers, as used by
/// `Document::get_pages`) plus whether the page contains text-show operators
/// at all.
pub fn extract_page_text_report(
    doc: &Document,
    page_number: u32,
    detect_tables: bool,
    detect_layout: bool,
    detect_math: bool,
) -> Result<PageText, String> {
    extract_page_text_report_with_marker(
        doc,
        page_number,
        detect_tables,
        detect_layout,
        detect_math,
    )
    .map(|(pt, _)| pt)
}

/// Like `extract_page_text_report`, but also returns the caller's page-marker
/// hint for a page the layout batch newly routed (`Some`), so the caller can
/// keep the marker the non-routed string-walker path would have emitted.
pub fn extract_page_text_report_with_marker(
    doc: &Document,
    page_number: u32,
    detect_tables: bool,
    detect_layout: bool,
    detect_math: bool,
) -> Result<(PageText, Option<bool>), String> {
    let pages: std::collections::BTreeMap<u32, ObjectId> = doc.get_pages();
    let page_id = pages
        .get(&page_number)
        .copied()
        .ok_or_else(|| format!("page {page_number} not found"))?;
    extract_page(doc, page_id, detect_tables, detect_layout, detect_math)
}
