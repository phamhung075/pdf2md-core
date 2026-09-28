// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Reading order recovery, multi-column stream separation, and structured DocBlock generation.

use super::*;

impl ListRunState {
    fn end_run(&mut self) {
        self.base_x = None;
        self.counters.clear();
    }

    /// Depth 0 is the first list item's own indent; deeper items are bucketed
    /// in units of `1.5 * body_size` from that anchor (mirrors
    /// `layout::semantic::detect_list_item`'s indent-to-depth formula).
    fn depth_for(&mut self, x: f64, body_size: f64) -> u8 {
        let base = *self.base_x.get_or_insert(x);
        let raw = (x - base) / (1.5 * body_size.max(1.0));
        raw.round().clamp(0.0, 4.0) as u8
    }

    /// Next ordinal for an ordered item at `depth`; resets any deeper
    /// counters (a new depth-0 item restarts nested numbering underneath it).
    fn next_ordinal(&mut self, depth: u8) -> usize {
        let d = depth as usize;
        if self.counters.len() <= d {
            self.counters.resize(d + 1, 0);
        }
        for c in self.counters.iter_mut().skip(d + 1) {
            *c = 0;
        }
        self.counters[d] += 1;
        self.counters[d]
    }

    /// Raise the counter at `depth` to `ordinal` when the source marker runs
    /// ahead of it, so the next item continues from the author's numbering.
    ///
    /// The counter alone restarts at 1 every time a `Body` line ends a list
    /// run, which silently renumbered an author-numbered document's sections
    /// (`2.`, `3.`, `4.` …) into a wall of `1.`. Adopting the marker only when
    /// it is *ahead* keeps the auto-numbered-producer case working: a source
    /// that repeats the same `1.` on every item never exceeds the counter, so
    /// it still numbers consecutively.
    fn adopt_ordinal(&mut self, depth: u8, ordinal: usize) {
        let d = depth as usize;
        if self.counters.len() <= d {
            self.counters.resize(d + 1, 0);
        }
        if ordinal > self.counters[d] {
            self.counters[d] = ordinal;
        }
    }
}

/// A heading is a short label, not a sentence. At or below this many
/// characters a trailing full stop is tolerated (e.g. `Résumé.`); above it the
/// line is a complete sentence and is never promoted, at any font size.
const LONG_SENTENCE_HEADING_MAX_CHARS: usize = 50;

/// Maximum baseline pitch, in multiples of the candidate's own font size, for a
/// heading to count as sitting "directly under" the previous heading. One
/// single-spaced line of leading is ~1.2x its size; 1.6x leaves room for metric
/// jitter while still excluding a heading separated by body text or a blank
/// line.
const BILINGUAL_MAX_PITCH_EM: f64 = 1.6;

/// Character-weighted average font size of a line's visible spans.
fn weighted_avg_size(sized: &[&Span]) -> f64 {
    let total: f64 = sized.iter().map(|s| s.text.chars().count().max(1) as f64).sum();
    sized
        .iter()
        .map(|s| s.size * s.text.chars().count().max(1) as f64)
        .sum::<f64>()
        / total.max(1.0)
}

/// Whether the majority of a line's visible spans are bold.
fn line_is_bold(sized: &[&Span]) -> bool {
    sized.iter().filter(|s| s.is_bold).count() * 2 > sized.len()
}

