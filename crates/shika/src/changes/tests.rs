use super::keys::{Scroll, first_in_view, scroll_top};
use super::refresh::error_line;
use super::*;
use shika_core::{Collapse, DiffLine, DiffStat, FileDiff, FileStatus, Hunk, LineKind, SessionDiff};

fn line(kind: LineKind, old: Option<u32>, new: Option<u32>, text: &str) -> DiffLine {
    DiffLine {
        kind,
        old,
        new,
        text: text.into(),
    }
}

fn file(path: &str, hunks: Vec<Hunk>) -> FileDiff {
    FileDiff {
        path: path.into(),
        hunks,
        ..Default::default()
    }
}

fn sample() -> SessionDiff {
    let a = file(
        "src/a.rs",
        vec![
            Hunk {
                header: "@@ -1,2 +1,2 @@".into(),
                lines: vec![
                    line(LineKind::Context, Some(1), Some(1), "fn a() {"),
                    line(LineKind::Removed, Some(2), None, "\told"),
                    line(LineKind::Added, None, Some(2), "\tnew"),
                ],
            },
            Hunk {
                header: "@@ -40,1 +40,2 @@".into(),
                lines: vec![
                    line(LineKind::Context, Some(40), Some(40), "}"),
                    line(LineKind::Added, None, Some(12345), "x"),
                    line(
                        LineKind::NoNewline,
                        None,
                        None,
                        "\\ No newline at end of file",
                    ),
                ],
            },
        ],
    );
    let binary = FileDiff {
        path: "logo.png".into(),
        binary: true,
        ..Default::default()
    };
    let big = FileDiff {
        path: "big.txt".into(),
        collapsed: Some(Collapse::Lines),
        hidden_lines: 4812,
        insertions: 4812,
        ..Default::default()
    };
    let mode = file("run.sh", vec![]);
    SessionDiff {
        files: vec![a, binary, big, mode],
        stat: DiffStat {
            files: 4,
            insertions: 4814,
            deletions: 1,
        },
    }
}

#[test]
fn rows_flatten_files_hunks_and_states_in_order() {
    let view = DiffView::new(sample());
    assert_eq!(
        view.rows,
        vec![
            Row::File(0),
            Row::FileMeta(0),
            Row::Line {
                file: 0,
                hunk: 0,
                line: 0
            },
            Row::Line {
                file: 0,
                hunk: 0,
                line: 1
            },
            Row::Line {
                file: 0,
                hunk: 0,
                line: 2
            },
            Row::HunkGap(0),
            Row::Line {
                file: 0,
                hunk: 1,
                line: 0
            },
            Row::Line {
                file: 0,
                hunk: 1,
                line: 1
            },
            Row::Line {
                file: 0,
                hunk: 1,
                line: 2
            },
            Row::End(0),
            Row::Spacer,
            Row::File(1),
            Row::FileMeta(1),
            Row::Binary(1),
            Row::End(1),
            Row::Spacer,
            Row::File(2),
            Row::FileMeta(2),
            Row::Collapsed(2),
            Row::End(2),
            Row::Spacer,
            Row::File(3),
            Row::FileMeta(3),
            Row::End(3),
        ]
    );
    assert_eq!(view.headers, vec![0, 11, 16, 21]);
    assert_eq!(view.collapsed, vec![18]);
    // 12345 is five digits; the gutter is never under four.
    assert_eq!(view.number_columns, 5);
    // "\tnew": the tab reaches column 4, then three letters.
    assert_eq!(view.columns, 8);
    assert_eq!(view.file_of(18), Some(2));
    assert_eq!(view.file_of(15), None);
    assert_eq!(DiffView::new(SessionDiff::default()).rows, vec![]);
    assert_eq!(DiffView::new(SessionDiff::default()).number_columns, 4);
}

#[test]
fn expanding_a_file_replaces_its_rows_and_keeps_the_rest() {
    let view = DiffView::new(sample());
    let full = FileDiff {
        path: "big.txt".into(),
        insertions: 4812,
        hunks: vec![Hunk {
            header: "@@ -0,0 +1,2 @@".into(),
            lines: vec![
                line(LineKind::Added, None, Some(1), "one"),
                line(LineKind::Added, None, Some(2), &"w".repeat(30)),
            ],
        }],
        hidden_lines: 4810,
        ..Default::default()
    };
    let expanded = view.with_file(2, full);
    assert_eq!(
        expanded.rows[16..],
        [
            Row::File(2),
            Row::FileMeta(2),
            Row::Line {
                file: 2,
                hunk: 0,
                line: 0
            },
            Row::Line {
                file: 2,
                hunk: 0,
                line: 1
            },
            Row::Cut(2),
            Row::End(2),
            Row::Spacer,
            Row::File(3),
            Row::FileMeta(3),
            Row::End(3),
        ]
    );
    assert!(expanded.collapsed.is_empty());
    assert_eq!(expanded.headers, vec![0, 11, 16, 23]);
    assert_eq!(expanded.columns, 30);
    assert_eq!(expanded.stat, view.stat);
    assert!(Arc::ptr_eq(&expanded.files[0], &view.files[0]));
    // Out of range changes nothing.
    assert_eq!(view.with_file(9, FileDiff::default()).rows, view.rows);
}

