//! Untracked files, read directly and never through a symlink, shown as one
//! added hunk the way `git diff` shows a new file.

use std::ffi::OsStr;
use std::fs;
use std::io::{ErrorKind, Read};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::Path;

use super::budget::{Budget, Limit, collapse};
use super::{DiffLine, FileDiff, FileKey, FileStatus, Hunk, LineKind, lines, lossy};

/// Git calls a file binary when its first 8000 bytes hold a NUL.
const BINARY_PROBE: usize = 8000;

/// What an untracked path holds, read the way `git diff` shows a new file.
pub(super) enum Untracked {
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
    pub(super) fn lines(&self) -> usize {
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
pub(super) fn read_untracked(path: &Path, limit: usize) -> Untracked {
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

pub(super) fn untracked_file(
    worktree: &Path,
    rel: &[u8],
    base: &str,
    budget: &mut Budget,
) -> FileDiff {
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
