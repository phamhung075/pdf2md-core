// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Modern Semantic AST (CommonMark / GFM Compliant) representation and Markdown serializer.

use serde::{Deserialize, Serialize};
use crate::models::CanvasTable;

/// Semantic AST node representing a structured document element.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum AstNode {
    Heading { level: u8, text: String },
    Paragraph { text: String },
    Table(CanvasTable),
    ListItem { depth: u8, text: String, ordered: bool },
    CodeBlock { code: String, language: Option<String> },
}

/// Hierarchical Semantic AST produced by modern layout analysis.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LayoutAST {
    pub nodes: Vec<AstNode>,
}

impl LayoutAST {
    pub fn new(nodes: Vec<AstNode>) -> Self {
        Self { nodes }
    }

    /// Serializes the Semantic AST into clean CommonMark / GFM markdown.
    pub fn to_markdown(&self) -> String {
        let mut md = String::new();
        let mut prev_was_list = false;

        for node in &self.nodes {
            match node {
                AstNode::Heading { level, text } => {
                    if !md.is_empty() {
                        md.push_str("\n\n");
                    }
                    let clamped = (*level).clamp(1, 6) as usize;
                    let hashes = "#".repeat(clamped);
                    md.push_str(&format!("{} {}", hashes, text.trim()));
                    prev_was_list = false;
                }
                AstNode::Paragraph { text } => {
                    let trimmed = text.trim();
                    if !trimmed.is_empty() {
                        if !md.is_empty() {
                            md.push_str("\n\n");
                        }
                        md.push_str(trimmed);
                    }
                    prev_was_list = false;
                }
                AstNode::Table(table) => {
                    let table_md = table.to_markdown();
                    let trimmed = table_md.trim();
                    if !trimmed.is_empty() {
                        if !md.is_empty() {
                            md.push_str("\n\n");
                        }
                        md.push_str(trimmed);
                    }
                    prev_was_list = false;
                }
                AstNode::ListItem { depth, text, ordered } => {
                    if !md.is_empty() {
                        if prev_was_list {
                            md.push('\n');
                        } else {
                            md.push_str("\n\n");
                        }
                    }
                    let indent = "  ".repeat(*depth as usize);
                    let prefix = if *ordered { "1." } else { "-" };
                    md.push_str(&format!("{}{} {}", indent, prefix, text.trim()));
                    prev_was_list = true;
                }
                AstNode::CodeBlock { code, language } => {
                    if !md.is_empty() {
                        md.push_str("\n\n");
                    }
                    let lang = language.as_deref().unwrap_or("");
                    md.push_str(&format!("```{}\n{}\n```", lang, code.trim_end()));
                    prev_was_list = false;
                }
            }
        }
        md
    }
}
