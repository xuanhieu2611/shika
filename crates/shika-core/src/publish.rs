//! Explicit, confirmed publishing through the user's git and GitHub CLI.
//! Preview uses a disposable index, never the user's staging area. The
//! author confirms in the Create PR dialog; a Lead's `shika pr` is the one
//! caller that publishes without it ([`publish_now`]).
use crate::{Error, Result, Session, worktree};
use serde::Deserialize;
use std::{
    io::Read,
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublishPreview {
    pub session_id: String,
    pub branch: String,
    /// Explicit GitHub repository, including host. Never inferred by gh.
    pub repository: String,
    pub target: Option<String>,
    pub branches: Vec<String>,
    pub files: Vec<String>,
    pub title: String,
    pub(crate) head: String,
    pub(crate) tree: String,
    pub(crate) origin: String,
}

/// The PR a confirmed publish created or reused, and the commit it pushed.
/// The card watches its checks from this; nothing about it is persisted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublishedPr {
    pub url: String,
    /// Explicit GitHub repository, including host, as in the preview.
    pub repository: String,
    /// None when the URL does not end in `/pull/<number>`; the card then
    /// shows no checks mark.
    pub number: Option<u64>,
    pub head: String,
}

/// Where a PR's checks stand for one commit. Read-only: Shika never
/// re-runs, cancels, or lists checks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChecksState {
    /// GitHub reports no checks for the commit, or none yet.
    NoChecks,
    Pending,
    Passed,
    /// At least one check failed, even while others still run.
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrChecks {
    /// The PR head commit the state belongs to.
    pub head: String,
    /// False once the PR is merged or closed.
    pub open: bool,
    pub state: ChecksState,
}

/// GitHub's limit on a pull request body, in characters.
pub const MAX_PR_BODY_CHARS: usize = 65_536;

fn fail(message: impl Into<String>) -> Error {
    Error::Publish(message.into())
}

/// Bounded subprocesses, no interactive authentication, and a process group
/// so timeout also stops git's SSH/auth/hook children. Always drain both pipes.
fn run(command: Command) -> Result<Vec<u8>> {
    run_with_timeout(command, Duration::from_secs(120))
}
fn run_with_timeout(mut command: Command, timeout: Duration) -> Result<Vec<u8>> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ASKPASS", "/usr/bin/false")
        .env("SSH_ASKPASS", "/usr/bin/false")
        .env("GH_PROMPT_DISABLED", "1")
        .env("GIT_SSH_COMMAND", "ssh -oBatchMode=yes")
        .process_group(0);
    let mut child = command
        .spawn()
        .map_err(|e| fail(format!("Could not run publishing command: {e}")))?;
    let drain = |mut pipe: Box<dyn Read + Send>| {
        let (send, receive) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut kept = Vec::new();
            let mut buf = [0; 8192];
            while let Ok(n) = pipe.read(&mut buf) {
                if n == 0 {
                    break;
                }
                let room = (4 * 1024 * 1024_usize).saturating_sub(kept.len());
                kept.extend_from_slice(&buf[..n.min(room)]);
            }
            let _ = send.send(kept);
        });
        receive
    };
    let out = drain(Box::new(child.stdout.take().unwrap()));
    let err = drain(Box::new(child.stderr.take().unwrap()));
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            other => {
                unsafe {
                    libc::kill(-(child.id() as i32), libc::SIGKILL);
                }
                let _ = child.wait();
                break Err(fail(match other {
                    Err(e) => format!("Could not wait for publishing command: {e}"),
                    _ => "Publishing command timed out. Completed commits and pushes were kept; retry after checking the shell.".into(),
                }));
            }
        }
    };
    let status = status?;
    let receive = |pipe: std::sync::mpsc::Receiver<Vec<u8>>| {
        pipe.recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .map_err(|_| {
                unsafe {
                    libc::kill(-(child.id() as i32), libc::SIGKILL);
                }
                fail("Publishing command output timed out. Check the shell before retrying.")
            })
    };
    let stdout = receive(out)?;
    let stderr = receive(err)?;
    if !status.success() {
        return Err(fail(
            crate::error::first_line(&stderr)
                .unwrap_or_else(|| "Publishing command failed.".into()),
        ));
    }
    Ok(stdout)
}
fn git(git: &Path, env: &str, dir: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let mut cmd = worktree::git_cmd(git, env, dir);
    cmd.args(args);
    run(cmd)
}
fn text(bytes: Vec<u8>) -> String {
    String::from_utf8_lossy(&bytes).trim().to_owned()
}
fn gh(gh: &Path, env: &str, dir: &Path, args: &[&str]) -> Result<Vec<u8>> {
    run(gh_cmd(gh, env, dir, args))
}
fn gh_cmd(gh: &Path, env: &str, dir: &Path, args: &[&str]) -> Command {
    let mut cmd = Command::new(gh);
    cmd.current_dir(dir).env("PATH", env).args(args);
    for key in worktree::GIT_REDIRECTS {
        cmd.env_remove(key);
    }
    // A gh environment default must not redirect an explicitly scoped task.
    cmd.env_remove("GH_REPO");
    cmd
}

