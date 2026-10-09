//! The Changes panel's data: git's unified patch for a task, parsed into
//! owned, UI-free types, with caps so a huge change stays cheap to hold and
//! to draw.
//!
//! `worktree::session_diff` runs git once for the tracked patch and once for
//! the untracked list. The patch is parsed as it streams in, holding at most
//! one file's body, and only as much of it as the caps let that file show.
//! Untracked files are read directly, never through a symlink. Paths come from the patch headers the way
//! `git apply` reads them: `rename from`/`rename to`, then the `---`/`+++`
//! names, then the `diff --git` line, whose two names are equal for anything
//! that is not a rename. C-quoted names are unquoted, so spaces, tabs,
//! newlines, quotes, non-ASCII, and a `" b/"` inside a name all round-trip.

use std::ffi::OsStr;
use std::fs;
use std::io::{self, BufRead, ErrorKind, Read};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::Path;

use crate::DiffStat;

/// A file with more changed lines than this comes collapsed.
const FILE_CHANGED_LINES: usize = 2_000;
/// A file whose patch, or untracked content, is larger than this comes
/// collapsed, so one minified line cannot cost megabytes.
const FILE_BYTES: usize = 1024 * 1024;
/// Diff lines parsed for a whole task. Files past it come collapsed.
const TOTAL_LINES: usize = 50_000;
/// Bytes parsed for a whole task, the same guard for long lines.
const TOTAL_BYTES: usize = 8 * 1024 * 1024;
/// The hard limit for one expanded file. Lines past it stay hidden.
const EXPAND_LINES: usize = 100_000;
const EXPAND_BYTES: usize = 16 * 1024 * 1024;
/// Git calls a file binary when its first 8000 bytes hold a NUL.
const BINARY_PROBE: usize = 8000;

/// A task's change for the Changes panel: the tracked files in git's order
/// (by path), then the untracked files, and the totals. `stat` counts exactly what `Core::session_diff_stat` counts for the
/// same tree, collapsed files included.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionDiff {
    pub files: Vec<FileDiff>,
    pub stat: DiffStat,
}

/// How a file changed against the task's base.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum FileStatus {
    /// New in the task's commits or index.
    Added,
    #[default]
    Modified,
    Deleted,
    /// Moved, with or without edits. `FileDiff::old_path` holds the old name.
    Renamed,
    /// In the worktree and not ignored, but unknown to git. Shown as all
    /// added.
    Untracked,
}

impl FileStatus {
    /// The one-letter tag the panel shows: A, M, D, R, or U.
    pub fn letter(self) -> char {
        match self {
            FileStatus::Added => 'A',
            FileStatus::Modified => 'M',
            FileStatus::Deleted => 'D',
            FileStatus::Renamed => 'R',
            FileStatus::Untracked => 'U',
        }
    }
}

/// A file's mode change, such as `100644` to `100755`, as git prints it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModeChange {
    pub old: String,
    pub new: String,
}

/// Why a file came collapsed. The panel words each one differently, so a
/// small file past the task's budget does not read as a large one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Collapse {
    /// More than 2,000 changed lines in the file.
    Lines,
    /// At most 2,000 changed lines, but more than 1 MiB of patch for a
    /// tracked file, or of content for an untracked one: that size in bytes.
    /// An untracked file this large is counted as it streams, never held.
    Size(u64),
    /// Small enough on its own, but after the task's total budget (about
    /// 50,000 lines or 8 MiB) ran out. Every later file is collapsed too.
    Budget,
}

/// One file of a task's change.
///
/// A file changed only in mode, a binary file, an empty file, and a rename
/// without edits all have no hunks and are not collapsed. A typechange, such
/// as a file replaced by a symlink, is two entries with the same path: the
/// deletion, then the addition, as `git diff` prints it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileDiff {
    /// The new path, or the old one for a deletion, relative to the worktree
    /// root. Invalid UTF-8 is replaced; [`FileDiff::key`] keeps the bytes.
    pub path: String,
    /// The old path of a rename.
    pub old_path: Option<String>,
    pub status: FileStatus,
    /// Git found binary content. A binary file has no hunks.
    pub binary: bool,
    pub mode_change: Option<ModeChange>,
    /// Added and removed lines for the whole file, counted even when it is
    /// collapsed. Binary files count none, as in the card's diff stat.
    pub insertions: usize,
    pub deletions: usize,
    /// Parsed hunks, in order. Empty when collapsed.
    pub hunks: Vec<Hunk>,
    /// Set when the file has lines to show but none were parsed because of a
    /// cap, with the cap that applied. The panel shows one collapsed row and
    /// offers expand through `Core::session_file_diff`, whatever the reason.
    pub collapsed: Option<Collapse>,
    /// Context, added, and removed lines not parsed because of a cap. All of
    /// the file's lines when collapsed; the rest past the hard limit after an
    /// expand, which keeps the hunks parsed before it.
    pub hidden_lines: usize,
    /// What `Core::session_file_diff` needs to read this file again in full,
    /// against the same base.
    pub key: FileKey,
}

