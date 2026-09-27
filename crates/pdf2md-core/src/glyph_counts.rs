// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Per-page accounting of glyph codes shown under each font.
//!
//! A font whose encoding cannot be resolved into a Unicode [`Codec`]
//! (`resolve_codec` returns `None` — e.g. a Type0 `/Identity-H` font with no
//! `/ToUnicode`, or a genuine dingbat face) is dropped from every text path,
//! so the strings drawn with it vanish with no signal. This module counts how
//! many codes were shown under such a font (`undecodable`) versus under a font
//! that did decode (`decoded`), so the caller can make the loss observable.
//!
//! The count is taken by one canonical pass over the page's own content and
//! every Form XObject it invokes, mirroring the string walker's traversal
//! (same depth / `Do` / byte budgets and per-path cycle guard). Routing may
//! walk a page through the string walker, the layout glyph engine and/or the
//! structure tree; counting once here, from the raw content rather than from a
//! selected path's output, keeps a page from being counted twice and makes the
//! number independent of which path wins.
//!
//! [`Codec`]: crate::text_extract::Codec

use crate::layout::glyph_stream::{resolve_widths, string_bytes, Widths};
use crate::text_extract::{
    collect_fonts, decode_page_content, form_resource_chain, get_name, lookup_form,
    resource_dicts, resolve_codec, Codec, WalkerBudget, FORM_INVOCATION_OPS,
    MAX_WALKER_FORM_DEPTH,
};
use lopdf::content::{Content, Operation};
use lopdf::{Dictionary, Document, Object, ObjectId};
use std::collections::BTreeMap;

/// Bytes per code for a CID-keyed (Type0) font with no usable metrics.
const TYPE0_CODE_BYTES: usize = 2;
/// Bytes per code for a simple font with no usable metrics.
const SIMPLE_CODE_BYTES: usize = 1;

/// Glyph codes shown on one page: those shown under a font with no resolvable
/// Unicode codec (dropped by every text path) and those under a font that
/// decoded.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct GlyphCounts {
    pub(crate) undecodable: usize,
    pub(crate) decoded: usize,
}

/// One collected font's counting facts.
struct FontCounter {
    name: Vec<u8>,
    /// The resolved codec, or `None` when `resolve_codec` returned `None` and
    /// every text path drops this font.
    codec: Option<Codec>,
    /// Advance-width table; its kind is the existing code-width source used
    /// when no codec exists.
    widths: Widths,
    /// True for `Subtype /Type0`, the fallback code width when metrics are
    /// unusable.
    is_type0: bool,
}

impl FontCounter {
    /// Count the codes of one shown string under this font.
    fn count(&self, bytes: &[u8], counts: &mut GlyphCounts) {
        match &self.codec {
            Some(codec) => counts.decoded += codec.code_metrics(bytes).0,
            None => {
                counts.undecodable += match self.widths {
                    // Unusable metrics: fall back to the subtype's fixed code
                    // width (a Type0/CID font is two bytes per code).
                    Widths::None => {
                        let per = if self.is_type0 {
                            TYPE0_CODE_BYTES
                        } else {
                            SIMPLE_CODE_BYTES
                        };
                        bytes.len().div_ceil(per)
                    }
                    // Existing code-width logic, shared with `Widths::width`.
                    _ => self.widths.code_count(bytes),
                }
            }
        }
    }
}

/// Count the glyph codes shown on one page, once, over its content streams and
/// every Form XObject they invoke.
pub(crate) fn count_page_glyphs(doc: &Document, page_id: ObjectId) -> GlyphCounts {
    let chain = resource_dicts(doc, page_id);
    let Ok(content) = decode_page_content(doc, page_id) else {
        return GlyphCounts::default();
    };
    let mut counts = GlyphCounts::default();
    let mut budget = WalkerBudget::new();
    let mut form_path: Vec<ObjectId> = Vec::new();
    count_content(
        doc,
        &chain,
        &content.operations,
        &mut counts,
        &mut form_path,
        0,
        &mut budget,
    );
    counts
}

