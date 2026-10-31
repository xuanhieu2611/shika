//! Reading git's unified patch: one section per file, its header names and
//! status, its body counted and held as far as the read limit allows, and
//! its hunks parsed up to the budget.

use std::io::{self, BufRead, ErrorKind};

use super::budget::{Budget, Limit, collapse};
use super::{DiffLine, FileDiff, FileKey, FileStatus, Hunk, LineKind, ModeChange, lines, lossy};

/// One file's part of the patch: its header read, its body counted, and as
/// much of the body held as the budget allowed when it started.
pub(super) struct Section {
    pub(super) path: Vec<u8>,
    pub(super) old_path: Option<Vec<u8>>,
    pub(super) status: FileStatus,
    pub(super) binary: bool,
    pub(super) mode_change: Option<ModeChange>,
    /// The held body, whole lines from the first `@@` line on. Empty without
    /// hunks or when the body was over the read limit; a prefix when partial.
    pub(super) body: Vec<u8>,
    /// The whole body's size, held or not.
    pub(super) body_bytes: usize,
    pub(super) insertions: usize,
    pub(super) deletions: usize,
    /// Context, added, and removed lines in the whole body.
    pub(super) lines: usize,
}

impl Section {
    pub(super) fn file(&self, base: &str, budget: &mut Budget) -> FileDiff {
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

/// Every line is held up to this much, enough for any header line, which
/// holds a quoted path or two.
const HEADER_LINE: usize = 64 * 1024;

/// Read a patch, handing each file's section to `each` as soon as it ends.
/// A line that starts with `diff --git ` always starts a new file: a hunk line
/// starts with a space, `+`, `-`, or `\`.
pub(super) fn read_patch(
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
pub(super) fn read_line(
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
pub(super) fn parse_hunks(body: &[u8], limit: Limit) -> (Vec<Hunk>, usize) {
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

/// The value of `rename from`, quoted or not.
fn name_value(value: &[u8]) -> Vec<u8> {
    match unquote(value) {
        Some((name, _)) => name,
        None => value.to_vec(),
    }
}

/// The path of a `---` or `+++` line without its prefix. None for
/// `/dev/null`. Git ends an unquoted name that holds a space with a tab.
pub(super) fn patch_name(value: &[u8], prefix: &[u8]) -> Option<Vec<u8>> {
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
pub(super) fn header_name(names: &[u8]) -> Option<Vec<u8>> {
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
pub(super) fn unquote(value: &[u8]) -> Option<(Vec<u8>, &[u8])> {
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
