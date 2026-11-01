use super::*;
use std::time::Duration;

fn engine(rows: u16, cols: u16) -> Engine {
    Engine::new(TerminalSize::new(rows, cols), 100, Palette::shika_dark())
}

fn feed(engine: &mut Engine, text: &str) {
    engine.advance(text.as_bytes());
}

#[test]
fn plain_text_lands_in_the_grid_with_the_cursor_after_it() {
    let mut e = engine(4, 10);
    feed(&mut e, "hello\r\nworld");
    let snap = e.snapshot();
    assert_eq!(snap.text_lines(), vec!["hello", "world", "", ""]);
    let cursor = snap.cursor.unwrap();
    assert_eq!((cursor.row, cursor.col), (1, 5));
    assert_eq!(cursor.shape, CursorShape::Block);
    assert_eq!(snap.lines[0].cells[0].fg, Palette::shika_dark().foreground);
    assert_eq!(snap.lines[0].cells[0].bg, Palette::shika_dark().background);
}

#[test]
fn sgr_colors_resolve_to_rgb() {
    let mut e = engine(2, 20);
    // red, bright green, 256-color 196, truecolor, then a background.
    feed(
        &mut e,
        "\x1b[31mA\x1b[92mB\x1b[38;5;196mC\x1b[38;2;1;2;3mD\x1b[0;44mE\x1b[0m",
    );
    let cells = &e.snapshot().lines[0].cells;
    let palette = Palette::shika_dark();
    assert_eq!(cells[0].fg, palette.ansi[1]);
    assert_eq!(cells[1].fg, palette.ansi[10]);
    assert_eq!(cells[2].fg, Rgb::new(255, 0, 0));
    assert_eq!(cells[3].fg, Rgb::new(1, 2, 3));
    assert_eq!(cells[4].bg, palette.ansi[4]);
    assert_eq!(cells[4].fg, palette.foreground);
    assert_eq!(cells[5].bg, palette.background);
}

#[test]
fn bold_italic_underline_strike_and_inverse() {
    let mut e = engine(2, 20);
    feed(
        &mut e,
        "\x1b[1mB\x1b[0;3mI\x1b[0;4mU\x1b[0;4:3mC\x1b[0;9mS\x1b[0;7mR\x1b[0;1;31mX\x1b[0m",
    );
    let cells = &e.snapshot().lines[0].cells;
    let palette = Palette::shika_dark();
    assert!(cells[0].flags.contains(CellFlags::BOLD));
    assert!(cells[1].flags.contains(CellFlags::ITALIC));
    assert!(cells[2].flags.contains(CellFlags::UNDERLINE));
    assert!(cells[3].flags.contains(CellFlags::CURLY_UNDERLINE));
    assert!(cells[4].flags.contains(CellFlags::STRIKETHROUGH));
    assert_eq!(cells[5].fg, palette.background);
    assert_eq!(cells[5].bg, palette.foreground);
    // Bold red draws as bright red.
    assert_eq!(cells[6].fg, palette.ansi[9]);
}

#[test]
fn long_lines_wrap_and_mark_the_soft_break() {
    let mut e = engine(3, 5);
    feed(&mut e, "abcdefgh");
    let snap = e.snapshot();
    assert_eq!(snap.text_lines(), vec!["abcde", "fgh", ""]);
    assert!(snap.lines[0].cells[4].flags.contains(CellFlags::WRAPPED));
    assert!(!snap.lines[1].cells[4].flags.contains(CellFlags::WRAPPED));
}

#[test]
fn wide_characters_take_two_cells() {
    let mut e = engine(2, 10);
    feed(&mut e, "a\u{4F60}b\u{1F600}");
    let line = &e.snapshot().lines[0];
    assert_eq!(line.cells[0].ch, 'a');
    assert_eq!(line.cells[1].ch, '\u{4F60}');
    assert!(line.cells[1].flags.contains(CellFlags::WIDE));
    assert!(line.cells[2].flags.contains(CellFlags::WIDE_SPACER));
    assert_eq!(line.cells[3].ch, 'b');
    assert_eq!(line.cells[4].ch, '\u{1F600}');
    assert!(line.cells[4].flags.contains(CellFlags::WIDE));
    assert!(line.cells[5].flags.contains(CellFlags::WIDE_SPACER));
    assert_eq!(line.text(), "a\u{4F60}b\u{1F600}");
    let cursor = e.snapshot().cursor.unwrap();
    assert_eq!(cursor.col, 6);
}

