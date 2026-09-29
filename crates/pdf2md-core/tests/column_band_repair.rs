// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! End-to-end test for the class-A fix: a two-column page whose right column
//! starts one baseline above the left column (a one-sided top row) must be read
//! left column then right column, not woven row by row.
//!
//! The fixture is generated in-process (lopdf): a full-width three-cell header
//! block (so the page renders through `render_with_tables` and the body's
//! reading order comes from `detect_column_bands`, exactly the failing shape),
//! then a two-column body whose first row carries only the right column. Each
//! column's line is drawn as one `Tj` per word so the row-local splitter can
//! seed on the page gutter; the leading one-sided row is what the repair pass
//! has to fold into the right column.

use lopdf::content::{Content, Operation};
use lopdf::{dictionary, Document, Object, Stream};
use pdf2md_core::{convert_pdf_bytes_to_markdown, ConversionOptions};

const FONT_SIZE: f64 = 16.0;
/// Left column margin, right column margin, and the per-character stride used
/// to lay a column's words out with small (< 1.2em) inter-word gaps.
const LEFT_X: f64 = 60.0;
const RIGHT_X: f64 = 330.0;
const CHAR_STRIDE: f64 = 8.0;
const WORD_GAP: f64 = 4.0;

fn esc(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('(', "\\(")
        .replace(')', "\\)")
}

/// Lay one column's words left-to-right at `x0`, returning `(x, text)` pairs.
fn column_words(words: &[&str], x0: f64) -> Vec<(f64, String)> {
    let mut x = x0;
    let mut out = Vec::new();
    for w in words {
        out.push((x, (*w).to_string()));
        x += w.len() as f64 * CHAR_STRIDE + WORD_GAP;
    }
    out
}

/// `(x, y, text)` fragments drawn one per `Tm`/`Tj` pair on a single page.
fn build_pdf(fragments: &[(f64, f64, String)]) -> Vec<u8> {
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let font = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
    });
    let resources_id = doc.add_object(dictionary! {
        "Font" => dictionary! { "F1" => font },
    });

    let mut ops = vec![
        Operation::new("BT", vec![]),
        Operation::new("Tf", vec!["F1".into(), FONT_SIZE.into()]),
    ];
    for (x, y, text) in fragments {
        ops.push(Operation::new(
            "Tm",
            vec![1.0.into(), 0.0.into(), 0.0.into(), 1.0.into(), (*x).into(), (*y).into()],
        ));
        ops.push(Operation::new("Tj", vec![Object::string_literal(esc(text))]));
    }
    ops.push(Operation::new("ET", vec![]));

    let content_id = doc.add_object(Stream::new(
        dictionary! {},
        Content { operations: ops }.encode().unwrap(),
    ));
    let page_id = doc.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "Contents" => content_id,
    });
    let pages = dictionary! {
        "Type" => "Pages",
        "Kids" => vec![page_id.into()],
        "Count" => 1,
        "Resources" => resources_id,
        "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
    };
    doc.objects.insert(pages_id, Object::Dictionary(pages));
    let catalog_id = doc.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    doc.trailer.set("Root", catalog_id);
    let mut bytes = Vec::new();
    doc.save_to(&mut bytes).expect("save pdf");
    bytes
}

#[test]
fn leading_one_sided_right_row_reads_after_the_left_column() {
    let mut fragments: Vec<(f64, f64, String)> = Vec::new();
    // Full-width three-cell header block: detected as a table, so the body
    // below is banded by `detect_column_bands` rather than `page_two_columns`.
    let header = [
        (60.0, "Carnegie Mellon University", 250.0, "Microsoft Research", 430.0, "Microsoft Research"),
        (60.0, "5000 Forbes Avenue", 250.0, "One Microsoft Way", 430.0, "One Microsoft Way"),
        (60.0, "Pittsburgh, PA", 250.0, "Redmond, WA", 430.0, "Redmond, WA"),
        (60.0, "15213", 250.0, "98052", 430.0, "98052"),
    ];
    for (i, (x1, t1, x2, t2, x3, t3)) in header.iter().enumerate() {
        let y = 800.0 - i as f64 * 20.0;
        for (x, t) in [(x1, t1), (x2, t2), (x3, t3)] {
            fragments.push((*x, y, (*t).to_string()));
        }
    }

    // The right column's top line sits one baseline above the first crossing
    // row: the running-gutter pass leaves it in a `Full` band flushed before
    // the column run, so only the repair pass keeps it in the right stream.
    fragments.push((RIGHT_X, 720.0, "righttopline".to_string()));
    let body = [
        (700.0, &["leftalpha", "beta", "gamma"][..], &["rightone", "two", "three"][..]),
        (683.0, &["delta", "epsilon", "zeta", "eta"][..], &["rightfour", "five", "six"][..]),
        (666.0, &["theta", "iota", "kappa"][..], &["rightseven", "eight", "nine", "ten"][..]),
        (649.0, &["lambda", "mu", "nu"][..], &["righteleven", "twelve", "thirteen"][..]),
        (632.0, &["xi", "omicron", "pi", "rho"][..], &["rightfourteen", "fifteen", "sixteen"][..]),
        (615.0, &["sigma", "tau", "upsilon"][..], &["rightseventeen", "eighteen", "nineteen"][..]),
    ];
    for (y, left, right) in body {
        for (x, t) in column_words(left, LEFT_X) {
            fragments.push((x, y, t));
        }
        for (x, t) in column_words(right, RIGHT_X) {
            fragments.push((x, y, t));
        }
    }

    let bytes = build_pdf(&fragments);
    let res = convert_pdf_bytes_to_markdown(&bytes, &ConversionOptions::default()).unwrap();
    let md = &res.markdown;
    println!("--- markdown ---\n{md}\n----------------");

    let pos = |needle: &str| {
        md.find(needle)
            .unwrap_or_else(|| panic!("{needle:?} missing from output:\n{md}"))
    };
    // Every left-column line must precede the right column's top line, which in
    // turn precedes the right column's first crossing line. The woven bug put
    // `righttopline` first (before all of the left column).
    let right_top = pos("righttopline");
    for left in ["leftalpha", "delta", "theta", "lambda", "xi", "sigma"] {
        assert!(
            pos(left) < right_top,
            "left-column {left:?} must come before the right column:\n{md}"
        );
    }
    assert!(
        right_top < pos("rightone"),
        "the right column must stay top-to-bottom after its leading row:\n{md}"
    );
}
