//! The read-only Changes panel right of the terminal: the selected task's
//! diff against its base, as `Core::session_diff` reads it.
//!
//! The panel fetches, never watches. It reads when it opens, when the shown
//! card changes while it is open, when the shown card turns Ready, and on
//! `r`. Git and the row index run on the background executor; a result is
//! applied only while its generation is the newest and the card it was read
//! for is still the one shown. Closed, the panel holds nothing and runs
//! nothing.
//!
//! The list is a `uniform_list`: every row is one terminal row high (at
//! least 21px for header labels), so a 50,000-line diff costs about as much
//! per frame as a 50-line one. File cards are painted as row slices. Rows hold
//! indices into the diff, not text; a row's text is cut to the columns on
//! screen and made safe (tabs expanded, control characters drawn as
//! symbols) as it is painted. Long lines do not wrap. One horizontal offset,
//! kept here rather than in the list, shifts the line text of every row,
//! while the gutter and the headers stay put.
//!
//! Behavior, focus, and the refresh rules are in `docs/changes-panel.md`;
//! the look is under Changes panel in `design/DESIGN.md`.

use crate::appearance::{Chrome, DiffColors, with_alpha};
use crate::{BAR_HEIGHT, KeyTip, MIN_TERMINAL_WIDTH, MONO, Shika, TAB_HEIGHT, UI_FONT, kbd, model};
use gpui::{
    AnyElement, AppContext, Context, FocusHandle, Focusable, FontWeight, InteractiveElement,
    IntoElement, KeyDownEvent, MouseButton, ParentElement, Pixels, Render, Rgba, ScrollWheelEvent,
    StatefulInteractiveElement, Styled, UniformListScrollHandle, WeakEntity, Window, div,
    prelude::FluentBuilder, px,
};
use shika_core::{Changes, Collapse, DiffStat, FileDiff, FileStatus, LineKind, SessionDiff};
use std::{
    collections::HashSet,
    io::Write,
    ops::Range,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};
use unicode_width::UnicodeWidthChar;

/// A tab advances to the next multiple of this many columns, counted from
/// the start of the line.
pub const TAB_WIDTH: usize = 4;
/// Columns `h` / `l` and Left / Right move the line text.
const STEP_COLUMNS: usize = 8;
/// A newly shown card's list stays blank this long before "Reading changes".
const READING_AFTER: Duration = Duration::from_millis(500);
/// The line number gutter is at least this many cells wide.
const MIN_NUMBER_COLUMNS: usize = 4;
/// Card inset on both sides of the list, and content inset inside a card.
const INSET: f32 = 12.;
/// The list starts this far under the title row...
const LIST_TOP: f32 = 12.;
/// ...and ends this far after the last row, the terminal's bottom pad.
const LIST_BOTTOM: f32 = 28.;
/// The title row's hints show only when the panel is at least this wide.
const HINTS_FIT: f32 = 420.;
/// Row height as a multiple of the font size, the terminal's own.
const LINE_HEIGHT: f32 = 1.52;

/// The window's right pane split off: the column icon, mirrored. Drawn for
/// Shika like `COLUMN_ICON`.
pub const CHANGES_ICON: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><path transform="matrix(-1 0 0 1 16 0)" fill="#000" fill-rule="evenodd" d="M3 2h10a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H3a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2Zm3 1.5H3a.5.5 0 0 0-.5.5v8a.5.5 0 0 0 .5.5h3v-9Zm1.5 0v9H13a.5.5 0 0 0 .5-.5V4a.5.5 0 0 0-.5-.5H7.5Z"/></svg>"##;

/// The drag on the panel's left edge. It draws nothing: the panel follows
/// the pointer.
pub struct ChangesDrag;
impl Render for ChangesDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        gpui::Empty
    }
}

/// What the panel is showing, from the selected card.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    NoAgent,
    /// The card's worktree is being created or prepared.
    Preparing,
    /// Setup failed; the card has no worktree to read.
    SetupFailed,
    /// A live task, by session id.
    Session(String),
    /// The project's Lead, which works in a detached worktree and has no
    /// task changes to show.
    Lead,
}

/// The shown task's diff, as far as it has been read.
pub enum Content {
    /// The first read for this card is in flight.
    Reading {
        since: Instant,
    },
    Ready(Arc<DiffView>),
    /// Git failed. The text is its first error line.
    Failed(String),
}

/// One row of the list. Rows point into `DiffView::files`, so the index
/// costs 16 bytes a row whatever the line length.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Row {
    /// The empty row before every file header but the first.
    Spacer,
    File(u32),
    FileMeta(u32),
    /// Quiet separation between hunks, with no Git metadata.
    HunkGap(u32),
    /// The rounded bottom of a file card.
    End(u32),
    Line {
        file: u32,
        hunk: u32,
        line: u32,
    },
    /// "Binary file" under a binary file's header.
    Binary(u32),
    /// A file over a cap, expandable: "Large diff hidden", "Large file
    /// hidden", or "Diff hidden" (`collapsed_label`).
    Collapsed(u32),
    /// Lines past the hard limit of an expanded file.
    Cut(u32),
}

/// A diff flattened into rows, built off the UI thread once per result.
#[derive(Debug, Default)]
pub struct DiffView {
    pub files: Vec<Arc<FileDiff>>,
    pub stat: DiffStat,
    pub rows: Vec<Row>,
    /// Row of each file header, in order.
    pub headers: Vec<usize>,
    /// Row of each collapsed file's collapsed row, in order.
    pub collapsed: Vec<usize>,
    /// Each file's widest line in columns, so expanding one file does not
    /// measure the others again.
    widths: Vec<usize>,
    /// The widest line in columns.
    pub columns: usize,
    /// The gutter's width in cells: the widest line number, at least 4.
    pub number_columns: usize,
}

impl DiffView {
    pub fn new(diff: SessionDiff) -> Self {
        let files: Vec<Arc<FileDiff>> = diff.files.into_iter().map(Arc::new).collect();
        let widths = files.iter().map(|file| file_columns(file)).collect();
        Self::index(files, widths, diff.stat)
    }

    /// This view with file `at` replaced, as an expand returns it.
    pub fn with_file(&self, at: usize, file: FileDiff) -> Self {
        let mut files = self.files.clone();
        let mut widths = self.widths.clone();
        if at < files.len() {
            widths[at] = file_columns(&file);
            files[at] = Arc::new(file);
        }
        Self::index(files, widths, self.stat)
    }

