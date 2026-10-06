use std::ffi::OsStr;
use std::fs;
use std::io::{ErrorKind, Read};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

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
    let base = diff_base(git, path_env, repo, worktree, base);
    // Comparing the base to the working tree covers commits and edits at once.
    let output = read_only_git(git, path_env, worktree)
        .args([
            "diff",
            "--numstat",
            "--no-color",
            "--no-ext-diff",
            "--find-renames",
            &base,
            "--",
        ])
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
    let output = read_only_git(git, path_env, worktree)
        .args(["ls-files", "--others", "--exclude-standard", "-z"])
        .output()
        .map_err(|_| Error::GitStatus(None))?;
    if !output.status.success() {
        return Err(Error::GitStatus(first_line(&output.stderr)));
    }
    for path in output.stdout.split(|byte| *byte == 0) {
        if path.is_empty() {
            continue;
        }
        stat.files += 1;
        stat.insertions += untracked_lines(&worktree.join(OsStr::from_bytes(path)));
    }
    Ok(stat)
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

/// Lines in an untracked file, counted as `git diff` would count them for a
/// new file. Binary, unreadable, and non-file entries count none; a symlink is
/// one line, its target.
fn untracked_lines(path: &Path) -> usize {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return 0;
    };
    if meta.file_type().is_symlink() {
        return 1;
    }
    if !meta.is_file() {
        return 0;
    }
    let Ok(mut file) = fs::File::open(path) else {
        return 0;
    };
    // Git calls a file binary when its first 8000 bytes hold a NUL.
    const BINARY_PROBE: usize = 8000;
    let mut buf = vec![0; 64 * 1024];
    let mut probed = 0;
    let mut lines = 0;
    let mut last = b'\n';
    loop {
        let n = match file.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(err) if err.kind() == ErrorKind::Interrupted => continue,
            Err(_) => return 0,
        };
        let chunk = &buf[..n];
        if probed < BINARY_PROBE {
            let take = (BINARY_PROBE - probed).min(n);
            if chunk[..take].contains(&0) {
                return 0;
            }
            probed += take;
        }
        lines += chunk.iter().filter(|byte| **byte == b'\n').count();
        last = chunk[n - 1];
    }
    // A last line without a newline still counts.
    lines + usize::from(last != b'\n')
}

/// A git call for background reads. Optional locks are off so a refresh never
/// takes the index lock out from under the user's own git commands.
fn read_only_git(program: &Path, path_env: &str, dir: &Path) -> Command {
    let mut cmd = git_cmd(program, path_env, dir);
    cmd.env("GIT_OPTIONAL_LOCKS", "0");
    cmd
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
}
