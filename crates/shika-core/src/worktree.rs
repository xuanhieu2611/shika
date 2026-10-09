use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fs;
use std::io::{self, BufReader, Read};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::{ChildStdout, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::diff::{self, FileDiff, FileKey, FileStatus, SessionDiff};
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Draft {
    pub branch: String,
    pub path: PathBuf,
}

/// One Shika worktree on disk. `worktrees.json` keeps these so a quit or a
/// crash can list what was left behind.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct JournalEntry {
    pub project_id: String,
    pub branch: String,
    pub path: PathBuf,
    /// The ref the branch started from, such as `refs/remotes/origin/dev`.
    /// Missing in entries written before it was recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_ref: Option<String>,
}

pub struct Journal {
    path: PathBuf,
    lock: Mutex<()>,
}

impl Journal {
    pub fn open(path: PathBuf) -> Self {
        Self {
            path,
            lock: Mutex::new(()),
        }
    }

    pub fn list(&self) -> Result<Vec<JournalEntry>> {
        let _guard = self.lock.lock().unwrap_or_else(|err| err.into_inner());
        load(&self.path)
    }

    pub fn add(&self, entry: &JournalEntry) -> Result<()> {
        let _guard = self.lock.lock().unwrap_or_else(|err| err.into_inner());
        let mut entries = load(&self.path)?;
        entries.push(entry.clone());
        save(&self.path, &entries)
    }

    pub fn rename_branch(&self, path: &Path, branch: &str) -> Result<()> {
        let _guard = self.lock.lock().unwrap_or_else(|err| err.into_inner());
        let mut entries = load(&self.path)?;
        let entry = entries
            .iter_mut()
            .find(|entry| entry.path == path)
            .ok_or(Error::ReadJournal)?;
        entry.branch = branch.to_string();
        save(&self.path, &entries)
    }

    pub fn remove_path(&self, path: &Path) -> Result<()> {
        let _guard = self.lock.lock().unwrap_or_else(|err| err.into_inner());
        let mut entries = load(&self.path)?;
        entries.retain(|entry| entry.path != path);
        save(&self.path, &entries)
    }
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

/// The branch on origin worth fetching before New: the configured base, or
/// the branch `origin/HEAD` names. None without an `origin` remote.
pub fn fetch_target(
    git: &Path,
    path_env: &str,
    repo: &Path,
    configured: Option<&str>,
) -> Option<String> {
    if !has_origin(git, path_env, repo) {
        return None;
    }
    if let Some(branch) = configured {
        return Some(branch.to_string());
    }
    let head = git_cmd(git, path_env, repo)
        .args(["symbolic-ref", "--quiet", "refs/remotes/origin/HEAD"])
        .output()
        .ok()?;
    if !head.status.success() {
        return None;
    }
    let reference = String::from_utf8_lossy(&head.stdout).trim().to_string();
    reference
        .strip_prefix("refs/remotes/origin/")
        .filter(|branch| !branch.is_empty())
        .map(str::to_string)
}

/// Whether the repository has a remote named `origin`.
pub fn has_origin(git: &Path, path_env: &str, repo: &Path) -> bool {
    git_cmd(git, path_env, repo)
        .args(["config", "--get", "remote.origin.url"])
        .output()
        .is_ok_and(|output| output.status.success())
}

/// How long New waits for a base branch fetch before using the ref it has.
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(4);

/// Best effort: updates `refs/remotes/origin/<branch>` from origin and
/// nothing else. It can never prompt: no terminal prompt, no askpass, ssh in
/// batch mode, no stdin. Gives up after `timeout`, killing the fetch and its
/// ssh. Returns whether the fetch succeeded; callers carry on either way.
pub fn fetch_branch(
    git: &Path,
    path_env: &str,
    repo: &Path,
    branch: &str,
    timeout: Duration,
) -> bool {
    // A valid branch name has no glob or other refspec syntax.
    if branch.is_empty()
        || branch.starts_with('-')
        || !is_valid_branch(git, path_env, repo, branch).unwrap_or(false)
    {
        return false;
    }
    let mut cmd = git_cmd(git, path_env, repo);
    // An explicit destination also works when the clone fetches only some
    // branches, as `git clone --single-branch` sets up.
    cmd.args([
        "fetch",
        "--quiet",
        "--no-tags",
        "--no-recurse-submodules",
        "origin",
    ])
    .arg(format!("+refs/heads/{branch}:refs/remotes/origin/{branch}"))
    .env("GIT_TERMINAL_PROMPT", "0")
    // Empty means no askpass helper at all, so git cannot ask for a password.
    .env("GIT_ASKPASS", "")
    .env("SSH_ASKPASS_REQUIRE", "never")
    .env("GCM_INTERACTIVE", "never")
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::null());
    if !user_ssh_command(git, path_env, repo) {
        cmd.env("GIT_SSH_COMMAND", "ssh -o BatchMode=yes");
    }
    // Its own process group, so a timeout can stop ssh along with git.
    std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
    let Ok(mut child) = cmd.spawn() else {
        return false;
    };
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            _ => break,
        }
    }
    let _ = Command::new("/bin/kill")
        .args(["-KILL", "--", &format!("-{}", child.id())])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let _ = child.kill();
    let _ = child.wait();
    false
}

/// Whether the user chose their own ssh command, which Shika leaves alone.
fn user_ssh_command(git: &Path, path_env: &str, repo: &Path) -> bool {
    if std::env::var_os("GIT_SSH_COMMAND").is_some() || std::env::var_os("GIT_SSH").is_some() {
        return true;
    }
    git_cmd(git, path_env, repo)
        .args(["config", "--get", "core.sshCommand"])
        .output()
        .is_ok_and(|output| output.status.success())
}

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

