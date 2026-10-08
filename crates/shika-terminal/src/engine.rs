//! The only module that names `alacritty_terminal`. It turns PTY bytes into
//! Shika's own snapshot types and back. Swapping the engine (for example to
//! libghostty-vt) means rewriting this file and nothing else.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Direction, Line as GridLine, Point};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::vte::ansi::{
    Color, CursorShape as EngineCursorShape, NamedColor, Processor, Rgb as EngineRgb,
    StdSyncHandler,
};

use crate::theme::Palette;
use crate::types::{
    Cell, CellFlags, CellSide, Cursor, CursorShape, Line, LinkSpan, Modes, MouseEncoding,
    MouseTracking, Rgb, SelectionKind, Snapshot, TerminalSize, ViewportPoint, fill_plain_links,
    openable_uri,
};

/// What a burst of PTY output asked the outside world to do.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Output {
    /// Answers to queries (cursor position, device attributes, colors).
    /// They go back to the PTY as if typed.
    pub replies: Vec<Vec<u8>>,
    /// `Some(None)` means the program reset its title.
    pub title: Option<Option<String>>,
    pub bell: bool,
    /// OSC 52 copy.
    pub clipboard: Option<String>,
}

impl Output {
    pub fn has_notice(&self) -> bool {
        self.title.is_some() || self.bell || self.clipboard.is_some()
    }
}

#[derive(Clone)]
struct Listener(Arc<Mutex<Vec<Event>>>);

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        self.0
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .push(event);
    }
}

struct Dims {
    rows: usize,
    cols: usize,
}

impl Dimensions for Dims {
    fn total_lines(&self) -> usize {
        self.rows
    }

    fn screen_lines(&self) -> usize {
        self.rows
    }

    fn columns(&self) -> usize {
        self.cols
    }
}

pub(crate) struct Engine {
    term: Term<Listener>,
    parser: Processor<StdSyncHandler>,
    events: Arc<Mutex<Vec<Event>>>,
    palette: Palette,
    size: TerminalSize,
}

impl Engine {
    pub fn new(size: TerminalSize, scrollback: usize, palette: Palette) -> Self {
        let events = Arc::new(Mutex::new(Vec::new()));
        let config = Config {
            scrolling_history: scrollback,
            // The input encoder speaks the legacy xterm protocol only.
            // Leaving this off makes the engine refuse kitty keyboard
            // requests, so programs fall back to what we send.
            kitty_keyboard: false,
            ..Config::default()
        };
        let term = Term::new(
            config,
            &Dims {
                rows: size.rows as usize,
                cols: size.cols as usize,
            },
            Listener(events.clone()),
        );
        Self {
            term,
            parser: Processor::new(),
            events,
            palette,
            size,
        }
    }

    /// Parse PTY output. Returns false when every byte went into a pending
    /// synchronized update (DEC 2026), so there is nothing new to draw yet.
    pub fn advance(&mut self, bytes: &[u8]) -> bool {
        if bytes.is_empty() {
            return false;
        }
        self.flush_sync_if_expired(Instant::now());
        let syncing_before = self.sync_deadline().is_some();
        let buffered_before = self.parser.sync_bytes_count();
        self.parser.advance(&mut self.term, bytes);
        let all_buffered = syncing_before
            && self.sync_deadline().is_some()
            && self.parser.sync_bytes_count() == buffered_before + bytes.len();
        !all_buffered
    }

    /// When a synchronized update started and has not ended.
    pub fn sync_deadline(&self) -> Option<Instant> {
        self.parser.sync_timeout().sync_timeout()
    }

    /// A program that began a synchronized update and never ended it gets
    /// its buffered output drawn anyway once the deadline passes. Returns
    /// true when that happened.
    pub fn flush_sync_if_expired(&mut self, now: Instant) -> bool {
        match self.sync_deadline() {
            Some(deadline) if deadline <= now => {
                self.parser.stop_sync(&mut self.term);
                true
            }
            _ => false,
        }
    }

