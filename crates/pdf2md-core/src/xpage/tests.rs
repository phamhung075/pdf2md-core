// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Synthetic tests for [`merge_continuation_tables`]. No PDF fixtures: the
//! input is the assembled per-page Markdown chunk list itself.

use super::*;

fn page(n: u32, body: &str) -> (u32, String) {
    (n, body.to_string())
}

fn run(pages: &[(u32, String)]) -> String {
    let mut v = pages.to_vec();
    merge_continuation_tables(&mut v);
    v.iter()
        .map(|(_, c)| c.as_str())
        .collect::<Vec<&str>>()
        .join("\n")
}

fn count_table_headers(md: &str, header: &str) -> usize {
    md.match_indices(header).count()
}

#[test]
fn identical_headers_chain_three_pages_into_one_table() {
    let pages = vec![
        page(
            1,
            "## Page 1\n\nFIRST PAGE TITLE\n\n| A | B |\n| --- | --- |\n| a1 | b1 |\n\nFOOTER ONE\n",
        ),
        page(
            2,
            "## Page 2\n\nSECOND PAGE ADDRESS\n\n| A | B |\n| --- | --- |\n| a2 | b2 |\n\nFOOTER TWO\n",
        ),
        page(
            3,
            "## Page 3\n\nTHIRD PAGE ADDRESS\n\n| A | B |\n| --- | --- |\n| a3 | b3 |\n\nFOOTER THREE\n",
        ),
    ];
    let md = run(&pages);

    // One table: a single header row and one separator row.
    assert_eq!(count_table_headers(&md, "| A | B |\n"), 1, "{md}");
    assert_eq!(count_table_headers(&md, "| --- | --- |\n"), 1, "{md}");

    // Rows stay in page order, under the page-1 header.
    let i1 = md.find("| a1 | b1 |").expect("page 1 row");
    let i2 = md.find("| a2 | b2 |").expect("page 2 row");
    let i3 = md.find("| a3 | b3 |").expect("page 3 row");
    let ih = md.find("| A | B |").expect("header");
    assert!(ih < i1 && i1 < i2 && i2 < i3, "{md}");

    // Every page's non-table content and every `## Page` marker survive.
    for kept in [
        "FOOTER ONE",
        "SECOND PAGE ADDRESS",
        "FOOTER TWO",
        "THIRD PAGE ADDRESS",
        "FOOTER THREE",
        "## Page 2",
        "## Page 3",
    ] {
        assert!(md.contains(kept), "missing {kept:?} in {md}");
    }
}

#[test]
fn different_headers_are_not_merged() {
    let pages = vec![
        page(
            1,
            "## Page 1\n\n| A | B |\n| --- | --- |\n| a1 | b1 |\n",
        ),
        page(
            2,
            "## Page 2\n\n| X | Y |\n| --- | --- |\n| x1 | y1 |\n",
        ),
    ];
    let md = run(&pages);
    assert_eq!(count_table_headers(&md, "| A | B |"), 1);
    assert_eq!(count_table_headers(&md, "| X | Y |"), 1);
    assert!(md.contains("| a1 | b1 |"));
    assert!(md.contains("| x1 | y1 |"));
}

#[test]
fn previous_page_table_not_last_is_not_merged() {
    // Page 1 has two tables; the matching-header one is not its last table, so
    // page 2's first table must not be pulled into it.
    let pages = vec![
        page(
            1,
            "## Page 1\n\n| A | B |\n| --- | --- |\n| a1 | b1 |\n\nINTRO\n\n| X | Y |\n| --- | --- |\n| x1 | y1 |\n",
        ),
        page(
            2,
            "## Page 2\n\n| A | B |\n| --- | --- |\n| a2 | b2 |\n",
        ),
    ];
    let md = run(&pages);
    // The `A | B` header survives on both pages: no merge happened.
    assert_eq!(count_table_headers(&md, "| A | B |"), 2, "{md}");
    assert!(md.contains("| a1 | b1 |"));
    assert!(md.contains("| a2 | b2 |"));
}