/// Statistical 3-level heading detector — see this section's module-level
/// doc comment for provenance. Returns `None` for anything that doesn't look
/// like a heading, including prose that merely happens to be short or bold.
pub(super) fn detect_heading_level(line: &[Span], body_size: f64) -> Option<u8> {
    let sized: Vec<&Span> = line.iter().filter(|s| !s.text.trim().is_empty()).collect();
    if sized.is_empty() {
        return None;
    }
    let plain: String = sized.iter().map(|s| s.text.as_str()).collect();
    let trimmed = plain.trim();
    if trimmed.is_empty() || trimmed.chars().count() > 180 {
        return None;
    }
    // Trailing full stop with no letters at all, or two-plus sentence
    // breaks, reads as prose rather than a heading.
    if trimmed.ends_with('.') && !trimmed.contains(|c: char| c.is_ascii_alphabetic()) {
        return None;
    }
    if trimmed.matches(". ").count() >= 2 {
        return None;
    }

    let avg_fs = weighted_avg_size(&sized);
    let is_bold = line_is_bold(&sized);

    // Lowercase continuation guard: a heading in a Latin-script document starts
    // with an uppercase letter or a digit. A line opening with lowercase text
    // is a wrapped/indented continuation of the sentence above it (e.g. an
    // emphasis run like "cliquez sur « Connexion »" inside a list item) and must
    // never be promoted to H2/H3 just because it is bold or slightly larger.
    // H1 (>= 1.4x body) is left untouched: a genuinely large lowercase title is
    // rare, and the guard is scoped below that ratio per the classifier's ranges.
    if avg_fs < 1.4 * body_size {
        if let Some(first) = trimmed.chars().find(|c| c.is_alphabetic()) {
            if first.is_lowercase() {
                return None;
            }
        }
    }
    // Long complete-sentence guard: a line over half a typical body line and
    // ending in a full stop is a sentence, not a section heading, at *any*
    // size. Section headings are short labels; a heading-sized full sentence is
    // the page's opening line, not its title.
    if trimmed.ends_with('.') && trimmed.chars().count() > LONG_SENTENCE_HEADING_MAX_CHARS {
        return None;
    }

    if avg_fs >= 1.6 * body_size || (avg_fs >= 1.4 * body_size && is_bold) {
        return Some(1);
    }
    if avg_fs >= 1.3 * body_size {
        return Some(2);
    }
    if avg_fs >= 1.15 * body_size && is_bold {
        return Some(3);
    }
    None
}

/// Detects a leading list marker (bullet, checkbox, or ordered) on `line`'s
/// first non-space span and returns `(ordered, spans_to_skip)` — how many
/// leading spans are the marker itself (plus a following pure-space span),
/// to be sliced off before rendering the item's own text. A checkbox marker
/// (`[ ]`/`[x]`) is kept as part of the item text (GFM task-list syntax is
/// `- [ ] text`, not a separate marker), so it reports 0 spans to skip.
pub(super) fn detect_list_marker(line: &[Span]) -> Option<(bool, usize)> {
    let mut idx = 0;
    while idx < line.len() && line[idx].text.trim().is_empty() {
        idx += 1;
    }
    let w0 = line.get(idx)?.text.trim();
    if w0.is_empty() {
        return None;
    }

    let is_checkbox = w0 == "[ ]" || w0.eq_ignore_ascii_case("[x]");
    if is_checkbox {
        // Require item text after the checkbox; a lone checkbox is noise.
        return if idx + 1 < line.len() { Some((false, idx)) } else { None };
    }

    let is_bullet = matches!(w0, "-" | "*" | "•" | "●" | "+" | "◦" | "▪");
    let is_ordered = if (w0.ends_with('.') || w0.ends_with(')')) && w0.len() <= 5 {
        let digits = &w0[..w0.len() - 1];
        !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit())
    } else if (w0.starts_with('(') && w0.ends_with(')')) || (w0.starts_with('[') && w0.ends_with(']')) {
        let inner = &w0[1..w0.len().saturating_sub(1)];
        !inner.is_empty() && inner.chars().all(|c| c.is_ascii_digit())
    } else {
        false
    };
    if !is_bullet && !is_ordered {
        return None;
    }

    let marker = &line[idx];
    let mut skip = idx + 1;
    // A marker is separated from its item text by whitespace: either the
    // marker span itself carries a trailing space, or the next span is a
    // pure-space span. `"20."` immediately followed by `"0"` — the integer and
    // fractional parts of a decimal number kerned into two spans — has
    // neither, and is a value, not an ordered-list marker.
    let mut separated = marker.text.ends_with(|c: char| c.is_whitespace());
    if skip < line.len() && line[skip].text.chars().all(|c| c == ' ') {
        separated = true;
        skip += 1;
    }
    if skip >= line.len() {
        return None; // marker with no item text
    }
    if !separated {
        // No explicit space span: require a real horizontal gap. Kerning a
        // decimal apart places the two spans flush (~0 pt apart), while a
        // genuine list space leaves roughly a quarter-em or more. The same
        // applies to a bullet: a negative amount reaches this layer as glyph
        // runs, so `-218,48` is the lone marker-shaped span `-` followed
        // flush by the digits. Without a gap it is a sign, not a Markdown
        // bullet — treating it as one strips the minus and shifts the value
        // into a list, which silently turns every credit-note amount positive.
        let gap = line[skip].x - (marker.x + marker.advance);
        if gap <= 0.1 * marker.size.max(1.0) {
            return None;
        }
    }
    Some((is_ordered, skip))
}

