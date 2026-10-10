//! The terminal handle shared by the PTY reader thread and the UI. The
//! reader feeds bytes in; the view takes snapshots out. Neither side waits
//! on the other for long: the engine lock is held only while parsing one
//! read or copying one screen.

use std::io::{self, Read};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Instant;

use parking_lot::MutexGuard;

use crate::engine::Engine;
use crate::theme::Palette;
use crate::types::{
    CellSide, InputSource, Modes, SelectionKind, Snapshot, TerminalSize, ViewportPoint,
};

/// Where the terminal's output goes: bytes for the program's stdin, and grid
/// size changes for the PTY.
///
/// `write` is called on the UI thread (typing, paste, focus and mouse
/// reports) and on the PTY reader thread (answers to terminal queries), with
/// `source` saying which. It must not block: queue the bytes and write them
/// on another thread. [`crate::PtyWriter`] does exactly that.
pub trait PtyHost: Send + Sync + 'static {
    fn write(&self, bytes: &[u8], source: InputSource);
    fn resize(&self, size: TerminalSize);
}

#[derive(Clone, Debug)]
pub struct TerminalOptions {
    /// The size before the view has measured itself.
    pub size: TerminalSize,
    /// Rows of history kept above the screen.
    pub scrollback: usize,
    pub palette: Palette,
}

impl Default for TerminalOptions {
    fn default() -> Self {
        Self {
            size: TerminalSize::default(),
            scrollback: 10_000,
            palette: Palette::default(),
        }
    }
}

/// Things the program asked for that the UI thread has to act on.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Notices {
    /// `Some(None)` means the title was reset.
    pub title: Option<Option<String>>,
    pub bell: bool,
    /// OSC 52 copy request.
    pub clipboard: Option<String>,
}

impl Notices {
    pub fn is_empty(&self) -> bool {
        self.title.is_none() && !self.bell && self.clipboard.is_none()
    }
}

type Waker = Arc<dyn Fn() + Send + Sync>;

struct Shared {
    engine: parking_lot::Mutex<Engine>,
    host: Box<dyn PtyHost>,
    /// The grid changed since the view last took a snapshot.
    dirty: AtomicBool,
    /// Notices are waiting for the UI thread.
    notice_pending: AtomicBool,
    /// A synchronized update is open and the UI has been told to watch its
    /// deadline.
    sync_pending: AtomicBool,
    notices: Mutex<Notices>,
    title: Mutex<Option<String>>,
    waker: Mutex<Option<Waker>>,
}

/// A cheap, cloneable handle to one terminal.
#[derive(Clone)]
pub struct Terminal {
    shared: Arc<Shared>,
}

/// Size of one PTY read. Large enough that a flood is parsed in big
/// batches, small enough that one parse holds the lock for well under a
/// frame.
const READ_CHUNK: usize = 64 * 1024;

impl Terminal {
    pub fn new(options: TerminalOptions, host: impl PtyHost) -> Self {
        let engine = Engine::new(options.size, options.scrollback, options.palette);
        Self {
            shared: Arc::new(Shared {
                engine: parking_lot::Mutex::new(engine),
                host: Box::new(host),
                dirty: AtomicBool::new(true),
                notice_pending: AtomicBool::new(false),
                sync_pending: AtomicBool::new(false),
                notices: Mutex::new(Notices::default()),
                title: Mutex::new(None),
                waker: Mutex::new(None),
            }),
        }
    }

    /// Parse PTY output. Safe to call from any thread, and it always
    /// accepts the bytes, so a hidden terminal keeps draining its PTY.
    pub fn feed(&self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        let mut engine = self.shared.engine.lock();
        let changed = engine.advance(bytes);
        let output = engine.take_output();
        let syncing = engine.sync_deadline().is_some();
        // Hand the lock straight to a waiting UI thread so a flood cannot
        // starve the next snapshot.
        MutexGuard::unlock_fair(engine);

        for reply in &output.replies {
            self.shared.host.write(reply, InputSource::Reply);
        }

        let mut wake = false;
        if output.has_notice() {
            {
                let mut notices = lock(&self.shared.notices);
                if let Some(title) = output.title {
                    *lock(&self.shared.title) = title.clone();
                    notices.title = Some(title);
                }
                notices.bell |= output.bell;
                if output.clipboard.is_some() {
                    notices.clipboard = output.clipboard;
                }
            }
            wake |= !self.shared.notice_pending.swap(true, Ordering::AcqRel);
        }
        if changed {
            wake |= !self.shared.dirty.swap(true, Ordering::AcqRel);
        }
        if syncing {
            wake |= !self.shared.sync_pending.swap(true, Ordering::AcqRel);
        } else {
            self.shared.sync_pending.store(false, Ordering::Release);
        }
        if wake {
            self.wake();
        }
    }

