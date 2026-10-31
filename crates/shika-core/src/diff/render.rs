//! A task's change as plain text for the Lead's `shika diff`, from the same
//! `SessionDiff` the Changes panel shows.

use std::path::Path;

use super::{Collapse, FileDiff, FileStatus, Hunk, LineKind, SessionDiff};

/// The most `shika diff` text a Lead gets. Past it the text ends with a note.
pub const RENDER_CAP_BYTES: usize = 200 * 1024;

/// A file's name in the rendered text: `old -> new` for a rename.
fn rendered_name(file: &FileDiff) -> String {
    match &file.old_path {
        Some(old) => format!("{old} -> {}", file.path),
        None => file.path.clone(),
    }
}

/// Output under a byte cap, filled a line at a time.
struct Capped {
    text: String,
    cap: usize,
    full: bool,
}

impl Capped {
    /// Adds `line` unless it would pass the cap. Once one line did not fit,
    /// nothing more is added.
    fn push(&mut self, line: &str) -> bool {
        if self.full || self.text.len() + line.len() + 1 > self.cap {
            self.full = true;
            return false;
        }
        self.text.push_str(line);
        self.text.push('\n');
        true
    }
}

/// A task's change as plain text for the Lead, from the same [`SessionDiff`]
/// the Changes panel shows. With `stat_only` one line per file and a total;
/// otherwise unified diff text under a header per file (`diff M path (+3 -1)`;
/// a rename reads `old -> new`; an untracked file is shown as added; a binary
/// file is noted, with no body). A file the panel shows collapsed is noted
/// with its size, not expanded. Output stops at a whole line once it would
/// pass `cap` bytes, and ends with a note naming `worktree` so the reader can
/// open the rest. Pure: no git, no files.
pub fn render_unified(diff: &SessionDiff, stat_only: bool, worktree: &Path, cap: usize) -> String {
    if diff.files.is_empty() {
        return "No changes.".to_string();
    }
    let total = format!(
        "{} {} +{} -{}",
        diff.stat.files,
        if diff.stat.files == 1 {
            "file"
        } else {
            "files"
        },
        diff.stat.insertions,
        diff.stat.deletions
    );
    let mut out = Capped {
        text: String::new(),
        cap,
        full: false,
    };
    let mut shown = 0;
    for file in &diff.files {
        let name = rendered_name(file);
        let untracked = if file.status == FileStatus::Untracked {
            " untracked"
        } else {
            ""
        };
        let counts = if file.binary {
            "binary".to_string()
        } else {
            format!("+{} -{}", file.insertions, file.deletions)
        };
        if stat_only {
            if !out.push(&format!(
                "{} {name} {counts}{untracked}",
                file.status.letter()
            )) {
                break;
            }
            shown += 1;
            continue;
        }
        if !push_file(&mut out, file, &name, &counts, untracked) {
            break;
        }
        shown += 1;
    }
    let mut text = out.text;
    if out.full {
        let _ = std::fmt::Write::write_fmt(
            &mut text,
            format_args!(
                "[truncated at {} KB after {shown} of {} files. Read the rest in the worktree: {}]\n",
                cap / 1024,
                diff.files.len(),
                worktree.display()
            ),
        );
    }
    let _ = std::fmt::Write::write_fmt(&mut text, format_args!("{total}"));
    text
}

/// One file's section of the unified text: its header line, mode change,
/// binary note, `---`/`+++` names, hunks, and collapsed note. False once a
/// line did not fit under the cap.
fn push_file(out: &mut Capped, file: &FileDiff, name: &str, counts: &str, untracked: &str) -> bool {
    let mut fits = out.push(&format!(
        "diff {} {name} ({counts}){untracked}",
        file.status.letter()
    ));
    if fits && let Some(mode) = &file.mode_change {
        fits = out.push(&format!("mode {} -> {}", mode.old, mode.new));
    }
    if fits && file.binary {
        fits = out.push("Binary file, not shown.");
    }
    if fits && !file.binary && !file.hunks.is_empty() {
        let old = match file.status {
            FileStatus::Added | FileStatus::Untracked => "/dev/null".to_string(),
            _ => format!("a/{}", file.old_path.as_deref().unwrap_or(&file.path)),
        };
        let new = match file.status {
            FileStatus::Deleted => "/dev/null".to_string(),
            _ => format!("b/{}", file.path),
        };
        fits = out.push(&format!("--- {old}")) && out.push(&format!("+++ {new}"));
    }
    if fits {
        fits = push_hunks(out, &file.hunks);
    }
    if fits && let Some(reason) = file.collapsed {
        let why = match reason {
            Collapse::Lines => "it changes more than 2000 lines".to_string(),
            Collapse::Size(bytes) => format!("its patch is {bytes} bytes"),
            Collapse::Budget => "the task's diff is too large to show in full".to_string(),
        };
        fits = out.push(&format!(
            "[{} changed lines not shown: {why}. Read {name} in the worktree.]",
            file.insertions + file.deletions
        ));
    }
    fits
}

/// A file's hunks as unified diff lines. False once a line did not fit.
fn push_hunks(out: &mut Capped, hunks: &[Hunk]) -> bool {
    for hunk in hunks {
        if !out.push(&hunk.header) {
            return false;
        }
        for line in &hunk.lines {
            let prefix = match line.kind {
                LineKind::Context => " ",
                LineKind::Added => "+",
                LineKind::Removed => "-",
                LineKind::NoNewline => "",
            };
            if !out.push(&format!("{prefix}{}", line.text.trim_end_matches('\r'))) {
                return false;
            }
        }
    }
    true
}
