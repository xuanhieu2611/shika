//! The git command builders every worktree call goes through: the redirect
//! variables removed from every call, the bounded read-only form for
//! background reads, the check that a tree is its own checkout, and small
//! ref lookups.

use std::ffi::OsStr;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::process::Command;

use crate::error::{Error, Result, first_line};

/// Variables that would make git follow another checkout instead of the
/// directory it runs in. Removed from every git call and every PTY.
pub(crate) const GIT_REDIRECTS: [&str; 6] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_PREFIX",
    "GIT_COMMON_DIR",
    "GIT_OBJECT_DIRECTORY",
];

/// One line of `git rev-parse --verify <rev>`, or git's first error line.
pub(super) fn rev_parse(
    git: &Path,
    path_env: &str,
    dir: &Path,
    rev: &str,
) -> Result<String, Option<String>> {
    let output = git_cmd(git, path_env, dir)
        .args(["rev-parse", "--verify", "--quiet", rev])
        .output()
        .map_err(|_| None)?;
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !output.status.success() || text.is_empty() {
        return Err(first_line(&output.stderr));
    }
    Ok(text)
}

/// Whether `reference`, a full `refs/...` name, names a commit in `repo`.
pub(super) fn ref_exists(git: &Path, path_env: &str, repo: &Path, reference: &str) -> Result<bool> {
    let output = git_cmd(git, path_env, repo)
        .args(["rev-parse", "--verify", "--quiet"])
        .arg(format!("{reference}^{{commit}}"))
        .output()
        .map_err(|_| Error::GitStatus(None))?;
    Ok(output.status.success())
}

/// A git call for background reads in a task worktree. Optional locks are
/// off so a refresh never takes the index lock out from under the user's own
/// git commands. Discovery stops at `worktree`: a task worktree sits at
/// `<repo>/.worktrees/<branch>`, so with its `.git` file missing, git would
/// otherwise walk up and read the main checkout as if it were the task.
/// `GIT_CEILING_DIRECTORIES` is the worktree's parent, which git resolves
/// through symlinks. Callers pass the root of the tree they read: a task
/// worktree, or a checkout session's repository root, which is bounded the
/// same way so a broken `.git` never reads a repository around it.
pub(super) fn read_only_git(program: &Path, path_env: &str, worktree: &Path) -> Command {
    let mut cmd = git_cmd(program, path_env, worktree);
    cmd.env("GIT_OPTIONAL_LOCKS", "0");
    if let Some(parent) = worktree.parent() {
        cmd.env("GIT_CEILING_DIRECTORIES", parent);
    }
    cmd
}

/// Fails unless git, run in `worktree`, finds that same directory as the top
/// of its checkout. The ceiling in [`read_only_git`] already stops discovery
/// there; this also holds when the parent path cannot be written as a ceiling
/// (git splits the list at `:`, which a macOS folder name can hold). Paths
/// are compared canonicalized, so `/tmp` and `/private/tmp` agree. The error
/// is git's first line, or a sentence naming the worktree.
pub(super) fn check_worktree(
    git: &Path,
    path_env: &str,
    worktree: &Path,
) -> Result<(), Option<String>> {
    let output = read_only_git(git, path_env, worktree)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .map_err(|_| None)?;
    if !output.status.success() {
        return Err(first_line(&output.stderr));
    }
    let top = output.stdout.strip_suffix(b"\n").unwrap_or(&output.stdout);
    let found = fs::canonicalize(OsStr::from_bytes(top));
    match (found, fs::canonicalize(worktree)) {
        (Ok(found), Ok(expected)) if found == expected => Ok(()),
        _ => Err(Some(format!(
            "{} is not a git worktree of its own.",
            worktree.display()
        ))),
    }
}

pub(crate) fn git_cmd(program: &Path, path_env: &str, repo: &Path) -> Command {
    let mut cmd = Command::new(program);
    if !path_env.is_empty() {
        cmd.env("PATH", path_env);
    }
    for key in GIT_REDIRECTS {
        cmd.env_remove(key);
    }
    cmd.arg("-C").arg(repo);
    cmd
}