#[test]
fn combining_marks_stay_with_their_base_character() {
    let mut e = engine(2, 10);
    // Vietnamese "e" plus combining circumflex and acute.
    feed(&mut e, "e\u{0302}\u{0301}x");
    let line = &e.snapshot().lines[0];
    assert_eq!(line.cells[0].ch, 'e');
    assert_eq!(line.combining_at(0), Some("\u{0302}\u{0301}"));
    assert_eq!(line.cells[1].ch, 'x');
}

#[test]
fn alt_screen_swaps_and_restores_the_primary_screen() {
    let mut e = engine(3, 10);
    feed(&mut e, "shell$ ");
    feed(&mut e, "\x1b[?1049h\x1b[Htui");
    let snap = e.snapshot();
    assert!(snap.modes.alt_screen);
    assert_eq!(snap.text_lines()[0], "tui");
    feed(&mut e, "\x1b[?1049l");
    let snap = e.snapshot();
    assert!(!snap.modes.alt_screen);
    assert_eq!(snap.text_lines()[0], "shell$");
}

#[test]
fn a_pager_round_trip_restores_the_shell_screen_and_modes() {
    // What `less` sends with TERM=xterm-256color: smcup, keypad
    // transmit (app cursor plus app keypad), a page, a reverse-video
    // search hit, then keypad local and rmcup on `q`.
    let mut e = engine(4, 20);
    feed(&mut e, "$ seq 1 300 | less\r\n");
    feed(&mut e, "\x1b[?1049h\x1b[22;0;0t\x1b[?1h\x1b=\r");
    let modes = e.modes();
    assert!(modes.alt_screen && modes.app_cursor && modes.app_keypad);
    assert!(modes.alternate_scroll);
    feed(&mut e, "\x1b[H\x1b[2J1\r\n\x1b[7m2\x1b[27m\r\n3\r\n:");
    let snap = e.snapshot();
    assert_eq!(snap.text_lines(), vec!["1", "2", "3", ":"]);
    // The search hit is drawn inverted.
    assert_eq!(snap.lines[1].cells[0].bg, Palette::shika_dark().foreground);
    // The alternate screen keeps no history of its own.
    assert_eq!(snap.history, 0);
    feed(&mut e, "\r\x1b[K\x1b[?1l\x1b>\x1b[?1049l\x1b[23;0;0t");
    let snap = e.snapshot();
    let modes = snap.modes;
    assert!(!modes.alt_screen && !modes.app_cursor && !modes.app_keypad);
    assert_eq!(snap.text_lines()[0], "$ seq 1 300 | less");
    assert_eq!((snap.cursor.unwrap().row, snap.cursor.unwrap().col), (1, 0));
}

#[test]
fn resizing_the_alternate_screen_keeps_the_primary_intact() {
    let mut e = engine(4, 20);
    feed(&mut e, "prompt$ ");
    feed(&mut e, "\x1b[?1049h\x1b[Hpager");
    e.resize(TerminalSize::new(6, 30));
    assert_eq!(e.snapshot().rows, 6);
    feed(&mut e, "\x1b[?1049l");
    let snap = e.snapshot();
    assert_eq!(snap.cols, 30);
    assert_eq!(snap.text_lines()[0], "prompt$");
}

#[test]
fn scrollback_keeps_old_rows_and_scrolls_back_to_them() {
    let mut e = engine(3, 10);
    for i in 0..10 {
        feed(&mut e, &format!("line{i}\r\n"));
    }
    let snap = e.snapshot();
    assert_eq!(snap.history, 8);
    assert_eq!(snap.text_lines(), vec!["line8", "line9", ""]);
    e.scroll(2);
    let snap = e.snapshot();
    assert_eq!(snap.display_offset, 2);
    assert_eq!(snap.text_lines(), vec!["line6", "line7", "line8"]);
    // The cursor row is out of view now.
    assert!(snap.cursor.is_none());
    e.scroll_to_bottom();
    assert_eq!(e.snapshot().display_offset, 0);
}

