//! Adding and removing task worktrees on disk: the draft worktree on its own
//! branch, its first index refresh, the `.worktrees/` line in
//! `info/exclude`, and removal of a tree and its branch.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::git::git_cmd;
use crate::error::{Error, Result, first_line};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Draft {
    pub branch: String,
    pub path: PathBuf,
}

/// Creates `<repo>/.worktrees/shika-draft-<id>` on a new branch that starts
/// at `start`, a ref or `HEAD`. The branch gets no upstream even when it
/// starts from a remote-tracking branch, so a fresh card never looks pushed
/// and a plain `git push` cannot land on the base.
pub fn create_draft(
    git: &Path,
    path_env: &str,
    repo: &Path,
    id: &str,
    start: &str,
) -> Result<Draft> {
    ensure_excluded(git, path_env, repo)?;
    let branch = format!("shika-draft-{id}");
    let path = repo.join(".worktrees").join(&branch);
    if path.exists() {
        return Err(Error::DraftExists);
    }
    fs::create_dir_all(repo.join(".worktrees")).map_err(|_| Error::CreateWorktree(None))?;
    let output = git_cmd(git, path_env, repo)
        .args(["worktree", "add", "--no-track", "-b", &branch])
        .arg(&path)
        .arg(start)
        .output()
        .map_err(|_| Error::CreateWorktree(None))?;
    if !output.status.success() {
        let _ = fs::remove_dir(repo.join(".worktrees"));
        return Err(Error::CreateWorktree(first_line(&output.stderr)));
    }
    Ok(Draft { branch, path })
}

/// The smallest index worth waiting out a second for. Measured 2026-11-19
/// on a synthetic 50,000-file repo: racy entries cost each diff read about
/// 5us a file (0.13 to 0.34s against 0.09s refreshed), and the wait averages
/// half a second before the CLI starts. Below this a racy read costs a few
/// milliseconds of background work, less than the wait would cost the
/// author on every New.
const WAIT_FROM_ENTRIES: u32 = 10_000;

/// Rewrites a new worktree's index once. A checkout stamps most files in the
/// second it writes the index, and git re-reads every file stamped in the
/// index's own second or later ("racily clean") on each status and diff.
/// The diff stat and Changes read with optional locks off, so they never
/// rewrite it, and stay slow on a large repo until something else does.
/// When the refresh lands in the checkout's second, its index is just as
/// racy, so a large index waits that second out and refreshes once more.
/// Best effort: a failure leaves git slower, never the task unstarted. Run
/// it before setup and the CLI start, since it holds `index.lock`.
pub fn refresh_index(git: &Path, path_env: &str, worktree: &Path) {
    let _ = try_refresh_index(git, path_env, worktree, WAIT_FROM_ENTRIES);
}

pub(super) fn try_refresh_index(
    git: &Path,
    path_env: &str,
    worktree: &Path,
    wait_from_entries: u32,
) -> Result<()> {
    let output = git_cmd(git, path_env, worktree)
        .args(["rev-parse", "--git-path", "index"])
        .output()
        .map_err(|_| Error::Git(None))?;
    let index = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !output.status.success() || index.is_empty() {
        return Err(Error::Git(first_line(&output.stderr)));
    }
    let index = worktree.join(index);
    let checkout = mtime_second(&index);
    refresh(git, path_env, worktree)?;
    let (Some(checkout), Some(refreshed)) = (checkout, mtime_second(&index)) else {
        return Ok(());
    };
    if refreshed > checkout || index_entries(&index).is_none_or(|n| n < wait_from_entries) {
        return Ok(());
    }
    // File timestamps can trail the clock by a kernel tick, hence the 50ms.
    // A wait of two seconds or more means this clock does not match the
    // file timestamps, as on a share another machine stamps, so it cannot
    // say when that second ends.
    let end = UNIX_EPOCH + Duration::from_secs(checkout + 1) + Duration::from_millis(50);
    let delay = end.duration_since(SystemTime::now()).unwrap_or_default();
    if delay >= Duration::from_secs(2) {
        return Ok(());
    }
    thread::sleep(delay);
    refresh(git, path_env, worktree)
}

/// The entry count from the index header: `DIRC`, a version, then the count,
/// each four bytes, big-endian.
pub(super) fn index_entries(index: &Path) -> Option<u32> {
    let mut header = [0u8; 12];
    fs::File::open(index).ok()?.read_exact(&mut header).ok()?;
    if &header[..4] != b"DIRC" {
        return None;
    }
    Some(u32::from_be_bytes(header[8..12].try_into().ok()?))
}

