// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Paragraph reflow for the emitted Markdown.
//!
//! The string walker (and, in places, the structured layout engine) emits one
//! output line per *visual* PDF line. A wrapped sentence therefore arrives as
//! several Markdown lines that a downstream consumer reads as separate
//! paragraphs, e.g. `est de 36,02` / `euros par jour` / `, avant application`.
//! [`reflow_markdown`] joins those continuation lines back into one paragraph.
//!
//! It is deliberately conservative and purely line-oriented:
//!
//! * structural lines (headings, blockquotes, images, table rows, fenced code)
//!   are never touched and act as hard boundaries; a list item is the one
//!   exception — it absorbs a wrapped continuation whose wrap point landed on a
//!   comma — and a wrapped heading is merged back into one;
//! * a paragraph break (blank line, or a line that ends with `:`) is preserved,
//!   except that a lowercase clause ending in `:` is a wrapped line, not a
//!   label, and joins the line above;
//! * two plain body lines join only when the earlier one has no sentence
//!   terminator and the later one continues it (starts lowercase, with
//!   `, ; . )`, or after a trailing comma); the first *visible* character is
//!   tested, so a line opening with `**`/`<u>` is classified by its text, and
//!   two lines wrapped in the same emphasis merge into one run;
//! * a trailing line-break hyphen is removed before a lowercase *fragment*
//!   (not a French clitic / compound tail) when either the joined word occurs
//!   elsewhere in the document as a standalone word (`infor-` + `mation`
//!   collapses when `information` is attested) **or** the chunk's own evidence
//!   says the two pieces are fragments rather than a compound (no half is an
//!   attested content word, the hyphenated form is not written mid-line, the
//!   prefix is not a productive compound prefix, and the wrap is not inside a
//!   multi-part hyphenated token). `peut-être`, `c'est-à-dire`,
//!   `non-professionnel`, `sous-total`, `ci-dessus` and `porte-monnaie` always
//!   survive;
//! * a space is kept before `, ; . )` only when the source already carries the
//!   whitespace after it, so two numbers are never fused across a join.

mod hyphen;

pub(crate) use hyphen::{
    classify_hyphen_join, dehyphenated_word, is_unattested_fragment, JoinEvidence, HyphenJoin,
};

/// Line prefixes that always mark a structural (non-paragraph) line.
const STRUCTURAL_STARTS: &[&str] = &["#", "- ", "* ", "> ", "<", "![", "|", "*["];

/// Whole-line inline wrappers the renderer emits, longest opener first so
/// `**bold**` is not read as an italic `*` wrapping `*bold*`. Used to merge two
/// consecutive wrapped lines into a single run of the *same* emphasis
/// (`**a**` + `**b**` -> `**a b**`, not `**a** **b**`).
const WHOLE_LINE_WRAPPERS: &[(&str, &str)] = &[("**", "**"), ("<u>", "</u>"), ("*", "*")];

/// True when the whitespace-stripped line opens a structural Markdown element.
fn is_structural_start(t: &str) -> bool {
    STRUCTURAL_STARTS.iter().any(|p| t.starts_with(p))
}

/// Split an ATX heading line into its `#` marker and its text, or `None` when
/// the line is not a heading (`#hashtag`, a bare `#`, or plain text). The
/// marker and text are returned as borrowed slices of `s`.
fn split_heading(s: &str) -> Option<(&str, &str)> {
    let t = s.trim_end();
    let marker_len = t.len() - t.trim_start_matches('#').len();
    if marker_len == 0 {
        return None;
    }
    let after = &t[marker_len..];
    let text = after.strip_prefix(' ')?;
    let text = text.trim_start();
    if text.is_empty() {
        return None;
    }
    Some((&t[..marker_len], text))
}

/// A heading whose text opens with the word `Page`. The converter's
/// `## Page N` marker matches this, and the private-corpus page-marker count
/// also counts a document heading that begins with `Page`; either way such a
/// heading is a page boundary, never a wrapped continuation of the heading
/// above, so it must not be merged.
fn is_page_marker_heading(text: &str) -> bool {
    text == "Page" || text.starts_with("Page ")
}