#[test]
fn live_text_ignores_scrollback_without_changing_view_or_selection() {
    let mut e = engine(3, 10);
    feed(&mut e, "old0\r\nold1\r\nold2\r\nlive0\r\nlive1");
    let live = e.snapshot().text_lines();
    assert_eq!(live, vec!["old2", "live0", "live1"]);
    e.scroll(2);
    e.start_selection(
        SelectionKind::Simple,
        ViewportPoint { row: 0, col: 0 },
        CellSide::Left,
    );
    e.update_selection(ViewportPoint { row: 0, col: 3 }, CellSide::Right);
    let before = e.snapshot();
    let selected = e.selection_text();
    assert_eq!(before.text_lines(), vec!["old0", "old1", "old2"]);
    assert_eq!(selected.as_deref(), Some("old0"));
    e.take_output();
    for _ in 0..3 {
        assert_eq!(e.live_text_lines(), live);
    }
    assert_eq!(e.display_offset(), 2);
    assert_eq!(e.snapshot(), before);
    assert_eq!(e.selection_text(), selected);
    assert!(e.events.lock().unwrap().is_empty());
}

#[test]
fn text_with_history_prepends_scrollback_without_moving_the_view() {
    let mut e = engine(3, 10);
    for i in 0..10 {
        feed(&mut e, &format!("line{i}\r\n"));
    }
    e.scroll(2);
    let before = e.snapshot();
    assert_eq!(e.text_with_history(0), e.live_text_lines());
    assert_eq!(
        e.text_with_history(2),
        vec!["line6", "line7", "line8", "line9", ""]
    );
    // More than exists returns all of it.
    assert_eq!(e.text_with_history(500).len(), 8 + 3);
    assert_eq!(e.snapshot(), before);
}

#[test]
fn live_text_matches_snapshot_for_wide_combining_hidden_and_blank_cells() {
    let mut e = engine(4, 5);
    // A wide glyph at the right edge leaves a leading spacer before
    // wrapping. Hidden text is retained, just as in Snapshot::text_lines.
    feed(
        &mut e,
        "abce\u{0302}\u{0301}\u{4f60}\u{0301}\x1b[8mx\x1b[0m\r\nz",
    );
    let expected = vec!["abce\u{0302}\u{0301}", "\u{4f60}\u{0301}x", "z", ""];
    assert_eq!(e.snapshot().text_lines(), expected);
    assert_eq!(e.live_text_lines(), expected);
    // NUL-valued cells are normalized rather than returned verbatim.
    e.term.grid_mut()[Point::new(GridLine(2), Column(0))].c = '\0';
    assert_eq!(e.live_text_lines(), e.snapshot().text_lines());
    assert_eq!(e.live_text_lines()[2], "");
}

#[test]
fn live_text_reads_the_active_alternate_screen_and_restored_primary() {
    let mut e = engine(3, 10);
    feed(&mut e, "old0\r\nold1\r\nold2\r\nshell$");
    let primary = e.live_text_lines();
    e.scroll(1);
    let before = e.snapshot();
    feed(&mut e, "\x1b[?1049h\x1b[Htui\x1b[?25l");
    assert_eq!(e.live_text_lines(), vec!["tui", "", ""]);
    assert!(e.snapshot().cursor.is_none());
    e.scroll(100);
    assert_eq!(e.live_text_lines(), e.snapshot().text_lines());
    feed(&mut e, "\x1b[?1049l");
    assert_eq!(e.live_text_lines(), primary);
    assert_eq!(e.display_offset(), before.display_offset);
    assert_eq!(e.snapshot().text_lines(), before.text_lines());
}

