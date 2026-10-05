//! Shika's own terminal types. Nothing here names the engine, so the view,
//! the input encoder, and later the app can stay the same if the engine is
//! swapped.

use bitflags::bitflags;

/// A plain 24-bit color.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    pub const fn hex(value: u32) -> Self {
        Self {
            r: (value >> 16) as u8,
            g: (value >> 8) as u8,
            b: value as u8,
        }
    }

    /// Two thirds of the brightness, the same rule xterm and alacritty use
    /// for faint text when no dim palette is set.
    pub fn dimmed(self) -> Self {
        let scale = |c: u8| ((c as u16 * 2) / 3) as u8;
        Self::new(scale(self.r), scale(self.g), scale(self.b))
    }
}

/// Where bytes for the program came from, so a host can tell the user's own
/// input from the traffic a terminal sends on its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputSource {
    /// Keys or a paste.
    Typed,
    /// Focus, mouse, and scroll reports from the view.
    Report,
    /// The engine answering a query in the program's output.
    Reply,
}

/// Grid size in cells, plus the pixel size of one cell so the PTY can
/// answer TIOCGWINSZ pixel queries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TerminalSize {
    pub rows: u16,
    pub cols: u16,
    pub cell_width: u16,
    pub cell_height: u16,
}

impl TerminalSize {
    pub const MIN_ROWS: u16 = 2;
    pub const MIN_COLS: u16 = 2;

    pub fn new(rows: u16, cols: u16) -> Self {
        Self {
            rows: rows.max(Self::MIN_ROWS),
            cols: cols.max(Self::MIN_COLS),
            cell_width: 0,
            cell_height: 0,
        }
    }

    pub fn with_cell(mut self, cell_width: u16, cell_height: u16) -> Self {
        self.cell_width = cell_width;
        self.cell_height = cell_height;
        self
    }

    pub fn same_grid(&self, other: &TerminalSize) -> bool {
        self.rows == other.rows && self.cols == other.cols
    }
}

impl Default for TerminalSize {
    fn default() -> Self {
        Self::new(24, 80)
    }
}

/// How the program running in the terminal asked for mouse events.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum MouseTracking {
    /// No reporting. The mouse selects text and the wheel scrolls history.
    #[default]
    Off,
    /// Press and release only (DECSET 1000).
    Click,
    /// Press, release, and motion while a button is held (DECSET 1002).
    Drag,
    /// Every motion (DECSET 1003).
    Motion,
}

/// How mouse reports are encoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum MouseEncoding {
    /// `CSI M Cb Cx Cy` with one byte per value, capped at 223.
    #[default]
    Normal,
    /// Same, with UTF-8 coordinates (DECSET 1005).
    Utf8,
    /// `CSI < b ; x ; y M/m` (DECSET 1006).
    Sgr,
}

/// Terminal modes the input side has to honor.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Modes {
    pub app_cursor: bool,
    pub app_keypad: bool,
    pub bracketed_paste: bool,
    pub focus_reporting: bool,
    pub alt_screen: bool,
    /// Wheel becomes arrow keys on the alternate screen (DECSET 1007).
    pub alternate_scroll: bool,
    pub show_cursor: bool,
    pub mouse: MouseTracking,
    pub mouse_encoding: MouseEncoding,
}

impl Modes {
    pub fn mouse_reporting(&self) -> bool {
        self.mouse != MouseTracking::Off
    }
}