#[test]
fn folding_uses_file_ranges_and_keeps_navigation_and_caps() {
    let view = DiffView::new(sample());
    let open = VisibleRows::new(&view, &HashSet::new());
    assert_eq!(open.len, view.rows.len());
    assert_eq!(open.headers, view.headers);
    assert_eq!(open.collapsed, view.collapsed);
    for row in 0..open.len {
        assert_eq!(open.raw(row), Some(row));
        assert_eq!(open.position(row), Some(row));
    }
    let folded = HashSet::from([
        ("src/a.rs".into(), FileStatus::Modified),
        ("big.txt".into(), FileStatus::Modified),
    ]);
    let closed = VisibleRows::new(&view, &folded);
    assert_eq!(closed.headers, [0, 4, 9, 13]);
    assert!(closed.collapsed.is_empty());
    assert_eq!(closed.len, 16);
    assert_eq!(closed.raw(2), Some(9)); // Rounded bottom, no code.
    assert_eq!(closed.position(3), None);
    assert_eq!(closed.raw(closed.len), None);
    for row in 0..closed.len {
        assert_eq!(closed.position(closed.raw(row).unwrap()), Some(row));
    }
    assert_eq!(VisibleRows::new(&view, &HashSet::new()).collapsed, [18]);
    assert_eq!(VisibleRows::new(&DiffView::default(), &folded).raw(0), None);
}

#[test]
fn folding_preserves_the_top_row_or_returns_to_its_header() {
    let view = DiffView::new(sample());
    let open = VisibleRows::new(&view, &HashSet::new());
    let closed = VisibleRows::new(
        &view,
        &HashSet::from([("src/a.rs".into(), FileStatus::Modified)]),
    );
    // A later file remains at the same offset inside its row.
    assert_eq!(
        folded_top(&open, &closed, &view, 11. * 21. + 3., 21., 42.),
        4. * 21. + 3.
    );
    // Code hidden by the fold falls back to the folded header.
    assert_eq!(folded_top(&open, &closed, &view, 3. * 21., 21., 42.), 0.);
    assert_eq!(
        folded_top(&closed, &open, &view, 4. * 21. + 3., 21., 42.),
        11. * 21. + 3.
    );
    assert_eq!(folded_top(&open, &closed, &view, 0., 21., 1000.), 0.);
}

#[test]
fn folding_survives_refresh_by_path_and_status_not_index() {
    let folded = HashSet::from([("src/a.rs".into(), FileStatus::Modified)]);
    let mut diff = sample();
    diff.files.swap(0, 1);
    let view = DiffView::new(diff);
    let visible = VisibleRows::new(&view, &folded);
    assert!(visible.folded.contains(&1));
    assert!(!visible.folded.contains(&0));
    let mut diff = sample();
    diff.files[0].status = FileStatus::Deleted;
    assert!(
        VisibleRows::new(&DiffView::new(diff), &folded)
            .folded
            .is_empty()
    );
}

#[test]
fn file_cards_keep_names_directories_and_rename_paths_safe() {
    assert_eq!(
        card_labels(&file("src/a.rs", vec![])),
        ("a.rs".into(), "src".into())
    );
    assert_eq!(
        card_labels(&file("README.md", vec![])),
        ("README.md".into(), "".into())
    );
    let renamed = FileDiff {
        path: "new/b.rs".into(),
        old_path: Some("old/a.rs".into()),
        status: FileStatus::Renamed,
        ..Default::default()
    };
    assert_eq!(
        card_labels(&renamed),
        ("b.rs".into(), "old/a.rs → new/b.rs".into())
    );
    assert_eq!(
        card_labels(&file("src/evil\u{202e}.rs", vec![])).0,
        "evil�.rs"
    );
}