    pub fn take_output(&mut self) -> Output {
        let events =
            std::mem::take(&mut *self.events.lock().unwrap_or_else(|err| err.into_inner()));
        let mut out = Output::default();
        for event in events {
            match event {
                Event::PtyWrite(text) => out.replies.push(text.into_bytes()),
                Event::ColorRequest(index, format) => {
                    let rgb = self.color_for_index(index);
                    out.replies.push(format(engine_rgb(rgb)).into_bytes());
                }
                Event::TextAreaSizeRequest(format) => {
                    let size = WindowSize {
                        num_lines: self.size.rows,
                        num_cols: self.size.cols,
                        cell_width: self.size.cell_width,
                        cell_height: self.size.cell_height,
                    };
                    out.replies.push(format(size).into_bytes());
                }
                Event::Title(title) => out.title = Some(Some(title)),
                Event::ResetTitle => out.title = Some(None),
                Event::Bell => out.bell = true,
                Event::ClipboardStore(_, text) => out.clipboard = Some(text),
                // Paste through OSC 52 is off (the engine's default policy is
                // copy only), and the rest are alacritty window concerns.
                Event::ClipboardLoad(..)
                | Event::MouseCursorDirty
                | Event::CursorBlinkingChange
                | Event::Wakeup
                | Event::Exit
                | Event::ChildExit(_) => {}
            }
        }
        out
    }

    pub fn size(&self) -> TerminalSize {
        self.size
    }

    pub fn resize(&mut self, size: TerminalSize) {
        let grid_changed = !self.size.same_grid(&size);
        self.size = size;
        if grid_changed {
            self.term.resize(Dims {
                rows: size.rows as usize,
                cols: size.cols as usize,
            });
        }
    }

    pub fn set_palette(&mut self, palette: Palette) {
        self.palette = palette;
    }

    pub fn modes(&self) -> Modes {
        modes_from(*self.term.mode())
    }

    pub fn display_offset(&self) -> usize {
        self.term.grid().display_offset()
    }

    /// Positive scrolls back into history.
    pub fn scroll(&mut self, lines: i32) {
        if lines != 0 {
            self.term.scroll_display(Scroll::Delta(lines));
        }
    }

    pub fn scroll_page(&mut self, up: bool) {
        self.term
            .scroll_display(if up { Scroll::PageUp } else { Scroll::PageDown });
    }

    pub fn scroll_to_bottom(&mut self) {
        if self.display_offset() != 0 {
            self.term.scroll_display(Scroll::Bottom);
        }
    }

    pub fn start_selection(&mut self, kind: SelectionKind, at: ViewportPoint, side: CellSide) {
        let ty = match kind {
            SelectionKind::Simple => SelectionType::Simple,
            SelectionKind::Word => SelectionType::Semantic,
            SelectionKind::Line => SelectionType::Lines,
        };
        let point = self.grid_point(at);
        self.term.selection = Some(Selection::new(ty, point, direction(side)));
    }

    pub fn update_selection(&mut self, at: ViewportPoint, side: CellSide) {
        let point = self.grid_point(at);
        if let Some(selection) = self.term.selection.as_mut() {
            selection.update(point, direction(side));
        }
    }

    pub fn clear_selection(&mut self) {
        self.term.selection = None;
    }

    /// True when the selection covers at least one cell.
    pub fn has_selection(&self) -> bool {
        self.term
            .selection
            .as_ref()
            .is_some_and(|selection| !selection.is_empty())
    }

    pub fn selection_text(&self) -> Option<String> {
        if !self.has_selection() {
            return None;
        }
        self.term
            .selection_to_string()
            .filter(|text| !text.is_empty())
    }

    /// Copy only the active live screen's text, independent of scrollback.
    /// Grid lines 0..screen_lines are live rows; display_offset affects only
    /// the viewport. Reading them directly leaves all terminal state alone.
    pub fn live_text_lines(&self) -> Vec<String> {
        let grid = self.term.grid();
        let rows = grid.screen_lines();
        let cols = grid.columns();
        let mut lines = Vec::with_capacity(rows);
        for row in 0..rows {
            let source = &grid[GridLine(row as i32)];
            let mut text = String::with_capacity(cols);
            for col in 0..cols {
                let cell = &source[Column(col)];
                // Match Line::text: omit both kinds of wide spacer, keep
                // hidden text, normalize NUL, and retain combining marks.
                if cell
                    .flags
                    .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
                {
                    continue;
                }
                text.push(if cell.c == '\0' { ' ' } else { cell.c });
                if let Some(marks) = cell.zerowidth() {
                    text.extend(marks.iter().copied());
                }
            }
            text.truncate(text.trim_end_matches(' ').len());
            lines.push(text);
        }
        lines
    }

