// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Synthetic Type3-font metric tests.
//!
//! A Type3 font defines its own glyph space through `/FontMatrix`, so `/Widths`
//! and the `Tf` size operand must be mapped into real points before any
//! gap-based layout runs. The fixtures are built in-test with lopdf — no
//! fixture files, no personal data.

use super::tests_common::*;
use super::*;
use lopdf::{dictionary, Document, Object, ObjectId, Stream};

const SPACE: u8 = b' ';
const COMMA: u8 = b',';
const FIRST_CHAR: u8 = b' ';

/// A minimal Type3 font dictionary: `/Widths` in glyph units, a `/FontMatrix`
/// mapping glyph space to text space, and a `/FontBBox` giving the glyph
/// extent. `widths` is indexed from `FIRST_CHAR`.
fn type3_font(
    doc: &mut Document,
    matrix: [f64; 6],
    bbox: [f64; 4],
    widths: &[f64],
) -> ObjectId {
    let id = doc.new_object_id();
    doc.objects.insert(
        id,
        Object::Dictionary(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type3",
            "FontMatrix" => Object::Array(matrix.iter().map(|&v| rl(v)).collect()),
            "FontBBox" => Object::Array(bbox.iter().map(|&v| rl(v)).collect()),
            "Encoding" => "WinAnsiEncoding",
            "FirstChar" => FIRST_CHAR as i64,
            "LastChar" => FIRST_CHAR as i64 + widths.len() as i64 - 1,
            "Widths" => Object::Array(widths.iter().map(|&w| rl(w)).collect()),
        }),
    );
    id
}

/// `/Widths` for the printable ASCII range, with the given glyph-space widths
/// for a digit, the comma, and the space glyph. The digit width also stands in
/// for every digit (`0`–`9`).
fn ascii_widths(digit: f64, comma: f64, space: f64) -> Vec<f64> {
    let mut w = vec![0.0f64; (b'~' - FIRST_CHAR) as usize + 1];
    w[(SPACE - FIRST_CHAR) as usize] = space;
    w[(COMMA - FIRST_CHAR) as usize] = comma;
    for c in b'0'..=b'9' {
        w[(c - FIRST_CHAR) as usize] = digit;
    }
    w
}

/// One-page document whose `/F1` is a Type3 font with the given matrix, box and
/// glyph-space widths.
fn pdf_type3_page(
    matrix: [f64; 6],
    bbox: [f64; 4],
    widths: &[f64],
    ops: Vec<Operation>,
) -> (Document, ObjectId) {
    let mut doc = Document::with_version("1.5");
    let font = type3_font(&mut doc, matrix, bbox, widths);
    let pages_id = doc.new_object_id();
    let res = doc.add_object(dictionary! { "Font" => dictionary! { "F1" => font } });
    let content = doc.add_object(Stream::new(
        Dictionary::new(),
        Content { operations: ops }.encode().unwrap(),
    ));
    let page_id = doc.add_object(dictionary! {
        "Type" => "Page", "Parent" => pages_id, "Contents" => content, "Resources" => res,
    });
    let pages = dictionary! {
        "Type" => "Pages", "Kids" => vec![page_id.into()], "Count" => 1,
        "MediaBox" => vec![rl(0.0), rl(0.0), rl(595.0), rl(842.0)],
    };
    doc.objects.insert(pages_id, Object::Dictionary(pages));
    let cat = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
    doc.trailer.set("Root", cat);
    (doc, page_id)
}

/// A 0.24 Type3 matrix over a 47-unit box, matching the common statement font.
fn statement_matrix() -> [f64; 6] {
    [0.24, 0.0, 0.0, 0.24, 0.0, 0.0]
}

/// The standard 1/1000 Type3 matrix (TeX bitmap fonts and other faces that
/// keep glyph space at 1/1000 em).
fn standard_matrix() -> [f64; 6] {
    [
        STANDARD_GLYPH_SCALE,
        0.0,
        0.0,
        STANDARD_GLYPH_SCALE,
        0.0,
        0.0,
    ]
}

