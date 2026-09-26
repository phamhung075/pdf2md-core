//! pdf2md-core — High-performance native Rust core engine for sub-millisecond
//! PDF-to-Markdown extraction and 2D spatial canvas table reconstruction.
//!
//! Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
//! SPDX-License-Identifier: BSL-1.1
//! Licensed under the Business Source License 1.1 (BSL-1.1).

use super::*;

/// Converts PDF byte slice to clean Markdown with 2D spatial table reconstruction.
pub fn convert_pdf_bytes_to_markdown(
    bytes: &[u8],
    options: &ConversionOptions,
) -> Result<ConversionResult, String> {
    let t0 = MonoClock::now();

    // Try parsing with lopdf, falling back to a cross-reference repair for the
    // real-world producers whose `startxref`/object offsets have drifted.
    let doc = load_pdf_document(bytes)?;

    let mut full_markdown = String::new();
    let total_pages = doc.get_pages().len();
    if total_pages > MAX_PAGES {
        return Err(format!(
            "document has {total_pages} pages, over the {MAX_PAGES}-page limit"
        ));
    }
    let mut total_words = 0;
    // Pages whose own word count falls below `options.min_words_per_page`.
    // Unlike `total_words == 0` (checked once, document-wide, below), this
    // catches the case a whole-document zero-check misses entirely: a
    // mostly-scanned document where a handful of pages carry real text (a
    // cover sheet, a signed last page) so the document-level total is well
    // above zero, but most individual pages are near-empty. Callers (the
    // gateway's escalation decision) compare this against `total_pages` as a
    // ratio — a document-wide count alone can't distinguish "a few blank
    // pages in an otherwise fine document" from "mostly blank".
    let mut pages_below_word_floor = 0usize;
    let mut any_text_ops = false;
    let mut any_fonts = false;
    // True when any page's own content stream could not be decoded at all
    // (e.g. an over-cap decompression bomb). Such a page looks like
    // "text-show operators present, zero words" once the fallback fills in
    // `text_ops_seen: true`, so this flag keeps a genuine decode failure on
    // the explicit-error path instead of the glyph-encoded status path.
    let mut any_decode_failure = false;
    let mut tables_detected = 0usize;
    // True when any page's extraction hit a work bound and truncated.
    let mut budget_exhausted = false;
    let mut media_items: Vec<media::MediaItem> = Vec::new();
    // Running total of base64 bytes inlined into the markdown as `data:`
    // URIs so far, across every page. Once `options.max_media_bytes_per_doc`
    // is reached, further images are replaced with a text placeholder
    // instead of another data URI (see the `MediaMode::Embed` loop below) — this
    // is what actually bounds the emitted markdown size for scan-heavy
    // documents; the per-image downscale in `media::extract_page_media`
    // only shrinks the common case.
    let mut media_budget_used: usize = 0;
    let mut block_items: Vec<layout::DocBlock> = Vec::new();
    // Per-page markdown chunks (page number, content) so running headers and
    // footers can be suppressed after the doc-level furniture pass.
    let mut page_md: Vec<(u32, String)> = Vec::new();
    // True when the document carries a tagged /StructTreeRoot. Computed once so
    // the (untagged) common case pays no per-page structure scan.
    let has_struct_tree = layout::has_struct_tree(&doc);

    for (page_num, page_id) in doc.get_pages() {
        // Prefer our own multilingual decoder (correct WinAnsi/Differences/
        // ToUnicode handling — see text_extract.rs) and only fall back to
        // lopdf's extractor when the page content cannot be parsed at all.
        let (page_text, marker_hint, decode_failed) =
            select_page_text(&doc, page_num, page_id, options, has_struct_tree);
        any_decode_failure |= decode_failed;
        any_text_ops |= page_text.text_ops_seen;
        any_fonts |= page_text.has_fonts;
        tables_detected += page_text.tables;
        budget_exhausted |= page_text.budget_exhausted;
        // Fold presentation-form ligatures, drop soft hyphens and normalise the
        // French digit-group no-break spaces, consistently for the string
        // walker and the geometry path (the geometry path already folds
        // ligatures, so this is a no-op there for those).
        let text = text_extract::normalize_decoded_text(&page_text.text);
        let mut page_blocks: Vec<layout::DocBlock> = page_text.blocks;
        for b in &mut page_blocks {
            b.page = page_num as usize;
            b.text = text_extract::normalize_decoded_text(&b.text);
        }

        let page_media: Vec<media::MediaItem> = if options.media_mode != MediaMode::None {
            let page_bbox = page_media_box(&doc, page_id);
            let mut m = media::extract_page_media(
                &doc,
                page_id,
                page_num as usize,
                page_bbox,
                options.max_image_dimension,
            );
            if options.detect_vectors {
                m.extend(media::extract_page_vector_figures(
                    &doc,
                    page_id,
                    page_num as usize,
                    page_bbox,
                ));
            }
            m
        } else {
            Vec::new()
        };

        // Figure + caption adjacency: when this page has structured layout
        // blocks, mark the short text line attached to a content image as a
        // caption, and give the figure itself a role in the block list at its
        // reading position (right before the text that follows it).
        if options.detect_layout && !page_media.is_empty() {
            let content: Vec<&media::MediaItem> =
                page_media.iter().filter(|m| !m.decorative).collect();
            if !content.is_empty() && !page_blocks.is_empty() {
                for m in &content {
                    let my = (m.y0 + m.y1) / 2.0;
                    let best = page_blocks
                        .iter_mut()
                        .filter(|b| b.kind != "figure")
                        .min_by(|a, b2| {
                            let da = ((a.y0 + a.y1) / 2.0 - my).abs();
                            let db = ((b2.y0 + b2.y1) / 2.0 - my).abs();
                            da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
                        });
                    if let Some(b) = best {
                        let gap = (my - (b.y0 + b.y1) / 2.0).abs();
                        let size_hint = (b.y1 - b.y0).abs().max(8.0);
                        if is_caption_block(&b.text, gap, size_hint) {
                            b.kind = "caption".to_string();
                        }
                    }
                }
                // Insert figure blocks immediately before the caption line (or
                // the first text block that sits below the figure).
                let mut figures: Vec<layout::DocBlock> = content
                    .iter()
                    .map(|m| layout::DocBlock {
                        page: page_num as usize,
                        kind: "figure".to_string(),
                        x0: m.x0.min(m.x1),
                        y0: m.y0.min(m.y1),
                        x1: m.x0.max(m.x1),
                        y1: m.y0.max(m.y1),
                        text: format!("[{}]", m.kind.as_str()),
                        is_bold: false,
                        is_italic: false,
                        is_underline: false,
                    })
                    .collect();
                // Highest figure first.
                figures.sort_by(|a, b| {
                    let ay = (a.y0 + a.y1) / 2.0;
                    let by = (b.y0 + b.y1) / 2.0;
                    by.partial_cmp(&ay).unwrap_or(std::cmp::Ordering::Equal)
                });
                for fig in figures {
                    // Place after the last text block above it (reading order).
                    let insert_at = page_blocks
                        .iter()
                        .position(|b| (b.y0 + b.y1) / 2.0 <= (fig.y0 + fig.y1) / 2.0)
                        .unwrap_or(page_blocks.len());
                    page_blocks.insert(insert_at, fig);
                }
            }
        }

        // Words are counted from the text layer only, so corpus text-word
        // baselines are unaffected by image embedding.
        let words: Vec<&str> = text.split_whitespace().collect();
        total_words += words.len();
        if words.len() < options.min_words_per_page {
            pages_below_word_floor += 1;
        }
        for m in &page_media {
            media_items.push(m.clone());
        }

        let mut chunk = String::new();
        // A newly-routed page reports the marker decision from the string
        // walker (`marker_hint`), so routing does not move its page boundary.
        let heading_like = marker_hint.unwrap_or_else(|| {
            text.starts_with("# ")
                || text.lines().next().map_or(false, |l| {
                    l.len() < 60 && l.chars().all(|c| c.is_alphanumeric() || c.is_whitespace())
                })
        });
        if options.detect_headings && heading_like {
            chunk.push_str(&format!("\n## Page {}\n\n", page_num));
        }

        let mut processed_text = format_urls_and_footnotes(&text);

        if options.media_mode == MediaMode::Embed {
            // Page-relative sizing: reproduce the on-page footprint so a high-DPI
            // placement that covers only part of the page does not render at its
            // full pixel width (which is much larger than it was on the page).
            let page_bbox = page_media_box(&doc, page_id);
            let mut imgs: Vec<&media::MediaItem> = page_media
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
                    media_budget_used += b64_len;
                    inline = Some((m.data_b64.clone(), m.format.as_str()));
                } else {
                    // Over the per-document embed budget: before falling back
                    // to the omission placeholder, try progressively
                    // downscaling (and, under `vision`, JPEG-recompressing)
                    // the figure so it fits the *remaining* budget. Only this
                    // markdown copy is shrunk — `media_items`/`media` (the
                    // JSON side-channel, pushed above) keeps the full bytes.
                    let remaining =
                        options.max_media_bytes_per_doc.saturating_sub(media_budget_used);
                    if let Some((bytes, mime)) =
                        media::raster::shrink_encoded_image_to_fit(&m.data, &m.format, remaining)
                    {
                        let b64 = media::b64encode(&bytes);
                        media_budget_used += b64.len();
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
                            let (t_start, t_end) = find_table_boundaries(&processed_text, pos);
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
                                pos = processed_text[..pos]
                                    .rfind("\n\n")
                                    .map_or(0, |i| i + 2);
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

            chunk.push_str(&processed_text);
            chunk.push_str("\n\n");
            remaining_imgs.reverse();
            for img_md in remaining_imgs {
                chunk.push_str(&img_md);
            }
        } else {
            chunk.push_str(&processed_text);
            chunk.push_str("\n\n");
        }
        page_md.push((page_num, chunk));
        block_items.extend(page_blocks);
    }

    // When nothing was decoded, give an actionable reason instead of a silent
    // empty markdown:
    //  * text-show operators seen but zero words decoded AND no page failed to
    //    decode -> the text layer is unusable; glyph-encoded/outlined fonts, a
    //    missing or broken ToUnicode CMap, and a corrupt font program are all
    //    possible. This is the Type3/CID class. Return a well-formed status
    //    document plus structured rescue metadata instead of a 0-byte output,
    //    so a downstream consumer gets an unambiguous signal (the legacy exit
    //    code is preserved by the CLI, which still exits non-zero for this
    //    case). The font subtype is not inspected here, so the text must not
    //    assert a specific one;
    //  * a page whose content stream could not be decoded at all (bomb/corrupt)
    //    is a genuine hard failure and stays on the explicit-error path;
    //  * no fonts and no text-show operators at all -> scanned/image page.
    if total_words == 0 {
        if any_text_ops && !any_decode_failure {
            let status = format!(
                "# Conversion status: glyph-encoded text layer\n\n\
                 > **Vision rescue required.** This document draws text (text-show\n\
                 > operators are present) but no Unicode-mappable words were decoded.\n\
                 > The fast digital-text path cannot recover it — route it through the\n\
                 > OCR/Vision pipeline (vision-LLM rescue).\n\n\
                 <!-- pdf2md: {{\"needs_vision_rescue\":true,\"rescue_reason\":\"glyph_encoded\",\"pages\":{total_pages},\"words\":0}} -->\n"
            );
            return Ok(ConversionResult {
                markdown: status,
                total_pages,
                total_words,
                pages_below_word_floor,
                tables_detected,
                duration_us: t0.elapsed_us(),
                budget_exhausted,
                media: media_items,
                blocks: block_items,
                needs_vision_rescue: true,
                rescue_reason: Some(RescueReason::GlyphEncoded),
            });
        }
        if !any_fonts {
            return Err(
                "Document lacks a digital text layer or is scanned (images only); \
                 route through the OCR/Vision pipeline (vision-LLM rescue)."
                    .to_string(),
            );
        }
        return Err(
            "Document references fonts but no readable text was found (scanned or outlined); \
             route through the OCR/Vision pipeline (vision-LLM rescue)."
                .to_string(),
        );
    }

    // Document-level furniture pass: running headers/footers, page counters and
    // repeated multi-line letterhead blocks are removed before reflow.
    apply_furniture(options, &mut page_md, &mut block_items);
    for (p, chunk) in page_md {
        // Paragraph reflow: join the walker's one-line-per-PDF-line output
        // back into paragraphs. Run per page, after the line-based furniture
        // and page-counter passes, so a running header or a suppressed
        // counter is never welded into body prose.
        //
        // The opt-in page marker is emitted here, not inside the per-page
        // build loop: anything inserted before the furniture passes could be
        // normalized to a single cross-page key (the marker differs only by
        // page number) and silently collapsed away on every page but the
        // first. `p` is the 1-indexed page number, matching the `## Page {}`
        // heading heuristic.
        if options.page_markers {
            full_markdown.push_str(&format!("<!-- pdf2w:page n=\"{p}\" -->\n\n"));
        }
        let reflowed = reflow::reflow_markdown(&chunk);
        full_markdown.push_str(&reflowed);
    }

    let duration_us = t0.elapsed_us();

    // The C ABI/JSON surface (ffi.rs) is frozen, so the Go worker cannot see
    // the flag through `ConversionResult`; make the truncation visible on the
    // debug channel instead of failing silently.
    if budget_exhausted && std::env::var_os("PDF2MD_DEBUG").is_some() {
        eprintln!("pdf2md: a page hit an extraction work bound; output may be truncated");
    }

    Ok(ConversionResult {
        markdown: full_markdown,
        total_pages,
        total_words,
        pages_below_word_floor,
        tables_detected,
        duration_us,
        budget_exhausted,
        media: media_items,
        blocks: block_items,
        needs_vision_rescue: false,
        rescue_reason: None,
    })
}
