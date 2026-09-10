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

use std::collections::HashMap;

use lopdf::{Dictionary, Document, Object, ObjectId};

use crate::layout::glyph_stream::{num, resolve_font_style, resolve_widths, push_span, Mtx, Span, Widths};
use crate::layout::reading_order::DocBlock;
use crate::models::CanvasTable;
use crate::text_extract::{resolve_codec, Codec};

/// The decoded text plus a device-space bounding box for one marked-content range.
#[derive(Clone, Debug, Default)]
struct McidText {
    text: String,
    min_x: f64,
    min_y: f64,
    max_x: f64,
    max_y: f64,
    words: usize,
    has_bbox: bool,
    prev_end_x: f64,
    prev_advance: f64,
    prev_size: f64,
    has_prev: bool,
    /// Glyph spans accumulated under this MCID (used for LaTeX math synthesis).
    spans: Vec<Span>,
}

impl McidText {
    fn add_span(&mut self, s: &Span) {
        if s.text.is_empty() {
            return;
        }
        self.spans.push(s.clone());
        // Preserve spaces encoded in the decoded strings (producers usually emit
        // them), but infer a word break from a horizontal gap when consecutive
        // spans carry no space of their own (glyph-positioned text). Collapse
        // runs of spaces so stray alignment glyphs never double a gap.
        let gap = s.x - self.prev_end_x;
        let space_adv = 0.25 * s.size;
        let wants_space = self.has_prev
            && (gap - self.prev_advance > 0.65 * space_adv || gap > 2.5 * s.size.max(self.prev_size));
        if wants_space && !self.text.is_empty() && !self.text.ends_with(' ') {
            self.text.push(' ');
        }
        for ch in s.text.chars() {
            if ch == ' ' {
                if !self.text.is_empty() && !self.text.ends_with(' ') {
                    self.text.push(' ');
                }
            } else {
                self.text.push(ch);
            }
        }
        self.words += s.text.split_whitespace().count();
        if !self.has_bbox {
            self.min_x = s.x;
            self.min_y = s.y;
            self.max_x = s.x + s.advance;
            self.max_y = s.y + s.size;
            self.has_bbox = true;
        } else {
            self.min_x = self.min_x.min(s.x);
            self.min_y = self.min_y.min(s.y);
            self.max_x = self.max_x.max(s.x + s.advance);
            self.max_y = self.max_y.max(s.y + s.size);
        }
        self.prev_end_x = s.x + s.advance;
        self.prev_advance = s.advance;
        self.prev_size = s.size;
        self.has_prev = true;
    }

    /// The text used to build block content: the accumulated string, or —
    /// when LaTeX math synthesis is enabled and it actually synthesises `$...$`
    /// — its math-annotated form. The original `text` is kept for word counting
    /// and the geometry coverage gate, and for plain marked-content runs (no
    /// detected math) so a plain line is never re-rendered or re-spaced.
    fn math_text(&self, detect_math: bool) -> String {
        if detect_math && !self.spans.is_empty() {
            let s = crate::layout::latex_math::synthesize_spans_math(&self.spans, &[]);
            if s.contains('$') {
                return s;
            }
        }
        self.text.clone()
    }
}

/// Result of extracting one page from the structure tree.
pub struct TaggedPage {
    pub text: String,
    pub blocks: Vec<DocBlock>,
    pub tables: usize,
    pub words: usize,
}

// ---------------------------------------------------------------------------
// Structure tree parsing
// ---------------------------------------------------------------------------

fn get_name<'a>(d: &'a Dictionary, key: &[u8]) -> Option<&'a [u8]> {
    d.get(key).ok().and_then(|o| o.as_name().ok())
}

