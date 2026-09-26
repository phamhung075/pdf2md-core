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

/// Locate the `/StructTreeRoot` object id by scanning for its type marker.
pub(super) fn struct_root_id(doc: &Document) -> Option<ObjectId> {
    // We cache nothing: a fresh lookup per page is cheap relative to parsing.
    // Locate the /StructTreeRoot object by scanning for its type marker.
    for (id, obj) in doc.objects.iter() {
        if let Object::Dictionary(d) = obj {
            if d.get(b"Type").ok().and_then(|o| o.as_name().ok()).map(|n| n == b"StructTreeRoot").unwrap_or(false) {
                return Some(*id);
            }
        }
    }
    None
}

/// Cheap presence probe: does the document carry a tagged `/StructTreeRoot`?
/// Callers use this once to avoid re-scanning objects for every page of an
/// untagged document.
pub fn has_struct_tree(doc: &Document) -> bool {
    struct_root_id(doc).is_some()
}

/// Expand the `/RoleMap`: producer role name -> standard role name.
pub(super) fn rolemap(doc: &Document, root: &Dictionary) -> HashMap<Vec<u8>, Vec<u8>> {
    let mut map = HashMap::new();
    if let Ok(rm) = root.get(b"RoleMap") {
        if let Some(rmd) = as_dict(doc, rm) {
            for (k, v) in rmd {
                if let Some(Object::Name(n)) = deref(doc, v) {
                    map.insert(k.clone(), n.clone());
                }
            }
        }
    }
    map
}

pub(super) fn standard_role(role: &[u8], map: &HashMap<Vec<u8>, Vec<u8>>) -> Vec<u8> {
    map.get(role).cloned().unwrap_or_else(|| role.to_ascii_uppercase())
}

/// Parse one `/K` operand (array / integer / dict / ref) into `Node`s, in order.
pub(super) fn parse_nodes(doc: &Document, obj: &Object, role: &[u8], map: &HashMap<Vec<u8>, Vec<u8>>, out: &mut Vec<Node>) {
    match deref(doc, obj) {
        Some(Object::Array(items)) => {
            for it in items {
                parse_nodes(doc, it, role, map, out);
            }
        }
        Some(Object::Integer(i)) => {
            if *i >= 0 {
                out.push(Node::Mcid { role: role.to_vec(), mcid: *i as usize, actual_text: None });
            }
        }
        Some(Object::Dictionary(d)) => {
            // A marked-content reference (`/Type /MCR`).
            if let Some(mcid) = d.get(b"MCID").ok().and_then(|v| v.as_i64().ok()) {
                if mcid >= 0 {
                    out.push(Node::Mcid {
                        role: role.to_vec(),
                        mcid: mcid as usize,
                        actual_text: actual_text_of(d),
                    });
                    return;
                }
            }
            // A nested StructElem.
            let this_role = standard_role(get_name(d, b"S").unwrap_or(b""), map);
            let child_role = if this_role.is_empty() { role.to_vec() } else { this_role };
            let mut children = Vec::new();
            if let Ok(ks) = d.get(b"K") {
                parse_nodes(doc, ks, &child_role, map, &mut children);
            }
            out.push(Node::Elem {
                role: child_role.clone(),
                actual_text: actual_text_of(d),
                children,
            });
        }
        _ => {}
    }
}

pub(super) fn actual_text_of(d: &Dictionary) -> Option<String> {
    if let Ok(o) = d.get(b"ActualText") {
        if let Ok(s) = o.as_str() {
            return Some(String::from_utf8_lossy(s).into_owned());
        }
    }
    if let Ok(a) = d.get(b"A") {
        if let Ok(ad) = a.as_dict() {
            if let Ok(o) = ad.get(b"ActualText") {
                if let Ok(s) = o.as_str() {
                    return Some(String::from_utf8_lossy(s).into_owned());
                }
            }
        }
    }
    None
}

/// The top-level structure elements for a page (in order), via `/ParentTree`.
pub(super) fn page_elements(doc: &Document, root: &Dictionary, page_id: ObjectId) -> Vec<Object> {
    let page_index = doc.get_pages().iter().find(|(_, p)| **p == page_id).map(|(i, _)| (*i as i64).saturating_sub(1));
    if let Some(pi) = page_index {
        if let Ok(pt) = root.get(b"ParentTree") {
            if let Some(v) = number_tree_value(doc, pt, pi) {
                let mut els = Vec::new();
                if let Some(arr) = v.as_array().ok() {
                    // Some producers repeat the same StructElem reference in a
                    // page's ParentTree array (observed with GnuAccounting:
                    // `33 0 R 33 0 R 33 0 R`, `64 0 R` nine times). The tree
                    // is malformed but still resolves, and walking an element
                    // once per duplicate reference re-emits the same
                    // marked-content text — a multi-line table cell or a
                    // letterhead ends up duplicated in the markdown. Dedupe by
                    // object identity (references) / structural form (inline
                    // dictionaries) while preserving the original order.
                    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
                    for it in arr {
                        let key = match it {
                            Object::Reference(id) => format!("R{}:{}", id.0, id.1),
                            _ => format!("{:?}", it),
                        };
                        if seen.insert(key) {
                            els.push(it.clone());
                        }
                    }
                } else {
                    els.push(v);
                }
                if !els.is_empty() {
                    return els;
                }
            }
        }
    }
    Vec::new()
}

/// Look up a key in a PDF number tree (`/Nums` leaves with optional `/Kids`).
pub(super) fn number_tree_value(doc: &Document, root: &Object, key: i64) -> Option<Object> {
    match deref(doc, root) {
        Some(Object::Dictionary(d)) => {
            if let Ok(nums) = d.get(b"Nums") {
                if let Ok(arr) = nums.as_array() {
                    for pair in arr.chunks(2) {
                        if pair.len() == 2 {
                            if let Ok(k) = pair[0].as_i64() {
                                if k == key {
                                    return Some(pair[1].clone());
                                }
                            }
                        }
                    }
                    return None;
                }
            }
            if let Ok(kids) = d.get(b"Kids") {
                if let Ok(karr) = kids.as_array() {
                    for kid in karr {
                        if let Some(kd) = as_dict(doc, kid) {
                            let (lo, hi) = kd.get(b"Limits").ok().and_then(|l| l.as_array().ok()).and_then(|a| {
                                Some((a.first().and_then(|x| x.as_i64().ok()), a.get(1).and_then(|x| x.as_i64().ok())))
                            }).unwrap_or((None, None));
                            if let (Some(lo), Some(hi)) = (lo, hi) {
                                if key >= lo && key <= hi {
                                    if let Some(v) = number_tree_value(doc, &Object::Dictionary(kd.clone()), key) {
                                        return Some(v);
                                    }
                                }
                            }
                        }
                    }
                }
            }
            None
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Marked-content text extraction (per MCID)
// ---------------------------------------------------------------------------