/// Identifies a file of a [`SessionDiff`] for `Core::session_file_diff`: its
/// exact path bytes, the old path of a rename, its status, and the base commit
/// the diff was taken against. Opaque; small to clone onto a background task.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct FileKey {
    pub(crate) base: String,
    pub(crate) path: Vec<u8>,
    pub(crate) old_path: Option<Vec<u8>>,
    pub(crate) status: FileStatus,
}

/// One `@@` hunk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    /// The whole header line, such as `@@ -1,3 +1,4 @@ fn main() {`.
    pub header: String,
    pub lines: Vec<DiffLine>,
}

/// One line of a hunk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: LineKind,
    /// The line number on the base side, for context and removed lines.
    pub old: Option<u32>,
    /// The line number in the worktree, for context and added lines.
    pub new: Option<u32>,
    /// The line without its `+`, `-`, or space prefix and without its `\n`.
    /// It is raw file content: it can hold tabs, the `\r` of a CRLF line, and
    /// other control characters, which the UI must draw safely. Invalid UTF-8
    /// is replaced. A `NoNewline` line holds git's whole marker,
    /// `\ No newline at end of file`.
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LineKind {
    Context,
    Added,
    Removed,
    /// Git's marker after a line that has no newline at the end of the file.
    /// It belongs to the line before it and has no line numbers.
    NoNewline,
}

/// Build the task's diff from a `git diff` patch stream and the untracked
/// paths from `git ls-files --others -z`, read relative to `worktree`.
///
/// The patch is read one line at a time and at most one file's body is held,
/// up to what the caps let that file show. A file over a cap is only counted,
/// so a 50 MB generated file costs a line count, not 50 MB.
pub(crate) fn session_diff(
    patch: impl BufRead,
    untracked: &[&[u8]],
    worktree: &Path,
    base: &str,
) -> io::Result<SessionDiff> {
    session_diff_with(patch, untracked, worktree, base, Budget::session())
}

fn session_diff_with(
    patch: impl BufRead,
    untracked: &[&[u8]],
    worktree: &Path,
    base: &str,
    mut budget: Budget,
) -> io::Result<SessionDiff> {
    let mut stat = DiffStat::default();
    let mut files: Vec<FileDiff> = Vec::new();
    read_patch(patch, &mut budget, |section, budget| {
        // `--numstat` counts a typechange once; the patch prints it as a
        // deletion followed by an addition of the same path.
        let typechange = section.status == FileStatus::Added
            && files.last().is_some_and(|last| {
                last.status == FileStatus::Deleted && last.key.path == section.path
            });
        stat.files += usize::from(!typechange);
        stat.insertions += section.insertions;
        stat.deletions += section.deletions;
        files.push(section.file(base, budget));
    })?;
    // Untracked files follow the tracked ones, as in `git status`.
    for path in untracked {
        let file = untracked_file(worktree, path, base, &mut budget);
        stat.files += 1;
        stat.insertions += file.insertions;
        files.push(file);
    }
    Ok(SessionDiff { files, stat })
}

/// One tracked file of a patch read again for `key`, up to the expand limit.
/// None when the file no longer differs.
pub(crate) fn expand_tracked(patch: impl BufRead, key: &FileKey) -> io::Result<Option<FileDiff>> {
    // A typechange gives two sections for the path; the status picks one.
    let (mut exact, mut same_path) = (None, None);
    read_patch(patch, &mut Budget::expand(), |section, _| {
        if section.path != key.path || exact.is_some() {
            return;
        }
        let file = section.file(&key.base, &mut Budget::expand());
        if section.status == key.status {
            exact = Some(file);
        } else if same_path.is_none() {
            same_path = Some(file);
        }
    })?;
    Ok(exact.or(same_path))
}

/// One untracked file read again for `key`, up to the expand limit. None
/// when it is gone.
pub(crate) fn expand_untracked(worktree: &Path, key: &FileKey) -> Option<FileDiff> {
    let full = worktree.join(OsStr::from_bytes(&key.path));
    fs::symlink_metadata(&full).ok()?;
    Some(untracked_file(
        worktree,
        &key.path,
        &key.base,
        &mut Budget::expand(),
    ))
}

/// Lines in an untracked file, counted as `git diff` would count them for a
/// new file. Binary, unreadable, and non-file entries count none; a symlink is
/// one line, its target. Streams the file, so a large one costs no memory.
pub(crate) fn untracked_lines(path: &Path) -> usize {
    read_untracked(path, 0).lines()
}

/// How many lines and bytes may still be parsed, and the per-file caps.
struct Budget {
    lines: usize,
    bytes: usize,
    file_changed: usize,
    file_bytes: usize,
    /// Once a file did not fit, every later file is collapsed too, so the cap
    /// cuts the list at one place instead of skipping around.
    exhausted: bool,
    /// Parse a file that does not fit up to the limit, instead of not at all.
    /// Expand uses this; the task view collapses whole files.
    partial: bool,
}