/// The ref a session's work is measured against: the recorded base while it
/// still resolves, else the default branch, with its safety refusal.
fn compare_base(
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

/// Whether `reference`, a full `refs/...` name, names a commit in `repo`.
fn ref_exists(git: &Path, path_env: &str, repo: &Path, reference: &str) -> Result<bool> {
    let output = git_cmd(git, path_env, repo)
        .args(["rev-parse", "--verify", "--quiet"])
        .arg(format!("{reference}^{{commit}}"))
        .output()
        .map_err(|_| Error::GitStatus(None))?;
    Ok(output.status.success())
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

/// What the task changed against where its branch left its base (the
/// recorded start ref, else the default branch):
/// committed and uncommitted tracked edits, plus untracked files that are not
/// ignored. Reads only; never stages anything or writes the index.
pub fn diff_stat(
    git: &Path,
    path_env: &str,
    repo: &Path,
    worktree: &Path,
    base: Option<&str>,
) -> Result<crate::DiffStat> {
    check_worktree(git, path_env, worktree).map_err(Error::GitStatus)?;
    let base = diff_base(git, path_env, repo, worktree, base);
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

/// The task's whole change for the Changes panel, measured exactly as
/// [`diff_stat`] measures it. After the base is found, two git processes:
/// the patch, streamed and parsed as it arrives, and the untracked list.
/// Reads only, and only this worktree: one whose `.git` is missing or broken
/// is an error, never the main checkout around it.
pub fn session_diff(
    git: &Path,
    path_env: &str,
    repo: &Path,
    worktree: &Path,
    base: Option<&str>,
) -> Result<SessionDiff> {
    check_worktree(git, path_env, worktree).map_err(Error::ReadChanges)?;
    let base = diff_base(git, path_env, repo, worktree, base);
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

/// A git call for background reads in a task worktree. Optional locks are
/// off so a refresh never takes the index lock out from under the user's own
/// git commands. Discovery stops at `worktree`: a task worktree sits at
/// `<repo>/.worktrees/<branch>`, so with its `.git` file missing, git would
/// otherwise walk up and read the main checkout as if it were the task.
/// `GIT_CEILING_DIRECTORIES` is the worktree's parent, which git resolves
/// through symlinks. Every caller passes a task worktree root, never the main
/// checkout; use [`git_cmd`] there.
fn read_only_git(program: &Path, path_env: &str, worktree: &Path) -> Command {
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
fn check_worktree(git: &Path, path_env: &str, worktree: &Path) -> Result<(), Option<String>> {
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

fn ensure_excluded(git: &Path, path_env: &str, repo: &Path) -> Result<()> {
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

fn exclude_file(git: &Path, path_env: &str, repo: &Path) -> Result<PathBuf> {
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

fn load(path: &Path) -> Result<Vec<JournalEntry>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = fs::read_to_string(path).map_err(|_| Error::ReadJournal)?;
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }
    serde_json::from_str(&text).map_err(|_| Error::ReadJournal)
}

fn save(path: &Path, entries: &[JournalEntry]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|_| Error::SaveJournal)?;
    }
    let mut json = serde_json::to_string_pretty(entries).map_err(|_| Error::SaveJournal)?;
    json.push('\n');
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, json).map_err(|_| Error::SaveJournal)?;
    fs::rename(&tmp, path).map_err(|_| Error::SaveJournal)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::Collapse;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    struct Scratch {
        path: PathBuf,
    }

    impl Scratch {
        fn new() -> Self {
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!("shika-worktree-{nanos}-{n}"));
            fs::create_dir_all(&path).unwrap();
            Self { path }
        }

        fn repo(&self, name: &str) -> PathBuf {
            let dir = self.path.join(name);
            fs::create_dir_all(&dir).unwrap();
            let status = Command::new("git")
                .arg("init")
                .current_dir(&dir)
                .status()
                .unwrap();
            assert!(status.success());
            let status = Command::new("git")
                .arg("-C")
                .arg(&dir)
                .args(["commit", "--allow-empty", "-m", "init"])
                .env("GIT_AUTHOR_NAME", "Shika")
                .env("GIT_AUTHOR_EMAIL", "shika@example.com")
                .env("GIT_COMMITTER_NAME", "Shika")
                .env("GIT_COMMITTER_EMAIL", "shika@example.com")
                .status()
                .unwrap();
            assert!(status.success(), "empty commit failed");
            dir
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn git() -> PathBuf {
        PathBuf::from("git")
    }

    fn porcelain(repo: &Path) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["status", "--porcelain"])
            .output()
            .unwrap();
        assert!(output.status.success());
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    fn git_ok(repo: &Path, args: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .env("GIT_AUTHOR_NAME", "Shika")
            .env("GIT_AUTHOR_EMAIL", "shika@example.com")
            .env("GIT_COMMITTER_NAME", "Shika")
            .env("GIT_COMMITTER_EMAIL", "shika@example.com")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn known_branches_lists_local_and_origin_once_and_skips_head() {
        let scratch = Scratch::new();
        let repo = scratch.path.join("demo");
        fs::create_dir_all(&repo).unwrap();
        git_ok(&repo, &["init", "-b", "main"]);
        git_ok(&repo, &["commit", "--allow-empty", "-m", "init"]);
        git_ok(&repo, &["branch", "staging"]);
        let remote = scratch.path.join("remote.git");
        fs::create_dir_all(&remote).unwrap();
        git_ok(&remote, &["init", "--bare"]);
        git_ok(
            &repo,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );
        git_ok(&repo, &["push", "origin", "main"]);
        git_ok(&repo, &["remote", "set-head", "origin", "main"]);
        git_ok(&repo, &["switch", "-c", "dev"]);
        git_ok(&repo, &["commit", "--allow-empty", "-m", "dev"]);
        git_ok(&repo, &["push", "origin", "dev"]);
        git_ok(&repo, &["switch", "main"]);
        git_ok(&repo, &["branch", "-D", "dev"]);

        let raw = Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["for-each-ref", "--format=%(refname)", "refs/remotes/origin"])
            .output()
            .unwrap();
        let raw = String::from_utf8_lossy(&raw.stdout);
        assert!(
            raw.contains("refs/remotes/origin/HEAD"),
            "origin/HEAD was not there to omit: {raw}"
        );

        let found = known_branches(&git(), "", &repo).unwrap();
        assert_eq!(found.names, vec!["dev", "main", "staging"]);
        assert_eq!(found.checked_out.as_deref(), Some("main"));
        assert!(found.has_origin);
        assert!(!found.names.iter().any(|name| name == "HEAD"));
    }

    #[test]
    fn head_branch_distinguishes_detached_head_from_git_failure() {
        let scratch = Scratch::new();
        let repo = scratch.repo("demo");
        assert!(head_branch(&git(), "", &repo).unwrap().is_some());
        assert!(
            Command::new("git")
                .current_dir(&repo)
                .args(["checkout", "--detach"])
                .status()
                .unwrap()
                .success()
        );
        assert_eq!(head_branch(&git(), "", &repo).unwrap(), None);
        assert!(matches!(
            head_branch(&git(), "", &scratch.path),
            Err(Error::GitStatus(Some(_)))
        ));
    }

    #[test]
    fn slugs_collapse_trim_and_stop_at_a_word() {
        assert_eq!(
            branch_slug("  Fix / Login__Flow!  ", "").as_deref(),
            Some("fix-login-flow")
        );
        assert_eq!(branch_slug("?!", ""), None);
        assert_eq!(branch_slug(&"A".repeat(80), ""), Some("a".repeat(48)));
        assert_eq!(
            branch_slug(&format!("{} xx", "A".repeat(47)), ""),
            Some("a".repeat(47))
        );
        assert_eq!(
            branch_slug(
                "i'm not sure if this is expected but i tried to use Shika",
                ""
            )
            .as_deref(),
            Some("i-m-not-sure-if-this-is-expected-but-i-tried-to")
        );
    }

    #[test]
    fn slugs_leave_out_the_project_name() {
        assert_eq!(
            branch_slug("Shika background opacity and blur", "shika").as_deref(),
            Some("background-opacity-and-blur")
        );
        assert_eq!(
            branch_slug("Fix Sample Project tracker", "sample-project").as_deref(),
            Some("fix-tracker")
        );
        // Only whole words, and never the whole name.
        assert_eq!(
            branch_slug("Shikari support", "shika").as_deref(),
            Some("shikari-support")
        );
        assert_eq!(branch_slug("Shika", "shika").as_deref(), Some("shika"));
        assert_eq!(
            branch_slug("Setting Placement Query", "shika").as_deref(),
            Some("setting-placement-query")
        );
    }

    #[test]
    fn prefixes_are_made_safe_for_git() {
        assert_eq!(normalize_prefix(""), "");
        assert_eq!(normalize_prefix("  "), "");
        assert_eq!(normalize_prefix("dev"), "dev/");
        assert_eq!(normalize_prefix("dev/"), "dev/");
        assert_eq!(normalize_prefix("/dev//feat/"), "dev/feat/");
        assert_eq!(normalize_prefix("dev-"), "dev-");
        assert_eq!(normalize_prefix("dev_"), "dev_");
        assert_eq!(normalize_prefix("De v~^:?*[\\"), "Dev/");
        assert_eq!(normalize_prefix("-.hidden/x..y/z.lock/"), "hidden/x.y/z/");
        assert_eq!(normalize_prefix("./../"), "");
        for raw in ["dev", "a.b", "-x", "x.lock", "Sample User/", "@{x"] {
            let prefix = normalize_prefix(raw);
            if !prefix.is_empty() {
                assert!(
                    is_valid_branch(&git(), "", Path::new("."), &format!("{prefix}fix")).unwrap(),
                    "{raw} gave {prefix}"
                );
            }
        }
    }

    #[test]
    fn rename_skips_names_taken_locally_or_on_a_remote() {
        let scratch = Scratch::new();
        let repo = scratch.repo("demo");
        let draft = create_draft(&git(), "", &repo, "one", "HEAD").unwrap();
        let run = |args: &[&str]| {
            let status = Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(args)
                .status()
                .unwrap();
            assert!(status.success(), "git {args:?}");
        };
        run(&["branch", "fix-login"]);
        run(&["update-ref", "refs/remotes/origin/fix-login-2", "HEAD"]);
        let renamed = rename_branch(&git(), "", &draft.path, &draft.branch, "fix-login").unwrap();
        assert_eq!(renamed, "fix-login-3");
        assert_eq!(
            head_branch(&git(), "", &draft.path).unwrap().as_deref(),
            Some("fix-login-3")
        );
        // Asking for the name it already has changes nothing.
        assert_eq!(
            rename_branch(&git(), "", &draft.path, &renamed, "fix-login-3").unwrap(),
            "fix-login-3"
        );
        assert!(!is_published(&git(), "", &draft.path, &renamed).unwrap());
        run(&["update-ref", "refs/remotes/origin/fix-login-3", "HEAD"]);
        assert!(is_published(&git(), "", &draft.path, &renamed).unwrap());

        let prefixed = rename_branch(&git(), "", &draft.path, &renamed, "dev/fix-login").unwrap();
        assert_eq!(prefixed, "dev/fix-login");
        assert!(!is_published(&git(), "", &draft.path, &prefixed).unwrap());
        remove_draft(&git(), "", &repo, &draft.path, &prefixed, true).unwrap();
    }

    #[test]
    fn exclude_is_written_once_and_hides_the_directory() {
        let scratch = Scratch::new();
        let repo = scratch.repo("demo");
        ensure_excluded(&git(), "", &repo).unwrap();
        ensure_excluded(&git(), "", &repo).unwrap();
        let exclude = exclude_file(&git(), "", &repo).unwrap();
        let text = fs::read_to_string(exclude).unwrap();
        assert_eq!(text.matches(".worktrees/").count(), 1);

        fs::create_dir_all(repo.join(".worktrees")).unwrap();
        fs::write(repo.join(".worktrees").join("note"), "hi").unwrap();
        assert!(
            !porcelain(&repo).contains(".worktrees"),
            "status was {}",
            porcelain(&repo)
        );
    }

    #[test]
    fn draft_worktree_stays_out_of_main_status_and_can_be_removed() {
        let scratch = Scratch::new();
        let repo = scratch.repo("demo");
        let draft = create_draft(&git(), "", &repo, "one", "HEAD").unwrap();

        assert_eq!(draft.branch, "shika-draft-one");
        assert!(draft.path.join(".git").exists());
        assert!(
            !porcelain(&repo).contains(".worktrees"),
            "status was {}",
            porcelain(&repo)
        );

        let listed = Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["worktree", "list", "--porcelain"])
            .output()
            .unwrap();
        let listed = String::from_utf8_lossy(&listed.stdout);
        assert!(listed.contains(&draft.path.display().to_string()));

        assert!(!is_dirty(&git(), "", &draft.path).unwrap());
        remove_draft(&git(), "", &repo, &draft.path, &draft.branch, false).unwrap();
        assert!(!draft.path.exists());
        let listed = Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["worktree", "list", "--porcelain"])
            .output()
            .unwrap();
        let listed = String::from_utf8_lossy(&listed.stdout);
        assert!(!listed.contains("shika-draft-one"));
        let branches = Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["branch", "--list", "shika-draft-one"])
            .output()
            .unwrap();
        assert!(String::from_utf8_lossy(&branches.stdout).trim().is_empty());
    }

    #[test]
    fn a_dirty_worktree_is_kept_until_removal_is_forced() {
        let scratch = Scratch::new();
        let repo = scratch.repo("demo");
        let draft = create_draft(&git(), "", &repo, "one", "HEAD").unwrap();
        fs::write(draft.path.join("note.txt"), "wip\n").unwrap();

        assert!(is_dirty(&git(), "", &draft.path).unwrap());
        let refused = remove_draft(&git(), "", &repo, &draft.path, &draft.branch, false);
        assert!(
            matches!(refused, Err(Error::WorktreeHasChanges(_))),
            "{refused:?}"
        );
        assert!(draft.path.exists());

        remove_draft(&git(), "", &repo, &draft.path, &draft.branch, true).unwrap();
        assert!(!draft.path.exists());
        let branches = Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["branch", "--list", "shika-draft-one"])
            .output()
            .unwrap();
        assert!(String::from_utf8_lossy(&branches.stdout).trim().is_empty());
    }

    fn run(dir: &Path, args: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_AUTHOR_NAME", "Shika")
            .env("GIT_AUTHOR_EMAIL", "shika@example.com")
            .env("GIT_COMMITTER_NAME", "Shika")
            .env("GIT_COMMITTER_EMAIL", "shika@example.com")
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?} failed");
    }

    fn commit_all(dir: &Path, message: &str) {
        run(dir, &["add", "-A"]);
        run(dir, &["commit", "-m", message]);
    }

    /// A repository with a three-line tracked file, and a draft made from it.
    fn repo_with_draft(scratch: &Scratch) -> (PathBuf, Draft) {
        let repo = scratch.repo("demo");
        fs::write(repo.join("a.txt"), "one\ntwo\nthree\n").unwrap();
        commit_all(&repo, "base");
        let draft = create_draft(&git(), "", &repo, "one", "HEAD").unwrap();
        (repo, draft)
    }

    fn stat(repo: &Path, draft: &Draft) -> crate::DiffStat {
        diff_stat(&git(), "", repo, &draft.path, None).unwrap()
    }

    fn counts(files: usize, insertions: usize, deletions: usize) -> crate::DiffStat {
        crate::DiffStat {
            files,
            insertions,
            deletions,
        }
    }

    #[test]
    fn a_clean_branch_changed_nothing() {
        let scratch = Scratch::new();
        let (repo, draft) = repo_with_draft(&scratch);
        assert_eq!(stat(&repo, &draft), crate::DiffStat::default());
    }

    #[test]
    fn committed_work_counts_from_where_the_branch_left() {
        let scratch = Scratch::new();
        let (repo, draft) = repo_with_draft(&scratch);
        fs::write(draft.path.join("a.txt"), "one\n2\nthree\nfour\n").unwrap();
        commit_all(&draft.path, "task");
        // Later work on the default branch is not the task's.
        fs::write(repo.join("main.txt"), "main\n").unwrap();
        commit_all(&repo, "main moves on");
        assert_eq!(stat(&repo, &draft), counts(1, 2, 1));
    }

    #[test]
    fn uncommitted_edits_count() {
        let scratch = Scratch::new();
        let (repo, draft) = repo_with_draft(&scratch);
        fs::write(draft.path.join("a.txt"), "one\ntwo\nthree\nfour\nfive\n").unwrap();
        assert_eq!(stat(&repo, &draft), counts(1, 2, 0));
        // Staged and committed work add up with what is still in the tree.
        run(&draft.path, &["add", "a.txt"]);
        fs::write(draft.path.join("b.txt"), "b\n").unwrap();
        commit_all(&draft.path, "task");
        fs::write(draft.path.join("b.txt"), "b\nc\n").unwrap();
        assert_eq!(stat(&repo, &draft), counts(2, 4, 0));
    }

    #[test]
    fn untracked_files_count_their_lines_without_touching_the_index() {
        let scratch = Scratch::new();
        let (repo, draft) = repo_with_draft(&scratch);
        fs::create_dir_all(draft.path.join("src")).unwrap();
        fs::write(draft.path.join("src/new.rs"), "fn a() {}\nfn b() {}\n").unwrap();
        fs::write(draft.path.join("tail.txt"), "no newline").unwrap();
        fs::write(draft.path.join("empty.txt"), "").unwrap();
        fs::write(draft.path.join(".gitignore"), "*.log\n").unwrap();
        fs::write(draft.path.join("build.log"), "ignored\n").unwrap();
        assert_eq!(stat(&repo, &draft), counts(4, 4, 0));
        let status = porcelain(&draft.path);
        assert!(status.contains("?? src/"), "{status}");
        assert!(!status.contains("A "), "{status}");
    }

    #[test]
    fn binary_files_count_as_files_without_lines() {
        let scratch = Scratch::new();
        let (repo, draft) = repo_with_draft(&scratch);
        fs::write(draft.path.join("logo.png"), [0x89, b'P', 0, 0, b'\n', 1]).unwrap();
        commit_all(&draft.path, "binary");
        fs::write(draft.path.join("loose.bin"), [1, 0, b'\n', b'\n']).unwrap();
        assert_eq!(stat(&repo, &draft), counts(2, 0, 0));
    }

    #[test]
    fn deletions_count() {
        let scratch = Scratch::new();
        let (repo, draft) = repo_with_draft(&scratch);
        fs::remove_file(draft.path.join("a.txt")).unwrap();
        assert_eq!(stat(&repo, &draft), counts(1, 0, 3));
        commit_all(&draft.path, "delete");
        assert_eq!(stat(&repo, &draft), counts(1, 0, 3));
    }

    #[test]
    fn without_a_default_branch_only_uncommitted_work_counts() {
        let scratch = Scratch::new();
        let (repo, draft) = repo_with_draft(&scratch);
        fs::write(draft.path.join("b.txt"), "b\n").unwrap();
        commit_all(&draft.path, "task");
        // The main checkout on the task branch hides the default branch.
        run(&repo, &["branch", "-m", "trunk"]);
        run(
            &repo,
            &["checkout", "--ignore-other-worktrees", &draft.branch],
        );
        assert_eq!(stat(&repo, &draft), crate::DiffStat::default());
        fs::write(draft.path.join("b.txt"), "b\nc\n").unwrap();
        assert_eq!(stat(&repo, &draft), counts(1, 1, 0));
    }

    #[test]
    fn journal_records_and_forgets_a_draft() {
        let scratch = Scratch::new();
        let journal = Journal::open(scratch.path.join("worktrees.json"));
        let entry = JournalEntry {
            project_id: "p1".into(),
            branch: "shika-draft-one".into(),
            path: "/tmp/demo/.worktrees/shika-draft-one".into(),
            base_ref: Some("refs/remotes/origin/dev".into()),
        };
        journal.add(&entry).unwrap();
        let again = Journal::open(scratch.path.join("worktrees.json"));
        again
            .add(&JournalEntry {
                project_id: "p1".into(),
                branch: "shika-draft-two".into(),
                path: "/tmp/demo/.worktrees/shika-draft-two".into(),
                base_ref: None,
            })
            .unwrap();
        again.remove_path(&entry.path).unwrap();
        let text = fs::read_to_string(scratch.path.join("worktrees.json")).unwrap();
        assert!(!text.contains("shika-draft-one"));
        assert!(text.contains("shika-draft-two"));
        assert!(text.contains("\"projectId\": \"p1\""), "{text}");
        // No base recorded leaves the field out, as in older files.
        assert!(!text.contains("baseRef"), "{text}");
        assert_eq!(again.list().unwrap().len(), 1);
        again.add(&entry).unwrap();
        let text = fs::read_to_string(scratch.path.join("worktrees.json")).unwrap();
        assert!(
            text.contains("\"baseRef\": \"refs/remotes/origin/dev\""),
            "{text}"
        );
        assert_eq!(again.list().unwrap()[1], entry);
    }

    fn changes(repo: &Path, draft: &Draft) -> SessionDiff {
        session_diff(&git(), "", repo, &draft.path, None).unwrap()
    }

    fn file<'a>(diff: &'a SessionDiff, path: &str) -> &'a FileDiff {
        diff.files
            .iter()
            .find(|file| file.path == path)
            .unwrap_or_else(|| panic!("{path} not in {:?}", paths(diff)))
    }

    fn paths(diff: &SessionDiff) -> Vec<&str> {
        diff.files.iter().map(|file| file.path.as_str()).collect()
    }

    /// Each line as `kind old new text`, for compact assertions.
    fn rows(file: &FileDiff) -> Vec<String> {
        file.hunks
            .iter()
            .flat_map(|hunk| &hunk.lines)
            .map(|line| {
                let no = |n: Option<u32>| n.map_or("-".to_string(), |n| n.to_string());
                let kind = match line.kind {
                    diff::LineKind::Context => ' ',
                    diff::LineKind::Added => '+',
                    diff::LineKind::Removed => '-',
                    diff::LineKind::NoNewline => '\\',
                };
                format!("{kind} {} {} {}", no(line.old), no(line.new), line.text)
            })
            .collect()
    }

    fn numbered(count: usize, tag: &str) -> String {
        (0..count).map(|n| format!("{tag} {n}\n")).collect()
    }

    #[test]
    fn changes_of_a_clean_task_are_empty() {
        let scratch = Scratch::new();
        let (repo, draft) = repo_with_draft(&scratch);
        assert_eq!(changes(&repo, &draft), SessionDiff::default());
    }

    #[test]
    fn changes_show_edits_with_line_numbers() {
        let scratch = Scratch::new();
        let (repo, draft) = repo_with_draft(&scratch);
        fs::write(draft.path.join("a.txt"), "one\n2\nthree\nfour\n").unwrap();
        let diff = changes(&repo, &draft);
        let a = file(&diff, "a.txt");
        assert_eq!(a.status, FileStatus::Modified);
        assert_eq!(a.status.letter(), 'M');
        assert_eq!((a.insertions, a.deletions), (2, 1));
        assert_eq!(a.hunks.len(), 1);
        assert_eq!(a.hunks[0].header, "@@ -1,3 +1,4 @@");
        assert_eq!(
            rows(a),
            [
                "  1 1 one",
                "- 2 - two",
                "+ - 2 2",
                "  3 3 three",
                "+ - 4 four"
            ]
        );
        assert!(a.collapsed.is_none() && a.hidden_lines == 0 && a.old_path.is_none());
    }

    #[test]
    fn changes_count_commits_and_edits_from_where_the_branch_left() {
        let scratch = Scratch::new();
        let (repo, draft) = repo_with_draft(&scratch);
        fs::write(draft.path.join("a.txt"), "one\ntwo\nthree\nfour\n").unwrap();
        fs::write(draft.path.join("added.txt"), "new\n").unwrap();
        commit_all(&draft.path, "task");
        fs::write(draft.path.join("added.txt"), "new\nmore\n").unwrap();
        // Later work on the default branch is not the task's.
        fs::write(repo.join("main.txt"), "main\n").unwrap();
        commit_all(&repo, "main moves on");
        let diff = changes(&repo, &draft);
        assert_eq!(paths(&diff), ["a.txt", "added.txt"]);
        let added = file(&diff, "added.txt");
        assert_eq!(added.status, FileStatus::Added);
        assert_eq!(rows(added), ["+ - 1 new", "+ - 2 more"]);
        assert_eq!(diff.stat, counts(2, 3, 0));
        assert_eq!(diff.stat, stat(&repo, &draft));
    }

    #[test]
    fn deleted_files_show_their_old_lines() {
        let scratch = Scratch::new();
        let (repo, draft) = repo_with_draft(&scratch);
        fs::remove_file(draft.path.join("a.txt")).unwrap();
        let diff = changes(&repo, &draft);
        let a = file(&diff, "a.txt");
        assert_eq!(a.status, FileStatus::Deleted);
        assert_eq!(rows(a), ["- 1 - one", "- 2 - two", "- 3 - three"]);
        assert_eq!(diff.stat, counts(1, 0, 3));
    }

    #[test]
    fn renames_with_and_without_edits() {
        let scratch = Scratch::new();
        let repo = scratch.repo("demo");
        let body = numbered(10, "line");
        fs::write(repo.join("same.txt"), &body).unwrap();
        fs::write(repo.join("edit.txt"), numbered(10, "other")).unwrap();
        commit_all(&repo, "base");
        let draft = create_draft(&git(), "", &repo, "one", "HEAD").unwrap();
        run(&draft.path, &["mv", "same.txt", "moved.txt"]);
        run(&draft.path, &["mv", "edit.txt", "edited.txt"]);
        let edited = numbered(10, "other").replace("other 5", "changed");
        fs::write(draft.path.join("edited.txt"), edited).unwrap();
        let diff = changes(&repo, &draft);
        let moved = file(&diff, "moved.txt");
        assert_eq!(moved.status, FileStatus::Renamed);
        assert_eq!(moved.old_path.as_deref(), Some("same.txt"));
        assert!(moved.hunks.is_empty() && moved.collapsed.is_none());
        let edited = file(&diff, "edited.txt");
        assert_eq!(edited.status.letter(), 'R');
        assert_eq!(edited.old_path.as_deref(), Some("edit.txt"));
        assert_eq!((edited.insertions, edited.deletions), (1, 1));
        assert!(rows(edited).contains(&"+ - 6 changed".to_string()));
        assert_eq!(diff.stat, stat(&repo, &draft));
    }

    #[test]
    fn binary_mode_only_and_empty_files_have_no_lines() {
        let scratch = Scratch::new();
        let repo = scratch.repo("demo");
        fs::write(repo.join("run.sh"), "echo hi\n").unwrap();
        fs::write(repo.join("logo.png"), [0x89, b'P', 0, 1, b'\n']).unwrap();
        commit_all(&repo, "base");
        let draft = create_draft(&git(), "", &repo, "one", "HEAD").unwrap();
        run(&draft.path, &["update-index", "--chmod=+x", "run.sh"]);
        fs::set_permissions(
            draft.path.join("run.sh"),
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .unwrap();
        fs::write(draft.path.join("logo.png"), [0x89, b'P', 0, 2, b'\n']).unwrap();
        fs::write(draft.path.join("empty.txt"), "").unwrap();
        run(&draft.path, &["add", "empty.txt"]);
        let diff = changes(&repo, &draft);
        let mode = file(&diff, "run.sh");
        assert_eq!(
            mode.mode_change,
            Some(diff::ModeChange {
                old: "100644".into(),
                new: "100755".into()
            })
        );
        assert!(mode.hunks.is_empty() && !mode.binary);
        let logo = file(&diff, "logo.png");
        assert!(logo.binary && logo.hunks.is_empty() && logo.collapsed.is_none());
        assert_eq!((logo.insertions, logo.deletions), (0, 0));
        let empty = file(&diff, "empty.txt");
        assert_eq!(empty.status, FileStatus::Added);
        assert!(empty.hunks.is_empty() && empty.collapsed.is_none());
        assert_eq!(diff.stat, counts(3, 0, 0));
        assert_eq!(diff.stat, stat(&repo, &draft));
    }

    #[test]
    fn untracked_files_show_as_added_without_following_symlinks() {
        let scratch = Scratch::new();
        let (repo, draft) = repo_with_draft(&scratch);
        fs::write(draft.path.join("new.txt"), "a\nb").unwrap();
        fs::write(draft.path.join("blob.bin"), [1, 0, b'\n', b'\n']).unwrap();
        fs::write(draft.path.join("empty.txt"), "").unwrap();
        std::os::unix::fs::symlink("/etc/hosts", draft.path.join("link")).unwrap();
        fs::write(draft.path.join(".gitignore"), "*.log\n").unwrap();
        fs::write(draft.path.join("build.log"), "ignored\n").unwrap();
        let diff = changes(&repo, &draft);
        assert_eq!(
            paths(&diff),
            [".gitignore", "blob.bin", "empty.txt", "link", "new.txt"]
        );
        assert!(diff.files.iter().all(|f| f.status == FileStatus::Untracked));
        let new = file(&diff, "new.txt");
        assert_eq!(new.hunks[0].header, "@@ -0,0 +1,2 @@");
        assert_eq!(
            rows(new),
            ["+ - 1 a", "+ - 2 b", "\\ - - \\ No newline at end of file"]
        );
        let blob = file(&diff, "blob.bin");
        assert!(blob.binary && blob.hunks.is_empty());
        assert!(file(&diff, "empty.txt").hunks.is_empty());
        let link = file(&diff, "link");
        assert_eq!(link.hunks[0].header, "@@ -0,0 +1 @@");
        assert_eq!(rows(link)[0], "+ - 1 /etc/hosts");
        assert_eq!(diff.stat, counts(5, 4, 0));
        assert_eq!(diff.stat, stat(&repo, &draft));
        // Reading the diff stages nothing.
        assert!(porcelain(&draft.path).contains("?? new.txt"));
    }

    #[test]
    fn line_endings_and_invalid_utf8_come_through() {
        let scratch = Scratch::new();
        let repo = scratch.repo("demo");
        fs::write(repo.join("tail.txt"), "a\nb\n").unwrap();
        fs::write(repo.join("crlf.txt"), "x\r\ny\r\n").unwrap();
        commit_all(&repo, "base");
        let draft = create_draft(&git(), "", &repo, "one", "HEAD").unwrap();
        fs::write(draft.path.join("tail.txt"), "a\nb").unwrap();
        fs::write(draft.path.join("crlf.txt"), "x\r\nY\r\n").unwrap();
        fs::write(draft.path.join("latin1.txt"), b"caf\xe9\n").unwrap();
        run(&draft.path, &["add", "latin1.txt"]);
        let diff = changes(&repo, &draft);
        assert_eq!(
            rows(file(&diff, "tail.txt")),
            [
                "  1 1 a",
                "- 2 - b",
                "+ - 2 b",
                "\\ - - \\ No newline at end of file"
            ]
        );
        assert_eq!(
            rows(file(&diff, "crlf.txt")),
            ["  1 1 x\r", "- 2 - y\r", "+ - 2 Y\r"]
        );
        assert_eq!(rows(file(&diff, "latin1.txt")), ["+ - 1 caf\u{fffd}"]);
        assert_eq!(diff.stat, stat(&repo, &draft));
    }

    #[test]
    fn a_typechange_is_two_entries_and_one_file() {
        let scratch = Scratch::new();
        let (repo, draft) = repo_with_draft(&scratch);
        fs::remove_file(draft.path.join("a.txt")).unwrap();
        std::os::unix::fs::symlink("target", draft.path.join("a.txt")).unwrap();
        let diff = changes(&repo, &draft);
        let statuses: Vec<FileStatus> = diff.files.iter().map(|f| f.status).collect();
        assert_eq!(statuses, [FileStatus::Deleted, FileStatus::Added]);
        assert_eq!(diff.stat, counts(1, 1, 3));
        assert_eq!(diff.stat, stat(&repo, &draft));
        let added = file_diff(&git(), "", &draft.path, &diff.files[1].key)
            .unwrap()
            .unwrap();
        assert_eq!(added.status, FileStatus::Added);
    }

    #[test]
    fn unusual_paths_round_trip() {
        let scratch = Scratch::new();
        let repo = scratch.repo("demo");
        let names = [
            "sp ace.txt",
            "dir b/x b/y.txt",
            "ünï cødé.txt",
            "q\"uote.txt",
            "tab\there.txt",
            "new\nline.txt",
            "star*.txt",
            "back\\slash.txt",
        ];
        fs::create_dir_all(repo.join("dir b/x b")).unwrap();
        for name in names {
            fs::write(repo.join(name), "one\n").unwrap();
        }
        fs::write(repo.join("star.txt"), "one\n").unwrap();
        fs::write(repo.join("m b x"), "m\n").unwrap();
        fs::write(repo.join("old b x.txt"), numbered(5, "r")).unwrap();
        commit_all(&repo, "base");
        let draft = create_draft(&git(), "", &repo, "one", "HEAD").unwrap();
        for name in names {
            fs::write(draft.path.join(name), "one\ntwo\n").unwrap();
        }
        fs::set_permissions(
            draft.path.join("m b x"),
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .unwrap();
        for dir in ["new b", "bin b", "untracked ü b"] {
            fs::create_dir_all(draft.path.join(dir)).unwrap();
        }
        run(&draft.path, &["mv", "old b x.txt", "new b/ü x.txt"]);
        fs::write(draft.path.join("bin b/ary.bin"), [0, 1]).unwrap();
        run(&draft.path, &["add", "bin b/ary.bin"]);
        fs::write(draft.path.join("untracked ü b/c.txt"), "u\n").unwrap();
        let diff = changes(&repo, &draft);
        for name in names {
            let changed = file(&diff, name);
            assert_eq!(changed.status, FileStatus::Modified, "{name}");
            assert_eq!(rows(changed), ["  1 1 one", "+ - 2 two"], "{name}");
        }
        assert!(file(&diff, "m b x").mode_change.is_some());
        assert_eq!(
            file(&diff, "new b/ü x.txt").old_path.as_deref(),
            Some("old b x.txt")
        );
        assert!(file(&diff, "bin b/ary.bin").binary);
        assert_eq!(rows(file(&diff, "untracked ü b/c.txt")), ["+ - 1 u"]);
        assert_eq!(diff.stat, stat(&repo, &draft));
        // Expanding reads the same file back by its exact bytes, and a `*`
        // matches only itself.
        for changed in &diff.files {
            let again = file_diff(&git(), "", &draft.path, &changed.key)
                .unwrap()
                .unwrap_or_else(|| panic!("{} gone", changed.path));
            assert_eq!(&again, changed);
        }
    }

    #[test]
    fn a_file_over_the_line_cap_collapses_and_expands() {
        let scratch = Scratch::new();
        let (repo, draft) = repo_with_draft(&scratch);
        fs::write(draft.path.join("big.txt"), numbered(2_001, "big")).unwrap();
        fs::write(draft.path.join("small.txt"), "s\n").unwrap();
        commit_all(&draft.path, "task");
        // An untracked file over 1 MiB is not read at all.
        let huge = "y".repeat(1023) + "\n";
        fs::write(draft.path.join("huge.log.txt"), huge.repeat(1100)).unwrap();
        let diff = changes(&repo, &draft);
        let big = file(&diff, "big.txt");
        assert_eq!(big.collapsed, Some(Collapse::Lines));
        assert!(big.hunks.is_empty());
        assert_eq!((big.insertions, big.hidden_lines), (2_001, 2_001));
        let huge_file = file(&diff, "huge.log.txt");
        assert_eq!(huge_file.collapsed, Some(Collapse::Size(1100 * 1024)));
        assert_eq!(huge_file.hidden_lines, 1_100);
        // A file over its own cap leaves the others alone.
        assert_eq!(file(&diff, "small.txt").collapsed, None);
        assert_eq!(diff.stat, stat(&repo, &draft));

        let expanded = file_diff(&git(), "", &draft.path, &big.key)
            .unwrap()
            .unwrap();
        assert_eq!(expanded.collapsed, None);
        assert_eq!((expanded.hidden_lines, rows(&expanded).len()), (0, 2_001));
        assert_eq!(rows(&expanded)[2_000], "+ - 2001 big 2000");
        let expanded = file_diff(&git(), "", &draft.path, &huge_file.key)
            .unwrap()
            .unwrap();
        assert_eq!((expanded.hidden_lines, rows(&expanded).len()), (0, 1_100));
        // A file that no longer differs has nothing to expand.
        run(&draft.path, &["reset", "--hard", "HEAD~1"]);
        fs::remove_file(draft.path.join("huge.log.txt")).unwrap();
        assert_eq!(file_diff(&git(), "", &draft.path, &big.key).unwrap(), None);
        assert_eq!(
            file_diff(&git(), "", &draft.path, &huge_file.key).unwrap(),
            None
        );
    }

    #[test]
    fn the_total_cap_collapses_every_file_past_it() {
        let scratch = Scratch::new();
        let (repo, draft) = repo_with_draft(&scratch);
        // 26 files of 1,990 lines pass 50,000 lines in the 26th.
        for n in 0..26 {
            fs::write(
                draft.path.join(format!("f{n:02}.txt")),
                numbered(1_990, "x"),
            )
            .unwrap();
        }
        fs::write(draft.path.join("y.txt"), numbered(2_001, "y")).unwrap();
        fs::write(draft.path.join("z.txt"), "z\n").unwrap();
        commit_all(&draft.path, "task");
        let diff = changes(&repo, &draft);
        let collapsed: Vec<_> = diff.files.iter().map(|f| f.collapsed).collect();
        // f00 to f24 fit; f25 and z.txt are past the cap. y.txt is past it
        // too, but over its own line cap, which names it.
        let mut expected = vec![None; 25];
        expected.push(Some(Collapse::Budget));
        expected.push(Some(Collapse::Lines));
        expected.push(Some(Collapse::Budget));
        assert_eq!(collapsed, expected);
        let small = file(&diff, "z.txt");
        assert_eq!(small.hidden_lines, 1);
        assert_eq!(diff.stat, counts(28, 26 * 1_990 + 2_001 + 1, 0));
        assert_eq!(diff.stat, stat(&repo, &draft));
        // A file past the budget expands like a large one.
        let expanded = file_diff(&git(), "", &draft.path, &small.key)
            .unwrap()
            .unwrap();
        assert_eq!(expanded.collapsed, None);
        assert_eq!(rows(&expanded), ["+ - 1 z"]);
    }

    #[test]
    fn expand_stops_at_the_hard_limit() {
        let scratch = Scratch::new();
        let (repo, draft) = repo_with_draft(&scratch);
        fs::write(draft.path.join("gen.txt"), numbered(120_000, "g")).unwrap();
        commit_all(&draft.path, "task");
        let diff = changes(&repo, &draft);
        let generated = file(&diff, "gen.txt");
        assert_eq!(generated.collapsed, Some(Collapse::Lines));
        let expanded = file_diff(&git(), "", &draft.path, &generated.key)
            .unwrap()
            .unwrap();
        assert_eq!(expanded.collapsed, None);
        assert_eq!(rows(&expanded).len(), 100_000);
        assert_eq!(expanded.hidden_lines, 20_000);
        assert_eq!(expanded.insertions, 120_000);
    }

    #[test]
    fn the_diff_stat_agrees_on_a_mixed_tree() {
        let scratch = Scratch::new();
        let repo = scratch.repo("demo");
        fs::write(repo.join("keep.txt"), numbered(50, "k")).unwrap();
        fs::write(repo.join("gone.txt"), numbered(5, "g")).unwrap();
        fs::write(repo.join("move.txt"), numbered(20, "m")).unwrap();
        fs::write(repo.join("swap"), "s\n").unwrap();
        commit_all(&repo, "base");
        let draft = create_draft(&git(), "", &repo, "one", "HEAD").unwrap();
        fs::write(draft.path.join("keep.txt"), numbered(60, "K")).unwrap();
        commit_all(&draft.path, "task");
        fs::remove_file(draft.path.join("gone.txt")).unwrap();
        run(&draft.path, &["mv", "move.txt", "moved.txt"]);
        fs::remove_file(draft.path.join("swap")).unwrap();
        std::os::unix::fs::symlink("keep.txt", draft.path.join("swap")).unwrap();
        fs::write(draft.path.join("big.txt"), numbered(3_000, "b")).unwrap();
        run(&draft.path, &["add", "big.txt"]);
        fs::write(draft.path.join("loose.txt"), "l\nm").unwrap();
        fs::write(draft.path.join("loose.bin"), [0, 0]).unwrap();
        let diff = changes(&repo, &draft);
        assert_eq!(diff.stat, stat(&repo, &draft));
        let sum = |f: fn(&FileDiff) -> usize| diff.files.iter().map(f).sum::<usize>();
        assert_eq!(sum(|f| f.insertions), diff.stat.insertions);
        assert_eq!(sum(|f| f.deletions), diff.stat.deletions);
    }

    /// The reads that must fail, not fall through to the main checkout, when
    /// a task worktree has lost its `.git` file.
    fn assert_reads_fail(repo: &Path, draft: &Draft, key: &FileKey) {
        let stat = diff_stat(&git(), "", repo, &draft.path, None);
        assert!(matches!(stat, Err(Error::GitStatus(_))), "{stat:?}");
        let changes = session_diff(&git(), "", repo, &draft.path, None);
        assert!(matches!(changes, Err(Error::ReadChanges(_))), "{changes:?}");
        let expanded = file_diff(&git(), "", &draft.path, key);
        assert!(
            matches!(expanded, Err(Error::ReadChanges(_))),
            "{expanded:?}"
        );
    }

    #[test]
    fn a_worktree_without_its_git_file_never_reads_the_main_checkout() {
        let scratch = Scratch::new();
        let (repo, draft) = repo_with_draft(&scratch);
        fs::write(draft.path.join("a.txt"), "one\n").unwrap();
        let key = file(&changes(&repo, &draft), "a.txt").key.clone();
        // The main checkout around `.worktrees/` is clean, so reading it
        // instead would look like "No changes".
        fs::remove_file(draft.path.join(".git")).unwrap();
        assert_reads_fail(&repo, &draft, &key);
        let changes = session_diff(&git(), "", &repo, &draft.path, None).unwrap_err();
        assert!(
            changes.to_string().contains("not a git repository"),
            "{changes}"
        );
        // A broken `.git` file fails in git itself.
        fs::write(draft.path.join(".git"), "gitdir: /nonexistent/dir\n").unwrap();
        assert_reads_fail(&repo, &draft, &key);
    }

    #[test]
    fn a_colon_in_the_path_still_stops_at_the_worktree() {
        // Git splits `GIT_CEILING_DIRECTORIES` at `:`, so this ceiling is
        // wrong; the top-level check still catches the main checkout.
        let scratch = Scratch::new();
        let repo = scratch.repo("a:b");
        fs::write(repo.join("a.txt"), "one\n").unwrap();
        commit_all(&repo, "base");
        let draft = create_draft(&git(), "", &repo, "one", "HEAD").unwrap();
        fs::write(draft.path.join("a.txt"), "two\n").unwrap();
        assert_eq!(stat(&repo, &draft), counts(1, 1, 1));
        let key = file(&changes(&repo, &draft), "a.txt").key.clone();
        fs::remove_file(draft.path.join(".git")).unwrap();
        assert_reads_fail(&repo, &draft, &key);
    }

    #[test]
    fn a_worktree_reached_through_a_symlink_still_reads() {
        let scratch = Scratch::new();
        let (repo, draft) = repo_with_draft(&scratch);
        fs::write(draft.path.join("a.txt"), "one\n").unwrap();
        let link = scratch.path.join("link");
        std::os::unix::fs::symlink(&draft.path, &link).unwrap();
        let linked = Draft {
            branch: draft.branch.clone(),
            path: link,
        };
        assert_eq!(stat(&repo, &linked), counts(1, 0, 2));
        assert_eq!(changes(&repo, &linked).stat, counts(1, 0, 2));
    }

    /// Timing on a large change: 200 files with 150 lines replaced in each,
    /// 60,000 changed lines. Run with
    /// `cargo test -p shika-core --release large_diff_timing -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn large_diff_timing() {
        let scratch = Scratch::new();
        let repo = scratch.repo("demo");
        for n in 0..200 {
            fs::write(
                repo.join(format!("f{n:03}.rs")),
                numbered(300, "let base ="),
            )
            .unwrap();
        }
        commit_all(&repo, "base");
        let draft = create_draft(&git(), "", &repo, "one", "HEAD").unwrap();
        for n in 0..200 {
            let text: String = (0..300)
                .map(|i| {
                    if i % 2 == 0 {
                        format!("let task = {i};\n")
                    } else {
                        format!("let base = {i}\n")
                    }
                })
                .collect();
            fs::write(draft.path.join(format!("f{n:03}.rs")), text).unwrap();
        }
        let mut best = (Duration::MAX, Duration::MAX);
        let mut collapsed = 0;
        for _ in 0..5 {
            let started = Instant::now();
            let diff = changes(&repo, &draft);
            let full = started.elapsed();
            let started = Instant::now();
            let numstat = stat(&repo, &draft);
            let quick = started.elapsed();
            assert_eq!(diff.stat, numstat);
            assert_eq!(diff.stat.insertions + diff.stat.deletions, 60_000);
            best = (best.0.min(full), best.1.min(quick));
            collapsed = diff.files.iter().filter(|f| f.collapsed.is_some()).count();
        }
        eprintln!(
            "session_diff {:?}, diff_stat {:?}, {collapsed} of 200 files past the total cap",
            best.0, best.1
        );
    }
}
