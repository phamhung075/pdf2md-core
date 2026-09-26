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

mod marked_content;
use marked_content::*;
mod tree;
pub use tree::*;
mod render;
use render::*;

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

/// A semantic block with its role (kind), optional heading level, text, and bbox.
struct Block {
    kind: String,
    level: Option<u8>,
    text: String,
    bx: Option<(f64, f64, f64, f64)>,
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
mod tests;
