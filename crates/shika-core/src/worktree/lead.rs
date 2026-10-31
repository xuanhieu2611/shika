//! The project's one fixed Lead worktree: creating it detached, and moving it
//! to the latest base only when it is clean and holds no unreferenced commits.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use super::close::{has_unreferenced_commits, is_dirty};
use super::git::{git_cmd, rev_parse};
use super::identity::is_verified_worktree;
use super::lifecycle::ensure_excluded;
use crate::error::{Error, Result, first_line};

/// The folder name of a project's one Lead worktree, under `.worktrees/`.
pub const LEAD_WORKTREE: &str = "shika-lead";

/// `<repo>/.worktrees/shika-lead`, where the project's Lead always runs.
pub fn lead_worktree_path(repo: &Path) -> PathBuf {
    repo.join(".worktrees").join(LEAD_WORKTREE)
}

/// What a Lead start did to the project's fixed worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeadWorktree {
    /// There was none; it is now a detached checkout of the base.
    Created,
    /// It was clean and already at the base.
    Unchanged,
    /// It was clean and moved to the latest base. Short commit ids.
    Refreshed { from: String, to: String },
    /// It has uncommitted changes, so it was left exactly as it was.
    KeptDirty,
    /// Its HEAD has commits that are on no branch, tag, or remote-tracking
    /// ref, so moving it would orphan them. Left exactly as it was.
    KeptCommits,
    /// It could not be checked or moved; it was left as it was. Git's reason.
    RefreshFailed(String),
}

/// Makes `<repo>/.worktrees/shika-lead` ready for a Lead. Missing: a detached
/// checkout of `start` (`git worktree add --detach`, no branch). Present, a
/// registered worktree of this repository, and clean (no uncommitted changes
/// and no commits that are on no ref): moved to `start`. Present and dirty,
/// or holding unreferenced commits: left alone. Present but not a real
/// directory that is a worktree of this repository (a symbolic link, a file,
/// a folder): [`Error::LeadWorktreeInvalid`], and nothing is deleted.
/// `start` is a ref or `HEAD` of the main checkout; it is resolved to a commit
/// first, so the Lead's own `HEAD` is never the answer.
pub fn prepare_lead_worktree(
    git: &Path,
    path_env: &str,
    repo: &Path,
    start: &str,
) -> Result<LeadWorktree> {
    ensure_excluded(git, path_env, repo)?;
    let path = lead_worktree_path(repo);
    let target = rev_parse(git, path_env, repo, &format!("{start}^{{commit}}"))
        .map_err(Error::CreateWorktree)?;
    match fs::symlink_metadata(&path) {
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            fs::create_dir_all(repo.join(".worktrees")).map_err(|_| Error::CreateWorktree(None))?;
            // `--force` also reuses a registration whose folder was deleted.
            let output = git_cmd(git, path_env, repo)
                .args(["worktree", "add", "--detach", "--force"])
                .arg(&path)
                .arg(&target)
                .output()
                .map_err(|_| Error::CreateWorktree(None))?;
            if !output.status.success() {
                let _ = fs::remove_dir(repo.join(".worktrees"));
                return Err(Error::CreateWorktree(first_line(&output.stderr)));
            }
            Ok(LeadWorktree::Created)
        }
        Err(_) => Err(Error::LeadWorktreeInvalid(path)),
        Ok(meta) => {
            // `symlink_metadata`: a link to a directory is not a directory.
            if !meta.is_dir() || !is_verified_worktree(git, path_env, repo, &path)? {
                return Err(Error::LeadWorktreeInvalid(path));
            }
            Ok(refresh_lead_worktree(git, path_env, &path, &target))
        }
    }
}

/// Moves a registered, existing Lead worktree to `target` when it is clean.
fn refresh_lead_worktree(git: &Path, path_env: &str, path: &Path, target: &str) -> LeadWorktree {
    let reason = |line: Option<String>| {
        LeadWorktree::RefreshFailed(line.unwrap_or_else(|| "git failed".to_string()))
    };
    match is_dirty(git, path_env, path) {
        Ok(true) => return LeadWorktree::KeptDirty,
        Ok(false) => {}
        Err(Error::GitStatus(line)) => return reason(line),
        Err(err) => return reason(Some(err.to_string())),
    }
    match has_unreferenced_commits(git, path_env, path) {
        Ok(true) => return LeadWorktree::KeptCommits,
        Ok(false) => {}
        Err(Error::GitStatus(line)) => return reason(line),
        Err(err) => return reason(Some(err.to_string())),
    }
    let current = match rev_parse(git, path_env, path, "HEAD") {
        Ok(current) => current,
        Err(line) => return reason(line),
    };
    if current == target {
        return LeadWorktree::Unchanged;
    }
    match git_cmd(git, path_env, path)
        // Never discover an ancestor repository if the gitfile disappears
        // after validation. Git accepts a linked worktree's gitfile here.
        .arg("--git-dir")
        .arg(path.join(".git"))
        .args(["checkout", "--detach", "--quiet", target])
        .output()
    {
        Ok(output) if output.status.success() => LeadWorktree::Refreshed {
            from: short_commit(&current),
            to: short_commit(target),
        },
        Ok(output) => reason(first_line(&output.stderr)),
        Err(_) => reason(None),
    }
}

fn short_commit(commit: &str) -> String {
    commit.chars().take(10).collect()
}
