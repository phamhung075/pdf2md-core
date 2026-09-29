// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Line-wrap hyphen classification and the evidence used to de-hyphenate an
//! *unattested* join without fusing a genuine compound.
//!
//! Two rules share this module:
//!
//! * the conservative rule in [`classify_hyphen_join`] recognises the shape of
//!   a wrap (letter + `-`, lowercase fragment, not a French clitic / compound
//!   tail);
//! * the evidence-based rule in [`is_unattested_fragment`] additionally checks
//!   the chunk's own attestation so a wrapped fragment is joined even when the
//!   joined word never occurs standalone, while a real compound keeps its
//!   hyphen.

use std::collections::HashSet;

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

/// Productive derivational prefixes: a trailing `-` after one of these is a
/// real compound hyphen (`sous-total`, `auto-école`, `anti-virus`), not a
/// line-wrap break, even when the joined word is never attested standalone.
/// `ci-` / `pare-` were added from the private-corpus audit (they fused
/// `ci-dessus` and `pare-brise`); both are genuinely productive French forms.
/// The only entry shared with [`COMPOUND_TAILS`] is `ci`, which is legitimately
/// both a tail clitic (`celui-ci`) and a prefix (`ci-dessus`).
const COMPOUND_PREFIXES: &[&str] = &[
    "non", "anti", "auto", "co", "contre", "ex", "extra", "inter", "micro", "multi", "post",
    "pre", "pré", "pro", "semi", "sous", "sur", "self", "quasi", "demi", "mi", "vice", "néo", "re",
    "ci", "pare",
];

/// Shortest standalone-word half that protects a hyphen from being dropped.
/// A 2-letter attested half is almost always a function word that merely
/// happens to appear before/after the wrap (`in`, `de`, `en`), not evidence of a
/// compound: the strict "either half attested" rule kept `in-crementally` and
/// cost a real olmOCR pass, while `pocket-sprung` is protected because
/// `pocket` clears this floor.
const MIN_COMPOUND_HALF_LEN: usize = 3;

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

