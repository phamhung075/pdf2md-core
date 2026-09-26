// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Per-page text-source selection: tagged structure tree vs. the geometry fast path.

use super::*;

/// Minimum number of words a tagged structure tree must resolve before it is
/// preferred over the geometry fast path (guards against a near-empty tree).
const STRUCT_TREE_MIN_WORDS: usize = 5;
/// Minimum coverage of the geometry fast path's word count, in percent, for the
/// structure tree to replace it.
const STRUCT_TREE_COVERAGE_PCT: usize = 85;

/// Extract one page's text, preferring our multilingual decoder and falling
/// back to lopdf only when the page content cannot be parsed at all.
///
/// When the document is tagged (`/StructTreeRoot`, PDF/UA, Word/LibreOffice/
/// Acrobat exports) the structure tree is ground truth for reading order and
/// semantic roles (`H1..H6`, `P`, `Table`, `Figure`, …) — but it is used ONLY
/// when it covers >= 85% of the words the geometry fast path produces,
/// otherwise a partial/decorative structure tree (common in invoice generators)
/// would drop content and regress the tuned geometric engine.
///
/// Returns `(page_text, marker_hint, decode_failed)`.
pub(super) fn select_page_text(
    doc: &lopdf::Document,
    page_num: u32,
    page_id: lopdf::ObjectId,
    options: &ConversionOptions,
    has_struct_tree: bool,
) -> (text_extract::PageText, Option<bool>, bool) {
    let geo_result = text_extract::extract_page_text_report_with_marker(
        doc,
        page_num,
        options.detect_tables,
        options.detect_layout,
        options.detect_math,
    );
    let decode_failed = geo_result.is_err();
    let (geo, geo_hint) = match geo_result {
        Ok(pair) => pair,
        Err(_) => (
            text_extract::PageText {
                text: doc
                    .extract_text_with_limit(&[page_num], text_extract::MAX_PAGE_CONTENT_TOTAL)
                    .unwrap_or_default(),
                text_ops_seen: true,
                has_fonts: !text_extract::page_fonts(doc, page_id).is_empty(),
                tables: 0,
                blocks: Vec::new(),
                budget_exhausted: false,
            },
            None,
        ),
    };
    let selected = if has_struct_tree {
        if let Some(tagged) = layout::extract_tagged_page(doc, page_id, options.detect_math) {
            let geo_words = geo.text.split_whitespace().count();
            if tagged.words >= STRUCT_TREE_MIN_WORDS
                && tagged.words * 100 >= STRUCT_TREE_COVERAGE_PCT * geo_words
            {
                (
                    text_extract::PageText {
                        text: tagged.text,
                        text_ops_seen: true,
                        has_fonts: true,
                        tables: tagged.tables,
                        blocks: tagged.blocks,
                        budget_exhausted: false,
                    },
                    None,
                )
            } else {
                (geo, geo_hint)
            }
        } else {
            (geo, geo_hint)
        }
    } else {
        (geo, geo_hint)
    };
    (selected.0, selected.1, decode_failed)
}
