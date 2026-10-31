//! The task's diff stat and Changes panel diff, read only and bounded to the
//! task worktree, measured against its `Baseline`.

use std::ffi::OsStr;
use std::io::{self, BufReader, Read};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::process::{ChildStdout, Command, Stdio};

use super::base::compare_base;
use super::git::{check_worktree, read_only_git};
use crate::diff::{self, FileDiff, FileKey, FileStatus, SessionDiff};
use crate::error::{Error, Result, first_line};

/// What a diff of a session's tree is measured against.
#[derive(Debug, Clone, Copy)]
pub enum Baseline<'a> {
    /// Where a task branch left its base: the recorded start ref, else the
    /// default branch of `repo`. Commits and uncommitted work both count.
    Task {
        repo: &'a Path,
        recorded: Option<&'a str>,
    },
    /// `HEAD`: uncommitted work only. A checkout session's baseline, since
    /// the author's own commits there are not the session's.
    Head,
}

/// The commit or ref `baseline` resolves to in `worktree`.
fn resolve_baseline(git: &Path, path_env: &str, worktree: &Path, baseline: Baseline) -> String {
    match baseline {
        Baseline::Task { repo, recorded } => diff_base(git, path_env, repo, worktree, recorded),
        Baseline::Head => "HEAD".into(),
    }
}

/// What the session changed against `baseline`: tracked edits (and, for a
/// task, its commits), plus untracked files that are not ignored. Reads
/// only; never stages anything or writes the index.
pub fn diff_stat(
    git: &Path,
    path_env: &str,
    worktree: &Path,
    baseline: Baseline,
) -> Result<crate::DiffStat> {
    check_worktree(git, path_env, worktree).map_err(Error::GitStatus)?;
    let base = resolve_baseline(git, path_env, worktree, baseline);
    // Comparing the base to the working tree covers commits and edits at once.
    let output = read_only_git(git, path_env, worktree)
        .args(["diff", "--numstat"])
        .args(TASK_DIFF)
        .args([&base, "--"])
        .output()
        .map_err(|_| Error::GitStatus(None))?;
    if !output.status.success() {
        return Err(Error::GitStatus(first_line(&output.stderr)));
    }
    let mut stat = crate::DiffStat::default();
    // One line per file, renames included; unusual paths are quoted.
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let mut fields = line.splitn(3, '\t');
        let (Some(added), Some(deleted), Some(_)) = (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        stat.files += 1;
        // A binary file reports `-` for both counts.
        stat.insertions += added.parse::<usize>().unwrap_or(0);
        stat.deletions += deleted.parse::<usize>().unwrap_or(0);
    }
    let untracked = untracked(git, path_env, worktree).map_err(Error::GitStatus)?;
    for path in untracked.split(|byte| *byte == 0) {
        if path.is_empty() {
            continue;
        }
        stat.files += 1;
        stat.insertions += diff::untracked_lines(&worktree.join(OsStr::from_bytes(path)));
    }
    Ok(stat)
}

/// Options every task diff shares, so the Changes panel and the card's stat
/// count the same files and lines whatever the user's git config says:
/// no color, no external diff or textconv program, renames found, and
/// submodules as one commit line each (`diff.submodule=log` would print no
/// patch for them).
const TASK_DIFF: [&str; 5] = [
    "--no-color",
    "--no-ext-diff",
    "--no-textconv",
    "--find-renames",
    "--submodule=short",
];

/// Patch options on top of [`TASK_DIFF`]: three lines of context, and the
/// `a/` and `b/` prefixes the parser expects even under `diff.noprefix` or
/// `diff.mnemonicPrefix`.
const TASK_PATCH: [&str; 3] = ["-U3", "--src-prefix=a/", "--dst-prefix=b/"];