    fn index(files: Vec<Arc<FileDiff>>, widths: Vec<usize>, stat: DiffStat) -> Self {
        let lines: usize = files
            .iter()
            .flat_map(|file| &file.hunks)
            .map(|hunk| hunk.lines.len() + 1)
            .sum();
        let mut rows = Vec::with_capacity(lines + files.len() * 3);
        let mut headers = Vec::with_capacity(files.len());
        let mut collapsed = Vec::new();
        let mut widest_number = 0u32;
        for (f, file) in files.iter().enumerate() {
            let at = f as u32;
            if f > 0 {
                rows.push(Row::Spacer);
            }
            headers.push(rows.len());
            rows.push(Row::File(at));
            rows.push(Row::FileMeta(at));
            if file.binary {
                rows.push(Row::Binary(at));
            } else if file.collapsed.is_some() {
                collapsed.push(rows.len());
                rows.push(Row::Collapsed(at));
            } else {
                for (h, hunk) in file.hunks.iter().enumerate() {
                    if h > 0 {
                        rows.push(Row::HunkGap(at));
                    }
                    for (l, line) in hunk.lines.iter().enumerate() {
                        widest_number = widest_number.max(line.old.unwrap_or(0));
                        widest_number = widest_number.max(line.new.unwrap_or(0));
                        rows.push(Row::Line {
                            file: at,
                            hunk: h as u32,
                            line: l as u32,
                        });
                    }
                }
                if file.hidden_lines > 0 {
                    rows.push(Row::Cut(at));
                }
            }
            rows.push(Row::End(at));
        }
        let columns = widths.iter().copied().max().unwrap_or(0);
        Self {
            files,
            stat,
            rows,
            headers,
            collapsed,
            widths,
            columns,
            number_columns: digits(widest_number).max(MIN_NUMBER_COLUMNS),
        }
    }

    /// The file a row belongs to.
    pub fn file_of(&self, row: usize) -> Option<usize> {
        match self.rows.get(row)? {
            Row::Spacer => None,
            Row::File(f)
            | Row::FileMeta(f)
            | Row::HunkGap(f)
            | Row::End(f)
            | Row::Binary(f)
            | Row::Collapsed(f)
            | Row::Cut(f) => Some(*f as usize),
            Row::Line { file, .. } => Some(*file as usize),
        }
    }
}

fn digits(n: u32) -> usize {
    n.checked_ilog10().map_or(1, |log| log as usize + 1)
}

/// The widest line of a file, in columns as painted.
fn file_columns(file: &FileDiff) -> usize {
    file.hunks
        .iter()
        .flat_map(|hunk| &hunk.lines)
        .filter(|line| line.kind != LineKind::NoNewline)
        .map(|line| columns(&line.text))
        .max()
        .unwrap_or(0)
}

/// Visible ranges into the unchanged full index. Folding costs O(files),
/// not O(lines), and never clones code or remeasures it on the UI thread.
#[derive(Clone, Debug, Default)]
struct VisibleRows {
    spans: Vec<(usize, Range<usize>)>,
    len: usize,
    headers: Vec<usize>,
    collapsed: Vec<usize>,
    folded: HashSet<usize>,
}

impl VisibleRows {
    fn new(view: &DiffView, folded: &HashSet<(String, FileStatus)>) -> Self {
        let mut visible = Self::default();
        for (f, file) in view.files.iter().enumerate() {
            let start = view.headers[f];
            let end = view
                .headers
                .get(f + 1)
                .map_or(view.rows.len(), |next| next - 1);
            if f > 0 {
                visible.push(start - 1..start);
            }
            visible.headers.push(visible.len);
            if folded.contains(&(file.path.clone(), file.status)) {
                visible.folded.insert(f);
                visible.push(start..start + 2);
                visible.push(end - 1..end);
            } else {
                if file.collapsed.is_some() {
                    visible.collapsed.push(visible.len + 2);
                }
                visible.push(start..end);
            }
        }
        visible
    }

    fn push(&mut self, rows: Range<usize>) {
        self.spans.push((self.len, rows.clone()));
        self.len += rows.len();
    }

    fn raw(&self, row: usize) -> Option<usize> {
        if row >= self.len {
            return None;
        }
        let at = self.spans.partition_point(|(start, _)| *start <= row) - 1;
        let (start, range) = &self.spans[at];
        Some(range.start + row - start)
    }

    fn position(&self, raw: usize) -> Option<usize> {
        let at = self.spans.partition_point(|(_, range)| range.end <= raw);
        let (start, range) = self.spans.get(at)?;
        range.contains(&raw).then(|| start + raw - range.start)
    }
}

/// Keep the top visible row in place across a fold. If it was inside the
/// newly folded body, fall back to that file's header instead.
fn folded_top(
    old: &VisibleRows,
    new: &VisibleRows,
    view: &DiffView,
    top: f32,
    row: f32,
    viewport: f32,
) -> f32 {
    let raw = old.raw((top / row).floor().max(0.) as usize);
    let position = raw.and_then(|raw| new.position(raw));
    let header = raw
        .and_then(|raw| view.file_of(raw))
        .and_then(|f| new.headers.get(f).copied());
    let next = position.map_or_else(
        || header.unwrap_or(0) as f32 * row,
        |at| at as f32 * row + top % row,
    );
    next.clamp(0., (new.len as f32 * row - viewport).max(0.))
}

/// A character as the panel paints it. Control characters become their
/// Unicode control pictures, so a stray escape cannot reach the text system
/// and a `\r` in the middle of a line shows. C1 controls and the
/// bidirectional overrides become the replacement character, so text cannot
/// reorder itself on screen.
fn shown(ch: char) -> char {
    match ch {
        '\u{0}'..='\u{1f}' => char::from_u32(0x2400 + ch as u32).unwrap_or('\u{fffd}'),
        '\u{7f}' => '\u{2421}',
        '\u{80}'..='\u{9f}'
        | '\u{61c}'
        | '\u{200e}'
        | '\u{200f}'
        | '\u{202a}'..='\u{202e}'
        | '\u{2066}'..='\u{2069}' => '\u{fffd}',
        ch => ch,
    }
}

/// A line's text without the `\r` of a CRLF ending, which every line of a
/// CRLF file has and which would only be noise.
fn without_cr(text: &str) -> &str {
    text.strip_suffix('\r').unwrap_or(text)
}

/// A character as painted, with the cells it takes: two for a wide
/// character (CJK, most emoji), none for a combining mark or another
/// zero-width character, one otherwise. Widths are per character, as a
/// terminal counts them, so an emoji sequence joined with U+200D counts each
/// of its emoji.
fn glyph(ch: char) -> (char, usize) {
    if matches!(ch, ' '..='~') {
        return (ch, 1);
    }
    let ch = shown(ch);
    (ch, UnicodeWidthChar::width(ch).unwrap_or(1))
}

/// The next tab stop after `column`.
fn tab_stop(column: usize) -> usize {
    (column / TAB_WIDTH + 1) * TAB_WIDTH
}

/// Columns a line takes as painted: tabs to the next `TAB_WIDTH` stop, other
/// characters their display width.
pub fn columns(text: &str) -> usize {
    without_cr(text).chars().fold(0, |column, ch| {
        if ch == '\t' {
            tab_stop(column)
        } else {
            column + glyph(ch).1
        }
    })
}

