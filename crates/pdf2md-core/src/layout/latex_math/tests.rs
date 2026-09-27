// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! LaTeX math AST synthesis built directly from 2D glyph geometry.
//!
//! Modern PDF producers typeset math as positioned glyph runs rather than as a
//! semantic math tree: a built-up fraction is a smaller numerator run stacked
//! above a thin horizontal rule (the fraction bar) with a smaller denominator
//! run below it, and a simple super/subscript is a smaller run whose baseline
//! is raised/lowered relative to its base. Before the geometry engine exports
//! the page as Markdown it can now lift these visual patterns into a real
//! [`LatexExpr`] AST and serialize them as semantically correct LaTeX
//! (`$\frac{a}{b}$`, `$x^{2}$`, `$a_{i}$`) instead of emitting them as a flat
//! sequence of glyphs.
//!
//! The detector is intentionally geometric and conservative, and only runs when
//! the caller opts in via `detect_math` so the default extraction path stays
//! byte-identical.

    use super::*;

    fn span(text: &str, x: f64, y: f64, size: f64, advance: f64) -> Span {
        Span {
            text: text.to_string(),
            x,
            y,
            size,
            advance,
            word_advance: advance,
            is_bold: false,
            is_italic: false,
            is_underline: false,
            is_vertical: false,
        }
    }

    // --- AST serialization ---

    #[test]
    fn fraction_serializes_to_latex_frac() {
        let expr = LatexExpr::fraction(LatexExpr::text("a"), LatexExpr::text("b"));
        assert_eq!(expr.to_latex(), r"\frac{a}{b}");
        assert_eq!(expr.render_inline(), r"$\frac{a}{b}$");
    }

    #[test]
    fn superscript_serializes_to_caret() {
        let expr = LatexExpr::superscript(LatexExpr::text("x"), LatexExpr::text("2"));
        assert_eq!(expr.to_latex(), r"x^{2}");
        assert_eq!(expr.render_inline(), r"$x^{2}$");
    }

    #[test]
    fn subscript_serializes_to_underscore() {
        let expr = LatexExpr::subscript(LatexExpr::text("a"), LatexExpr::text("i"));
        assert_eq!(expr.to_latex(), r"a_{i}");
        assert_eq!(expr.render_inline(), r"$a_{i}$");
    }

    #[test]
    fn nested_sequence_serializes() {
        let expr = LatexExpr::seq(vec![
            LatexExpr::text("S"),
            LatexExpr::superscript(LatexExpr::text("2"), LatexExpr::text("3")),
            LatexExpr::text("+"),
            LatexExpr::fraction(LatexExpr::text("1"), LatexExpr::text("2")),
        ]);
        assert_eq!(expr.to_latex(), r"S2^{3}+\frac{1}{2}");
    }

    // --- Within-line super/subscript detection ---

    /// "x" baseline 700 at size 12, superscript "2" baseline 712 at size 8.
    #[test]
    fn detects_superscript_within_a_line() {
        let line = vec![
            span("x", 100.0, 700.0, 12.0, 8.0),
            span("2", 110.0, 712.0, 8.0, 5.0),
        ];
        let expr = synthesize_line_expr(&line, None);
        assert_eq!(expr.to_latex(), r"x^{2}");
        assert_eq!(render_math_line(&line, None), r"$x^{2}$");
    }

    /// "a" baseline 700, subscript "i" baseline 694.
    #[test]
    fn detects_subscript_within_a_line() {
        let line = vec![
            span("a", 100.0, 700.0, 12.0, 8.0),
            span("i", 108.0, 694.0, 8.0, 5.0),
        ];
        assert_eq!(synthesize_line_expr(&line, None).to_latex(), r"a_{i}");
    }

    /// A decorative whitespace-only spacer must never be treated as a
    /// super/subscript. Producers pad text with spacer runs drawn at a slightly
    /// smaller size and a shifted baseline; keeping such a run in the cell
    /// grouping let it masquerade as an *empty* script and wrapped ordinary
    /// prose in `$..._{}$` / `$...^{}$` (exactly the corruption observed on the
    /// CAF `SeConnecterAMonComptePartenaire.pdf` fixture, where five plain
    /// prose lines were synthesized as LaTeX math).
    #[test]
    fn whitespace_only_spacer_is_not_a_script() {
        let line = vec![
            span("Vous avez reçu un identifiant", 100.0, 700.0, 10.56, 200.0),
            // Trailing spacer: smaller size, ~4pt above the text baseline.
            span(" ", 302.0, 704.0, 9.96, 3.0),
        ];
        let rendered = render_math_line(&line, None);
        assert!(
            !rendered.contains('$'),
            "a whitespace spacer must not synthesize math, got {rendered}"
        );
        assert!(rendered.contains("identifiant"), "got {rendered}");
    }

    /// A long, multi-word run that merely sits on an offset baseline is body
    /// text, not a super/subscript: a neighbouring column serialised onto the
    /// same visual line used to be wrapped whole as `$_{...}$`, corrupting the
    /// prose and defeating text search. A real script is short (an exponent,
    /// an ordinal suffix, a footnote mark); anything longer must stay plain.
    #[test]
    fn long_offset_run_is_not_wrapped_as_a_script() {
        let line = vec![
            span("Sandrine brient-", 100.0, 700.0, 12.0, 90.0),
            // A phrase from the adjacent column: smaller face, lower baseline.
            span(
                "a aussi remporté le « prix cœur » du concours",
                300.0,
                694.0,
                9.0,
                200.0,
            ),
        ];
        let rendered = render_math_line(&line, None);
        assert!(
            !rendered.contains('$'),
            "a long offset run must not synthesize math, got {rendered}"
        );
        assert!(
            rendered.contains("prix cœur"),
            "the offset run's text must survive, got {rendered}"
        );
    }

    /// A short ordinal suffix on a numeric base is still recognised: the length
    /// cap only rejects runs too long to be a script, not legitimate ones
    /// (`x^{2}`, `1^{er}`, `1^{ère}`, `note^{1}`).
    #[test]
    fn short_ordinal_suffix_is_still_a_script() {
        // Three characters (`ère`) is exactly at the cap and must still pass.
        let line = vec![
            span("1", 100.0, 700.0, 12.0, 8.0),
            span("ère", 108.0, 712.0, 8.0, 12.0),
        ];
        assert_eq!(render_math_line(&line, None), r"$1^{ère}$");
    }

    /// Plain body text must remain byte-identical (no false positives).
    #[test]
    fn plain_line_remains_unchanged() {
        // Realistic body-word geometry: consecutive word starts stay within
        // 2.5*size of each other, so the legacy renderer keeps them on one line.
        let line = vec![
            span("Estim", 100.0, 700.0, 10.0, 20.0),
            span("caf", 124.0, 700.0, 10.0, 14.0),
            span("total", 146.0, 700.0, 10.0, 20.0),
        ];
        let rendered = render_math_line(&line, None);
        // The legacy renderer joins words with a single space.
        assert!(rendered.contains("Estim caf total"), "got {rendered}");
        assert!(!rendered.contains('$'), "got {rendered}");
    }

    /// A prose line carrying a footnote-style marker must NOT have its whole
    /// content swallowed into `$...$`: only the marker's immediate base word
    /// participates, surrounding words stay plain.
    #[test]
    fn script_attaches_to_immediate_word_not_whole_line() {
        // "See" and "note" share size 12 / baseline 700 (one cell); the raised
        // "1" (size 8, baseline 712) is a superscript attached to "note".
        let line = vec![
            span("See", 100.0, 700.0, 12.0, 32.0),
            span("note", 134.0, 700.0, 12.0, 40.0),
            span("1", 176.0, 712.0, 8.0, 5.0),
        ];
        let rendered = render_math_line(&line, None);
        assert!(
            !rendered.contains("$See"),
            "leading prose must stay plain, got {rendered}"
        );
        assert!(
            rendered.contains("note$^{1}$"),
            "only the base word is followed by the footnote marker, got {rendered}"
        );
        assert!(rendered.starts_with("See"), "got {rendered}");
    }

    /// A prose word carrying a footnote-style marker must keep the word as plain
    /// text and attach the marker as `word$^{1}$`; wrapping the word in math mode
    /// (`$implementation^{1}$`) renders it italic and breaks text search.
    #[test]
    fn prose_word_footnote_keeps_word_plain() {
        let line = vec![
            span("implementation", 100.0, 700.0, 12.0, 90.0),
            span("1", 192.0, 712.0, 8.0, 5.0),
        ];
        let rendered = render_math_line(&line, None);
        assert!(
            rendered.contains("implementation$^{1}$"),
            "prose word must stay plain, got {rendered}"
        );
        assert!(
            !rendered.contains("$implementation"),
            "prose word must not be wrapped in math, got {rendered}"
        );
    }

    /// A prose word + attached marker immediately followed by a period must not
    /// grow a space before the punctuation: `SkyPilot$^{2}.`, never
    /// `$SkyPilot^{2}$ .`.
    #[test]
    fn prose_word_footnote_followed_by_period_has_no_space() {
        let line = vec![
            span("SkyPilot", 100.0, 700.0, 12.0, 50.0),
            span("2", 152.0, 712.0, 8.0, 5.0),
            span(".", 160.0, 700.0, 12.0, 4.0),
        ];
        let rendered = render_math_line(&line, None);
        assert_eq!(rendered, "SkyPilot$^{2}$.", "got {rendered}");
    }

    /// A single-letter variable is genuine math, so the base stays inside the
    /// inline math delimiters.
    #[test]
    fn single_letter_math_superscript_preserved() {
        let line = vec![
            span("x", 100.0, 700.0, 12.0, 8.0),
            span("2", 110.0, 712.0, 8.0, 5.0),
        ];
        assert_eq!(render_math_line(&line, None), r"$x^{2}$");
    }

    /// A numeric base is genuine math, so it stays inside the inline math
    /// delimiters.
    #[test]
    fn numeric_base_math_superscript_preserved() {
        let line = vec![
            span("10", 100.0, 700.0, 12.0, 14.0),
            span("5", 116.0, 712.0, 8.0, 5.0),
        ];
        assert_eq!(render_math_line(&line, None), r"$10^{5}$");
    }

    #[test]
    fn render_inline_mixed_leaves_trailing_prose_plain() {
        // A base+script followed by more prose: the math node is delimited but
        // the trailing text is left untouched.
        let expr = LatexExpr::seq(vec![
            LatexExpr::superscript(LatexExpr::text("mc"), LatexExpr::text("2")),
            LatexExpr::text(" meters"),
        ]);
        assert_eq!(expr.render_inline_mixed(), "$mc^{2}$ meters");
    }

    #[test]
    fn sequence_text_parts_are_space_joined() {
        // `synthesize_line_expr` splits a prose run into word tokens around an
        // attached script; those `Text` parts are separate units and must not
        // be glued together (`comparestheperformance...`).
        let expr = LatexExpr::seq(vec![
            LatexExpr::text("compares"),
            LatexExpr::superscript(LatexExpr::text("34B"), LatexExpr::text("4")),
            LatexExpr::text("in different"),
        ]);
        assert_eq!(expr.render_inline_mixed(), "compares $34B^{4}$ in different");
    }

    #[test]
    fn math_inline_for_line_none_for_plain_text() {
        let line = vec![
            span("Estim", 100.0, 700.0, 10.0, 20.0),
            span("caf", 124.0, 700.0, 10.0, 14.0),
        ];
        assert_eq!(math_inline_for_line(&line), None);
    }

    // --- Cross-line fraction detection ---

    #[test]
    fn detects_built_up_fraction_across_lines() {
        let stream = vec![
            // numerator "1" above the bar
            vec![span("1", 150.0, 715.0, 8.0, 5.0)],
            // fraction bar (y=708, x 150..170)
            vec![],
            // denominator "2" below the bar
            vec![span("2", 150.0, 701.0, 8.0, 5.0)],
        ];
        // The bar is supplied as a rule segment, not a span line.
        let bars: Vec<RuleSeg> = vec![(708.0, 150.0, 170.0)];
        let hits = detect_fractions(&stream, &bars);
        assert_eq!(hits.len(), 1, "must detect one fraction: {hits:?}");
        assert_eq!(hits[0].numerator_line, 0);
        assert_eq!(hits[0].denominator_line, 2);
        assert_eq!(hits[0].expr.to_latex(), r"\frac{1}{2}");
    }

    #[test]
    fn fraction_bar_far_away_is_rejected() {
        let stream = vec![
            vec![span("1", 150.0, 715.0, 8.0, 5.0)],
            vec![span("2", 150.0, 640.0, 8.0, 5.0)],
        ];
        // A bar that is not vertically between the two runs.
        let bars: Vec<RuleSeg> = vec![(700.0, 150.0, 170.0)];
        assert!(detect_fractions(&stream, &bars).is_empty());
    }

    // Regression tests for a real bug found on a French utility invoice: a
    // decorative rule under a "NOUS CONTACTER" heading, separating it from
    // an unrelated "N° client : ..." line below, was read as a fraction bar
    // — width-ratio alone (1.68, well inside what a legitimate fraction's
    // bar/content ratio looks like) could not tell them apart. The real
    // signal is that a fraction's numerator/denominator is short; a phrase
    // or sentence never is.

    #[test]
    fn fraction_bar_rejects_a_long_phrase_numerator_even_at_a_plausible_width_ratio() {
        let stream = vec![
            // A short heading-like phrase, 14 chars — same shape as the real
            // "NOUS CONTACTER" false positive.
            vec![span("SOME HEADING12", 100.0, 715.0, 8.0, 90.0)],
            vec![span("42", 100.0, 701.0, 8.0, 90.0)],
        ];
        // bar_w=100 vs content_w=90 -> ratio 1.11, comfortably inside a
        // legitimate fraction's range; only the text-length check can catch this.
        let bars: Vec<RuleSeg> = vec![(708.0, 100.0, 200.0)];
        assert!(
            detect_fractions(&stream, &bars).is_empty(),
            "a 14-char phrase numerator must never be read as a fraction, regardless of bar width ratio"
        );
    }

    #[test]
    fn fraction_bar_rejects_a_long_phrase_denominator_too() {
        let stream = vec![
            vec![span("42", 100.0, 715.0, 8.0, 15.0)],
            vec![span("An unrelated line", 100.0, 701.0, 8.0, 130.0)],
        ];
        let bars: Vec<RuleSeg> = vec![(708.0, 100.0, 150.0)];
        assert!(detect_fractions(&stream, &bars).is_empty());
    }

    #[test]
    fn fraction_bar_rejects_empty_or_whitespace_only_lines() {
        // A non-empty Vec<Span> that is entirely whitespace glyphs (spacing/
        // underline decoration) must not produce an empty `$\frac{}{}$`.
        let stream = vec![
            vec![span("  ", 150.0, 715.0, 8.0, 10.0)],
            vec![span(" ", 150.0, 701.0, 8.0, 10.0)],
        ];
        let bars: Vec<RuleSeg> = vec![(708.0, 150.0, 170.0)];
        assert!(detect_fractions(&stream, &bars).is_empty());
    }

    #[test]
    fn stacked_fraction_rejects_empty_or_whitespace_only_lines() {
        let stream = vec![
            vec![span("  ", 150.0, 715.0, 8.0, 10.0)],
            vec![span(" ", 150.0, 704.0, 8.0, 10.0)],
        ];
        assert!(detect_stacked_fractions(&stream).is_empty());
    }

    #[test]
    fn stacked_fraction_rejects_right_aligned_numeric_column() {
        // ZUGFeRD fixture page 4: the `USt.-Betrag` column is right-aligned, so
        // two of its values (`61,07` over `8,93`) are consecutive short,
        // similar-sized, tightly-stacked numeric lines. They are not a
        // fraction: right-alignment leaves their box centres ~0.5 em apart.
        let stream = vec![
            vec![span("61,07", 528.6, 711.43, 9.75, 9.8)],
            vec![span("8,93", 533.5, 698.72, 9.75, 9.8)],
        ];
        assert!(
            detect_stacked_fractions(&stream).is_empty(),
            "right-aligned table numbers must not become a fraction"
        );
    }

    #[test]
    fn dense_fraction_cluster_is_rejected_but_isolated_hits_survive() {
        // Simulates the vertical reference-number strip: many tightly-packed
        // fraction-shaped hits in a row (indices 0..=7) must all be
        // discarded, while a single isolated hit far below (index 40/41)
        // must survive untouched.
        let cluster: Vec<FractionHit> = (0..4)
            .map(|k| FractionHit {
                numerator_line: k * 2,
                denominator_line: k * 2 + 1,
                expr: LatexExpr::fraction(LatexExpr::text("a"), LatexExpr::text("b")),
            })
            .collect();
        let isolated = FractionHit {
            numerator_line: 40,
            denominator_line: 41,
            expr: LatexExpr::fraction(LatexExpr::text("1"), LatexExpr::text("2")),
        };
        let mut hits = cluster;
        hits.push(isolated.clone());

        let kept = reject_dense_fraction_clusters(hits);
        assert_eq!(kept.len(), 1, "the dense 4-hit cluster must be fully discarded: {kept:?}");
        assert_eq!(kept[0].numerator_line, 40);
        assert_eq!(kept[0].expr.to_latex(), isolated.expr.to_latex());
    }

    #[test]
    fn two_isolated_fractions_are_both_kept() {
        // Below the 3-hit cluster threshold and far apart — a real document
        // with two unrelated formulas must not lose either one.
        let hits = vec![
            FractionHit {
                numerator_line: 0,
                denominator_line: 1,
                expr: LatexExpr::fraction(LatexExpr::text("1"), LatexExpr::text("2")),
            },
            FractionHit {
                numerator_line: 30,
                denominator_line: 31,
                expr: LatexExpr::fraction(LatexExpr::text("x"), LatexExpr::text("y")),
            },
        ];
        let kept = reject_dense_fraction_clusters(hits);
        assert_eq!(kept.len(), 2, "isolated fractions below the cluster-size threshold must survive: {kept:?}");
    }

    #[test]
    fn render_math_merges_fraction_into_inline_delimiters() {
        let stream = vec![
            vec![span("1", 150.0, 715.0, 8.0, 5.0)],
            vec![span("2", 150.0, 701.0, 8.0, 5.0)],
        ];
        let bars: Vec<RuleSeg> = vec![(708.0, 150.0, 170.0)];
        let text = render_math(&stream, &bars, 842.0, true, None);
        assert!(text.contains(r"$\frac{1}{2}$"), "got {text}");
    }

    // --- Cross-line super/subscript + stacked fraction (LayoutAST/tagged) ---

    #[test]
    fn cross_line_superscript_is_lifted() {
        // Base "x" (size 12, baseline 700) with the superscript "2" (size 8,
        // higher baseline 712) emitted as a separate visual line.
        let spans = vec![
            span("x", 100.0, 700.0, 12.0, 8.0),
            span("2", 110.0, 712.0, 8.0, 5.0),
        ];
        let text = synthesize_spans_math(&spans, &[]);
        assert_eq!(text, r"$x^{2}$", "got {text}");
    }

    #[test]
    fn cross_line_subscript_is_lifted() {
        let spans = vec![
            span("a", 100.0, 700.0, 12.0, 8.0),
            span("i", 108.0, 694.0, 8.0, 5.0),
        ];
        let text = synthesize_spans_math(&spans, &[]);
        assert_eq!(text, r"$a_{i}$", "got {text}");
    }

    #[test]
    fn stacked_fraction_without_bar_is_lifted() {
        // Numerator above denominator, same small size, horizontally centred,
        // tight vertical stack -> bar-free fraction.
        let spans = vec![
            span("1", 150.0, 715.0, 8.0, 5.0),
            span("2", 150.0, 701.0, 8.0, 5.0),
        ];
        let text = synthesize_spans_math(&spans, &[]);
        assert_eq!(text, r"$\frac{1}{2}$", "got {text}");
    }

    #[test]
    fn synthesize_spans_math_preserves_plain_lines() {
        // "E = mc" on the base baseline with the superscript "2" on a higher
        // baseline just right of "mc": only the trailing token becomes math.
        let spans = vec![
            span("E", 100.0, 700.0, 10.0, 8.0),
            span("=", 110.0, 700.0, 10.0, 8.0),
            span("mc", 120.0, 700.0, 10.0, 12.0),
            span("2", 134.0, 712.0, 7.0, 5.0),
        ];
        let text = synthesize_spans_math(&spans, &[]);
        assert!(text.contains(r"$mc^{2}$"), "got {text}");
        assert!(text.contains("E ="), "leading text must be preserved: {text}");
    }