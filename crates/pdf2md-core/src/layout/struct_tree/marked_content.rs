// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Tagged-PDF structure-tree reader (ISO 32000 §14.7).
//!
//! When a PDF carries a `/StructTreeRoot` (tagged / PDF/UA documents, plus many
//! Word/LibreOffice/Acrobat exports), the document already encodes the reading
//! order and the semantic role of every element (`H1..H6`, `P`, `Table`, `TR`,
//! `TD`, `Figure`, `Header`, `Footer`, `L`, …) together with `/ActualText`
//! overrides. Re-deriving those from glyph geometry (XY-cut + font-size
//! heuristics) is a guess; the structure tree is ground truth.
//!
//! This reader therefore recovers the block list *and* the markdown for a page
//! directly from the structure tree, using the text layer only to fill in what
//! each marked-content item actually says. It is deliberately conservative: a
//! page only takes this path when the tagged structure resolves to real,
//! non-empty marked-content text, and the caller validates coverage against the
//! geometry fast-path so the (already-tuned) geometric engine is never regressed
//! by a broken/decorative structure tree.
//!
//! Role handling follows the standard PDF role set after expanding the
//! `/RoleMap` (which maps producer-specific role names — e.g. EDF's
//! `SIMM_Lieu_Conso_7.2` — to `TD`/`Text`/…).

use super::*;

// One source of truth for reading a `/Name` entry (shared with `text_extract`).
pub(super) use crate::text_extract::get_name;

// NB: deliberately *not* shared with `text_extract::deref` — this reader uses a
// 20-hop reference limit where the text extractor uses 16, so the two are kept
// separate rather than silently changing structure-tree resolution depth.
pub(super) fn deref<'d>(doc: &'d Document, obj: &'d Object) -> Option<&'d Object> {
    let mut cur = obj;
    for _ in 0..20 {
        match cur {
            Object::Reference(id) => match doc.get_object(*id) {
                Ok(next) => cur = next,
                Err(_) => return None,
            },
            _ => return Some(cur),
        }
    }
    None
}

pub(super) fn as_dict<'d>(doc: &'d Document, obj: &'d Object) -> Option<&'d Dictionary> {
    deref(doc, obj).and_then(|o| o.as_dict().ok())
}

pub(super) fn mtx_from_op(op: &lopdf::content::Operation) -> Option<Mtx> {
    Some(Mtx::from_parts(
        num(op.operands.get(0)?)?,
        num(op.operands.get(1)?)?,
        num(op.operands.get(2)?)?,
        num(op.operands.get(3)?)?,
        num(op.operands.get(4)?)?,
        num(op.operands.get(5)?)?,
    ))
}

/// Find a `/MCID` in an operand list (the marked-content property dict, which a
/// generator may attach to the `BDC` or leak onto the following text-show op).
pub(super) fn mcid_in_operands(doc: &Document, operands: &[Object]) -> Option<i64> {
    operands.iter().find_map(|x| match deref(doc, x) {
        Some(Object::Dictionary(d)) => d.get(b"MCID").ok().and_then(|v| v.as_i64().ok()),
        _ => None,
    })
}