    pub fn snapshot(&self) -> Snapshot {
        let grid = self.term.grid();
        let rows = grid.screen_lines();
        let cols = grid.columns();
        let offset = grid.display_offset();
        let mode = *self.term.mode();
        let selection = self
            .term
            .selection
            .as_ref()
            .and_then(|selection| selection.to_range(&self.term));

        let foreground = self.named(NamedColor::Foreground);
        let background = self.named(NamedColor::Background);
        let cursor_color = self.named(NamedColor::Cursor);

        let mut lines = Vec::with_capacity(rows);
        for row in 0..rows {
            let line = GridLine(row as i32 - offset as i32);
            let source = &grid[line];
            let mut out = Line {
                cells: Vec::with_capacity(cols),
                combining: Vec::new(),
                links: hyperlink_spans(source, cols),
            };
            for col in 0..cols {
                let cell = &source[Column(col)];
                let selected = selection
                    .as_ref()
                    .is_some_and(|range| range.contains(Point::new(line, Column(col))));
                out.cells.push(self.cell(cell, selected));
                if let Some(marks) = cell.zerowidth()
                    && !marks.is_empty()
                {
                    out.combining.push((col as u16, marks.iter().collect()));
                }
            }
            if cols > 0
                && source[Column(cols - 1)].flags.contains(Flags::WRAPLINE)
                && let Some(cell) = out.cells.last_mut()
            {
                cell.flags |= CellFlags::WRAPPED;
            }
            lines.push(out);
        }
        fill_plain_links(&mut lines);

        let cursor = self.cursor(rows, offset);
        Snapshot {
            rows,
            cols,
            lines,
            cursor,
            modes: modes_from(mode),
            display_offset: offset,
            history: grid.history_size(),
            foreground,
            background,
            cursor_color,
        }
    }

    fn cursor(&self, rows: usize, offset: usize) -> Option<Cursor> {
        let content = self.term.renderable_content();
        let shape = match content.cursor.shape {
            EngineCursorShape::Hidden => return None,
            EngineCursorShape::Block | EngineCursorShape::HollowBlock => CursorShape::Block,
            EngineCursorShape::Underline => CursorShape::Underline,
            EngineCursorShape::Beam => CursorShape::Beam,
        };
        let point = content.cursor.point;
        let row = point.line.0 + offset as i32;
        if row < 0 || row as usize >= rows {
            return None;
        }
        let wide = self.term.grid()[point].flags.contains(Flags::WIDE_CHAR);
        Some(Cursor {
            row: row as usize,
            col: point.column.0,
            shape,
            wide,
        })
    }

    fn cell(&self, cell: &alacritty_terminal::term::cell::Cell, selected: bool) -> Cell {
        let flags = cell.flags;
        let bold = flags.contains(Flags::BOLD);
        let mut fg = self.resolve(cell.fg, bold);
        let mut bg = self.resolve(cell.bg, false);
        if flags.contains(Flags::DIM) {
            fg = fg.dimmed();
        }
        if flags.contains(Flags::INVERSE) {
            std::mem::swap(&mut fg, &mut bg);
        }
        let mut out = CellFlags::empty();
        out.set(CellFlags::BOLD, bold);
        out.set(CellFlags::ITALIC, flags.contains(Flags::ITALIC));
        out.set(CellFlags::UNDERLINE, flags.contains(Flags::UNDERLINE));
        out.set(
            CellFlags::DOUBLE_UNDERLINE,
            flags.contains(Flags::DOUBLE_UNDERLINE),
        );
        out.set(CellFlags::CURLY_UNDERLINE, flags.contains(Flags::UNDERCURL));
        out.set(
            CellFlags::DOTTED_UNDERLINE,
            flags.contains(Flags::DOTTED_UNDERLINE),
        );
        out.set(
            CellFlags::DASHED_UNDERLINE,
            flags.contains(Flags::DASHED_UNDERLINE),
        );
        out.set(CellFlags::STRIKETHROUGH, flags.contains(Flags::STRIKEOUT));
        out.set(CellFlags::HIDDEN, flags.contains(Flags::HIDDEN));
        out.set(CellFlags::WIDE, flags.contains(Flags::WIDE_CHAR));
        out.set(
            CellFlags::WIDE_SPACER,
            flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER),
        );
        out.set(CellFlags::SELECTED, selected);
        Cell {
            ch: cell.c,
            fg,
            bg,
            flags: out,
        }
    }

    fn resolve(&self, color: Color, bold: bool) -> Rgb {
        match color {
            Color::Spec(rgb) => Rgb::new(rgb.r, rgb.g, rgb.b),
            Color::Indexed(index) => {
                let index = if bold && self.palette.bold_is_bright && index < 8 {
                    index + 8
                } else {
                    index
                };
                self.color_for_index(index as usize)
            }
            Color::Named(name) => {
                let name = if bold && self.palette.bold_is_bright {
                    name.to_bright()
                } else {
                    name
                };
                self.named(name)
            }
        }
    }

    fn named(&self, name: NamedColor) -> Rgb {
        self.color_for_index(name as usize)
    }

    /// Indexes follow alacritty's color table: 0 to 255 are the indexed
    /// colors, then foreground, background, cursor, the dim set, bright
    /// foreground, and dim foreground. An OSC override wins over the palette.
    fn color_for_index(&self, index: usize) -> Rgb {
        if let Some(Some(rgb)) =
            (index < alacritty_terminal::term::color::COUNT).then(|| self.term.colors()[index])
        {
            return Rgb::new(rgb.r, rgb.g, rgb.b);
        }
        let palette = &self.palette;
        match index {
            0..=255 => palette.indexed(index as u8),
            i if i == NamedColor::Foreground as usize => palette.foreground,
            i if i == NamedColor::Background as usize => palette.background,
            i if i == NamedColor::Cursor as usize => palette.cursor,
            i if i == NamedColor::BrightForeground as usize => palette.foreground,
            i if i == NamedColor::DimForeground as usize => palette.foreground.dimmed(),
            i if (NamedColor::DimBlack as usize..=NamedColor::DimWhite as usize).contains(&i) => {
                palette.ansi[i - NamedColor::DimBlack as usize].dimmed()
            }
            _ => palette.foreground,
        }
    }

    fn grid_point(&self, at: ViewportPoint) -> Point {
        let grid = self.term.grid();
        let row = at.row.min(grid.screen_lines().saturating_sub(1));
        let col = at.col.min(grid.columns().saturating_sub(1));
        Point::new(
            GridLine(row as i32 - grid.display_offset() as i32),
            Column(col),
        )
    }
}