fn deref<'d>(doc: &'d Document, obj: &'d Object) -> Option<&'d Object> {
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

fn as_dict<'d>(doc: &'d Document, obj: &'d Object) -> Option<&'d Dictionary> {
    deref(doc, obj).and_then(|o| o.as_dict().ok())
}

/// Locate the `/StructTreeRoot` object id by scanning for its type marker.
fn struct_root_id(doc: &Document) -> Option<ObjectId> {
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
fn rolemap(doc: &Document, root: &Dictionary) -> HashMap<Vec<u8>, Vec<u8>> {
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

fn standard_role(role: &[u8], map: &HashMap<Vec<u8>, Vec<u8>>) -> Vec<u8> {
    map.get(role).cloned().unwrap_or_else(|| role.to_ascii_uppercase())
}

/// Recursive structure-tree node.
#[derive(Clone, Debug)]
enum Node {
    /// A semantic element (may have children; typically no MCID of its own).
    Elem {
        role: Vec<u8>,
        actual_text: Option<String>,
        children: Vec<Node>,
    },
    /// A leaf marked-content run.
    Mcid {
        role: Vec<u8>,
        mcid: usize,
        actual_text: Option<String>,
    },
}

/// Parse one `/K` operand (array / integer / dict / ref) into `Node`s, in order.
fn parse_nodes(doc: &Document, obj: &Object, role: &[u8], map: &HashMap<Vec<u8>, Vec<u8>>, out: &mut Vec<Node>) {
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

fn actual_text_of(d: &Dictionary) -> Option<String> {
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
fn page_elements(doc: &Document, root: &Dictionary, page_id: ObjectId) -> Vec<Object> {
    let page_index = doc.get_pages().iter().find(|(_, p)| **p == page_id).map(|(i, _)| (*i as i64).saturating_sub(1));
    if let Some(pi) = page_index {
        if let Ok(pt) = root.get(b"ParentTree") {
            if let Some(v) = number_tree_value(doc, pt, pi) {
                let mut els = Vec::new();
                if let Some(arr) = v.as_array().ok() {
                    for it in arr {
                        els.push(it.clone());
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
fn number_tree_value(doc: &Document, root: &Object, key: i64) -> Option<Object> {
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

fn mtx_from_op(op: &lopdf::content::Operation) -> Option<Mtx> {
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
fn mcid_in_operands(doc: &Document, operands: &[Object]) -> Option<i64> {
    operands.iter().find_map(|x| match deref(doc, x) {
        Some(Object::Dictionary(d)) => d.get(b"MCID").ok().and_then(|v| v.as_i64().ok()),
        _ => None,
    })
}

/// Decode every string/array operand of a text-show op into spans. Scanning the
/// whole operand list (rather than a fixed index) tolerates the marked-content
/// property dict that lopdf leaks onto the show operator.
fn emit_show(
    fonts_info: &[(Vec<u8>, Codec, Widths, (bool, bool))],
    cur_font: Option<usize>,
    operands: &[Object],
    tm: &Mtx,
    ctm: &Mtx,
    tfs: f64,
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
                push_span(codec, width, bytes, 0.0, tm, ctm, tfs, *style, &mut spans);
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
                            push_span(codec, width, bytes, offset, tm, ctm, tfs, *style, &mut spans);
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
fn extract_marked_content(doc: &Document, page_id: ObjectId) -> HashMap<usize, McidText> {
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
    let content = match doc.get_and_decode_page_content(page_id) {
        Ok(c) => c,
        Err(e) => {
            if std::env::var("PDF2MD_STRUCT_DEBUG").is_ok() {
                eprintln!("[struct] get_and_decode_page_content error: {:?}", e);
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
                    let _ = v;
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
                emit_show(&fonts_info, cur_font, &op.operands, &tm, &ctm, tfs, tc, tw, &mut |s: &Span| {
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
                emit_show(&fonts_info, cur_font, &op.operands, &tm, &ctm, tfs, tc, tw, &mut |s: &Span| {
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

fn heading_level(role: &[u8]) -> Option<u8> {
    if role == b"H" {
        return Some(1);
    }
    if role.len() >= 2 && role[0] == b'H' && role[1..].iter().all(|c| c.is_ascii_digit()) {
        return std::str::from_utf8(&role[1..]).ok().and_then(|s| s.parse::<u8>().ok()).map(|l| l.clamp(1, 6));
    }
    None
}

/// Gather the concatenated marked-content text and bbox for every descendant
/// `Mcid` of a node, in order. `/ActualText` on the node overrides the text.
/// When `detect_math` is set the per-MCID text is synthesised through the
/// LaTeX math AST first.
fn gather(
    node: &Node,
    map: &HashMap<usize, McidText>,
    detect_math: bool,
) -> (String, Option<(f64, f64, f64, f64)>) {
    let mut text = String::new();
    let mut bx: Option<(f64, f64, f64, f64)> = None;
    gather_into(node, map, &mut text, &mut bx, detect_math);
    (text, bx)
}

#[allow(clippy::too_many_arguments)]
fn gather_into(
    node: &Node,
    map: &HashMap<usize, McidText>,
    text: &mut String,
    bx: &mut Option<(f64, f64, f64, f64)>,
    detect_math: bool,
) {
    match node {
        Node::Mcid { mcid, .. } => {
            if let Some(mt) = map.get(mcid) {
                let emit = mt.math_text(detect_math);
                if !emit.is_empty() {
                    if !text.is_empty() && !text.ends_with(' ') && !emit.starts_with(' ') {
                        text.push(' ');
                    }
                    text.push_str(&emit.trim_end());
                }
                if mt.has_bbox {
                    let b = (mt.min_x, mt.min_y, mt.max_x, mt.max_y);
                    *bx = Some(match *bx {
                        Some((x0, y0, x1, y1)) => (x0.min(b.0), y0.min(b.1), x1.max(b.2), y1.max(b.3)),
                        None => b,
                    });
                }
            }
        }
        Node::Elem { children, .. } => {
            for c in children {
                gather_into(c, map, text, bx, detect_math);
            }
        }
    }
}

/// A semantic block with its role (kind), optional heading level, text, and bbox.
struct Block {
    kind: String,
    level: Option<u8>,
    text: String,
    bx: Option<(f64, f64, f64, f64)>,
}

fn classify_role(role: &[u8]) -> (&'static str, Option<u8>) {
    match role {
        b"H" | b"H1" | b"H2" | b"H3" | b"H4" | b"H5" | b"H6" => ("heading", heading_level(role)),
        b"L" | b"LI" | b"UL" | b"OL" | b"LBody" => ("list", None),
        b"Table" | b"THead" | b"TBody" | b"TFoot" | b"TR" | b"TD" | b"TH" => ("table", None),
        b"Figure" | b"Fig" => ("figure", None),
        b"Header" | b"Head" => ("header", None),
        b"Footer" | b"Foot" => ("footer", None),
        b"TOC" => ("toc", None),
        b"P" | b"Text" | b"Span" | b"Lbl" | b"Note" | b"Quote" | b"BlockQuote" | b"Sidebar" => ("body", None),
        _ => ("body", None),
    }
}

/// Reconstruct a GFM pipe table from a `Table` node's `TR`/`TD`/`TH` structure.
fn table_from_node(node: &Node, map: &HashMap<usize, McidText>, detect_math: bool) -> Option<String> {
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut heads: Option<Vec<String>> = None;
    let mut is_head = false;
    collect_table_rows(node, map, &mut rows, &mut heads, &mut is_head, detect_math);
    if rows.is_empty() {
        return None;
    }
    if let Some(h) = heads.take() {
        rows.insert(0, h);
    }
    // Drop rows with no meaningful content.
    let rows: Vec<Vec<String>> = rows
        .into_iter()
        .filter(|r| r.iter().any(|c| !c.trim().is_empty()))
        .collect();
    if rows.len() < 2 {
        return None;
    }
    let table = CanvasTable::new(rows, crate::models::BoundingBox::new(0.0, 0.0, 0.0, 0.0));
    Some(table.to_markdown())
}

fn collect_table_rows(node: &Node, map: &HashMap<usize, McidText>, rows: &mut Vec<Vec<String>>, heads: &mut Option<Vec<String>>, is_head: &mut bool, detect_math: bool) {
    match node {
        Node::Elem { role, children, .. } => {
            if role == b"TR" {
                let mut cells: Vec<String> = Vec::new();
                // A TR typically has TD/TH children.
                let mut row_head = false;
                for c in children {
                    match c {
                        Node::Elem { role: r2, .. } if r2 == b"TD" || r2 == b"TH" => {
                            let (t, _) = gather(c, map, detect_math);
                            if let Some(at) = c.actual_text_ref() {
                                cells.push(at.clone());
                            } else {
                                cells.push(t.trim().to_string());
                            }
                            if r2 == b"TH" {
                                row_head = true;
                            }
                        }
                        _ => {
                            // Non-cell children — ignore for cell columns.
                        }
                    }
                }
                if !cells.is_empty() {
                    if row_head {
                        *heads = Some(cells);
                    } else {
                        rows.push(cells);
                    }
                }
            } else {
                if role == b"THead" { *is_head = true; }
                for c in children {
                    collect_table_rows(c, map, rows, heads, is_head, detect_math);
                }
                if role == b"THead" { *is_head = false; }
            }
        }
        _ => {}
    }
}

// Helper trait to read a node's ActualText without matching the enum repeatedly.
impl Node {
    fn actual_text_ref(&self) -> Option<&String> {
        match self {
            Node::Elem { actual_text, .. } => actual_text.as_ref(),
            Node::Mcid { actual_text, .. } => actual_text.as_ref(),
        }
    }
}

/// Recursively emit blocks from the node tree in structure (reading) order.
fn walk(node: &Node, map: &HashMap<usize, McidText>, blocks: &mut Vec<Block>, detect_math: bool) {
    match node {
        Node::Mcid { role, actual_text, .. } => {
            // A bare marked-content item with no parent semantic wrapper.
            let (kind, level) = classify_role(role);
            let (text, bx) = if let Some(at) = actual_text {
                (at.clone(), None)
            } else {
                gather(node, map, detect_math)
            };
            if text.trim().is_empty() {
                return;
            }
            blocks.push(Block {
                kind: kind.to_string(),
                level,
                text: text.trim().to_string(),
                bx,
            });
        }
        Node::Elem { role, children, actual_text } => {
            let (kind, level) = classify_role(role);
            match role.as_slice() {
                b"P" | b"Text" | b"Span" | b"Lbl" | b"Note" | b"Quote" | b"BlockQuote" | b"Sidebar" => {
                    let (text, bx) = if let Some(at) = actual_text {
                        (at.clone(), None)
                    } else {
                        gather(node, map, detect_math)
                    };
                    if !text.trim().is_empty() {
                        blocks.push(Block {
                            kind: kind.to_string(),
                            level,
                            text: text.trim().to_string(),
                            bx,
                        });
                    }
                }
                b"H" | b"H1" | b"H2" | b"H3" | b"H4" | b"H5" | b"H6" => {
                    let (text, bx) = if let Some(at) = actual_text {
                        (at.clone(), None)
                    } else {
                        gather(node, map, detect_math)
                    };
                    if !text.trim().is_empty() {
                        blocks.push(Block {
                            kind: kind.to_string(),
                            level,
                            text: text.trim().to_string(),
                            bx,
                        });
                    }
                }
                b"LI" => {
                    let (text, bx) = gather(node, map, detect_math);
                    let clean = strip_list_marker(&text);
                    if !clean.trim().is_empty() {
                        blocks.push(Block {
                            kind: "list".to_string(),
                            level,
                            text: clean.trim().to_string(),
                            bx,
                        });
                    }
                }
                b"Table" | b"THead" | b"TBody" | b"TFoot" => {
                    if let Some(md) = table_from_node(node, map, detect_math) {
                        blocks.push(Block {
                            kind: "table".to_string(),
                            level: None,
                            text: md,
                            bx: gather_bbox(node, map, detect_math),
                        });
                    } else {
                        // Failed table structure: emit descendant text as body.
                        for c in children {
                            walk(c, map, blocks, detect_math);
                        }
                    }
                }
                b"Figure" | b"Fig" => {
                    let (t, bx) = gather(node, map, detect_math);
                    let label = actual_text.clone().unwrap_or_else(|| {
                        if t.trim().is_empty() { "[figure]".to_string() } else { t.trim().to_string() }
                    });
                    blocks.push(Block {
                        kind: "figure".to_string(),
                        level: None,
                        text: label,
                        bx,
                    });
                }
                _ => {
                    // Container (Sect/Document/Part/Div/Artifact/…): recurse.
                    for c in children {
                        walk(c, map, blocks, detect_math);
                    }
                }
            }
        }
    }
}

fn strip_list_marker(t: &str) -> String {
    let tt = t.trim_start();
    for p in ["•", "-", "*", "\u{2022}"] {
        if let Some(rest) = tt.strip_prefix(p) {
            return rest.trim().to_string();
        }
    }
    t.to_string()
}

fn gather_bbox(node: &Node, map: &HashMap<usize, McidText>, detect_math: bool) -> Option<(f64, f64, f64, f64)> {
    let (_t, bx) = gather(node, map, detect_math);
    bx
}

fn block_to_docblock(b: &Block, page: usize) -> Option<DocBlock> {
    let (x0, y0, x1, y1) = b.bx.unwrap_or((0.0, 0.0, 0.0, 0.0));
    Some(DocBlock {
        page,
        kind: b.kind.clone(),
        x0,
        y0,
        x1,
        y1,
        text: b.text.clone(),
        is_bold: false,
        is_italic: false,
        is_underline: false,
    })
}

fn render_blocks(blocks: &mut [Block]) -> String {
    let mut md = String::new();
    let mut prev_was_list = false;
    for b in blocks.iter() {
        let line = block_markdown(b);
        if line.trim().is_empty() {
            continue;
        }
        let is_list = b.kind == "list";
        if !md.is_empty() {
            if prev_was_list && is_list {
                md.push('\n');
            } else {
                md.push_str("\n\n");
            }
        }
        md.push_str(line.trim_end());
        prev_was_list = is_list;
    }
    md.trim().to_string()
}

fn block_markdown(b: &Block) -> String {
    match b.kind.as_str() {
        "heading" => {
            let l = b.level.unwrap_or(1).clamp(1, 6) as usize;
            format!("{} {}", "#".repeat(l), b.text)
        }
        "list" => format!("- {}", b.text),
        "table" => b.text.clone(),
        "figure" => b.text.clone(),
        _ => b.text.clone(),
    }
}

// ---------------------------------------------------------------------------
// Public (crate) entrypoint
// ---------------------------------------------------------------------------

/// Try to extract a page from its tagged structure tree. Returns `None` when the
/// document is untagged, the page has no structure, or the structure does not
/// resolve to non-empty marked-content text (so the caller keeps the geometry path).
pub fn extract_tagged_page(doc: &Document, page_id: ObjectId, detect_math: bool) -> Option<TaggedPage> {
    let root_id = struct_root_id(doc)?;
    let root = doc.get_object(root_id).ok()?;
    let root_dict = as_dict(doc, &root)?;
    if root_dict.get(b"K").is_err() {
        return None;
    }
    let map = rolemap(doc, root_dict);
    let elems = page_elements(doc, root_dict, page_id);
    if elems.is_empty() {
        return None;
    }

    let mut roots: Vec<Node> = Vec::new();
    for e in &elems {
        parse_nodes(doc, e, &Vec::new(), &map, &mut roots);
    }
    if roots.is_empty() {
        return None;
    }

    let mcid_map = extract_marked_content(doc, page_id);
    if mcid_map.is_empty() {
        return None;
    }

    // The structure must resolve to non-empty text, otherwise it is decorative.
    let mutable_have_text = |nodes: &[Node]| -> bool {
        let mut n = 0;
        let mut total = 0;
        count_mcids(nodes, &mcid_map, &mut n, &mut total);
        n > 0 && (total == 0 || n >= total / 2)
    };
    if !mutable_have_text(&roots) {
        return None;
    }

    let mut blocks: Vec<Block> = Vec::new();
    for r in &roots {
        walk(r, &mcid_map, &mut blocks, detect_math);
    }
    if blocks.is_empty() {
        return None;
    }
    let text = render_blocks(&mut blocks);
    if text.trim().is_empty() {
        return None;
    }

    let tables = blocks.iter().filter(|b| b.kind == "table").count();
    let words: usize = blocks.iter().map(|b| b.text.split_whitespace().count()).sum();
    let doc_blocks: Vec<DocBlock> = blocks.iter().filter_map(|b| block_to_docblock(b, 0)).collect();

    Some(TaggedPage {
        text,
        blocks: doc_blocks,
        tables,
        words,
    })
}

fn count_mcids(nodes: &[Node], map: &HashMap<usize, McidText>, resolved: &mut usize, total: &mut usize) {
    for n in nodes {
        match n {
            Node::Mcid { mcid, .. } => {
                *total += 1;
                if map.get(mcid).map_or(false, |t| !t.text.trim().is_empty()) {
                    *resolved += 1;
                }
            }
            Node::Elem { children, .. } => count_mcids(children, map, resolved, total),
        }
    }
}




#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::{dictionary, Object, Stream};

    fn refs(ids: &[ObjectId]) -> Object {
        Object::Array(ids.iter().map(|i| Object::Reference(*i)).collect())
    }
    fn ints(values: &[i64]) -> Object {
        Object::Array(values.iter().map(|v| Object::Integer(*v)).collect())
    }

    /// Build a minimal, well-tagged single-page PDF in memory:
    ///   H1 "Document Title", then a P made of two marked-content lines.
    /// The structure tree (H1 + P) is fully consistent with the content stream's
    /// `/MCID` ranges, so coverage should be ~100% and the reader must activate.
    fn build_tagged_doc() -> (Document, ObjectId) {
        let mut doc = Document::with_version("1.7");

        let font_id = doc.new_object_id();
        let content_id = doc.new_object_id();
        let page_id = doc.new_object_id();
        let pages_id = doc.new_object_id();
        let catalog_id = doc.new_object_id();
        let root_id = doc.new_object_id();
        let parenttree_id = doc.new_object_id();
        let sect_id = doc.new_object_id();
        let h1_id = doc.new_object_id();
        let p1_id = doc.new_object_id();

        doc.objects.insert(
            font_id,
            Object::Dictionary(dictionary! {
                "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica",
                "Encoding" => "WinAnsiEncoding",
            }),
        );

        let content_bytes = b"BT\n/F1 18 Tf\n72 750 Td\nBDC /Span << /MCID 0 >>\n(Document Title) Tj\nEMC\nET\n\
BT\n/F1 12 Tf\n72 720 Td\nBDC /Span << /MCID 1 >>\n(First paragraph sentence one and it flows on.) Tj\nEMC\n\
0 -16 Td\nBDC /Span << /MCID 2 >>\n(Second line keeps right on going.) Tj\nEMC\nET\n"
            .to_vec();
        doc.objects.insert(
            content_id,
            Object::Stream(Stream::new(dictionary! {}, content_bytes)),
        );

        let page_dict = dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "MediaBox" => Object::Array(vec![Object::Integer(0), Object::Integer(0), Object::Integer(612), Object::Integer(792)]),
            "StructParents" => Object::Integer(0),
            "Resources" => dictionary! {
                "Font" => dictionary! { "F1" => font_id },
            },
            "Contents" => content_id,
        };
        doc.objects.insert(page_id, Object::Dictionary(page_dict));
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages", "Kids" => refs(&[page_id]), "Count" => 1,
            }),
        );
        doc.objects.insert(
            catalog_id,
            Object::Dictionary(dictionary! { "Type" => "Catalog", "Pages" => pages_id }),
        );

        // Structure tree.
        let sect = dictionary! {
            "Type" => "StructElem", "S" => "Sect",
            "K" => refs(&[h1_id, p1_id]),
        };
        doc.objects.insert(sect_id, Object::Dictionary(sect));
        let h1 = dictionary! {
            "Type" => "StructElem", "S" => "H1", "K" => Object::Integer(0), "Pg" => page_id,
        };
        doc.objects.insert(h1_id, Object::Dictionary(h1));
        let p1 = dictionary! {
            "Type" => "StructElem", "S" => "P",
            "K" => ints(&[1, 2]),
            "Pg" => page_id,
        };
        doc.objects.insert(p1_id, Object::Dictionary(p1));

        let parenttree = dictionary! {
            "Nums" => Object::Array(vec![Object::Integer(0), refs(&[sect_id])]),
        };
        doc.objects.insert(parenttree_id, Object::Dictionary(parenttree));
        doc.objects.insert(
            root_id,
            Object::Dictionary(dictionary! {
                "Type" => "StructTreeRoot",
                "RoleMap" => dictionary! {},
                "ParentTree" => parenttree_id,
                "K" => refs(&[sect_id]),
            }),
        );
        doc.trailer.set(b"Root", Object::Reference(catalog_id));

        (doc, page_id)
    }

    #[test]
    fn tagged_page_reads_heading_and_paragraph_from_structure() {
        let (doc, page_id) = build_tagged_doc();
        assert!(has_struct_tree(&doc), "fixture must be tagged");

        let tagged = extract_tagged_page(&doc, page_id, false).expect("tagged extraction must succeed");
        // Coverage should be high: the structure marks all text on the page.
        assert!(tagged.words >= 10, "expected a full paragraph, got {} words", tagged.words);
        assert!(tagged.blocks.len() >= 2, "expected heading + paragraph blocks, got {}", tagged.blocks.len());

        // Reading order and roles come from the structure tree.
        let kinds: Vec<&str> = tagged.blocks.iter().map(|b| b.kind.as_str()).collect();
        assert_eq!(kinds[0], "heading", "first block must be the H1 heading");
        assert_eq!(kinds[1], "body", "second block must be the paragraph");

        assert!(
            tagged.text.contains("# Document Title"),
            "markdown must emit an H1 for the heading: {}",
            tagged.text
        );
        assert!(
            tagged.text.contains("First paragraph sentence one and it flows on."),
            "markdown must contain the first paragraph line: {}",
            tagged.text
        );
        assert!(
            tagged.text.contains("Second line keeps right on going."),
            "markdown must contain the second paragraph line: {}",
            tagged.text
        );
    }

    #[test]
    fn tagged_page_returns_none_for_untagged_document() {
        let mut doc = Document::with_version("1.7");
        let page = doc.new_object_id();
        let pages = doc.new_object_id();
        doc.objects.insert(
            pages,
            Object::Dictionary(dictionary! {
                "Type" => "Pages", "Kids" => refs(&[page]), "Count" => 1,
            }),
        );
        doc.objects.insert(
            page,
            Object::Dictionary(dictionary! {
                "Type" => "Page", "Parent" => pages,
                "MediaBox" => Object::Array(vec![Object::Integer(0), Object::Integer(0), Object::Integer(612), Object::Integer(792)]),
            }),
        );
        let cat = doc.new_object_id();
        doc.objects.insert(cat, Object::Dictionary(dictionary! { "Type" => "Catalog", "Pages" => pages }));
        doc.trailer.set(b"Root", Object::Reference(cat));
        assert!(!has_struct_tree(&doc));
        assert!(extract_tagged_page(&doc, page, false).is_none(), "untagged page must not route to structure");
    }

    #[test]
    fn tagged_page_with_math_keeps_plain_text_unchanged() {
        // Enabling math synthesis on a tagged page with only plain prose must
        // not alter the output (no false-positive scripts/fractions).
        let (doc, page_id) = build_tagged_doc();
        let tagged = extract_tagged_page(&doc, page_id, true).expect("tagged extraction must succeed");
        assert!(tagged.text.contains("# Document Title"), "{}", tagged.text);
        assert!(
            tagged.text.contains("First paragraph sentence one and it flows on."),
            "{}",
            tagged.text
        );
        assert!(!tagged.text.contains('$'), "plain tagged text must not gain math: {}", tagged.text);
    }

    #[test]
    fn tagged_doc_full_convert_emits_structure_from_tree() {
        let (mut doc, _page) = build_tagged_doc();
        let mut bytes = Vec::new();
        doc.save_to(&mut bytes).unwrap();
        let res = crate::convert_pdf_bytes_to_markdown(&bytes, &crate::ConversionOptions::default())
            .expect("conversion must succeed");
        // The structure tree is authoritative: heading becomes an H1 and the P is
        // a single paragraph, in reading order (the coverage gate passes here
        // because the tree marks all the page's text).
        assert!(
            res.markdown.contains("# Document Title"),
            "tagged H1 must become '# Document Title': {}",
            res.markdown
        );
        assert!(
            res.markdown.contains("First paragraph sentence one and it flows on."),
            "paragraph line 1 must be present: {}",
            res.markdown
        );
        assert!(
            res.markdown.contains("Second line keeps right on going."),
            "paragraph line 2 must be present: {}",
            res.markdown
        );
    }
}
