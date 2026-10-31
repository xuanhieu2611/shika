//! Where a task starts and what its work is measured against: the configured
//! or default base branch, the branches the Base branch dialog lists, and
//! the compare base that Close and the diff share.

use std::collections::BTreeSet;
use std::path::Path;

use super::branch::{head_branch, is_valid_branch};
use super::fetch::has_origin;
use super::git::{git_cmd, ref_exists};
use crate::error::{Error, Result, first_line};

/// Where New starts a task branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BaseStart {
    /// The branch as the app shows it: `dev`, `main`. None only when the start
    /// is a detached HEAD in the main checkout.
    pub name: Option<String>,
    /// The full ref the branch starts from, such as `refs/remotes/origin/dev`.
    /// None when the start is the main checkout's HEAD, the last resort, which
    /// is no fixed base: later checks then use the default branch.
    pub reference: Option<String>,
}

impl BaseStart {
    /// What `git worktree add` starts from.
    pub fn start(&self) -> &str {
        self.reference.as_deref().unwrap_or("HEAD")
    }
}

/// Resolves where New starts. A configured base `B` is `origin/B`, else the
/// local `B`, and is an error when neither exists: it never falls back to
/// another branch. Unset, it is the remote default (`origin/HEAD`), then local
/// main, then master, then the main checkout's HEAD.
pub fn resolve_base(
    git: &Path,
    path_env: &str,
    repo: &Path,
    configured: Option<&str>,
) -> Result<BaseStart> {
    if let Some(branch) = configured {
        if let Some(reference) = configured_ref(git, path_env, repo, branch)? {
            return Ok(BaseStart {
                name: Some(branch.to_string()),
                reference: Some(reference),
            });
        }
        return Err(Error::BaseBranchMissing(branch.to_string()));
    }
    if let Some(reference) = named_default(git, path_env, repo)? {
        return Ok(BaseStart {
            name: Some(short_branch(&reference).to_string()),
            reference: Some(reference),
        });
    }
    let head = git_cmd(git, path_env, repo)
        .args(["symbolic-ref", "--quiet", "--short", "HEAD"])
        .output()
        .map_err(|_| Error::GitStatus(None))?;
    let name = String::from_utf8_lossy(&head.stdout).trim().to_string();
    Ok(BaseStart {
        name: (head.status.success() && !name.is_empty()).then_some(name),
        reference: None,
    })
}

/// Branches already stored in this clone, for the Base branch dialog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnownBranches {
    /// Short names, sorted. A local branch and `origin/` with the same name
    /// are one row.
    pub names: Vec<String>,
    /// The branch checked out in the main checkout, when that name is listed.
    pub checked_out: Option<String>,
    /// Whether a remote named `origin` exists, so a missing name can be fetched.
    pub has_origin: bool,
}

/// Local heads and `origin/*` already on disk. `origin/dev` is `dev`, `HEAD`
/// is dropped, and duplicates collapse. Does not fetch.
pub fn known_branches(git: &Path, path_env: &str, repo: &Path) -> Result<KnownBranches> {
    let output = git_cmd(git, path_env, repo)
        .args([
            "for-each-ref",
            "--format=%(refname)",
            "refs/heads",
            "refs/remotes/origin",
        ])
        .output()
        .map_err(|_| Error::Git(None))?;
    if !output.status.success() {
        return Err(Error::Git(first_line(&output.stderr)));
    }
    let mut names = BTreeSet::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let line = line.trim();
        let short = if let Some(name) = line.strip_prefix("refs/heads/") {
            name
        } else if let Some(name) = line.strip_prefix("refs/remotes/origin/") {
            name
        } else {
            continue;
        };
        if short.is_empty() || short == "HEAD" {
            continue;
        }
        names.insert(short.to_string());
    }
    let checked_out = head_branch(git, path_env, repo)?.filter(|name| names.contains(name));
    Ok(KnownBranches {
        names: names.into_iter().collect(),
        checked_out,
        has_origin: has_origin(git, path_env, repo),
    })
}