#[test]
fn mode_flags_follow_decset() {
    let mut e = engine(3, 10);
    let modes = e.modes();
    assert!(!modes.app_cursor && !modes.bracketed_paste && !modes.focus_reporting);
    assert!(modes.show_cursor);
    feed(
        &mut e,
        "\x1b[?1h\x1b[?2004h\x1b[?1004h\x1b[?1002h\x1b[?1006h\x1b[?25l",
    );
    let modes = e.modes();
    assert!(modes.app_cursor);
    assert!(modes.bracketed_paste);
    assert!(modes.focus_reporting);
    assert_eq!(modes.mouse, MouseTracking::Drag);
    assert_eq!(modes.mouse_encoding, MouseEncoding::Sgr);
    assert!(!modes.show_cursor);
    assert!(e.snapshot().cursor.is_none());
    feed(&mut e, "\x1b[?1l\x1b[?2004l\x1b[?1002l");
    let modes = e.modes();
    assert!(!modes.app_cursor && !modes.bracketed_paste);
    assert_eq!(modes.mouse, MouseTracking::Off);
}

#[test]
fn cursor_shape_follows_decscusr() {
    let mut e = engine(2, 10);
    feed(&mut e, "\x1b[6 q");
    assert_eq!(e.snapshot().cursor.unwrap().shape, CursorShape::Beam);
    feed(&mut e, "\x1b[4 q");
    assert_eq!(e.snapshot().cursor.unwrap().shape, CursorShape::Underline);
}

#[test]
fn queries_produce_replies_for_the_pty() {
    let mut e = engine(5, 20);
    feed(&mut e, "ab\x1b[6n");
    let out = e.take_output();
    assert_eq!(out.replies, vec![b"\x1b[1;3R".to_vec()]);
    // OSC 11 background query answers with the palette background.
    feed(&mut e, "\x1b]11;?\x07");
    let out = e.take_output();
    let reply = String::from_utf8(out.replies[0].clone()).unwrap();
    assert!(reply.starts_with("\x1b]11;rgb:1a1a/1c1c/1919"), "{reply:?}");
}

#[test]
fn title_and_bell_are_reported() {
    let mut e = engine(2, 10);
    feed(&mut e, "\x1b]2;my task\x07\x07");
    let out = e.take_output();
    assert_eq!(out.title, Some(Some("my task".to_string())));
    assert!(out.bell);
    assert!(e.take_output() == Output::default());
}

#[test]
fn synchronized_output_is_held_until_the_end_marker() {
    let mut e = engine(2, 10);
    assert!(e.advance(b"\x1b[?2026hfirst"));
    assert!(e.sync_deadline().is_some());
    // Still inside the update: nothing new to draw.
    assert!(!e.advance(b" more"));
    assert_eq!(e.snapshot().text_lines()[0], "");
    assert!(e.advance(b"\x1b[?2026l"));
    assert!(e.sync_deadline().is_none());
    assert_eq!(e.snapshot().text_lines()[0], "first more");
}

#[test]
fn an_unfinished_synchronized_update_is_drawn_after_the_deadline() {
    let mut e = engine(2, 10);
    e.advance(b"\x1b[?2026hstuck");
    let deadline = e.sync_deadline().unwrap();
    assert!(!e.flush_sync_if_expired(deadline - Duration::from_millis(1)));
    assert!(e.flush_sync_if_expired(deadline));
    assert_eq!(e.snapshot().text_lines()[0], "stuck");
}

#[test]
fn selection_marks_cells_and_copies_text() {
    let mut e = engine(3, 20);
    feed(&mut e, "hello world\r\nsecond line");
    e.start_selection(
        SelectionKind::Simple,
        ViewportPoint { row: 0, col: 6 },
        CellSide::Left,
    );
    e.update_selection(ViewportPoint { row: 1, col: 5 }, CellSide::Right);
    assert_eq!(e.selection_text().as_deref(), Some("world\nsecond"));
    let snap = e.snapshot();
    assert!(!snap.lines[0].cells[5].flags.contains(CellFlags::SELECTED));
    assert!(snap.lines[0].cells[6].flags.contains(CellFlags::SELECTED));
    assert!(snap.lines[1].cells[5].flags.contains(CellFlags::SELECTED));
    assert!(!snap.lines[1].cells[6].flags.contains(CellFlags::SELECTED));

    e.start_selection(
        SelectionKind::Word,
        ViewportPoint { row: 0, col: 2 },
        CellSide::Left,
    );
    assert_eq!(e.selection_text().as_deref(), Some("hello"));

    e.start_selection(
        SelectionKind::Line,
        ViewportPoint { row: 1, col: 3 },
        CellSide::Left,
    );
    // A line selection ends with its line break, like alacritty and iTerm.
    assert_eq!(e.selection_text().as_deref(), Some("second line\n"));

    e.clear_selection();
    assert!(e.selection_text().is_none());
}

