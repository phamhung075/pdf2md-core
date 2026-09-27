// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Reading order recovery, multi-column stream separation, and structured DocBlock generation.

use super::*;

impl InlineStyle {
    fn of(span: &Span) -> Self {
        Self {
            bold: span.is_bold,
            italic: span.is_italic,
            underline: span.is_underline,
        }
    }
}

/// Open the Markdown emphasis delimiters for `st`. Order matters for the
/// combined bold+italic case: `<u>` then `*` then `**` yields `***text***`
/// (bold-italic) wrapped in `<u>`.
pub(super) fn open_style(out: &mut String, st: InlineStyle) {
    if st.underline {
        out.push_str("<u>");
    }
    if st.italic {
        out.push('*');
    }
    if st.bold {
        out.push_str("**");
    }
}

/// Close the Markdown emphasis delimiters for `st` (reverse order of `open_style`).
///
/// A span's own text can embed a trailing space (a producer drawing the bold
/// run `"2 "` as one glyph run, rather than emitting the space as its own
/// span). CommonMark rejects a closing delimiter that is preceded by
/// whitespace — `**2 **` is *not* bold — and the orphaned `**` then consumes
/// the next `**…**` run on the line. So any whitespace sitting at the end of
/// the open run is re-emitted *after* the closing delimiters instead.
pub(super) fn close_style(out: &mut String, st: InlineStyle) {
    if st == InlineStyle::default() {
        return;
    }
    let kept = out.trim_end_matches(' ').len();
    let trailing_space = out[kept..].to_string();
    out.truncate(kept);
    if st.bold {
        out.push_str("**");
    }
    if st.italic {
        out.push('*');
    }
    if st.underline {
        out.push_str("</u>");
    }
    out.push_str(&trailing_space);
}

/// Style of the next span after `after` that carries visible text, skipping
/// empty and whitespace-only spans; `None` once the line ends. Used to keep one
/// emphasis run open across an ordinary inter-word space: `**Mistral 7B**` is
/// valid CommonMark, so the delimiters must not be closed and reopened at every
/// word.
pub(super) fn next_visible_style(line: &[Span], after: usize) -> Option<InlineStyle> {
    line.get(after + 1..)?
        .iter()
        .find(|s| !s.text.is_empty() && !s.text.chars().all(|c| c == ' '))
        .map(InlineStyle::of)
}