/// `https://HOST/OWNER/REPO/pull/N` to N. Anything else gives no number,
/// so the card shows no mark rather than watching another PR.
fn pr_number(url: &str) -> Option<u64> {
    let (_, number) = url.trim_end_matches('/').rsplit_once("/pull/")?;
    number.parse().ok().filter(|n| *n > 0)
}

#[derive(Deserialize)]
struct PrView {
    #[serde(rename = "headRefOid")]
    head: String,
    state: String,
    #[serde(rename = "statusCheckRollup", default)]
    rollup: Vec<RollupItem>,
}
/// A CheckRun has `status` and `conclusion`; a commit StatusContext has
/// `state`. gh returns both kinds in one list.
#[derive(Deserialize)]
struct RollupItem {
    status: Option<String>,
    conclusion: Option<String>,
    state: Option<String>,
}

/// Fail as soon as one check fails, so the card turns red while slower
/// checks still run. Cancelled, skipped, neutral, and stale runs do not ask
/// for a fix and count as finished.
fn classify(items: &[RollupItem]) -> ChecksState {
    if items.is_empty() {
        return ChecksState::NoChecks;
    }
    let mut pending = false;
    for item in items {
        let verdict = match item.state.as_deref().filter(|s| !s.is_empty()) {
            Some(state) => state,
            None if item.status.as_deref() != Some("COMPLETED") => "PENDING",
            None => item
                .conclusion
                .as_deref()
                .filter(|c| !c.is_empty())
                .unwrap_or("PENDING"),
        };
        match verdict {
            "FAILURE" | "ERROR" | "TIMED_OUT" | "ACTION_REQUIRED" | "STARTUP_FAILURE" => {
                return ChecksState::Failed;
            }
            "PENDING" | "EXPECTED" => pending = true,
            _ => {}
        }
    }
    if pending {
        ChecksState::Pending
    } else {
        ChecksState::Passed
    }
}

/// One read of a PR's head and checks. Bounded, non-interactive, and
/// scoped to the explicit repository, like publishing.
pub(crate) fn checks(
    gh_bin: &Path,
    env: &str,
    dir: &Path,
    repository: &str,
    number: u64,
) -> Result<PrChecks> {
    let output = run_with_timeout(
        gh_cmd(
            gh_bin,
            env,
            dir,
            &[
                "pr",
                "view",
                &number.to_string(),
                "--repo",
                repository,
                "--json",
                "headRefOid,state,statusCheckRollup",
            ],
        ),
        Duration::from_secs(20),
    )?;
    let view: PrView =
        serde_json::from_slice(&output).map_err(|_| fail("Could not read PR checks."))?;
    Ok(PrChecks {
        state: classify(&view.rollup),
        open: view.state == "OPEN",
        head: view.head,
    })
}

