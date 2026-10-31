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

mod keys;
mod paint;
mod refresh;
mod rows;
mod text;
mod view;

pub(crate) use rows::*;
pub(crate) use text::*;
pub(crate) use view::*;

use gpui::{Context, FocusHandle, IntoElement, Pixels, Render, UniformListScrollHandle, Window};
use shika_core::{Changes, FileStatus};
use std::{
    collections::HashSet,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

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

#[cfg(test)]
mod tests;
