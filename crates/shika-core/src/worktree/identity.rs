//! Worktree identity: a real directory, registered with this repository, whose
//! own git discovery, common directory, and gitfile backlink all name it.
//! Registration alone is not proof that commands act on this tree.

use std::ffi::OsStr;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::process::Command;

use super::git::{check_worktree, git_cmd, read_only_git};
use crate::error::{Error, Result, first_line};

/// Whether `path` is a real directory. A symbolic link is not, even to a
/// directory: the Lead's fixed path must be the worktree itself, or Forget and
/// refresh would act on whatever the link points at.
pub fn is_real_dir(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|meta| meta.is_dir())
}

/// Whether `git worktree list` of `repo` names `path`, compared canonicalized
/// so `/tmp` and `/private/tmp` agree.
pub fn is_registered_worktree(
    git: &Path,
    path_env: &str,
    repo: &Path,
    path: &Path,
) -> Result<bool> {
    let output = git_cmd(git, path_env, repo)
        .args(["worktree", "list", "--porcelain"])
        .output()
        .map_err(|_| Error::Git(None))?;
    if !output.status.success() {
        return Err(Error::Git(first_line(&output.stderr)));
    }
    let wanted = fs::canonicalize(path).ok();
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.strip_prefix("worktree "))
        .any(|listed| match (&wanted, fs::canonicalize(listed)) {
            (Some(wanted), Ok(listed)) => *wanted == listed,
            _ => Path::new(listed) == path,
        }))
}

/// A real, registered worktree whose own Git discovery resolves to this
/// directory and this repository. Registration alone survives a deleted or
/// replaced `.git` file and is not proof that commands act on this tree.
/// Invalid paths are left untouched by Lead refresh and Forget.
pub(crate) fn is_verified_worktree(
    git: &Path,
    path_env: &str,
    repo: &Path,
    path: &Path,
) -> Result<bool> {
    if !is_real_dir(path)
        || !fs::symlink_metadata(path.join(".git")).is_ok_and(|meta| meta.is_file())
        || !is_registered_worktree(git, path_env, repo, path)?
        || check_worktree(git, path_env, path).is_err()
    {
        return Ok(false);
    }
    let git_path = |mut cmd: Command, field: &str| {
        let output = cmd
            .args(["rev-parse", "--path-format=absolute", field])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let raw = output.stdout.strip_suffix(b"\n").unwrap_or(&output.stdout);
        fs::canonicalize(OsStr::from_bytes(raw)).ok()
    };
    if !matches!(
        (
            git_path(git_cmd(git, path_env, repo), "--git-common-dir"),
            git_path(read_only_git(git, path_env, path), "--git-common-dir"),
        ),
        (Some(expected), Some(found)) if expected == found
    ) {
        return Ok(false);
    }
    // A replaced gitfile could point at the main checkout's index, or at
    // another linked worktree in the same repository. Require the admin
    // directory to point back at this exact gitfile as well.
    let Some(admin) = git_path(read_only_git(git, path_env, path), "--absolute-git-dir") else {
        return Ok(false);
    };
    let backlink = fs::read(admin.join("gitdir")).ok().and_then(|raw| {
        let raw = raw.strip_suffix(b"\n").unwrap_or(&raw);
        fs::canonicalize(OsStr::from_bytes(raw)).ok()
    });
    Ok(matches!(
        (backlink, fs::canonicalize(path.join(".git"))),
        (Some(found), Ok(expected)) if found == expected
    ))
}
