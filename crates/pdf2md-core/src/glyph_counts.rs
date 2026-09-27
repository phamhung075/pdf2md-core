// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Per-page accounting of glyph codes shown under each font.
//!
//! A font whose encoding cannot be resolved into a Unicode [`Codec`]
//! (`resolve_codec` returns `None` — e.g. a Type0 `/Identity-H` font with no
//! `/ToUnicode`) is dropped from every text path, so the strings drawn with it
//! vanish with no signal. This module counts how many codes were shown under
//! such a font (`undecodable`) versus under a font that did decode (`decoded`),
//! so the caller can make the loss observable. A simple dingbat/icon face
//! (`is_dingbat_face`) is rejected on purpose and is not text loss, so its
//! codes count toward neither total; a Type0 face is rejected for its missing
//! `/ToUnicode` instead, so it always counts as undecodable.
//!
//! The count is taken by one canonical pass over the page's own content and
//! every Form XObject it invokes, mirroring the string walker's traversal
//! (same depth / `Do` / byte budgets and per-path cycle guard). Routing may
//! walk a page through the string walker, the layout glyph engine and/or the
//! structure tree, and each of those paths decodes the content stream; to keep
//! a page from being counted twice and to avoid a second decode, the caller
//! hands this module the operations `extract_page` already decoded (the pass
//! that always runs for every page), so the count is independent of which path
//! wins.
//!
//! [`Codec`]: crate::text_extract::Codec

use crate::layout::glyph_stream::{resolve_widths, string_bytes, Widths};
use crate::text_extract::{
    collect_fonts, form_resource_chain, get_name, is_dingbat_face, lookup_form, resolve_codec,
    Codec, WalkerBudget, FORM_INVOCATION_OPS, MAX_WALKER_FORM_DEPTH,
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
    /// True for a face `is_dingbat_face` rejects (Symbol / ZapfDingbats /
    /// Wingdings / pictogram faces) on the *simple-font* path, where
    /// `resolve_codec` returns `None` precisely because of that predicate.
    /// Such a face draws icons, not text: its codes are deliberately not
    /// decoded, so they are counted as neither decoded nor undecodable — only
    /// glyphs that a text path *could* have decoded belong in the loss ratio.
    ///
    /// A Type0 face is rejected for a different reason (no `/ToUnicode`), and
    /// `resolve_codec` never consults `is_dingbat_face` on that path, so a
    /// Type0 symbol-BaseFont face still counts as undecodable.
    dingbat: bool,
}

impl FontCounter {
    /// Count the codes of one shown string under this font.
    fn count(&self, bytes: &[u8], counts: &mut GlyphCounts) {
        match &self.codec {
            Some(codec) => counts.decoded += codec.code_metrics(bytes).0,
            None => {
                // Dingbat/icon faces are rejected by design: their codes are
                // not text loss, so they count toward neither total.
                if self.dingbat {
                    return;
                }
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

/// Count the glyph codes shown on one page, once, over its already-decoded
/// content operations and every Form XObject they invoke.
///
/// `chain` is the page's resource chain (from `resource_dicts`), `page_fonts`
/// the font map `extract_page` already collected for it, and `ops` the
/// operations it decoded — so the page content stream is neither decompressed
/// nor scanned a second time.
pub(crate) fn count_glyphs(
    doc: &Document,
    chain: &[&Dictionary],
    ops: &[Operation],
    page_fonts: &BTreeMap<Vec<u8>, &Dictionary>,
) -> GlyphCounts {
    let table = build_table(doc, page_fonts);
    let mut counts = GlyphCounts::default();
    let mut budget = WalkerBudget::new();
    let mut form_path: Vec<ObjectId> = Vec::new();
    walk_ops(
        doc,
        chain,
        ops,
        &table,
        &mut counts,
        &mut form_path,
        0,
        &mut budget,
    );
    counts
}

/// Resolve one content stream's collected fonts into counting facts.
fn build_table(
    doc: &Document,
    fonts: &BTreeMap<Vec<u8>, &Dictionary>,
) -> Vec<FontCounter> {
    fonts
        .iter()
        .map(|(name, fd)| {
            let is_type0 = get_name(fd, b"Subtype").map_or(false, |s| s == b"Type0");
            let codec = resolve_codec(doc, fd);
            // The advance widths are read only when there is no codec; skip the
            // (comparatively costly) `/W` parse for every decoded font.
            let widths = if codec.is_none() {
                resolve_widths(doc, fd)
            } else {
                Widths::None
            };
            FontCounter {
                name: name.clone(),
                codec,
                widths,
                is_type0,
                // `resolve_codec` rejects a *simple* symbolic face solely
                // because of this predicate; a Type0 face is rejected for its
                // missing ToUnicode, so it still counts even with a symbol
                // BaseFont.
                dingbat: !is_type0 && is_dingbat_face(fd),
            }
        })
        .collect()
}

/// Walk one content stream (page or Form XObject), counting the codes of every
/// `Tj` / `'` / `"` / `TJ`-element shown under the current font. Recursion and
/// budgets mirror [`crate::text_extract::walk_content`] so the count covers
/// exactly the content the walkers can output.
#[allow(clippy::too_many_arguments)]
fn walk_ops(
    doc: &Document,
    chain: &[&Dictionary],
    ops: &[Operation],
    table: &[FontCounter],
    counts: &mut GlyphCounts,
    form_path: &mut Vec<ObjectId>,
    depth: usize,
    budget: &mut WalkerBudget,
) {
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
                let mut form_fonts: BTreeMap<Vec<u8>, &Dictionary> = BTreeMap::new();
                collect_fonts(doc, &fchain, &mut form_fonts);
                let form_table = build_table(doc, &form_fonts);
                walk_ops(
                    doc,
                    &fchain,
                    &fc.operations,
                    &form_table,
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