/// `refs/remotes/origin/<branch>` if it exists, else `refs/heads/<branch>`.
/// None for a name git does not accept as a branch.
pub fn configured_ref(
    git: &Path,
    path_env: &str,
    repo: &Path,
    branch: &str,
) -> Result<Option<String>> {
    if branch.is_empty() || !is_valid_branch(git, path_env, repo, branch)? {
        return Ok(None);
    }
    for reference in [
        format!("refs/remotes/origin/{branch}"),
        format!("refs/heads/{branch}"),
    ] {
        if ref_exists(git, path_env, repo, &reference)? {
            return Ok(Some(reference));
        }
    }
    Ok(None)
}

/// `dev` from `refs/remotes/origin/dev` or `refs/heads/dev`.
fn short_branch(reference: &str) -> &str {
    reference
        .strip_prefix("refs/remotes/origin/")
        .or_else(|| reference.strip_prefix("refs/heads/"))
        .unwrap_or(reference)
}

/// The ref a session's work is measured against: the recorded base while it
/// still resolves, else the default branch, with its safety refusal.
pub(super) fn compare_base(
    git: &Path,
    path_env: &str,
    repo: &Path,
    worktree: &Path,
    recorded: Option<&str>,
) -> Result<String> {
    if let Some(reference) = recorded
        && ref_exists(git, path_env, repo, reference)?
    {
        return Ok(reference.to_string());
    }
    default_branch(git, path_env, repo, worktree)
}

/// The full ref `refs/remotes/origin/HEAD` points at, when it is valid.
fn remote_default(git: &Path, path_env: &str, repo: &Path) -> Result<Option<String>> {
    let remote = git_cmd(git, path_env, repo)
        .args(["symbolic-ref", "--quiet", "refs/remotes/origin/HEAD"])
        .output()
        .map_err(|_| Error::GitStatus(None))?;
    if !remote.status.success() {
        return Ok(None);
    }
    let reference = String::from_utf8_lossy(&remote.stdout).trim().to_string();
    if reference.is_empty() || !ref_exists(git, path_env, repo, &reference)? {
        return Ok(None);
    }
    Ok(Some(reference))
}

/// The default branch as a named ref: the remote default, then local main,
/// then master. None when the repository has none of them.
fn named_default(git: &Path, path_env: &str, repo: &Path) -> Result<Option<String>> {
    if let Some(reference) = remote_default(git, path_env, repo)? {
        return Ok(Some(reference));
    }
    for candidate in ["refs/heads/main", "refs/heads/master"] {
        if ref_exists(git, path_env, repo, candidate)? {
            return Ok(Some(candidate.to_string()));
        }
    }
    Ok(None)
}

fn default_branch(git: &Path, path_env: &str, repo: &Path, worktree: &Path) -> Result<String> {
    if let Some(reference) = named_default(git, path_env, repo)? {
        return Ok(reference);
    }
    // A custom default branch can be the main checkout's HEAD. Refuse if
    // that checkout is on the task branch: it cannot prove anything is safe.
    let main_ref = git_cmd(git, path_env, repo)
        .args(["symbolic-ref", "--quiet", "HEAD"])
        .output()
        .map_err(|_| Error::GitStatus(None))?;
    let task_ref = git_cmd(git, path_env, worktree)
        .args(["symbolic-ref", "--quiet", "HEAD"])
        .output()
        .map_err(|_| Error::GitStatus(None))?;
    if main_ref.status.success() && task_ref.status.success() && main_ref.stdout == task_ref.stdout
    {
        return Err(Error::GitStatus(Some(
            "Could not identify the default branch safely.".into(),
        )));
    }
    let head = git_cmd(git, path_env, repo)
        .args(["rev-parse", "--verify", "HEAD"])
        .output()
        .map_err(|_| Error::GitStatus(None))?;
    if head.status.success() {
        return Ok(String::from_utf8_lossy(&head.stdout).trim().to_string());
    }
    Err(Error::GitStatus(None))
}