/// Whether the heading lines `prev` and `next` are one heading wrapped onto a
/// second visual line rather than two separate headings.
///
/// A wrapped heading leaves its first line without sentence punctuation (a
/// section label that ends a phrase would have been ended by its own period or
/// colon); a genuinely separate heading is either preceded by body text or
/// ends a sentence, so the two are never adjacent same-level ATX headings with
/// the first unpunctuated. The `#` marker the renderer inserts between the two
/// visual lines is otherwise a foreign token inside the wrapped title.
fn headings_join(prev: &str, next: &str) -> bool {
    match (split_heading(prev), split_heading(next)) {
        (Some((pl, pt)), Some((nl, nt))) => {
            pl == nl
                && !pt.is_empty()
                && !nt.is_empty()
                && !ends_sentence(pt)
                && !is_page_marker_heading(pt)
                && !is_page_marker_heading(nt)
        }
        _ => false,
    }
}

/// True for an ordered-list marker (`N. `) at the start of the line.
fn has_ordered_list_marker(line: &str) -> bool {
    let t = line.trim_start();
    let mut digits = 0usize;
    let mut chars = t.chars().peekable();
    while let Some(c) = chars.peek() {
        if c.is_ascii_digit() {
            digits += 1;
            chars.next();
        } else {
            break;
        }
    }
    if digits == 0 {
        return false;
    }
    chars.next() == Some('.') && chars.peek().map_or(false, |c| c.is_whitespace())
}

/// True for an unordered (`- `/`* `) or ordered (`N. `) list-item line at the
/// start of the line. A list item may absorb one wrapped continuation (see
/// `reflow_markdown`), unlike the other structural prefixes.
fn is_list_start(line: &str) -> bool {
    let t = line.trim_start();
    t.starts_with("- ") || t.starts_with("* ") || has_ordered_list_marker(line)
}

/// Number of maximal runs of at least three spaces in `line`.
fn wide_space_runs(line: &str) -> usize {
    let mut runs = 0usize;
    let mut cur = 0usize;
    for c in line.chars() {
        if c == ' ' {
            cur += 1;
        } else {
            if cur >= 3 {
                runs += 1;
            }
            cur = 0;
        }
    }
    if cur >= 3 {
        runs += 1;
    }
    runs
}

/// A plain body line: content that a paragraph may be built from. Structural
/// lines, label/`:` lines and form rows with wide gaps are excluded. A line
/// that is a single whole-line `<u>…</u>` run is body text with underline
/// emphasis, not an HTML block, so it is *not* treated as structural.
fn is_plain_body(line: &str) -> bool {
    let t = line.trim();
    if t.is_empty() || has_ordered_list_marker(line) {
        return false;
    }
    let underlined_run = whole_line_wrapper(t) == Some(("<u>", "</u>"));
    if (is_structural_start(t) && !underlined_run) || t.ends_with(':') {
        return false;
    }
    wide_space_runs(line) < 2
}

/// Whether `t` ends with a sentence terminator, ignoring any trailing
/// emphasis/underline markup so a terminator inside `**...**` still counts.
fn ends_sentence(t: &str) -> bool {
    matches!(
        trim_trailing_markup(t).chars().last(),
        Some('.' | '!' | '?' | ':' | ';' | '»')
    )
}

/// `s` with any trailing `**`/`*`/`</u>` markup removed, so the last *visible*
/// character can be tested.
fn trim_trailing_markup(s: &str) -> &str {
    let mut t = s.trim_end();
    loop {
        if let Some(r) = t.strip_suffix("</u>") {
            t = r.trim_end();
            continue;
        }
        let stars = t.len() - t.trim_end_matches('*').len();
        if stars > 0 {
            t = t[..t.len() - stars].trim_end();
            continue;
        }
        break;
    }
    t
}