impl Budget {
    fn session() -> Self {
        Self {
            lines: TOTAL_LINES,
            bytes: TOTAL_BYTES,
            file_changed: FILE_CHANGED_LINES,
            file_bytes: FILE_BYTES,
            exhausted: false,
            partial: false,
        }
    }

    fn expand() -> Self {
        Self {
            lines: EXPAND_LINES,
            bytes: EXPAND_BYTES,
            file_changed: usize::MAX,
            file_bytes: usize::MAX,
            exhausted: false,
            partial: true,
        }
    }

    /// How many lines and bytes of a file to parse: everything, a prefix
    /// when partial, or nothing, with the cap that cut it. A file over its
    /// own cap is named for that cap even past the total budget, and does
    /// not use the budget up.
    fn admit(&mut self, changed: usize, lines: usize, bytes: usize) -> (Limit, Option<Collapse>) {
        if lines == 0 {
            return (Limit::All, None);
        }
        // Too many lines reads best as a line count; size names the file
        // whose few lines are long, such as a minified bundle.
        if changed > self.file_changed {
            return (Limit::NONE, Some(Collapse::Lines));
        }
        if bytes > self.file_bytes {
            let size = u64::try_from(bytes).unwrap_or(u64::MAX);
            return (Limit::NONE, Some(Collapse::Size(size)));
        }
        if self.exhausted {
            return (Limit::NONE, Some(Collapse::Budget));
        }
        if lines <= self.lines && bytes <= self.bytes {
            self.lines -= lines;
            self.bytes -= bytes;
            return (Limit::All, None);
        }
        self.exhausted = true;
        let limit = if self.partial {
            Limit::Upto {
                lines: self.lines,
                bytes: self.bytes,
            }
        } else {
            Limit::NONE
        };
        (limit, Some(Collapse::Budget))
    }

    /// The most of one file worth holding in memory: what `admit` could let
    /// it show. Anything larger is collapsed, or cut when partial.
    fn read_limit(&self) -> usize {
        if self.exhausted {
            0
        } else {
            self.file_bytes.min(self.bytes)
        }
    }
}

#[derive(Clone, Copy)]
enum Limit {
    All,
    Upto { lines: usize, bytes: usize },
}

impl Limit {
    const NONE: Limit = Limit::Upto { lines: 0, bytes: 0 };

    fn lines(self) -> usize {
        match self {
            Limit::All => usize::MAX,
            Limit::Upto { lines, .. } => lines,
        }
    }

    fn bytes(self) -> usize {
        match self {
            Limit::All => usize::MAX,
            Limit::Upto { bytes, .. } => bytes,
        }
    }
}

/// One file's part of the patch: its header read, its body counted, and as
/// much of the body held as the budget allowed when it started.
struct Section {
    path: Vec<u8>,
    old_path: Option<Vec<u8>>,
    status: FileStatus,
    binary: bool,
    mode_change: Option<ModeChange>,
    /// The held body, whole lines from the first `@@` line on. Empty without
    /// hunks or when the body was over the read limit; a prefix when partial.
    body: Vec<u8>,
    /// The whole body's size, held or not.
    body_bytes: usize,
    insertions: usize,
    deletions: usize,
    /// Context, added, and removed lines in the whole body.
    lines: usize,
}

impl Section {
    fn file(&self, base: &str, budget: &mut Budget) -> FileDiff {
        let (limit, cap) = budget.admit(
            self.insertions + self.deletions,
            self.lines,
            self.body_bytes,
        );
        let (hunks, parsed) = parse_hunks(&self.body, limit);
        FileDiff {
            path: lossy(&self.path),
            old_path: self.old_path.as_deref().map(lossy),
            status: self.status,
            binary: self.binary,
            mode_change: self.mode_change.clone(),
            insertions: self.insertions,
            deletions: self.deletions,
            hunks,
            collapsed: collapse(parsed, self.lines, cap),
            hidden_lines: self.lines - parsed,
            key: FileKey {
                base: base.to_string(),
                path: self.path.clone(),
                old_path: self.old_path.clone(),
                status: self.status,
            },
        }
    }
}

/// The reason a file with `lines` to show is collapsed, when none of them
/// were parsed.
fn collapse(parsed: usize, lines: usize, cap: Option<Collapse>) -> Option<Collapse> {
    (parsed == 0 && lines > 0).then(|| cap.unwrap_or(Collapse::Budget))
}

/// Every line is held up to this much, enough for any header line, which
/// holds a quoted path or two.
const HEADER_LINE: usize = 64 * 1024;

/// Read a patch, handing each file's section to `each` as soon as it ends.
/// A line that starts with `diff --git ` always starts a new file: a hunk line
/// starts with a space, `+`, `-`, or `\`.
fn read_patch(
    mut patch: impl BufRead,
    budget: &mut Budget,
    mut each: impl FnMut(Section, &mut Budget),
) -> io::Result<()> {
    let mut line = Vec::new();
    let mut current: Option<SectionReader> = None;
    loop {
        // Keep enough of a line to classify it and, while it still fits the
        // file's read limit, all of it. A longer line is counted, not held.
        let keep = current
            .as_ref()
            .map_or(0, SectionReader::room)
            .max(HEADER_LINE);
        let Some(len) = read_line(&mut patch, &mut line, keep)? else {
            break;
        };
        if line.starts_with(b"diff --git ") {
            if let Some(done) = current.take() {
                each(done.finish(), budget);
            }
            current = Some(SectionReader::new(&line, budget));
        } else if let Some(reader) = current.as_mut() {
            reader.push(&line, len);
        }
    }
    if let Some(done) = current.take() {
        each(done.finish(), budget);
    }
    Ok(())
}

