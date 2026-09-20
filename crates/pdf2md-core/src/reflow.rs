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
//! * structural lines (headings, list items, blockquotes, images, table rows,
//!   fenced code) are never touched and act as hard boundaries;
//! * a paragraph break (blank line, or a line that ends with `:`) is preserved;
//! * two plain body lines join only when the earlier one has no sentence
//!   terminator and the later one continues it (starts lowercase or with
//!   `, ; . )`); a line that ends a sentence is always a paragraph boundary,
//!   however long it is;
//! * a trailing line-break hyphen is removed only before a lowercase *fragment*
//!   (not a French clitic / compound tail) **and** when the joined word occurs
//!   elsewhere in the document as a standalone word, so `infor-` + `mation`
//!   collapses only when `information` is attested, while `peut-être`,
//!   `c'est-à-dire` and `non-professionnel` always survive;
//! * a space is kept before `, ; . )` only when the source already carries the
//!   whitespace after it, so two numbers are never fused across a join.

use std::collections::HashSet;

/// Line prefixes that always mark a structural (non-paragraph) line.
const STRUCTURAL_STARTS: &[&str] = &["#", "- ", "* ", "> ", "<", "![", "|", "*["];

/// Second elements of French hyphenated compounds / inversion clitics. A
/// trailing `-` before one of these is a *real* hyphen, not a line-wrap break.
const COMPOUND_TAILS: &[&str] = &[
    // inversion / elision clitics
    "ce", "il", "elle", "on", "je", "tu", "nous", "vous", "ils", "elles", "t", "en", "y", "ci",
    "là", "même", "moi", "toi", "soi", "lui", "leur",
    // common compounds whose second element is a full word
    "etre", "être", "dire", "professionnel", "professionnelle", "professionnels", "professionnelles",
    "tout", "tous", "toute", "toutes", "rien", "jamais", "mieux", "moins", "plus", "même",
];

/// How a line break that lands on a trailing hyphen should be joined.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum HyphenJoin {
    /// A line-wrap break: drop the hyphen and concatenate (`infor-` + `mation`).
    Dehyphenate,
    /// A real compound hyphen: keep it and concatenate (`peut-` + `être`).
    KeepHyphen,
    /// Not a hyphen break: join with a normal space.
    None,
}

/// Whether `word` is a French clitic / compound tail that must keep a preceding
/// hyphen when two lines are joined.
pub(crate) fn is_compound_tail(word: &str) -> bool {
    let w = word.to_lowercase();
    COMPOUND_TAILS.contains(&w.as_str())
}