/// The first *visible* character of `s`, skipping any leading emphasis or
/// underline markup the renderer put in front of it (`**en` -> `e`,
/// `<u>For` -> `F`). A wrapped continuation that opens with `**` or `<u>` is
/// therefore tested by the character the reader actually sees.
fn first_visible_char(s: &str) -> Option<char> {
    let mut rest = s.trim_start();
    loop {
        if let Some(r) = rest.strip_prefix("<u>") {
            rest = r;
            continue;
        }
        let stars = rest.len() - rest.trim_start_matches('*').len();
        if stars > 0 {
            rest = &rest[stars..];
            continue;
        }
        break;
    }
    rest.chars().next()
}

/// The single whole-line emphasis/underline wrapper spanning `s`, if any.
///
/// Ambiguous shapes are rejected: the closer must be unique to the outer edges
/// (`<u>a</u> <u>b</u>` is two runs, not one wrapper), and a star wrapper may
/// not open onto another star (`***x***` is not a `**` wrapper).
fn whole_line_wrapper(s: &str) -> Option<(&'static str, &'static str)> {
    let t = s.trim();
    for &(open, close) in WHOLE_LINE_WRAPPERS {
        let Some(inner) = t.strip_prefix(open).and_then(|r| r.strip_suffix(close)) else {
            continue;
        };
        if inner.is_empty() || inner.contains(close) {
            continue;
        }
        if open.starts_with('*') && (inner.starts_with('*') || inner.ends_with('*')) {
            continue;
        }
        return Some((open, close));
    }
    None
}

/// Drop the space before an opening `, ; . )` only when the source already has
/// whitespace after that punctuation. This keeps `par jour` + `, avant` as
/// `par jour, avant` while refusing to fuse `12` + `.50` into `12.50`.
fn punct_join_safe(ns: &str) -> bool {
    let mut it = ns.chars();
    match it.next() {
        Some(c) if matches!(c, ',' | ';' | '.' | ')') => {
            it.next().map_or(false, |n| n.is_whitespace())
        }
        _ => false,
    }
}

