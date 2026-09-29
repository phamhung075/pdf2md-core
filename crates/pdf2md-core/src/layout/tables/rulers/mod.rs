// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Stage 3 & Stage 3b ruler scanning, grid alignment, and column corridor analysis.

mod tokens;
pub use tokens::*;
mod rulers;
pub use rulers::*;
mod cells;
use cells::*;
mod banded;
use banded::*;
mod grid_scan;
use grid_scan::*;
mod header_spine;
use header_spine::*;
mod header_spine_legacy;
#[cfg(test)]
mod header_spine_tests;
mod window_rules;
use window_rules::*;
mod scan;
pub use scan::*;

use crate::layout::glyph_stream::Span;
use crate::layout::reading_order::{detect_column_bands_for_tables, ColumnBand};
use crate::layout::tables::consolidation::{bucket, bucket_rows_content_aware, bucket_words, consolidate_table_rows, merge_complementary_columns};
use crate::layout::tables::validation::{has_data_tokens, is_tabular_rows};
use crate::models::BoundingBox;

/// Ruler-alignment tolerance as a fraction of a row's font size, clamped
/// between [`TOL_MIN_PT`] and [`TOL_MAX_PT`].
pub(super) const TOL_SIZE_FRAC: f64 = 0.06;
pub(super) const TOL_MIN_PT: f64 = 0.5;
pub(super) const TOL_MAX_PT: f64 = 1.2;
/// Minimum column gutter: [`MIN_GUTTER_SIZE_MULT`] × font size, floored at
/// [`MIN_GUTTER_FLOOR_PT`].
pub(super) const MIN_GUTTER_SIZE_MULT: f64 = 1.1;
pub(super) const MIN_GUTTER_FLOOR_PT: f64 = 6.0;

/// Per-row alignment tolerance in points for a row of `size` pt.
pub(super) fn tol_for(size: f64) -> f64 {
    (TOL_SIZE_FRAC * size).clamp(TOL_MIN_PT, TOL_MAX_PT)
}

/// Minimum gutter width in points for a table set in `size` pt.
pub(super) fn min_gutter_for(size: f64) -> f64 {
    (MIN_GUTTER_SIZE_MULT * size).max(MIN_GUTTER_FLOOR_PT)
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_common;
#[cfg(test)]
mod tests_part2;
#[cfg(test)]
mod prose_veto_tests;