/// `--force-write-index`: older git (such as 2.30) skips the write when the
/// racy entries merely check out clean.
fn refresh(git: &Path, path_env: &str, worktree: &Path) -> Result<()> {
    let output = git_cmd(git, path_env, worktree)
        .args(["update-index", "-q", "--refresh", "--force-write-index"])
        .output()
        .map_err(|_| Error::Git(None))?;
    if !output.status.success() {
        return Err(Error::Git(first_line(&output.stderr)));
    }
    Ok(())
}

fn mtime_second(path: &Path) -> Option<u64> {
    let modified = fs::metadata(path).ok()?.modified().ok()?;
    Some(modified.duration_since(UNIX_EPOCH).ok()?.as_secs())
}

/// Remove only the directory, preserving an already pushed local branch.
/// Without `force`, git's own clean check sees untracked files even when the
/// author's config sets `status.showUntrackedFiles=no`.
pub fn remove_worktree(
    git: &Path,
    path_env: &str,
    repo: &Path,
    worktree: &Path,
    force: bool,
) -> Result<()> {
    let mut cmd = git_cmd(git, path_env, repo);
    cmd.args([
        "-c",
        "status.showUntrackedFiles=normal",
        "worktree",
        "remove",
    ]);
    if force {
        cmd.arg("--force");
    }
    let output = cmd
        .arg("--")
        .arg(worktree)
        .output()
        .map_err(|_| Error::RemoveWorktree(None))?;
    if !output.status.success() && worktree.exists() {
        let line = first_line(&output.stderr);
        if String::from_utf8_lossy(&output.stderr).contains("use --force") {
            return Err(Error::WorktreeHasChanges(line));
        }
        return Err(Error::RemoveWorktree(line));
    }
    Ok(())
}

/// Removes the worktree, then deletes its local branch. Without `force`, a
/// tree with changes is refused with [`Error::WorktreeHasChanges`].
pub fn remove_draft(
    git: &Path,
    path_env: &str,
    repo: &Path,
    worktree: &Path,
    branch: &str,
    force: bool,
) -> Result<()> {
    remove_worktree(git, path_env, repo, worktree, force)?;
    let deleted = git_cmd(git, path_env, repo)
        .args(["branch", "-D", "--", branch])
        .output()
        .map_err(|_| Error::RemoveWorktree(None))?;
    if !deleted.status.success() {
        let stderr = String::from_utf8_lossy(&deleted.stderr);
        if !stderr.contains("not found") {
            return Err(Error::RemoveWorktree(first_line(&deleted.stderr)));
        }
    }
    Ok(())
}

pub(super) fn ensure_excluded(git: &Path, path_env: &str, repo: &Path) -> Result<()> {
    add_exclude(
        git,
        path_env,
        repo,
        ".worktrees/",
        &[".worktrees/", ".worktrees"],
    )
}

/// Adds `pattern` to the repository's `info/exclude` unless it is there.
/// Linked worktrees share that file, so a pattern anchored with `/` hides a
/// path at the root of every worktree, the Lead's included.
pub(crate) fn ensure_excluded_path(
    git: &Path,
    path_env: &str,
    repo: &Path,
    pattern: &str,
) -> Result<()> {
    add_exclude(git, path_env, repo, pattern, &[pattern])
}

fn add_exclude(
    git: &Path,
    path_env: &str,
    repo: &Path,
    pattern: &str,
    present: &[&str],
) -> Result<()> {
    let exclude = exclude_file(git, path_env, repo)?;
    let mut text = if exclude.exists() {
        fs::read_to_string(&exclude).map_err(|_| Error::Exclude)?
    } else {
        String::new()
    };
    if text.lines().any(|line| present.contains(&line.trim())) {
        return Ok(());
    }
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(pattern);
    text.push('\n');
    if let Some(parent) = exclude.parent() {
        fs::create_dir_all(parent).map_err(|_| Error::Exclude)?;
    }
    fs::write(exclude, text).map_err(|_| Error::Exclude)
}

pub(super) fn exclude_file(git: &Path, path_env: &str, repo: &Path) -> Result<PathBuf> {
    let output = git_cmd(git, path_env, repo)
        .args(["rev-parse", "--absolute-git-dir"])
        .output()
        .map_err(|_| Error::Git(None))?;
    if !output.status.success() {
        return Err(Error::Git(first_line(&output.stderr)));
    }
    let dir = String::from_utf8_lossy(&output.stdout);
    let dir = dir.trim();
    if dir.is_empty() {
        return Err(Error::Git(None));
    }
    Ok(PathBuf::from(dir).join("info").join("exclude"))
}