/// The part of a line painted from column `start`, at most `count` columns:
/// tabs expanded to spaces, a CRLF's `\r` dropped, control characters drawn
/// as symbols, and columns counted by display width. A wide character is
/// never split: when the left edge cuts it, its visible half is a space, and
/// one that starts in the last column is kept whole for the text area to
/// clip. Zero-width characters go with the character before them. Work stops
/// at the last column shown, so a very long line costs only the columns up to
/// the right edge, never the rest of the line.
pub fn visible_text(text: &str, start: usize, count: usize) -> String {
    let end = start.saturating_add(count);
    let mut out = String::with_capacity(count.min(text.len()));
    let mut column = 0;
    // Whether the last character that takes cells was painted, so a
    // combining mark after it is painted too, and one after a skipped
    // character is skipped with it.
    let mut painted = false;
    for ch in without_cr(text).chars() {
        if ch == '\t' {
            if column >= end {
                break;
            }
            let next = tab_stop(column);
            for _ in column.max(start)..next.min(end) {
                out.push(' ');
            }
            painted = next > start;
            column = next;
            continue;
        }
        let (ch, width) = glyph(ch);
        if width == 0 {
            if painted {
                out.push(ch);
            }
            continue;
        }
        if column >= end {
            break;
        }
        painted = column >= start;
        if painted {
            out.push(ch);
        } else {
            // A wide character that the left edge cuts shows its right half
            // as blank cells.
            for _ in start..(column + width).min(end) {
                out.push(' ');
            }
        }
        column += width;
    }
    out
}

/// A path or header made safe to paint on one line.
pub fn safe_label(text: &str) -> String {
    text.chars()
        .map(|ch| if ch == '\t' { ' ' } else { shown(ch) })
        .collect()
}

/// `4,812 lines`, or `1 line`.
pub fn lines_label(n: usize) -> String {
    if n == 1 {
        "1 line".to_string()
    } else {
        format!("{} lines", thousands(n))
    }
}

/// A collapsed file's row, worded for the cap that collapsed it: its own
/// line or size cap, or the task's total budget, which collapses small files
/// too. Every one expands the same way.
pub fn collapsed_label(file: &FileDiff) -> String {
    let lines = lines_label(file.hidden_lines);
    match file.collapsed {
        Some(Collapse::Size(bytes)) if file.status == FileStatus::Untracked => {
            format!("Large file hidden - {}", size_label(bytes))
        }
        Some(Collapse::Size(bytes)) => format!("Large diff hidden - {}", size_label(bytes)),
        Some(Collapse::Budget) => format!("Diff hidden - {lines}"),
        Some(Collapse::Lines) | None => format!("Large diff hidden - {lines}"),
    }
}