/// Reflow `input`: join wrapped body lines into paragraphs.
///
/// Pure and allocation-bounded: the output is at most the input plus one space
/// per join, and every decision is a single linear scan.
pub fn reflow_markdown(input: &str) -> String {
    let lines: Vec<&str> = input.split('\n').collect();

    // Chunk attestation: a line-end hyphen is dropped when the joined word
    // appears elsewhere as a standalone word, or when the pieces are attested
    // as fragments rather than a compound (see `is_unattested_fragment`).
    let evidence = JoinEvidence::from_text(lines.iter().copied());

    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let mut pending: Option<String> = None;
    // True when `pending` is a list item, which only continues across a wrap
    // that lands on a comma (see the join test below).
    let mut pending_list = false;
    let mut last = String::new();
    let mut fence = false;

    for &ln in &lines {
        let trimmed = ln.trim();

        // Headings / page markers: hard boundary, never touched — except that
        // a title/heading wrapped onto a second visual line arrives as two
        // same-level headings, and the `#` inserted between the wrapped words
        // is a foreign token inside the title. Merge that continuation back
        // into the single heading it was; a separate heading is never an
        // adjacent same-level heading whose first line is unpunctuated.
        if ln.starts_with('#') {
            if let Some(p) = pending.take() {
                out.push(p);
            }
            pending_list = false;
            if let Some(prev) = out.last() {
                if headings_join(prev, ln) {
                    let text = split_heading(ln).expect("headings_join checked").1;
                    let prev = out.last_mut().expect("checked above");
                    prev.push(' ');
                    prev.push_str(text);
                    continue;
                }
            }
            out.push(ln.to_string());
            continue;
        }

        let is_fence = trimmed.starts_with("```") || trimmed.starts_with("~~~");
        if is_fence {
            if let Some(p) = pending.take() {
                out.push(p);
            }
            pending_list = false;
            out.push(ln.to_string());
            fence = !fence;
            continue;
        }
        if fence {
            out.push(ln.to_string());
            continue;
        }

        if trimmed.is_empty() {
            if let Some(p) = pending.take() {
                out.push(p);
            }
            pending_list = false;
            out.push(ln.to_string());
            continue;
        }

        // A list item may be continued by a wrapped fragment; every other
        // structural line is a hard boundary. A line ending in `:` that opens
        // lowercase is a wrapped clause whose colon fell at the wrap point, not
        // a standalone label, so it too may join the paragraph above.
        let list_line = is_list_start(ln);
        let colon_continuation = !is_structural_start(trimmed)
            && !list_line
            && trimmed.ends_with(':')
            && wide_space_runs(ln) < 2
            && first_visible_char(trimmed).is_some_and(|c| c.is_lowercase());
        if !is_plain_body(ln) && !list_line && !colon_continuation {
            if let Some(p) = pending.take() {
                out.push(p);
            }
            pending_list = false;
            out.push(ln.to_string());
            continue;
        }

        if pending.is_none() {
            pending = Some(ln.to_string());
            pending_list = list_line;
            last.clear();
            last.push_str(ln);
            continue;
        }

        let ns = ln.trim_start();
        let first = first_visible_char(ns);
        let a_term = ends_sentence(&last);
        // A comma at a wrap point continues a comma-separated run even when the
        // next visual line opens on an uppercase word (`…, SIQA,\nOpenbookQA…`).
        let comma_continuation = trim_trailing_markup(&last).ends_with(',') && !a_term;
        // A line that already ends a sentence is never a wrapped continuation,
        // however long it is: a following lowercase line starts a new
        // paragraph. Only a leading `, ; . )` (the continuation punctuation
        // itself), a lowercase opener, or a trailing comma continues it. A new
        // list item always starts its own line, and a wrapped list fragment
        // only continues after the comma case above.
        let join = !list_line
            && match first {
                Some(c) if matches!(c, ',' | ';' | '.' | ')') => true,
                Some(c) if c.is_lowercase() && !a_term && !pending_list => true,
                _ => comma_continuation,
            };

        if join {
            let buf = pending.as_mut().expect("pending is set");
            match whole_line_wrapper(buf).filter(|w| whole_line_wrapper(ln) == Some(*w)) {
                Some((open, close)) => {
                    // Both lines carry the same whole-line emphasis: merge them
                    // into one run rather than leaving `**a** **b**`.
                    let prev = buf.trim();
                    let next = ln.trim();
                    let prev_inner = prev
                        .strip_prefix(open)
                        .and_then(|r| r.strip_suffix(close))
                        .unwrap_or(prev)
                        .trim();
                    let next_inner = next
                        .strip_prefix(open)
                        .and_then(|r| r.strip_suffix(close))
                        .unwrap_or(next)
                        .trim();
                    *buf = format!("{open}{prev_inner} {next_inner}{close}");
                }
                None => match classify_hyphen_join(buf, ln) {
                    HyphenJoin::Dehyphenate
                        if evidence.attests(&dehyphenated_word(buf, ln))
                            || is_unattested_fragment(buf, ln, &evidence) =>
                    {
                        let n = buf.trim_end().len();
                        buf.truncate(n - 1); // '-' is one byte
                        buf.push_str(ns);
                    }
                    // A real compound / clitic, or a wrap whose pieces are not
                    // evidenced as fragments: keep the hyphen.
                    HyphenJoin::Dehyphenate | HyphenJoin::KeepHyphen => {
                        let n = buf.trim_end().len();
                        buf.truncate(n);
                        buf.push_str(ns);
                    }
                    HyphenJoin::None => {
                        let n = buf.trim_end().len();
                        buf.truncate(n);
                        if punct_join_safe(ns) {
                            buf.push_str(ns);
                        } else {
                            buf.push(' ');
                            buf.push_str(ns);
                        }
                    }
                },
            }
            last.clear();
            last.push_str(ln);
        } else {
            if let Some(p) = pending.take() {
                out.push(p);
            }
            pending = Some(ln.to_string());
            pending_list = list_line;
            last.clear();
            last.push_str(ln);
        }
    }
    if let Some(p) = pending.take() {
        out.push(p);
    }
    out.join("\n")
}

#[cfg(test)]
mod tests;