/// Recognize ordinary HTTPS, SSH, and SCP-style GitHub remotes. Local remotes
/// and ambiguous layouts cannot accidentally select another repository.
fn repository(url: &str) -> Result<String> {
    let parts = if let Some(url) = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
    {
        url.to_string()
    } else if let Some(url) = url.strip_prefix("ssh://") {
        url.strip_prefix("git@").unwrap_or(url).to_string()
    } else if let Some(url) = url.strip_prefix("git@") {
        url.replacen(':', "/", 1)
    } else {
        return Err(fail(
            "Origin must point to a GitHub repository over HTTPS or SSH.",
        ));
    };
    let parts = parts.trim_end_matches('/').trim_end_matches(".git");
    let fields: Vec<_> = parts.split('/').collect();
    if fields.len() != 3
        || fields.iter().any(|s| {
            s.is_empty()
                || !s
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        })
    {
        return Err(fail("Could not identify origin's GitHub repository."));
    }
    Ok(parts.to_string())
}
fn origin(git_bin: &Path, env: &str, dir: &Path) -> Result<(String, String)> {
    let fetch = text(git(git_bin, env, dir, &["remote", "get-url", "origin"])?);
    let pushes = text(git(
        git_bin,
        env,
        dir,
        &["remote", "get-url", "--push", "--all", "origin"],
    )?);
    let repo = repository(&fetch)?;
    let urls: Vec<_> = pushes.lines().collect();
    if urls.len() != 1 || repository(urls[0])? != repo {
        return Err(fail(
            "Origin's fetch and push repositories differ. Configure one matching GitHub destination before publishing.",
        ));
    }
    Ok((format!("{fetch}\n{pushes}"), repo))
}
fn base_name(reference: Option<&str>) -> Option<String> {
    reference
        .and_then(|r| {
            r.strip_prefix("refs/remotes/origin/")
                .or_else(|| r.strip_prefix("refs/heads/"))
        })
        .filter(|s| *s != "HEAD" && !s.is_empty())
        .map(str::to_owned)
}
fn dirty_submodule(status: &[u8]) -> bool {
    status.split(|b| *b == 0).any(|entry| {
        let sub = entry.split(|b| *b == b' ').nth(2).unwrap_or_default();
        sub.len() == 4 && sub[0] == b'S' && (sub[2] != b'.' || sub[3] != b'.')
    })
}
fn ensure_not_integrating(git_bin: &Path, env: &str, dir: &Path) -> Result<()> {
    if dirty_submodule(&git(
        git_bin,
        env,
        dir,
        &["status", "--porcelain=v2", "-z", "--ignore-submodules=none"],
    )?) {
        return Err(fail(
            "Commit or discard changes inside submodules in the shell before publishing.",
        ));
    }
    if !git(git_bin, env, dir, &["ls-files", "--unmerged", "-z"])?.is_empty() {
        return Err(fail(
            "Resolve Git conflicts in the shell before publishing.",
        ));
    }
    for state in [
        "MERGE_HEAD",
        "CHERRY_PICK_HEAD",
        "REVERT_HEAD",
        "rebase-merge",
        "rebase-apply",
    ] {
        let path = PathBuf::from(text(git(
            git_bin,
            env,
            dir,
            &["rev-parse", "--git-path", state],
        )?));
        if dir.join(path).exists() {
            return Err(fail(
                "Finish or abort the merge, rebase, or cherry-pick in the shell before publishing.",
            ));
        }
    }
    Ok(())
}
struct Index(PathBuf);
impl Drop for Index {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
        let _ = std::fs::remove_file(self.0.with_extension("lock"));
    }
}
fn candidate(git_bin: &Path, env: &str, dir: &Path) -> Result<String> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let index = Index(std::env::temp_dir().join(format!(
            "shika-publish-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        )));
    for args in [&["read-tree", "HEAD"][..], &["add", "-A", "--", "."][..]] {
        let mut cmd = worktree::git_cmd(git_bin, env, dir);
        cmd.env("GIT_INDEX_FILE", &index.0).args(args);
        run(cmd)?;
    }
    let mut cmd = worktree::git_cmd(git_bin, env, dir);
    cmd.env("GIT_INDEX_FILE", &index.0).arg("write-tree");
    Ok(text(run(cmd)?))
}

pub(crate) fn preview(
    git_bin: &Path,
    gh_bin: &Path,
    env: &str,
    session: &Session,
) -> Result<PublishPreview> {
    let dir = &session.worktree;
    ensure_not_integrating(git_bin, env, dir)?;
    let (origin, repository) = origin(git_bin, env, dir)?;
    // Proves gh is installed, authenticated, and origin is a supported repo.
    // `repo view` takes the repository as an argument. It does not accept
    // `--repo` (confirmed on gh 2.102.0); that flag is only on other commands.
    gh(
        gh_bin,
        env,
        dir,
        &["repo", "view", &repository, "--json", "nameWithOwner"],
    )?;
    let host = repository.split('/').next().unwrap();
    let repo = repository.split_once('/').unwrap().1;
    let bytes = gh(
        gh_bin,
        env,
        dir,
        &[
            "api",
            "--hostname",
            host,
            "--paginate",
            &format!("repos/{repo}/branches?per_page=100"),
            "--jq",
            ".[].name",
        ],
    )?;
    let mut branches: Vec<String> = text(bytes).lines().map(str::to_owned).collect();
    branches.sort();
    let target = base_name(session.base_ref.as_deref()).filter(|name| branches.contains(name));
    let head = text(git(git_bin, env, dir, &["rev-parse", "HEAD"])?);
    let tree = candidate(git_bin, env, dir)?;
    let files = git(
        git_bin,
        env,
        dir,
        &["diff", "--name-only", "-z", &head, &tree, "--"],
    )?
    .split(|b| *b == 0)
    .filter(|b| !b.is_empty())
    .map(|b| String::from_utf8_lossy(b).into_owned())
    .collect();
    Ok(PublishPreview {
        session_id: session.id.clone(),
        branch: session.branch.clone(),
        repository,
        target,
        branches,
        files,
        title: session.title.clone(),
        head,
        tree,
        origin,
    })
}

/// `shika pr`: what the Create PR dialog does when the author confirms it
/// untouched, in one step. The preview's defaults are the dialog's: the
/// recorded starting base as target and the task title. A task with no such
/// target is refused, never defaulted to another branch. `title` and `body`
/// replace the defaults; no body keeps `gh --fill`'s commit description.
pub(crate) fn publish_now(
    git_bin: &Path,
    gh_bin: &Path,
    env: &str,
    session: &Session,
    title: Option<&str>,
    body: Option<&str>,
) -> Result<PublishedPr> {
    let preview = preview(git_bin, gh_bin, env, session)?;
    let Some(target) = preview.target.clone() else {
        return Err(Error::PublishNoTarget(base_name(
            session.base_ref.as_deref(),
        )));
    };
    let title = title.unwrap_or(&preview.title);
    publish(
        git_bin, gh_bin, env, session, &preview, &target, title, body,
    )
}

#[derive(Deserialize)]
struct PullRequest {
    url: String,
    #[serde(rename = "headRepository")]
    repository: Option<HeadRepository>,
    #[serde(rename = "headRepositoryOwner")]
    owner: Option<HeadOwner>,
}
#[derive(Deserialize)]
struct HeadRepository {
    name: String,
}
#[derive(Deserialize)]
struct HeadOwner {
    login: String,
}
/// `body` is the new PR's description; None (or only whitespace) leaves it to
/// `gh --fill`. A reused PR keeps its title and body.
#[allow(clippy::too_many_arguments)]
pub(crate) fn publish(
    git_bin: &Path,
    gh_bin: &Path,
    env: &str,
    session: &Session,
    preview: &PublishPreview,
    target: &str,
    title: &str,
    body: Option<&str>,
) -> Result<PublishedPr> {
    let dir = &session.worktree;
    ensure_not_integrating(git_bin, env, dir)?;
    let body = validate_request(session, preview, target, title, body)?;
    let repo = recheck_preview(git_bin, env, dir, session, preview)?;
    // Verify target remotely again before mutating anything; never default it.
    let (host, repo_name) = repo.split_once('/').unwrap();
    recheck_remote_target(gh_bin, env, dir, host, repo_name, target)?;
    fetch_target(git_bin, env, dir, target)?;
    let existing = open_prs(gh_bin, env, dir, &repo, &session.branch, target)?;
    commit_if_needed(git_bin, env, dir, preview, title)?;
    let approved_head = approved_commit(git_bin, env, dir, session, preview, target)?;
    push_approved_commit(git_bin, env, dir, &approved_head, &session.branch)?;
    // A fork can have the same head branch name. Reuse only our source repo.
    if let Some(pr) = reusable_pr(&existing, repo_name) {
        return Ok(PublishedPr {
            number: pr_number(&pr.url),
            url: pr.url.clone(),
            repository: repo,
            head: approved_head,
        });
    }
    let mut create = vec![
        "pr",
        "create",
        "--repo",
        &repo,
        "--head",
        &session.branch,
        "--base",
        target,
        "--title",
        title.trim(),
    ];
    // An explicit body replaces `--fill` rather than joining it, so gh never
    // has to choose between them.
    match body {
        Some(body) => create.extend(["--body", body]),
        None => create.push("--fill"),
    }
    let output = gh(gh_bin, env, dir, &create)?;
    let url = text(output);
    if !url.starts_with("https://") || url.contains(['\n', '\r']) {
        return Err(fail(
            "PR command completed without a recognizable URL. Check GitHub before retrying.",
        ));
    }
    Ok(PublishedPr {
        number: pr_number(&url),
        url,
        repository: repo,
        head: approved_head,
    })
}

/// A single-line title, a body within GitHub's limit, and a target from the
/// preview's branch list. Returns the body to send; None keeps `gh --fill`.
fn validate_request<'a>(
    session: &Session,
    preview: &PublishPreview,
    target: &str,
    title: &str,
    body: Option<&'a str>,
) -> Result<Option<&'a str>> {
    if title.trim().is_empty() || title.contains(['\n', '\r', '\0']) {
        return Err(fail("Enter a single-line commit and PR title."));
    }
    let body = body.filter(|body| !body.trim().is_empty());
    if let Some(body) = body
        && (body.contains('\0') || body.chars().count() > MAX_PR_BODY_CHARS)
    {
        return Err(fail(format!(
            "The PR body must be text of at most {MAX_PR_BODY_CHARS} characters."
        )));
    }
    if !preview.branches.iter().any(|b| b == target) || target == session.branch {
        return Err(fail("Choose a different, existing target branch."));
    }
    Ok(body)
}

