// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Reading order recovery, multi-column stream separation, and structured DocBlock generation.

use super::*;

/// Human reading order for the page as streams of visual lines: single-column
/// pages produce one stream (top-down); two-column pages produce full-width
/// header rows, left column, right column, footer rows.
pub fn page_read_order(lines: &[Vec<Span>]) -> Vec<Vec<Vec<Span>>> {
    if let Some(pc) = page_two_columns(lines).filter(columns_are_viable_prose) {
        let mut streams = Vec::new();
        if !pc.top_full.is_empty() {
            streams.push(pc.top_full);
        }
        streams.push(pc.left);
        streams.push(pc.right);
        if !pc.bottom_full.is_empty() {
            streams.push(pc.bottom_full);
        }
        return streams;
    }
    read_order_segmented(lines)
}

/// Depth cap for the recursive page segmentation. The recursion already
/// terminates (each projection peel passes strictly smaller line slices), but a
/// pathological page that peels one region at a time could nest once per line;
/// at the cap the remaining lines are emitted as one stream. 512 is far above
/// any real page's segmentation depth.
const MAX_SEGMENT_RECURSION_DEPTH: usize = 512;

/// Segment a page — or the part of one left over after a `page_two_columns`
/// decision — into column bands, recovering *every* multi-column region rather
/// than only the single longest one.
///
/// `multi_column_projection` finds one 3+-column region and, previously, the
/// rows above and below it were emitted as one full-width stream each. Real
/// pages carry several such regions — a three-column deck at the top, a
/// two-column quote box below, then a three-column footer block — so the
/// leftovers are segmented recursively: the projection pass peels off the
/// longest 3+-column region, the rows before and after it are segmented again,
/// and a slice with no projection region falls through to
/// `detect_column_bands` for its 2-column bands. `page_two_columns` is
/// deliberately not re-run on the leftovers: it is a whole-page decision, and
/// applying it to an arbitrary slice would let a partial layout masquerade as
/// a page-wide one.
pub(super) fn read_order_segmented(lines: &[Vec<Span>]) -> Vec<Vec<Vec<Span>>> {
    read_order_segmented_at(lines, 0)
}

fn read_order_segmented_at(lines: &[Vec<Span>], depth: usize) -> Vec<Vec<Vec<Span>>> {
    if depth >= MAX_SEGMENT_RECURSION_DEPTH {
        return vec![lines.to_vec()];
    }
    // A 3+-column page whose narrow (~0.7em) gutters no single-gutter detector
    // can seed: `page_two_columns` needs one gutter across the whole page and
    // `detect_column_bands` splits a run at one gutter, leaving the remaining
    // columns of the split half welded. Recover every column at once by
    // vertical projection before the band pass.
    if let Some(region) = multi_column_projection(lines) {
        let mut streams = Vec::new();
        if region.start > 0 {
            streams.extend(read_order_segmented_at(&lines[..region.start], depth + 1));
        }
        streams.extend(region.columns);
        if region.end + 1 < lines.len() {
            streams.extend(read_order_segmented_at(&lines[region.end + 1..], depth + 1));
        }
        return streams;
    }
    // `page_two_columns` only recognizes a page that is two-column under ONE
    // consistent gutter for its entire height. Real business documents often
    // change layout partway down (e.g. a seller/buyer two-column header block,
    // a full-width item table, then a differently-positioned two-column
    // footer block with bank details) — no single global gutter fits all of
    // that, so the whole-page detector bails out and the page fell back to
    // one linear top-to-bottom stream, weaving unrelated columns together
    // line-by-line. `detect_column_bands` recovers each such block
    // independently instead of requiring one page-wide layout.
    let bands = detect_column_bands(lines);
    if bands.len() > 1
        || matches!(
            bands.first(),
            Some(ColumnBand::Columns { .. } | ColumnBand::Stacks(_))
        )
    {
        let mut streams = Vec::new();
        for band in bands {
            match band {
                ColumnBand::Full(rows) => streams.push(rows),
                ColumnBand::Columns { left, right } => {
                    streams.push(left);
                    streams.push(right);
                }
                ColumnBand::Stacks(columns) => streams.extend(columns),
            }
        }
        return streams;
    }
    vec![lines.to_vec()]
}