    /// Start a thread that reads the PTY until end of file and feeds every
    /// byte in. `on_end` runs on that thread after the last read.
    pub fn spawn_reader(
        &self,
        mut reader: impl Read + Send + 'static,
        on_end: impl FnOnce() + Send + 'static,
    ) -> io::Result<JoinHandle<()>> {
        let terminal = self.clone();
        thread::Builder::new()
            .name("shika-pty-reader".into())
            .spawn(move || {
                let mut buf = vec![0u8; READ_CHUNK];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => terminal.feed(&buf[..n]),
                        Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                        Err(_) => break,
                    }
                }
                on_end();
            })
    }

    /// Bytes for the program, as if typed.
    pub fn write(&self, bytes: &[u8]) {
        if !bytes.is_empty() {
            self.shared.host.write(bytes, InputSource::Typed);
        }
    }

    /// A focus, mouse, or scroll report for the program. The user did not
    /// type it.
    pub fn report(&self, bytes: &[u8]) {
        if !bytes.is_empty() {
            self.shared.host.write(bytes, InputSource::Report);
        }
    }

    /// Resize the grid and tell the PTY. Returns true when anything changed.
    pub fn resize(&self, size: TerminalSize) -> bool {
        let mut engine = self.shared.engine.lock();
        if engine.size() == size {
            return false;
        }
        let grid_changed = !engine.size().same_grid(&size);
        engine.resize(size);
        drop(engine);
        self.shared.dirty.store(true, Ordering::Release);
        if grid_changed {
            self.shared.host.resize(size);
        }
        true
    }

    pub fn size(&self) -> TerminalSize {
        self.shared.engine.lock().size()
    }

    pub fn modes(&self) -> Modes {
        self.shared.engine.lock().modes()
    }

    /// Copy the visible screen.
    pub fn snapshot(&self) -> Snapshot {
        self.shared.engine.lock().snapshot()
    }

    /// Copy the active live screen as text, even when the view is hidden or
    /// scrolled into history. Returns one string per screen row, with wide
    /// spacers omitted and trailing blanks trimmed, like Snapshot::text_lines.
    /// Does not change the viewport, selection, cursor, dirty state, or wakeups.
    pub fn live_text_lines(&self) -> Vec<String> {
        self.shared.engine.lock().live_text_lines()
    }

    /// The live screen with up to `history` lines of scrollback above it, one
    /// string per row, oldest first. Like [`Terminal::live_text_lines`], it
    /// leaves the viewport, selection, and cursor alone.
    pub fn text_with_history(&self, history: usize) -> Vec<String> {
        self.shared.engine.lock().text_with_history(history)
    }

    /// The title the program set with OSC 0 or 2, if any.
    pub fn title(&self) -> Option<String> {
        lock(&self.shared.title).clone()
    }

    pub fn selection_text(&self) -> Option<String> {
        self.shared.engine.lock().selection_text()
    }

    pub fn set_palette(&self, palette: Palette) {
        self.update(|engine| engine.set_palette(palette));
    }

    /// Positive scrolls back into history.
    pub fn scroll(&self, lines: i32) {
        self.update(|engine| engine.scroll(lines));
    }

    pub fn scroll_page(&self, up: bool) {
        self.update(|engine| engine.scroll_page(up));
    }

    pub fn scroll_to_bottom(&self) {
        self.update(|engine| engine.scroll_to_bottom());
    }

    pub fn display_offset(&self) -> usize {
        self.shared.engine.lock().display_offset()
    }

    pub fn start_selection(&self, kind: SelectionKind, at: ViewportPoint, side: CellSide) {
        self.update(|engine| engine.start_selection(kind, at, side));
    }

    pub fn update_selection(&self, at: ViewportPoint, side: CellSide) {
        self.update(|engine| engine.update_selection(at, side));
    }

    pub fn clear_selection(&self) {
        self.update(|engine| engine.clear_selection());
    }

    /// Install the function that wakes the UI. It is called at most once
    /// until the view takes a snapshot or drains notices, so a busy PTY
    /// cannot flood the UI thread with wakeups.
    pub(crate) fn set_waker(&self, waker: impl Fn() + Send + Sync + 'static) {
        let waker: Waker = Arc::new(waker);
        *lock(&self.shared.waker) = Some(waker.clone());
        waker();
    }

    /// True when the grid changed since the last call. The view calls this
    /// right before taking a snapshot.
    pub(crate) fn take_dirty(&self) -> bool {
        self.shared.dirty.swap(false, Ordering::AcqRel)
    }

    pub(crate) fn take_notices(&self) -> Notices {
        self.shared.notice_pending.store(false, Ordering::Release);
        std::mem::take(&mut *lock(&self.shared.notices))
    }

    pub(crate) fn sync_deadline(&self) -> Option<Instant> {
        self.shared.engine.lock().sync_deadline()
    }

    /// Draw a synchronized update whose end marker never came. Returns the
    /// next deadline when the update is still open and not yet due.
    pub(crate) fn flush_expired_sync(&self) -> Option<Instant> {
        let mut engine = self.shared.engine.lock();
        if engine.flush_sync_if_expired(Instant::now()) {
            drop(engine);
            self.shared.sync_pending.store(false, Ordering::Release);
            self.shared.dirty.store(true, Ordering::Release);
            None
        } else {
            let deadline = engine.sync_deadline();
            if deadline.is_none() {
                self.shared.sync_pending.store(false, Ordering::Release);
            }
            deadline
        }
    }

    fn update<R>(&self, f: impl FnOnce(&mut Engine) -> R) -> R {
        let result = f(&mut self.shared.engine.lock());
        self.shared.dirty.store(true, Ordering::Release);
        result
    }

    fn wake(&self) {
        let waker = lock(&self.shared.waker).clone();
        if let Some(waker) = waker {
            waker();
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|err| err.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[derive(Clone, Default)]
    struct Recorder {
        written: Arc<Mutex<Vec<u8>>>,
        sizes: Arc<Mutex<Vec<TerminalSize>>>,
    }

    impl PtyHost for Recorder {
        fn write(&self, bytes: &[u8], _: InputSource) {
            lock(&self.written).extend_from_slice(bytes);
        }

        fn resize(&self, size: TerminalSize) {
            lock(&self.sizes).push(size);
        }
    }

    fn terminal() -> (Terminal, Recorder) {
        let recorder = Recorder::default();
        let options = TerminalOptions {
            size: TerminalSize::new(5, 20),
            ..TerminalOptions::default()
        };
        (Terminal::new(options, recorder.clone()), recorder)
    }

    #[test]
    fn query_replies_go_back_to_the_pty() {
        let (term, recorder) = terminal();
        term.feed(b"\x1b[c");
        let written = lock(&recorder.written).clone();
        assert!(written.starts_with(b"\x1b[?6"), "{written:?}");
    }

    #[test]
    fn wakeups_are_coalesced_until_the_view_takes_a_snapshot() {
        let (term, _) = terminal();
        let wakes = Arc::new(AtomicUsize::new(0));
        let counter = wakes.clone();
        term.set_waker(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        });
        // Installing the waker wakes once so the view draws what arrived
        // before it existed.
        assert_eq!(wakes.load(Ordering::SeqCst), 1);
        assert!(term.take_dirty());
        for _ in 0..1000 {
            term.feed(b"x");
        }
        assert_eq!(wakes.load(Ordering::SeqCst), 2);
        assert!(term.take_dirty());
        assert!(!term.take_dirty());
        term.feed(b"y");
        assert_eq!(wakes.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn live_sampling_preserves_dirty_state_and_wakeup_coalescing() {
        let (term, recorder) = terminal();
        let wakes = Arc::new(AtomicUsize::new(0));
        let counter = wakes.clone();
        term.set_waker(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        });
        assert!(term.take_dirty());
        term.feed(b"old0\r\nold1\r\nold2\r\nold3\r\nold4\r\nlive");
        term.scroll(1);
        let before = term.snapshot();
        let count = wakes.load(Ordering::SeqCst);
        assert_eq!(before.text_lines()[4], "old4");
        assert_eq!(term.live_text_lines()[4], "live");
        assert!(
            term.take_dirty(),
            "sampling must not consume pending damage"
        );
        assert_eq!(term.live_text_lines()[4], "live");
        assert!(!term.take_dirty(), "sampling must not create damage");
        assert_eq!(term.snapshot(), before);
        assert_eq!(term.display_offset(), 1);
        assert_eq!(wakes.load(Ordering::SeqCst), count);
        assert!(lock(&recorder.written).is_empty());
        assert!(lock(&recorder.sizes).is_empty());
        term.feed(b"!");
        assert_eq!(wakes.load(Ordering::SeqCst), count + 1);
        term.live_text_lines();
        term.feed(b"?");
        assert_eq!(wakes.load(Ordering::SeqCst), count + 1);
        assert!(term.take_dirty());
    }

    #[test]
    fn live_sampling_without_a_view_tracks_output_and_alternate_screen() {
        let (term, _) = terminal();
        // No view or waker is installed, and no dirty state is consumed.
        for i in 0..20 {
            term.feed(format!("line{i}\r\n").as_bytes());
        }
        term.scroll(10);
        assert_eq!(term.snapshot().text_lines()[0], "line6");
        assert_eq!(
            term.live_text_lines(),
            vec!["line16", "line17", "line18", "line19", ""]
        );
        term.feed(b"\x1b[?1049h\x1b[Hhidden tui\x1b[?25l");
        assert_eq!(term.live_text_lines(), vec!["hidden tui", "", "", "", ""]);
        term.feed(b"\x1b[?1049l");
        assert_eq!(term.live_text_lines()[3], "line19");
        assert_eq!(term.display_offset(), 10);
        assert!(term.take_dirty());
    }

    #[test]
    fn titles_and_bells_wake_even_when_the_grid_is_already_dirty() {
        let (term, _) = terminal();
        let wakes = Arc::new(AtomicUsize::new(0));
        let counter = wakes.clone();
        term.set_waker(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        });
        term.feed(b"text");
        let before = wakes.load(Ordering::SeqCst);
        term.feed(b"\x1b]0;agent\x07\x07");
        assert_eq!(wakes.load(Ordering::SeqCst), before + 1);
        let notices = term.take_notices();
        assert_eq!(notices.title, Some(Some("agent".into())));
        assert!(notices.bell);
        assert_eq!(term.title().as_deref(), Some("agent"));
        assert!(term.take_notices().is_empty());
    }

    #[test]
    fn resize_reports_grid_changes_once() {
        let (term, recorder) = terminal();
        assert!(term.resize(TerminalSize::new(10, 40)));
        assert!(!term.resize(TerminalSize::new(10, 40)));
        // A new cell size alone does not resize the PTY grid.
        assert!(term.resize(TerminalSize::new(10, 40).with_cell(8, 19)));
        assert_eq!(*lock(&recorder.sizes), vec![TerminalSize::new(10, 40)]);
    }

    #[test]
    fn the_reader_thread_drains_until_end_of_file() {
        let (term, _) = terminal();
        let (tx, rx) = std::sync::mpsc::channel();
        let data: &'static [u8] = b"one\r\ntwo\r\n";
        term.spawn_reader(data, move || tx.send(()).unwrap())
            .unwrap();
        rx.recv().unwrap();
        assert_eq!(&term.snapshot().text_lines()[..2], ["one", "two"]);
    }

    #[test]
    fn a_large_flood_is_parsed_without_a_view() {
        let (term, _) = terminal();
        let mut data = Vec::new();
        for i in 0..200_000 {
            data.extend_from_slice(format!("{i}\r\n").as_bytes());
        }
        let (tx, rx) = std::sync::mpsc::channel();
        term.spawn_reader(std::io::Cursor::new(data), move || tx.send(()).unwrap())
            .unwrap();
        rx.recv().unwrap();
        let lines = term.snapshot().text_lines();
        assert_eq!(lines[3], "199999");
    }
}