/// Read one line into `buf` without its newline, keeping at most `keep`
/// bytes of it. Returns the line's whole length, or None at the end.
fn read_line(
    patch: &mut impl BufRead,
    buf: &mut Vec<u8>,
    keep: usize,
) -> io::Result<Option<usize>> {
    buf.clear();
    let mut len = 0;
    let mut started = false;
    loop {
        let available = match patch.fill_buf() {
            Ok(available) => available,
            Err(err) if err.kind() == ErrorKind::Interrupted => continue,
            Err(err) => return Err(err),
        };
        if available.is_empty() {
            return Ok(started.then_some(len));
        }
        started = true;
        let newline = available.iter().position(|byte| *byte == b'\n');
        let chunk = &available[..newline.unwrap_or(available.len())];
        let room = keep.saturating_sub(buf.len());
        buf.extend_from_slice(&chunk[..chunk.len().min(room)]);
        len += chunk.len();
        let used = newline.map_or(available.len(), |at| at + 1);
        patch.consume(used);
        if newline.is_some() {
            return Ok(Some(len));
        }
    }
}

/// A section being read.
struct SectionReader {
    header: Vec<Vec<u8>>,
    in_body: bool,
    body: Vec<u8>,
    body_bytes: usize,
    read_limit: usize,
    partial: bool,
    over: bool,
    insertions: usize,
    deletions: usize,
    lines: usize,
}

impl SectionReader {
    fn new(first: &[u8], budget: &Budget) -> Self {
        Self {
            header: vec![first.to_vec()],
            in_body: false,
            body: Vec::new(),
            body_bytes: 0,
            read_limit: budget.read_limit(),
            partial: budget.partial,
            over: false,
            insertions: 0,
            deletions: 0,
            lines: 0,
        }
    }

    /// How much more of the body may be held.
    fn room(&self) -> usize {
        if self.over {
            0
        } else {
            self.read_limit.saturating_sub(self.body.len())
        }
    }

    /// Take one line, of which `line` holds the first bytes and `len` is the
    /// whole length.
    fn push(&mut self, line: &[u8], len: usize) {
        if !self.in_body {
            if !line.starts_with(b"@@") {
                self.header.push(line.to_vec());
                return;
            }
            self.in_body = true;
        }
        match line.first() {
            Some(b'@' | b'\\') => {}
            first => {
                self.lines += 1;
                match first {
                    Some(b'+') => self.insertions += 1,
                    Some(b'-') => self.deletions += 1,
                    _ => {}
                }
            }
        }
        self.body_bytes += len + 1;
        if self.over {
            return;
        }
        if len == line.len() && self.body.len() + len < self.read_limit {
            self.body.extend_from_slice(line);
            self.body.push(b'\n');
        } else {
            // Over the limit: the task view drops the file's body, an expand
            // keeps the whole lines it already has.
            self.over = true;
            if !self.partial {
                self.body = Vec::new();
            }
        }
    }

    fn finish(self) -> Section {
        let mut header = self.header.iter().map(Vec::as_slice);
        let git_names = header
            .next()
            .and_then(|line| line.strip_prefix(b"diff --git "))
            .unwrap_or_default();
        let mut status = FileStatus::Modified;
        let mut binary = false;
        let (mut old_mode, mut new_mode) = (None, None);
        let (mut rename_from, mut rename_to) = (None, None);
        let (mut minus, mut plus) = (None, None);
        for line in header {
            if let Some(mode) = line.strip_prefix(b"old mode ") {
                old_mode = Some(lossy(mode));
            } else if let Some(mode) = line.strip_prefix(b"new mode ") {
                new_mode = Some(lossy(mode));
            } else if line.starts_with(b"deleted file mode ") {
                status = FileStatus::Deleted;
            } else if line.starts_with(b"new file mode ") {
                status = FileStatus::Added;
            } else if let Some(name) = line.strip_prefix(b"rename from ") {
                rename_from = Some(name_value(name));
            } else if let Some(name) = line.strip_prefix(b"rename to ") {
                rename_to = Some(name_value(name));
            } else if let Some(name) = line.strip_prefix(b"--- ") {
                minus = patch_name(name, b"a/");
            } else if let Some(name) = line.strip_prefix(b"+++ ") {
                plus = patch_name(name, b"b/");
            } else if line.starts_with(b"Binary files ") || line == b"GIT binary patch" {
                binary = true;
            }
        }
        let (path, old_path) = match (rename_from, rename_to) {
            (Some(from), Some(to)) => {
                status = FileStatus::Renamed;
                (to, Some(from))
            }
            // `/dev/null` is None, so a deletion takes the old name.
            _ => match plus.or(minus).or_else(|| header_name(git_names)) {
                Some(path) => (path, None),
                // Not a header git writes; show it rather than drop the file.
                None => (git_names.to_vec(), None),
            },
        };
        let mode_change = match (old_mode, new_mode) {
            (Some(old), Some(new)) => Some(ModeChange { old, new }),
            _ => None,
        };
        Section {
            path,
            old_path,
            status,
            binary,
            mode_change,
            body: self.body,
            body_bytes: self.body_bytes,
            insertions: self.insertions,
            deletions: self.deletions,
            lines: self.lines,
        }
    }
}