/// Whether `prefix` is a productive derivational prefix that must keep a
/// following hyphen when two lines are joined.
fn is_compound_prefix(prefix: &str) -> bool {
    COMPOUND_PREFIXES.contains(&prefix)
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

/// The two lowercased fragments a line-end hyphen splits a word into: the
/// alphabetic run ending at the hyphen in `prev`, and the first word of `next`.
fn hyphen_fragments(prev: &str, next: &str) -> (String, String) {
    let p = prev.trim_end();
    let p = p.strip_suffix('-').unwrap_or(p);
    let prefix: String = p
        .chars()
        .rev()
        .take_while(|c| c.is_alphabetic())
        .collect::<Vec<char>>()
        .into_iter()
        .rev()
        .collect();
    (prefix.to_lowercase(), first_word(next).to_lowercase())
}

/// The word formed by undoing a line-end hyphen: the alphabetic run ending at
/// the hyphen in `prev`, concatenated with the first word of `next`, lowercased.
pub(crate) fn dehyphenated_word(prev: &str, next: &str) -> String {
    let (prefix, tail) = hyphen_fragments(prev, next);
    format!("{prefix}{tail}")
}

/// True when the last whitespace-delimited token of `prev` already carries an
/// interior hyphen (`arc-en-`, `c'est-à-`): the wrap sits inside a multi-part
/// compound, so the hyphen must survive even when neither half is attested
/// standalone.
fn has_inner_hyphen(prev: &str) -> bool {
    let token = prev.split_whitespace().last().unwrap_or("");
    token.trim_end_matches('-').contains('-')
}

/// True when the alphabetic run ending at the wrap hyphen starts with a capital
/// (`Dai-` / `hung`): a capitalised fragment is more likely the first part of a
/// hyphenated proper name than a wrapped word, and fusing a name changes the
/// identity it carries, while keeping a stray hyphen only costs a match.
fn wrap_prefix_is_capitalised(prev: &str) -> bool {
    prev.trim_end()
        .trim_end_matches('-')
        .chars()
        .rev()
        .take_while(|c| c.is_alphabetic())
        .last()
        .is_some_and(char::is_uppercase)
}

/// The chunk's own attestation, used to drop an unattested wrap hyphen without
/// fusing a compound. Built once per page / reflow chunk.
#[derive(Default)]
pub(crate) struct JoinEvidence {
    /// Words that occur as a complete standalone word somewhere in the chunk,
    /// lowercased. The two fragments of a line-wrap hyphen (the token ending in
    /// `-`, and the first word of the line it continues) are excluded: they are
    /// pieces of one word, not standalone attestations.
    vocab: HashSet<String>,
    /// Tokens that occur with an interior hyphen within a single line of the
    /// chunk, lowercased (`well-known`). A line-end `foo-` is not such a token.
    hyphenated: HashSet<String>,
}

impl JoinEvidence {
    /// Collect attestation from every line of `texts` (each entry may itself be
    /// a multi-line chunk). All entries are flattened into one line stream so a
    /// wrap that crosses an entry boundary is still recognised.
    pub(crate) fn from_text<'a, I>(texts: I) -> Self
    where
        I: IntoIterator<Item = &'a str>,
    {
        let mut ev = Self::default();
        let mut lines: Vec<&str> = Vec::new();
        for text in texts {
            lines.extend(text.split('\n'));
        }
        for (i, line) in lines.iter().enumerate() {
            let continues_wrap = i > 0 && ends_with_wrap_hyphen(lines[i - 1]);
            let words: Vec<&str> = line
                .split(|c: char| !c.is_alphabetic())
                .filter(|w| !w.is_empty())
                .collect();
            // The last word of a hyphen-ended line is the wrap prefix, not a
            // standalone word; likewise the first word of the line that
            // continues it is the wrap tail.
            let prefix_idx = if ends_with_wrap_hyphen(line) {
                words.len().checked_sub(1)
            } else {
                None
            };
            for (j, word) in words.iter().enumerate() {
                if Some(j) == prefix_idx || (continues_wrap && j == 0) {
                    continue;
                }
                ev.vocab.insert(word.to_lowercase());
            }
            for token in line.split(|c: char| !(c.is_alphabetic() || c == '-')) {
                let t = token.trim_matches('-');
                if t.contains('-') {
                    ev.hyphenated.insert(t.to_lowercase());
                }
            }
        }
        ev
    }

    /// Whether the fully joined `word` occurs standalone in the chunk.
    pub(crate) fn attests(&self, word: &str) -> bool {
        self.vocab.contains(word)
    }

    fn attests_hyphenated(&self, token: &str) -> bool {
        self.hyphenated.contains(token)
    }
}

/// True when `line` ends with the letter + hyphen shape of a wrap break.
fn ends_with_wrap_hyphen(line: &str) -> bool {
    let t = line.trim_end();
    let mut rev = t.chars().rev();
    rev.next() == Some('-') && rev.next().is_some_and(|c| c.is_alphabetic())
}

/// Whether an *unattested* join (`prefix-tail` itself never occurs standalone)
/// is nevertheless a wrapped fragment rather than a genuine compound. Requires
/// [`classify_hyphen_join`] to have already returned
/// [`HyphenJoin::Dehyphenate`]; a compound is kept when any of these holds:
///
/// * the wrap sits inside a multi-part hyphenated token (`arc-en-ciel`);
/// * the fragment before the hyphen is capitalised (`Dai-hung`, a likely name);
/// * the prefix is a productive compound prefix (`sous-total`, `ci-dessus`);
/// * the hyphenated form occurs mid-line elsewhere in the chunk;
/// * either half occurs standalone and is at least
///   [`MIN_COMPOUND_HALF_LEN`] letters (`porte` / `monnaie`), or both halves
///   occur standalone as any length (`air` / `bag`).
pub(crate) fn is_unattested_fragment(prev: &str, next: &str, ev: &JoinEvidence) -> bool {
    if has_inner_hyphen(prev) {
        return false;
    }
    let (prefix, tail) = hyphen_fragments(prev, next);
    if prefix.is_empty() || tail.is_empty() || wrap_prefix_is_capitalised(prev) {
        return false;
    }
    if is_compound_prefix(&prefix) {
        return false;
    }
    if ev.attests_hyphenated(&format!("{prefix}-{tail}")) {
        return false;
    }
    let content_half = (ev.attests(&prefix) && prefix.chars().count() >= MIN_COMPOUND_HALF_LEN)
        || (ev.attests(&tail) && tail.chars().count() >= MIN_COMPOUND_HALF_LEN);
    if content_half || (ev.attests(&prefix) && ev.attests(&tail)) {
        return false;
    }
    true
}
