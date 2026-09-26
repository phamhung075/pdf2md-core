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

pub(crate) fn get_name<'a>(dict: &'a Dictionary, key: &[u8]) -> Option<&'a [u8]> {
    dict.get(key).ok().and_then(|o| o.as_name().ok())
}

/// Follow indirect references to the underlying object.
pub(crate) fn deref<'d>(doc: &'d Document, obj: &'d Object) -> Option<&'d Object> {
    let mut cur = obj;
    for _ in 0..16 {
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

/// The `/Resources` dictionaries that apply to a page, nearest first (the page
/// itself, then each `/Pages` ancestor up to the root). `lopdf`'s
/// `get_page_resources` only collects ancestor resources that are *indirect*
/// references, so a page whose parent carries `/Resources << ... >>` inline
/// (QZP payslips, some bank exports) resolves to nothing; walking the chain
/// ourselves handles both shapes. Cycle-safe and depth-bounded.
pub(crate) fn resource_dicts<'a>(doc: &'a Document, page_id: ObjectId) -> Vec<&'a Dictionary> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut cur = Some(page_id);
    while let Some(id) = cur {
        if out.len() >= 32 || !seen.insert(id) {
            break;
        }
        let Ok(dict) = doc.get_dictionary(id) else { break };
        if let Some(res) = dict
            .get(b"Resources")
            .ok()
            .and_then(|o| deref(doc, o))
            .and_then(|o| o.as_dict().ok())
        {
            out.push(res);
        }
        cur = dict.get(b"Parent").ok().and_then(|o| o.as_reference().ok());
    }
    out
}

/// Merge the `/Font` entries of a resource chain into `fonts`, nearest-wins and
/// without overwriting a name already found closer to the page.
pub(crate) fn collect_fonts<'a>(
    doc: &'a Document,
    chain: &[&'a Dictionary],
    fonts: &mut BTreeMap<Vec<u8>, &'a Dictionary>,
) {
    for resources in chain {
        let Some(font) = resources
            .get(b"Font")
            .ok()
            .and_then(|o| deref(doc, o))
            .and_then(|o| o.as_dict().ok())
        else {
            continue;
        };
        for (name, value) in font.iter() {
            if fonts.contains_key(name) {
                continue;
            }
            if let Some(fd) = deref(doc, value).and_then(|o| o.as_dict().ok()) {
                fonts.insert(name.clone(), fd);
            }
        }
    }
}

/// Fonts available to a page, resolving `/Resources` inherited from the
/// `/Pages` tree (see [`resource_dicts`]).
pub(crate) fn page_fonts<'a>(
    doc: &'a Document,
    page_id: ObjectId,
) -> BTreeMap<Vec<u8>, &'a Dictionary> {
    let mut fonts = BTreeMap::new();
    let chain = resource_dicts(doc, page_id);
    collect_fonts(doc, &chain, &mut fonts);
    fonts
}

/// The `/Resources` dictionary in effect inside a Form XObject: its own when
/// present, otherwise the enclosing page/form resources.
pub(crate) fn form_resource_chain<'a>(
    doc: &'a Document,
    form: &'a Dictionary,
    parent: &[&'a Dictionary],
) -> Vec<&'a Dictionary> {
    match form
        .get(b"Resources")
        .ok()
        .and_then(|o| deref(doc, o))
        .and_then(|o| o.as_dict().ok())
    {
        Some(res) => vec![res],
        None => parent.to_vec(),
    }
}

/// Resolve a `Do` operand to a Form XObject stream, searching the resource
/// chain nearest-first. The returned id is `Some` when the XObject was an
/// indirect reference, letting callers detect a form that draws itself.
pub(crate) fn lookup_form<'a>(
    doc: &'a Document,
    chain: &[&'a Dictionary],
    name: &[u8],
) -> Option<(Option<ObjectId>, &'a lopdf::Stream)> {
    for resources in chain {
        let Some(xobjects) = resources
            .get(b"XObject")
            .ok()
            .and_then(|o| deref(doc, o))
            .and_then(|o| o.as_dict().ok())
        else {
            continue;
        };
        let Some(value) = xobjects.get(name).ok() else {
            continue;
        };
        let id = value.as_reference().ok();
        if let Some(Object::Stream(stream)) = deref(doc, value) {
            if get_name(&stream.dict, b"Subtype") == Some(b"Form") {
                return Some((id, stream));
            }
        }
    }
    None
}
