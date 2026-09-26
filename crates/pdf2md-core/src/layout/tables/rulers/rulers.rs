// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Stage 3 & Stage 3b ruler scanning, grid alignment, and column corridor analysis.

use super::*;

/// Whether word `i` may testify that its own centre is a column position: it
/// must be either the row's leading word or its own cell, separated from both
/// neighbours by a column gutter. An interior word of a multi-word cell can
/// never be a centred column on its own.
pub(super) fn word_may_define_center(row: &RowInfo, i: usize, min_gutter: f64) -> bool {
    let words = &row.words;
    let sep_left = i == 0 || words[i].x0 - words[i - 1].x1 >= min_gutter;
    let sep_right = i + 1 >= words.len() || words[i + 1].x0 - words[i].x1 >= min_gutter;
    sep_left && sep_right
}

/// Centres of words that are their own cell, clustered across rows, for
/// columns that are neither left- nor right-aligned.
///
/// The start and end passes each need a shared edge; a column whose values are
/// *centred* in their cell (a common layout for numeric totals: "34155",
/// "1146" and "3,40 %" share neither a left nor a right edge, only a centre)
/// produces no ruler from either, so the whole grid is silently dropped to
/// plain text. A centre candidate is kept only when it spans >= 2 rows AND its
/// words are not already anchored by a start or end ruler — a left/right
/// aligned column whose values happen to be equal-width would otherwise gain a
/// redundant ruler that splits the column.
pub(super) fn center_rulers(
    info: &[RowInfo],
    window_rows: &[usize],
    tol: f64,
    min_gutter: f64,
    start_rulers: &[f64],
    end_rulers: &[f64],
) -> Vec<f64> {
    let mut points: Vec<(f64, usize, f64, f64)> = Vec::new();
    for &ri in window_rows {
        let row = &info[ri];
        for i in 0..row.words.len() {
            if word_may_define_center(row, i, min_gutter) {
                let w = &row.words[i];
                points.push((0.5 * (w.x0 + w.x1), ri, w.x0, w.x1));
            }
        }
    }
    if points.len() < 2 {
        return Vec::new();
    }
    points.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

    struct Clust {
        sum_c: f64,
        n: usize,
        rows: std::collections::HashSet<usize>,
        members: Vec<(f64, f64)>,
    }
    let mut clusters: Vec<Clust> = Vec::new();
    for (c, ri, x0, x1) in points {
        if let Some(last) = clusters.last_mut() {
            let mean = last.sum_c / last.n as f64;
            if (c - mean).abs() <= tol {
                last.sum_c += c;
                last.n += 1;
                last.rows.insert(ri);
                last.members.push((x0, x1));
                continue;
            }
        }
        let mut rows = std::collections::HashSet::new();
        rows.insert(ri);
        clusters.push(Clust { sum_c: c, n: 1, rows, members: vec![(x0, x1)] });
    }

    let near = |x: f64, rulers: &[f64]| rulers.iter().any(|&r| (r - x).abs() <= tol * 1.5);
    clusters
        .into_iter()
        .filter(|c| c.rows.len() >= 2)
        .filter(|c| {
            let anchored = c
                .members
                .iter()
                .filter(|(x0, x1)| near(*x0, start_rulers) || near(*x1, end_rulers))
                .count();
            anchored * 2 <= c.members.len()
        })
        .map(|c| c.sum_c / c.n as f64)
        .collect()
}

/// Table column rulers in rows lo..=hi (inclusive): x positions that appear
/// in at least 2 rows within tolerance, spaced by at least min_gutter.
///
/// Three independent clustering passes feed the candidate list: word *starts*
/// (left-aligned columns — descriptions, names, dates), word *ends*
/// (right-aligned columns — quantities, unit prices, Montant HT/TVA/TTC), and
/// word *centres* (centred value columns — see `center_rulers`).
/// A purely start-based scan misses financial tables entirely: "145,50 €",
/// "9,20 €", and "1 200,00 €" have wildly different `x0` (their digit counts
/// differ) but a common `x1` (they're right-aligned to the column edge), so
/// only the end-based pass produces a ruler for that column at all.
///
/// A candidate ruler is dropped if any word in the window *straddles* it
/// (starts strictly to its left and ends strictly to its right). Such an x is
/// not a column boundary — it is an interior word-start of a multi-word cell
/// (e.g. a long line-item description like "Base - 03kVA - du 17/05/25 au
/// 31/07/25"), which would otherwise fragment that single cell into bogus
/// columns and, worse, make the whole window fail the downstream straddle
/// check. A genuine column edge can never be cut through by text, so this
/// pruning can only remove spurious rulers; it never removes a ruler from a
/// table that the caller would already accept (such tables already pass the
/// same straddle test for every interior ruler).
///
/// A row that has no column-like spacing of its own — every one of its words
/// sits within one ordinary word-space of the next, i.e. it reads as a single
/// flowing sentence rather than separate cells — is exempt from vetoing any
/// ruler this way (see `row_straddles`). Such a row is typically a wrapped
/// continuation of a multi-line description (e.g. "Période de 01/12/2018 au
/// 31/12/2018" following an item row with real numeric columns): it has no
/// aligned cells of its own, so it says nothing about where the table's real
/// column boundaries are, and letting it veto every candidate — as a naive
/// straddle check would — silently drops the whole table to plain text even
/// though every numeric row agrees on the columns.
pub fn table_rulers(info: &[RowInfo], tol: f64, lo: usize, hi: usize, min_gutter: f64) -> Vec<f64> {
    // Preserve the original (strict) public behaviour; callers that want the
    // relaxed merged-cell veto use `table_rulers_opts`.
    table_rulers_opts(info, tol, lo, hi, min_gutter, false)
}

