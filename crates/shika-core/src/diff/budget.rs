//! The caps that keep a huge change cheap: per-file limits, the task's total
//! budget, the larger limit for one expanded file, and why a file collapsed.

use super::Collapse;

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

/// How many lines and bytes may still be parsed, and the per-file caps.
pub(super) struct Budget {
    pub(super) lines: usize,
    bytes: usize,
    file_changed: usize,
    file_bytes: usize,
    /// Once a file did not fit, every later file is collapsed too, so the cap
    /// cuts the list at one place instead of skipping around.
    exhausted: bool,
    /// Parse a file that does not fit up to the limit, instead of not at all.
    /// Expand uses this; the task view collapses whole files.
    pub(super) partial: bool,
}

impl Budget {
    pub(super) fn session() -> Self {
        Self {
            lines: TOTAL_LINES,
            bytes: TOTAL_BYTES,
            file_changed: FILE_CHANGED_LINES,
            file_bytes: FILE_BYTES,
            exhausted: false,
            partial: false,
        }
    }

    pub(super) fn expand() -> Self {
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
    pub(super) fn admit(
        &mut self,
        changed: usize,
        lines: usize,
        bytes: usize,
    ) -> (Limit, Option<Collapse>) {
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
    pub(super) fn read_limit(&self) -> usize {
        if self.exhausted {
            0
        } else {
            self.file_bytes.min(self.bytes)
        }
    }
}

#[derive(Clone, Copy)]
pub(super) enum Limit {
    All,
    Upto { lines: usize, bytes: usize },
}

impl Limit {
    pub(super) const NONE: Limit = Limit::Upto { lines: 0, bytes: 0 };

    pub(super) fn lines(self) -> usize {
        match self {
            Limit::All => usize::MAX,
            Limit::Upto { lines, .. } => lines,
        }
    }

    pub(super) fn bytes(self) -> usize {
        match self {
            Limit::All => usize::MAX,
            Limit::Upto { bytes, .. } => bytes,
        }
    }
}

/// The reason a file with `lines` to show is collapsed, when none of them
/// were parsed.
pub(super) fn collapse(parsed: usize, lines: usize, cap: Option<Collapse>) -> Option<Collapse> {
    (parsed == 0 && lines > 0).then(|| cap.unwrap_or(Collapse::Budget))
}