/// The source branch, origin, HEAD, and candidate tree must still be what the
/// author approved. Returns the origin's repository.
fn recheck_preview(
    git_bin: &Path,
    env: &str,
    dir: &Path,
    session: &Session,
    preview: &PublishPreview,
) -> Result<String> {
    let (current_origin, repo) = origin(git_bin, env, dir)?;
    if session.branch != preview.branch
        || worktree::head_branch(git_bin, env, dir)?.as_ref() != Some(&preview.branch)
        || current_origin != preview.origin
        || repo != preview.repository
        || text(git(git_bin, env, dir, &["rev-parse", "HEAD"])?) != preview.head
        || candidate(git_bin, env, dir)? != preview.tree
    {
        return Err(fail(
            "The task changed after the preview. Cancel and create a fresh preview before publishing.",
        ));
    }
    Ok(repo)
}

/// The target branch must still exist on GitHub.
fn recheck_remote_target(
    gh_bin: &Path,
    env: &str,
    dir: &Path,
    host: &str,
    repo_name: &str,
    target: &str,
) -> Result<()> {
    let exists = text(gh(
        gh_bin,
        env,
        dir,
        &[
            "api",
            "--hostname",
            host,
            "--paginate",
            &format!("repos/{repo_name}/branches?per_page=100"),
            "--jq",
            ".[].name",
        ],
    )?);
    if !exists.lines().any(|name| name == target) {
        return Err(fail(
            "The PR target no longer exists. Cancel and choose another target.",
        ));
    }
    Ok(())
}

