//! Adding and removing task worktrees on disk: the draft worktree on its own
//! branch, the `.worktrees/` line in `info/exclude`, and removal of a tree
//! and its branch.

use std::fs;
use std::path::{Path, PathBuf};

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

/// Remove only the directory, preserving an already pushed local branch.
pub fn remove_worktree(
    git: &Path,
    path_env: &str,
    repo: &Path,
    worktree: &Path,
    force: bool,
) -> Result<()> {
    let mut cmd = git_cmd(git, path_env, repo);
    cmd.args(["worktree", "remove"]);
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
    let exclude = exclude_file(git, path_env, repo)?;
    let mut text = if exclude.exists() {
        fs::read_to_string(&exclude).map_err(|_| Error::Exclude)?
    } else {
        String::new()
    };
    if text.lines().any(|line| {
        let line = line.trim();
        line == ".worktrees/" || line == ".worktrees"
    }) {
        return Ok(());
    }
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(".worktrees/\n");
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