/// The explicit index carried by a *bracketed* ordered marker (`[3]`), or
/// `None` for any other marker shape.
///
/// A bracketed numeric marker is an author-supplied citation index, not an
/// auto-numbered list bullet. Its number must be rendered verbatim rather than
/// re-derived from `ListRunState`: the run counter restarts at 1 every time a
/// wrapped continuation line (a `Body` line) ends the Markdown list run between
/// two markers, so a reference list `[1] [2] [3] …` collapsed to `1. 1. 1. …`
/// and the citations lost their identity. `[N]` is unambiguous — the digits
/// are the index — so prefer them. Dot/paren markers (`1.`, `1)`) are not
/// always authoritative (some producers emit the same number on every item),
/// so `explicit_dot_ordinal` is only adopted when it runs ahead of the counter.
pub(super) fn explicit_bracketed_ordinal(line: &[Span]) -> Option<usize> {
    let marker = line.iter().find(|s| !s.text.trim().is_empty())?.text.trim();
    let inner = marker.strip_prefix('[')?.strip_suffix(']')?;
    if inner.is_empty() || !inner.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    inner.parse().ok()
}

/// The numeric value carried by a *dot/paren* ordered marker (`2.`, `2)`,
/// `(2)`), or `None` for any other marker shape.
///
/// Unlike a bracketed citation index this is not always authoritative — some
/// producers stamp the same `1.` on every item — so the caller only adopts it
/// when it runs ahead of the synthetic counter (see `ListRunState::adopt_ordinal`).
pub(super) fn explicit_dot_ordinal(line: &[Span]) -> Option<usize> {
    let marker = line.iter().find(|s| !s.text.trim().is_empty())?.text.trim();
    let digits = if marker.starts_with('(') && marker.ends_with(')') && marker.len() <= 5 {
        &marker[1..marker.len().saturating_sub(1)]
    } else if (marker.ends_with('.') || marker.ends_with(')')) && marker.len() <= 5 {
        &marker[..marker.len() - 1]
    } else {
        return None;
    };
    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

/// Build the heading geometry remembered for the next visual line.
fn heading_line_of(line: &[Span], level: u8, size: f64) -> HeadingLine {
    let x0 = line.iter().map(|s| s.x).fold(f64::INFINITY, f64::min);
    let x1 = line
        .iter()
        .map(|s| s.x + s.advance)
        .fold(f64::NEG_INFINITY, f64::max);
    HeadingLine { level, size, x0, x1, y: line[0].y }
}

/// True when `line` is the translation half of a bilingual heading pair: it
/// sits directly under the heading above (`prev`), shares its horizontal
/// extent, and is typographically subordinate — a deeper level, not bold, or a
/// smaller size than the heading it translates. Such a line is rendered as
/// plain text so the source heading is not duplicated at a second level.
fn is_bilingual_translation(
    prev: &HeadingLine,
    line: &[Span],
    level: u8,
    size: f64,
    bold: bool,
) -> bool {
    if line.is_empty() {
        return false;
    }
    let pitch = prev.y - line[0].y;
    if !(0.0..=BILINGUAL_MAX_PITCH_EM * size).contains(&pitch) {
        return false;
    }
    let x0 = line.iter().map(|s| s.x).fold(f64::INFINITY, f64::min);
    let x1 = line
        .iter()
        .map(|s| s.x + s.advance)
        .fold(f64::NEG_INFINITY, f64::max);
    if x1 < prev.x0 || x0 > prev.x1 {
        return false;
    }
    level > prev.level || !bold || size < prev.size
}

/// Classifies one visual line, threading `list_state` across consecutive
/// calls in one render pass. Returns the role plus the span slice the caller
/// should actually render as the line's text — the marker sliced off for a
/// list item (the caller prefixes the Markdown bullet/number itself instead),
/// the full line otherwise.
pub(crate) fn classify_line<'a>(
    line: &'a [Span],
    body_size: f64,
    list_state: &mut ListRunState,
) -> (LineRole, &'a [Span]) {
    if line.is_empty() {
        return (LineRole::Body, line);
    }
    let sized: Vec<&Span> = line.iter().filter(|s| !s.text.trim().is_empty()).collect();
    if let Some(level) = detect_heading_level(line, body_size) {
        let size = sized.iter().map(|s| s.size).fold(0.0f64, f64::max).max(0.1);
        let bold = line_is_bold(&sized);
        let demote = list_state
            .last_heading
            .is_some_and(|p| is_bilingual_translation(&p, line, level, size, bold));
        list_state.end_run();
        if demote {
            list_state.last_heading = None;
            return (LineRole::Body, line);
        }
        list_state.last_heading = Some(heading_line_of(line, level, size));
        return (LineRole::Heading(level), line);
    }
    if let Some((ordered, skip)) = detect_list_marker(line) {
        list_state.last_heading = None;
        let depth = list_state.depth_for(line[0].x, body_size);
        // Always advance the run counter so a following dot/paren marker keeps
        // counting on from here, but let an explicit `[N]` citation index win,
        // and let a dot/paren marker's own number win whenever it runs ahead of
        // the counter (see `adopt_ordinal`).
        let ordinal = list_state.next_ordinal(depth);
        let ordinal = if ordered {
            if let Some(n) = explicit_bracketed_ordinal(line) {
                n
            } else if let Some(n) = explicit_dot_ordinal(line).filter(|&n| n > ordinal) {
                list_state.adopt_ordinal(depth, n);
                n
            } else {
                ordinal
            }
        } else {
            ordinal
        };
        return (LineRole::List { depth, ordered, ordinal }, &line[skip..]);
    }
    list_state.end_run();
    list_state.last_heading = None;
    (LineRole::Body, line)
}

