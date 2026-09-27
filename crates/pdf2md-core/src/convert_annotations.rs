// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Document-level annotations `convert` appends to the emitted markdown.
//! Kept apart from `convert.rs` so the conversion control flow stays under the
//! Rule 07 file limit while these user-visible strings have one source.

/// Status document returned when text-show operators were seen but no word
/// decoded (a glyph-encoded/outlined text layer). It carries a machine-readable
/// `needs_vision_rescue` comment in the same shape as the partial-loss marker.
pub(crate) fn glyph_encoded_status(total_pages: usize) -> String {
    format!(
        "# Conversion status: glyph-encoded text layer\n\n\
         > **Vision rescue required.** This document draws text (text-show\n\
         > operators are present) but no Unicode-mappable words were decoded.\n\
         > The fast digital-text path cannot recover it — route it through the\n\
         > OCR/Vision pipeline (vision-LLM rescue).\n\n\
         <!-- pdf2md: {{\"needs_vision_rescue\":true,\"rescue_reason\":\"glyph_encoded\",\"pages\":{total_pages},\"words\":0}} -->\n"
    )
}

/// Append the partial-glyph-loss marker when some glyph codes were shown under
/// a font with no Unicode codec **and** the document still produced words.
///
/// The `total_words > 0` half is deliberate: a zero-word document already takes
/// the glyph-encoded status path (and `needs_vision_rescue`), so this marker
/// covers exactly the *silent* case the status path misses. With no undecodable
/// glyphs nothing is appended, so an ordinary document stays byte-identical.
pub(crate) fn append_glyph_loss_comment(
    markdown: &mut String,
    undecodable_glyphs: usize,
    decoded_glyphs: usize,
    total_words: usize,
) {
    if undecodable_glyphs == 0 || total_words == 0 {
        return;
    }
    if !markdown.ends_with('\n') {
        markdown.push('\n');
    }
    markdown.push_str(&format!(
        "<!-- pdf2md: {{\"undecodable_glyphs\":{undecodable_glyphs},\"decoded_glyphs\":{decoded_glyphs}}} -->\n"
    ));
}