#[test]
fn a_click_without_a_drag_selects_nothing() {
    let mut e = engine(2, 20);
    feed(&mut e, "hello");
    e.start_selection(
        SelectionKind::Simple,
        ViewportPoint { row: 0, col: 2 },
        CellSide::Left,
    );
    assert!(!e.has_selection());
    assert!(e.selection_text().is_none());
}

#[test]
fn osc8_label_maps_to_its_uri() {
    let mut e = engine(2, 40);
    feed(
        &mut e,
        "\x1b]8;;https://example.com/pr/1\x1b\\PR #1\x1b]8;;\x1b\\",
    );
    let snap = e.snapshot();
    assert_eq!(snap.lines[0].text(), "PR #1");
    assert_eq!(snap.lines[0].link_at(0), Some("https://example.com/pr/1"));
    assert_eq!(snap.lines[0].link_at(4), Some("https://example.com/pr/1"));
    assert_eq!(snap.lines[0].link_at(5), None);
}

#[test]
fn plain_url_drops_trailing_punctuation() {
    let mut e = engine(2, 40);
    feed(&mut e, "see https://example.com/a. next");
    let line = &e.snapshot().lines[0];
    let start = line.text().find("https://").unwrap();
    assert_eq!(line.link_at(start), Some("https://example.com/a"));
    assert_eq!(line.link_at(start + "https://example.com/a".len()), None);
}

#[test]
fn a_wrapped_url_joins_into_one_link() {
    let mut e = engine(4, 12);
    feed(&mut e, "https://example.com/pull/1");
    let snap = e.snapshot();
    assert!(
        snap.lines[0]
            .cells
            .last()
            .unwrap()
            .flags
            .contains(CellFlags::WRAPPED)
    );
    assert_eq!(snap.lines[0].link_at(0), Some("https://example.com/pull/1"));
    assert_eq!(snap.lines[1].link_at(0), Some("https://example.com/pull/1"));
}

#[test]
fn osc8_wins_over_the_text_underneath() {
    let mut e = engine(2, 40);
    feed(
        &mut e,
        "\x1b]8;;file:///tmp/src.rs\x1b\\https://example.com\x1b]8;;\x1b\\",
    );
    let line = &e.snapshot().lines[0];
    assert_eq!(line.link_at(0), Some("file:///tmp/src.rs"));
    assert!(
        line.links
            .iter()
            .all(|link| link.uri == "file:///tmp/src.rs")
    );
}

#[test]
fn a_javascript_hyperlink_is_not_a_link() {
    let mut e = engine(2, 20);
    feed(
        &mut e,
        "\x1b]8;;javascript:alert(1)\x1b\\click\x1b]8;;\x1b\\",
    );
    assert_eq!(e.snapshot().lines[0].link_at(0), None);
}

#[test]
fn resize_reflows_and_reports_the_new_size() {
    let mut e = engine(3, 10);
    feed(&mut e, "0123456789abc");
    e.resize(TerminalSize::new(3, 20));
    let snap = e.snapshot();
    assert_eq!(snap.cols, 20);
    assert_eq!(snap.text_lines()[0], "0123456789abc");
    e.resize(TerminalSize::new(5, 20).with_cell(8, 19));
    feed(&mut e, "\x1b[18t");
    let out = e.take_output();
    assert_eq!(out.replies, vec![b"\x1b[8;5;20t".to_vec()]);
}