/// [`table_rulers`] with the merged-cell veto optionally disabled. When
/// `wide_ok` is false, a word crossing a candidate start ruler always vetoes
/// it (the original, geometrically strict behaviour); when true, a wide
/// merged/spanning cell that begins at a real column and covers another one is
/// tolerated (see `row_straddles_wide_ok`).
pub(super) fn table_rulers_opts(
    info: &[RowInfo],
    tol: f64,
    lo: usize,
    hi: usize,
    min_gutter: f64,
    wide_ok: bool,
) -> Vec<f64> {
    let num_rows = hi - lo + 1;
    if num_rows < 2 {
        return Vec::new();
    }
    let mut starts_with_row: Vec<(f64, usize)> = Vec::new();
    let mut ends_with_row: Vec<(f64, usize)> = Vec::new();
    for ri in lo..=hi {
        for &s in &info[ri].starts {
            starts_with_row.push((s, ri));
        }
        for &e in &info[ri].ends {
            ends_with_row.push((e, ri));
        }
    }
    if starts_with_row.is_empty() && ends_with_row.is_empty() {
        return Vec::new();
    }

    let start_candidates = cluster_positions(starts_with_row, tol);
    let end_candidates = cluster_positions(ends_with_row, tol);

    // Drop "rulers" that are actually interior word-starts/ends of one cell.
    // Start-derived candidates use the relaxed test (a wide merged cell may
    // legitimately cover them); end-derived candidates use the strict test.
    // Collapse near-duplicate positions, keeping the leftmost of each group.
    let dedup = |mut v: Vec<f64>| -> Vec<f64> {
        v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let mut out: Vec<f64> = Vec::new();
        for r in v {
            match out.last() {
                Some(prev) if r - *prev < min_gutter => {}
                _ => out.push(r),
            }
        }
        out
    };

    let window_rows: Vec<usize> = (lo..=hi).collect();
    let col_starts = start_candidates.clone();

    // Every distinct candidate position, used to count how many column rulers
    // an isolated wide cell spans (see `classify_straddle`).
    let mut all_rulers: Vec<f64> = Vec::new();
    for &r in start_candidates.iter().chain(end_candidates.iter()) {
        if all_rulers.iter().all(|&p| (r - p).abs() > tol) {
            all_rulers.push(r);
        }
    }
    all_rulers.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let strong_support = |x: f64| -> bool {
        let n = window_rows
            .iter()
            .filter(|&&ri| row_matches_ruler(&info[ri], x, tol))
            .count();
        if window_rows.len() <= 2 {
            n >= 1
        } else {
            n >= 2
        }
    };
    // Straddle veto with isolated-cell tolerance: a cell that merely contains
    // `x` is tolerated only in the table's header row (row 0 of the window),
    // where a wide header label ("Dossard" 74..119) legitimately sits over a
    // narrower numeric column (bibs 90..108 in 12ac87e6). In data or trailing
    // rows, a crossing word indicates an interior word, not a column boundary.
    let straddle_ok = |ri: usize, x: f64| -> bool {
        match classify_straddle(&info[ri], &col_starts, x, tol, min_gutter) {
            Straddle::None => true,
            Straddle::Contained => ri == window_rows[0] && strong_support(x),
            Straddle::Spanning | Straddle::Veto => false,
        }
    };
    // An end-derived ruler is a value column's own *right edge*: any word
    // crossing it is a fragmentation signal, not a wide cell that merely
    // contains a start ruler. Only a clean crossing (`Straddle::None`) may
    // pass, exactly as before the `classify_straddle` relaxation. The
    // `Contained` tolerance belongs to *start* rulers, where a wide header
    // cell legitimately sits over a narrower value column ("Dossard" over the
    // bib ruler); applying it to end candidates admitted spurious word-right-
    // edge rulers a few points past a real column start (the pressure table in
    // `0225173d`), which then tripped the window gutter check and collapsed the
    // whole grid.
    let end_straddle_ok = |ri: usize, x: f64| -> bool {
        match classify_straddle(&info[ri], &col_starts, x, tol, min_gutter) {
            Straddle::None => true,
            Straddle::Contained => ri == window_rows[0] && strong_support(x),
            Straddle::Spanning | Straddle::Veto => false,
        }
    };
    let start_rulers = dedup(
        start_candidates
            .into_iter()
            .filter(|&x| {
                window_rows.iter().all(|&ri| {
                    if wide_ok {
                        !row_straddles_wide_ok(&info, &col_starts, ri, x, tol, min_gutter)
                    } else {
                        straddle_ok(ri, x)
                    }
                })
            })
            // A column start must be a separate cell in at least one row; a
            // position that only ever falls between the second and later words
            // of a tight cell (an ordinary word space) is an interior
            // word-start, not a boundary. See `start_ruler_is_separate_cell`.
            .filter(|&x| {
                window_rows
                    .iter()
                    .any(|&ri| start_ruler_is_separate_cell(&info[ri], x, tol, min_gutter))
            })
            .collect(),
    );
    let end_rulers = dedup(
        end_candidates
            .into_iter()
            .filter(|&x| {
                window_rows.iter().all(|&ri| end_straddle_ok(ri, x))
                    // The end pass exists for a *right-aligned value column*
                    // whose values are their own cells. A left-aligned text
                    // column whose two longest cells merely share a rendered
                    // width — the target fixture's "Gesamtbetrag der
                    // Zuschläge" / "…Abschläge" — ends on the same x with no
                    // cell boundary there: each is one contiguous multi-word
                    // label anchored at that column's own start ruler. Reading
                    // their shared right edge as a new column boundary splits
                    // the label column and strands the unit "EUR" in a phantom
                    // middle column, which the totals-row consolidator then
                    // folds into a bogus multi-row span. Require at least two
                    // rows where the word ending on x is itself a separate cell
                    // (the row's first word, or preceded by a column gutter), so
                    // a shared text right-edge cannot invent a column.
                    && window_rows
                        .iter()
                        .filter(|&&ri| end_ruler_is_separate_cell(&info[ri], x, tol, min_gutter))
                        .count()
                        >= 2
            })
            .collect(),
    );

    // Start-aligned boundaries are the primary column signal, so never let an
    // end-derived ruler displace a start-derived one. An end cluster is only a
    // word's right edge; if it sits a few points to the left of the next
    // column's start (a normal narrow gutter), the old x-order dedup dropped
    // the *start* in favour of the *end* (`|start - end| < min_gutter`), which
    // silently deleted the whole next column. That only became visible once
    // `Span.advance` was measured correctly: with the old 1-em advances every
    // end sat at `start + 10pt`, safely clear of the following start. Keeping
    // starts first and only *adding* end-derived rulers that are at least
    // `min_gutter` away preserves the right-aligned-column support that the
    // end pass exists for.
    let center_candidates =
        center_rulers(info, &window_rows, tol, min_gutter, &start_rulers, &end_rulers);
    let mut merged = start_rulers;
    for e in end_rulers {
        let redundant = merged.iter().any(|&r| (e - r).abs() <= tol || (r < e && e - r < min_gutter));
        if !redundant {
            merged.push(e);
        }
    }
    for c in center_candidates {
        // A centre candidate is only as good as its weakest row: a word that
        // spans the centre in *any* window row — e.g. a long label whose two
        // word-spans happen to fall either side of the candidate — means the x
        // is interior to a cell, not a column boundary. Start/end candidates
        // already pass this veto above; centre candidates must too, or the
        // recovered centre ruler fragments that row's label column.
        let straddled = window_rows.iter().any(|&ri| {
            if wide_ok {
                row_straddles_wide_ok(&info, &col_starts, ri, c, tol, min_gutter)
            } else {
                !straddle_ok(ri, c)
            }
        });
        if !straddled && merged.iter().all(|&r| (c - r).abs() >= min_gutter) {
            merged.push(c);
        }
    }
    merged.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    merged
}

/// Whether row `row` has a word aligned to ruler `r` — by its start (an
/// ordinary left-aligned column) or its end (a right-aligned numeric
/// column). Used wherever a row is checked for "does it actually populate
/// this column", so seed/continuation validation accepts a right-aligned
/// ruler on the same footing as a left-aligned one — checking only `starts`
/// would reject every row of a financial table whose only reliable column
/// signal is its aligned right edge.
pub(super) fn row_matches_ruler(row: &RowInfo, r: f64, tol: f64) -> bool {
    row.starts.iter().any(|&s| (r - s).abs() <= tol)
        || row.ends.iter().any(|&e| (r - e).abs() <= tol)
        || row
            .words
            .iter()
            .any(|w| (0.5 * (w.x0 + w.x1) - r).abs() <= tol)
}
