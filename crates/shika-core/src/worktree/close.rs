//! What Close checks before it removes anything: uncommitted changes, own and
//! unpushed commits against the compare base, commits on no ref, the
//! branch-switch tips, and the push Close offers. None of it commits.

use std::path::Path;

use super::base::compare_base;
use super::git::{git_cmd, read_only_git};
use crate::error::{Error, Result, first_line};

pub fn is_dirty(git: &Path, path_env: &str, worktree: &Path) -> Result<bool> {
    let output = git_cmd(git, path_env, worktree)
        .args(["status", "--porcelain"])
        .output()
        .map_err(|_| Error::GitStatus(None))?;
    if !output.status.success() {
        return Err(Error::GitStatus(first_line(&output.stderr)));
    }
    Ok(!String::from_utf8_lossy(&output.stdout).trim().is_empty())
}

/// Whether the worktree's HEAD has commits that no branch, tag, or
/// remote-tracking ref reaches, which a checkout of another commit would leave
/// with nothing pointing at them. `--all` is not used: it counts HEAD itself.
/// Reads only (`GIT_OPTIONAL_LOCKS=0`).
pub fn has_unreferenced_commits(git: &Path, path_env: &str, worktree: &Path) -> Result<bool> {
    let output = git_cmd(git, path_env, worktree)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .args([
            "rev-list",
            "--max-count=1",
            "HEAD",
            "--not",
            "--branches",
            "--tags",
            "--remotes",
        ])
        .output()
        .map_err(|_| Error::GitStatus(None))?;
    if !output.status.success() {
        return Err(Error::GitStatus(first_line(&output.stderr)));
    }
    Ok(!String::from_utf8_lossy(&output.stdout).trim().is_empty())
}

/// Dirty, unpushed, and pushed, measured against `base`: the ref the task
/// branch started from, when it still resolves. Otherwise the default branch.
pub fn git_state(
    git: &Path,
    path_env: &str,
    repo: &Path,
    worktree: &Path,
    base: Option<&str>,
    agent_working: bool,
) -> Result<crate::SessionGitState> {
    let dirty = is_dirty(git, path_env, worktree)?;
    let upstream = git_cmd(git, path_env, worktree)
        .args([
            "rev-parse",
            "--abbrev-ref",
            "--symbolic-full-name",
            "@{upstream}",
        ])
        .output()
        .map_err(|_| Error::GitStatus(None))?;
    let default = compare_base(git, path_env, repo, worktree, base)?;
    let has_own_commits = commits_not_in(git, path_env, worktree, &[&default])?;
    let (unpushed, pushed) = if upstream.status.success() {
        let upstream = String::from_utf8_lossy(&upstream.stdout).trim().to_string();
        let unpushed = commits_not_in(git, path_env, worktree, &[&upstream])?;
        (unpushed, !unpushed)
    } else {
        // `git push origin HEAD` sets no upstream but still updates a
        // remote-tracking ref, so any remote branch holding the commits counts.
        let unpushed = commits_not_in(git, path_env, worktree, &[&default, "--remotes"])?;
        (unpushed, has_own_commits && !unpushed)
    };
    Ok(crate::SessionGitState {
        dirty,
        unpushed,
        pushed,
        has_own_commits,
        agent_working,
    })
}

/// Verify a switched task without deleting or adopting either branch.
/// Full refs avoid revision-option ambiguity. Missing refs fail closed.
pub fn switched_close_tips(
    git: &Path,
    path_env: &str,
    session: &crate::Session,
    current: &str,
) -> Result<(String, String)> {
    if is_dirty(git, path_env, &session.worktree)? {
        return Err(Error::SwitchedCloseUnsafe(
            "The worktree has uncommitted changes. Commit and push in the shell, then close again."
                .into(),
        ));
    }
    let base = compare_base(
        git,
        path_env,
        &session.repo,
        &session.worktree,
        session.base_ref.as_deref(),
    )?;
    let mut tips = Vec::new();
    for branch in [&session.branch, current] {
        let reference = format!("refs/heads/{branch}");
        let output = read_only_git(git, path_env, &session.worktree)
            .args(["rev-parse", "--verify", &format!("{reference}^{{commit}}")])
            .output()
            .map_err(|_| Error::GitStatus(None))?;
        if !output.status.success() {
            return Err(Error::SwitchedCloseUnsafe(format!(
                "Cannot verify branch {branch}. Return to the task branch before closing."
            )));
        }
        // A local base equal to the branch being checked proves nothing about
        // unpublished commits on that branch. In that case only remotes count.
        let base_ref = read_only_git(git, path_env, &session.worktree)
            .args(["rev-parse", "--symbolic-full-name", &base])
            .output()
            .map_err(|_| Error::GitStatus(None))?;
        if !base_ref.status.success() {
            return Err(Error::GitStatus(first_line(&base_ref.stderr)));
        }
        let base_is_branch = String::from_utf8_lossy(&base_ref.stdout).trim() == reference;
        let mut command = read_only_git(git, path_env, &session.worktree);
        command.args([
            "rev-list",
            "--max-count=1",
            &reference,
            "--not",
            "--remotes",
        ]);
        if !base_is_branch {
            command.arg(&base);
        }
        command.arg("--");
        let unpublished = command.output().map_err(|_| Error::GitStatus(None))?;
        if !unpublished.status.success() {
            return Err(Error::GitStatus(first_line(&unpublished.stderr)));
        }
        if !unpublished.stdout.is_empty() {
            return Err(Error::SwitchedCloseUnsafe(format!(
                "Branch {branch} has unpublished commits. Push or integrate them before closing, or return to the task branch for the existing close choices."
            )));
        }
        tips.push(String::from_utf8_lossy(&output.stdout).trim().to_string());
    }
    Ok((tips.remove(0), tips.remove(0)))
}

fn commits_not_in(git: &Path, path_env: &str, worktree: &Path, excluded: &[&str]) -> Result<bool> {
    let output = git_cmd(git, path_env, worktree)
        .args(["rev-list", "--count", "HEAD", "--not"])
        .args(excluded)
        .output()
        .map_err(|_| Error::GitStatus(None))?;
    if !output.status.success() {
        return Err(Error::GitStatus(first_line(&output.stderr)));
    }
    let count = String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse::<u64>()
        .map_err(|_| Error::GitStatus(None))?;
    Ok(count > 0)
}

pub fn push(git: &Path, path_env: &str, worktree: &Path) -> Result<()> {
    let output = git_cmd(git, path_env, worktree)
        .args(["push", "-u", "origin", "HEAD"])
        .output()
        .map_err(|_| Error::Push(None))?;
    if !output.status.success() {
        return Err(Error::Push(first_line(&output.stderr)));
    }
    Ok(())
}