fn direction(side: CellSide) -> Direction {
    match side {
        CellSide::Left => Direction::Left,
        CellSide::Right => Direction::Right,
    }
}

fn engine_rgb(rgb: Rgb) -> EngineRgb {
    EngineRgb {
        r: rgb.r,
        g: rgb.g,
        b: rgb.b,
    }
}

/// Group consecutive OSC 8 cells that share one openable URI. A wide-character
/// spacer stays inside the span of the cell before it.
fn hyperlink_spans(
    row: &alacritty_terminal::grid::Row<alacritty_terminal::term::cell::Cell>,
    cols: usize,
) -> Vec<LinkSpan> {
    let mut links = Vec::new();
    let mut open: Option<(u16, String)> = None;
    for col in 0..cols {
        let cell = &row[Column(col)];
        let spacer = cell
            .flags
            .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER);
        let uri = cell.hyperlink().and_then(|link| {
            let uri = link.uri().to_owned();
            openable_uri(&uri).then_some(uri)
        });
        let same = open
            .as_ref()
            .is_some_and(|(_, current)| uri.as_ref() == Some(current));
        if same || (uri.is_none() && spacer && open.is_some()) {
            continue;
        }
        if let Some((start, uri)) = open.take()
            && (col as u16) > start
        {
            links.push(LinkSpan {
                start_col: start,
                end_col: col as u16,
                uri,
            });
        }
        if let Some(uri) = uri {
            open = Some((col as u16, uri));
        }
    }
    if let Some((start, uri)) = open
        && (cols as u16) > start
    {
        links.push(LinkSpan {
            start_col: start,
            end_col: cols as u16,
            uri,
        });
    }
    links
}

fn modes_from(mode: TermMode) -> Modes {
    let mouse = if mode.contains(TermMode::MOUSE_MOTION) {
        MouseTracking::Motion
    } else if mode.contains(TermMode::MOUSE_DRAG) {
        MouseTracking::Drag
    } else if mode.contains(TermMode::MOUSE_REPORT_CLICK) {
        MouseTracking::Click
    } else {
        MouseTracking::Off
    };
    let mouse_encoding = if mode.contains(TermMode::SGR_MOUSE) {
        MouseEncoding::Sgr
    } else if mode.contains(TermMode::UTF8_MOUSE) {
        MouseEncoding::Utf8
    } else {
        MouseEncoding::Normal
    };
    Modes {
        app_cursor: mode.contains(TermMode::APP_CURSOR),
        app_keypad: mode.contains(TermMode::APP_KEYPAD),
        bracketed_paste: mode.contains(TermMode::BRACKETED_PASTE),
        focus_reporting: mode.contains(TermMode::FOCUS_IN_OUT),
        alt_screen: mode.contains(TermMode::ALT_SCREEN),
        alternate_scroll: mode.contains(TermMode::ALTERNATE_SCROLL),
        show_cursor: mode.contains(TermMode::SHOW_CURSOR),
        mouse,
        mouse_encoding,
    }
}

#[cfg(test)]
mod tests {
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
}