/// Decode every string/array operand of a text-show op into spans. Scanning the
/// whole operand list (rather than a fixed index) tolerates the marked-content
/// property dict that lopdf leaks onto the show operator.
pub(super) fn emit_show(
    fonts_info: &[(Vec<u8>, Codec, Widths, (bool, bool))],
    cur_font: Option<usize>,
    operands: &[Object],
    tm: &Mtx,
    ctm: &Mtx,
    tfs: f64,
    hz: f64,
    tc: f64,
    tw: f64,
    emit: &mut dyn FnMut(&Span),
) {
    let Some(ci) = cur_font else { return };
    let (_, codec, width, style) = &fonts_info[ci];
    for operand in operands {
        match operand {
            Object::String(bytes, _) => {
                let mut spans = Vec::new();
                push_span(codec, width, bytes, 0.0, tm, ctm, tfs, hz, tc, tw, *style, &mut spans);
                for s in spans {
                    emit(&s);
                }
            }
            Object::Array(items) => {
                let mut offset = 0.0f64;
                for item in items {
                    match item {
                        Object::String(bytes, _) => {
                            let mut spans = Vec::new();
                            push_span(codec, width, bytes, offset, tm, ctm, tfs, hz, tc, tw, *style, &mut spans);
                            for s in spans {
                                emit(&s);
                            }
                            let w = width.width(bytes).unwrap_or(500.0);
                            let adv = if (tc != 0.0 || tw != 0.0) && tfs > 0.0 {
                                let sc = bytes.iter().filter(|&&b| b == b' ').count() as f64;
                                let cc = bytes.len() as f64;
                                w + (tc * cc + tw * sc) / tfs * 1000.0
                            } else {
                                w
                            };
                            offset += adv;
                        }
                        Object::Integer(v) => offset -= *v as f64,
                        Object::Real(v) => offset -= *v as f64,
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
}

/// Walk the page content stream, tracking the `/MCID` marked-content stack, and
/// return the decoded text + bbox accumulating under each MCID.
pub(super) fn extract_marked_content(doc: &Document, page_id: ObjectId) -> HashMap<usize, McidText> {
    let mut map: HashMap<usize, McidText> = HashMap::new();

    let fonts = match doc.get_page_fonts(page_id) {
        Ok(f) => f,
        Err(e) => {
            if std::env::var("PDF2MD_STRUCT_DEBUG").is_ok() {
                eprintln!("[struct] get_page_fonts error: {:?}", e);
            }
            return map;
        }
    };
    let mut fonts_info: Vec<(Vec<u8>, Codec, Widths, (bool, bool))> = Vec::new();
    for (name, fd) in &fonts {
        if let Some(c) = resolve_codec(doc, fd) {
            fonts_info.push((name.clone(), c, resolve_widths(doc, fd), resolve_font_style(doc, fd)));
        }
    }
    if fonts_info.is_empty() {
        return map;
    }
    let content = match crate::text_extract::decode_page_content(doc, page_id) {
        Ok(c) => c,
        Err(e) => {
            if std::env::var("PDF2MD_STRUCT_DEBUG").is_ok() {
                eprintln!("[struct] decode_page_content error: {:?}", e);
            }
            return map;
        }
    };

    let mut mcid_stack: Vec<i64> = Vec::new();
    let mut ctm = Mtx::ID;
    let mut ctm_stack: Vec<Mtx> = Vec::new();
    let mut tlm = Mtx::ID;
    let mut tm = Mtx::ID;
    let mut tfs = 0.0f64;
    let mut leading = 0.0f64;
    let mut tc = 0.0f64;
    let mut tw = 0.0f64;
    let mut hz = 1.0f64;
    let mut cur_font: Option<usize> = None;

    let target = |stack: &[i64]| -> Option<usize> {
        stack.last().and_then(|&m| if m >= 0 { Some(m as usize) } else { None })
    };

    for op in &content.operations {
        match op.operator.as_str() {
            "q" => ctm_stack.push(ctm),
            "Q" => {
                if let Some(m) = ctm_stack.pop() {
                    ctm = m;
                }
            }
            "cm" => {
                if let Some(m) = mtx_from_op(op) {
                    ctm.pre_mul(&m);
                }
            }
            "BT" => {
                tlm = Mtx::ID;
                tm = Mtx::ID;
            }
            "Tm" => {
                if let Some(m) = mtx_from_op(op) {
                    tlm = m;
                    tm = m;
                }
            }
            "Td" | "TD" => {
                if let (Some(tx), Some(ty)) = (op.operands.first().and_then(num), op.operands.get(1).and_then(num)) {
                    tlm.pre_mul(&Mtx::translate(tx, ty));
                    tm = tlm;
                    if op.operator == "TD" {
                        leading = -ty;
                    }
                }
            }
            "T*" => {
                tlm.pre_mul(&Mtx::translate(0.0, -leading));
                tm = tlm;
            }
            "TL" => {
                if let Some(v) = op.operands.first().and_then(num) {
                    leading = v;
                }
            }
            "Tc" => {
                if let Some(v) = op.operands.first().and_then(num) {
                    tc = v;
                }
            }
            "Tw" => {
                if let Some(v) = op.operands.first().and_then(num) {
                    tw = v;
                }
            }
            "Tz" => {
                if let Some(v) = op.operands.first().and_then(num) {
                    hz = v / 100.0;
                }
            }
            "Tf" => {
                let name = op.operands.first().and_then(|o| o.as_name().ok());
                cur_font = name.and_then(|nm| fonts_info.iter().position(|f| f.0.as_slice() == nm));
                if let Some(sz) = op.operands.get(1).and_then(num) {
                    tfs = sz;
                }
            }
            "BMC" => mcid_stack.push(-1),
            "BDC" => {
                mcid_stack.push(mcid_in_operands(doc, &op.operands).unwrap_or(-1));
            }
            "EMC" => {
                mcid_stack.pop();
            }
            "Tj" | "'" | "\"" => {
                if op.operator == "'" {
                    tlm.pre_mul(&Mtx::translate(0.0, -leading));
                    tm = tlm;
                }
                // Dual MCID resolution. A generator may attach the property dict
                // to the `BDC` (EDF: operands *before* the operator) or leak it
                // onto this text-show itself (standard `BDC /Tag << /MCID n >>`).
                let own_mcid = mcid_in_operands(doc, &op.operands);
                if let Some(m) = own_mcid {
                    if let Some(top) = mcid_stack.last_mut() {
                        *top = m;
                    }
                }
                let eff = own_mcid.map(|m| m as usize).or_else(|| target(&mcid_stack));
                emit_show(&fonts_info, cur_font, &op.operands, &tm, &ctm, tfs, hz, tc, tw, &mut |s: &Span| {
                    if let Some(m) = eff {
                        map.entry(m).or_default().add_span(s);
                    }
                });
            }
            "TJ" => {
                let own_mcid = mcid_in_operands(doc, &op.operands);
                if let Some(m) = own_mcid {
                    if let Some(top) = mcid_stack.last_mut() {
                        *top = m;
                    }
                }
                let eff = own_mcid.map(|m| m as usize).or_else(|| target(&mcid_stack));
                emit_show(&fonts_info, cur_font, &op.operands, &tm, &ctm, tfs, hz, tc, tw, &mut |s: &Span| {
                    if let Some(m) = eff {
                        map.entry(m).or_default().add_span(s);
                    }
                });
            }
            _ => {}
        }
    }

    map
}

// ---------------------------------------------------------------------------
// Block building + markdown rendering
// ---------------------------------------------------------------------------
