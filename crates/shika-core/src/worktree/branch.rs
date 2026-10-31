//! Task branch names and reads: the name made from a prompt or title, the
//! prefix setting, the checked-out branch, the checkout's HEAD, whether a
//! branch was renamed or published, and the collision-free rename.

use std::path::Path;

use super::git::{check_worktree, git_cmd, read_only_git, ref_exists};
use crate::error::{Error, Result, first_line};

/// Most characters a generated name keeps, before the prefix and any
/// collision suffix.
const SLUG_MAX: usize = 48;

/// Lowercase ASCII words joined by `-`, cut at a word boundary. Whole-word
/// copies of the project's own name are left out, unless nothing else is
/// left: "Shika background blur" in project `shika` is `background-blur`.
/// None when the text has no ASCII letters or digits.
pub fn branch_slug(text: &str, project: &str) -> Option<String> {
    fn words(text: &str) -> Vec<String> {
        text.split(|c: char| !c.is_ascii_alphanumeric())
            .filter(|word| !word.is_empty())
            .map(str::to_ascii_lowercase)
            .collect()
    }
    let mut words_left = words(text);
    let name = words(project);
    if !name.is_empty() {
        let mut kept = Vec::new();
        let mut at = 0;
        while at < words_left.len() {
            if words_left[at..].starts_with(&name) {
                at += name.len();
            } else {
                kept.push(words_left[at].clone());
                at += 1;
            }
        }
        if !kept.is_empty() {
            words_left = kept;
        }
    }
    let mut slug = String::new();
    for word in words_left {
        let needed = if slug.is_empty() {
            word.len()
        } else {
            word.len() + 1
        };
        if slug.len() + needed > SLUG_MAX {
            if slug.is_empty() {
                slug.push_str(&word[..SLUG_MAX]);
            }
            break;
        }
        if !slug.is_empty() {
            slug.push('-');
        }
        slug.push_str(&word);
    }
    (!slug.is_empty()).then_some(slug)
}

/// The branch prefix setting, made safe for git: path parts of ASCII
/// letters, digits, `.`, `_`, and `-`, joined by `/`. A prefix that does not
/// already end in `-` or `_` gets a `/`, so `dev` and `dev/` both give
/// `dev/`. Empty means no prefix.
pub fn normalize_prefix(raw: &str) -> String {
    let parts: Vec<String> = raw
        .split('/')
        .map(|part| {
            let mut part: String = part
                .chars()
                .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
                .collect();
            while part.contains("..") {
                part = part.replace("..", ".");
            }
            loop {
                let trimmed = part
                    .trim_start_matches(['.', '-'])
                    .trim_end_matches('.')
                    .trim_end_matches(".lock");
                if trimmed.len() == part.len() {
                    break part;
                }
                part = trimmed.to_string();
            }
        })
        .filter(|part| !part.is_empty())
        .collect();
    let mut prefix = parts.join("/");
    if !prefix.is_empty() && !prefix.ends_with(['-', '_']) {
        prefix.push('/');
    }
    prefix
}

/// Whether git accepts `name` as a branch name.
pub fn is_valid_branch(git: &Path, path_env: &str, worktree: &Path, name: &str) -> Result<bool> {
    let status = git_cmd(git, path_env, worktree)
        .args(["check-ref-format", "--branch", name])
        .output()
        .map_err(|_| Error::RenameBranch(None))?
        .status;
    Ok(status.success())
}

/// The branch checked out in `worktree`, or None on a detached HEAD.
pub fn head_branch(git: &Path, path_env: &str, worktree: &Path) -> Result<Option<String>> {
    let output = git_cmd(git, path_env, worktree)
        .args(["symbolic-ref", "--quiet", "--short", "HEAD"])
        .output()
        .map_err(|_| Error::GitStatus(None))?;
    if output.status.code() == Some(1) {
        return Ok(None);
    }
    if !output.status.success() {
        return Err(Error::GitStatus(first_line(&output.stderr)));
    }
    Ok(Some(
        String::from_utf8_lossy(&output.stdout).trim().to_string(),
    ))
}

/// What a project's checkout has checked out, for its checkout session's card
/// and the picker footer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckoutHead {
    /// A branch, including one with no commits yet.
    Branch(String),
    /// A detached HEAD, as its short commit id.
    Detached(String),
}

impl CheckoutHead {
    /// The branch name, or the short commit when detached.
    pub fn label(&self) -> &str {
        match self {
            Self::Branch(name) | Self::Detached(name) => name,
        }
    }
}

/// The checkout's HEAD: one git process, two when detached. A background
/// read bounded to `repo` like the diff reads: no optional locks, and a
/// folder whose `.git` is missing or that is not the top of its own checkout
/// is an error, never a parent repository.
pub fn checkout_head(git: &Path, path_env: &str, repo: &Path) -> Result<CheckoutHead> {
    check_worktree(git, path_env, repo).map_err(Error::GitStatus)?;
    let output = read_only_git(git, path_env, repo)
        .args(["symbolic-ref", "--quiet", "--short", "HEAD"])
        .output()
        .map_err(|_| Error::GitStatus(None))?;
    if output.status.success() {
        let name = String::from_utf8_lossy(&output.stdout).trim().to_string();
        return Ok(CheckoutHead::Branch(name));
    }
    if output.status.code() != Some(1) {
        return Err(Error::GitStatus(first_line(&output.stderr)));
    }
    let output = read_only_git(git, path_env, repo)
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .map_err(|_| Error::GitStatus(None))?;
    if !output.status.success() {
        return Err(Error::GitStatus(first_line(&output.stderr)));
    }
    Ok(CheckoutHead::Detached(
        String::from_utf8_lossy(&output.stdout).trim().to_string(),
    ))
}

