// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Integration tests for the `pdf2md` binary's `--page-markers` flag.
//!
//! Note: this crate's `.gitignore` (`**/tests/`) normally keeps tests out of
//! the public repository; the orchestrator should `git add -f` this file if it
//! must ship. The synthetic fixture is built inline so no binary fixture is
//! ever committed.

use std::process::Command;

/// A two-page synthetic PDF with a text layer, built by hand so the test has
/// no PDF-writing dependency. Unequal page content keeps the page boundary
/// honest.
fn build_two_page_pdf() -> Vec<u8> {
    let res = "<< /Font << /F1 3 0 R >> >>";
    let objects: Vec<String> = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [4 0 R 6 0 R] /Count 2 >>".to_string(),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>"
            .to_string(),
        format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] /Resources {res} /Contents 5 0 R >>"
        ),
        content_obj("BT /F1 12 Tf 72 700 Td (CLI PAGE ONE UNIQUE) Tj ET"),
        format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] /Resources {res} /Contents 7 0 R >>"
        ),
        content_obj("BT /F1 12 Tf 72 700 Td (CLI PAGE TWO UNIQUE) Tj ET"),
    ];

    let mut out: Vec<u8> = b"%PDF-1.4\n".to_vec();
    let mut offsets = vec![0usize; objects.len() + 1];
    for (i, body) in objects.iter().enumerate() {
        offsets[i + 1] = out.len();
        out.extend_from_slice(format!("{} 0 obj\n{}\nendobj\n", i + 1, body).as_bytes());
    }
    let xref_pos = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n");
    for offset in offsets.iter().skip(1) {
        out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_pos}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    out
}

fn content_obj(content: &str) -> String {
    format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len())
}

fn run_cli(pdf: &std::path::Path, extra: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_pdf2md"))
        .args(extra)
        .arg(pdf)
        .output()
        .expect("spawn pdf2md");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

#[test]
fn page_markers_flag_inserts_markers_and_default_omits_them() {
    let dir = std::env::temp_dir().join(format!("pdf2md-cli-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let pdf = dir.join("two-page.pdf");
    std::fs::write(&pdf, build_two_page_pdf()).expect("write fixture");

    let (ok, without) = run_cli(&pdf, &["--quiet"]);
    assert!(ok, "conversion without the flag must succeed: {without}");
    assert!(
        !without.contains("pdf2w:page"),
        "default must emit no marker:\n{without}"
    );

    let (ok, with) = run_cli(&pdf, &["--quiet", "--page-markers"]);
    assert!(ok, "conversion with --page-markers must succeed: {with}");
    assert_eq!(
        with.matches("<!-- pdf2w:page n=\"").count(),
        2,
        "one marker per page:\n{with}"
    );
    let first = with
        .find("<!-- pdf2w:page n=\"1\" -->")
        .expect("page 1 marker");
    let second = with
        .find("<!-- pdf2w:page n=\"2\" -->")
        .expect("page 2 marker");
    assert!(first < second, "markers must be ordered 1 then 2:\n{with}");
    assert!(
        with[first..second].contains("CLI PAGE ONE UNIQUE"),
        "page 1 marker must precede page 1 text:\n{with}"
    );
    assert!(
        with[second..].contains("CLI PAGE TWO UNIQUE"),
        "page 2 marker must precede page 2 text:\n{with}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
