// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! PDF content-stream glyph extraction, font metric resolution, and 2D span aggregation.

use super::*;

/// Extract text for a glyph-positioned page using geometry reconstruction.
/// When `detect_tables` is false, returns the plain reading-order text with
/// no table recovery (byte-identical to the table-less renderer).
pub fn extract_page_glyphs(
    doc: &Document,
    page_id: ObjectId,
    detect_tables: bool,
    detect_layout: bool,
    detect_math: bool,
) -> Result<PageText, String> {
    let chain = crate::text_extract::resource_dicts(doc, page_id);
    let mut raw_fonts: std::collections::BTreeMap<Vec<u8>, &Dictionary> =
        std::collections::BTreeMap::new();
    crate::text_extract::collect_fonts(doc, &chain, &mut raw_fonts);
    let has_fonts = !raw_fonts.is_empty();

    let content: Content<Vec<Operation>> = crate::text_extract::decode_page_content(doc, page_id)
        .map_err(|e| e.to_string())?;

    let (init_ctm, mut page_height, mut page_width) = page_display_size(doc, page_id);

    let font_key = chain
        .first()
        .map(|d| *d as *const Dictionary as usize)
        .unwrap_or(0);
    let mut font_cache: HashMap<usize, Vec<GlyphFontInfo>> = HashMap::new();
    font_cache.insert(font_key, build_glyph_font_table(doc, &raw_fonts));
    let mut text_ops_seen = false;
    let mut budget = GlyphBudget::new();
    let walk = walk_glyphs(
        doc,
        &chain,
        &content.operations,
        init_ctm,
        &mut Vec::new(),
        0,
        &mut text_ops_seen,
        &mut budget,
        &mut font_cache,
    );
    let GlyphWalk {
        mut spans,
        underline_segs,
        vertical_up_chars,
        vertical_down_chars,
    } = walk;

    // A page whose text is dominantly vertical (a sideways OCR layer, or a
    // producer that draws the whole page rotated 90° with a text matrix instead
    // of `/Rotate`) is otherwise emptied: every run is classified vertical and
    // dropped as margin furniture by `append_vertical_text`. Rotate such a page
    // upright and run the normal horizontal pipeline. A horizontal-majority
    // page keeps the existing margin behaviour for its minority vertical runs
    // (side stamps, letterheads).
    let total_chars: usize = spans.iter().map(|s| s.text.chars().count()).sum();
    let vertical_chars = vertical_up_chars + vertical_down_chars;
    if total_chars > 0 && vertical_chars * 5 >= total_chars * 3 {
        let old_height = page_height;
        page_height = rotate_spans_upright(&mut spans, vertical_up_chars >= vertical_down_chars);
        // Rotating a dominantly-vertical page upright swaps its display box.
        page_width = old_height;
    }

    let (horizontal_spans, vertical_spans): (Vec<Span>, Vec<Span>) =
        spans.into_iter().partition(|s| !s.is_vertical);


    let mut vertical_blocks: Vec<crate::layout::reading_order::DocBlock> = Vec::new();
    let mut vertical_text = String::new();
    if !vertical_spans.is_empty() {
        let mut runs: Vec<Vec<Span>> = Vec::new();
        for s in vertical_spans {
            match runs.last_mut() {
                Some(last) => {
                    let last_sp = last.last().unwrap();
                    let dist = ((s.x - last_sp.x).powi(2) + (s.y - last_sp.y).powi(2)).sqrt();
                    if dist <= 3.5 * s.size.max(last_sp.size) {
                        last.push(s);
                    } else {
                        runs.push(vec![s]);
                    }
                }
                None => runs.push(vec![s]),
            }
        }
        for run in runs {
            let mut run_text = String::new();
            let mut min_x = f64::INFINITY;
            let mut min_y = f64::INFINITY;
            let mut max_x = f64::NEG_INFINITY;
            let mut max_y = f64::NEG_INFINITY;
            for sp in &run {
                min_x = min_x.min(sp.x);
                min_y = min_y.min(sp.y);
                max_x = max_x.max(sp.x + sp.advance);
                max_y = max_y.max(sp.y + sp.size);
                if sp.text == " " {
                    if !run_text.is_empty() && !run_text.ends_with(' ') {
                        run_text.push(' ');
                    }
                } else {
                    run_text.push_str(&sp.text);
                }
            }
            let trimmed = run_text.trim();
            if !trimmed.is_empty() {
                if !vertical_text.is_empty() {
                    vertical_text.push('\n');
                }
                vertical_text.push_str(trimmed);
                vertical_blocks.push(crate::layout::reading_order::DocBlock {
                    page: 0,
                    kind: "margin".to_string(),
                    x0: min_x,
                    y0: min_y,
                    x1: max_x,
                    y1: max_y,
                    text: trimmed.to_string(),
                    is_bold: false,
                    is_italic: false,
                    is_underline: false,
                });
            }
        }
    }

    // Lightweight Hough/Radon skew correction, applied BEFORE line clustering.
    // `build_lines` groups spans by their baseline `y`, so a page tilted by
    // even a couple of degrees fragments rows (spans on one visual line no
    // longer share a `y`) and smears column gutters. Estimate the tilt from the
    // raw span cloud — no line grouping required — and rotate the spans back
    // onto the page axes, so clustering, two-column detection and grid
    // recovery all see axis-aligned rows. On an axis-aligned page the estimator
    // returns a sub-threshold angle and the spans pass through untouched.
    let skew_deg = crate::layout::skew::estimate_skew_angle_deg_from_spans(
        &horizontal_spans,
        crate::layout::skew::DEFAULT_MAX_SKEW_DEG,
        crate::layout::skew::COARSE_STEP_DEG,
    );
    let mut lines = if skew_deg.abs() >= crate::layout::skew::MIN_SKEW_TO_CORRECT_DEG {
        build_lines(&crate::layout::skew::deskew_spans(&horizontal_spans, skew_deg))
    } else {
        build_lines(&horizontal_spans)
    };
    if std::env::var("PDF2MD_DEBUG").is_ok() {
        for (i, l) in lines.iter().enumerate() {
            let txt: String = l.iter().map(|s| format!("({},{}) '{}'", s.x.round(), s.y.round(), s.text)).collect::<Vec<_>>().join(" | ");
            eprintln!("Line {}: {}", i, txt);
        }
    }
    let mut hits = if detect_tables {
        find_tables(&lines)
    } else {
        Vec::new()
    };
    // Stage-3b: re-run the grid scan with a wider alignment tolerance over
    // rows the strict pass missed (jittered / borderless tables).
    if detect_tables {
        let gap_hits = find_gap_tables(&lines, &hits);
        if std::env::var("PDF2MD_DEBUG").is_ok() {
            eprintln!("DBG: find_tables hits={}, gap_hits={}", hits.len(), gap_hits.len());
        }
        hits.extend(gap_hits);
        hits.sort_by(|a, b| a.start.cmp(&b.start));
    }
    if std::env::var("PDF2MD_DEBUG").is_ok() {
        eprintln!("DBG: total hits before 2col filter: {}", hits.len());
    }
    // When a page contains a 2-column prose reading block, filter out any
    // 2-column table hits that lie inside the prose block (false positive prose).
    // Genuine tables with >= 3 columns or outside the prose block are kept.
    if detect_layout {
        if let Some(pc) = page_two_columns(&lines) {
            let col_top = pc.left.iter().chain(pc.right.iter())
                .map(|l| l[0].y).fold(f64::NEG_INFINITY, f64::max);
            let col_bottom = pc.left.iter().chain(pc.right.iter())
                .map(|l| l[0].y).fold(f64::INFINITY, f64::min);
            hits.retain(|h| {
                let num_cols = h.rows.iter().map(|r| r.len()).max().unwrap_or(0);
                if num_cols >= 3 {
                    return true;
                }
                let hit_y0 = lines[h.end][0].y;
                let hit_y1 = lines[h.start][0].y;
                let outside_prose = hit_y0 > col_top || hit_y1 < col_bottom;
                if outside_prose {
                    return true;
                }
                // Inside the two-column block: a genuine 2/3-column *table*
                // should be kept even when `page_two_columns` misreads the
                // table's own aligned columns as a prose gutter. Only drop a
                // hit that is actually prose noise: ragged columns, or cells
                // that hold long flowing text (a paragraph fragment). A real
                // table has a consistent column count and brief cells.
                let cols = h.rows.first().map(|r| r.len()).unwrap_or(0);
                let rectangular = cols >= 2 && h.rows.iter().all(|r| r.len() == cols);
                let max_cell_words = h
                    .rows
                    .iter()
                    .flat_map(|r| r.iter())
                    .map(|c| c.split_whitespace().count())
                    .max()
                    .unwrap_or(0);
                rectangular && max_cell_words <= 5
            });
        }
    }
    // Underline: match collected thin horizontal rules to spans, skipping any
    // visual line already claimed by a recovered table (whose row borders are
    // the same sort of thin rule).
    if !underline_segs.is_empty() {
        let covered: std::collections::HashSet<usize> =
            hits.iter().flat_map(|h| h.start..=h.end).collect();
        mark_underlines(&mut lines, &underline_segs, &covered);
    }
    let table_rendered = !hits.is_empty();
    let mut text = if table_rendered {
        // Byte-identical to the plain text renderer when no table is found.
        render_with_tables(&lines, &hits, Some(page_width))
    } else if detect_layout {
        if detect_math {
            render_math(&lines, &underline_segs, page_height, true, Some(page_width))
        } else {
            render_human_order(&lines, page_height, true, Some(page_width))
        }
    } else if detect_math {
        render_math(&lines, &underline_segs, page_height, false, Some(page_width))
    } else {
        render_cluster(&lines, Some(page_width))
    };


    let mut blocks = if detect_layout {
        build_doc_blocks(&lines, page_height, Some(page_width))
    } else {
        Vec::new()
    };

    // Emit one "table" zone per recovered canvas grid so callers can visualise
    // the detected layer. Reading-order fragments (body/list text) that fall
    // entirely inside a recovered table bbox are suppressed so the zone view
    // shows the table as a single region rather than scattered cells.
    if detect_tables && !hits.is_empty() {
        append_table_zones(&mut blocks, &hits);
    }

    if !vertical_text.is_empty() {
        // Side furniture/margin stamps stay out of the reading-order body text
        // when layout analysis is on (their blocks are still emitted below for
        // zone inspectors). Without layout analysis, preserve legacy behavior.
        append_vertical_text(&mut text, &vertical_text, detect_layout);
        blocks.extend(vertical_blocks);
    }

    Ok(PageText {
        text,
        text_ops_seen,
        has_fonts,
        tables: if table_rendered { hits.len() } else { 0 },
        blocks,
        budget_exhausted: budget.exhausted,
    })
}
