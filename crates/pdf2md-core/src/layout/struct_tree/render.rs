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

pub(super) fn heading_level(role: &[u8]) -> Option<u8> {
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
pub(super) fn gather(
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
pub(super) fn gather_into(
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

pub(super) fn classify_role(role: &[u8]) -> (&'static str, Option<u8>) {
    match role {
        b"H" | b"H1" | b"H2" | b"H3" | b"H4" | b"H5" | b"H6" => ("heading", heading_level(role)),
        b"L" | b"LI" | b"UL" | b"OL" | b"LBody" => ("list", None),
        // Only the `Table` role is a table marker. `THead`/`TBody`/`TFoot`/
        // `TR`/`TD`/`TH` are children *within* a table subtree; encountered as
        // the role of a leaf outside a `Table` ancestor they are ordinary
        // content, not independent tables (F7/R4). The `walk` container arm
        // applies the single-column / flowing-prose layout heuristic before
        // rendering a `Table` node.
        b"Table" => ("table", None),
        b"THead" | b"TBody" | b"TFoot" | b"TR" | b"TD" | b"TH" => ("body", None),
        b"Figure" | b"Fig" => ("figure", None),
        b"Header" | b"Head" => ("header", None),
        b"Footer" | b"Foot" => ("footer", None),
        b"TOC" => ("toc", None),
        b"P" | b"Text" | b"Span" | b"Lbl" | b"Note" | b"Quote" | b"BlockQuote" | b"Sidebar" => ("body", None),
        _ => ("body", None),
    }
}

/// Reconstruct a GFM pipe table from a `Table` node's `TR`/`TD`/`TH` structure.
pub(super) fn table_from_node(node: &Node, map: &HashMap<usize, McidText>, detect_math: bool) -> Option<String> {
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

pub(super) fn collect_table_rows(node: &Node, map: &HashMap<usize, McidText>, rows: &mut Vec<Vec<String>>, heads: &mut Option<Vec<String>>, is_head: &mut bool, detect_math: bool) {
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

/// True when a single cell holds flowing prose rather than a short tabular
/// token: a paragraph-length cell (>= 20 words) or multi-sentence text.
pub(super) fn is_flowing_prose(cell: &str) -> bool {
    let t = cell.trim();
    if t.is_empty() {
        return false;
    }
    let words = t.split_whitespace().count();
    if words >= 20 {
        return true;
    }
    let terminators = t
        .chars()
        .filter(|c| matches!(c, '.' | '!' | '?'))
        .count();
    terminators >= 2 && words >= 8
}

/// A `Table` structure element is frequently used purely for page layout by
/// invoice/letter generators: its cells hold ordinary flowing prose, or it has
/// a single column so its "rows" are just stacked blocks. Rendering such a
/// node as a GFM pipe table both inflates the customer-visible table count and
/// hides the real paragraphs. Returns `true` when the node should instead be
/// walked as a plain container (F7/R4).
pub(super) fn is_layout_table(node: &Node, map: &HashMap<usize, McidText>, detect_math: bool) -> bool {
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut heads: Option<Vec<String>> = None;
    let mut is_head = false;
    collect_table_rows(node, map, &mut rows, &mut heads, &mut is_head, detect_math);
    if let Some(h) = heads.take() {
        rows.insert(0, h);
    }
    let rows: Vec<Vec<String>> = rows
        .into_iter()
        .filter(|r| r.iter().any(|c| !c.trim().is_empty()))
        .collect();
    let cols = rows.iter().map(|r| r.len()).max().unwrap_or(0);
    // No tabular geometry, or a single-column wrapper: children stack
    // vertically, so recurse into them as ordinary blocks.
    if cols < 2 {
        return true;
    }
    rows.iter().flatten().any(|c| is_flowing_prose(c))
}

/// Recursively emit blocks from the node tree in structure (reading) order.
pub(super) fn walk(node: &Node, map: &HashMap<usize, McidText>, blocks: &mut Vec<Block>, detect_math: bool) {
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
                b"Table" => {
                    if is_layout_table(node, map, detect_math) {
                        // Layout table (single column and/or prose cells): walk
                        // it as a plain container so its paragraphs survive as
                        // ordinary blocks instead of a fake table zone.
                        for c in children {
                            walk(c, map, blocks, detect_math);
                        }
                    } else if let Some(md) = table_from_node(node, map, detect_math) {
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
                // Table-section and cell roles reached outside a `Table` node
                // are not independent tables: recurse into them as containers.
                b"THead" | b"TBody" | b"TFoot" | b"TR" | b"TD" | b"TH" => {
                    for c in children {
                        walk(c, map, blocks, detect_math);
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

pub(super) fn strip_list_marker(t: &str) -> String {
    let tt = t.trim_start();
    for p in ["•", "-", "*", "\u{2022}"] {
        if let Some(rest) = tt.strip_prefix(p) {
            return rest.trim().to_string();
        }
    }
    t.to_string()
}

pub(super) fn gather_bbox(node: &Node, map: &HashMap<usize, McidText>, detect_math: bool) -> Option<(f64, f64, f64, f64)> {
    let (_t, bx) = gather(node, map, detect_math);
    bx
}

pub(super) fn block_to_docblock(b: &Block, page: usize) -> Option<DocBlock> {
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

pub(super) fn render_blocks(blocks: &mut [Block]) -> String {
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

pub(super) fn block_markdown(b: &Block) -> String {
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