/// Walk one content stream (page or Form XObject), counting the codes of every
/// `Tj` / `'` / `"` / `TJ`-element shown under the current font. Recursion and
/// budgets mirror [`crate::text_extract::walk_content`] so the count covers
/// exactly the content the walkers can output.
#[allow(clippy::too_many_arguments)]
fn count_content(
    doc: &Document,
    chain: &[&Dictionary],
    ops: &[Operation],
    counts: &mut GlyphCounts,
    form_path: &mut Vec<ObjectId>,
    depth: usize,
    budget: &mut WalkerBudget,
) {
    let mut fonts: BTreeMap<Vec<u8>, &Dictionary> = BTreeMap::new();
    collect_fonts(doc, chain, &mut fonts);
    let table: Vec<FontCounter> = fonts
        .iter()
        .map(|(name, fd)| FontCounter {
            name: name.clone(),
            codec: resolve_codec(doc, fd),
            widths: resolve_widths(doc, fd),
            is_type0: get_name(fd, b"Subtype").map_or(false, |s| s == b"Type0"),
        })
        .collect();

    let mut cur: Option<usize> = None;
    for op in ops {
        if budget.ops_left == 0 {
            budget.exhausted = true;
            break;
        }
        budget.ops_left -= 1;
        match op.operator.as_str() {
            "Tf" => {
                let name = op.operands.first().and_then(|o| o.as_name().ok());
                cur = name.and_then(|nm| table.iter().position(|f| f.name == nm));
            }
            "Tj" | "'" => {
                if let (Some(ci), Some(bytes)) =
                    (cur, op.operands.first().and_then(string_bytes))
                {
                    table[ci].count(bytes, counts);
                }
            }
            "\"" => {
                if let (Some(ci), Some(bytes)) =
                    (cur, op.operands.get(2).and_then(string_bytes))
                {
                    table[ci].count(bytes, counts);
                }
            }
            "TJ" => {
                if let (Some(ci), Some(arr)) =
                    (cur, op.operands.first().and_then(|o| o.as_array().ok()))
                {
                    for item in arr {
                        if let Object::String(bytes, _) = item {
                            table[ci].count(bytes, counts);
                        }
                    }
                }
            }
            // Form XObject text: mirror `walk_content`'s `Do` arm exactly
            // (same bounds and per-path cycle guard) so a form's codes are
            // counted once per invocation.
            "Do" => {
                if budget.do_left == 0 || depth >= MAX_WALKER_FORM_DEPTH {
                    budget.exhausted = true;
                    continue;
                }
                let Some(name) = op.operands.first().and_then(|o| o.as_name().ok()) else {
                    continue;
                };
                let Some((id, form)) = lookup_form(doc, chain, name) else {
                    continue;
                };
                if id.map_or(false, |id| form_path.contains(&id)) {
                    continue;
                }
                let fchain = form_resource_chain(doc, &form.dict, chain);
                let Ok(data) = form.get_plain_content_with_limit(16 << 20) else {
                    continue;
                };
                if data.len() > budget.bytes_left {
                    budget.exhausted = true;
                    continue;
                }
                let Ok(fc) = Content::decode(&data) else {
                    continue;
                };
                budget.bytes_left = budget.bytes_left.saturating_sub(data.len());
                budget.ops_left = budget.ops_left.saturating_sub(FORM_INVOCATION_OPS);
                if budget.ops_left == 0 {
                    budget.exhausted = true;
                }
                budget.do_left -= 1;
                if let Some(id) = id {
                    form_path.push(id);
                }
                count_content(
                    doc,
                    &fchain,
                    &fc.operations,
                    counts,
                    form_path,
                    depth + 1,
                    budget,
                );
                if id.is_some() {
                    form_path.pop();
                }
            }
            _ => {}
        }
    }
}
