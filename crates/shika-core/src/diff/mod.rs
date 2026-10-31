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

mod budget;
mod patch;
mod render;
mod untracked;

use std::ffi::OsStr;
use std::fs;
use std::io::{self, BufRead};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use crate::DiffStat;
use budget::Budget;
use patch::read_patch;
pub use render::{RENDER_CAP_BYTES, render_unified};
use untracked::{read_untracked, untracked_file};

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

#[cfg(test)]
mod tests;
