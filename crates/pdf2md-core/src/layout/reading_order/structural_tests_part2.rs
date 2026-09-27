// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Reading order recovery, multi-column stream separation, and structured DocBlock generation.

use super::*;
use super::structural_tests_common::*;

    #[test]
    fn ordered_list_item_gets_numbered_prefix() {
        let out = format_structured_line(
            &LineRole::List { depth: 0, ordered: true, ordinal: 3 },
            "Third item",
        );
        assert_eq!(out, "3. Third item");
    }

    #[test]
    fn nested_list_item_is_indented() {
        let out = format_structured_line(
            &LineRole::List { depth: 2, ordered: false, ordinal: 1 },
            "Deep item",
        );
        assert_eq!(out, "    - Deep item");
    }

    #[test]
    fn body_role_passes_text_through_unchanged() {
        let out = format_structured_line(&LineRole::Body, "Just a paragraph.");
        assert_eq!(out, "Just a paragraph.");
    }

    // -- end-to-end: render_cluster now emits structural Markdown --------------

    #[test]
    fn render_cluster_emits_heading_and_list_markdown() {
        let lines = vec![
            one_span_line("Document Title", 20.0, false), // body ~10 -> 2.0x -> H1
            one_span_line("First paragraph of body text.", 10.0, false),
            bulleted_line("-"),
            bulleted_line("-"),
        ];
        let md = render_cluster(&lines, None);
        assert!(md.contains("# Document Title"), "got:\n{md}");
        assert!(md.contains("- Item text"), "got:\n{md}");
        assert!(
            !md.contains("**Document Title**"),
            "heading text must not be redundantly bold-wrapped, got:\n{md}"
        );
    }
