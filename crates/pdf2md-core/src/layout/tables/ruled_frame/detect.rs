// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Drawn-rule frame detection: merge collinear vertical rules, then find the
//! frames whose interior rules agree on one row band. See [`super`] for the
//! table model built from a detected frame.

use super::{t, Frame, EDGE_EPS_PT};
use crate::layout::glyph_stream::Span;
use crate::layout::tables::rulers::line_words;

/// Two collinear vertical rules closer than this are one separator drawn in
/// pieces.
const RULE_X_MERGE_TOL_PT: f64 = 2.0;
/// A frame's column rule must overlap at least this fraction of the band to
/// count as one of its separators.
const MIN_RULE_BAND_COVERAGE: f64 = 0.6;
/// A frame needs a left rule, a right rule and at least two interior rules, so
/// at least three columns.
const MIN_FRAME_RULES: usize = 4;
/// A frame band shorter than this is not a table.
const MIN_FRAME_HEIGHT_PT: f64 = 24.0;
/// An outer rule may extend at most this fraction of the band beyond it before
/// it is page furniture (the page border) rather than the frame edge.
const MAX_OUTER_OVERSHOOT_FRAC: f64 = 0.35;

/// Merge collinear vertical rules (same x within [`RULE_X_MERGE_TOL_PT`]) into
/// one separator whose y-extent is their union. Returns `(x, ylo, yhi)` sorted
/// by x.
fn merge_verticals(rules: &[(f64, f64, f64)]) -> Vec<(f64, f64, f64)> {
    let mut sorted: Vec<(f64, f64, f64)> = rules
        .iter()
        .filter(|&&(x, y0, y1)| x.is_finite() && y0.is_finite() && y1.is_finite())
        .map(|&(x, y0, y1)| (x, y0.min(y1), y0.max(y1)))
        .collect();
    sorted.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

    let mut out: Vec<(f64, f64, f64)> = Vec::new();
    for (x, y0, y1) in sorted {
        match out.last_mut() {
            Some(last) if (x - last.0).abs() <= RULE_X_MERGE_TOL_PT => {
                // Keep a running mean x so a chain of near-collinear pieces
                // does not drift onto the next separator.
                last.0 = 0.5 * (last.0 + x);
                last.1 = last.1.min(y0);
                last.2 = last.2.max(y1);
            }
            _ => out.push((x, y0, y1)),
        }
    }
    out
}


/// Does any in-band line carry a word whose centre lies strictly between
/// `x0` and `x1`?
fn band_has_content(lines: &[Vec<Span>], x0: f64, x1: f64, lo: f64, hi: f64) -> bool {
    if x1 - x0 <= EDGE_EPS_PT {
        return false;
    }
    lines.iter().any(|line| {
        line.first().map(|s| (lo..=hi).contains(&s.y)).unwrap_or(false)
            && line_words(line).iter().any(|w| {
                let c = 0.5 * (w.x0 + w.x1);
                c > x0 + EDGE_EPS_PT && c < x1 - EDGE_EPS_PT
            })
    })
}