/// A size in decimal units, as Finder shows it: `512 bytes`, `3.2 MB`.
pub fn size_label(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if bytes < 1000 {
        return if bytes == 1 {
            "1 byte".to_string()
        } else {
            format!("{bytes} bytes")
        };
    }
    let mut value = bytes as f64 / 1000.;
    let mut unit = 0;
    // Round first, so 999,999 bytes reads 1.0 MB, not 1000.0 KB.
    while (value * 10.).round() >= 10_000. && unit + 1 < UNITS.len() {
        value /= 1000.;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

/// `4812` as `4,812`.
pub fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// A file header's path: `old → new` for a rename. Returns the text and
/// the arrow's byte range, which paints dimmer.
pub fn file_label(file: &FileDiff) -> (String, Option<Range<usize>>) {
    let path = safe_label(&file.path);
    match &file.old_path {
        Some(old) => {
            let old = safe_label(old);
            let arrow = old.len() + 1..old.len() + 1 + '\u{2192}'.len_utf8();
            (format!("{old} \u{2192} {path}"), Some(arrow))
        }
        None => (path, None),
    }
}

/// Keep the basename prominent; renames retain both complete paths below it.
fn card_labels(file: &FileDiff) -> (String, String) {
    let (directory, name) = file.path.rsplit_once('/').unwrap_or(("", &file.path));
    let directory = if file.old_path.is_some() {
        file_label(file).0
    } else {
        safe_label(directory)
    };
    (safe_label(name), directory)
}

/// The widths on screen of the agent column and the panel, from their
/// saved widths (`None` when hidden or closed). The terminal keeps
/// `min_terminal`: the panel gives way first, beside the column's saved
/// width, then the column, beside the panel as shown. Neither goes under
/// its minimum, so only then does the terminal shrink.
pub fn pane_widths(
    viewport: f32,
    min_terminal: f32,
    column: Option<f32>,
    panel: Option<f32>,
) -> (Option<f32>, Option<f32>) {
    let column_min = f32::from(shika_core::Column::MIN_WIDTH);
    let panel_min = f32::from(Changes::MIN_WIDTH);
    let panel = panel.map(|saved| {
        saved
            .min(viewport - min_terminal - column.unwrap_or(0.))
            .max(panel_min)
    });
    let column = column.map(|saved| {
        saved
            .min(viewport - min_terminal - panel.unwrap_or(0.))
            .max(column_min)
    });
    (column, panel)
}

/// The panel width a drag on its left edge asks for, before the saved
/// range applies: the pointer's distance from the window's right edge, but
/// never so wide that the terminal goes under `min_terminal` beside the
/// column as shown.
pub fn drag_width(viewport: f32, x: f32, min_terminal: f32, column: f32) -> f32 {
    (viewport - x).min(viewport - min_terminal - column)
}

/// A scroll the keys ask for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scroll {
    Rows(isize),
    HalfPage(isize),
    Page(isize),
    Top,
    Bottom,
    NextFile,
    PreviousFile,
}

/// The list's new top, in pixels from its first row, after `scroll` from
/// `top`. `row` is the row height, `viewport` the list's height, and
/// `headers` the file header rows in order. The result stays between the
/// first row and the last full page.
pub fn scroll_top(
    scroll: Scroll,
    top: f32,
    row: f32,
    rows: usize,
    viewport: f32,
    headers: &[usize],
) -> f32 {
    let max = (rows as f32 * row - viewport).max(0.);
    let page = (viewport / row).floor().max(1.);
    // Half a row of slack, so a header sitting at the top counts as there.
    let slack = row / 2.;
    let next = match scroll {
        Scroll::Rows(n) => top + n as f32 * row,
        Scroll::HalfPage(n) => top + n as f32 * (page / 2.).floor().max(1.) * row,
        Scroll::Page(n) => top + n as f32 * page * row,
        Scroll::Top => 0.,
        Scroll::Bottom => max,
        Scroll::NextFile => headers
            .iter()
            .map(|&header| header as f32 * row)
            .find(|&at| at > top + slack)
            .unwrap_or(top),
        Scroll::PreviousFile => headers
            .iter()
            .rev()
            .map(|&header| header as f32 * row)
            .find(|&at| at < top - slack)
            .unwrap_or(0.),
    };
    next.clamp(0., max)
}

/// The first of `rows` (sorted) on screen, with the list's top at `top` and
/// `viewport` high.
pub fn first_in_view(rows: &[usize], top: f32, row: f32, viewport: f32) -> Option<usize> {
    let first = (top / row).floor().max(0.) as usize;
    let last = ((top + viewport) / row).ceil().max(0.) as usize;
    let at = rows.partition_point(|&r| r < first);
    rows.get(at).copied().filter(|&r| r < last)
}

/// The list area's state, before any element is built.
#[derive(Debug)]
pub enum Body<'a> {
    /// One centered line, such as "No changes".
    Message(&'static str),
    /// The first read for a newly shown card, under 500ms old.
    Blank,
    /// [`COULD_NOT_READ`], with git's first error line under it unless empty.
    Failed(&'a str),
    List(&'a Arc<DiffView>),
}

/// The title of the failed state.
pub const COULD_NOT_READ: &str = "Could not read changes";

/// What the list area shows for `target` and `content` at `now`.
pub fn panel_body<'a>(target: Option<&Target>, content: &'a Content, now: Instant) -> Body<'a> {
    match (target, content) {
        (None | Some(Target::NoAgent), _) => Body::Message("No agent selected"),
        (Some(Target::Preparing), _) => Body::Message("Preparing worktree"),
        (Some(Target::SetupFailed), _) => Body::Message("Setup failed"),
        (Some(Target::Lead), _) => Body::Message("The Lead does not change files"),
        (Some(Target::Session(_)), Content::Reading { since }) => {
            if now.saturating_duration_since(*since) >= READING_AFTER {
                Body::Message("Reading changes")
            } else {
                Body::Blank
            }
        }
        (Some(Target::Session(_)), Content::Failed(line)) => Body::Failed(line),
        (Some(Target::Session(_)), Content::Ready(view)) if view.rows.is_empty() => {
            Body::Message("No changes")
        }
        (Some(Target::Session(_)), Content::Ready(view)) => Body::List(view),
    }
}

/// An error as the panel shows it under "Could not read changes": git's
/// first line when there is one.
fn error_line(error: &shika_core::Error) -> String {
    match error {
        shika_core::Error::ReadChanges(Some(line)) => line.clone(),
        shika_core::Error::ReadChanges(None) => String::new(),
        other => other.to_string(),
    }
}

/// Row height and cell width of the list's mono font at one size.
#[derive(Clone, Copy, Debug)]
pub struct Metrics {
    points: f32,
    pub font_size: Pixels,
    pub row: Pixels,
    pub cell: Pixels,
}

/// The panel's state. Closed, it holds only its width and focus handle.
pub struct Panel {
    pub open: bool,
    /// The saved width, as in `settings.json`.
    pub width: Changes,
    /// The width when the current drag on the edge began. The file is
    /// written once the drag ends.
    pub drag_from: Option<Changes>,
    pub focus: FocusHandle,
    /// The focus saved when the panel opened, restored once when it closes
    /// from inside.
    pub return_focus: Option<FocusHandle>,
    /// None until the next paint picks the selected card.
    target: Option<Target>,
    content: Content,
    generation: u64,
    scroll: UniformListScrollHandle,
    /// How far the line text is scrolled right, in pixels.
    shift: f32,
    /// The most `shift` can be at the last paint.
    max_shift: f32,
    /// Files folded by the author, kept across a refresh by path/status.
    folded: HashSet<(String, FileStatus)>,
    visible: Arc<VisibleRows>,
    /// Files expanded on this card, kept across a refresh by path.
    expanded: HashSet<(String, FileStatus)>,
    /// Files with an expand in flight, by index in the shown view.
    expanding: HashSet<usize>,
    metrics: Option<Metrics>,
    /// `--diagnostics-file` plus `.changes`: read timings, no content.
    diagnostics: Option<PathBuf>,
}

impl Panel {
    pub fn new(width: Changes, focus: FocusHandle, diagnostics: Option<PathBuf>) -> Self {
        Self {
            open: false,
            width,
            drag_from: None,
            focus,
            return_focus: None,
            target: None,
            content: Content::Reading {
                since: Instant::now(),
            },
            generation: 0,
            scroll: UniformListScrollHandle::new(),
            shift: 0.,
            max_shift: 0.,
            folded: HashSet::new(),
            visible: Arc::new(VisibleRows::default()),
            expanded: HashSet::new(),
            expanding: HashSet::new(),
            metrics: None,
            diagnostics: diagnostics.map(|path| {
                let mut name = path.into_os_string();
                name.push(".changes");
                PathBuf::from(name)
            }),
        }
    }

    /// Whether the panel shows this session.
    pub fn shows(&self, id: &str) -> bool {
        self.open && matches!(&self.target, Some(Target::Session(shown)) if shown == id)
    }

    fn view(&self) -> Option<&Arc<DiffView>> {
        match &self.content {
            Content::Ready(view) => Some(view),
            _ => None,
        }
    }

    /// Forget the shown diff: a new card, or the panel closing. A pending
    /// read's generation no longer matches, so its result is dropped.
    fn clear(&mut self) {
        self.generation += 1;
        self.content = Content::Reading {
            since: Instant::now(),
        };
        self.scroll = UniformListScrollHandle::new();
        self.shift = 0.;
        self.folded.clear();
        self.visible = Arc::new(VisibleRows::default());
        self.expanded.clear();
        self.expanding.clear();
    }

    fn set_view(&mut self, view: DiffView) {
        self.visible = Arc::new(VisibleRows::new(&view, &self.folded));
        self.content = Content::Ready(Arc::new(view));
    }

    fn top_and_viewport(&self) -> (f32, f32) {
        let state = self.scroll.0.borrow();
        let handle = &state.base_handle;
        (
            -f32::from(handle.offset().y),
            f32::from(handle.bounds().size.height),
        )
    }
}

/// Everything a row needs to paint, cloned into the list's render closure.
#[derive(Clone)]
struct RowPaint {
    row: Pixels,
    cell: Pixels,
    /// First line column shown, and the part of a cell scrolled past it.
    start: usize,
    offset: Pixels,
    /// Line columns painted per row, enough to fill the text area.
    count: usize,
    dim: Rgba,
    text: Rgba,
    faint: Rgba,
    line: Rgba,
    hover: Rgba,
    header: Rgba,
    header_dim: Rgba,
    header_white: Rgba,
    header_added: Rgba,
    header_removed: Rgba,
    added: DiffColors,
    removed: DiffColors,
    shika: WeakEntity<Shika>,
}

impl RowPaint {
    fn base(&self) -> gpui::Div {
        div()
            .h(self.row)
            .w_full()
            .flex()
            .items_center()
            .px(px(INSET))
            .whitespace_nowrap()
            .overflow_hidden()
    }

    fn render(&self, view: &DiffView, ix: usize, folded: bool) -> AnyElement {
        let Some(row) = view.rows.get(ix) else {
            return self.base().into_any_element();
        };
        let content = match *row {
            Row::Spacer | Row::End(_) => self.base().into_any_element(),
            Row::File(f) => self.file_header(&view.files[f as usize], f as usize, folded, false),
            Row::FileMeta(f) => self.file_header(&view.files[f as usize], f as usize, folded, true),
            Row::HunkGap(_) => self
                .base()
                .border_t_1()
                .border_color(self.line)
                .into_any_element(),
            Row::Line { file, hunk, line } => self.line(
                &view.files[file as usize].hunks[hunk as usize].lines[line as usize],
                view.number_columns,
            ),
            Row::Binary(_) => self
                .base()
                .text_size(px(11.5))
                .text_color(self.dim)
                .child("Binary file")
                .into_any_element(),
            Row::Collapsed(f) => self.collapsed(ix, f as usize, &view.files[f as usize]),
            Row::Cut(f) => self
                .base()
                .text_size(px(11.5))
                .text_color(self.dim)
                .child(format!(
                    "{} not shown",
                    lines_label(view.files[f as usize].hidden_lines)
                ))
                .into_any_element(),
        };
        if matches!(row, Row::Spacer) {
            return content;
        }
        // Each visible row paints a slice of the card. No per-file entity,
        // nested list, shadow, or offscreen code is laid out.
        div()
            .h(self.row)
            .w_full()
            .px(px(INSET))
            .child(
                div()
                    .h_full()
                    .w_full()
                    .overflow_hidden()
                    .border_l_1()
                    .border_r_1()
                    .border_color(self.line)
                    .when(matches!(row, Row::File(_)), |d| {
                        d.border_t_1().rounded_tl(px(10.)).rounded_tr(px(10.))
                    })
                    .when(matches!(row, Row::End(_)), |d| {
                        d.border_b_1().rounded_bl(px(10.)).rounded_br(px(10.))
                    })
                    .when(
                        matches!(row, Row::File(_) | Row::FileMeta(_))
                            || (folded && matches!(row, Row::End(_))),
                        |d| d.bg(self.header),
                    )
                    .when(matches!(row, Row::FileMeta(_)) && !folded, |d| {
                        d.border_b_1()
                    })
                    .child(content),
            )
            .into_any_element()
    }

    /// Two uniform-height header rows: filename/counts, then directory/status.
    fn file_header(&self, file: &FileDiff, f: usize, folded: bool, metadata: bool) -> AnyElement {
        let shika = self.shika.clone();
        let identity = (file.path.clone(), file.status);
        let hover = self.hover;
        let (name, directory) = card_labels(file);
        let status = match file.status {
            FileStatus::Added => "Added",
            FileStatus::Modified => "Modified",
            FileStatus::Deleted => "Deleted",
            FileStatus::Renamed => "Renamed",
            FileStatus::Untracked => "Untracked",
        };
        let header = self
            .base()
            .id((
                if metadata {
                    "changes-file-meta"
                } else {
                    "changes-file"
                },
                f,
            ))
            .gap(px(8.))
            .cursor_pointer()
            .font_family(MONO)
            .line_height(px(16.))
            .when(!metadata, |d| d.rounded_tl(px(10.)).rounded_tr(px(10.)))
            .hover(move |style| style.bg(hover))
            .on_click(move |_, _, cx| {
                let _ = shika.update(cx, |this, cx| {
                    this.toggle_changes_file(identity.clone(), cx)
                });
            });
        if metadata {
            return header
                .text_size(px(10.5))
                .text_color(self.header_dim)
                .child(div().flex_none().w(px(10.)))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_ellipsis_start()
                        .overflow_hidden()
                        .child(directory),
                )
                .child(div().flex_none().font_family(UI_FONT).child(status))
                .into_any_element();
        }
        header
            .text_size(px(12.5))
            .text_color(self.header_white)
            .child(
                div()
                    .flex_none()
                    .w(px(10.))
                    .font_family(UI_FONT)
                    .text_color(self.header_dim)
                    .child(if folded { "›" } else { "⌄" }),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .font_weight(FontWeight::MEDIUM)
                    .child(name),
            )
            .child(
                div()
                    .flex_none()
                    .flex()
                    .gap(px(8.))
                    .text_size(px(11.5))
                    .when(file.insertions > 0, |d| {
                        d.child(
                            div()
                                .text_color(self.header_added)
                                .child(format!("+{}", file.insertions)),
                        )
                    })
                    .when(file.deletions > 0, |d| {
                        d.child(
                            div()
                                .text_color(self.header_removed)
                                .child(format!("\u{2212}{}", file.deletions)),
                        )
                    }),
            )
            .into_any_element()
    }

    fn line(&self, line: &shika_core::DiffLine, number_columns: usize) -> AnyElement {
        let gutter = div()
            .flex_none()
            .w(self.cell * (number_columns + 1) as f32)
            .overflow_hidden();
        if line.kind == LineKind::NoNewline {
            return self
                .base()
                .child(gutter)
                .child(div().text_color(self.dim).child(safe_label(&line.text)))
                .into_any_element();
        }
        let (colors, number) = match line.kind {
            LineKind::Added => (Some(self.added), line.new),
            LineKind::Removed => (Some(self.removed), line.old),
            _ => (None, line.new),
        };
        let color = colors.map_or(self.text, |colors| colors.text);
        let number = number.map(|n| n.to_string()).unwrap_or_default();
        let numbers = format!("{number:>number_columns$} ");
        self.base()
            .when_some(colors, |d, colors| d.bg(colors.tint))
            .child(gutter.text_color(self.dim).child(numbers))
            .child(
                div().flex_1().min_w_0().h_full().overflow_hidden().child(
                    div()
                        .relative()
                        .left(-self.offset)
                        .h_full()
                        .flex()
                        .items_center()
                        .text_color(color)
                        .child(visible_text(&line.text, self.start, self.count)),
                ),
            )
            .into_any_element()
    }

    fn collapsed(&self, ix: usize, f: usize, file: &FileDiff) -> AnyElement {
        let shika = self.shika.clone();
        let hover = self.hover;
        let text = collapsed_label(file);
        self.base()
            .id(("changes-collapsed", ix))
            .gap(px(12.))
            .cursor_pointer()
            .hover(move |style| style.bg(hover))
            .on_click(move |_, _, cx| {
                let _ = shika.update(cx, |this, cx| this.expand_changes_file(f, cx));
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(11.5))
                    .text_color(self.dim)
                    .child(text),
            )
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .font_family(UI_FONT)
                    .text_size(px(11.5))
                    .line_height(px(14.))
                    .text_color(self.faint)
                    .child(kbd("o", self.line, self.dim))
                    .child("show"),
            )
            .into_any_element()
    }
}

