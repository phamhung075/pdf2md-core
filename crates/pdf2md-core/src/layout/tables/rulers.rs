// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Stage 3 & Stage 3b ruler scanning, grid alignment, and column corridor analysis.

use crate::layout::glyph_stream::Span;
use crate::layout::reading_order::{detect_column_bands, ColumnBand};
use crate::layout::tables::consolidation::{bucket, bucket_rows_content_aware, bucket_words, consolidate_table_rows, merge_complementary_columns};
use crate::layout::tables::validation::is_tabular_rows;
use crate::models::BoundingBox;

/// A detected table: line range [start, end] (inclusive) plus cell rows.
#[derive(Debug, Clone)]
pub struct TableHit {
    pub start: usize,
    pub end: usize,
    pub rows: Vec<Vec<String>>,
    pub bbox: BoundingBox,
}

/// One decoded word with its device start x and end x (the column-start ruler).
#[derive(Debug, Clone)]
pub struct WordTok {
    pub text: String,
    pub x0: f64,
    pub x1: f64,
}

/// Word tokens + sorted start/end positions per visual line. `ends` (word
/// `x1`) exists alongside `starts` (word `x0`) so a column can be recognized
/// by either its left edge (ordinary left-aligned text: descriptions, names)
/// or its right edge (right-aligned numeric columns: quantities, unit prices,
/// Montant HT/TVA/TTC) — see `table_rulers`'s doc comment.
#[derive(Debug, Clone)]
pub struct RowInfo {
    pub words: Vec<WordTok>,
    pub starts: Vec<f64>,
    pub ends: Vec<f64>,
    pub size: f64,
}

/// Split one visual line into words (same gap rules as the renderer).
pub fn line_words(line: &[Span]) -> Vec<WordTok> {
    let mut out: Vec<WordTok> = Vec::new();
    let mut text = String::new();
    let mut x0: Option<f64> = None;
    let mut prev_x: Option<f64> = None;
    let mut prev_advance = 0.0f64;
    let mut last_end = 0.0f64;

    let flush = |out: &mut Vec<WordTok>, text: &mut String, x0: &mut Option<f64>, last_end: f64| {
        if let Some(s) = x0.take() {
            if !text.trim().is_empty() {
                out.push(WordTok {
                    text: std::mem::take(text).trim().to_string(),
                    x0: s,
                    x1: last_end,
                });
            }
            text.clear();
        }
    };

    for sp in line {
        if sp.text.is_empty() {
            continue;
        }
        let size = sp.size.max(0.1);
        let space_adv = 0.25 * size;
        let is_space = sp.text.chars().all(|c| c == ' ');
        if let Some(px) = prev_x {
            // Measure the *residual* whitespace between the two spans
            // (start-to-start distance minus the previous span's own advance),
            // exactly as `reading_order::render_spans` and `split_hard_breaks`
            // do. Comparing the raw start-to-start distance against the 2.5em
            // column threshold makes any multi-span word wider than ~2.5em in
            // total look like a new column: "Customer VAT Number" drawn as
            // `C`+`us`+`t`+`ome`+`r `+`V`+`A`+`T`+` `+`N`+`umbe`+`r` fragmented
            // into "Numbe" + "r", and "Huile d'olive à l'ancienne" into
            // "Huile d'" + "olive à l'" + "ancien" + "ne" — the table bucketer
            // then dropped each stray fragment into its own column/cell. The
            // residual here is only the real inter-glyph gap, so a genuine
            // column gutter (which `render_spans` still breaks on) keeps
            // splitting while intra-word kerns never do.
            let gap = (sp.x - px) - prev_advance;
            let word_break =
                is_space || (sp.text != " " && (gap > 2.5 * size || gap > 0.65 * space_adv));
            if word_break {
                flush(&mut out, &mut text, &mut x0, last_end);
            }
        } else if is_space {
            continue;
        }
        if x0.is_none() {
            x0 = Some(sp.x);
        }
        // Keep the span's own internal whitespace. PDF producers routinely
        // split one line into chunks that *carry* the separating space, e.g.
        // `(Pé)(r)(i)(o)(de)( de)( 0)...` or `(RE )(N° )(:)`; the old
        // `sp.text.trim()` dropped those spaces and fused whole table cells
        // into one token ("Périodede01/12/2018au31/12/2018", "FACTUREN°:").
        // The outer ends are still trimmed by `flush`, so a token never keeps
        // leading/trailing spaces, and the token count / x positions used for
        // column detection are unchanged.
        text.push_str(sp.text.as_str());
        prev_x = Some(sp.x);
        prev_advance = sp.advance;
        last_end = sp.x + sp.advance;
    }
    flush(&mut out, &mut text, &mut x0, last_end);
    merge_value_symbol_tokens(merge_sign_tokens(out))
}

/// Fold a lone sign glyph back onto the number it prefixes ("-" + "93" →
/// "-93"). A producer often draws the sign as its own run a few points left of
/// the digits — a kerning gap wider than the word-space threshold in
/// `line_words` — so the two become separate tokens, are bucketed into
/// separate columns, and the sign is emitted in one cell while the digits land
/// in the next ("- -" / "93<br>269"). Joining them keeps the value — and its
/// sign — in one cell.
///
/// Only a token that is *exactly* a sign and is immediately followed by the
/// line's final token is joined. That is the shape of a right-aligned signed
/// amount at the end of a row; an inline separator inside a longer run
/// ("022 735 - 3477 (service …)") is left untouched, as is a standalone dash
/// cell or a text bullet.
fn merge_sign_tokens(words: Vec<WordTok>) -> Vec<WordTok> {
    let last = words.len().saturating_sub(1);
    let mut out: Vec<WordTok> = Vec::with_capacity(words.len());
    for (i, w) in words.into_iter().enumerate() {
        if let Some(prev) = out.last_mut() {
            let prev_is_sign = matches!(prev.text.as_str(), "-" | "−" | "+");
            let starts_digit = w
                .text
                .chars()
                .next()
                .map(|c| c.is_ascii_digit())
                .unwrap_or(false);
            if i == last && prev_is_sign && starts_digit {
                prev.text.push_str(&w.text);
                prev.x1 = prev.x1.max(w.x1);
                continue;
            }
        }
        out.push(w);
    }
    out
}

/// A lone currency/percent symbol that a PDF producer emitted as its own glyph
/// run. These are units, never standalone table columns.
fn is_value_symbol(text: &str) -> bool {
    matches!(text, "€" | "$" | "£" | "¥" | "%" | "₽" | "¢")
}

/// Fold a bare trailing currency/percent symbol back into the numeric token it
/// immediately follows. Producers routinely draw "81,90 €" as two runs, the
/// amount and then the symbol starting exactly at the amount's right edge (the
/// intervening space span is consumed by `line_words`). Left alone, that symbol
/// becomes a word whose start x is zero-distance from the number's end, which
/// the ruler scanner promotes to a *phantom last column*: every data row then
/// splits into a number cell plus an adjacent symbol cell, and the window-level
/// `flowing` veto reads the near-zero gap as prose and rejects the whole table.
///
/// The merge is deliberately narrow: the symbol must be exactly one of the
/// known unit symbols, the previous token must end in a digit, and the symbol
/// must abut that token (no intervening whitespace). It can never fuse two
/// genuine columns, because a real column boundary is separated by a column
/// gutter, far wider than the zero/single-point gap tested here.
fn merge_value_symbol_tokens(words: Vec<WordTok>) -> Vec<WordTok> {
    let mut out: Vec<WordTok> = Vec::with_capacity(words.len());
    for w in words {
        if let Some(prev) = out.last_mut() {
            let prev_ends_digit = prev
                .text
                .chars()
                .last()
                .map(|c| c.is_ascii_digit())
                .unwrap_or(false);
            let abuts = (w.x0 - prev.x1).abs() <= 1.0;
            if prev_ends_digit && is_value_symbol(&w.text) && abuts {
                // French/typographic convention: a space before a currency
                // symbol ("81,90 €") but none before a percent ("10%").
                if w.text != "%" {
                    prev.text.push(' ');
                }
                prev.text.push_str(&w.text);
                prev.x1 = prev.x1.max(w.x1);
                continue;
            }
        }
        out.push(w);
    }
    out
}