/// Render one visual line's spans to text with inline `**bold**` / `*italic*` /
/// `<u>underline</u>` emphasis. The spatial spacing rules are identical to the
/// legacy plain-text renderer (`render_cluster`), so a line of unstyled spans
/// produces byte-identical output; only runs whose style is non-plain gain
/// emphasis delimiters.
pub(crate) fn render_spans(line: &[Span], page_width: Option<f64>) -> String {
    let mut out = String::new();
    let mut prev_x: Option<f64> = None;
    let mut prev_word_advance = 0.0f64;
    let mut cur = InlineStyle::default();

    for (i, span) in line.iter().enumerate() {
        if span.text.is_empty() {
            continue;
        }
        let size = span.size.max(0.1);
        let is_space = span.text.chars().all(|c| c == ' ');

        if let Some(px) = prev_x {
            let gap = span.x - px;
            // `gap` is start-to-start; the *whitespace* between the two spans is
            // what it is after the previous span's own advance. Comparing the
            // raw start-to-start distance against a size threshold (as the
            // hard-break branch used to) makes any word run wider than ~2.5 em
            // look like a new column and emits a spurious newline mid-sentence.
            let gap = gap - prev_word_advance;
            if is_space {
                // A whitespace-only run whose origin sits behind the previous
                // run's right edge adds no visible whitespace. Producers draw
                // such zero-advance "spacer" spaces (often with a huge negative
                // `TJ` kern) purely to position the next run, and the spacer
                // then overlaps the following word at the same x. Treating it
                // as a separator split `Env`+`elo` at the spacer's own x; skip
                // it so the next run measures its gap against the real word end.
                if gap < -0.05 * size {
                    continue;
                }
            }
            if !is_space {
                if wide_gap_breaks(gap, size, 0.0, &line[..i], &line[i..], page_width) {
                    // A distinct column / element on the same row: break the
                    // line AND close any open emphasis first.
                    close_style(&mut out, cur);
                    cur = InlineStyle::default();
                    if !out.is_empty() && !out.ends_with('\n') {
                        out.push('\n');
                    }
                } else if gap > WORD_GAP_EM * size {
                    // Ordinary inter-word space. A space *inside* emphasis is
                    // valid CommonMark (`**Mistral 7B**`), so if the styled run
                    // simply continues with the same non-plain style, keep `cur`
                    // open and emit the space inside the delimiters. Only close
                    // (and emit the space outside) when the run ends or changes.
                    if cur == InlineStyle::default() || InlineStyle::of(span) != cur {
                        close_style(&mut out, cur);
                        cur = InlineStyle::default();
                    }
                    if !out.is_empty() && !out.ends_with(' ') && !out.ends_with('\n') {
                        out.push(' ');
                    }
                }
            }
        }

        if is_space {
            // A space glyph is never emphasized on its own, but it may sit in
            // the *interior* of an emphasis run when the next visible span keeps
            // the same non-plain style (`**Mistral 7B**` is valid CommonMark).
            // Only close the style when the run ends or changes; otherwise keep
            // it open and emit the space inside the delimiters.
            if !out.is_empty() && !out.ends_with(' ') && !out.ends_with('\n') {
                let continues =
                    cur != InlineStyle::default() && next_visible_style(line, i) == Some(cur);
                if !continues {
                    close_style(&mut out, cur);
                    cur = InlineStyle::default();
                }
                out.push(' ');
            }
        } else {
            let st = InlineStyle::of(span);
            if st != cur {
                // Opening a new delimiter. A span's own text can also *begin*
                // with a space (a producer run like `" www.caf.fr"`); an
                // opening delimiter followed by whitespace (`** x**`) is not
                // emphasis either, so flush that leading space outside the
                // delimiter. `close_style` already moved any trailing space of
                // the previous run outside its closing delimiter.
                let lead = span.text.len() - span.text.trim_start_matches(' ').len();
                close_style(&mut out, cur);
                if lead > 0 && !out.is_empty() && !out.ends_with(' ') && !out.ends_with('\n') {
                    out.push(' ');
                }
                open_style(&mut out, st);
                cur = st;
                out.push_str(&span.text[lead..]);
            } else {
                // Same style stays open: the whole run is emphasized and any
                // embedded whitespace is interior, so it is kept verbatim.
                out.push_str(&span.text);
            }
        }

        prev_x = Some(span.x);
        prev_word_advance = span.word_advance;
    }

    close_style(&mut out, cur);
    out.trim_end().to_string()
}

/// Render pre-built visual lines to plain text (block/paragraph separation +
/// word gaps). Line model must come from `build_lines`.
pub fn render_cluster(lines: &[Vec<Span>], page_width: Option<f64>) -> String {
    if lines.is_empty() {
        return String::new();
    }

    let body_size = body_size_for(lines);
    let mut list_state = ListRunState::default();
    let mut out = String::new();
    let mut prev_line_y: Option<f64> = None;

    for line in lines {
        push_line(&mut out, line, &mut prev_line_y, &mut list_state, body_size, page_width);
    }

    out.trim_end().to_string()
}

/// Render page text in human reading order. When the page is a single column
/// and nothing was removed, output equals `render_cluster` byte-for-byte.
pub fn render_human_order(lines: &[Vec<Span>], page_height: f64, drop_furniture: bool, page_width: Option<f64>) -> String {
    let streams = page_read_order(lines);
    if streams.len() == 1 {
        // Single column: identical to the plain renderer unless we strip
        // furniture lines (page numbers).
        if !drop_furniture {
            return render_cluster(lines, page_width);
        }
        let keep: Vec<Vec<Span>> = lines
            .iter()
            .filter(|l| !is_page_number_line(l, page_height))
            .cloned()
            .collect();
        return render_cluster(&keep, page_width);
    }
    let body_size = body_size_for(lines);
    let mut out = String::new();
    for (ci, stream) in streams.iter().enumerate() {
        if ci > 0 && !out.is_empty() {
            out.push('\n');
        }
        let mut prev_y: Option<f64> = None;
        let mut list_state = ListRunState::default();
        for line in stream {
            if drop_furniture && is_page_number_line(line, page_height) {
                continue;
            }
            push_line(&mut out, line, &mut prev_y, &mut list_state, body_size, page_width);
        }
    }
    out.trim_end().to_string()
}
