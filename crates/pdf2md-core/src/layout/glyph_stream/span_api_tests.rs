// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Unit tests for the public [`page_glyph_spans`] API and the shared glyph
//! prefix it uses together with [`extract_page_glyphs`].

use super::tests_common::{pdf_page_with, show_at};
use super::*;

fn courier_ops() -> Vec<Operation> {
    let mut ops = vec![Operation::new(
        "Tf",
        vec![Object::Name(b"F1".to_vec()), Object::Real(10.0)],
    )];
    ops.extend(show_at(50.0, 700.0, "ABC"));
    ops
}

#[test]
fn page_glyph_spans_returns_raw_spans_before_layout() {
    let (doc, page_id) = pdf_page_with(courier_ops(), None, None, [0.0, 0.0, 595.0, 842.0]);
    let page = page_glyph_spans(&doc, page_id).expect("spans");
    assert!(!page.spans.is_empty(), "expected raw spans");
    assert!((page.width - 595.0).abs() < 0.01, "width={}", page.width);
    assert!((page.height - 842.0).abs() < 0.01, "height={}", page.height);
    assert!(page.text_ops_seen);
    let joined: String = page.spans.iter().map(|s| s.text.as_str()).collect();
    assert!(joined.contains("ABC"), "text={joined:?}");
    let first = &page.spans[0];
    // Bottom-left PDF user space: the Tm places the run at (50, 700).
    assert!((first.x - 50.0).abs() < 0.01, "x={}", first.x);
    assert!((first.y - 700.0).abs() < 0.01, "y={}", first.y);
}

#[test]
fn extract_page_glyphs_uses_the_same_prefix() {
    let (doc, page_id) = pdf_page_with(courier_ops(), None, None, [0.0, 0.0, 595.0, 842.0]);
    let text = extract_page_glyphs(&doc, page_id, false, false, false).expect("text");
    assert!(text.text.contains("ABC"), "{}", text.text);
    assert!(text.text_ops_seen);
    assert!(text.has_fonts);
}
