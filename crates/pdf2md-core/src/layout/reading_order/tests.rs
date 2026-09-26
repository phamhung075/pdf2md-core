// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Reading order recovery, multi-column stream separation, and structured DocBlock generation.

    use super::*;
    use crate::layout::glyph_stream::Span;

    fn span(text: &str, x: f64, style: (bool, bool, bool)) -> Span {
        Span {
            text: text.to_string(),
            x,
            y: 700.0,
            size: 10.0,
            advance: text.len() as f64 * 6.0,
            word_advance: text.len() as f64 * 6.0,
            is_bold: style.0,
            is_italic: style.1,
            is_underline: style.2,
            is_vertical: false,
        }
    }

    /// A word + explicit space span + word (space spans are the common case
    /// from PDF producers).
    fn words_spaced(pairs: &[(&str, (bool, bool, bool))]) -> Vec<Span> {
        let mut v = Vec::new();
        let mut x = 100.0;
        for (i, (t, st)) in pairs.iter().enumerate() {
            if i > 0 {
                v.push(span(" ", x, (false, false, false)));
                x += 3.0;
            }
            v.push(span(t, x, *st));
            x += text_width(t) + 2.0;
        }
        v
    }

    fn text_width(t: &str) -> f64 {
        t.len() as f64 * 6.0
    }

    #[test]
    fn long_word_run_is_not_a_line_break() {
        // "Hello" is 30pt wide; "world" starts 33pt later, i.e. one normal
        // space after Hello's advance. The raw start-to-start distance is
        // 33 > 2.5 * size, but the *whitespace* gap is only 3pt, so the two
        // words belong on one line.
        let line = vec![
            span("Hello", 100.0, (false, false, false)),
            span("world", 133.0, (false, false, false)),
        ];
        assert_eq!(render_spans(&line), "Hello world");
    }

    #[test]
    fn wide_column_gutter_still_breaks_the_line() {
        // Second run starts 40pt after the first run's right edge: a genuine
        // column/element boundary must still become a hard newline.
        let line = vec![
            span("Hello", 100.0, (false, false, false)),
            span("world", 170.0, (false, false, false)),
        ];
        assert_eq!(render_spans(&line), "Hello\nworld");
    }

    #[test]
    fn unstyled_line_has_no_markers() {
        let line = words_spaced(&[("Hello", (false, false, false)), ("world", (false, false, false))]);
        assert_eq!(render_spans(&line), "Hello world");
    }

    #[test]
    fn bold_run_stays_open_across_words() {
        // A style run spanning several words must render as ONE emphasis span:
        // `**Bold text**`, not `**Bold** **text**`. (This test previously
        // asserted the fragmented form; that was the bug this change fixes.)
        let line = words_spaced(&[
            ("Bold", (true, false, false)),
            ("text", (true, false, false)),
        ]);
        assert_eq!(render_spans(&line), "**Bold text**");
    }

    #[test]
    fn adjacent_bold_spans_across_a_gap_merge() {
        // No explicit space span: the words are separated only by their x-gap.
        // "Word1" is 5*6=30pt wide and starts at 100; "Word2" starts at 133, so
        // the whitespace gap is 3pt (> 0.65 * space_adv, < 2.5 * size).
        let line = vec![
            span("Word1", 100.0, (true, false, false)),
            span("Word2", 133.0, (true, false, false)),
        ];
        assert_eq!(render_spans(&line), "**Word1 Word2**");
    }

    #[test]
    fn adjacent_bold_spans_across_a_space_span_merge() {
        // Same run, but the space arrives as its own whitespace-only span.
        let line = words_spaced(&[
            ("Word1", (true, false, false)),
            ("Word2", (true, false, false)),
        ]);
        assert_eq!(render_spans(&line), "**Word1 Word2**");
    }

    #[test]
    fn bold_then_unstyled_separates_cleanly() {
        let line = words_spaced(&[
            ("Bold", (true, false, false)),
            ("normal", (false, false, false)),
        ]);
        assert_eq!(render_spans(&line), "**Bold** normal");
    }

    #[test]
    fn bold_then_italic_separates_cleanly() {
        let line = words_spaced(&[
            ("Bold", (true, false, false)),
            ("Italic", (false, true, false)),
        ]);
        assert_eq!(render_spans(&line), "**Bold** *Italic*");
    }

    #[test]
    fn column_break_closes_style_and_breaks_line() {
        // Second run starts 40pt after the first run's right edge: a genuine
        // column/element boundary must still become a hard newline and close
        // the open emphasis before it.
        let line = vec![
            span("Hello", 100.0, (true, false, false)),
            span("world", 170.0, (true, false, false)),
        ];
        assert_eq!(render_spans(&line), "**Hello**\n**world**");
    }

    #[test]
    fn italic_run_uses_single_asterisk() {
        let line = words_spaced(&[("Note", (false, true, false))]);
        assert_eq!(render_spans(&line), "*Note*");
    }

    #[test]
    fn underline_run_uses_html_u() {
        let line = words_spaced(&[("Link", (false, false, true))]);
        assert_eq!(render_spans(&line), "<u>Link</u>");
    }

    #[test]
    fn style_transitions_close_and_reopen() {
        let line = words_spaced(&[
            ("Bold", (true, false, false)),
            ("normal", (false, false, false)),
            ("Italic", (false, true, false)),
        ]);
        assert_eq!(render_spans(&line), "**Bold** normal *Italic*");
    }

    #[test]
    fn explicit_space_span_never_emphasized() {
        let line = vec![
            span("BoldTail", 100.0, (true, false, false)),
            span(" ", 160.0, (true, false, false)), // space glyph, style ignored
            span("After", 165.0, (false, false, false)),
        ];
        assert_eq!(render_spans(&line), "**BoldTail** After");
    }

    #[test]
    fn embedded_trailing_space_is_moved_outside_bold_delimiters() {
        // Regression for SeConnecterAMonComptePartenaire.pdf: the producer draws
        // the bold run "2 " (digit + trailing space) as ONE glyph run, so the
        // space is not its own span and the `is_space` branch never fires. It
        // used to be pushed inside the emphasis, producing the invalid `**2 **`
        // (a closing delimiter preceded by whitespace), which also swallowed the
        // following `**mail**` run. The space must land after the closing `**`.
        let line = vec![
            span("dans ", 100.0, (false, false, false)),
            span("2 ", 130.0, (true, false, false)),
            span("mail", 142.0, (false, false, false)),
        ];
        let rendered = render_spans(&line);
        assert_eq!(rendered, "dans **2** mail");
        assert!(
            !rendered.contains("2 **"),
            "closing delimiter must not be preceded by a space: {rendered:?}"
        );
    }

    #[test]
    fn embedded_trailing_space_is_moved_out_before_a_different_style() {
        // Same pathology when the next span switches style: the bold run must
        // close flush against "2" and the space live between the two runs.
        let line = vec![
            span("dans ", 100.0, (false, false, false)),
            span("2 ", 130.0, (true, false, false)),
            span("mail", 142.0, (false, true, false)),
        ];
        let rendered = render_spans(&line);
        assert_eq!(rendered, "dans **2** *mail*");
        assert!(!rendered.contains("2 **"), "{rendered:?}");
    }

    #[test]
    fn embedded_leading_space_is_moved_outside_bold_delimiters() {
        // Mirror case: a span whose own text begins with a space (`" 2"`).
        // An opening delimiter followed by whitespace (`** 2**`) is not
        // emphasis either; the space belongs before the opening `**`.
        let line = vec![
            span(" 2", 100.0, (true, false, false)),
            span("mail", 120.0, (false, false, false)),
        ];
        let rendered = render_spans(&line);
        assert_eq!(rendered, "**2** mail");
        assert!(
            !rendered.starts_with("** "),
            "opening delimiter must not be followed by a space: {rendered:?}"
        );
    }

    #[test]
    fn projection_columns_keep_gutter_straddling_span() {
        // Regression for fnfe_Facture_FR_MINIMUM.pdf: `detect_projection_two_columns`
        // assigned each column half with two independent half-open filters
        // (`x + advance <= gx` and `x >= gx`). A span whose box *straddles* the
        // page gutter satisfied neither test, so its text was dropped from BOTH
        // columns — silently deleting the middle `0` of `120 000,00 €` (and the
        // `d` of `Fiducial`) from the structured `blocks` channel while the
        // Markdown, rendered via the band path, kept it. Every span must land in
        // exactly one side.
        fn mk(text: &str, x: f64, adv: f64, y: f64) -> Span {
            Span {
                text: text.to_string(),
                x,
                y,
                size: 10.0,
                advance: adv,
                word_advance: adv,
                is_bold: false,
                is_italic: false,
                is_underline: false,
                is_vertical: false,
            }
        }
        let mut lines: Vec<Vec<Span>> = Vec::new();
        for i in 0..4 {
            let y = 700.0 - 10.0 * i as f64;
            // Left column [10,65]; right column [100,160]; the "Z" span
            // [66,86] straddles the resulting gx = 82.5.
            lines.push(vec![
                mk("L", 10.0, 55.0, y),
                mk("Z", 66.0, 20.0, y),
                mk("R", 100.0, 60.0, y),
            ]);
        }
        // Left-only and right-only rows give the projection enough support.
        lines.push(vec![mk("L", 10.0, 55.0, 660.0)]);
        lines.push(vec![mk("R", 100.0, 60.0, 650.0)]);

        let pc = detect_projection_two_columns(&lines)
            .expect("a consistent two-column projection must be detected");
        let kept: String = pc
            .left
            .iter()
            .chain(pc.right.iter())
            .flat_map(|row| row.iter())
            .map(|s| s.text.as_str())
            .collect();
        assert_eq!(
            kept.matches('Z').count(),
            4,
            "a gutter-straddling span must be kept whole in one column: {:?}",
            pc.left
                .iter()
                .map(|r| r.iter().map(|s| s.text.clone()).collect::<Vec<_>>())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn projection_columns_keep_unsplittable_straddling_line() {
        // A whole line emitted as ONE span (PDF producers commonly draw
        // `TVA Intracommunautaire : FR23391284650` as a single TJ run)
        // crosses the column gutter, so the total-complement partition puts
        // the entire span on one side and leaves the other half empty. The
        // `both halves non-empty` guard then sends the row to the top/bottom
        // fallback, which only accepts rows at the block's edge — so a
        // mid-block row vanished from BOTH reading-order streams (and from
        // the `blocks` channel) while the Markdown, rendered through the
        // separate band path, kept it. Unlike the multi-span straddler
        // covered by `projection_columns_keep_gutter_straddling_span`, no
        // split of this row can produce two non-empty halves, so it must be
        // kept whole on the side its box leans to.
        fn mk(text: &str, x: f64, adv: f64, y: f64) -> Span {
            Span {
                text: text.to_string(),
                x,
                y,
                size: 10.0,
                advance: adv,
                word_advance: adv,
                is_bold: false,
                is_italic: false,
                is_underline: false,
                is_vertical: false,
            }
        }
        // Left column [10,65], right column [100,160] -> gx = 82.5.
        // The left-only / right-only rows set col_top=690, col_bottom=660 so
        // the straddler at y=675 is strictly *inside* the column block.
        let lines: Vec<Vec<Span>> = vec![
            vec![mk("L", 10.0, 55.0, 700.0), mk("R", 100.0, 60.0, 700.0)],
            vec![mk("A", 10.0, 55.0, 690.0)],
            vec![mk("B", 100.0, 60.0, 680.0)],
            vec![mk("TVA Intracommunautaire : FR23391284650", 10.0, 130.0, 675.0)],
            vec![mk("C", 10.0, 55.0, 670.0)],
            vec![mk("D", 100.0, 60.0, 660.0)],
        ];
        let pc = detect_projection_two_columns(&lines)
            .expect("a consistent two-column projection must be detected");
        let kept: String = pc
            .left
            .iter()
            .chain(pc.right.iter())
            .flat_map(|row| row.iter())
            .map(|s| s.text.as_str())
            .collect();
        assert!(
            kept.contains("TVA Intracommunautaire"),
            "a single-span line crossing the gutter must not vanish: left={:?} right={:?} top={:?} bottom={:?}",
            pc.left.iter().map(|r| r.iter().map(|s| s.text.clone()).collect::<Vec<_>>()).collect::<Vec<_>>(),
            pc.right.iter().map(|r| r.iter().map(|s| s.text.clone()).collect::<Vec<_>>()).collect::<Vec<_>>(),
            pc.top_full.iter().map(|r| r.iter().map(|s| s.text.clone()).collect::<Vec<_>>()).collect::<Vec<_>>(),
            pc.bottom_full.iter().map(|r| r.iter().map(|s| s.text.clone()).collect::<Vec<_>>()).collect::<Vec<_>>(),
        );
    }

    #[test]
    fn projection_columns_reject_centered_words_split_across_runs() {
        // Regression for the FR "Statut EI et régime micro entreprise" slide
        // deck: every visual line is a centered single-column sentence, but the
        // producer emits each word as several `Tj` runs (`Le statut et le
        // r|égim|e`). `detect_projection_two_columns` dropped the straddling run
        // from *both* halves, measured that run's own width as a white gutter,
        // and read the whole slide as two columns — emitting every line's tail
        // after every line's head (`# Le statut et le r` … `# égime`). A
        // straddler that bridges `l_end`..`r_start` is filler, not a gutter.
        fn mk(text: &str, x: f64, adv: f64, y: f64) -> Span {
            Span {
                text: text.to_string(),
                x,
                y,
                size: 10.0,
                advance: adv,
                word_advance: adv,
                is_bold: false,
                is_italic: false,
                is_underline: false,
                is_vertical: false,
            }
        }
        let mut lines: Vec<Vec<Span>> = Vec::new();
        for i in 0..4 {
            let y = 700.0 - 10.0 * i as f64;
            lines.push(vec![
                mk("Le statut et le r", 10.0, 55.0, y),
                mk("égim", 65.0, 30.0, y),
                mk("e ", 95.0, 65.0, y),
            ]);
        }
        // A head-only and a tail-only row give the projection its usual support.
        lines.push(vec![mk("Le statut et le r", 10.0, 55.0, 660.0)]);
        lines.push(vec![mk("e ", 95.0, 65.0, 650.0)]);

        assert!(
            detect_projection_two_columns(&lines).is_none(),
            "a straddling run that bridges the gap must not be read as a column gutter"
        );
        let streams = page_read_order(&lines);
        assert_eq!(streams.len(), 1, "centered prose must stay one column");
        let joined: String = streams[0]
            .iter()
            .flat_map(|l| l.iter())
            .map(|s| s.text.as_str())
            .collect();
        assert!(
            joined.contains("Le statut et le régime"),
            "the words must stay intact in reading order, got {joined:?}"
        );
    }