/// The local remote-tracking ref of `branch` on origin. Git updates it on
/// every push from this worktree, so it is the cheap signal that a new
/// commit reached the PR.
pub fn pushed_head(
    git: &Path,
    path_env: &str,
    worktree: &Path,
    branch: &str,
) -> Result<Option<String>> {
    let output = git_cmd(git, path_env, worktree)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .args([
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/remotes/origin/{branch}^{{commit}}"),
        ])
        .output()
        .map_err(|_| Error::GitStatus(None))?;
    if output.status.code() == Some(1) {
        return Ok(None);
    }
    if !output.status.success() {
        return Err(Error::GitStatus(first_line(&output.stderr)));
    }
    let head = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok((!head.is_empty()).then_some(head))
}

/// Proves a rename chain from the recorded task branch to the current branch.
/// Missing refs or equal commits alone cannot distinguish a rename from a
/// branch switch. Git preserves explicit rename entries in the branch reflog.
/// Missing/expired history is a refusal, and a recreated old branch must keep
/// its protection even if the current branch once carried that name.
pub fn was_renamed(
    git: &Path,
    path_env: &str,
    worktree: &Path,
    recorded: &str,
    current: &str,
) -> Result<bool> {
    if ref_exists(git, path_env, worktree, &format!("refs/heads/{recorded}"))? {
        return Ok(false);
    }
    let output = read_only_git(git, path_env, worktree)
        .args(["reflog", "show", "-n", "256", "--format=%gs"])
        .arg(format!("refs/heads/{current}"))
        .output()
        .map_err(|_| Error::GitStatus(None))?;
    if !output.status.success() {
        return Ok(false);
    }
    let mut name = format!("refs/heads/{current}");
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        // `git branch -c` copies reflog history too. A copy is a new branch,
        // even when its older entries include the recorded task's name.
        if line.starts_with("Branch: copied ") {
            return Ok(false);
        }
        if let Some(rename) = line.strip_prefix("Branch: renamed ")
            && let Some((from, to)) = rename.split_once(" to ")
        {
            if to != name {
                return Ok(false);
            }
            if from == format!("refs/heads/{recorded}") {
                return Ok(true);
            }
            name = from.to_string();
        }
    }
    Ok(false)
}

/// Whether `branch` already exists on a remote: it has an upstream, or a
/// remote-tracking branch has its name, as after `git push origin HEAD`.
/// Renaming it then would leave the old name behind on the remote.
pub fn is_published(git: &Path, path_env: &str, worktree: &Path, branch: &str) -> Result<bool> {
    let upstream = git_cmd(git, path_env, worktree)
        .args(["rev-parse", "--abbrev-ref", "--symbolic-full-name"])
        .arg(format!("{branch}@{{upstream}}"))
        .output()
        .map_err(|_| Error::GitStatus(None))?;
    if upstream.status.success() {
        return Ok(true);
    }
    refs_exist(
        git,
        path_env,
        worktree,
        &[format!("refs/remotes/*/{branch}")],
    )
    .map_err(|_| Error::GitStatus(None))
}

fn refs_exist(git: &Path, path_env: &str, worktree: &Path, patterns: &[String]) -> Result<bool> {
    let output = git_cmd(git, path_env, worktree)
        .args(["for-each-ref", "--count=1", "--format=%(refname)"])
        .args(patterns)
        .output()
        .map_err(|_| Error::RenameBranch(None))?;
    if !output.status.success() {
        return Err(Error::RenameBranch(first_line(&output.stderr)));
    }
    Ok(!output.stdout.trim_ascii().is_empty())
}

/// Renames `current` to `name`, or to `name-2`, `name-3`, and so on when a
/// local branch or a remote-tracking branch already uses the name. Returns
/// the name the branch has afterwards. Only the local ref changes; nothing
/// is sent to a remote.
pub fn rename_branch(
    git: &Path,
    path_env: &str,
    worktree: &Path,
    current: &str,
    name: &str,
) -> Result<String> {
    let mut suffix = 1;
    loop {
        let branch = if suffix == 1 {
            name.to_string()
        } else {
            format!("{name}-{suffix}")
        };
        if branch == current {
            return Ok(branch);
        }
        let taken = refs_exist(
            git,
            path_env,
            worktree,
            &[
                format!("refs/heads/{branch}"),
                format!("refs/remotes/*/{branch}"),
            ],
        )?;
        if !taken {
            let output = git_cmd(git, path_env, worktree)
                .args(["branch", "-m", "--", current, &branch])
                .output()
                .map_err(|_| Error::RenameBranch(None))?;
            if output.status.success() {
                return Ok(branch);
            }
            return Err(Error::RenameBranch(first_line(&output.stderr)));
        }
        suffix += 1;
    }
}