/// gh --fill reads local history against the selected remote base. A local
/// starting branch or explicit replacement target may not have a tracking
/// ref yet, so fetch exactly that branch, without touching any checkout.
fn fetch_target(git_bin: &Path, env: &str, dir: &Path, target: &str) -> Result<()> {
    git(
        git_bin,
        env,
        dir,
        &[
            "fetch",
            "--no-tags",
            "origin",
            &format!("+refs/heads/{target}:refs/remotes/origin/{target}"),
        ],
    )?;
    Ok(())
}

/// Open PRs from the task branch to the target, looked up before any commit.
fn open_prs(
    gh_bin: &Path,
    env: &str,
    dir: &Path,
    repo: &str,
    branch: &str,
    target: &str,
) -> Result<Vec<PullRequest>> {
    serde_json::from_slice(&gh(
        gh_bin,
        env,
        dir,
        &[
            "pr",
            "list",
            "--repo",
            repo,
            "--head",
            branch,
            "--base",
            target,
            "--state",
            "open",
            "--json",
            "url,headRepository,headRepositoryOwner",
        ],
    )?)
    .map_err(|_| fail("Could not read existing pull requests."))
}

/// Stages the real index and commits with the title when HEAD's tree is not
/// the approved tree, verifying the staged and committed trees.
fn commit_if_needed(
    git_bin: &Path,
    env: &str,
    dir: &Path,
    preview: &PublishPreview,
    title: &str,
) -> Result<()> {
    let head_tree = text(git(git_bin, env, dir, &["rev-parse", "HEAD^{tree}"])?);
    if head_tree != preview.tree {
        git(git_bin, env, dir, &["add", "-A", "--", "."])?;
        if text(git(git_bin, env, dir, &["write-tree"])?) != preview.tree {
            return Err(fail(
                "Files changed during staging. Changes were left staged; create a fresh preview.",
            ));
        }
        if worktree::head_branch(git_bin, env, dir)?.as_ref() != Some(&preview.branch)
            || text(git(git_bin, env, dir, &["rev-parse", "HEAD"])?) != preview.head
        {
            return Err(fail(
                "The task branch changed during staging. Changes were left staged; inspect the shell before retrying.",
            ));
        }
        git(git_bin, env, dir, &["commit", "-m", title.trim()])?;
        if text(git(git_bin, env, dir, &["rev-parse", "HEAD^{tree}"])?) != preview.tree {
            return Err(fail(
                "A commit hook changed the approved tree. Commit kept locally; inspect it before retrying.",
            ));
        }
    }
    Ok(())
}

