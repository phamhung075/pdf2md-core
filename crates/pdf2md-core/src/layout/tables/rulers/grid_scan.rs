// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Stage 3 & Stage 3b ruler scanning, grid alignment, and column corridor analysis.

use super::*;

/// [`scan_aligned_grids`] with the merged-cell `wide_ok` veto setting threaded
/// through to `table_rulers_opts`.
pub(super) fn scan_aligned_grids_opts(
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
            .map(|r| tol_for(r.size))
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
                    let mid_gutter = min_gutter_for(mid_size);
                    if row_has_internal_gutter(&info[band[mid]].words, mid_gutter) {
                        break;
                    }
                }
                let max_size = [band[lo], band[hi]]
                    .iter()
                    .map(|&i| info[i].size)
                    .fold(0.0f64, f64::max);
                let min_gutter = min_gutter_for(max_size);
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
            let min_gutter = min_gutter_for(max_size);
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
                    (1.5 * tol_mult * tol_for(member_size)).min(tol * 1.5);
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
            // A wrapped/multi-line table header is often set immediately above
            // the first data row but produced none of the seed's shared rulers
            // (its multi-word cells left-align between the data columns), so it
            // was emitted as loose text above the table and every `top_heading`
            // relation failed. Annex up to four immediately-preceding header
            // lines from the same band when each is provably header-like (a
            // complex 3-line bilingual header is common in the French corpus).
            let mut win_lo = lo;
            while win_lo > 0 && lo - win_lo < 4 {
                let prev = band[win_lo - 1];
                let gap = lines[prev][0].y - lines[band[win_lo]][0].y;
                let size = info[prev].size.max(info[band[win_lo]].size).max(0.1);
                if !(gap > 0.0 && gap <= 2.2 * size) {
                    break;
                }
                if !header_like_row(&info, prev, &rulers, tol, min_gutter) {
                    break;
                }
                win_lo -= 1;
            }
            let win_rows: Vec<usize> = band[win_lo..=hi].to_vec();
            t(&format!(
                "  WINDOW lo={} (line {}, '{}') hi={} (line {}, '{}') rulers={:?}",
                win_lo, band[win_lo], line_text(&lines[band[win_lo]]),
                hi, band[hi], line_text(&lines[band[hi]]),
                rulers
            ));

            // No indivisible word can straddle an interior column ruler,
            // except in a prose-only continuation row (see `row_straddles`)
            // or when the crossing word is a wide merged/spanning cell
            // (`row_straddles_wide_ok`). As in `table_rulers_opts`, a crossing
            // only vetoes a ruler the window genuinely agrees on; a spurious
            // candidate crossed by a header's edge cell must not reject the
            // whole window.
            let win_starts: Vec<f64> = supported_starts(&info, &win_rows, tol);
            let win_support = |x: f64| -> bool {
                let n = win_rows
                    .iter()
                    .filter(|&&ri| row_matches_ruler(&info[ri], x, tol))
                    .count();
                n >= 2
            };
            let has_straddling_word = win_rows.iter().any(|&ri| {
                rulers[1..].iter().any(|&r| {
                    let straddled = if wide_ok {
                        row_straddles_wide_ok(&info, &win_starts, ri, r, tol, min_gutter)
                    } else {
                        row_straddles(&info[ri], &rulers, r, tol, min_gutter)
                    };
                    straddled && win_support(r)
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

            let multi_col_rows = count_multi_col_rows(&info, &win_rows, &rulers);
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
                let min_gutter = min_gutter_for(max_size);
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
                        let flowing_rows = count_flowing_rows(&info, &win_rows, &rulers);
                        // The per-row `flowing` test fires on ANY tight
                        // adjacent pair, so a compact description cell or one
                        // overflowing word can mark a row "flowing" even in a
                        // clearly tabular grid (the 60-row, 16-column
                        // itinerary). A numeric/currency/time column is a
                        // structural table signal no paragraph has, and a grid
                        // whose adjacent columns are mostly separated by wide
                        // gutters is tabular too; either overrides the veto.
                        let table_has_data_tokens = win_rows
                            .iter()
                            .any(|&ri| has_data_tokens(&bucket(&info, ri, &rulers)));
                        let wide_column_majority =
                            has_wide_column_majority(&info, &win_rows, &rulers);
                        if flowing_rows >= 2
                            && flowing_rows * 2 >= multi_col_rows
                            && !table_has_data_tokens
                            && !wide_column_majority
                        {
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
