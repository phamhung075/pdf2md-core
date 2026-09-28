// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Column-boundary derivation for a header-anchored ledger: the boundaries the
//! header labels and the data words agree on, and the exact cuts a page's drawn
//! thin vertical rules give.

use super::ledger::{ColumnKind, HeaderColumn};
use super::rulers::WordTok;

/// A text word may widen its column's extent only when its centre lies within
/// this many text sizes of the header label's own x-band.
const UNANIMOUS_TEXT_MARGIN_SIZE_MULT: f64 = 0.5;
/// A right-aligned amount value sits to the right of its header label, so it
/// gets a wider tolerance than a text word.
const UNANIMOUS_AMOUNT_MARGIN_SIZE_MULT: f64 = 3.0;
/// Line pitch assumed when the window is too short to measure one.
const FALLBACK_LINE_PITCH_PT: f64 = 12.0;
/// A drawn rule within this many points of the ledger's outer label edges is a
/// left/right table border, not an internal column separator.
const RULE_EDGE_TOL_PT: f64 = 1.0;
/// Two rule x's closer than this are the same separator drawn row by row.
const RULE_MERGE_TOL_PT: f64 = 2.0;

/// Re-derive the boundaries from the header labels *and* the data words: a
/// right-aligned amount column widens its extent to its values' right edge, so
/// the boundary between two columns is the midpoint of the two extents.
///
/// Only a word whose centre falls inside the header label's own x-band (plus a
/// kind-dependent margin) may widen that column. A short word at the left edge
/// of the wide description column — a section label, a continuation line —
/// centres well left of the label's band and would otherwise inflate the narrow
/// value column beside it, pushing the value/description boundary over the
/// description text. Amount columns take the wider margin because a
/// right-aligned value sits to the right of its label.
pub(super) fn refine_boundaries(
    header: &[HeaderColumn],
    data: &[Vec<WordTok>],
    size: f64,
) -> Vec<f64> {
    let margin_for = |kind: ColumnKind| match kind {
        ColumnKind::Amount => UNANIMOUS_AMOUNT_MARGIN_SIZE_MULT * size,
        _ => UNANIMOUS_TEXT_MARGIN_SIZE_MULT * size,
    };
    let mut left: Vec<f64> = header.iter().map(|c| c.x0).collect();
    let mut right: Vec<f64> = header.iter().map(|c| c.x1).collect();
    for words in data {
        for w in words {
            let center = 0.5 * (w.x0 + w.x1);
            for c in 0..header.len() {
                let m = margin_for(header[c].kind);
                if center >= header[c].x0 - m && center <= header[c].x1 + m {
                    left[c] = left[c].min(w.x0);
                    right[c] = right[c].max(w.x1);
                    break;
                }
            }
        }
    }
    let mut out = Vec::with_capacity(header.len().saturating_sub(1));
    for i in 0..header.len() - 1 {
        let mut b = 0.5 * (right[i] + left[i + 1]);
        if let Some(prev) = out.last() {
            if b <= *prev {
                b = *prev + 0.5;
            }
        }
        out.push(b);
    }
    out
}

/// Median vertical pitch of consecutive lines, or [`FALLBACK_LINE_PITCH_PT`].
pub(super) fn line_pitch(ys: &[f64]) -> f64 {
    let mut gaps: Vec<f64> = ys
        .windows(2)
        .map(|w| w[0] - w[1])
        .filter(|g| *g > 0.0)
        .collect();
    if gaps.is_empty() {
        return FALLBACK_LINE_PITCH_PT;
    }
    gaps.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    gaps[gaps.len() / 2]
}

/// The distinct x positions of drawn thin vertical rules that sit strictly
/// inside the `[left, right]` x-span and overlap the `[band_lo, band_hi]`
/// y-band. Collinear rules drawn row by row are merged within
/// [`RULE_MERGE_TOL_PT`]. Sorted ascending; may be empty.
///
/// This is the single source of truth for "which drawn rules are column
/// separators", shared by the header-anchored ledger ([`rule_boundaries`]) and
/// the ruled-frame model.
pub(crate) fn interior_rule_xs(
    rules: &[(f64, f64, f64)],
    left: f64,
    right: f64,
    band: (f64, f64),
) -> Vec<f64> {
    let (band_lo, band_hi) = band;
    let mut xs: Vec<f64> = Vec::new();
    for &(x, y0, y1) in rules {
        let (lo, hi) = (y0.min(y1), y0.max(y1));
        if hi < band_lo || lo > band_hi {
            continue;
        }
        if x <= left + RULE_EDGE_TOL_PT || x >= right - RULE_EDGE_TOL_PT {
            continue;
        }
        if xs.iter().all(|&v| (v - x).abs() > RULE_MERGE_TOL_PT) {
            xs.push(x);
        }
    }
    xs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    xs
}

/// The internal column separators given by drawn thin vertical rules.
///
/// When the surviving rules number exactly one per internal boundary they are
/// returned sorted; otherwise `None`, so a page whose rules are partial or
/// carry a stray line falls back to the label/data-derived boundaries.
pub(super) fn rule_boundaries(
    rules: &[(f64, f64, f64)],
    ncols: usize,
    left: f64,
    right: f64,
    band: (f64, f64),
) -> Option<Vec<f64>> {
    if rules.is_empty() || ncols < 2 {
        return None;
    }
    let xs = interior_rule_xs(rules, left, right, band);
    (xs.len() == ncols - 1).then_some(xs)
}