/// Captures the commit to push and verifies it: the approved tree, still on
/// the task branch, ahead of the target, and origin unchanged.
fn approved_commit(
    git_bin: &Path,
    env: &str,
    dir: &Path,
    session: &Session,
    preview: &PublishPreview,
    target: &str,
) -> Result<String> {
    let approved_head = text(git(git_bin, env, dir, &["rev-parse", "HEAD"])?);
    if text(git(
        git_bin,
        env,
        dir,
        &["rev-parse", &format!("{approved_head}^{{tree}}")],
    )?) != preview.tree
        || worktree::head_branch(git_bin, env, dir)?.as_ref() != Some(&session.branch)
        || candidate(git_bin, env, dir)? != preview.tree
    {
        return Err(fail(
            "The task changed while committing. Commit kept locally; inspect it before retrying.",
        ));
    }
    if text(git(
        git_bin,
        env,
        dir,
        &[
            "rev-list",
            "--count",
            &format!("refs/remotes/origin/{target}..{approved_head}"),
        ],
    )?) == "0"
    {
        return Err(fail("There are no task commits to publish to this target."));
    }
    if origin(git_bin, env, dir)?.0 != preview.origin {
        return Err(fail(
            "Origin changed while committing. Commit kept locally; inspect the shell before retrying.",
        ));
    }
    Ok(approved_head)
}

/// Pin the approved commit: a concurrent commit after these checks must not
/// hitch a ride on the push. Explicit destination bypasses push.default.
fn push_approved_commit(
    git_bin: &Path,
    env: &str,
    dir: &Path,
    approved_head: &str,
    branch: &str,
) -> Result<()> {
    git(
        git_bin,
        env,
        dir,
        &[
            "push",
            "-u",
            "origin",
            &format!("{approved_head}:refs/heads/{}", branch),
        ],
    )?;
    git(
        git_bin,
        env,
        dir,
        &[
            "branch",
            &format!("--set-upstream-to=origin/{}", branch),
            "--",
            branch,
        ],
    )?;
    Ok(())
}

/// An open PR whose head is our source repository, not a fork's same-named
/// branch.
fn reusable_pr<'a>(existing: &'a [PullRequest], repo_name: &str) -> Option<&'a PullRequest> {
    existing.iter().find(|pr| {
        pr.owner
            .as_ref()
            .zip(pr.repository.as_ref())
            .is_some_and(|(owner, repository)| {
                format!("{}/{}", owner.login, repository.name).eq_ignore_ascii_case(repo_name)
            })
    })
}

#[cfg(test)]
#[path = "publish_tests.rs"]
mod tests;
