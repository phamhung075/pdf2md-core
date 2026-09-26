//! pdf2md-core — High-performance native Rust core engine for sub-millisecond
//! PDF-to-Markdown extraction and 2D spatial canvas table reconstruction.
//!
//! Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
//! SPDX-License-Identifier: BSL-1.1
//! Licensed under the Business Source License 1.1 (BSL-1.1).

use super::*;

/// True when a page (or a Form XObject it draws, recursively) references fonts
/// or issues a text-show operator. This is what makes a page "digital" for
/// routing; it must resolve inherited `/Resources` and descend into forms.
pub(super) fn page_has_text_layer(doc: &lopdf::Document, page_id: lopdf::ObjectId) -> bool {
    let chain = text_extract::resource_dicts(doc, page_id);
    let mut fonts = std::collections::BTreeMap::new();
    text_extract::collect_fonts(doc, &chain, &mut fonts);
    if !fonts.is_empty() {
        return true;
    }
    let Ok(content) = text_extract::decode_page_content(doc, page_id) else {
        return false;
    };
    let mut budget = text_extract::WalkerBudget::new();
    let found = content_has_text_layer(
        doc,
        &chain,
        &content.operations,
        &mut Vec::new(),
        0,
        &mut budget,
    );
    // The probe shares the walker's work bounds; a page whose probe was
    // truncated must not fail silently (it routes to rescue on an incomplete
    // scan).
    if budget.exhausted && std::env::var_os("PDF2MD_DEBUG").is_some() {
        eprintln!("pdf2md: digital-text-layer probe hit a work bound; result may be incomplete");
    }
    found
}

pub(super) fn content_has_text_layer(
    doc: &lopdf::Document,
    chain: &[&lopdf::Dictionary],
    ops: &[lopdf::content::Operation],
    form_path: &mut Vec<lopdf::ObjectId>,
    depth: usize,
    budget: &mut text_extract::WalkerBudget,
) -> bool {
    if depth >= text_extract::MAX_WALKER_FORM_DEPTH {
        budget.exhausted = true;
        return false;
    }
    // Scan this stream's operators for a text-show, charging the shared page
    // budget so a form DAG cannot multiply the work (a page whose budget is
    // exhausted is reported as needing rescue rather than walked forever).
    for op in ops {
        if budget.ops_left == 0 {
            budget.exhausted = true;
            return false;
        }
        budget.ops_left -= 1;
        if matches!(op.operator.as_str(), "Tj" | "TJ" | "'" | "\"") {
            return true;
        }
    }
    for op in ops {
        if op.operator != "Do" {
            continue;
        }
        if budget.do_left == 0 {
            budget.exhausted = true;
            break;
        }
        let Some(name) = op.operands.first().and_then(|o| o.as_name().ok()) else {
            continue;
        };
        let Some((id, form)) = text_extract::lookup_form(doc, chain, name) else {
            continue;
        };
        // A form already on the current path draws itself (directly or through
        // a chain); stop instead of recursing.
        if id.map_or(false, |id| form_path.contains(&id)) {
            continue;
        }
        let fchain = text_extract::form_resource_chain(doc, &form.dict, chain);
        let mut fonts = std::collections::BTreeMap::new();
        text_extract::collect_fonts(doc, &fchain, &mut fonts);
        if !fonts.is_empty() {
            return true;
        }
        let Ok(data) = form.get_plain_content_with_limit(16 << 20) else {
            continue;
        };
        if data.len() > budget.bytes_left {
            budget.exhausted = true;
            continue;
        }
        let Ok(fc) = lopdf::content::Content::decode(&data) else {
            continue;
        };
        budget.bytes_left = budget.bytes_left.saturating_sub(data.len());
        budget.ops_left = budget
            .ops_left
            .saturating_sub(text_extract::FORM_INVOCATION_OPS);
        if budget.ops_left == 0 {
            budget.exhausted = true;
        }
        budget.do_left -= 1;
        if let Some(id) = id {
            form_path.push(id);
        }
        let found = content_has_text_layer(
            doc,
            &fchain,
            &fc.operations,
            form_path,
            depth + 1,
            budget,
        );
        if id.is_some() {
            form_path.pop();
        }
        if found {
            return true;
        }
    }
    false
}

/// Probes whether raw PDF bytes contain a digital text stream without full rendering.
pub fn is_digital_pdf_bytes(bytes: &[u8]) -> bool {
    if bytes.len() < 32 || !bytes.starts_with(b"%PDF-") {
        return false;
    }

    // Structural check: parse and look for a real text layer. Unlike a raw
    // string scan, this handles content streams that are FlateDecode-compressed
    // (where "BT"/"Tj" never appear in the raw bytes) — e.g. PDFCreator/
    // Ghostscript tickets and most modern PDFs. A page is "digital" if it
    // references any font (including one inherited from the /Pages tree) or
    // issues any text-show operator, descending recursively into Form XObjects.
    if let Ok(doc) = load_pdf_document(bytes) {
        for (_page_num, page_id) in doc.get_pages().into_iter().take(MAX_PAGES) {
            if page_has_text_layer(&doc, page_id) {
                return true;
            }
        }
        return false;
    }

    // Fallback (failed parse / truncated input): raw markers.
    let text_markers: &[&[u8]] = &[b"BT\n", b"BT\r", b"BT ", b"/Font", b"Tj", b"TJ"];
    let mut matches = 0;
    for marker in text_markers {
        if bytes.windows(marker.len()).any(|w| w == *marker) {
            matches += 1;
        }
    }
    matches >= 2
}