#[test]
fn fifty_thousand_lines_keep_folding_and_paint_mapping_small() {
    let mut diff = SessionDiff::default();
    for f in 0..25 {
        diff.files.push(file(
            &format!("src/{f}.rs"),
            vec![Hunk {
                header: "@@ -0,0 +1,2000 @@".into(),
                lines: (1..=2000)
                    .map(|n| line(LineKind::Added, None, Some(n), "let width = bounds.width;"))
                    .collect(),
            }],
        ));
    }
    let started = Instant::now();
    let view = DiffView::new(diff);
    let indexed = started.elapsed();
    let folded = HashSet::from([("src/0.rs".into(), FileStatus::Modified)]);
    let started = Instant::now();
    let visible = VisibleRows::new(&view, &folded);
    let folded_time = started.elapsed();
    assert_eq!(visible.spans.len(), 50); // Two spans for each boundary, not each line.
    assert_eq!(visible.len, view.rows.len() - 2000);
    assert_eq!(view.files[0].hunks[0].lines.len(), 2000);
    let first = visible.headers[12];
    assert_eq!(
        (first..first + 40).filter_map(|i| visible.raw(i)).count(),
        40
    );
    eprintln!(
        "50,000-line index: {indexed:?}; fold ranges: {folded_time:?}; rows={}, spans={}",
        view.rows.len(),
        visible.spans.len()
    );
}

#[test]
fn line_text_expands_tabs_and_draws_controls_safely() {
    assert_eq!(visible_text("a\tb", 0, 80), "a   b");
    assert_eq!(visible_text("\tx", 0, 80), "    x");
    assert_eq!(visible_text("abcd\te", 0, 80), "abcd    e");
    assert_eq!(columns("abcd\te"), 9);
    assert_eq!(visible_text("crlf\r", 0, 80), "crlf");
    assert_eq!(columns("crlf\r"), 4);
    assert_eq!(visible_text("a\rb", 0, 80), "a\u{240d}b");
    assert_eq!(visible_text("\u{1b}[31m", 0, 80), "\u{241b}[31m");
    assert_eq!(visible_text("\u{7f}\u{85}", 0, 80), "\u{2421}\u{fffd}");
    assert_eq!(visible_text("a\u{202e}b", 0, 80), "a\u{fffd}b");
    assert_eq!(visible_text("", 0, 80), "");
    assert_eq!(visible_text("héllo wörld", 0, 80), "héllo wörld");
}

#[test]
fn line_text_is_cut_to_the_columns_on_screen() {
    assert_eq!(visible_text("abcdefgh", 2, 3), "cde");
    assert_eq!(visible_text("abcdefgh", 6, 10), "gh");
    assert_eq!(visible_text("abc", 5, 10), "");
    // A tab that straddles the start shows only its part on screen.
    assert_eq!(visible_text("a\tb", 2, 10), "  b");
    assert_eq!(visible_text("a\tb", 0, 2), "a ");
    let long = "x".repeat(100_000);
    assert_eq!(visible_text(&long, 99_990, 50).len(), 10);
    assert_eq!(visible_text("ab", usize::MAX - 1, usize::MAX), "");
}

#[test]
fn wide_characters_take_two_columns_and_are_never_split() {
    assert_eq!(columns("日本語"), 6);
    assert_eq!(columns("a👍b"), 4);
    assert_eq!(visible_text("日本語", 0, 80), "日本語");
    assert_eq!(visible_text("日本語", 2, 2), "本");
    // The left edge cuts 日: its right half is a blank cell.
    assert_eq!(visible_text("日本語", 1, 80), " 本語");
    assert_eq!(visible_text("a日b", 2, 80), " b");
    assert_eq!(visible_text("👍ok", 1, 3), " ok");
    // One that starts in the last column stays whole; the area clips it.
    assert_eq!(visible_text("日本語", 2, 1), "本");
    assert_eq!(visible_text("ab日", 0, 3), "ab日");
    // Columns after a wide character line up with the cells.
    assert_eq!(visible_text("日本x", 4, 1), "x");
    assert_eq!(visible_text("日\tx", 0, 80), "日  x");
    assert_eq!(columns("日\tx"), 5);
    // A line full of them costs only the columns up to the right edge.
    let long = "語".repeat(50_000);
    assert_eq!(visible_text(&long, 99_990, 50), "語".repeat(5));
    assert_eq!(visible_text(&long, 99_991, 4), " 語語");
}

#[test]
fn zero_width_characters_go_with_the_character_before_them() {
    let accent = "e\u{301}x";
    assert_eq!(columns(accent), 2);
    assert_eq!(visible_text(accent, 0, 1), "e\u{301}");
    // Cut off with its base, the mark is not painted on its own.
    assert_eq!(visible_text(accent, 1, 5), "x");
    // A cut wide character takes its marks with it.
    assert_eq!(visible_text("日\u{302}x", 1, 5), " x");
    assert_eq!(columns("a\u{200b}b"), 2);
    // A control character is drawn as one symbol whatever unicode-width
    // says of the character itself.
    assert_eq!(columns("\u{1b}\u{7f}\u{85}"), 3);
    // Per character, as a terminal counts: the man, the joiner, the
    // laptop.
    assert_eq!(columns("👨\u{200d}💻"), 4);
}

