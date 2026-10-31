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
        self.text_lines_from(0)
    }

    /// The live screen preceded by up to `history` lines of scrollback, oldest
    /// first, as text. Reads the grid directly, so the viewport, selection,
    /// and cursor stay as they are. The alternate screen has no history.
    pub fn text_with_history(&self, history: usize) -> Vec<String> {
        self.text_lines_from(history.min(self.term.grid().history_size()))
    }

    fn text_lines_from(&self, history: usize) -> Vec<String> {
        let grid = self.term.grid();
        let rows = grid.screen_lines();
        let cols = grid.columns();
        let mut lines = Vec::with_capacity(rows + history);
        for row in -(history as i32)..rows as i32 {
            let source = &grid[GridLine(row)];
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
#[path = "engine_tests.rs"]
mod tests;