bitflags! {
    /// Per-cell attributes after colors are resolved. Inverse is already
    /// applied to `fg` and `bg`, so it is not a flag here.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
    pub struct CellFlags: u16 {
        const BOLD             = 1 << 0;
        const ITALIC           = 1 << 1;
        const UNDERLINE        = 1 << 2;
        const DOUBLE_UNDERLINE = 1 << 3;
        const CURLY_UNDERLINE  = 1 << 4;
        const DOTTED_UNDERLINE = 1 << 5;
        const DASHED_UNDERLINE = 1 << 6;
        const STRIKETHROUGH    = 1 << 7;
        /// Text that should not be drawn (SGR 8).
        const HIDDEN           = 1 << 8;
        /// First half of a double-width character.
        const WIDE             = 1 << 9;
        /// Second half of a double-width character. Holds no glyph.
        const WIDE_SPACER      = 1 << 10;
        /// Inside the current selection.
        const SELECTED         = 1 << 11;
        /// The row continues on the next row (soft wrap). Only set on the
        /// last cell of a row.
        const WRAPPED          = 1 << 12;
        const ANY_UNDERLINE    = Self::UNDERLINE.bits()
                               | Self::DOUBLE_UNDERLINE.bits()
                               | Self::CURLY_UNDERLINE.bits()
                               | Self::DOTTED_UNDERLINE.bits()
                               | Self::DASHED_UNDERLINE.bits();
    }
}

/// One grid cell, ready to paint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cell {
    pub ch: char,
    pub fg: Rgb,
    pub bg: Rgb,
    pub flags: CellFlags,
}

impl Cell {
    /// Nothing to draw in the glyph pass.
    pub fn is_blank(&self) -> bool {
        self.ch == ' '
            || self.ch == '\0'
            || self
                .flags
                .intersects(CellFlags::HIDDEN | CellFlags::WIDE_SPACER)
    }
}

/// One visible row.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Line {
    pub cells: Vec<Cell>,
    /// Combining marks and other zero-width characters, by column. Almost
    /// always empty.
    pub combining: Vec<(u16, String)>,
}

impl Line {
    pub fn combining_at(&self, col: usize) -> Option<&str> {
        self.combining
            .iter()
            .find(|(c, _)| *c as usize == col)
            .map(|(_, s)| s.as_str())
    }

    /// The row as text, with trailing blanks trimmed.
    pub fn text(&self) -> String {
        let mut out = String::with_capacity(self.cells.len());
        for (col, cell) in self.cells.iter().enumerate() {
            if cell.flags.contains(CellFlags::WIDE_SPACER) {
                continue;
            }
            out.push(if cell.ch == '\0' { ' ' } else { cell.ch });
            if let Some(marks) = self.combining_at(col) {
                out.push_str(marks);
            }
        }
        let trimmed = out.trim_end_matches(' ').len();
        out.truncate(trimmed);
        out
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum CursorShape {
    #[default]
    Block,
    Underline,
    Beam,
}

/// Where to draw the cursor, in viewport coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cursor {
    pub row: usize,
    pub col: usize,
    pub shape: CursorShape,
    /// The cursor sits on a double-width character.
    pub wide: bool,
}

/// Everything the view needs to paint one frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub rows: usize,
    pub cols: usize,
    pub lines: Vec<Line>,
    /// `None` when the program hid the cursor or it is scrolled out of view.
    pub cursor: Option<Cursor>,
    pub modes: Modes,
    /// Rows scrolled back into history. Zero means the live screen.
    pub display_offset: usize,
    /// Rows of history available above the screen.
    pub history: usize,
    /// The effective default colors, after any OSC 10/11/12 overrides.
    pub foreground: Rgb,
    pub background: Rgb,
    pub cursor_color: Rgb,
}

impl Snapshot {
    /// Visible rows as text, trailing blanks trimmed.
    pub fn text_lines(&self) -> Vec<String> {
        self.lines.iter().map(Line::text).collect()
    }
}

/// A cell position in the visible viewport.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ViewportPoint {
    pub row: usize,
    pub col: usize,
}

/// Which half of a cell the pointer is over. Selection uses it to decide
/// whether the cell under the pointer is included.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CellSide {
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectionKind {
    /// Character by character, from a single click and drag.
    Simple,
    /// Whole words, from a double click.
    Word,
    /// Whole lines, from a triple click.
    Line,
}