fn statement_bbox() -> [f64; 4] {
    [-2.0, -9.0, 38.0, 38.0]
}

#[test]
fn type3_widths_and_em_scale_follow_font_matrix() {
    let widths = ascii_widths(17.0, 9.0, 9.0);
    let doc = Document::new();
    let font = dictionary! {
        "Subtype" => "Type3",
        "FontMatrix" => Object::Array(statement_matrix().iter().map(|&v| rl(v)).collect()),
        "FontBBox" => Object::Array(statement_bbox().iter().map(|&v| rl(v)).collect()),
        "FirstChar" => FIRST_CHAR as i64,
        "Widths" => Object::Array(widths.iter().map(|&w| rl(w)).collect()),
    };
    let w = resolve_widths(&doc, &font);
    assert!(
        matches!(w, Widths::ByteType3 { .. }),
        "a Type3 font must carry its glyph-space widths and em scale"
    );
    // 17 glyph units x 0.24 maps to 4.08 pt (x1000 for the 1/1000-em table).
    assert!((w.width(b"1").unwrap() - 17.0 * 0.24 * 1000.0).abs() < 1e-3);
    // The em is the box height through the vertical matrix scale, divided by
    // the typical box/em ratio: 0.24 * 47 / 1.2.
    let expected = 0.24 * 47.0 / 1.2;
    assert!((w.em_scale() - expected).abs() < 1e-6, "em scale {}", w.em_scale());

    // A non-Type3 font with the same /Widths keeps the 1/1000-em convention.
    let mut plain = font.clone();
    plain.set(b"Subtype", Object::Name(b"Type1".to_vec()));
    let p = resolve_widths(&doc, &plain);
    assert!(matches!(p, Widths::Byte(_)));
    assert!((p.width(b"1").unwrap() - 17.0).abs() < 1e-9);
    assert_eq!(p.em_scale(), 1.0);
}

#[test]
fn type3_standard_matrix_keeps_em_scale_one() {
    // Glyph space is already 1/1000 em, so the `Tf` operand is already the em:
    // the em scale must be exactly 1.0 and `/Widths` pass through unscaled.
    // The box is deliberately non-typical (height 1900 glyph units); it must
    // not influence the scale for a standard matrix.
    let widths = ascii_widths(17.0, 9.0, 9.0);
    let doc = Document::new();
    let font = dictionary! {
        "Subtype" => "Type3",
        "FontMatrix" => Object::Array(standard_matrix().iter().map(|&v| rl(v)).collect()),
        "FontBBox" => Object::Array(
            [0.0, -900.0, 1000.0, 1000.0].iter().map(|&v| rl(v)).collect(),
        ),
        "FirstChar" => FIRST_CHAR as i64,
        "Widths" => Object::Array(widths.iter().map(|&w| rl(w)).collect()),
    };
    let w = resolve_widths(&doc, &font);
    assert!(
        matches!(w, Widths::ByteType3 { .. }),
        "a Type3 font with /Widths must expose them"
    );
    assert_eq!(w.em_scale(), 1.0);
    // Standard matrix => width_scale 1.0, so glyph-space widths are unchanged.
    // `rl` stores the matrix as f32, so the scale is 1.0 only to ~1e-8.
    assert!(
        (w.width(b"1").unwrap() - 17.0).abs() < 1e-3,
        "digit width {:?}",
        w.width(b"1")
    );
    assert!((w.width(b",").unwrap() - 9.0).abs() < 1e-3);
}

#[test]
fn type3_without_widths_has_no_usable_metrics() {
    // Type3 `/Widths` is required and has no base-14 equivalent, so a font
    // missing it must fall back to no metrics rather than to guessed widths.
    let doc = Document::new();
    let font = dictionary! {
        "Subtype" => "Type3",
        "BaseFont" => "Helvetica",
        "FontMatrix" => Object::Array(statement_matrix().iter().map(|&v| rl(v)).collect()),
        "FontBBox" => Object::Array(statement_bbox().iter().map(|&v| rl(v)).collect()),
    };
    assert!(matches!(resolve_widths(&doc, &font), Widths::None));
}