/// Parse hunks up to `limit`. Returns them and the lines parsed. A
/// `NoNewline` marker right after the last parsed line is kept with it.
fn parse_hunks(body: &[u8], limit: Limit) -> (Vec<Hunk>, usize) {
    let (max_lines, max_bytes) = (limit.lines(), limit.bytes());
    let mut hunks: Vec<Hunk> = Vec::new();
    let (mut old, mut new) = (0u32, 0u32);
    let (mut parsed, mut bytes) = (0, 0);
    if max_lines == 0 {
        return (hunks, 0);
    }
    for line in lines(body) {
        let kind = match line.first() {
            Some(b'@') => {
                if parsed == max_lines {
                    break;
                }
                (old, new) = hunk_start(line);
                hunks.push(Hunk {
                    header: lossy(line),
                    lines: Vec::new(),
                });
                continue;
            }
            Some(b'+') => LineKind::Added,
            Some(b'-') => LineKind::Removed,
            Some(b'\\') => LineKind::NoNewline,
            // A blank line is an empty context line under
            // `diff.suppressBlankEmpty`.
            _ => LineKind::Context,
        };
        let Some(hunk) = hunks.last_mut() else {
            continue;
        };
        let text = match kind {
            LineKind::NoNewline => line,
            _ => line.get(1..).unwrap_or_default(),
        };
        if kind != LineKind::NoNewline {
            if parsed == max_lines || bytes + text.len() > max_bytes {
                break;
            }
            parsed += 1;
            bytes += text.len();
        }
        let (old_no, new_no) = match kind {
            LineKind::Context => (Some(old), Some(new)),
            LineKind::Removed => (Some(old), None),
            LineKind::Added => (None, Some(new)),
            LineKind::NoNewline => (None, None),
        };
        match kind {
            LineKind::Context => (old, new) = (old + 1, new + 1),
            LineKind::Removed => old += 1,
            LineKind::Added => new += 1,
            LineKind::NoNewline => {}
        }
        hunk.lines.push(DiffLine {
            kind,
            old: old_no,
            new: new_no,
            text: lossy(text),
        });
    }
    // A header whose first line did not fit.
    if hunks.last().is_some_and(|hunk| hunk.lines.is_empty()) {
        hunks.pop();
    }
    (hunks, parsed)
}

/// The first old and new line numbers of `@@ -a[,b] +c[,d] @@`.
fn hunk_start(line: &[u8]) -> (u32, u32) {
    let number = |marker: u8| -> u32 {
        let Some(at) = line.iter().position(|byte| *byte == marker) else {
            return 0;
        };
        line[at + 1..]
            .iter()
            .take_while(|byte| byte.is_ascii_digit())
            .fold(0u32, |n, digit| {
                n.saturating_mul(10).saturating_add(u32::from(digit - b'0'))
            })
    };
    (number(b'-'), number(b'+'))
}

/// What an untracked path holds, read the way `git diff` shows a new file.
enum Untracked {
    /// The whole content, read because it fits the limit.
    Text(Vec<u8>),
    /// Text too large to read; its line count, streamed.
    Large {
        lines: usize,
        size: u64,
    },
    Binary,
    /// The link's own target. A symlink is never followed.
    Symlink(Vec<u8>),
    /// A directory (a nested repository), or unreadable.
    Other,
}

impl Untracked {
    fn lines(&self) -> usize {
        match self {
            Untracked::Text(content) => count_lines(content),
            Untracked::Large { lines, .. } => *lines,
            Untracked::Symlink(_) => 1,
            Untracked::Binary | Untracked::Other => 0,
        }
    }
}