impl Shika {
    /// What the panel should show for the selected card.
    fn changes_target(&self) -> Target {
        let Some(card) = self.selected_card().map(|i| &self.cards[i]) else {
            return Target::NoAgent;
        };
        if card.creating {
            return Target::Preparing;
        }
        if card.lead.is_some() {
            return Target::Lead;
        }
        match &card.session {
            Some(session) => Target::Session(session.id.clone()),
            None if card.launch_error.is_some() => Target::SetupFailed,
            None => Target::Preparing,
        }
    }

    /// Follow the selected card. Runs on every paint while the panel is
    /// open; it only acts when the card changed, so a newly shown card
    /// drops the old diff at once and starts its own read.
    pub fn sync_changes(&mut self, cx: &mut Context<Self>) {
        if !self.changes.open {
            return;
        }
        let target = self.changes_target();
        if self.changes.target.as_ref() == Some(&target) {
            return;
        }
        self.changes.clear();
        let session = matches!(target, Target::Session(_));
        self.changes.target = Some(target);
        if session {
            self.fetch_changes(cx);
            let generation = self.changes.generation;
            // Nothing for READING_AFTER is blank; then the list says so.
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(READING_AFTER).await;
                let _ = this.update(cx, |this, cx| {
                    if this.changes.generation == generation
                        && matches!(this.changes.content, Content::Reading { .. })
                    {
                        cx.notify();
                    }
                });
            })
            .detach();
        }
    }

    /// Read the shown task's diff again, off the UI thread. The rows on
    /// screen stay until the result arrives; a newer read or another card
    /// makes this one's result stale, and it is dropped.
    pub fn fetch_changes(&mut self, cx: &mut Context<Self>) {
        let Some(Target::Session(id)) = self.changes.target.clone() else {
            return;
        };
        self.changes.generation += 1;
        let generation = self.changes.generation;
        let expanded: Vec<(String, FileStatus)> = self.changes.expanded.iter().cloned().collect();
        let diagnostics = self.changes.diagnostics.clone();
        let core = self.core.clone();
        cx.spawn(async move |this, cx| {
            let session = id.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    let started = Instant::now();
                    let mut diff = core.session_diff(&id).map_err(|e| error_line(&e))?;
                    let read = started.elapsed();
                    // Files expanded before this refresh stay expanded.
                    for file in diff.files.iter_mut() {
                        if file.collapsed.is_some()
                            && expanded
                                .iter()
                                .any(|(path, status)| *path == file.path && *status == file.status)
                            && let Ok(Some(full)) = core.session_file_diff(&id, &file.key)
                        {
                            *file = full;
                        }
                    }
                    let indexed = Instant::now();
                    let view = DiffView::new(diff);
                    if let Some(path) = diagnostics {
                        note_timing(&path, read, indexed.elapsed(), &view);
                    }
                    Ok::<_, String>(view)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.changes.generation != generation || !this.changes.shows(&session) {
                    return;
                }
                match result {
                    Ok(view) => this.changes.set_view(view),
                    Err(line) => this.changes.content = Content::Failed(line),
                }
                this.changes.expanding.clear();
                cx.notify();
            });
        })
        .detach();
    }

    /// Fold a file without Git reads or rebuilding the full line index.
    fn toggle_changes_file(&mut self, identity: (String, FileStatus), cx: &mut Context<Self>) {
        if self.busy || self.overlay.is_some() || !self.changes.open {
            return;
        }
        let Some(view) = self.changes.view().cloned() else {
            return;
        };
        if !view
            .files
            .iter()
            .any(|file| file.path == identity.0 && file.status == identity.1)
        {
            return;
        }
        if !self.changes.folded.remove(&identity) {
            self.changes.folded.insert(identity);
        }
        let visible = VisibleRows::new(&view, &self.changes.folded);
        if let Some(metrics) = self.changes.metrics {
            let (top, viewport) = self.changes.top_and_viewport();
            let next = folded_top(
                &self.changes.visible,
                &visible,
                &view,
                top,
                f32::from(metrics.row),
                viewport,
            );
            let state = self.changes.scroll.0.borrow();
            let offset = state.base_handle.offset();
            state
                .base_handle
                .set_offset(gpui::point(offset.x, px(-next)));
        }
        self.changes.visible = Arc::new(visible);
        cx.notify();
    }

    /// Expand a collapsed file in place, through `Core::session_file_diff`.
    /// A file that no longer differs means the diff is stale: read it again.
    pub fn expand_changes_file(&mut self, file: usize, cx: &mut Context<Self>) {
        let Some(Target::Session(id)) = self.changes.target.clone() else {
            return;
        };
        let Some(view) = self.changes.view().cloned() else {
            return;
        };
        let Some(diff) = view.files.get(file) else {
            return;
        };
        if diff.collapsed.is_none() || !self.changes.expanding.insert(file) {
            return;
        }
        let identity = (diff.path.clone(), diff.status);
        self.changes.expanded.insert(identity.clone());
        let key = diff.key.clone();
        let core = self.core.clone();
        cx.spawn(async move |this, cx| {
            let session = id.clone();
            let base = view.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    core.session_file_diff(&id, &key)
                        .map(|found| found.map(|full| view.with_file(file, full)))
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if !this.changes.shows(&session) {
                    return;
                }
                this.changes.expanding.remove(&file);
                let current = this.changes.view().cloned();
                match result {
                    Ok(Some(expanded)) => match current {
                        Some(current) if Arc::ptr_eq(&current, &base) => {
                            this.changes.set_view(expanded);
                        }
                        // A refresh landed meanwhile. Expand the same file
                        // in the new rows if it is still collapsed there.
                        Some(current) => {
                            if let Some(at) = current.files.iter().position(|f| {
                                f.collapsed.is_some()
                                    && f.path == identity.0
                                    && f.status == identity.1
                            }) {
                                this.expand_changes_file(at, cx);
                            }
                        }
                        None => {}
                    },
                    Ok(None) => this.fetch_changes(cx),
                    Err(error) => this.message(error.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Open the panel and focus it, saving `return_focus` for its close.
    fn open_changes(
        &mut self,
        return_focus: Option<FocusHandle>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.changes.open {
            self.changes.open = true;
            self.changes.target = None;
        }
        self.changes.return_focus = return_focus;
        window.focus(&self.changes.focus, cx);
        cx.notify();
    }

    /// Close the panel and drop its diff. From inside the panel, focus goes
    /// back to where it was when the panel opened; from anywhere else it
    /// stays put.
    pub fn close_changes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.changes.open {
            return;
        }
        let inside = self.changes.focus.is_focused(window);
        self.changes.open = false;
        self.changes.target = None;
        self.changes.clear();
        let saved = self.changes.return_focus.take();
        if inside {
            self.restore_changes_focus(saved, window, cx);
        }
        cx.notify();
    }

    /// The saved focus, if it still leads somewhere: the cards, or a live
    /// terminal. A terminal of another card than the one now selected gives
    /// way to the selected card's terminal, since the panel followed the
    /// selection. A closed view falls back to the cards.
    fn restore_changes_focus(
        &mut self,
        saved: Option<FocusHandle>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let terminal = saved.as_ref().filter(|handle| {
            self.cards.iter().any(|card| {
                std::iter::once(&card.agent)
                    .chain(card.shells.iter())
                    .any(|pane| pane.view.focus_handle(cx) == **handle)
            })
        });
        let Some(handle) = terminal.cloned() else {
            window.focus(&self.focus, cx);
            return;
        };
        let selected = self
            .selected_card()
            .map(|i| self.cards[i].active_pane().view.focus_handle(cx));
        if selected.as_ref() == Some(&handle) {
            window.focus(&handle, cx);
        } else if selected.is_some() && !self.busy {
            self.focus_terminal(window, cx);
        } else {
            window.focus(&self.focus, cx);
        }
    }

    /// Cmd+Option+B, the toggle buttons, and the View menu. Closing while
    /// something else has focus moves no focus.
    pub fn toggle_changes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.overlay.is_some() {
            return;
        }
        if self.changes.open {
            self.close_changes(window, cx);
        } else {
            let saved = window.focused(cx);
            self.open_changes(saved, window, cx);
        }
    }

    /// The panel's plain keys. They run only while the panel has focus, and
    /// none of them reaches a PTY.
    pub fn changes_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let stroke = &event.keystroke;
        let mods = &stroke.modifiers;
        if mods.platform || mods.control || mods.alt || mods.function {
            return;
        }
        let scroll = match (stroke.key.as_str(), mods.shift) {
            ("escape", false) => {
                self.close_changes(window, cx);
                None
            }
            ("j" | "down", false) => Some(Scroll::Rows(1)),
            ("k" | "up", false) => Some(Scroll::Rows(-1)),
            ("d", false) => Some(Scroll::HalfPage(1)),
            ("u", false) => Some(Scroll::HalfPage(-1)),
            ("space", false) => Some(Scroll::Page(1)),
            ("space", true) => Some(Scroll::Page(-1)),
            ("g", false) => Some(Scroll::Top),
            ("g", true) => Some(Scroll::Bottom),
            ("]", false) => Some(Scroll::NextFile),
            ("[", false) => Some(Scroll::PreviousFile),
            ("h" | "left", false) => {
                self.shift_changes(-self.step_columns_px(), cx);
                None
            }
            ("l" | "right", false) => {
                self.shift_changes(self.step_columns_px(), cx);
                None
            }
            ("o", false) => {
                self.expand_first_in_view(cx);
                None
            }
            ("r", false) => {
                self.fetch_changes(cx);
                None
            }
            _ => return,
        };
        cx.stop_propagation();
        if let Some(scroll) = scroll {
            self.scroll_changes(scroll, cx);
        }
    }

    fn step_columns_px(&self) -> f32 {
        self.changes
            .metrics
            .map_or(0., |m| f32::from(m.cell) * STEP_COLUMNS as f32)
    }

    fn scroll_changes(&mut self, scroll: Scroll, cx: &mut Context<Self>) {
        let (Some(_), Some(metrics)) = (self.changes.view(), self.changes.metrics) else {
            return;
        };
        let (top, viewport) = self.changes.top_and_viewport();
        let next = scroll_top(
            scroll,
            top,
            f32::from(metrics.row),
            self.changes.visible.len,
            viewport,
            &self.changes.visible.headers,
        );
        let state = self.changes.scroll.0.borrow();
        let offset = state.base_handle.offset();
        state
            .base_handle
            .set_offset(gpui::point(offset.x, px(-next)));
        drop(state);
        cx.notify();
    }

    /// Move the line text right (positive) or left, within the widest line.
    fn shift_changes(&mut self, delta: f32, cx: &mut Context<Self>) {
        let next = (self.changes.shift + delta).clamp(0., self.changes.max_shift);
        if next != self.changes.shift {
            self.changes.shift = next;
            cx.notify();
        }
    }

    /// `o`: the first collapsed file on screen.
    fn expand_first_in_view(&mut self, cx: &mut Context<Self>) {
        let (Some(view), Some(metrics)) = (self.changes.view(), self.changes.metrics) else {
            return;
        };
        let (top, viewport) = self.changes.top_and_viewport();
        let found = first_in_view(
            &self.changes.visible.collapsed,
            top,
            f32::from(metrics.row),
            viewport,
        )
        .and_then(|row| self.changes.visible.raw(row))
        .and_then(|row| view.file_of(row));
        if let Some(file) = found {
            self.expand_changes_file(file, cx);
        }
    }

    /// A drag on the panel's edge. It stops at 320 and where the terminal
    /// would go under 420, never closes the panel, and never moves the
    /// column. The width is saved once the drag ends.
    pub fn drag_changes(&mut self, x: Pixels, window: &mut Window, cx: &mut Context<Self>) {
        cx.set_active_drag_cursor_style(gpui::CursorStyle::ResizeLeftRight, window);
        self.changes.drag_from.get_or_insert(self.changes.width);
        let viewport = f32::from(window.viewport_size().width);
        let column = self.column_width(window).map_or(0., f32::from);
        let width = drag_width(viewport, f32::from(x), MIN_TERMINAL_WIDTH, column);
        let next = self.changes.width.with_width(width);
        if next != self.changes.width {
            self.changes.width = next;
            cx.notify();
        }
    }

    fn changes_metrics(&mut self, window: &Window) -> Metrics {
        let points = self.font_size.points();
        if let Some(metrics) = self.changes.metrics
            && metrics.points == points
        {
            return metrics;
        }
        let font_size = px(points);
        let text = window.text_system();
        let font = text.resolve_font(&gpui::font(MONO));
        let cell = text
            .advance(font, font_size, 'm')
            .map(|advance| advance.width)
            .unwrap_or(font_size * 0.6);
        let metrics = Metrics {
            points,
            font_size,
            row: (font_size * LINE_HEIGHT).round().max(px(21.)),
            cell,
        };
        self.changes.metrics = Some(metrics);
        metrics
    }

    /// The show/hide button: at the right end of the terminal header while
    /// the panel is closed, and of the panel's title row while it is open,
    /// 12 from the window's edge in both places.
    pub fn changes_toggle(&self, chrome: &Chrome, cx: &mut Context<Self>) -> impl IntoElement {
        let (tip_bg, tip_fg) = (chrome.toast_bg, chrome.toast_fg);
        let tip = if self.changes.open {
            "Hide changes  \u{2325}\u{2318}B"
        } else {
            "Show changes  \u{2325}\u{2318}B"
        };
        let (hover, ink_hover) = (chrome.term_hover, chrome.term_white);
        div()
            .id("changes-toggle")
            .group("changes-toggle")
            .occlude()
            .flex_none()
            .size(px(24.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(6.))
            .cursor_pointer()
            .hover(move |style| style.bg(hover))
            .tooltip(move |_, cx| {
                cx.new(|_| KeyTip {
                    bg: tip_bg,
                    fg: tip_fg,
                    text: tip.into(),
                })
                .into()
            })
            .on_click(cx.listener(|this, _, window, cx| this.toggle_changes(window, cx)))
            .child(
                gpui::svg()
                    .data(CHANGES_ICON)
                    .size(px(15.))
                    .text_color(chrome.term_dim)
                    .group_hover("changes-toggle", move |style| style.text_color(ink_hover)),
            )
    }

    /// The invisible strip over the panel's left edge. Dragging it resizes
    /// the panel; a double-click restores 480.
    pub fn changes_handle(
        &self,
        left: Pixels,
        chrome: &Chrome,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let line = chrome.ink_4;
        div()
            .id("changes-handle")
            .group("changes-handle")
            .occlude()
            .absolute()
            .top_0()
            .bottom_0()
            .left(left - px(crate::COLUMN_HANDLE_WIDTH / 2.))
            .w(px(crate::COLUMN_HANDLE_WIDTH))
            .flex()
            .justify_center()
            .cursor_col_resize()
            .on_drag(ChangesDrag, |_, _, _, cx| cx.new(|_| ChangesDrag))
            .on_click(cx.listener(|this, event: &gpui::ClickEvent, _, cx| {
                if event.click_count() == 2 {
                    this.changes.width = Changes::default();
                    this.save_settings();
                    cx.notify();
                }
            }))
            .child(
                div()
                    .w(px(2.))
                    .h_full()
                    .when(self.changes.drag_from.is_some(), |d| d.bg(line))
                    .group_hover("changes-handle", move |style| style.bg(line)),
            )
    }

    /// The panel: its title row, then the list or an empty state. It paints
    /// term-surface once; rows paint only their opaque tints.
    pub fn changes_panel(
        &mut self,
        width: Pixels,
        chrome: &Chrome,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let focused = self.changes.focus.is_focused(window);
        let metrics = self.changes_metrics(window);
        let stat = self
            .changes
            .view()
            .filter(|view| view.stat.files > 0)
            .map(|view| {
                model::diff_stat_label(view.stat.files, view.stat.insertions, view.stat.deletions)
            });
        let hint = |key: &'static str, label: &'static str| {
            div()
                .flex_none()
                .flex()
                .items_center()
                .gap(px(4.))
                .text_size(px(11.5))
                .line_height(px(14.))
                .text_color(chrome.term_faint)
                .child(kbd(key, chrome.term_line, chrome.term_dim))
                .child(label)
        };
        let title = div()
            .id("changes-title")
            .h(px(BAR_HEIGHT))
            .flex_shrink_0()
            .flex()
            .flex_col()
            .justify_end()
            .bg(with_alpha(chrome.term_header, chrome.term_header_alpha))
            .border_b_1()
            .border_color(chrome.term_line)
            .child(
                div()
                    .h(px(TAB_HEIGHT))
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .pl(px(20.))
                    .pr(px(12.))
                    .whitespace_nowrap()
                    .child(
                        div()
                            .flex_none()
                            .text_size(px(13.))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(if focused {
                                chrome.term_white
                            } else {
                                chrome.term_fg
                            })
                            .child("Changes"),
                    )
                    .when_some(stat, |d, stat| {
                        d.child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_size(px(12.))
                                .text_color(chrome.term_dim)
                                .child(stat),
                        )
                    })
                    .when(focused && f32::from(width) >= HINTS_FIT, |d| {
                        d.child(
                            div()
                                .flex_none()
                                .flex()
                                .gap(px(12.))
                                .child(hint("r", "refresh"))
                                .child(hint("esc", "close")),
                        )
                    })
                    .child(div().flex_1())
                    .child(self.changes_toggle(chrome, cx)),
            );
        let title = self.title_drag(title, cx);
        let body = div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .bg(chrome.term_surface);
        let content = match panel_body(
            self.changes.target.as_ref(),
            &self.changes.content,
            Instant::now(),
        ) {
            Body::Message(text) => body_empty(chrome).child(text).into_any_element(),
            Body::Blank => div().into_any_element(),
            Body::Failed(line) => body_empty(chrome)
                .gap(px(6.))
                .child(COULD_NOT_READ)
                .when(!line.is_empty(), |d| {
                    d.child(
                        div()
                            .max_w_full()
                            .px(px(INSET))
                            .truncate()
                            .font_family(MONO)
                            .text_size(px(11.5))
                            .text_color(chrome.term_fainter)
                            .child(safe_label(line)),
                    )
                })
                .into_any_element(),
            Body::List(view) => {
                let view = view.clone();
                self.changes_list(view, width, metrics, chrome, cx)
            }
        };
        div()
            .id("changes")
            .track_focus(&self.changes.focus)
            .key_context("Changes")
            .on_key_down(cx.listener(Self::changes_key))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    window.focus(&this.changes.focus, cx);
                    cx.notify();
                }),
            )
            .relative()
            .flex_none()
            .w(width)
            .h_full()
            .flex()
            .flex_col()
            .text_color(chrome.term_fg)
            .child(title)
            .child(body.child(content))
            // The left hairline, over the panel's own fills: the window
            // under it can be transparent.
            .child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left_0()
                    .w(px(1.))
                    .bg(chrome.term_line),
            )
    }

    fn changes_list(
        &mut self,
        view: Arc<DiffView>,
        width: Pixels,
        metrics: Metrics,
        chrome: &Chrome,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let cell = f32::from(metrics.cell).max(1.);
        // Room for line text: the panel less both insets and the gutter.
        let text_width =
            f32::from(width) - 4. * INSET - 2. - cell * (view.number_columns + 1) as f32;
        let text_columns = (text_width / cell).floor().max(1.) as usize;
        self.changes.max_shift = view.columns.saturating_sub(text_columns) as f32 * cell;
        self.changes.shift = self.changes.shift.clamp(0., self.changes.max_shift);
        let start = (self.changes.shift / cell).floor() as usize;
        let paint = RowPaint {
            row: metrics.row,
            cell: metrics.cell,
            start,
            offset: px(self.changes.shift - start as f32 * cell),
            count: text_columns + 2,
            dim: chrome.term_dim,
            text: chrome.term_text,
            faint: chrome.term_faint,
            line: chrome.term_line,
            hover: chrome.term_hover,
            header: chrome.diff_file_header(),
            header_dim: chrome.diff_file_header_text(chrome.term_dim),
            header_white: chrome.diff_file_header_text(chrome.term_white),
            header_added: chrome.diff_file_header_text(chrome.diff_added.text),
            header_removed: chrome.diff_file_header_text(chrome.diff_removed.text),
            added: chrome.diff_added,
            removed: chrome.diff_removed,
            shika: cx.entity().downgrade(),
        };
        let row = metrics.row;
        let visible = self.changes.visible.clone();
        let mut list = gpui::uniform_list("changes-rows", visible.len, move |range, _, _| {
            range
                .filter_map(|ix| visible.raw(ix))
                .map(|ix| {
                    let is_folded = view
                        .file_of(ix)
                        .is_some_and(|f| visible.folded.contains(&f));
                    paint.render(&view, ix, is_folded)
                })
                .collect::<Vec<_>>()
        })
        .track_scroll(&self.changes.scroll)
        .flex_1()
        .min_h_0()
        .on_scroll_wheel(cx.listener(move |this, event: &ScrollWheelEvent, _, cx| {
            // The list scrolls itself vertically; sideways moves the text.
            let delta = event.delta.pixel_delta(row);
            if delta.x.abs() > delta.y.abs() {
                this.shift_changes(-f32::from(delta.x), cx);
            }
        }));
        // A sideways swipe must not scroll the list down.
        list.style().restrict_scroll_to_axis = Some(true);
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .pt(px(LIST_TOP))
            .pb(px(LIST_BOTTOM))
            .font_family(MONO)
            .text_size(metrics.font_size)
            .line_height(metrics.row)
            .child(list)
            .into_any_element()
    }
}

/// The empty state's box, centered in the list area.
fn body_empty(chrome: &Chrome) -> gpui::Div {
    div()
        .flex_1()
        .min_h_0()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .text_size(px(13.))
        .text_color(chrome.term_faint)
}

/// One line per read in the diagnostics file: timings and sizes only, never
/// paths or content.
fn note_timing(path: &std::path::Path, read: Duration, index: Duration, view: &DiffView) {
    let line = format!(
        "read_ms={} index_ms={} files={} rows={} collapsed={}\n",
        read.as_millis(),
        index.as_millis(),
        view.files.len(),
        view.rows.len(),
        view.collapsed.len()
    );
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = file.write_all(line.as_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shika_core::{DiffLine, Hunk};

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
}
