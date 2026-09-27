// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Page-body assembly for one page of `convert_pdf_bytes_to_markdown`: media
//! embedding (data-URI inlining, adaptive shrink, budget omission) and the
//! reading-order placement of each figure next to its caption/block. Split out
//! of `convert.rs` to keep both files under the Rule 07 limit; the media
//! budget behaviour is unchanged and covered by the `regression_tests`
//! `media_budget_*` tests.

use super::*;
use crate::layout::DocBlock;
use crate::media::MediaItem;
use lopdf::{Document, ObjectId};

/// Append one page's body (`processed_text`, then any media whose
/// reading-order anchor could not be found) to `chunk`.
///
/// `processed_text` is taken by `&mut` so the embed branch can splice image
/// markdown into it; any figures that could not be anchored are appended at the
/// end. `media_budget_used` is the running document-wide base64 byte total.
#[allow(clippy::too_many_arguments)]
pub(crate) fn append_page_body(
    doc: &Document,
    page_id: ObjectId,
    options: &ConversionOptions,
    page_media: &[MediaItem],
    page_blocks: &[DocBlock],
    processed_text: &mut String,
    media_budget_used: &mut usize,
    chunk: &mut String,
) {
    if options.media_mode != MediaMode::Embed {
        chunk.push_str(processed_text.as_str());
        chunk.push_str("\n\n");
        return;
    }

    // Page-relative sizing: reproduce the on-page footprint so a high-DPI
    // placement that covers only part of the page does not render at its
    // full pixel width (which is much larger than it was on the page).
    let page_bbox = page_media_box(doc, page_id);
    let mut imgs: Vec<&MediaItem> = page_media
        .iter()
        .filter(|m| {
            !m.decorative
                && !m.data_b64.is_empty()
                && (m.format == "image/jpeg" || m.format == "image/png")
        })
        .collect();
    // Sort ascending by y (lowest y first) so bottom-most images are inserted first,
    // preserving string character offsets for images higher up on the page.
    imgs.sort_by(|a, b| {
        let ay = (a.y0 + a.y1) / 2.0;
        let by = (b.y0 + b.y1) / 2.0;
        ay.partial_cmp(&by).unwrap_or(std::cmp::Ordering::Equal)
    });

    let mut remaining_imgs = Vec::new();
    for m in imgs {
        let my = (m.y0 + m.y1) / 2.0;
        let b64_len = m.data_b64.len();
        // Inline payload to splice into the markdown: either the
        // original data URI or an adaptive shrink when the full-size
        // copy would push the running total over budget. `None` is the
        // true last resort (omission placeholder).
        let mut inline: Option<(String, &str)> = None;
        if media_budget_used.saturating_add(b64_len) <= options.max_media_bytes_per_doc {
            *media_budget_used += b64_len;
            inline = Some((m.data_b64.clone(), m.format.as_str()));
        } else {
            // Over the per-document embed budget: before falling back
            // to the omission placeholder, try progressively
            // downscaling (and, under `vision`, JPEG-recompressing)
            // the figure so it fits the *remaining* budget. Only this
            // markdown copy is shrunk — `media_items`/`media` (the
            // JSON side-channel, pushed above) keeps the full bytes.
            let remaining = options.max_media_bytes_per_doc.saturating_sub(*media_budget_used);
            if let Some((bytes, mime)) =
                media::raster::shrink_encoded_image_to_fit(&m.data, &m.format, remaining)
            {
                let b64 = media::b64encode(&bytes);
                *media_budget_used += b64.len();
                inline = Some((b64, mime));
            }
        }
        let img_md = match inline {
            Some((b64, mime)) => match embed_width_frac(m, page_bbox) {
                Some(frac) => format!(
                    "<img alt=\"{}\" src=\"data:{};base64,{}\" width=\"{}\" />\n\n",
                    m.kind.as_str(),
                    mime,
                    b64,
                    // Emit as a percent of the reader's content width, which
                    // matches the fraction of the page the image occupied.
                    format!("{}%", (frac * 100.0).round() as u32),
                ),
                None => format!(
                    "![{}](data:{};base64,{})\n\n",
                    m.kind.as_str(),
                    mime,
                    b64
                ),
            },
            // Even the pixel/quality floor does not fit: keep the
            // figure's reading-order position (so surrounding text
            // still makes sense) but drop the payload instead of
            // inlining another multi-hundred-KB data URI.
            None => format!(
                "*[{} omitted: {}x{} {}, {} KB — over the {} KB per-document image budget]*\n\n",
                m.kind.as_str(),
                m.width,
                m.height,
                m.format,
                b64_len / 1024,
                options.max_media_bytes_per_doc / 1024,
            ),
        };

        // Prefer the figure's caption as the anchor when one sits
        // below/near the image and horizontally overlaps it (or spans
        // the page): a descriptive caption is the true reading-order
        // neighbour, and inserting before it keeps the figure next to
        // its caption. Otherwise pick the nearest block immediately
        // below (max mid-y) that horizontally overlaps the image, so a
        // footnote or a different-column block is not used and the
        // figure is not displaced far from its caption.
        let mw = (m.x1 - m.x0).abs();
        let page_w = page_bbox.map_or(0.0, |(px0, _, px1, _)| (px1 - px0).abs());
        let caption_block = page_blocks
            .iter()
            .filter(|b| b.kind == "caption")
            .filter(|b| {
                let overlap = b.x1.min(m.x1) - b.x0.max(m.x0);
                let full_width = page_w > 0.0 && (b.x1 - b.x0).abs() >= 0.9 * page_w;
                overlap > 0.0 || full_width
            })
            .filter(|b| {
                let size_hint = (b.y1 - b.y0).abs().max(8.0);
                (b.y0 + b.y1) / 2.0 <= my + 4.0 * size_hint
            })
            .max_by(|a, b| {
                let ay = (a.y0 + a.y1) / 2.0;
                let by = (b.y0 + b.y1) / 2.0;
                ay.partial_cmp(&by).unwrap_or(std::cmp::Ordering::Equal)
            });
        let next_block = caption_block.or_else(|| {
            page_blocks
                .iter()
                .filter(|b| b.kind != "figure")
                .filter(|b| (b.y0 + b.y1) / 2.0 <= my)
                .filter(|b| {
                    let bw = (b.x1 - b.x0).abs();
                    let overlap = b.x1.min(m.x1) - b.x0.max(m.x0);
                    overlap > 0.2 * mw.min(bw) || mw > 300.0
                })
                .max_by(|a, b| {
                    let ay = (a.y0 + a.y1) / 2.0;
                    let by = (b.y0 + b.y1) / 2.0;
                    ay.partial_cmp(&by).unwrap_or(std::cmp::Ordering::Equal)
                })
        });

        let mut inserted = false;
        if let Some(b) = next_block {
            let search_key = b
                .text
                .lines()
                .map(|l| l.trim())
                .find(|l| l.len() >= 4)
                .unwrap_or_else(|| b.text.lines().next().unwrap_or(&b.text).trim());
            if !search_key.is_empty() {
                if let Some(mut pos) = processed_text.find(search_key) {
                    // `search_key` is plain block text (from
                    // build_doc_blocks) with no structural prefix, so a
                    // match can land mid-line — e.g. after the "# "/
                    // "- "/"1. " a heading/list line now carries. Snap
                    // back to the start of that line so the image is
                    // never spliced into the middle of a marker,
                    // orphaning it as a bare "#" with nothing after.
                    let line_start = processed_text[..pos].rfind('\n').map_or(0, |i| i + 1);
                    pos = line_start;
                    let (t_start, t_end) = find_table_boundaries(processed_text, pos);
                    if t_start != t_end {
                        pos = t_end;
                    } else if pos > 0 && !processed_text[..pos].ends_with("\n\n") {
                        // The matched line is a wrapped continuation
                        // inside an ongoing paragraph or list item: only
                        // a single newline separates it from the
                        // non-empty text above, so splicing `img_md` at
                        // `pos` would cut the sentence/list item in half.
                        // Walk back to the nearest preceding blank line
                        // (or the string start) so the image lands
                        // before the entire block, never inside it.
                        pos = processed_text[..pos].rfind("\n\n").map_or(0, |i| i + 2);
                    }
                    let mut prefix = String::new();
                    if !processed_text[..pos].ends_with("\n\n") {
                        if processed_text[..pos].ends_with('\n') {
                            prefix.push('\n');
                        } else {
                            prefix.push_str("\n\n");
                        }
                    }
                    let full_img = format!("{}{}", prefix, img_md);
                    processed_text.insert_str(pos, &full_img);
                    inserted = true;
                }
            }
        }
        if !inserted {
            remaining_imgs.push(img_md);
        }
    }

    chunk.push_str(processed_text.as_str());
    chunk.push_str("\n\n");
    remaining_imgs.reverse();
    for img_md in remaining_imgs {
        chunk.push_str(&img_md);
    }
}
