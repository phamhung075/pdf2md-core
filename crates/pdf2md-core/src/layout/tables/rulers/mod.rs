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
mod window_rules;
use window_rules::*;
mod scan;
pub use scan::*;

use crate::layout::glyph_stream::Span;
use crate::layout::reading_order::{detect_column_bands_for_tables, ColumnBand};
use crate::layout::tables::consolidation::{bucket, bucket_rows_content_aware, bucket_words, consolidate_table_rows, merge_complementary_columns};
use crate::layout::tables::validation::{has_data_tokens, is_tabular_rows};
use crate::models::BoundingBox;

#[cfg(test)]
mod tests;
