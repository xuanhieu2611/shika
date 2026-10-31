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
    /// Empty for a Lead's detached worktree, which has no branch.
    pub branch: String,
    pub path: PathBuf,
    /// The ref the branch started from, such as `refs/remotes/origin/dev`.
    /// Missing in entries written before it was recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_ref: Option<String>,
    /// A Lead started this worker, so a later Lead start can report it when
    /// its session did not survive. Missing in older entries means false.
    #[serde(default, skip_serializing_if = "is_false")]
    pub started_by_lead: bool,
    /// Small recovery hint, never terminal output or conversation contents.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery: Option<RecoveryMetadata>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryMetadata {
    pub cli: String,
    pub started_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

fn is_false(value: &bool) -> bool {
    !value
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

    /// Points the entry for `path` at another project. A path with no entry
    /// is left alone. Atomic like every journal write.
    pub fn set_project(&self, path: &Path, project_id: &str) -> Result<()> {
        let _guard = self.lock.lock().unwrap_or_else(|err| err.into_inner());
        let mut entries = load(&self.path)?;
        let mut changed = false;
        for entry in entries.iter_mut().filter(|entry| entry.path == path) {
            if entry.project_id != project_id {
                entry.project_id = project_id.to_string();
                changed = true;
            }
        }
        if changed {
            save(&self.path, &entries)?;
        }
        Ok(())
    }

    pub fn replace(&self, entry: &JournalEntry) -> Result<()> {
        let _guard = self.lock.lock().unwrap_or_else(|err| err.into_inner());
        let mut entries = load(&self.path)?;
        let target = entries
            .iter_mut()
            .find(|e| e.path == entry.path)
            .ok_or(Error::UnknownLeftover)?;
        *target = entry.clone();
        save(&self.path, &entries)
    }

    pub fn set_recovery(&self, path: &Path, recovery: RecoveryMetadata) -> Result<()> {
        let _guard = self.lock.lock().unwrap_or_else(|err| err.into_inner());
        let mut entries = load(&self.path)?;
        let entry = entries
            .iter_mut()
            .find(|entry| entry.path == path)
            .ok_or(Error::UnknownLeftover)?;
        if entry.recovery.as_ref() != Some(&recovery) {
            entry.recovery = Some(recovery);
            save(&self.path, &entries)?;
        }
        Ok(())
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

/// Whether `path` is a real directory. A symbolic link is not, even to a
/// directory: the Lead's fixed path must be the worktree itself, or Forget and
/// refresh would act on whatever the link points at.
pub fn is_real_dir(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|meta| meta.is_dir())
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

/// One line of `git rev-parse --verify <rev>`, or git's first error line.
fn rev_parse(git: &Path, path_env: &str, dir: &Path, rev: &str) -> Result<String, Option<String>> {
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

/// What a diff of a session's tree is measured against.
#[derive(Debug, Clone, Copy)]
pub enum Baseline<'a> {
    /// Where a task branch left its base: the recorded start ref, else the
    /// default branch of `repo`. Commits and uncommitted work both count.
    Task {
        repo: &'a Path,
        recorded: Option<&'a str>,
    },
    /// `HEAD`: uncommitted work only. A checkout session's baseline, since
    /// the author's own commits there are not the session's.
    Head,
}

/// The commit or ref `baseline` resolves to in `worktree`.
fn resolve_baseline(git: &Path, path_env: &str, worktree: &Path, baseline: Baseline) -> String {
    match baseline {
        Baseline::Task { repo, recorded } => diff_base(git, path_env, repo, worktree, recorded),
        Baseline::Head => "HEAD".into(),
    }
}

/// What the session changed against `baseline`: tracked edits (and, for a
/// task, its commits), plus untracked files that are not ignored. Reads
/// only; never stages anything or writes the index.
pub fn diff_stat(
    git: &Path,
    path_env: &str,
    worktree: &Path,
    baseline: Baseline,
) -> Result<crate::DiffStat> {
    check_worktree(git, path_env, worktree).map_err(Error::GitStatus)?;
    let base = resolve_baseline(git, path_env, worktree, baseline);
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

/// The session's whole change for the Changes panel, measured exactly as
/// [`diff_stat`] measures it. After the base is found, two git processes:
/// the patch, streamed and parsed as it arrives, and the untracked list.
/// Reads only, and only this worktree: one whose `.git` is missing or broken
/// is an error, never the main checkout around it.
pub fn session_diff(
    git: &Path,
    path_env: &str,
    worktree: &Path,
    baseline: Baseline,
) -> Result<SessionDiff> {
    check_worktree(git, path_env, worktree).map_err(Error::ReadChanges)?;
    let base = resolve_baseline(git, path_env, worktree, baseline);
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
/// through symlinks. Callers pass the root of the tree they read: a task
/// worktree, or a checkout session's repository root, which is bounded the
/// same way so a broken `.git` never reads a repository around it.
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
#[path = "worktree_tests.rs"]
mod tests;