/// Read an untracked path, holding its content only when it is at most
/// `limit` bytes.
fn read_untracked(path: &Path, limit: usize) -> Untracked {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return Untracked::Other;
    };
    if meta.file_type().is_symlink() {
        return match fs::read_link(path) {
            Ok(target) => Untracked::Symlink(target.into_os_string().into_vec()),
            Err(_) => Untracked::Other,
        };
    }
    if !meta.is_file() {
        return Untracked::Other;
    }
    let size = meta.len();
    if size <= limit as u64 {
        return match fs::read(path) {
            Ok(content) if is_binary(&content) => Untracked::Binary,
            Ok(content) => Untracked::Text(content),
            Err(_) => Untracked::Other,
        };
    }
    let Ok(mut file) = fs::File::open(path) else {
        return Untracked::Other;
    };
    let mut buf = vec![0; 64 * 1024];
    let mut probed = 0;
    let mut lines = 0;
    let mut last = b'\n';
    loop {
        let n = match file.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(err) if err.kind() == ErrorKind::Interrupted => continue,
            Err(_) => return Untracked::Other,
        };
        let chunk = &buf[..n];
        if probed < BINARY_PROBE {
            let take = (BINARY_PROBE - probed).min(n);
            if chunk[..take].contains(&0) {
                return Untracked::Binary;
            }
            probed += take;
        }
        lines += chunk.iter().filter(|byte| **byte == b'\n').count();
        last = chunk[n - 1];
    }
    // A last line without a newline still counts.
    Untracked::Large {
        lines: lines + usize::from(last != b'\n'),
        size,
    }
}

fn untracked_file(worktree: &Path, rel: &[u8], base: &str, budget: &mut Budget) -> FileDiff {
    let full = worktree.join(OsStr::from_bytes(rel));
    let content = read_untracked(&full, budget.read_limit());
    let lines = content.lines();
    let mut file = FileDiff {
        path: lossy(rel),
        status: FileStatus::Untracked,
        binary: matches!(content, Untracked::Binary),
        insertions: lines,
        key: FileKey {
            base: base.to_string(),
            path: rel.to_vec(),
            old_path: None,
            status: FileStatus::Untracked,
        },
        ..FileDiff::default()
    };
    let bytes = match &content {
        Untracked::Text(bytes) | Untracked::Symlink(bytes) => bytes.len(),
        Untracked::Large { size, .. } => usize::try_from(*size).unwrap_or(usize::MAX),
        Untracked::Binary | Untracked::Other => 0,
    };
    let (limit, cap) = budget.admit(lines, lines, bytes);
    let (hunk, parsed) = match &content {
        Untracked::Text(bytes) | Untracked::Symlink(bytes) => added_hunk(bytes, lines, limit, true),
        Untracked::Large { .. } if limit.lines() > 0 => {
            // Only an expand reads part of a file this large.
            let mut prefix = Vec::new();
            let read = fs::File::open(&full)
                .and_then(|f| f.take(limit.bytes() as u64).read_to_end(&mut prefix));
            match read {
                Ok(_) => added_hunk(&prefix, lines, limit, false),
                Err(_) => (None, 0),
            }
        }
        _ => (None, 0),
    };
    file.hunks.extend(hunk);
    file.collapsed = collapse(parsed, lines, cap);
    file.hidden_lines = lines - parsed;
    file
}

/// The one hunk git prints for a new file of `total` lines, from `content`,
/// up to `limit`. `complete` is false when `content` is a prefix whose last
/// piece may be a cut line.
fn added_hunk(content: &[u8], total: usize, limit: Limit, complete: bool) -> (Option<Hunk>, usize) {
    let (max_lines, max_bytes) = (limit.lines(), limit.bytes());
    if total == 0 || max_lines == 0 {
        return (None, 0);
    }
    let mut pieces: Vec<&[u8]> = lines(content).collect();
    let ends_open = content.last() != Some(&b'\n');
    if !complete && ends_open {
        pieces.pop();
    }
    let header = if total == 1 {
        "@@ -0,0 +1 @@".to_string()
    } else {
        format!("@@ -0,0 +1,{total} @@")
    };
    let mut hunk = Hunk {
        header,
        lines: Vec::new(),
    };
    let mut bytes = 0;
    for (i, piece) in pieces.iter().enumerate() {
        if i == max_lines || bytes + piece.len() > max_bytes {
            break;
        }
        bytes += piece.len();
        hunk.lines.push(DiffLine {
            kind: LineKind::Added,
            old: None,
            new: Some(u32::try_from(i + 1).unwrap_or(u32::MAX)),
            text: lossy(piece),
        });
    }
    let parsed = hunk.lines.len();
    if parsed == 0 {
        return (None, 0);
    }
    if complete && ends_open && parsed == total {
        hunk.lines.push(DiffLine {
            kind: LineKind::NoNewline,
            old: None,
            new: None,
            text: "\\ No newline at end of file".into(),
        });
    }
    (Some(hunk), parsed)
}

fn is_binary(content: &[u8]) -> bool {
    content[..content.len().min(BINARY_PROBE)].contains(&0)
}

/// Lines as `git diff` counts them for a new file: a last line without a
/// newline still counts.
fn count_lines(content: &[u8]) -> usize {
    let newlines = content.iter().filter(|byte| **byte == b'\n').count();
    newlines + usize::from(content.last().is_some_and(|byte| *byte != b'\n'))
}

/// Lines without their `\n`. A trailing newline does not make an empty last
/// line.
fn lines(bytes: &[u8]) -> impl Iterator<Item = &[u8]> {
    let trimmed = bytes.strip_suffix(b"\n").unwrap_or(bytes);
    trimmed
        .split(|byte| *byte == b'\n')
        .take(if bytes.is_empty() { 0 } else { usize::MAX })
}