#[test]
fn collapsed_rows_name_the_cap() {
    let collapsed = |collapse, status, hidden_lines| FileDiff {
        collapsed: Some(collapse),
        status,
        hidden_lines,
        ..Default::default()
    };
    assert_eq!(
        collapsed_label(&collapsed(Collapse::Lines, FileStatus::Modified, 4812)),
        "Large diff hidden - 4,812 lines"
    );
    assert_eq!(
        collapsed_label(&collapsed(
            Collapse::Size(3_200_000),
            FileStatus::Untracked,
            10
        )),
        "Large file hidden - 3.2 MB"
    );
    assert_eq!(
        collapsed_label(&collapsed(
            Collapse::Size(6_300_000),
            FileStatus::Modified,
            2
        )),
        "Large diff hidden - 6.3 MB"
    );
    // Past the task's budget, a small file is not called large.
    assert_eq!(
        collapsed_label(&collapsed(Collapse::Budget, FileStatus::Added, 1)),
        "Diff hidden - 1 line"
    );
}

#[test]
fn sizes_read_in_decimal_units() {
    assert_eq!(size_label(1), "1 byte");
    assert_eq!(size_label(999), "999 bytes");
    assert_eq!(size_label(1_000), "1.0 KB");
    assert_eq!(size_label(1_048_577), "1.0 MB");
    assert_eq!(size_label(999_999), "1.0 MB");
    assert_eq!(size_label(1_126_400), "1.1 MB");
    assert_eq!(size_label(12_500_000_000), "12.5 GB");
}

#[test]
fn a_worktree_that_cannot_be_read_shows_the_failed_state() {
    let session = Target::Session("s".into());
    // The error `Core::session_diff` returns once a task worktree lost
    // its `.git` file, instead of the main checkout's "No changes".
    let error = shika_core::Error::ReadChanges(Some(
        "fatal: not a git repository (or any of the parent directories): .git".into(),
    ));
    let failed = Content::Failed(error_line(&error));
    assert!(matches!(
        panel_body(Some(&session), &failed, Instant::now()),
        Body::Failed("fatal: not a git repository (or any of the parent directories): .git")
    ));
    assert_eq!(COULD_NOT_READ, "Could not read changes");
    let empty = Content::Ready(Arc::new(DiffView::default()));
    assert!(matches!(
        panel_body(Some(&session), &empty, Instant::now()),
        Body::Message("No changes")
    ));
    let rows = Content::Ready(Arc::new(DiffView::new(sample())));
    assert!(matches!(
        panel_body(Some(&session), &rows, Instant::now()),
        Body::List(_)
    ));
    let since = Instant::now();
    let reading = Content::Reading { since };
    assert!(matches!(
        panel_body(Some(&session), &reading, since),
        Body::Blank
    ));
    assert!(matches!(
        panel_body(Some(&session), &reading, since + READING_AFTER),
        Body::Message("Reading changes")
    ));
    assert!(matches!(
        panel_body(None, &failed, since),
        Body::Message("No agent selected")
    ));
    assert!(matches!(
        panel_body(Some(&Target::SetupFailed), &failed, since),
        Body::Message("Setup failed")
    ));
}

#[test]
fn labels_are_safe_and_renames_dim_their_arrow() {
    assert_eq!(safe_label("a\tb\nc"), "a b\u{240a}c");
    let renamed = FileDiff {
        path: "new.rs".into(),
        old_path: Some("old.rs".into()),
        status: FileStatus::Renamed,
        ..Default::default()
    };
    let (text, arrow) = file_label(&renamed);
    assert_eq!(text, "old.rs \u{2192} new.rs");
    assert_eq!(&text[arrow.unwrap()], "\u{2192}");
    let (text, arrow) = file_label(&file("plain.rs", vec![]));
    assert_eq!((text.as_str(), arrow), ("plain.rs", None));
}

#[test]
fn counts_use_thousands_separators() {
    assert_eq!(thousands(0), "0");
    assert_eq!(thousands(999), "999");
    assert_eq!(thousands(4812), "4,812");
    assert_eq!(thousands(1_234_567), "1,234,567");
    assert_eq!(lines_label(1), "1 line");
    assert_eq!(lines_label(4812), "4,812 lines");
}