#[test]
fn type3_explicit_offsets_render_one_line_with_single_spaces() {
    let widths = ascii_widths(17.0, 9.0, 9.0);
    let ops = vec![
        Operation::new("BT", vec![]),
        Operation::new("Tf", vec![Object::Name(b"F1".to_vec()), rl(1.0)]),
        Operation::new(
            "Tm",
            vec![rl(1.0), rl(0.0), rl(0.0), rl(1.0), rl(100.0), rl(700.0)],
        ),
        Operation::new("Tj", vec![Object::string_literal("1".to_string())]),
        // The producer's explicit thousands separator: "1" then 6.24 pt on.
        Operation::new("Td", vec![rl(6.24), rl(0.0)]),
        Operation::new("Tj", vec![Object::string_literal("234,56".to_string())]),
        Operation::new("ET", vec![]),
    ];
    let (doc, page_id) = pdf_type3_page(statement_matrix(), statement_bbox(), &widths, ops);
    let (spans, _) = walk_page(&doc, page_id);
    assert_eq!(spans.len(), 2, "one run per Tj");
    // Tf 1 with the 0.24 matrix over a 47-unit box is ~9.4 pt, not 1 pt.
    assert!((spans[0].size - 9.4).abs() < 1e-6, "size {}", spans[0].size);
    // "1" advances 4.08 pt, so the 6.24 pt offset leaves a 2.16 pt word space.
    assert!((spans[0].advance - 4.08).abs() < 1e-6, "advance {}", spans[0].advance);
    let gap = spans[1].x - (spans[0].x + spans[0].word_advance);
    assert!((gap - 2.16).abs() < 1e-6, "residual gap {gap}");

    let pt = extract_page_glyphs(&doc, page_id, false, false, false).expect("page text");
    assert!(
        pt.text.contains("1 234,56"),
        "amount must stay one line: {:?}",
        pt.text
    );
    assert!(
        !pt.text.contains("1\n234"),
        "amount broke across lines: {:?}",
        pt.text
    );
}

#[test]
fn type3_amount_stays_one_table_cell() {
    let widths = ascii_widths(17.0, 9.0, 9.0);
    let mut ops = vec![
        Operation::new("BT", vec![]),
        Operation::new("Tf", vec![Object::Name(b"F1".to_vec()), rl(1.0)]),
    ];
    for (x, text) in [(50.0, "REF"), (150.0, "QTY"), (300.0, "AMT")] {
        ops.extend(show_at(x, 700.0, text));
    }
    let row = |ops: &mut Vec<Operation>, y: f64, label: &str, qty: &str, whole: &str, rest: &str| {
        ops.extend(show_at(50.0, y, label));
        ops.extend(show_at(150.0, y, qty));
        ops.push(tm_at(1.0, 0.0, 0.0, 1.0, 300.0, y));
        ops.push(Operation::new("Tj", vec![Object::string_literal(whole.to_string())]));
        ops.push(Operation::new("Td", vec![rl(6.24), rl(0.0)]));
        ops.push(Operation::new("Tj", vec![Object::string_literal(rest.to_string())]));
    };
    row(&mut ops, 680.0, "A1", "2", "1", "234,56");
    row(&mut ops, 660.0, "B2", "1", "5", "678,90");
    ops.push(Operation::new("ET", vec![]));

    let (doc, page_id) = pdf_type3_page(statement_matrix(), statement_bbox(), &widths, ops);
    let pt = extract_page_glyphs(&doc, page_id, true, true, false).expect("page text");
    assert!(pt.tables >= 1, "table not recovered: {:?}", pt.text);
    assert!(
        pt.text.contains("1 234,56"),
        "amount split across cells: {:?}",
        pt.text
    );
    assert!(
        !pt.text.contains("| 1 | 234,56 |"),
        "amount rendered as two cells: {:?}",
        pt.text
    );
}
