// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Unit tests for the public [`page_glyph_spans`] API and the shared glyph
//! prefix it uses together with [`extract_page_glyphs`].

use super::tests_common::{pdf_page_with, rl, show_at};
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

/// `Tz` scales every horizontal glyph displacement (PDF 32000-1 §9.3.3). The
/// text-matrix walker already applied it, but `Span::advance` ignored it, so a
/// run's right edge overshot the rendered ink by `1 / (Tz/100)`. On the Courier
/// forms that set `Tz` ~50 the over-wide box swallowed the next word space and
/// the column gutter — words fused and gutters vanished.
#[test]
fn tz_scales_the_span_advance_and_tj_offsets() {
    // One 10pt Courier run ("ABC" = 3 * 600/1000 em = 18pt) at Tz 50 -> 9pt.
    let mut ops = vec![
        Operation::new("Tf", vec![Object::Name(b"F1".to_vec()), Object::Real(10.0)]),
        Operation::new("Tz", vec![Object::Real(50.0)]),
    ];
    ops.extend(show_at(50.0, 700.0, "ABC"));
    let (doc, page_id) = pdf_page_with(ops, None, None, [0.0, 0.0, 595.0, 842.0]);
    let page = page_glyph_spans(&doc, page_id).expect("spans");
    let run = page.spans.iter().find(|s| s.text.contains("ABC")).expect("run");
    assert!((run.advance - 9.0).abs() < 1e-6, "advance={}", run.advance);
    // Untransformed, the same run is 18pt wide: the test actually distinguishes.
    assert!((run.x - 50.0).abs() < 1e-6, "x={}", run.x);

    // A `TJ` kern is also horizontal displacement, so it scales with Tz: the
    // second string sits at `(600 + 1000) / 1000 * 10 * 0.5 = 8pt` past the
    // first, not 16pt.
    let mut ops = vec![
        Operation::new("Tf", vec![Object::Name(b"F1".to_vec()), Object::Real(10.0)]),
        Operation::new("Tz", vec![Object::Real(50.0)]),
        Operation::new("Tm", vec![rl(1.0), rl(0.0), rl(0.0), rl(1.0), rl(50.0), rl(700.0)]),
        Operation::new(
            "TJ",
            vec![Object::Array(vec![
                Object::string_literal("A".to_string()),
                Object::Integer(-1000),
                Object::string_literal("B".to_string()),
            ])],
        ),
    ];
    ops.push(Operation::new("ET", vec![]));
    let (doc, page_id) = pdf_page_with(ops, None, None, [0.0, 0.0, 595.0, 842.0]);
    let page = page_glyph_spans(&doc, page_id).expect("spans");
    let a = page.spans.iter().find(|s| s.text == "A").expect("A");
    let b = page.spans.iter().find(|s| s.text == "B").expect("B");
    assert!((b.x - (a.x + 8.0)).abs() < 1e-6, "a.x={} b.x={}", a.x, b.x);
}