/// Derive every candidate frame on the page from the drawn vertical rules.
pub(super) fn find_frames(lines: &[Vec<Span>], rules: &[(f64, f64, f64)]) -> Vec<Frame> {
    let merged = merge_verticals(rules);
    t(|| {
        format!(
            "verticals={} merged={:?}",
            rules.len(),
            merged
                .iter()
                .map(|&(x, lo, hi)| format!("{x:.0}[{lo:.0},{hi:.0}]"))
                .collect::<Vec<_>>()
        )
    });
    let mut candidates: Vec<Frame> = Vec::new();

    for &(_, sy0, sy1) in &merged {
        let (slo, shi) = (sy0, sy1);
        let height = shi - slo;
        if height < MIN_FRAME_HEIGHT_PT {
            continue;
        }
        // A candidate column rule must span most of this seed's band and must
        // not run far past it (a page border does).
        let mut members: Vec<(f64, f64, f64)> = Vec::new();
        for &r in &merged {
            let overlap = (r.2.min(shi) - r.1.max(slo)).max(0.0);
            if overlap / height < MIN_RULE_BAND_COVERAGE {
                continue;
            }
            let overshoot = (slo - r.1).max(0.0) + (r.2 - shi).max(0.0);
            if overshoot / height > MAX_OUTER_OVERSHOOT_FRAC {
                continue;
            }
            members.push(r);
        }
        if members.len() < MIN_FRAME_RULES {
            continue;
        }
        members.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        // The band is what the *interior* rules agree on; the outer rules may
        // span the whole page frame and would otherwise annex rows below the
        // table.
        let interior = &members[1..members.len() - 1];
        let lo = interior
            .iter()
            .map(|r| r.1)
            .fold(f64::NEG_INFINITY, f64::max);
        let hi = interior
            .iter()
            .map(|r| r.2)
            .fold(f64::INFINITY, f64::min);
        if !lo.is_finite() || !hi.is_finite() || hi - lo < MIN_FRAME_HEIGHT_PT {
            continue;
        }
        let (members, lo, hi) = (members, lo, hi);

        // A table's left edge is frequently the page's own left border: it
        // extends far past the row band, so the overshoot filter above drops it,
        // yet without it the wide first (label) column is lost. Adopt the
        // nearest rule to the left that spans the band, but only when the strip
        // between it and the first column rule actually holds table content —
        // so a small centered grid does not annex a distant page border. The
        // right edge is deliberately *not* extended this way: a rule beyond the
        // rightmost column rule is the start of a sidebar or the page border,
        // and the work order keeps anything outside the outer vertical rules out
        // of the table.
        let first_x = members[0].0;
        let left_edge = merged
            .iter()
            .filter(|r| r.0 < first_x - RULE_X_MERGE_TOL_PT)
            .filter(|r| {
                let band_height = hi - lo;
                let overlap = (r.2.min(hi) - r.1.max(lo)).max(0.0);
                band_height > 0.0 && overlap / band_height >= MIN_RULE_BAND_COVERAGE
            })
            .max_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal))
            .copied();
        let adopt_left = left_edge
            .filter(|r| band_has_content(lines, r.0, first_x, lo, hi))
            .map(|r| r.0);
        let mut boundaries: Vec<f64> = members.iter().map(|r| r.0).collect();
        if let Some(x) = adopt_left {
            boundaries.insert(0, x);
        }
        candidates.push(Frame { boundaries, lo, hi });
    }
    t(|| {
        format!(
            "candidates={}",
            candidates
                .iter()
                .map(|c| format!(
                    "{}cols[{:.0},{:.0}]{:?}",
                    c.ncols(),
                    c.lo,
                    c.hi,
                    c.boundaries.iter().map(|b| b.round()).collect::<Vec<_>>()
                ))
                .collect::<Vec<_>>()
                .join(" ")
        )
    });

    // Deduplicate identical boundaries, then prefer the widest column set and
    // the tallest band; drop candidates whose band overlaps an accepted one.
    candidates.sort_by(|a, b| {
        b.ncols()
            .cmp(&a.ncols())
            .then(
                (b.hi - b.lo)
                    .partial_cmp(&(a.hi - a.lo))
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
    });
    let mut chosen: Vec<Frame> = Vec::new();
    for c in candidates {
        let dup = chosen.iter().any(|f| {
            f.boundaries.len() == c.boundaries.len()
                && f.boundaries
                    .iter()
                    .zip(&c.boundaries)
                    .all(|(a, b)| (a - b).abs() <= RULE_X_MERGE_TOL_PT)
        });
        if dup {
            continue;
        }
        let overlaps = chosen.iter().any(|f| {
            let lo = f.lo.max(c.lo);
            let hi = f.hi.min(c.hi);
            hi - lo > 0.5 * (f.hi - f.lo).min(c.hi - c.lo)
        });
        if overlaps {
            continue;
        }
        chosen.push(c);
    }
    chosen
}
