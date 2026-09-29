    use super::*;
    use crate::models::BoundingBox;

    fn sp_at(text: &str, x: f64, y: f64, advance: f64) -> Span {
        Span {
            text: text.to_string(),
            x,
            y,
            size: 10.0,
            advance,
            word_advance: advance,
            is_bold: false,
            is_italic: false,
            is_underline: false,
            is_vertical: false,
        }
    }

    /// Four drawn rules at x = 10, 50, 90, 130 spanning y = 0..100.
    fn frame_rules() -> Vec<(f64, f64, f64)> {
        vec![(10.0, 0.0, 100.0), (50.0, 0.0, 100.0), (90.0, 0.0, 100.0), (130.0, 0.0, 100.0)]
    }

    fn table_of(hit: &TableHit) -> Vec<Vec<String>> {
        hit.rows.clone()
    }

    /// A minimal existing generic hit used to seed the pass: the frame model
    /// only rebuilds a table the generic geometry already found.
    fn seed(
        start: usize,
        end: usize,
        ncols: usize,
        x0: f64,
        x1: f64,
        y0: f64,
        y1: f64,
    ) -> TableHit {
        // One row per spanned line: a hit showing fewer rows than the lines it
        // spans is a folded table and is deliberately left to the pass that
        // produced it.
        let rows = vec![vec![String::new(); ncols]; end - start + 1];
        TableHit {
            start,
            end,
            rows,
            bbox: BoundingBox::new(x0, y0, x1, y1),
        }
    }

    #[test]
    fn sidebar_outside_the_right_rule_is_excluded() {
        // Three-column frame x=10..130; a sidebar value at x=140 is outside.
        let lines = vec![
            vec![
                sp_at("H1", 12.0, 90.0, 10.0),
                sp_at("H2", 52.0, 90.0, 10.0),
                sp_at("H3", 92.0, 90.0, 10.0),
                sp_at("SIDE", 135.0, 90.0, 20.0),
            ],
            vec![
                sp_at("a", 12.0, 70.0, 6.0),
                sp_at("1", 52.0, 70.0, 6.0),
                sp_at("2", 92.0, 70.0, 6.0),
                sp_at("999", 135.0, 70.0, 18.0),
            ],
            vec![
                sp_at("b", 12.0, 50.0, 6.0),
                sp_at("3", 52.0, 50.0, 6.0),
                sp_at("4", 92.0, 50.0, 6.0),
            ],
        ];
        let hits = apply_ruled_frame_model(&lines, vec![seed(1, 2, 3, 12.0, 110.0, 70.0, 50.0)], &frame_rules(), &[]);
        assert_eq!(hits.len(), 1);
        let rows = table_of(&hits[0]);
        assert_eq!(rows[0], vec!["H1", "H2", "H3"]);
        assert!(
            rows.iter().all(|r| !r.iter().any(|c| c.contains("SIDE") || c.contains("999"))),
            "sidebar text joined the table: {rows:?}"
        );
        assert_eq!(hits[0].bbox.x1, 130.0);
    }

    #[test]
    fn rows_after_a_vertical_gap_are_kept() {
        // Header + one row, a large empty gap, then two more rows: the sidebar
        // line inside the gap must not end the table.
        let lines = vec![
            vec![
                sp_at("H1", 12.0, 90.0, 10.0),
                sp_at("H2", 52.0, 90.0, 10.0),
                sp_at("H3", 92.0, 90.0, 10.0),
            ],
            vec![sp_at("a", 12.0, 70.0, 6.0), sp_at("1", 52.0, 70.0, 6.0), sp_at("2", 92.0, 70.0, 6.0)],
            // Gap: only text outside the frame.
            vec![sp_at("note", 135.0, 45.0, 20.0)],
            vec![sp_at("b", 12.0, 20.0, 6.0), sp_at("3", 52.0, 20.0, 6.0), sp_at("4", 92.0, 20.0, 6.0)],
        ];
        let hits = apply_ruled_frame_model(&lines, vec![seed(1, 3, 3, 12.0, 110.0, 70.0, 20.0)], &frame_rules(), &[]);
        assert_eq!(hits.len(), 1, "the frame must survive the gap");
        let rows = table_of(&hits[0]);
        assert_eq!(rows.len(), 3, "rows after the gap were dropped: {rows:?}");
        assert_eq!(rows[2], vec!["b", "3", "4"]);
        assert_eq!(hits[0].end, 3, "the hit must span the gap to the last row");
    }

    #[test]
    fn wrapped_header_cell_is_joined_per_column() {
        let lines = vec![
            vec![sp_at("Taux", 12.0, 92.0, 20.0), sp_at("Montant", 92.0, 92.0, 30.0)],
            vec![sp_at("salarial", 12.0, 88.0, 30.0), sp_at("patronal", 92.0, 88.0, 30.0)],
            vec![sp_at("x", 12.0, 70.0, 6.0), sp_at("1", 52.0, 70.0, 6.0), sp_at("2", 92.0, 70.0, 6.0)],
            vec![sp_at("y", 12.0, 50.0, 6.0), sp_at("3", 52.0, 50.0, 6.0), sp_at("4", 92.0, 50.0, 6.0)],
        ];
        let hits = apply_ruled_frame_model(&lines, vec![seed(2, 3, 3, 12.0, 130.0, 70.0, 50.0)], &frame_rules(), &[]);
        assert_eq!(hits.len(), 1);
        let rows = table_of(&hits[0]);
        assert_eq!(rows.len(), 3, "wrapped header must be one row: {rows:?}");
        assert!(
            rows[0][0].contains(CELL_LINE_BREAK_PENDING) && rows[0][0].contains("Taux"),
            "header cell not joined: {rows:?}"
        );
        assert!(rows[0][2].contains("Montant"), "second header cell lost: {rows:?}");
    }

    #[test]
    fn numbers_across_a_rule_are_not_joined() {
        // Two rules at x=50 and x=90; a value starting just left of 90 must stay
        // in its own column, and the two numbers must not fuse.
        let lines = vec![
            vec![sp_at("A", 12.0, 90.0, 6.0), sp_at("B", 52.0, 90.0, 6.0), sp_at("C", 92.0, 90.0, 6.0)],
            vec![
                sp_at("111", 12.0, 70.0, 18.0),
                sp_at("222", 52.0, 70.0, 18.0),
                sp_at("333", 92.0, 70.0, 18.0),
            ],
        ];
        let hits = apply_ruled_frame_model(&lines, vec![seed(1, 1, 3, 12.0, 110.0, 70.0, 70.0)], &frame_rules(), &[]);
        assert_eq!(hits.len(), 1);
        let rows = table_of(&hits[0]);
        assert_eq!(rows[1], vec!["111", "222", "333"]);
    }

    #[test]
    fn page_border_is_not_adopted_as_a_frame_edge() {
        // The page border at x=200 spans far past the interior band and must be
        // rejected; the frame edges stay at 10/130.
        let mut rules = frame_rules();
        rules.push((200.0, -300.0, 400.0));
        let lines = vec![
            vec![sp_at("H1", 12.0, 90.0, 6.0), sp_at("H2", 52.0, 90.0, 6.0), sp_at("H3", 92.0, 90.0, 6.0)],
            vec![sp_at("a", 12.0, 70.0, 6.0), sp_at("1", 52.0, 70.0, 6.0), sp_at("2", 92.0, 70.0, 6.0)],
            vec![sp_at("b", 12.0, 50.0, 6.0), sp_at("3", 52.0, 50.0, 6.0), sp_at("4", 92.0, 50.0, 6.0)],
        ];
        let hits = apply_ruled_frame_model(&lines, vec![seed(1, 2, 3, 12.0, 110.0, 70.0, 50.0)], &rules, &[]);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].bbox.x1, 130.0, "page border adopted as the frame edge");
    }

    #[test]
    fn left_page_border_is_adopted_when_it_holds_the_first_column() {
        // The frame's column rules span y=0..100; the page border at x=0 spans
        // far beyond and is dropped by the overshoot filter, yet it is the
        // table's own left rule. Content in the strip x=0..10 makes it the left
        // edge so the first column is not lost.
        let mut rules = frame_rules();
        rules.push((0.0, -200.0, 400.0));
        let lines = vec![
            vec![sp_at("H1", 2.0, 90.0, 6.0), sp_at("H2", 52.0, 90.0, 6.0), sp_at("H3", 92.0, 90.0, 6.0)],
            vec![sp_at("a", 2.0, 70.0, 6.0), sp_at("1", 52.0, 70.0, 6.0), sp_at("2", 92.0, 70.0, 6.0)],
            vec![sp_at("b", 2.0, 50.0, 6.0), sp_at("3", 52.0, 50.0, 6.0), sp_at("4", 92.0, 50.0, 6.0)],
        ];
        let hits = apply_ruled_frame_model(&lines, vec![seed(1, 2, 4, 2.0, 110.0, 70.0, 50.0)], &rules, &[]);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].bbox.x0, 0.0, "page-border left edge not adopted");
        // The adopted edge adds the x=0..10 column, which holds "H1".
        assert_eq!(hits[0].rows[0].len(), 4);
        assert_eq!(hits[0].rows[0][0], "H1");
        assert_eq!(hits[0].rows[0][3], "H3");
    }

    #[test]
    fn distant_page_border_without_content_is_not_adopted() {
        // Same page border at x=0, but every word sits at x >= 12: the empty
        // strip must not become an all-empty first column.
        let mut rules = frame_rules();
        rules.push((0.0, -200.0, 400.0));
        let lines = vec![
            vec![sp_at("H1", 12.0, 90.0, 6.0), sp_at("H2", 52.0, 90.0, 6.0), sp_at("H3", 92.0, 90.0, 6.0)],
            vec![sp_at("a", 12.0, 70.0, 6.0), sp_at("1", 52.0, 70.0, 6.0), sp_at("2", 92.0, 70.0, 6.0)],
            vec![sp_at("b", 12.0, 50.0, 6.0), sp_at("3", 52.0, 50.0, 6.0), sp_at("4", 92.0, 50.0, 6.0)],
        ];
        let hits = apply_ruled_frame_model(&lines, vec![seed(1, 2, 3, 12.0, 110.0, 70.0, 50.0)], &rules, &[]);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].bbox.x0, 10.0, "empty page-border column was adopted");
    }

    #[test]
    fn insufficient_rules_make_no_table() {
        // One interior rule (two columns) is below the frame threshold.
        let rules = vec![(10.0, 0.0, 100.0), (70.0, 0.0, 100.0), (130.0, 0.0, 100.0)];
        let lines = vec![
            vec![sp_at("H1", 12.0, 90.0, 6.0), sp_at("H2", 72.0, 90.0, 6.0)],
            vec![sp_at("a", 12.0, 70.0, 6.0), sp_at("1", 72.0, 70.0, 6.0)],
        ];
        let hits = apply_ruled_frame_model(&lines, Vec::new(), &rules, &[]);
        assert!(hits.is_empty(), "a two-column rule pair is not a frame");
    }

    #[test]
    fn generic_hit_inside_the_frame_is_replaced_not_doubled() {
        let lines = vec![
            vec![sp_at("H1", 12.0, 90.0, 6.0), sp_at("H2", 52.0, 90.0, 6.0), sp_at("H3", 92.0, 90.0, 6.0)],
            vec![sp_at("a", 12.0, 70.0, 6.0), sp_at("1", 52.0, 70.0, 6.0), sp_at("2", 92.0, 70.0, 6.0)],
            vec![sp_at("b", 12.0, 50.0, 6.0), sp_at("3", 52.0, 50.0, 6.0), sp_at("4", 92.0, 50.0, 6.0)],
        ];
        let stale = TableHit {
            start: 1,
            end: 2,
            rows: vec![
                vec!["stale".into(), String::new(), String::new()],
                vec![String::new(), String::new(), String::new()],
            ],
            bbox: BoundingBox::new(12.0, 70.0, 130.0, 50.0),
        };
        let hits = apply_ruled_frame_model(&lines, vec![stale], &frame_rules(), &[]);
        assert_eq!(hits.len(), 1);
        assert!(hits[0].rows.iter().all(|r| !r.iter().any(|c| c.contains("stale"))));
    }

    /// A prose column left of the frame shares the frame's rows. The generic hit
    /// spans prose + frame, but the frame must still re-cut: the prose falls
    /// outside the rebuilt hit's x-range (where the renderer recovers it) and
    /// must not become a table column.
    #[test]
    fn prose_strip_left_of_a_frame_is_cut_away() {
        let lines = vec![
            vec![sp_at("H1", 12.0, 90.0, 6.0), sp_at("H2", 52.0, 90.0, 6.0), sp_at("H3", 92.0, 90.0, 6.0)],
            vec![
                sp_at("three prose words here", 2.0, 70.0, 4.0),
                sp_at("a", 12.0, 70.0, 6.0),
                sp_at("1", 52.0, 70.0, 6.0),
                sp_at("2", 92.0, 70.0, 6.0),
            ],
            vec![
                sp_at("four more prose words now", 2.0, 50.0, 4.0),
                sp_at("b", 12.0, 50.0, 6.0),
                sp_at("3", 52.0, 50.0, 6.0),
                sp_at("4", 92.0, 50.0, 6.0),
            ],
        ];
        let hits = apply_ruled_frame_model(&lines, vec![seed(1, 2, 3, 2.0, 110.0, 70.0, 50.0)], &frame_rules(), &[]);
        assert_eq!(hits.len(), 1, "the prose-strip frame was refused");
        assert_eq!(hits[0].bbox.x0, 10.0, "frame left rule must bound the table");
        let rows = table_of(&hits[0]);
        assert!(
            rows.iter().all(|r| !r.iter().any(|c| c.contains("prose") || c.contains("here"))),
            "prose joined the table: {rows:?}"
        );
        assert_eq!(rows[0], vec!["H1", "H2", "H3"]);
    }

    /// The mirror case: when the excluded strip is itself a column of values,
    /// the frame does not enclose the whole table and the re-cut is refused.
    #[test]
    fn numeric_strip_left_of_a_frame_is_not_cut_away() {
        let lines = vec![
            vec![sp_at("H1", 12.0, 90.0, 6.0), sp_at("H2", 52.0, 90.0, 6.0), sp_at("H3", 92.0, 90.0, 6.0)],
            vec![
                sp_at("111,11", 2.0, 70.0, 6.0),
                sp_at("a", 12.0, 70.0, 6.0),
                sp_at("1", 52.0, 70.0, 6.0),
                sp_at("2", 92.0, 70.0, 6.0),
            ],
            vec![
                sp_at("222,22", 2.0, 50.0, 6.0),
                sp_at("b", 12.0, 50.0, 6.0),
                sp_at("3", 52.0, 50.0, 6.0),
                sp_at("4", 92.0, 50.0, 6.0),
            ],
        ];
        let hits = apply_ruled_frame_model(&lines, vec![seed(1, 2, 3, 2.0, 110.0, 70.0, 50.0)], &frame_rules(), &[]);
        assert_eq!(hits[0].bbox.x0, 2.0, "a numeric strip must not be cut away");
    }

    /// Two tables stacked with identical column rules must each be re-cut: the
    /// rules are one visual frame per section, not one tall frame over both.
    #[test]
    fn stacked_frames_sharing_columns_are_both_recut() {
        let rules = vec![
            (10.0, 60.0, 100.0),
            (50.0, 60.0, 100.0),
            (90.0, 60.0, 100.0),
            (130.0, 60.0, 100.0),
            (10.0, 0.0, 40.0),
            (50.0, 0.0, 40.0),
            (90.0, 0.0, 40.0),
            (130.0, 0.0, 40.0),
        ];
        let lines = vec![
            vec![sp_at("H1", 12.0, 90.0, 6.0), sp_at("H2", 52.0, 90.0, 6.0), sp_at("H3", 92.0, 90.0, 6.0)],
            vec![sp_at("a", 12.0, 70.0, 6.0), sp_at("1", 52.0, 70.0, 6.0), sp_at("2", 92.0, 70.0, 6.0)],
            vec![sp_at("J1", 12.0, 30.0, 6.0), sp_at("J2", 52.0, 30.0, 6.0), sp_at("J3", 92.0, 30.0, 6.0)],
            vec![sp_at("b", 12.0, 10.0, 6.0), sp_at("3", 52.0, 10.0, 6.0), sp_at("4", 92.0, 10.0, 6.0)],
        ];
        let hits = apply_ruled_frame_model(
            &lines,
            vec![
                seed(1, 1, 3, 12.0, 110.0, 70.0, 70.0),
                seed(3, 3, 3, 12.0, 110.0, 10.0, 10.0),
            ],
            &rules,
            &[],
        );
        assert_eq!(hits.len(), 2, "both stacked sections must be re-cut: {hits:?}");
    }

    /// The excluded strip is a table's numbered label column ("13-Bases"): it
    /// must stay inside the table, so the frame re-cut is refused.
    #[test]
    fn numbered_label_strip_left_of_a_frame_is_not_cut_away() {
        let lines = vec![
            vec![sp_at("H1", 12.0, 90.0, 6.0), sp_at("H2", 52.0, 90.0, 6.0), sp_at("H3", 92.0, 90.0, 6.0)],
            vec![
                sp_at("13-Bases", 2.0, 70.0, 6.0),
                sp_at("a", 12.0, 70.0, 6.0),
                sp_at("1", 52.0, 70.0, 6.0),
                sp_at("2", 92.0, 70.0, 6.0),
            ],
            vec![
                sp_at("14-dont", 2.0, 50.0, 6.0),
                sp_at("b", 12.0, 50.0, 6.0),
                sp_at("3", 52.0, 50.0, 6.0),
                sp_at("4", 92.0, 50.0, 6.0),
            ],
        ];
        let hits = apply_ruled_frame_model(&lines, vec![seed(1, 2, 3, 2.0, 110.0, 70.0, 50.0)], &frame_rules(), &[]);
        assert_eq!(hits[0].bbox.x0, 2.0, "a numbered-label strip must not be cut away");
    }

    /// A frame that covers the hit (excludes nothing) but offers fewer columns
    /// must not re-cut it: a lower column count would merge real cells.
    #[test]
    fn frame_covering_the_hit_with_fewer_columns_is_refused() {
        let lines = vec![
            vec![sp_at("H1", 12.0, 90.0, 6.0), sp_at("H2", 52.0, 90.0, 6.0), sp_at("H3", 92.0, 90.0, 6.0)],
            vec![
                sp_at("a", 12.0, 70.0, 6.0),
                sp_at("x", 40.0, 70.0, 6.0),
                sp_at("1", 52.0, 70.0, 6.0),
                sp_at("2", 92.0, 70.0, 6.0),
            ],
            vec![
                sp_at("b", 12.0, 50.0, 6.0),
                sp_at("y", 40.0, 50.0, 6.0),
                sp_at("3", 52.0, 50.0, 6.0),
                sp_at("4", 92.0, 50.0, 6.0),
            ],
        ];
        // Hit shows 4 columns; the frame offers 3 and covers the hit.
        let hits = apply_ruled_frame_model(&lines, vec![seed(1, 2, 4, 10.0, 130.0, 70.0, 50.0)], &frame_rules(), &[]);
        assert_eq!(hits.len(), 1);
        assert!(
            hits[0].rows.iter().all(|r| r.len() == 4),
            "a covered hit must not be re-cut to fewer columns: {:?}",
            hits[0].rows
        );
    }

    /// Rules at every `x` in `xs` spanning `y0..y1`.
    fn rules_at(xs: &[f64], y0: f64, y1: f64) -> Vec<(f64, f64, f64)> {
        xs.iter().map(|&x| (x, y0, y1)).collect()
    }

    /// A grid column that is empty in every row must still be a column: the
    /// drawn rules define the count, so no two real cells may fuse around it.
    #[test]
    fn sparse_grid_keeps_the_drawn_column_count() {
        let rules = rules_at(&[0.0, 40.0, 80.0, 120.0, 160.0], 0.0, 100.0);
        let lines = vec![
            vec![
                sp_at("H1", 5.0, 90.0, 6.0),
                sp_at("H2", 45.0, 90.0, 6.0),
                sp_at("H4", 125.0, 90.0, 6.0),
            ],
            vec![
                sp_at("a", 5.0, 70.0, 6.0),
                sp_at("1", 45.0, 70.0, 6.0),
                sp_at("2", 125.0, 70.0, 6.0),
            ],
            vec![
                sp_at("b", 5.0, 50.0, 6.0),
                sp_at("3", 45.0, 50.0, 6.0),
                sp_at("4", 125.0, 50.0, 6.0),
            ],
        ];
        // The generic pass saw two columns; the drawn cell grid states four.
        let rects: Vec<(f64, f64, f64, f64)> = (0..4)
            .map(|c| (40.0 * c as f64, 0.0, 40.0 * c as f64 + 40.0, 100.0))
            .collect();
        let hits = apply_ruled_frame_model(
            &lines,
            vec![seed(1, 2, 2, 0.0, 160.0, 70.0, 50.0)],
            &rules,
            &rects,
        );
        assert_eq!(hits.len(), 1);
        assert!(
            hits[0].rows.iter().all(|r| r.len() == 4),
            "the empty drawn column collapsed: {:?}",
            hits[0].rows
        );
        assert_eq!(hits[0].rows[0], vec!["H1", "H2", "", "H4"]);
    }

    /// A full-width section title line is one row of the ruled table, not a
    /// reason to reject the frame; the sparse data rows below stay rows.
    #[test]
    fn section_title_rows_between_data_rows_survive() {
        let rules = rules_at(&[0.0, 40.0, 80.0, 120.0], 0.0, 100.0);
        let lines = vec![
            vec![
                sp_at("H1", 5.0, 90.0, 6.0),
                sp_at("H2", 45.0, 90.0, 6.0),
                sp_at("H3", 85.0, 90.0, 6.0),
            ],
            vec![sp_at("SECTION", 5.0, 75.0, 6.0)],
            vec![
                sp_at("a", 5.0, 60.0, 6.0),
                sp_at("1", 45.0, 60.0, 6.0),
                sp_at("2", 85.0, 60.0, 6.0),
            ],
            vec![
                sp_at("b", 5.0, 40.0, 6.0),
                sp_at("3", 45.0, 40.0, 6.0),
                sp_at("4", 85.0, 40.0, 6.0),
            ],
        ];
        let hits =
            apply_ruled_frame_model(&lines, vec![seed(1, 3, 3, 0.0, 120.0, 75.0, 40.0)], &rules, &[]);
        assert_eq!(hits.len(), 1, "the frame was rejected around a section row");
        let rows = table_of(&hits[0]);
        assert!(
            rows.iter().any(|r| r.iter().any(|c| c.contains("SECTION"))),
            "the section row was dropped: {rows:?}"
        );
        assert_eq!(hits[0].rows[0], vec!["H1", "H2", "H3"]);
    }

    /// A grid stated only by drawn cell rectangles (no long column rules) is
    /// found from those stacked edges.
    #[test]
    fn cell_rectangles_supply_the_grid_rules() {
        // Seven columns x four rows of adjoining cells.
        let mut rects: Vec<(f64, f64, f64, f64)> = Vec::new();
        for row in 0..4 {
            let y0 = 25.0 * row as f64;
            for col in 0..7 {
                let x0 = 30.0 * col as f64;
                rects.push((x0, y0, x0 + 30.0, y0 + 25.0));
            }
        }
        let header: Vec<Span> = (0..7)
            .map(|c| sp_at(&format!("H{c}"), 5.0 + 30.0 * c as f64, 90.0, 6.0))
            .collect();
        let data_row = |y: f64, base: usize| -> Vec<Span> {
            (0..7)
                .map(|c| sp_at(&format!("{}", base + c), 5.0 + 30.0 * c as f64, y, 6.0))
                .collect()
        };
        let lines = vec![
            header,
            data_row(65.0, 10),
            data_row(40.0, 20),
            data_row(15.0, 30),
        ];
        let hits = apply_ruled_frame_model(&lines, Vec::new(), &[], &rects);
        assert_eq!(hits.len(), 1, "the drawn cell rectangles did not form a frame");
        assert!(hits[0].rows.iter().all(|r| r.len() == 7), "{:?}", hits[0].rows);
    }

    /// A box drawn around prose is not a table: a lone box has two vertical
    /// edges and a three-column box is below the creation floor.
    #[test]
    fn a_box_around_prose_is_not_turned_into_a_table() {
        let prose = vec![
            vec![sp_at("one two three four", 5.0, 80.0, 60.0)],
            vec![sp_at("five six seven eight", 5.0, 60.0, 60.0)],
            vec![sp_at("nine ten eleven twelve", 5.0, 40.0, 60.0)],
            vec![sp_at("thirteen fourteen fifteen", 5.0, 20.0, 60.0)],
        ];
        // A box: only its two edges.
        let box_rules = rules_at(&[10.0, 130.0], 0.0, 100.0);
        assert!(
            apply_ruled_frame_model(&prose, Vec::new(), &box_rules, &[]).is_empty(),
            "a lone box became a table"
        );
        // A three-column box is still below the creation floor (seven columns).
        let three = rules_at(&[10.0, 50.0, 90.0, 130.0], 0.0, 100.0);
        assert!(
            apply_ruled_frame_model(&prose, Vec::new(), &three, &[]).is_empty(),
            "a three-column prose box became a table"
        );
    }