#[test]
fn the_panel_gives_way_before_the_column() {
    let both = |viewport: f32| pane_widths(viewport, 420., Some(540.), Some(480.));
    assert_eq!(both(1600.), (Some(540.), Some(480.)));
    // Narrower: the panel shrinks first...
    assert_eq!(both(1300.), (Some(540.), Some(340.)));
    assert_eq!(both(1280.), (Some(540.), Some(320.)));
    // ...then the column, beside the panel as shown.
    assert_eq!(both(1100.), (Some(360.), Some(320.)));
    assert_eq!(both(1060.), (Some(320.), Some(320.)));
    // Only then does the terminal go under 420.
    assert_eq!(both(960.), (Some(320.), Some(320.)));
    // A hidden column gives its room to the panel.
    assert_eq!(
        pane_widths(1000., 420., None, Some(900.)),
        (None, Some(580.))
    );
    // A closed panel leaves the column as before.
    assert_eq!(
        pane_widths(1000., 420., Some(800.), None),
        (Some(580.), None)
    );
    assert_eq!(
        pane_widths(700., 420., Some(540.), None),
        (Some(320.), None)
    );
    assert_eq!(pane_widths(1000., 420., None, None), (None, None));
}

#[test]
fn a_drag_stops_where_the_terminal_would_go_under_its_minimum() {
    assert_eq!(drag_width(1400., 900., 420., 540.), 440.);
    assert_eq!(drag_width(1400., 100., 420., 540.), 440.);
    assert_eq!(drag_width(1400., 1300., 420., 0.), 100.);
    let saved = Changes::default();
    assert_eq!(
        saved.with_width(drag_width(1400., 1300., 420., 0.)).width,
        320
    );
    assert_eq!(
        saved.with_width(drag_width(2400., 100., 420., 0.)).width,
        900
    );
}

#[test]
fn keys_scroll_by_rows_pages_and_files() {
    let headers = [0, 10, 40];
    // 100 rows of 20 in a 200 high list: 10 rows a page, 1800 at most.
    let at = |scroll, top| scroll_top(scroll, top, 20., 100, 200., &headers);
    assert_eq!(at(Scroll::Rows(1), 0.), 20.);
    assert_eq!(at(Scroll::Rows(-1), 0.), 0.);
    assert_eq!(at(Scroll::Rows(1), 1800.), 1800.);
    assert_eq!(at(Scroll::HalfPage(1), 0.), 100.);
    assert_eq!(at(Scroll::HalfPage(-1), 300.), 200.);
    assert_eq!(at(Scroll::Page(1), 0.), 200.);
    assert_eq!(at(Scroll::Page(-1), 100.), 0.);
    assert_eq!(at(Scroll::Top, 500.), 0.);
    assert_eq!(at(Scroll::Bottom, 0.), 1800.);
    assert_eq!(at(Scroll::NextFile, 0.), 200.);
    assert_eq!(at(Scroll::NextFile, 200.), 800.);
    assert_eq!(at(Scroll::NextFile, 800.), 800.);
    assert_eq!(at(Scroll::NextFile, 195.), 800.);
    assert_eq!(at(Scroll::PreviousFile, 800.), 200.);
    assert_eq!(at(Scroll::PreviousFile, 300.), 200.);
    assert_eq!(at(Scroll::PreviousFile, 200.), 0.);
    assert_eq!(at(Scroll::PreviousFile, 0.), 0.);
    // A list shorter than the view does not scroll.
    assert_eq!(scroll_top(Scroll::Bottom, 0., 20., 5, 200., &[0]), 0.);
    assert_eq!(scroll_top(Scroll::Rows(3), 0., 20., 5, 200., &[0]), 0.);
}

#[test]
fn expand_picks_the_first_collapsed_row_on_screen() {
    let rows = [3, 30, 60];
    assert_eq!(first_in_view(&rows, 0., 20., 200.), Some(3));
    assert_eq!(first_in_view(&rows, 100., 20., 200.), None);
    assert_eq!(first_in_view(&rows, 500., 20., 200.), Some(30));
    assert_eq!(first_in_view(&rows, 1180., 20., 200.), Some(60));
    assert_eq!(first_in_view(&[], 0., 20., 200.), None);
}

#[test]
fn git_errors_show_their_first_line() {
    assert_eq!(
        error_line(&shika_core::Error::ReadChanges(Some("fatal: bad".into()))),
        "fatal: bad"
    );
    assert_eq!(error_line(&shika_core::Error::ReadChanges(None)), "");
    assert_eq!(
        error_line(&shika_core::Error::UnknownSession),
        shika_core::Error::UnknownSession.to_string()
    );
}
