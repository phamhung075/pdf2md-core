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
use crate::glyph_counts::GlyphCounts;

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
    decode_page_content_bounded(doc, page_id).map(|(content, _)| content)
}

/// [`decode_page_content`] that also reports whether the operator cap truncated
/// the stream (see [`bound_page_content`]). The extraction paths use the flag to
/// set `budget_exhausted`; the media/hash callers ignore it and get the same
/// bounded content as before.
pub(crate) fn decode_page_content_bounded(
    doc: &Document,
    page_id: ObjectId,
) -> lopdf::Result<(Content<Vec<Operation>>, bool)> {
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
    // Bound the operator count *before* lopdf materialises one `Operation` per
    // operator: the existing walker budgets run after decode and cannot stop the
    // allocation (see `content_bound`).
    let truncated = bound_page_content(&mut data);
    Ok((decode_with_comment_fallback(&data)?, truncated))
}

pub(super) fn extract_page(
    doc: &Document,
    page_id: ObjectId,
    detect_tables: bool,
    detect_layout: bool,
    detect_math: bool,
) -> (Result<(PageText, Option<bool>), String>, GlyphCounts) {
    let chain = resource_dicts(doc, page_id);
    let mut fonts: BTreeMap<Vec<u8>, &Dictionary> = BTreeMap::new();
    collect_fonts(doc, &chain, &mut fonts);
    let has_fonts = !fonts.is_empty();

    let (content, content_truncated) = match decode_page_content_bounded(doc, page_id) {
        Ok(c) => c,
        Err(e) => return (Err(format!("{e}")), GlyphCounts::default()),
    };
    // One canonical glyph-code count per page, over the content stream already
    // decoded here — the single pass that always runs for every page, whatever
    // text path `select_page_text` ultimately keeps (see `glyph_counts`). The
    // count is returned even when the text path below fails, matching the
    // previous independent counting pass.
    let glyph_counts = crate::glyph_counts::count_glyphs(doc, &chain, &content.operations, &fonts);

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
    let sig = crate::layout::glyph_stream::page_content_signals(doc, page_id, &content.operations);
    // The routing signal scan shares the same family of bounds as the walkers;
    // a page whose signal scan was truncated may have been misrouted, so carry
    // that fact into the page result too. `content_truncated` (the operator cap
    // applied before decode) is reported the same way.
    let signals_exhausted = sig.budget_exhausted || content_truncated;
    let form_geometry =
        (sig.has_tj_array || sig.has_tj_plain) && (sig.has_td_upper || sig.has_tm);
    let form_plain_td = sig.td_total >= 2 && sig.td_horizontal * 2 >= sig.td_total;
    // Every show-operator positioned with fragment `Td` placements — plain
    // `Tj` (CAF payslips, Engie bills), `TJ` arrays (glyph-positioned
    // statements such as F0686/F0687/F0688), or quote show-ops (`'`/`"`).
    // Routing is gated on `has_plain_td` either way: a page that shows
    // every string with `'`/`Tj`/`TJ` at one `Tm` and only vertical line
    // advances is one-string-per-line prose the geometry path would merge
    // into a single run, so it stays on the string walker. The `TJ` arm was
    // missing, so `TJ`+`Td` pages fell through to the walker, which never
    // runs table detection and emits each visual column as its own line.
    //
    // One source of truth: `with_tj_arm = false` is the exact pre-0.2.9
    // predicate, `true` adds the `TJ` arm. A page routed only by `true` was
    // moved off the walker by that arm, so it needs its walker-only vertical
    // content restored below.
    let routes_with_tj_arm = |with_tj_arm: bool| {
        legacy_geometry
            || (((with_tj_arm && has_tj) || has_tj_plain || has_quote) && has_plain_td)
            || (form_geometry && !legacy_geometry)
            || ((sig.has_tj_plain || sig.has_quote) && form_plain_td)
    };
    let route_to_layout = if detect_tables {
        routes_with_tj_arm(true)
    } else {
        has_tj && !has_tj_plain && has_td
    };
    let newly_routed = detect_tables && route_to_layout && !routes_with_tj_arm(false);
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
                Ok(mut pt) => {
                    // No table recovered, or a decoded digit run is missing:
                    // the walker is the lossless output, so keep it (tables
                    // are not worth unique content).
                    if pt.tables == 0 || loses_digit_content(&walker, &pt.text) {
                        return (
                            Ok((
                                PageText {
                                    text: walker,
                                    text_ops_seen: walker_ops,
                                    has_fonts,
                                    tables: 0,
                                    blocks: Vec::new(),
                                    budget_exhausted: budget.exhausted || signals_exhausted,
                                },
                                marker_hint,
                            )),
                            glyph_counts,
                        );
                    }
                    // A page the `TJ` arm newly moved off the string walker: the
                    // walker emits vertical runs the horizontal reading-order
                    // renderer leaves out — a rotated code in the margin is real
                    // content the walker printed. Put those runs back so
                    // recovering the table does not regress the walker's
                    // content; the margin blocks are still emitted for zone
                    // inspectors. Pages already on the layout engine before the
                    // routing change keep their exact old output.
                    if newly_routed {
                        let vertical: String = pt
                            .blocks
                            .iter()
                            .filter(|b| b.kind == "margin")
                            .map(|b| b.text.trim())
                            .filter(|t| !t.is_empty())
                            .collect::<Vec<_>>()
                            .join("\n");
                        if !vertical.is_empty() {
                            if !pt.text.is_empty() {
                                if !pt.text.ends_with('\n') {
                                    pt.text.push('\n');
                                }
                                pt.text.push('\n');
                            }
                            pt.text.push_str(&vertical);
                        }
                    }
                    return (
                        Ok((
                            PageText {
                                budget_exhausted: pt.budget_exhausted || signals_exhausted,
                                ..pt
                            },
                            marker_hint,
                        )),
                        glyph_counts,
                    );
                }
                Err(e) => return (Err(e), glyph_counts),
            }
        }
        let result = match crate::layout::extract_page_glyphs(
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
                    return (
                        Ok((
                            PageText {
                                budget_exhausted: pt.budget_exhausted || signals_exhausted,
                                ..pt
                            },
                            None,
                        )),
                        glyph_counts,
                    );
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
                    return (
                        Ok((
                            PageText {
                                budget_exhausted: pt.budget_exhausted || signals_exhausted,
                                ..pt
                            },
                            None,
                        )),
                        glyph_counts,
                    );
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
        return (result, glyph_counts);
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

    (
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
        )),
        glyph_counts,
    )
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
    extract_page_report_with_counts(
        doc,
        page_number,
        detect_tables,
        detect_layout,
        detect_math,
    )
    .0
}

/// Like `extract_page_text_report_with_marker`, but also returns the page's
/// canonical glyph-code counts, computed by the same pass that decoded the
/// content stream — so the page is decoded once and counted once. The counts
/// are returned even when the text path fails, matching the previous
/// independent counting pass. Used by `select_page_text`.
pub(crate) fn extract_page_report_with_counts(
    doc: &Document,
    page_number: u32,
    detect_tables: bool,
    detect_layout: bool,
    detect_math: bool,
) -> (Result<(PageText, Option<bool>), String>, GlyphCounts) {
    let pages: std::collections::BTreeMap<u32, ObjectId> = doc.get_pages();
    let Some(page_id) = pages.get(&page_number).copied() else {
        return (
            Err(format!("page {page_number} not found")),
            GlyphCounts::default(),
        );
    };
    extract_page(doc, page_id, detect_tables, detect_layout, detect_math)
}
