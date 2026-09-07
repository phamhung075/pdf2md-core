// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Statistical semantic classification (Headings, List Items, Monospace Code Blocks, Body Paragraphs).

use crate::cpdf_textpage::{TextBlock, TextLine};
use crate::layout::ast::AstNode;
use crate::layout::xy_cut::DocumentStatistics;

/// Classifies structured text blocks into semantic AST nodes (Headings, Lists, Code, Paragraphs)
/// using relative statistical deviation rather than fixed point sizes.
pub fn classify_semantic_blocks(
    blocks: &[TextBlock],
    stats: &DocumentStatistics,
) -> Vec<(f64, AstNode)> {
    let mut result = Vec::new();

    for block in blocks {
        if block.lines.is_empty() {
            continue;
        }

        // 1. Check if the block is a heading (single-line or short 2-line title)
        if block.lines.len() <= 2 {
            let all_heading = block.lines.iter().all(|l| detect_heading(l, stats).is_some());
            if all_heading {
                let level = detect_heading(&block.lines[0], stats).map(|(lvl, _)| lvl).unwrap_or(1);
                let h_text = block.lines.iter().map(|l| l.text.trim()).collect::<Vec<_>>().join(" ");
                result.push((block.block_bbox.max_y, AstNode::Heading { level, text: h_text }));
                continue;
            }
        }

        // 2. Check if block lines contain list items
        let mut is_list_block = false;
        let mut list_nodes: Vec<(f64, AstNode)> = Vec::new();
        for l in &block.lines {
            if let Some((depth, item_text, ordered)) =
                detect_list_item(l, block.block_bbox.min_x, stats.median_font_size)
            {
                is_list_block = true;
                list_nodes.push((l.line_bbox.max_y, AstNode::ListItem { depth, text: item_text, ordered }));
            } else if is_list_block {
                // Continuation line of previous list item
                if let Some((_, AstNode::ListItem { text, .. })) = list_nodes.last_mut() {
                    text.push(' ');
                    text.push_str(l.text.trim());
                }
            } else {
                break;
            }
        }
        if is_list_block && !list_nodes.is_empty() {
            result.extend(list_nodes);
            continue;
        }

        // 3. Check for Code Block (fenced or monospace)
        if block.text.trim().starts_with("```") {
            let clean_code = block.text.trim().trim_matches('`').to_string();
            result.push((
                block.block_bbox.max_y,
                AstNode::CodeBlock {
                    code: clean_code,
                    language: None,
                },
            ));
            continue;
        }

        // 4. Default: Paragraph
        result.push((
            block.block_bbox.max_y,
            AstNode::Paragraph {
                text: block.text.clone(),
            },
        ));
    }

    result
}

pub fn detect_heading(line: &TextLine, stats: &DocumentStatistics) -> Option<(u8, String)> {
    let trimmed = line.text.trim();
    if trimmed.is_empty() || trimmed.len() > 180 {
        return None;
    }

    // Trailing full stop usually indicates paragraph prose
    if trimmed.ends_with('.') && !trimmed.contains(|c: char| c.is_ascii_alphabetic()) {
        return None;
    }
    if trimmed.matches(". ").count() >= 2 {
        return None;
    }

    let avg_fs = if line.chars.is_empty() {
        stats.median_font_size
    } else {
        line.chars.iter().map(|c| c.font_size).sum::<f64>() / line.chars.len() as f64
    };

    let is_bold = line.chars.iter().filter(|c| c.is_bold).count() > line.chars.len() / 2;
    let median = stats.median_font_size;

    // H1: font_size >= 1.6 * median OR (font_size >= 1.4 * median && is_bold)
    if avg_fs >= 1.6 * median || (avg_fs >= 1.4 * median && is_bold) {
        return Some((1, trimmed.to_string()));
    }
    // H2: font_size >= 1.3 * median
    if avg_fs >= 1.3 * median {
        return Some((2, trimmed.to_string()));
    }
    // H3: font_size >= 1.15 * median && is_bold
    if avg_fs >= 1.15 * median && is_bold {
        return Some((3, trimmed.to_string()));
    }

    None
}