/// Strips one or more layers of outer `**`/`*`/`<u></u>` wrapping from an
/// already-rendered line. Headings render as clean text rather than
/// redundantly double-marking a `## **Heading**` when the source line
/// happened to be entirely bold — exactly the common case, since bold is one
/// of `detect_heading_level`'s own signals (the H3 threshold requires it).
pub(super) fn strip_outer_emphasis(s: &str) -> String {
    let mut t = s.trim();
    loop {
        // A prefix/suffix pair only represents ONE wrapper spanning the whole
        // string if the closing marker doesn't recur inside it. Without this
        // check, two independently-styled runs concatenated with a space —
        // e.g. `<u>Date</u> <u>:</u>` — look exactly like one outer `<u>...
        // </u>` wrap (prefix "<u>" + suffix "</u>"), so naively stripping
        // them removed the FIRST run's own `<u>` and the SECOND run's own
        // `</u>`, leaving the middle `</u> <u>` pair dangling unmatched
        // (`Date</u> <u>:` — invalid Markdown/HTML). Bail out instead of
        // guessing when the marker isn't unique to the true outer edges.
        if let Some(inner) = t.strip_prefix("<u>").and_then(|r| r.strip_suffix("</u>")) {
            if inner.contains("</u>") {
                break;
            }
            t = inner.trim();
            continue;
        }
        if let Some(inner) = t.strip_prefix("**").and_then(|r| r.strip_suffix("**")) {
            if inner.contains("**") {
                break;
            }
            t = inner.trim();
            continue;
        }
        if let Some(inner) = t.strip_prefix('*').and_then(|r| r.strip_suffix('*')) {
            if inner.contains('*') {
                break;
            }
            t = inner.trim();
            continue;
        }
        break;
    }
    t.to_string()
}

/// Formats a classified line's already-rendered inline text with its
/// structural Markdown prefix. `inline_text` must come from rendering
/// `classify_line`'s returned span slice (the marker-stripped remainder for
/// a list item), not the original full line.
pub(crate) fn format_structured_line(role: &LineRole, inline_text: &str) -> String {
    match role {
        LineRole::Heading(level) => {
            let hashes = "#".repeat((*level).clamp(1, 6) as usize);
            let text = strip_outer_emphasis(inline_text);
            if text.is_empty() {
                inline_text.to_string()
            } else {
                format!("{hashes} {text}")
            }
        }
        LineRole::List { depth, ordered, ordinal } => {
            let indent = "  ".repeat(*depth as usize);
            if *ordered {
                format!("{indent}{ordinal}. {inline_text}")
            } else {
                format!("{indent}- {inline_text}")
            }
        }
        LineRole::Body => inline_text.to_string(),
    }
}