fn lossy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// The value of `rename from`, quoted or not.
fn name_value(value: &[u8]) -> Vec<u8> {
    match unquote(value) {
        Some((name, _)) => name,
        None => value.to_vec(),
    }
}

/// The path of a `---` or `+++` line without its prefix. None for
/// `/dev/null`. Git ends an unquoted name that holds a space with a tab.
fn patch_name(value: &[u8], prefix: &[u8]) -> Option<Vec<u8>> {
    if value == b"/dev/null" {
        return None;
    }
    let name = match unquote(value) {
        Some((name, _)) => name,
        None => value.strip_suffix(b"\t").unwrap_or(value).to_vec(),
    };
    name.strip_prefix(prefix).map(<[u8]>::to_vec)
}

/// The path of a `diff --git a/P b/P` line whose two names are the same,
/// which is every file that is not a rename. Unquoted, the length decides
/// where the second name starts, so a `" b/"` inside the path is safe.
fn header_name(names: &[u8]) -> Option<Vec<u8>> {
    if names.first() == Some(&b'"') {
        let (old, rest) = unquote(names)?;
        let rest = rest.strip_prefix(b" ")?;
        let new = match unquote(rest) {
            Some((new, _)) => new,
            None => rest.to_vec(),
        };
        let old = old.strip_prefix(b"a/")?;
        let new = new.strip_prefix(b"b/")?;
        return (old == new).then(|| new.to_vec());
    }
    let len = names.len().checked_sub(5)?;
    if len % 2 != 0 {
        return None;
    }
    let half = len / 2;
    let old = names.strip_prefix(b"a/")?.get(..half)?;
    let new = names.get(half + 2..)?.strip_prefix(b" b/")?;
    (old == new).then(|| new.to_vec())
}