/// True when the whitespace-stripped line opens a structural Markdown element.
fn is_structural_start(t: &str) -> bool {
    STRUCTURAL_STARTS.iter().any(|p| t.starts_with(p))
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
/// lines, label/`:` lines and form rows with wide gaps are excluded.
fn is_plain_body(line: &str) -> bool {
    let t = line.trim();
    if t.is_empty() || is_structural_start(t) || has_ordered_list_marker(line) {
        return false;
    }
    if t.ends_with(':') {
        return false;
    }
    wide_space_runs(line) < 2
}

/// Whether `t` ends with a sentence terminator.
fn ends_sentence(t: &str) -> bool {
    matches!(t.chars().last(), Some('.' | '!' | '?' | ':' | ';' | '»'))
}

/// First run of alphabetic characters in `s`, or `""` when it does not start
/// with a letter.
fn first_word(s: &str) -> &str {
    let t = s.trim_start();
    let end = t.find(|c: char| !c.is_alphabetic()).unwrap_or(t.len());
    &t[..end]
}

/// Classify a join that lands on a trailing hyphen.
pub(crate) fn classify_hyphen_join(prev: &str, next: &str) -> HyphenJoin {
    let p = prev.trim_end();
    let mut rev = p.chars().rev();
    if rev.next() != Some('-') {
        return HyphenJoin::None;
    }
    match rev.next() {
        Some(c) if c.is_alphabetic() => {}
        _ => return HyphenJoin::None,
    }
    let w = first_word(next);
    let first = match w.chars().next() {
        Some(c) => c,
        None => return HyphenJoin::None,
    };
    if first.is_lowercase() && !is_compound_tail(w) {
        HyphenJoin::Dehyphenate
    } else {
        HyphenJoin::KeepHyphen
    }
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

/// Every maximal run of alphabetic characters in `lines`, lowercased. A
/// line-end hyphen may only be dropped when the de-hyphenated word actually
/// occurs elsewhere in the document as a standalone word, so the reflow needs
/// the document's own vocabulary.
fn document_vocabulary(lines: &[&str]) -> HashSet<String> {
    let mut set = HashSet::new();
    for line in lines {
        for word in line.split(|c: char| !c.is_alphabetic()) {
            if !word.is_empty() {
                set.insert(word.to_lowercase());
            }
        }
    }
    set
}

/// The word formed by undoing a line-end hyphen: the alphabetic run ending at
/// the hyphen in `prev`, concatenated with the first word of `next`, lowercased.
fn dehyphenated_word(prev: &str, next: &str) -> String {
    let p = prev.trim_end();
    let p = p.strip_suffix('-').unwrap_or(p);
    let mut frag: Vec<char> = p
        .chars()
        .rev()
        .take_while(|c| c.is_alphabetic())
        .collect();
    frag.reverse();
    let mut word: String = frag.into_iter().collect();
    word.push_str(&first_word(next).to_lowercase());
    word.to_lowercase()
}

/// Reflow `input`: join wrapped body lines into paragraphs.
///
/// Pure and allocation-bounded: the output is at most the input plus one space
/// per join, and every decision is a single linear scan.
pub fn reflow_markdown(input: &str) -> String {
    let lines: Vec<&str> = input.split('\n').collect();

    // Document vocabulary: a line-end hyphen is only removed when the joined
    // word appears elsewhere in the document as a standalone word.
    let vocab = document_vocabulary(&lines);

    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let mut pending: Option<String> = None;
    let mut last = String::new();
    let mut fence = false;

    for &ln in &lines {
        let trimmed = ln.trim();

        // Headings / page markers: hard boundary, never touched.
        if ln.starts_with('#') {
            if let Some(p) = pending.take() {
                out.push(p);
            }
            out.push(ln.to_string());
            continue;
        }

        let is_fence = trimmed.starts_with("```") || trimmed.starts_with("~~~");
        if is_fence {
            if let Some(p) = pending.take() {
                out.push(p);
            }
            out.push(ln.to_string());
            fence = !fence;
            continue;
        }
        if fence {
            out.push(ln.to_string());
            continue;
        }

        if trimmed.is_empty() || !is_plain_body(ln) {
            if let Some(p) = pending.take() {
                out.push(p);
            }
            out.push(ln.to_string());
            continue;
        }

        if pending.is_none() {
            pending = Some(ln.to_string());
            last.clear();
            last.push_str(ln);
            continue;
        }

        let ns = ln.trim_start();
        let first = ns.chars().next();
        let a_term = ends_sentence(last.trim_end());
        // A line that already ends a sentence is never a wrapped continuation,
        // however long it is: a following lowercase line starts a new
        // paragraph. Only a leading `, ; . )` (the continuation punctuation
        // itself) joins across the terminator.
        let join = match first {
            Some(c) if matches!(c, ',' | ';' | '.' | ')') => true,
            Some(c) if c.is_lowercase() && !a_term => true,
            _ => false,
        };

        if join {
            let buf = pending.as_mut().expect("pending is set");
            match classify_hyphen_join(buf, ln) {
                HyphenJoin::Dehyphenate
                    if vocab.contains(&dehyphenated_word(buf, ln)) =>
                {
                    let n = buf.trim_end().len();
                    buf.truncate(n - 1); // '-' is one byte
                    buf.push_str(ns);
                }
                // A real compound / clitic, or a fragment whose joined word
                // never appears standalone in this document: keep the hyphen.
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
            }
            last.clear();
            last.push_str(ln);
        } else {
            if let Some(p) = pending.take() {
                out.push(p);
            }
            pending = Some(ln.to_string());
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
mod tests {
    use super::*;

    fn join(input: &str) -> String {
        reflow_markdown(input)
    }

    #[test]
    fn consecutive_body_lines_join_into_one_paragraph() {
        // Three wrapped lines with no terminator and lowercase continuations.
        let out = join("le montant est de 36,02\neuro par jour\ndonc eleve");
        assert_eq!(out, "le montant est de 36,02 euro par jour donc eleve");
    }

    #[test]
    fn comma_continuation_joins_without_a_space_before_the_comma() {
        let out = join("le montant est de 36,02 euros par jour\n, avant application");
        assert_eq!(out, "le montant est de 36,02 euros par jour, avant application");
    }

    #[test]
    fn a_space_is_kept_when_it_would_fuse_two_numbers() {
        // `12` + `.50` must not become `12.50`; the space is preserved.
        let out = join("total 12\n.50 euros");
        assert_eq!(out, "total 12 .50 euros");
        // And the concatenated digit stream is unchanged.
        let digits = |s: &str| s.chars().filter(|c| c.is_ascii_digit()).collect::<String>();
        assert_eq!(digits(&out), digits("total 12\n.50 euros"));
    }

    #[test]
    fn structural_lines_are_never_joined() {
        let input = "# Heading one\ntext under heading\n- list item\nlist continuation\n| a | b |\ncell\n```\ncode line\nbody line\n```\nafter code\nand more";
        let out = join(input);
        assert!(out.contains("# Heading one\ntext under heading"));
        assert!(out.contains("- list item\nlist continuation"));
        assert!(out.contains("| a | b |\ncell"));
        assert!(out.contains("```\ncode line\nbody line\n```"));
        // Body lines after the fence reflow normally.
        assert!(out.contains("after code and more"));
    }

    #[test]
    fn blank_line_is_a_paragraph_boundary() {
        let out = join("premier paragraphe court\n\ndeuxieme paragraphe");
        assert_eq!(out, "premier paragraphe court\n\ndeuxieme paragraphe");
    }

    #[test]
    fn line_ending_with_colon_is_a_boundary() {
        let out = join("Voici la liste :\npremier element\ndeuxieme element");
        assert_eq!(out, "Voici la liste :\npremier element deuxieme element");
    }

    #[test]
    fn orderered_list_markers_are_untouched() {
        let out = join("1. premier point\n2. deuxieme point");
        assert_eq!(out, "1. premier point\n2. deuxieme point");
    }

    #[test]
    fn long_sentence_end_does_not_join_a_short_next_line() {
        // The first line ends a sentence and is short relative to the page
        // median, so a new paragraph is assumed rather than a continuation.
        let out = join("Fait.\nou alors la suite");
        assert_eq!(out, "Fait.\nou alors la suite");
    }

    #[test]
    fn sentence_terminator_never_joins_even_a_long_next_line() {
        // R3: however long the first line is, a sentence terminator is a hard
        // paragraph boundary; only a leading `, ; . )` continues it.
        let long = "This first sentence is deliberately long enough to look like \
                    the body line of a fully justified paragraph and it still ends.";
        let out = join(&format!("{long}\nanother lowercase paragraph starts here"));
        assert_eq!(
            out,
            format!("{long}\nanother lowercase paragraph starts here")
        );
        // The continuation-punctuation exception still joins.
        let out = join(&format!("{long}\n, before the next clause"));
        assert!(!out.contains('\n'), "comma continuation must join: {out}");
    }

    #[test]
    fn dehyphenates_a_lowercase_fragment_only_with_self_vocabulary() {
        // `information` occurs standalone earlier in the document: the wrapped
        // fragment is a line break, so the hyphen is dropped.
        let out = join("information complete\n\nThis is infor-\nmation you need");
        assert_eq!(out, "information complete\n\nThis is information you need");
        // No standalone `information` anywhere: keep the hyphen.
        let out = join("Ceci est infor-\nmation utile");
        assert_eq!(out, "Ceci est infor-mation utile");
    }

    #[test]
    fn keeps_the_hyphen_before_a_clitic_or_compound_tail() {
        assert_eq!(join("Comment va-\nt-il va"), "Comment va-t-il va");
        assert_eq!(join("C'est peut-\nêtre vrai"), "C'est peut-être vrai");
        // A real compound tail keeps its hyphen.
        assert_eq!(join("un non-\nprofessionnel ici"), "un non-professionnel ici");
    }

    #[test]
    fn wide_space_form_rows_are_not_joined() {
        let out = join("Nom    Prenom    Date\nMontant    1 234,56    EUR");
        assert!(out.contains('\n'));
        assert_eq!(out, "Nom    Prenom    Date\nMontant    1 234,56    EUR");
    }

    #[test]
    fn fenced_code_block_content_is_verbatim() {
        let out = join("text before\n```rust\nlet a = 1;\nlet b = 2;\n```\ntext after");
        assert!(out.contains("```rust\nlet a = 1;\nlet b = 2;\n```"));
    }

    #[test]
    fn word_multiset_is_preserved_by_joining() {
        // Joining adds whitespace / removes a hyphen but must not change the
        // set of letter words beyond the de-hyphenated fragment.
        let src = "La valeur totale est de\ntrente six euros par\njour, avant application\ndu tarif.";
        let out = join(src);
        let words = |s: &str| {
            let mut v: Vec<String> = s
                .split(|c: char| !c.is_alphabetic())
                .filter(|w| w.chars().count() >= 3)
                .map(|w| w.to_lowercase())
                .collect();
            v.sort();
            v
        };
        let before = words(src);
        let after = words(&out);
        // `par` + `jour` etc. survive; only the removed line break changes
        // nothing here, so the multisets are identical.
        assert_eq!(before, after);
    }

    #[test]
    fn empty_and_single_line_inputs_round_trip() {
        assert_eq!(join(""), "");
        assert_eq!(join("une seule ligne"), "une seule ligne");
        assert_eq!(join("a\n"), "a\n");
    }

    /// End-to-end: a one-page PDF whose wrapped body line is drawn as two `Tj`
    /// runs must come out of the converter as a single paragraph. Built
    /// programmatically with lopdf — no fixture, no document text.
    #[test]
    fn reflow_joins_wrapped_lines_end_to_end_in_a_synthetic_pdf() {
        use lopdf::content::{Content, Operation};
        use lopdf::{dictionary, Document, Object, Stream};

        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let font_id = doc.add_object(dictionary! {
            "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Courier",
        });
        let resources_id = doc.add_object(dictionary! {
            "Font" => dictionary! { "F1" => font_id },
        });
        let mut ops = vec![Operation::new("BT", vec![])];
        for (y, text) in [
            (700.0f64, "le montant est de 36,02"),
            (688.0f64, "euros par jour"),
        ] {
            ops.push(Operation::new("Tf", vec!["F1".into(), 10.0.into()]));
            ops.push(Operation::new(
                "Tm",
                vec![1.into(), 0.into(), 0.into(), 1.into(), 50.into(), y.into()],
            ));
            ops.push(Operation::new("Tj", vec![Object::string_literal(text)]));
        }
        ops.push(Operation::new("ET", vec![]));
        let content_id = doc.add_object(Stream::new(
            dictionary! {},
            Content { operations: ops }.encode().expect("encode content"),
        ));
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page", "Parent" => pages_id, "Contents" => content_id,
        });
        let pages = dictionary! {
            "Type" => "Pages",
            "Kids" => vec![page_id.into()],
            "Count" => 1,
            "Resources" => resources_id,
            "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        };
        doc.objects.insert(pages_id, Object::Dictionary(pages));
        let catalog_id = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        doc.trailer.set("Root", catalog_id);
        let mut bytes = Vec::new();
        doc.save_to(&mut bytes).expect("save pdf");

        let options = crate::ConversionOptions::default();
        let result = crate::convert_pdf_bytes_to_markdown(&bytes, &options).expect("convert");
        assert!(
            result.markdown.contains("36,02 euros par jour"),
            "wrapped line was not reflowed: {:?}",
            result.markdown
        );
        assert!(
            !result.markdown.contains("36,02\neuros"),
            "wrapped line stayed split: {:?}",
            result.markdown
        );
    }
}
