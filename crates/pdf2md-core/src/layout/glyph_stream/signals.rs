// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! PDF content-stream glyph extraction, font metric resolution, and 2D span aggregation.

use super::*;

#[allow(dead_code)]
pub(crate) fn page_content_signals(
    doc: &Document,
    page_id: ObjectId,
    ops: &[Operation],
) -> ContentSignals {
    let mut sig = ContentSignals::default();
    let chain = crate::text_extract::resource_dicts(doc, page_id);
    let mut budget = GlyphBudget::new();
    let mut form_path: Vec<ObjectId> = Vec::new();
    scan_content_signals(
        doc,
        &chain,
        ops,
        0,
        &mut form_path,
        &mut budget,
        &mut sig,
    );
    sig.budget_exhausted = budget.exhausted;
    sig
}

#[allow(clippy::too_many_arguments)]
pub(super) fn scan_content_signals(
    doc: &Document,
    chain: &[&Dictionary],
    ops: &[Operation],
    depth: usize,
    form_path: &mut Vec<ObjectId>,
    budget: &mut GlyphBudget,
    sig: &mut ContentSignals,
) {
    for op in ops {
        if budget.ops_left == 0 {
            budget.exhausted = true;
            return;
        }
        budget.ops_left -= 1;
        match op.operator.as_str() {
            "TJ" => sig.has_tj_array = true,
            "Tj" => sig.has_tj_plain = true,
            "'" | "\"" => sig.has_quote = true,
            "TD" => sig.has_td_upper = true,
            "Tm" => sig.has_tm = true,
            "Td" => {
                sig.td_total += 1;
                if let Some(tx) = op.operands.first().and_then(num) {
                    if tx.abs() > 0.01 {
                        sig.td_horizontal += 1;
                    }
                }
            }
            "Do" => {
                if budget.do_left == 0 || depth >= MAX_FORM_DEPTH {
                    budget.exhausted = true;
                    continue;
                }
                let Some(name) = op.operands.first().and_then(|o| o.as_name().ok()) else {
                    continue;
                };
                let Some((id, form)) = crate::text_extract::lookup_form(doc, chain, name) else {
                    continue;
                };
                if id.map_or(false, |id| form_path.contains(&id)) {
                    continue;
                }
                let fchain = crate::text_extract::form_resource_chain(doc, &form.dict, chain);
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
                budget.ops_left = budget
                    .ops_left
                    .saturating_sub(crate::text_extract::FORM_INVOCATION_OPS);
                if budget.ops_left == 0 {
                    budget.exhausted = true;
                }
                budget.do_left -= 1;
                if let Some(id) = id {
                    form_path.push(id);
                }
                scan_content_signals(doc, &fchain, &fc.operations, depth + 1, form_path, budget, sig);
                if id.is_some() {
                    form_path.pop();
                }
            }
            _ => {}
        }
    }
}

/// Rotate every span of a dominantly vertical page upright so the ordinary
/// horizontal pipeline can read it. `upward` selects the reading direction:
/// `(x, y) -> (y, -x)` for text drawn bottom-to-top, `(-y, x)` for top-to-bottom.
/// The rotated spans are translated to a `(0,0)` origin and `Span::is_vertical`
/// is cleared; the returned value is the new display height.
pub(super) fn rotate_spans_upright(spans: &mut [Span], upward: bool) -> f64 {
    let mut min_x = f64::INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    for s in spans.iter_mut() {
        let (nx, ny) = if upward {
            (s.y, -s.x)
        } else {
            (-s.y, s.x)
        };
        s.x = nx;
        s.y = ny;
        s.is_vertical = false;
        min_x = min_x.min(nx);
        min_y = min_y.min(ny);
        max_y = max_y.max(ny);
    }
    if !min_x.is_finite() || !min_y.is_finite() {
        return 0.0;
    }
    for s in spans.iter_mut() {
        s.x -= min_x;
        s.y -= min_y;
    }
    (max_y - min_y).max(1.0)
}