/// Sequentially clusters sorted `(x, row)` pairs within `tol` of the running
/// cluster mean, keeping only clusters that span at least 2 distinct rows.
/// Shared by `table_rulers`'s two clustering passes (word starts, word ends)
/// so both use exactly the same tolerance/majority logic.
fn cluster_positions(mut points: Vec<(f64, usize)>, tol: f64) -> Vec<f64> {
    points.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

    struct Clust {
        sum_x: f64,
        count: usize,
        rows: std::collections::HashSet<usize>,
    }
    let mut clusters: Vec<Clust> = Vec::new();
    for (x, ri) in points {
        let mut matched = false;
        if let Some(last) = clusters.last_mut() {
            let mean = last.sum_x / last.count as f64;
            if (x - mean).abs() <= tol {
                last.sum_x += x;
                last.count += 1;
                last.rows.insert(ri);
                matched = true;
            }
        }
        if !matched {
            let mut rows = std::collections::HashSet::new();
            rows.insert(ri);
            clusters.push(Clust { sum_x: x, count: 1, rows });
        }
    }

    clusters
        .into_iter()
        .filter(|c| c.rows.len() >= 2)
        .map(|c| c.sum_x / c.count as f64)
        .collect()
}

/// Table column rulers in rows lo..=hi (inclusive): x positions that appear
/// in at least 2 rows within tolerance, spaced by at least min_gutter.
///
/// Two independent clustering passes feed the candidate list: word *starts*
/// (left-aligned columns — descriptions, names, dates) and word *ends*
/// (right-aligned columns — quantities, unit prices, Montant HT/TVA/TTC).
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
fn table_rulers_opts(
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
    let start_rulers = dedup(
        start_candidates
            .into_iter()
            .filter(|&x| {
                window_rows.iter().all(|&ri| {
                    if wide_ok {
                        !row_straddles_wide_ok(&info, &col_starts, ri, x, tol, min_gutter)
                    } else {
                        !row_straddles(&info[ri], x, tol, min_gutter)
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
                window_rows.iter().all(|&ri| !row_straddles(&info[ri], x, tol, min_gutter))
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
    let mut merged = start_rulers;
    for e in end_rulers {
        if merged.iter().all(|&r| (e - r).abs() >= min_gutter) {
            merged.push(e);
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
fn row_matches_ruler(row: &RowInfo, r: f64, tol: f64) -> bool {
    row.starts.iter().any(|&s| (r - s).abs() <= tol) || row.ends.iter().any(|&e| (r - e).abs() <= tol)
}

/// Whether `words` (in left-to-right order) shows any column-like internal
/// spacing of its own — i.e. at least one gap between consecutive words is at
/// least `min_gutter`. A row where every word sits within ordinary
/// word-spacing of the next reads as one flowing sentence, not separate
/// cells.
fn row_has_internal_gutter(words: &[WordTok], min_gutter: f64) -> bool {
    words.windows(2).any(|p| p[1].x0 - p[0].x1 >= min_gutter)
}

/// Whether the word of `row` ending on `x` is a *separate value cell* rather
/// than part of the row's leading (label) cell. The leading cell is the run of
/// words from the row's start up to the first column-like gutter; a right edge
/// inside that run is just the label column's ragged extent. Only a word at or
/// after the first gutter — a standalone value, or the last token of a
/// multi-word value such as "1 200,00 €" — can testify that `x` is a
/// right-aligned column edge. A row that is one contiguous cell throughout has
/// no separate value cell at all.
fn end_ruler_is_separate_cell(row: &RowInfo, x: f64, tol: f64, min_gutter: f64) -> bool {
    let first_gutter = row
        .words
        .windows(2)
        .position(|p| p[1].x0 - p[0].x1 >= min_gutter)
        .map(|i| i + 1)
        .unwrap_or(row.words.len());
    if first_gutter >= row.words.len() {
        return false;
    }
    for (i, w) in row.words.iter().enumerate() {
        if (w.x1 - x).abs() <= tol {
            if i < first_gutter {
                return false;
            }
            // The word must also *end* its cell: either it is the row's last
            // word, or the next word is separated by a column-like gutter.
            // Otherwise this right edge sits between two words of one cell
            // ("20 Unit(s)": the edge of "20" a word space before "Unit(s)"),
            // which must not become a column boundary.
            if i + 1 < row.words.len() && row.words[i + 1].x0 - w.x1 < min_gutter {
                return false;
            }
            return true;
        }
    }
    false
}

/// Whether the word of `row` starting on `x` is a *separate cell* rather than
/// an interior word of a multi-word cell. Only a word at the row's own start
/// (index 0) or one separated from the preceding word by a column-like gutter
/// can testify that `x` is a real column boundary; a word that is merely the
/// second token of one cell ("20 Unit(s)") must not become one.
fn start_ruler_is_separate_cell(row: &RowInfo, x: f64, tol: f64, min_gutter: f64) -> bool {
    for (i, w) in row.words.iter().enumerate() {
        if (w.x0 - x).abs() <= tol {
            if i == 0 {
                return true;
            }
            let prev = &row.words[i - 1];
            if w.x0 - prev.x1 >= min_gutter {
                return true;
            }
            // The token starts only an ordinary word space after the previous
            // one. Treat it as an interior word of the same cell (not a column
            // boundary) only when the two tokens form a *tight* pair — a
            // number+unit cell like "20 Unit(s)" (both tokens start within
            // ~2em). A distant label whose last word merely abuts the next
            // column is left as its own start.
            return w.x0 - prev.x0 >= 2.0 * row.size.max(0.1);
        }
    }
    false
}

/// Whether row `row` straddles ruler `x` (a word starts strictly left of it
/// and ends strictly right of it) in a way that should veto `x` as a column
/// boundary. A row with no column-like spacing of its own (see
/// `row_has_internal_gutter`) is exempt: it is typically a wrapped
/// continuation line of a multi-line description with no aligned cells of
/// its own, so it cannot testify about where the table's real columns are —
/// letting it veto rulers that every numeric row agrees on would silently
/// drop the whole table to plain text.
fn row_straddles(row: &RowInfo, x: f64, tol: f64, min_gutter: f64) -> bool {
    if !row_has_internal_gutter(&row.words, min_gutter) {
        return false;
    }
    row.words.iter().any(|w| w.x0 < x - tol && w.x1 > x + tol)
}

/// Start positions shared by at least two of `rows` (the column-start
/// candidates for a window), used to tell a genuine merged cell from a
/// fragmenting interior word-start.
fn supported_starts(info: &[RowInfo], rows: &[usize], tol: f64) -> Vec<f64> {
    let mut pts: Vec<(f64, usize)> = Vec::new();
    for &ri in rows {
        for &s in &info[ri].starts {
            pts.push((s, ri));
        }
    }
    cluster_positions(pts, tol)
}

/// Like `row_straddles`, but tolerating a *wide merged-cell* word on a
/// continuation row. A word much wider than a normal column gutter, which
/// begins at a genuine column position (present in `col_starts`, i.e. shared
/// by at least two rows) and crosses the ruler comfortably inside its extent,
/// is a merged/spanning cell whose content legitimately covers several
/// columns; it must not veto a *start*-derived column boundary, because that
/// would delete real columns. Every other crossing word — a narrow word, a
/// word on a full header/data row, or a word that only grazes the ruler near
/// its start — is still an interior word-start/end and vetoes. This only
/// matters once run advances are measured correctly: with the old 1-em
/// advances every word was ~10pt wide, so a wrapped continuation word never
/// reached the next column and this distinction was invisible.
fn row_straddles_wide_ok(
    info: &[RowInfo],
    col_starts: &[f64],
    ri: usize,
    x: f64,
    tol: f64,
    min_gutter: f64,
) -> bool {
    let row = &info[ri];
    if !row_has_internal_gutter(&row.words, min_gutter) {
        return false;
    }
    // Only a continuation row — one carrying fewer words than there are
    // column starts — can plausibly hold a merged cell that overflows several
    // columns. A full header/data row (as many words as columns) that crosses
    // a boundary is a genuine fragmentation signal and keeps vetoing.
    let row_is_continuation = row.words.len() < col_starts.len();
    row.words.iter().any(|w| {
        let crosses = w.x0 < x - tol && w.x1 > x + tol;
        if !crosses {
            return false;
        }
        let wide = (w.x1 - w.x0) > 4.0 * min_gutter;
        if !wide {
            return true;
        }
        let begins_at_column = col_starts
            .iter()
            .any(|&s| (s - w.x0).abs() <= tol * 1.5);
        // The crossed ruler must sit comfortably *inside* the word, well clear
        // of both its start and its end. A ruler only a few points from the
        // word's start is an interior near-start (fragmentation); a merged
        // cell that continues a neighbouring column has the inner rulers deep
        // in its extent.
        let well_inside = (x - w.x0) > 2.0 * min_gutter && (w.x1 - x) > 2.0 * min_gutter;
        !(row_is_continuation && begins_at_column && well_inside)
    })
}

/// Diagnostic tracer: prints when TABLE_TRACE env is set.
fn t(msg: &str) {
    if std::env::var("TABLE_TRACE").is_ok() {
        eprintln!("[tbl] {}", msg);
    }
}

/// Reconstruct a line's text (for diagnostics).
fn line_text(line: &[Span]) -> String {
    line.iter().map(|s| s.text.as_str()).collect()
}

/// Detect grid tables on a page's visual lines.
///
/// The page is split into column bands first (see `scan_aligned_grids_banded`):
/// a side table sharing baselines with a neighboring column's prose is scanned
/// in isolation instead of against that prose.
pub fn scan_aligned_grids(lines: &[Vec<Span>], tol_mult: f64, covered: &[TableHit]) -> Vec<TableHit> {
    // The public/lossy passes (including the stage-3b gap recovery) keep the
    // original strict merged-cell veto; only `find_tables`'s explicit
    // refinement pass opts into the relaxed one.
    scan_aligned_grids_banded(lines, tol_mult, covered, false)
}

/// [`scan_aligned_grids`] with the merged-cell `wide_ok` veto setting threaded
/// through to `table_rulers_opts`.
fn scan_aligned_grids_opts(
    lines: &[Vec<Span>],
    tol_mult: f64,
    covered: &[TableHit],
    wide_ok: bool,
) -> Vec<TableHit> {
    // A grid needs at least a header row and one data row (2 lines). A single
    // visual line can never hold a table, so reject below 2.
    if lines.len() < 2 {
        return Vec::new();
    }

    let info: Vec<RowInfo> = lines
        .iter()
        .map(|l| {
            let words = line_words(l);
            let mut starts: Vec<f64> = words.iter().map(|w| w.x0).collect();
            starts.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let mut ends: Vec<f64> = words.iter().map(|w| w.x1).collect();
            ends.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            RowInfo {
                words,
                starts,
                ends,
                size: l[0].size.max(0.1),
            }
        })
        .collect();

    let tol = tol_mult
        * info
            .iter()
            .map(|r| (0.06 * r.size).clamp(0.5, 1.2))
            .fold(0.0f64, f64::max);

    // Vertical bands of consecutive rows that could sit in one grid (tight line pitch,
    // no huge paragraph gap inside). Rows already claimed by a previous pass are excluded.
    let mut bands: Vec<Vec<usize>> = Vec::new();
    for (i, r) in info.iter().enumerate() {
        if r.words.is_empty() {
            continue;
        }
        if covered.iter().any(|h| h.start <= i && i <= h.end) {
            continue;
        }
        match bands.last_mut() {
            Some(band) => {
                let prev = *band.last().unwrap();
                let gap = lines[prev][0].y - lines[i][0].y;
                let scale = info[prev].size.max(r.size);
                let line_pitch = 1.2 * scale;
                // Never bridge a region another pass already claimed. A band
                // that spans a previous hit's rows keeps only the rows around
                // it, yet the resulting `TableHit` covers the whole contiguous
                // line range — so the covered rows are neither in the hit's
                // `rows` nor rendered as text. Split the band instead, leaving
                // the previous hit in place.
                let bridges_covered = (prev + 1..i)
                    .any(|k| covered.iter().any(|h| h.start <= k && k <= h.end));
                if !bridges_covered && gap >= 0.0 && (gap <= 3.8 * line_pitch || gap <= 38.0) {
                    band.push(i);
                } else {
                    bands.push(vec![i]);
                }
            }
            None => bands.push(vec![i]),
        }
    }

    let mut hits: Vec<TableHit> = Vec::new();
    for band in bands {
        if band.len() < 2 {
            continue;
        }
        t(&format!(
            "BAND [{}-{}] rows={}: {}",
            band[0],
            band[band.len() - 1],
            band.len(),
            band.iter()
                .map(|&i| format!("[{}:{}]", i, line_text(&lines[i])))
                .collect::<Vec<_>>()
                .join(" | ")
        ));
        // A genuine two-row seed doesn't have to be two *adjacent* lines: a
        // multi-line item description commonly wraps onto its own
        // continuation row(s) between one numeric data row and the next
        // (e.g. an invoice line item followed by "Période de 01/12/2018 au
        // 31/12/2018" before the next item), and that continuation row
        // shares no column position with either data row at all — it isn't
        // pruned by `row_straddles`, it simply contributes nothing to
        // `cluster_positions`. Trying only `hi = lo + 1` therefore never
        // finds a seed once real data rows are separated by such filler, and
        // the whole table silently falls back to plain text. Search forward
        // from `lo` for the nearest row that *does* share >= 2 rulers with
        // it, skipping over non-participating rows in between (capped, so a
        // page with no real table nearby doesn't pay for an unbounded scan).
        const MAX_SEED_LOOKAHEAD: usize = 12;
        let mut lo = 0usize;
        while lo < band.len() {
            if lo + 1 >= band.len() {
                break;
            }
            let hi_limit = band.len().min(lo + 1 + MAX_SEED_LOOKAHEAD);
            let mut found: Option<(usize, Vec<f64>, usize, usize)> = None;
            for hi in (lo + 1)..hi_limit {
                // Only skip PAST a row that has no column-like spacing of its
                // own (a filler/continuation line — see `row_has_internal_gutter`).
                // A row that DOES look like its own data row (multiple
                // cell-like gaps) but merely missed strict tolerance (e.g. a
                // few tenths of a point of jitter) must not be leapfrogged:
                // that's exactly the case the separate, looser stage-3b pass
                // (`find_gap_tables`) exists to recover on its own tolerance,
                // and skipping past it here would let the strict pass
                // absorb jittered grids it is deliberately too tight to see.
                if hi > lo + 1 {
                    let mid = hi - 1;
                    let mid_size = info[band[mid]].size;
                    let mid_gutter = (1.1 * mid_size).max(6.0);
                    if row_has_internal_gutter(&info[band[mid]].words, mid_gutter) {
                        break;
                    }
                }
                let max_size = [band[lo], band[hi]]
                    .iter()
                    .map(|&i| info[i].size)
                    .fold(0.0f64, f64::max);
                let min_gutter = (1.1 * max_size).max(6.0);
                let rulers = table_rulers_opts(&info, tol, band[lo], band[hi], min_gutter, wide_ok);
                let match_lo = rulers.iter().filter(|&&r| row_matches_ruler(&info[band[lo]], r, tol * 1.5)).count();
                let match_hi = rulers.iter().filter(|&&r| row_matches_ruler(&info[band[hi]], r, tol * 1.5)).count();
                if rulers.len() >= 2 && match_lo >= 2 && match_hi >= 2 {
                    found = Some((hi, rulers, match_lo, match_hi));
                    break;
                }
            }
            let (mut hi, mut rulers, match_lo, match_hi) = match found {
                Some(f) => f,
                None => {
                    t(&format!(
                        "  REJECT at lo={} (line {} '{}'): no seed hi within lookahead [seed check]",
                        lo, band[lo], line_text(&lines[band[lo]])
                    ));
                    lo += 1;
                    continue;
                }
            };
            t(&format!(
                "  SEED lo={} (line {} '{}') hi={} (line {} '{}'): rulers={:?} match_lo={} match_hi={}",
                lo, band[lo], line_text(&lines[band[lo]]),
                hi, band[hi], line_text(&lines[band[hi]]),
                rulers, match_lo, match_hi
            ));
            let max_size = [band[lo], band[hi]]
                .iter()
                .map(|&i| info[i].size)
                .fold(0.0f64, f64::max);
            let min_gutter = (1.1 * max_size).max(6.0);
            // Whether this seed is a genuinely multi-column grid (3+ rulers).
            // A 2-column label/value block has no separate right-aligned
            // amount column for a totals block to share, so it keeps the
            // original (more permissive) growth rule.
            let seed_is_multi_col = rulers.len() >= 3;
            while hi + 1 < band.len() && rulers.len() >= 2 {
                let next_ri = band[hi + 1];
                // Grow only when the candidate row populates at least two
                // columns the table has *already* established. Judging the row
                // against the ruler set recomputed after adding it (the old
                // `next_match`) is circular: the row contributes its own new
                // rulers and then trivially "matches" them. An adjacent
                // totals/VAT/Règlement block under a multi-column line-item
                // grid shares only the right-aligned amount edge and would
                // otherwise be annexed wholesale. The membership test uses the
                // table's own font scale, not the pass-wide `tol` (inflated by
                // the largest title on the page). A genuine two-column
                // label/value grid has no amount column to be shared with a
                // totals block, so it keeps the original growth rule.
                let member_size = info[band[lo]].size.max(info[next_ri].size).max(0.1);
                let member_tol =
                    (1.5 * tol_mult * (0.06 * member_size).clamp(0.5, 1.2)).min(tol * 1.5);
                let established_match = rulers
                    .iter()
                    .filter(|&&r| row_matches_ruler(&info[next_ri], r, member_tol))
                    .count();
                let next_r = table_rulers_opts(&info, tol, band[lo], next_ri, min_gutter, wide_ok);
                let next_match = next_r.iter().filter(|&&r| row_matches_ruler(&info[next_ri], r, tol * 1.5)).count();
                // The membership rule only kicks in for a genuinely
                // multi-column seed AND a candidate set in a *smaller* font
                // than the table's own anchor row: the signature of a
                // totals/VAT summary appended below a product grid. A table
                // whose rows keep the same size (or grow into a larger
                // header) grows exactly as before, so header/invoice blocks
                // and ordinary continuations are untouched.
                let candidate_smaller_font = info[next_ri].size < info[band[lo]].size;
                let grows = next_r.len() >= 2
                    && if seed_is_multi_col && candidate_smaller_font {
                        established_match >= 2
                    } else {
                        next_match >= 2
                    };
                if grows {
                    hi += 1;
                    rulers = next_r;
                    continue;
                }
                // Single-column continuation row (e.g. wrapped airport name in a multi-line cell)
                if next_match == 1 {
                    // Deliberately the strict (non-exempted) straddle check
                    // here, unlike `table_rulers`'s own filter and the final
                    // window gate below: this decides whether to keep
                    // GROWING the table past its current end, and the
                    // prose-only exemption exists to stop a *filler row
                    // inside an already-bounded window* from vetoing the
                    // table's rulers — not to let arbitrary trailing prose
                    // (e.g. a legal disclaimer paragraph after the table)
                    // get annexed as one more "continuation" row just
                    // because it has no internal gutter of its own.
                    let straddles = rulers[1..]
                        .iter()
                        .any(|&r| {
                            info[next_ri]
                                .words
                                .iter()
                                .any(|w| w.x0 < r - tol && w.x1 > r + tol)
                        });
                    if !straddles {
                        let has_future_match = (hi + 2..band.len().min(hi + 4)).any(|fut_idx| {
                            let fut_ri = band[fut_idx];
                            rulers.iter().filter(|&&r| row_matches_ruler(&info[fut_ri], r, tol * 1.5)).count() >= 2
                        });
                        // A wrapped continuation of the *final* data row's
                        // cell has no future row to vouch for it — the table
                        // simply ends below. `has_future_match` alone therefore
                        // always drops it, splitting the cell value in two: the
                        // tail is emitted as a stray paragraph after the table.
                        // Annex it when it is a short, single-cell line that
                        // continues a *non-first* column the previous row
                        // actually populated, within the table's own line
                        // pitch. The first column is the row anchor (a lone
                        // first-column line below the table is new content, not
                        // an overflow of the cell above), and a trailing
                        // paragraph sits further down and/or spans a column
                        // gutter, so it is still left out.
                        let continues_populated_value_cell = {
                            let single_cell =
                                !row_has_internal_gutter(&info[next_ri].words, min_gutter);
                            let gap = lines[band[hi]][0].y - lines[next_ri][0].y;
                            let pitch = 2.2 * info[band[hi]].size.max(info[next_ri].size).max(1.0);
                            let within_pitch = gap > 0.0 && gap <= pitch;
                            let continues_nonfirst = rulers.iter().enumerate().any(|(i, &r)| {
                                i > 0
                                    && row_matches_ruler(&info[next_ri], r, tol * 1.5)
                                    && row_matches_ruler(&info[band[hi]], r, tol * 1.5)
                            });
                            single_cell && within_pitch && continues_nonfirst
                        };
                        if has_future_match || continues_populated_value_cell {
                            hi += 1;
                            continue;
                        }
                    }
                }
                break;
            }
            if rulers.len() < 2 {
                t(&format!(
                    "  REJECT at lo={} (line {} '{}'): after-hi rulers={:?} [rulers<2]",
                    lo, band[lo], line_text(&lines[band[lo]]), rulers
                ));
                lo += 1;
                continue;
            }
            let win_rows: Vec<usize> = band[lo..=hi].to_vec();
            t(&format!(
                "  WINDOW lo={} (line {}, '{}') hi={} (line {}, '{}') rulers={:?}",
                lo, band[lo], line_text(&lines[band[lo]]),
                hi, band[hi], line_text(&lines[band[hi]]),
                rulers
            ));

            // No indivisible word can straddle an interior column ruler,
            // except in a prose-only continuation row (see `row_straddles`)
            // or when the crossing word is a wide merged/spanning cell
            // (`row_straddles_wide_ok`).
            let win_starts: Vec<f64> = supported_starts(&info, &win_rows, tol);
            let has_straddling_word = win_rows.iter().any(|&ri| {
                rulers[1..].iter().any(|&r| {
                    if wide_ok {
                        row_straddles_wide_ok(&info, &win_starts, ri, r, tol, min_gutter)
                    } else {
                        row_straddles(&info[ri], r, tol, min_gutter)
                    }
                })
            });
            if has_straddling_word {
                t(&format!(
                    "  REJECT window [{}-{}]: straddling word [straddle]",
                    band[lo], band[hi]
                ));
                lo += 1;
                continue;
            }

            let multi_col_rows = win_rows
                .iter()
                .filter(|&&i| {
                    let row_cells = bucket(&info, i, &rulers);
                    row_cells.iter().filter(|c| !c.trim().is_empty()).count() >= 2
                })
                .count();
            if multi_col_rows < 2 {
                t(&format!(
                    "  REJECT window [{}-{}]: multi_col_rows={} [multi_col<2]",
                    band[lo], band[hi], multi_col_rows
                ));
                lo += 1;
                continue;
            }
            if hi - lo + 1 >= 2 && rulers.len() >= 2 {
                let max_size = win_rows
                    .iter()
                    .map(|&i| info[i].size)
                    .fold(0.0f64, f64::max);
                let min_gutter = (1.1 * max_size).max(6.0);
                let ok_gutter = rulers.windows(2).all(|p| p[1] - p[0] >= min_gutter);
                if !ok_gutter {
                    t(&format!(
                        "  REJECT window [{}-{}]: min_gutter={} rulers={:?} [gutter]",
                        band[lo], band[hi], min_gutter, rulers
                    ));
                }
                if ok_gutter {
                    let mid1 = (rulers[0] + rulers[1]) / 2.0;
                    let mut distinct: std::collections::HashSet<String> =
                        std::collections::HashSet::new();
                    for &i in &win_rows {
                        let first: String = info[i]
                            .words
                            .iter()
                            .filter(|w| w.x0 < mid1)
                            .map(|w| w.text.as_str())
                            .collect::<Vec<_>>()
                            .join(" ");
                        if !first.trim().is_empty() {
                            distinct.insert(first);
                        }
                    }
                    if distinct.len() < 2 {
                        t(&format!(
                            "  REJECT window [{}-{}]: distinct_first_cols={} [no 2 distinct first cols]",
                            band[lo], band[hi], distinct.len()
                        ));
                    }
                    if distinct.len() >= 2 {
                        let flowing_rows = win_rows
                            .iter()
                            .filter(|&&ri| {
                                let cells = bucket_words(&info, ri, &rulers);
                                let size = info[ri].size;
                                for c in 0..cells.len().saturating_sub(1) {
                                    if !cells[c].is_empty() && !cells[c + 1].is_empty() {
                                        let last_w = cells[c].last().unwrap();
                                        let first_next_w = cells[c + 1].first().unwrap();
                                        let gap = first_next_w.x0 - last_w.x1;
                                        if gap < 0.65 * size {
                                            return true;
                                        }
                                    }
                                }
                                false
                            })
                            .count();
                        if flowing_rows >= 2 && flowing_rows * 2 >= multi_col_rows {
                            t(&format!(
                                "  REJECT window [{}-{}]: flowing_rows={} multi_col_rows={} [flowing]",
                                band[lo], band[hi], flowing_rows, multi_col_rows
                            ));
                            lo += 1;
                            continue;
                        }

                        let table_rows: Vec<Vec<String>> =
                            bucket_rows_content_aware(&info, &win_rows, &rulers, tol);
                        let (table_rows, rulers) =
                            merge_complementary_columns(table_rows, &win_rows, &info, &rulers, tol);
                        // Drop fully-empty edge columns.
                        let ncol = rulers.len();
                        let mut c0 = 0usize;
                        let mut c1 = ncol;
                        while c0 < c1 && table_rows.iter().all(|r| r[c0].trim().is_empty()) {
                            c0 += 1;
                        }
                        while c1 > c0 && table_rows.iter().all(|r| r[c1 - 1].trim().is_empty()) {
                            c1 -= 1;
                        }
                        if c1 - c0 >= 2 {
                            let rows2: Vec<Vec<String>> =
                                table_rows.iter().map(|r| r[c0..c1].to_vec()).collect();
                            if !is_tabular_rows(&rows2) {
                                t(&format!(
                                    "  REJECT window [{}-{}]: is_tabular_rows false rows2={:?} [not_tabular]",
                                    band[lo], band[hi], rows2
                                ));
                                lo += 1;
                                continue;
                            }
                            let consolidated_rows =
                                consolidate_table_rows(rows2, &win_rows, lines, &info);
                            let mut min_x = f64::INFINITY;
                            let mut max_x = f64::NEG_INFINITY;
                            let mut min_y = f64::INFINITY;
                            let mut max_y = f64::NEG_INFINITY;
                            for &i in &win_rows {
                                for sp in &lines[i] {
                                    min_x = min_x.min(sp.x);
                                    max_x = max_x.max(sp.x + sp.advance);
                                    min_y = min_y.min(sp.y);
                                    max_y = max_y.max(sp.y);
                                }
                            }
                            hits.push(TableHit {
                                start: win_rows[0],
                                end: *win_rows.last().unwrap(),
                                rows: consolidated_rows,
                                bbox: BoundingBox::new(min_x, min_y, max_x, max_y),
                            });
                            lo = hi + 1;
                            continue;
                        }
                    }
                }
            }
            lo += 1;
        }
    }
    hits
}

/// Map each line of an isolated column stream back to its index in the page's
/// own `lines`, by baseline-y containment. A merged visual line holds spans
/// from both columns whose baselines can differ by a few points, so the stream
/// line's y is matched against the *range* of y values an original line spans
/// rather than an exact y key.
fn stream_index_map(stream: &[Vec<Span>], ranges: &[(f64, f64, usize)]) -> Vec<usize> {
    stream
        .iter()
        .map(|l| {
            let q = l.iter().map(|s| s.y).fold(f64::INFINITY, f64::min);
            ranges
                .iter()
                .filter(|&&(lo, hi, _)| q >= lo - 0.75 && q <= hi + 0.75)
                .min_by(|a, b| {
                    let da = (q - 0.5 * (a.0 + a.1)).abs();
                    let db = (q - 0.5 * (b.0 + b.1)).abs();
                    da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
                })
                .map(|&(_, _, i)| i)
                .unwrap_or(0)
        })
        .collect()
}

/// [`scan_aligned_grids_opts`] with the page split into column bands first, so a
/// side table's rows are never compared against a neighboring column's prose.
///
/// A page can interleave two independent regions on one visual line: a narrow
/// parameter/score table set in the right margin at the same baseline as the
/// main column's prose. Scanning the merged lines lets the seed-and-grow window
/// annex prose rows, which then trips the `flowing` veto and drops the whole
/// table to plain text. `detect_column_bands` already separates those regions,
/// so each column stream is scanned in isolation and every hit is mapped back to
/// the page's own line indices (the ones `render_with_tables` splices against).
///
/// A page with no genuine column band keeps the original whole-page scan, so
/// single-column table detection stays byte-identical.
fn scan_aligned_grids_banded(
    lines: &[Vec<Span>],
    tol_mult: f64,
    covered: &[TableHit],
    wide_ok: bool,
) -> Vec<TableHit> {
    if lines.len() < 2 {
        return Vec::new();
    }
    let bands = detect_column_bands(lines);
    if bands.iter().all(|b| matches!(b, ColumnBand::Full(_))) {
        return scan_aligned_grids_opts(lines, tol_mult, covered, wide_ok);
    }

    // Baseline-y ranges of each page line, for mapping a cloned stream line
    // back to the page line it came from.
    let mut ranges: Vec<(f64, f64, usize)> = Vec::with_capacity(lines.len());
    for (i, l) in lines.iter().enumerate() {
        if l.is_empty() {
            continue;
        }
        let lo = l.iter().map(|s| s.y).fold(f64::INFINITY, f64::min);
        let hi = l.iter().map(|s| s.y).fold(f64::NEG_INFINITY, f64::max);
        ranges.push((lo, hi, i));
    }

    // `scan_aligned_grids_opts` only reads `start`/`end` from `covered`; empty
    // rows and a zero bbox are enough to translate the page-level covered
    // ranges into a stream's local indices.
    let dummy = |start: usize, end: usize| TableHit {
        start,
        end,
        rows: Vec::new(),
        bbox: BoundingBox::new(0.0, 0.0, 0.0, 0.0),
    };

    let mut hits: Vec<TableHit> = Vec::new();
    for band in bands {
        let streams: Vec<Vec<Vec<Span>>> = match band {
            ColumnBand::Full(rows) => vec![rows],
            ColumnBand::Columns { left, right } => vec![left, right],
        };
        for stream in streams {
            if stream.len() < 2 {
                continue;
            }
            let index_map = stream_index_map(&stream, &ranges);
            let mut local_covered: Vec<TableHit> = Vec::new();
            let mut run: Option<usize> = None;
            for (local, &orig) in index_map.iter().enumerate() {
                let cov = covered.iter().any(|h| h.start <= orig && orig <= h.end);
                match (run, cov) {
                    (None, true) => run = Some(local),
                    (Some(s), false) => {
                        local_covered.push(dummy(s, local - 1));
                        run = None;
                    }
                    _ => {}
                }
            }
            if let Some(s) = run {
                local_covered.push(dummy(s, index_map.len() - 1));
            }

            for sh in scan_aligned_grids_opts(&stream, tol_mult, &local_covered, wide_ok) {
                let start = index_map[sh.start];
                let end = index_map[sh.end];
                if start <= end {
                    hits.push(TableHit {
                        start,
                        end,
                        rows: sh.rows,
                        bbox: sh.bbox,
                    });
                }
            }
        }
    }
    hits
}

/// Stage-3 table recovery: strict ruler alignment (the default pass).
pub fn find_tables(lines: &[Vec<Span>]) -> Vec<TableHit> {
    // The strict geometry is the primary detector. The relaxed pass (which
    // tolerates a wide merged/spanning cell crossing a genuine column
    // boundary — needed for dense bilingual grids whose continuation rows
    // overflow several columns) may only *refine* a table the strict pass
    // already found: if it overlaps a strict hit and resolves more columns,
    // its finer segmentation replaces that hit. It never invents a table the
    // strict geometry does not see, so it cannot turn ordinary address/prose
    // blocks into spurious grids.
    let mut hits = scan_aligned_grids_banded(lines, 1.0, &[], false);
    let wide_hits = scan_aligned_grids_banded(lines, 1.0, &[], true);
    for wh in wide_hits {
        let Some(pos) = hits
            .iter()
            .position(|h| h.start <= wh.end && wh.start <= h.end)
        else {
            continue;
        };
        let strict_cols = hits[pos].rows.first().map(|r| r.len()).unwrap_or(0);
        let wide_cols = wh.rows.first().map(|r| r.len()).unwrap_or(0);
        // Only refine a table that is already genuinely tabular. A two-column
        // strict hit is usually a clean label/value block; letting the relaxed
        // pass absorb surrounding address/prose lines into it produces a worse
        // grid, so small tables keep their strict segmentation.
        if wide_cols > strict_cols && strict_cols >= 3 {
            hits[pos] = wh;
        }
    }
    hits
}

/// Stage-3b recovery pass: the same grid scan with a wider alignment
/// tolerance, over rows the strict pass did not claim (jittered tables).
pub fn find_gap_tables(lines: &[Vec<Span>], covered: &[TableHit]) -> Vec<TableHit> {
    scan_aligned_grids_banded(lines, 2.0, covered, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sp(text: &str, x: f64, advance: f64) -> Span {
        Span {
            text: text.to_string(),
            x,
            y: 100.0,
            size: 10.0,
            advance,
            is_bold: false,
            is_italic: false,
            is_underline: false,
            is_vertical: false,
        }
    }

    /// A single word drawn as several `TJ` runs (kerned glyph chunks) must stay
    /// one token. `line_words` used to compare the *raw* start-to-start span
    /// distance against the 2.5em column threshold, so a multi-run word whose
    /// runs are cumulatively wider than 2.5em fragmented on a phantom gutter:
    /// the real invoice fixture's "Number" (`N`+`umbe`+`r`) became
    /// `["Numbe", "r"]` because `umbe` alone is ~4em wide, and the table
    /// bucketer then dropped the stray `r` into the next column.
    #[test]
    fn line_words_keeps_multispan_word_across_wide_runs() {
        let line = vec![sp("N", 0.0, 7.0), sp("umbe", 7.0, 40.0), sp("r", 47.0, 4.0)];
        let words: Vec<String> = line_words(&line).into_iter().map(|w| w.text).collect();
        assert_eq!(words, vec!["Number"], "multi-span word was fragmented: {words:?}");
    }

    /// A genuine column gutter — real whitespace wider than 2.5em *after* the
    /// previous run's own advance — must still split into separate tokens.
    #[test]
    fn line_words_still_splits_on_real_gutter() {
        let line = vec![sp("left", 0.0, 18.0), sp("right", 60.0, 25.0)];
        let words: Vec<String> = line_words(&line).into_iter().map(|w| w.text).collect();
        assert_eq!(words, vec!["left", "right"], "real gutter was not split: {words:?}");
    }

    fn sp_at(text: &str, x: f64, y: f64, advance: f64) -> Span {
        Span {
            text: text.to_string(),
            x,
            y,
            size: 10.0,
            advance,
            is_bold: false,
            is_italic: false,
            is_underline: false,
            is_vertical: false,
        }
    }

    /// Two left-aligned label cells in a totals column ("Gesamtbetrag der
    /// Zuschläge" / "…Abschläge") have the same rendered width, so their right
    /// edges fall on the same x. The end-ruler pass used to promote that shared
    /// text edge to a column boundary because neither row *straddled* it (the
    /// words end exactly on it). That split the label column, stranded the unit
    /// "EUR" in a phantom middle column, and made `consolidate_table_rows`
    /// fold four distinct totals rows into a single `<br>`-joined row. The end
    /// pass must only accept a shared right edge when the word ending there is
    /// a separate value cell, not the tail of a leading label.
    #[test]
    fn shared_label_right_edge_does_not_invent_a_column() {
        let label = |t: &str, y: f64, adv: f64| sp_at(t, 287.56, y, adv);
        let amount = |t: &str, x: f64, y: f64, adv: f64| sp_at(t, x, y, adv);
        let lines: Vec<Vec<Span>> = vec![
            vec![label("Positionssumme", 405.20, 83.98), amount("473,00", 508.03, 405.20, 36.00)],
            vec![label("Gesamtbetrag der Zuschläge", 392.38, 155.96), amount("0,00", 520.03, 392.38, 24.00)],
            vec![label("Gesamtbetrag der Abschläge", 379.56, 155.96), amount("-0,00", 514.03, 379.56, 30.00)],
            vec![label("Rechnungssumme ohne USt.", 366.74, 143.96), amount("473,00", 508.03, 366.74, 36.00)],
            vec![
                label("Steuerbetrag in", 353.92, 95.97),
                amount("EUR", 454.05, 353.92, 24.00),
                amount("56,87", 508.03, 353.92, 36.00),
            ],
            vec![label("Bruttosumme", 341.10, 65.98), amount("529,87", 508.03, 341.10, 36.00)],
            vec![label("Erhaltene Anzahlungen", 326.03, 125.96), amount("-0,00", 514.03, 326.03, 30.00)],
            vec![label("Zahlbetrag", 313.21, 59.98), amount("529,87", 508.03, 313.21, 36.00)],
        ];
        let hits = find_tables(&lines);
        let hit = hits
            .iter()
            .find(|h| h.rows.iter().any(|r| r.iter().any(|c| c.contains("Positionssumme"))))
            .expect("totals table was not detected");
        assert_eq!(
            hit.rows.iter().map(|r| r.len()).max(),
            Some(2),
            "phantom middle column appeared: {:?}",
            hit.rows
        );
        assert_eq!(hit.rows.len(), 8, "distinct totals rows were merged: {:?}", hit.rows);
        assert_eq!(hit.rows[5][0], "Bruttosumme", "row 5 folded into row 4: {:?}", hit.rows);
        assert!(
            hit.rows.iter().all(|r| !r.iter().any(|c| c.contains("<br>"))),
            "rows were folded together: {:?}",
            hit.rows
        );
    }

    /// The very same totals block, but asserting the *cell contents* the
    /// previous test left unchecked: the unit "EUR" of the label "Steuerbetrag
    /// in EUR" renders at x=454, beyond the midpoint (398) between the label
    /// ruler (288) and the amount ruler (508), so the plain midpoint bucketer
    /// filed it into the amount column and produced "EUR 56,87". The unit is
    /// part of the label cell and must stay there. Fails before the
    /// content-aware bucketing fix, passes after.
    #[test]
    fn stranded_unit_word_stays_in_its_label_cell() {
        let label = |t: &str, y: f64, adv: f64| sp_at(t, 287.56, y, adv);
        let amount = |t: &str, x: f64, y: f64, adv: f64| sp_at(t, x, y, adv);
        let lines: Vec<Vec<Span>> = vec![
            vec![label("Positionssumme", 405.20, 83.98), amount("473,00", 508.03, 405.20, 36.00)],
            vec![label("Gesamtbetrag der Zuschläge", 392.38, 155.96), amount("0,00", 520.03, 392.38, 24.00)],
            vec![label("Gesamtbetrag der Abschläge", 379.56, 155.96), amount("-0,00", 514.03, 379.56, 30.00)],
            vec![label("Rechnungssumme ohne USt.", 366.74, 143.96), amount("473,00", 508.03, 366.74, 36.00)],
            vec![
                label("Steuerbetrag in", 353.92, 95.97),
                amount("EUR", 454.05, 353.92, 24.00),
                amount("56,87", 508.03, 353.92, 36.00),
            ],
            vec![label("Bruttosumme", 341.10, 65.98), amount("529,87", 508.03, 341.10, 36.00)],
            vec![label("Erhaltene Anzahlungen", 326.03, 125.96), amount("-0,00", 514.03, 326.03, 30.00)],
            vec![label("Zahlbetrag", 313.21, 59.98), amount("529,87", 508.03, 313.21, 36.00)],
        ];
        let hits = find_tables(&lines);
        let hit = hits
            .iter()
            .find(|h| h.rows.iter().any(|r| r.iter().any(|c| c.contains("Positionssumme"))))
            .expect("totals table was not detected");
        let row = hit
            .rows
            .iter()
            .find(|r| r.iter().any(|c| c.contains("Steuerbetrag")))
            .expect("Steuerbetrag row missing");
        assert_eq!(
            row[0], "Steuerbetrag in EUR",
            "label's unit was stranded in the amount column: {:?}",
            hit.rows
        );
        assert_eq!(
            row[1], "56,87",
            "amount cell absorbed the label's unit: {:?}",
            hit.rows
        );
    }

    /// The line-item grid of `fnfe_Facture_FR_BASIC.pdf`: the quantity is drawn
    /// as "20 Unit(s)" with only an ordinary word space between the number and
    /// the unit. The unit's word start (and the number's right edge) used to be
    /// promoted to a column boundary, splitting one quantity cell into two
    /// adjacent cells separated by a word space. The window-level `flowing`
    /// veto then read that split as prose and dropped the *entire* invoice
    /// table to plain text. Neither the number/unit start nor the number's end
    /// may become a boundary. Fails before the tight-pair ruler fix, passes
    /// after.
    #[test]
    fn quantity_number_and_unit_are_one_cell() {
        let cell = |t: &str, x: f64, y: f64, adv: f64| sp_at(t, x, y, adv);
        let lines: Vec<Vec<Span>> = vec![
            vec![
                cell("Nougat de l'Abbaye 250g", 34.0, 486.0, 100.0),
                cell("20", 358.0, 486.0, 8.0),
                cell("Unit(s)", 370.0, 486.0, 22.0),
                cell("4,55 €", 432.0, 486.0, 25.0),
                cell("10%", 476.0, 486.0, 18.0),
                cell("81,90 €", 532.0, 486.0, 25.0),
            ],
            vec![
                cell("Biscuits aux raisins 300g", 34.0, 468.0, 100.0),
                cell("15", 358.0, 468.0, 8.0),
                cell("Unit(s)", 370.0, 468.0, 22.0),
                cell("3,20 €", 432.0, 468.0, 25.0),
                cell("48,00 €", 532.0, 468.0, 25.0),
            ],
            vec![
                cell("Huile d'olive à l'ancienne", 34.0, 450.0, 100.0),
                cell("25", 358.0, 450.0, 8.0),
                cell("Liter(s)", 370.0, 450.0, 22.0),
                cell("19,80 €", 427.0, 450.0, 30.0),
                cell("495,00 €", 527.0, 450.0, 30.0),
            ],
        ];
        let hits = find_tables(&lines);
        let cells: Vec<&str> = hits
            .iter()
            .flat_map(|h| h.rows.iter().flatten())
            .map(|c| c.as_str())
            .collect();
        assert!(
            cells.iter().any(|c| c.contains("20 Unit(s)")),
            "quantity number and unit must stay in one cell, got {cells:?}"
        );
        assert!(
            !cells.iter().any(|c| c.trim() == "20"),
            "the number must not become its own phantom column, got {cells:?}"
        );
    }

    /// A 2-column label/value grid whose *last* value wraps onto one more
    /// visual line ("RENDU DROITS NON" then "ACQUITTÉS" below it). Because the
    /// wrapped tail is the final line of the table, there is no future row for
    /// the single-match continuation branch to point at, so `has_future_match`
    /// was always false and the growth loop broke before the tail: the cell
    /// value was split in two and the tail was emitted as a stray paragraph
    /// after the table (vision against the FNFE "Facture DOM" invoice renders
    /// the cell as "RENDU DROITS NON ACQUITTÉS"). The tail must be annexed
    /// into the same row's value cell.
    #[test]
    fn wrapped_final_value_cell_is_annexed_into_its_last_row() {
        let cell = |t: &str, x: f64, y: f64, adv: f64| sp_at(t, x, y, adv);
        let lines: Vec<Vec<Span>> = vec![
            vec![cell("Votre référence", 34.1, 560.0, 70.0), cell("BC543", 154.6, 560.0, 28.0)],
            vec![cell("Réf. marché", 34.1, 543.0, 55.0), cell("WELCOME_PACK_2017", 154.6, 543.0, 105.0)],
            vec![cell("N° TVA client", 34.1, 526.0, 60.0), cell("FR90343434346", 154.6, 526.0, 78.0)],
            vec![cell("Incoterms", 34.1, 509.0, 45.0), cell("RENDU DROITS NON", 154.6, 509.0, 92.0)],
            // Wrapped tail of the Incoterms value: single cell, one row pitch down.
            vec![cell("ACQUITTÉS", 154.6, 497.0, 44.0)],
        ];
        let hits = find_tables(&lines);
        let hit = hits
            .iter()
            .find(|h| h.rows.iter().any(|r| r.iter().any(|c| c.contains("Incoterms"))))
            .expect("label/value grid was not detected");
        let incoterms = hit
            .rows
            .iter()
            .find(|r| r.iter().any(|c| c.contains("Incoterms")))
            .expect("Incoterms row missing");
        assert!(
            incoterms.iter().any(|c| c.contains("ACQUITTÉS")),
            "wrapped tail of the final value cell was dropped from the table: {:?}",
            hit.rows
        );
    }

    fn sz(text: &str, x: f64, y: f64, adv: f64, size: f64) -> Span {
        Span {
            text: text.to_string(),
            x,
            y,
            size,
            advance: adv,
            is_bold: false,
            is_italic: false,
            is_underline: false,
            is_vertical: false,
        }
    }

    /// A value drawn as two runs — the number, an explicit space span, then the
    /// currency symbol — exactly as the FNFE invoice generators emit it. Before
    /// `merge_value_symbol_tokens` this became two tokens: the number, and a
    /// `"€"` whose x0 sits exactly on the number's right edge.
    fn split_amount(num: &str, x: f64, num_adv: f64, y: f64, size: f64) -> Vec<Span> {
        vec![
            sz(num, x, y, num_adv, size),
            sz(" ", x + num_adv, y, 1.0, size),
            sz("€", x + num_adv + 1.0, y, 6.5, size),
        ]
    }

    /// The exact regression shape from the prior attempt: a line
    /// `["81,90" @ x, " " @ x+advance, "€" @ x+advance+space]` must tokenize to
    /// the single word `"81,90 €"`. Left split, the symbol becomes a phantom
    /// last column whose near-zero gap to the number reads as prose and makes
    /// the window-level `flowing` veto reject the whole table.
    #[test]
    fn trailing_currency_symbol_is_folded_into_its_number() {
        let line = vec![sp("81,90", 100.0, 28.0), sp(" ", 128.0, 2.5), sp("€", 130.5, 9.0)];
        let words = line_words(&line);
        assert_eq!(
            words.len(),
            1,
            "a trailing currency symbol must not become its own token: {words:?}"
        );
        assert_eq!(words[0].text, "81,90 €");
        assert!((words[0].x0 - 100.0).abs() < 1e-6, "x0 moved: {}", words[0].x0);
        assert!((words[0].x1 - 139.5).abs() < 1e-6, "x1 wrong: {}", words[0].x1);
    }

    /// A bare percent unit is folded the same way ("10" + "%" → "10%").
    #[test]
    fn trailing_percent_symbol_is_folded_into_its_number() {
        let line = vec![sp("10", 200.0, 11.0), sp(" ", 211.0, 2.0), sp("%", 213.0, 8.0)];
        let words = line_words(&line);
        assert_eq!(words.len(), 1, "percent unit split off: {words:?}");
        assert_eq!(words[0].text, "10%");
    }

    /// A currency symbol separated from the preceding token by a real column
    /// gutter is a genuinely different cell and must NOT be folded in.
    #[test]
    fn currency_symbol_after_a_gutter_is_not_folded() {
        let line = vec![sp("Total", 100.0, 30.0), sp("€", 200.0, 9.0)];
        let words: Vec<String> = line_words(&line).into_iter().map(|w| w.text).collect();
        assert_eq!(words, vec!["Total", "€"], "a separate symbol cell was fused: {words:?}");
    }

    /// Regression for the four fixtures the previous growth-boundary relaxation
    /// corrupted (`fnfe_Avoir_FR_type381_BASIC`, `fnfe_Facture_UE_*`,
    /// `mustang_validAvoir_FR_type380_BASICWL`): a two-row line-item grid whose
    /// totals/VAT block reuses the right-aligned amount edge, followed by a
    /// multi-line anchor-column label ("Taxe Base Montant Total HT" / "TVA
    /// collectée (vente)" / "Total taxes" / "Total TTC"). Once the currency
    /// symbols are folded the line-item grid is clean, so the only thing that
    /// used to stop the growth loop was the `flowing` veto — a totals row that
    /// shares the amount column's right edge could then be annexed, fabricating
    /// a single garbled product/totals row. An oversized title (20pt vs the
    /// table's 9pt) inflates the page-wide tolerance, which is exactly what let
    /// the totals row "match" the amount column in the 2×-tolerance gap pass.
    /// The totals block must never enter the line-item hit.
    #[test]
    fn totals_block_is_never_annexed_into_the_line_item_grid() {
        let mut lines: Vec<Vec<Span>> = Vec::new();
        // A 16pt title elsewhere on the page inflates the pass-wide tolerance
        // past the table's own 9pt scale — enough that a totals row 2.5pt off
        // the amount edge looks aligned in the 2×-tolerance gap pass.
        lines.push(vec![sz("AVOIR AV-2017-0005", 100.0, 700.0, 160.0, 16.0)]);

        // Two line items (9pt), each amount drawn as number + space + "€".
        let mut item_a = vec![
            sz("Nougat de l'Abbaye 250g", 34.1, 452.6, 100.8, 9.0),
            sz("5", 362.5, 452.6, 5.0, 9.0),
            sz("Unit(s)", 370.2, 452.6, 20.6, 9.0),
        ];
        item_a.extend(split_amount("4,55", 431.8, 17.5, 452.6, 9.0));
        item_a.push(sz("10%", 475.6, 452.6, 18.0, 9.0));
        item_a.extend(split_amount("-20,48", 528.8, 25.5, 452.6, 9.0));
        lines.push(item_a);

        let mut item_b = vec![
            sz("Huile d'olive à l'ancienne", 34.1, 434.4, 98.5, 9.0),
            sz("10", 357.5, 434.4, 10.0, 9.0),
            sz("Liter(s)", 370.2, 434.4, 21.8, 9.0),
        ];
        item_b.extend(split_amount("19,80", 426.8, 22.5, 434.4, 9.0));
        item_b.extend(split_amount("-198,00", 523.8, 30.5, 434.4, 9.0));
        lines.push(item_b);

        // Totals/VAT/Règlement block (7-10pt) reusing the amount right edge
        // (x≈561.8) and a multi-line anchor label.
        lines.push(vec![
            sz("Taxe", 88.8, 415.6, 15.4, 7.0),
            sz("Base", 188.3, 415.6, 16.8, 7.0),
            sz("Montant", 254.1, 415.6, 27.2, 7.0),
            sz("Total", 454.3, 415.6, 23.1, 7.0),
            sz("HT", 477.4, 415.6, 16.1, 7.0),
            sz("-218,48 €", 519.6, 415.6, 42.3, 7.0),
        ]);
        lines.push(vec![
            sz("TVA", 34.1, 402.0, 13.4, 7.0),
            sz("collectée", 47.0, 402.0, 29.3, 7.0),
            sz("(vente)", 76.4, 402.0, 23.6, 7.0),
            sz("20,0%", 100.0, 402.0, 21.7, 7.0),
            sz("-20,48 €", 203.2, 402.0, 25.7, 7.0),
            sz("-4,10 €", 279.3, 402.0, 21.8, 7.0),
        ]);
        lines.push(vec![
            sz("Total", 442.0, 395.5, 23.2, 10.0),
            sz("taxes", 465.2, 395.5, 28.2, 10.0),
            sz("-14,99 €", 525.1, 395.5, 36.8, 10.0),
        ]);
        lines.push(vec![
            sz("TVA", 34.1, 388.4, 13.4, 7.0),
            sz("collectée", 47.0, 388.4, 29.3, 7.0),
            sz("(vente)", 76.4, 388.4, 23.6, 7.0),
            sz("5,5%", 100.0, 388.4, 17.8, 7.0),
            sz("-198,00 €", 199.3, 388.4, 29.6, 7.0),
            sz("-10,89 €", 275.4, 388.4, 25.7, 7.0),
        ]);
        lines.push(vec![
            sz("Total TTC", 448.2, 378.2, 45.3, 10.0),
            sz("-233,47 €", 519.6, 378.2, 42.3, 10.0),
        ]);

        // The full detector: strict pass plus the 2×-tolerance stage-3b pass.
        let mut hits = find_tables(&lines);
        hits.extend(find_gap_tables(&lines, &hits));

        let item_hit = hits
            .iter()
            .find(|h| h.rows.iter().any(|r| r.iter().any(|c| c.contains("Nougat"))))
            .unwrap_or_else(|| panic!("line-item grid was not detected, hits={hits:?}"));
        let joined = item_hit
            .rows
            .iter()
            .flatten()
            .cloned()
            .collect::<Vec<_>>()
            .join(" | ");
        assert!(
            item_hit.rows.iter().any(|r| r.iter().any(|c| c.contains("Huile"))),
            "both line items must be in the grid: {joined}"
        );
        for forbidden in [
            "Taxe", "Base", "Montant", "Total", "taxes", "TTC", "TVA", "-218,48", "-14,99",
            "-233,47",
        ] {
            assert!(
                !joined.contains(forbidden),
                "totals/VAT label {forbidden:?} was annexed into the line-item hit: {joined}"
            );
        }
        assert_eq!(item_hit.rows.len(), 2, "totals rows merged into the grid: {:?}", item_hit.rows);
    }

    /// A narrow two-column table set in the page margin, sharing every visual
    /// line with the main column's prose. `find_tables` used to scan the merged
    /// lines, so the seed-and-grow window compared the table's rows against the
    /// prose and the `flowing` veto dropped the whole table to plain text. The
    /// detector must now isolate the right-hand column band and recover the
    /// table without annexing the prose.
    #[test]
    fn side_table_beside_prose_is_detected_without_the_prose() {
        let prose = |x: f64, y: f64, t: &str, adv: f64| sp_at(t, x, y, adv);
        let mut lines: Vec<Vec<Span>> = Vec::new();
        let params = [
            ("Parameter", "Value", "Unit"),
            ("dim", "4096", "params"),
            ("n_layers", "32", "layers"),
            ("head_dim", "128", "dim"),
            ("hidden_dim", "14336", "dim"),
            ("n_heads", "32", "heads"),
            ("vocab_size", "32000", "tokens"),
        ];
        for (i, (name, value, unit)) in params.iter().enumerate() {
            let y = 700.0 - 12.0 * i as f64;
            lines.push(vec![
                prose(40.0, y, "alpha", 28.0),
                prose(74.0, y, "beta", 24.0),
                prose(104.0, y, "gamma", 32.0),
                prose(142.0, y, "delta", 28.0),
                prose(300.0, y, name, 30.0),
                prose(360.0, y, value, 26.0),
                prose(410.0, y, unit, 30.0),
            ]);
        }
        let hits = find_tables(&lines);
        let hit = hits
            .iter()
            .find(|h| h.rows.iter().any(|r| r.iter().any(|c| c.contains("n_layers"))))
            .unwrap_or_else(|| panic!("side table was not detected, hits={hits:?}"));
        let joined = hit.rows.iter().flatten().cloned().collect::<Vec<_>>().join(" | ");
        assert!(
            joined.contains("4096") && joined.contains("32000"),
            "table cells missing: {joined}"
        );
        assert!(
            !joined.contains("alpha") && !joined.contains("delta"),
            "neighboring prose was annexed into the side table: {joined}"
        );
        assert!(
            hit.bbox.x0 >= 290.0,
            "hit must be confined to the right-hand column band: {:?}",
            hit.bbox
        );
    }

    /// A sparse value column (only one data row fills the last cell — the
    /// "± 0.07" of the Mistral benchmark grid) must not make the consolidator
    /// treat every following row as a wrapped continuation and fold the whole
    /// grid into one `<br>`-joined row.
    #[test]
    fn sparse_value_column_does_not_fold_rows_into_one() {
        let lines: Vec<Vec<Span>> = (0..5)
            .map(|i| vec![sp_at("x", 40.0, 700.0 - 12.0 * i as f64, 8.0)])
            .collect();
        let info: Vec<RowInfo> = lines
            .iter()
            .map(|l| RowInfo {
                words: line_words(l),
                starts: vec![40.0],
                ends: vec![48.0],
                size: 10.0,
            })
            .collect();
        let win_rows: Vec<usize> = (0..5).collect();
        let rows = vec![
            vec!["Model".into(), "MT".into(), "ELO".into()],
            vec!["A".into(), "1".into(), "2".into(), "+/- 0.07".into()],
            vec!["B".into(), "3".into(), "4".into(), "".into()],
            vec!["C".into(), "5".into(), "6".into(), "".into()],
            vec!["D".into(), "7".into(), "8".into(), "".into()],
        ];
        let out = consolidate_table_rows(rows, &win_rows, &lines, &info);
        assert_eq!(
            out.len(),
            5,
            "data rows were folded together: {out:?}"
        );
        for row in &out[1..] {
            assert!(
                !row.iter().any(|c| c.contains("<br>")),
                "distinct data rows were joined with <br>: {out:?}"
            );
        }
    }

    fn wt(text: &str, x0: f64, x1: f64) -> WordTok {
        WordTok { text: text.to_string(), x0, x1 }
    }

    /// A sign drawn as its own run before the line's final amount must fold
    /// onto it, so the deduction keeps its sign instead of splitting into a
    /// lone "-" cell and a separate "93" cell.
    #[test]
    fn merge_sign_tokens_joins_trailing_signed_amount() {
        let words = vec![wt("revenu", 0.0, 30.0), wt("-", 50.0, 53.0), wt("93", 86.0, 100.0)];
        let out = merge_sign_tokens(words);
        assert_eq!(out.len(), 2, "sign was not folded: {out:?}");
        assert_eq!(out[1].text, "-93");
    }

    /// An inline hyphen inside a longer run ("022 735 - 3477 (service …)") is
    /// not a signed trailing amount and must be left as drawn.
    #[test]
    fn merge_sign_tokens_leaves_inline_hyphen_alone() {
        let words = vec![
            wt("022", 0.0, 20.0),
            wt("735", 22.0, 40.0),
            wt("-", 42.0, 45.0),
            wt("3477", 60.0, 80.0),
            wt("(service", 82.0, 120.0),
        ];
        let out = merge_sign_tokens(words);
        assert!(out.iter().any(|w| w.text == "-"), "inline hyphen was folded: {out:?}");
        assert!(out.iter().any(|w| w.text == "3477"));
    }

    /// A later pass must not build a band that bridges rows another pass already
    /// claimed: the resulting hit would report the whole contiguous line range
    /// while omitting those covered rows, so `de_overlap_tables` would keep it
    /// and silently drop the covered table's data.
    #[test]
    fn gap_band_does_not_bridge_a_covered_region() {
        let lines = vec![
            vec![sp_at("label", 0.0, 400.0, 40.0), sp_at("100", 200.0, 400.0, 25.0)],
            vec![sp_at("covered", 0.0, 388.0, 60.0), sp_at("200", 200.0, 388.0, 25.0)],
            vec![sp_at("covered2", 0.0, 376.0, 65.0), sp_at("300", 200.0, 376.0, 25.0)],
            vec![sp_at("tail", 0.0, 364.0, 30.0), sp_at("400", 200.0, 364.0, 25.0)],
        ];
        let covered = vec![TableHit {
            start: 1,
            end: 2,
            rows: Vec::new(),
            bbox: BoundingBox::new(0.0, 0.0, 0.0, 0.0),
        }];
        let hits = scan_aligned_grids_opts(&lines, 2.0, &covered, false);
        assert!(
            hits.iter().all(|h| h.end < 1 || h.start > 2),
            "a gap hit bridged the covered rows: {hits:?}"
        );
    }
}