pub fn detect_list_item(line: &TextLine, block_min_x: f64, median_fs: f64) -> Option<(u8, String, bool)> {
    // 1. Check against first TextWord in TextLine if present
    if !line.words.is_empty() {
        let w0 = &line.words[0];
        let w0_text = w0.text.trim();

        // Check for checkbox / task markers: "[ ]", "[x]", "[X]"
        if w0_text == "[ ]" || w0_text == "[x]" || w0_text == "[X]" {
            let rest: String = line.words[1..].iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ");
            let depth = ((line.line_bbox.min_x - block_min_x) / (1.5 * median_fs)).clamp(0.0, 4.0) as u8;
            return Some((depth, format!("{} {}", w0_text, rest.trim()), false));
        }

        // Check for bullet markers: "-", "*", "•", "+", "◦", "▪"
        if w0_text == "-" || w0_text == "*" || w0_text == "•" || w0_text == "+" || w0_text == "◦" || w0_text == "▪" {
            let rest: String = line.words[1..].iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ");
            let depth = ((line.line_bbox.min_x - block_min_x) / (1.5 * median_fs)).clamp(0.0, 4.0) as u8;
            return Some((depth, rest.trim().to_string(), false));
        }

        // Check for ordered list markers: "1.", "2.", "1)", "(1)"
        if (w0_text.ends_with('.') || w0_text.ends_with(')')) && w0_text.len() <= 5 {
            let digits = &w0_text[..w0_text.len() - 1];
            if !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()) {
                let rest: String = line.words[1..].iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ");
                let depth = ((line.line_bbox.min_x - block_min_x) / (1.5 * median_fs)).clamp(0.0, 4.0) as u8;
                return Some((depth, rest.trim().to_string(), true));
            }
        }
        if (w0_text.starts_with('(') && w0_text.ends_with(')')) || (w0_text.starts_with('[') && w0_text.ends_with(']')) {
            let inner = &w0_text[1..w0_text.len() - 1];
            if !inner.is_empty() && inner.chars().all(|c| c.is_ascii_digit()) {
                let rest: String = line.words[1..].iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ");
                let depth = ((line.line_bbox.min_x - block_min_x) / (1.5 * median_fs)).clamp(0.0, 4.0) as u8;
                return Some((depth, rest.trim().to_string(), true));
            }
        }
    }

    // 2. Fallback prefix check on line.text
    detect_list_item_from_str(&line.text, line.line_bbox.min_x, block_min_x, median_fs)
}

fn detect_list_item_from_str(text: &str, line_min_x: f64, block_min_x: f64, median_fs: f64) -> Option<(u8, String, bool)> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }

    // Check for checkbox / task markers: "[ ]", "[x]", "[X]"
    for box_prefix in &["[ ] ", "[x] ", "[X] "] {
        if trimmed.starts_with(box_prefix) {
            let depth = ((line_min_x - block_min_x) / (1.5 * median_fs)).clamp(0.0, 4.0) as u8;
            return Some((depth, trimmed.to_string(), false));
        }
    }

    // Unordered bullet prefixes
    let unordered_prefixes = ["- ", "* ", "• ", "+ ", "◦ ", "▪ "];
    for prefix in &unordered_prefixes {
        if let Some(rest) = trimmed.strip_prefix(prefix) {
            let depth = ((line_min_x - block_min_x) / (1.5 * median_fs)).clamp(0.0, 4.0) as u8;
            return Some((depth, rest.trim().to_string(), false));
        }
    }

    // Ordered list markers: "1. ", "12. ", "1) ", "(1) "
    if let Some(first_space) = trimmed.find(' ') {
        let prefix = &trimmed[..first_space];
        let rest = &trimmed[first_space + 1..];

        if (prefix.ends_with('.') || prefix.ends_with(')')) && prefix.len() <= 5 {
            let digits = &prefix[..prefix.len() - 1];
            if !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()) {
                let depth = ((line_min_x - block_min_x) / (1.5 * median_fs)).clamp(0.0, 4.0) as u8;
                return Some((depth, rest.trim().to_string(), true));
            }
        }
        if (prefix.starts_with('(') && prefix.ends_with(')')) || (prefix.starts_with('[') && prefix.ends_with(']')) {
            let inner = &prefix[1..prefix.len() - 1];
            if !inner.is_empty() && inner.chars().all(|c| c.is_ascii_digit()) {
                let depth = ((line_min_x - block_min_x) / (1.5 * median_fs)).clamp(0.0, 4.0) as u8;
                return Some((depth, rest.trim().to_string(), true));
            }
        }
    }

    None
}