#[test]
fn next_page_first_table_not_matching_is_not_merged() {
    // Page 2's *first* table has a different header; its later table repeats
    // page 1's header but is not first, so nothing merges.
    let pages = vec![
        page(
            1,
            "## Page 1\n\n| A | B |\n| --- | --- |\n| a1 | b1 |\n",
        ),
        page(
            2,
            "## Page 2\n\n| X | Y |\n| --- | --- |\n| x1 | y1 |\n\n| A | B |\n| --- | --- |\n| a2 | b2 |\n",
        ),
    ];
    let md = run(&pages);
    assert_eq!(count_table_headers(&md, "| A | B |"), 2, "{md}");
}

#[test]
fn header_match_folds_case_and_in_cell_breaks() {
    let pages = vec![
        page(
            1,
            "## Page 1\n\n| DATE | AMOUNT |\n| --- | --- |\n| d1 | 1,00 |\n",
        ),
        page(
            2,
            "## Page 2\n\n| Date | Amount |\n| --- | --- |\n| d2 | 2,00 |\n",
        ),
        page(
            3,
            "## Page 3\n\n| DATE<br>VALUE | AMOUNT |\n| --- | --- |\n| d3 | 3,00 |\n",
        ),
    ];
    let md = run(&pages);
    // Page 2 merges (case-fold); page 3 does not (its first header differs).
    assert_eq!(count_table_headers(&md, "| DATE | AMOUNT |\n"), 1, "{md}");
    assert!(!md.contains("| Date | Amount |"), "{md}");
    assert!(md.contains("| d2 | 2,00 |"), "{md}");
}

#[test]
fn header_match_removes_br_tags_only_for_real_breaks() {
    let pages = vec![
        page(
            1,
            "## Page 1\n\n| DATE | AMOUNT |\n| --- | --- |\n| d1 | 1,00 |\n",
        ),
        // A real in-cell break: `DA<br>TE` folds to `DATE` and merges.
        page(
            2,
            "## Page 2\n\n| DA<br>TE | AMOUNT |\n| --- | --- |\n| d2 | 2,00 |\n",
        ),
        // A word that merely starts with `<br` is not a tag and does not merge.
        page(
            3,
            "## Page 3\n\n| <brown> | AMOUNT |\n| --- | --- |\n| d3 | 3,00 |\n",
        ),
    ];
    let md = run(&pages);
    assert_eq!(count_table_headers(&md, "| DATE | AMOUNT |\n"), 1, "{md}");
    assert!(md.contains("| d2 | 2,00 |"), "{md}");
    assert!(md.contains("| <brown> | AMOUNT |"), "{md}");
    assert!(md.contains("| d3 | 3,00 |"), "{md}");
}

#[test]
fn headerless_grids_are_not_merged() {
    // A headerless label/value grid renders with an all-empty header; a second
    // one on the next page must not be treated as its continuation.
    let pages = vec![
        page(1, "## Page 1\n\n| | |\n| --- | --- |\n| Total | 10,00 |\n"),
        page(2, "## Page 2\n\n| | |\n| --- | --- |\n| Total | 20,00 |\n"),
    ];
    let md = run(&pages);
    assert!(md.contains("| Total | 10,00 |"));
    assert!(md.contains("| Total | 20,00 |"));
    assert_eq!(md.matches("| --- | --- |").count(), 2, "{md}");
}

#[test]
fn a_table_free_page_breaks_the_chain() {
    let pages = vec![
        page(1, "## Page 1\n\n| A | B |\n| --- | --- |\n| a1 | b1 |\n"),
        page(2, "## Page 2\n\nPROSE PAGE\n"),
        page(3, "## Page 3\n\n| A | B |\n| --- | --- |\n| a3 | b3 |\n"),
    ];
    let md = run(&pages);
    assert_eq!(count_table_headers(&md, "| A | B |"), 2, "{md}");
}

#[test]
fn empty_continuation_table_is_left_alone() {
    let pages = vec![
        page(1, "## Page 1\n\n| A | B |\n| --- | --- |\n| a1 | b1 |\n"),
        page(2, "## Page 2\n\n| A | B |\n| --- | --- |\n"),
    ];
    let md = run(&pages);
    assert_eq!(count_table_headers(&md, "| A | B |"), 2, "{md}");
}

#[test]
fn a_single_page_is_unchanged() {
    let pages = vec![page(
        1,
        "## Page 1\n\n| A | B |\n| --- | --- |\n| a1 | b1 |\n",
    )];
    let md = run(&pages);
    assert_eq!(count_table_headers(&md, "| A | B |"), 1);
}
