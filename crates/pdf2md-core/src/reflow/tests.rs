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