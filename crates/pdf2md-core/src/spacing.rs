// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Post-reflow whitespace normalization for the emitted Markdown.
//!
//! Producers frequently draw an inter-word gap as a run of two space glyphs
//! (and, for aligned form rows and empty table cells, three). `render_spans`
//! copies a span's own text verbatim, so those runs survive into the Markdown
//! as `Total  1` or `|   |`. Markdown renders any such run as one word gap, so
//! the extra spaces are invisible but leave the output visibly noisier than a
//! converter that normalizes them.
//!
//! Normalization runs *after* [`crate::reflow::reflow_markdown`] because the
//! reflow pass uses a run of three or more spaces as the signal for an aligned,
//! non-paragraph form row (see `reflow::is_plain_body`). Collapsing earlier
//! would destroy that signal and weld form rows into prose.
//!
//! Rules:
//! * fenced code blocks are copied verbatim — indentation and embedded runs are
//!   significant there;
//! * leading indentation is preserved so list nesting is unaffected;
//! * every later maximal run of ASCII spaces becomes one space.

/// Collapse interior double-space runs in `markdown`, preserving leading
/// indentation and fenced code. See the module docs for why this runs after
/// reflow.
pub(crate) fn collapse_interior_spaces(markdown: &str) -> String {
    let mut out = String::with_capacity(markdown.len());
    // The fence marker currently open (` ``` ` or `~~~`), else `None`.
    let mut fence: Option<&str> = None;
    for (i, line) in markdown.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let trimmed = line.trim_start();
        if let Some(marker) = fence {
            out.push_str(line);
            if trimmed.starts_with(marker) {
                fence = None;
            }
            continue;
        }
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            fence = Some(if trimmed.starts_with("```") { "```" } else { "~~~" });
            out.push_str(line);
            continue;
        }
        push_collapsed_line(&mut out, line);
    }
    out
}

/// Append one non-code line, keeping its leading indentation and reducing each
/// later space run to a single space.
fn push_collapsed_line(out: &mut String, line: &str) {
    let indent = line.len() - line.trim_start_matches(' ').len();
    out.push_str(&line[..indent]);
    let mut run = 0usize;
    for c in line[indent..].chars() {
        if c == ' ' {
            run += 1;
            continue;
        }
        if run > 0 {
            // One space is what Markdown renders for any run; a double (or
            // wider) producer gap therefore collapses here.
            out.push(' ');
        }
        run = 0;
        out.push(c);
    }
    if run > 0 {
        out.push(' ');
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collapses_interior_double_spaces() {
        assert_eq!(collapse_interior_spaces("Total  1"), "Total 1");
        assert_eq!(
            collapse_interior_spaces("Revenue  and cost."),
            "Revenue and cost."
        );
        assert_eq!(collapse_interior_spaces("a   b    c"), "a b c");
    }

    #[test]
    fn preserves_leading_indentation() {
        assert_eq!(
            collapse_interior_spaces("    - niveau  deux"),
            "    - niveau deux"
        );
        // A nested list continuation keeps its indent, interior run collapses.
        assert_eq!(collapse_interior_spaces("    texte  suite"), "    texte suite");
    }

    #[test]
    fn collapses_empty_table_cells_keeping_table_valid() {
        assert_eq!(collapse_interior_spaces("| a |   |   |"), "| a | | |");
        assert_eq!(
            collapse_interior_spaces("| --- | --- | --- |"),
            "| --- | --- | --- |"
        );
    }

    #[test]
    fn fenced_code_is_verbatim() {
        let input = "body   text\n```rust\nlet a = 1;  // two  spaces\n```\nbody   again";
        assert_eq!(
            collapse_interior_spaces(input),
            "body text\n```rust\nlet a = 1;  // two  spaces\n```\nbody again"
        );
    }

    #[test]
    fn tilde_fence_is_verbatim() {
        let input = "~~~\nkeep   this   verbatim\n~~~\ncollapse  this";
        assert_eq!(
            collapse_interior_spaces(input),
            "~~~\nkeep   this   verbatim\n~~~\ncollapse this"
        );
    }

    #[test]
    fn single_spaces_and_blank_lines_survive() {
        assert_eq!(collapse_interior_spaces("a b c"), "a b c");
        assert_eq!(collapse_interior_spaces("a\n\nb"), "a\n\nb");
        assert_eq!(collapse_interior_spaces(""), "");
    }

    #[test]
    fn word_tokens_are_never_changed() {
        let src = "a  b   c\nd    e";
        let words = |s: &str| {
            let mut v: Vec<String> = s.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(String::from).collect();
            v.sort();
            v
        };
        assert_eq!(words(src), words(&collapse_interior_spaces(src)));
    }
}