/// Read a C-quoted name as git writes it, returning its bytes and what
/// follows the closing quote. None when `value` is not quoted.
fn unquote(value: &[u8]) -> Option<(Vec<u8>, &[u8])> {
    let mut rest = value.strip_prefix(b"\"")?;
    let mut name = Vec::with_capacity(rest.len());
    loop {
        let (&byte, tail) = rest.split_first()?;
        rest = tail;
        match byte {
            b'"' => return Some((name, rest)),
            b'\\' => {
                let (&escape, tail) = rest.split_first()?;
                rest = tail;
                name.push(match escape {
                    b'a' => 0x07,
                    b'b' => 0x08,
                    b't' => b'\t',
                    b'n' => b'\n',
                    b'v' => 0x0b,
                    b'f' => 0x0c,
                    b'r' => b'\r',
                    b'0'..=b'3' => {
                        let digits = rest.get(..2)?;
                        if !digits.iter().all(|d| (b'0'..=b'7').contains(d)) {
                            return None;
                        }
                        rest = &rest[2..];
                        ((escape - b'0') << 6) | ((digits[0] - b'0') << 3) | (digits[1] - b'0')
                    }
                    other => other,
                });
            }
            other => name.push(other),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(hunk: &Hunk) -> Vec<LineKind> {
        hunk.lines.iter().map(|line| line.kind).collect()
    }

    #[test]
    fn quoted_names_unquote_every_escape() {
        let (name, rest) = unquote(br#""a/q\"t\\x\t\n\303\274.txt" tail"#).unwrap();
        assert_eq!(name, "a/q\"t\\x\t\nü.txt".as_bytes());
        assert_eq!(rest, b" tail");
        assert_eq!(unquote(b"plain"), None);
        assert_eq!(unquote(br#""open"#), None);
    }

    #[test]
    fn header_names_split_by_length_and_quotes() {
        assert_eq!(
            header_name(b"a/x b/y b/x b/y").as_deref(),
            Some(&b"x b/y"[..])
        );
        assert_eq!(
            header_name(b"a/sp ace b/sp ace").as_deref(),
            Some(&b"sp ace"[..])
        );
        assert_eq!(
            header_name(br#""a/\303\274" "b/\303\274""#).as_deref(),
            Some("ü".as_bytes())
        );
        // A rename's header has two different names; the parser uses its
        // `rename from`/`rename to` lines instead.
        assert_eq!(header_name(b"a/old b/new"), None);
        assert_eq!(header_name(b"a/x"), None);
    }

    #[test]
    fn patch_names_drop_the_tab_and_the_prefix() {
        assert_eq!(
            patch_name(b"a/sp ace.txt\t", b"a/").as_deref(),
            Some(&b"sp ace.txt"[..])
        );
        assert_eq!(patch_name(b"/dev/null", b"a/"), None);
        assert_eq!(
            patch_name(br#""b/q\"x""#, b"b/").as_deref(),
            Some(&b"q\"x"[..])
        );
    }

    #[test]
    fn hunks_number_lines_and_keep_markers() {
        let patch = b"diff --git a/f b/f\nindex 1..2 100644\n--- a/f\n+++ b/f\n\
@@ -1,3 +1,3 @@ fn x\n a\n-b\n+B\n\n@@ -10 +10 @@\n-y\n\\ No newline at end of file\n+y\n";
        let diff = session_diff(&patch[..], &[], Path::new("/nonexistent"), "base").unwrap();
        let file = &diff.files[0];
        assert_eq!(file.path, "f");
        assert_eq!((file.insertions, file.deletions), (2, 2));
        assert_eq!(file.hunks.len(), 2);
        assert_eq!(file.hunks[0].header, "@@ -1,3 +1,3 @@ fn x");
        assert_eq!(
            kinds(&file.hunks[0]),
            [
                LineKind::Context,
                LineKind::Removed,
                LineKind::Added,
                LineKind::Context
            ]
        );
        // A blank line is an empty context line (`diff.suppressBlankEmpty`).
        let blank = &file.hunks[0].lines[3];
        assert_eq!(
            (blank.old, blank.new, blank.text.as_str()),
            (Some(3), Some(3), "")
        );
        let second = &file.hunks[1].lines;
        assert_eq!((second[0].old, second[0].new), (Some(10), None));
        assert_eq!(second[1].kind, LineKind::NoNewline);
        assert_eq!(second[1].text, "\\ No newline at end of file");
        assert_eq!((second[2].old, second[2].new), (None, Some(10)));
        assert_eq!(
            diff.stat,
            DiffStat {
                files: 1,
                insertions: 2,
                deletions: 2
            }
        );
    }

    #[test]
    fn a_limit_keeps_a_prefix_and_counts_the_rest() {
        let body = b"@@ -1,2 +1,2 @@\n-a\n+b\n@@ -9 +9 @@\n-c\n+d\n";
        let (hunks, parsed) = parse_hunks(
            body,
            Limit::Upto {
                lines: 3,
                bytes: usize::MAX,
            },
        );
        assert_eq!(parsed, 3);
        assert_eq!(hunks.len(), 2);
        assert_eq!(kinds(&hunks[1]), [LineKind::Removed]);
        // A hunk header whose first line does not fit is dropped.
        let (hunks, parsed) = parse_hunks(
            body,
            Limit::Upto {
                lines: 2,
                bytes: usize::MAX,
            },
        );
        assert_eq!((hunks.len(), parsed), (1, 2));
    }

    #[test]
    fn the_total_cap_collapses_every_later_file() {
        let mut patch = Vec::new();
        for name in ["a", "b", "c"] {
            patch.extend_from_slice(
                format!("diff --git a/{name} b/{name}\n--- a/{name}\n+++ b/{name}\n@@ -1,2 +1,2 @@\n-x\n+y\n")
                    .as_bytes(),
            );
        }
        let mut budget = Budget::session();
        budget.lines = 3;
        let diff =
            session_diff_with(&patch[..], &[], Path::new("/nonexistent"), "base", budget).unwrap();
        let collapsed: Vec<_> = diff.files.iter().map(|f| f.collapsed).collect();
        assert_eq!(
            collapsed,
            [None, Some(Collapse::Budget), Some(Collapse::Budget)]
        );
        assert_eq!(diff.files[1].hidden_lines, 2);
        assert_eq!(
            diff.stat,
            DiffStat {
                files: 3,
                insertions: 3,
                deletions: 3
            }
        );
    }

    #[test]
    fn a_file_over_the_read_limit_is_counted_but_not_held() {
        // One 3 MB line, as a minified bundle would have.
        let long = "x".repeat(3 * 1024 * 1024);
        let patch = format!(
            "diff --git a/min.js b/min.js\n--- a/min.js\n+++ b/min.js\n@@ -1 +1 @@\n-{long}\n+{long}\n\
diff --git a/b b/b\n--- a/b\n+++ b/b\n@@ -1 +1 @@\n-x\n+y\n"
        );
        let mut held = 0;
        read_patch(patch.as_bytes(), &mut Budget::session(), |section, _| {
            held = held.max(section.body.len());
        })
        .unwrap();
        assert!(held < 64, "{held}");
        let diff = session_diff(patch.as_bytes(), &[], Path::new("/nonexistent"), "base").unwrap();
        let min = &diff.files[0];
        // The size is the patch body's: the hunk header and both lines.
        let body = "@@ -1 +1 @@\n".len() + 2 * (long.len() + 2);
        assert_eq!(min.collapsed, Some(Collapse::Size(body as u64)));
        assert_eq!((min.insertions, min.deletions, min.hidden_lines), (1, 1, 2));
        // A file over its own cap does not use up the task's budget.
        assert_eq!(diff.files[1].collapsed, None);
        assert_eq!(diff.files[1].hunks[0].lines.len(), 2);
    }

    #[test]
    fn long_lines_are_read_in_part() {
        let mut line = Vec::new();
        let mut input = &b"abcdef\nxy"[..];
        assert_eq!(read_line(&mut input, &mut line, 3).unwrap(), Some(6));
        assert_eq!(line, b"abc");
        assert_eq!(read_line(&mut input, &mut line, 3).unwrap(), Some(2));
        assert_eq!(line, b"xy");
        assert_eq!(read_line(&mut input, &mut line, 3).unwrap(), None);
    }
}