/// The session's whole change for the Changes panel, measured exactly as
/// [`diff_stat`] measures it. After the base is found, two git processes:
/// the patch, streamed and parsed as it arrives, and the untracked list.
/// Reads only, and only this worktree: one whose `.git` is missing or broken
/// is an error, never the main checkout around it.
pub fn session_diff(
    git: &Path,
    path_env: &str,
    worktree: &Path,
    baseline: Baseline,
) -> Result<SessionDiff> {
    check_worktree(git, path_env, worktree).map_err(Error::ReadChanges)?;
    let base = resolve_baseline(git, path_env, worktree, baseline);
    let untracked = untracked(git, path_env, worktree).map_err(Error::ReadChanges)?;
    let paths: Vec<&[u8]> = untracked
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .collect();
    let mut command = read_only_git(git, path_env, worktree);
    command
        .arg("diff")
        .args(TASK_DIFF)
        .args(TASK_PATCH)
        .args([&base, "--"]);
    stream_git(command, |patch| {
        diff::session_diff(patch, &paths, worktree, &base)
    })
}

/// One file of an earlier [`session_diff`] read again, uncapped up to the
/// expand limit, against the same base. A tracked file costs one git process
/// limited to its path (both paths of a rename, so the pair is still found);
/// an untracked file is read directly. None when the file no longer differs.
pub fn file_diff(
    git: &Path,
    path_env: &str,
    worktree: &Path,
    key: &FileKey,
) -> Result<Option<FileDiff>> {
    if key.status == FileStatus::Untracked {
        return Ok(diff::expand_untracked(worktree, key));
    }
    check_worktree(git, path_env, worktree).map_err(Error::ReadChanges)?;
    let mut command = read_only_git(git, path_env, worktree);
    // Literal pathspecs, so a `*` or `:` in a name is only itself.
    command
        .arg("--literal-pathspecs")
        .arg("diff")
        .args(TASK_DIFF)
        .args(TASK_PATCH)
        .args([key.base.as_str(), "--"])
        .arg(OsStr::from_bytes(&key.path));
    if let Some(old) = &key.old_path {
        command.arg(OsStr::from_bytes(old));
    }
    stream_git(command, |patch| diff::expand_tracked(patch, key))
}

/// Untracked paths that are not ignored, NUL separated. The error is git's
/// first line.
fn untracked(git: &Path, path_env: &str, worktree: &Path) -> Result<Vec<u8>, Option<String>> {
    let output = read_only_git(git, path_env, worktree)
        .args(["ls-files", "--others", "--exclude-standard", "-z"])
        .output()
        .map_err(|_| None)?;
    if !output.status.success() {
        return Err(first_line(&output.stderr));
    }
    Ok(output.stdout)
}

/// Run git and hand its stdout to `read` as a stream, so the output is never
/// held whole. Stderr is drained on its own thread so a chatty git cannot
/// stall on a full pipe.
fn stream_git<T>(
    mut command: Command,
    read: impl FnOnce(BufReader<ChildStdout>) -> io::Result<T>,
) -> Result<T> {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null())
        .spawn()
        .map_err(|_| Error::ReadChanges(None))?;
    let (Some(stdout), Some(mut stderr)) = (child.stdout.take(), child.stderr.take()) else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(Error::ReadChanges(None));
    };
    let errors = std::thread::spawn(move || {
        let mut text = Vec::new();
        let _ = stderr.read_to_end(&mut text);
        text
    });
    let value = read(BufReader::with_capacity(64 * 1024, stdout));
    if value.is_err() {
        let _ = child.kill();
    }
    let status = child.wait().map_err(|_| Error::ReadChanges(None))?;
    let errors = errors.join().unwrap_or_default();
    if !status.success() {
        return Err(Error::ReadChanges(first_line(&errors)));
    }
    value.map_err(|err| Error::ReadChanges(Some(err.to_string())))
}

/// Where the task branch left its base. Falls back to HEAD, so only
/// uncommitted work counts, when there is no base to compare with or the two
/// share no history.
fn diff_base(
    git: &Path,
    path_env: &str,
    repo: &Path,
    worktree: &Path,
    recorded: Option<&str>,
) -> String {
    let Ok(default) = compare_base(git, path_env, repo, worktree, recorded) else {
        return "HEAD".into();
    };
    let output = read_only_git(git, path_env, worktree)
        .args(["merge-base", &default, "HEAD"])
        .output();
    match output {
        Ok(output) if output.status.success() => {
            let base = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if base.is_empty() { "HEAD".into() } else { base }
        }
        _ => "HEAD".into(),
    }
}
