//! Explicit, confirmed publishing through the user's git and GitHub CLI.
//! Preview uses a disposable index, never the user's staging area.
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
    // This proves gh is installed/authenticated and origin is a supported repo.
    gh(
        gh_bin,
        env,
        dir,
        &[
            "repo",
            "view",
            "--repo",
            &repository,
            "--json",
            "nameWithOwner",
        ],
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
pub(crate) fn publish(
    git_bin: &Path,
    gh_bin: &Path,
    env: &str,
    session: &Session,
    preview: &PublishPreview,
    target: &str,
    title: &str,
) -> Result<PublishedPr> {
    let dir = &session.worktree;
    ensure_not_integrating(git_bin, env, dir)?;
    if title.trim().is_empty() || title.contains(['\n', '\r', '\0']) {
        return Err(fail("Enter a single-line commit and PR title."));
    }
    if !preview.branches.iter().any(|b| b == target) || target == session.branch {
        return Err(fail("Choose a different, existing target branch."));
    }
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
    // Verify target remotely again before mutating anything; never default it.
    let (host, repo_name) = repo.split_once('/').unwrap();
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
    // gh --fill reads local history against the selected remote base. A local
    // starting branch or explicit replacement target may not have a tracking
    // ref yet, so fetch exactly that branch, without touching any checkout.
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
    let existing: Vec<PullRequest> = serde_json::from_slice(&gh(
        gh_bin,
        env,
        dir,
        &[
            "pr",
            "list",
            "--repo",
            &repo,
            "--head",
            &session.branch,
            "--base",
            target,
            "--state",
            "open",
            "--json",
            "url,headRepository,headRepositoryOwner",
        ],
    )?)
    .map_err(|_| fail("Could not read existing pull requests."))?;
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
    // Pin the approved commit: a concurrent commit after these checks must not
    // hitch a ride on the push. Explicit destination bypasses push.default.
    git(
        git_bin,
        env,
        dir,
        &[
            "push",
            "-u",
            "origin",
            &format!("{approved_head}:refs/heads/{}", session.branch),
        ],
    )?;
    git(
        git_bin,
        env,
        dir,
        &[
            "branch",
            &format!("--set-upstream-to=origin/{}", session.branch),
            "--",
            &session.branch,
        ],
    )?;
    // A fork can have the same head branch name. Reuse only our source repo.
    if let Some(pr) = existing.iter().find(|pr| {
        pr.owner
            .as_ref()
            .zip(pr.repository.as_ref())
            .is_some_and(|(owner, repository)| {
                format!("{}/{}", owner.login, repository.name).eq_ignore_ascii_case(repo_name)
            })
    }) {
        return Ok(PublishedPr {
            number: pr_number(&pr.url),
            url: pr.url.clone(),
            repository: repo,
            head: approved_head,
        });
    }
    let output = gh(
        gh_bin,
        env,
        dir,
        &[
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
            "--fill",
        ],
    )?;
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

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture {
        root: PathBuf,
        session: Session,
        git: PathBuf,
        gh: PathBuf,
        remote: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            use std::os::unix::fs::PermissionsExt;
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let root = std::env::temp_dir().join(format!(
                "shika-publish-test-{}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&root).unwrap();
            let root = root.canonicalize().unwrap();
            let repo = root.join("repo");
            std::fs::create_dir(&repo).unwrap();
            let real = Path::new("/usr/bin/git");
            git(real, "/usr/bin:/bin", &repo, &["init", "-b", "main"]).unwrap();
            git(
                real,
                "/usr/bin:/bin",
                &repo,
                &["config", "user.name", "Test User"],
            )
            .unwrap();
            git(
                real,
                "/usr/bin:/bin",
                &repo,
                &["config", "user.email", "test@example.com"],
            )
            .unwrap();
            std::fs::write(repo.join("tracked"), "base\n").unwrap();
            git(real, "/usr/bin:/bin", &repo, &["add", "."]).unwrap();
            git(real, "/usr/bin:/bin", &repo, &["commit", "-m", "base"]).unwrap();
            git(real, "/usr/bin:/bin", &repo, &["branch", "dev"]).unwrap();
            let remote = root.join("remote.git");
            git(
                real,
                "/usr/bin:/bin",
                &repo,
                &[
                    "clone",
                    "--bare",
                    repo.to_str().unwrap(),
                    remote.to_str().unwrap(),
                ],
            )
            .unwrap();
            git(
                real,
                "/usr/bin:/bin",
                &repo,
                &[
                    "remote",
                    "add",
                    "origin",
                    "https://github.com/test/repo.git",
                ],
            )
            .unwrap();
            let worktree = root.join("task");
            git(
                real,
                "/usr/bin:/bin",
                &repo,
                &[
                    "worktree",
                    "add",
                    "-b",
                    "feat/task",
                    worktree.to_str().unwrap(),
                    "dev",
                ],
            )
            .unwrap();
            let wrapper = root.join("git");
            // Only transport is fake; all staging, commits, and refs are real.
            std::fs::write(&wrapper, format!("#!/bin/sh\nif [ \"$3\" = fetch ]; then\n  dir=$2; shift 3; shift; shift\n  exec /usr/bin/git -C \"$dir\" fetch --no-tags '{}' \"$@\"\nfi\nif [ \"$3\" = push ]; then\n  dir=$2; shift 3\n  exec /usr/bin/git -C \"$dir\" -c remote.origin.pushurl='{}' push \"$@\"\nfi\nexec /usr/bin/git \"$@\"\n", remote.display(), remote.display())).unwrap();
            let gh = root.join("gh");
            std::fs::write(&gh, format!("#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}/calls'\ncase \"$1 $2\" in\n 'repo view') echo '{{\"nameWithOwner\":\"test/repo\"}}';;\n 'api --hostname') printf 'main\\ndev\\n';;\n 'pr list') if [ -f '{}/existing' ]; then echo '[{{\"url\":\"https://github.com/test/repo/pull/1\",\"headRepository\":{{\"name\":\"repo\"}},\"headRepositoryOwner\":{{\"login\":\"test\"}}}}]'; else echo '[]'; fi;;\n 'pr create') if [ -f '{}/fail' ]; then echo 'GitHub unavailable' >&2; exit 1; fi; echo 'https://github.com/test/repo/pull/1';;\n *) exit 9;;\nesac\n", root.display(), root.display(), root.display())).unwrap();
            for path in [&wrapper, &gh] {
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
            let session = Session {
                id: "task".into(),
                project_id: "project".into(),
                preset_id: "pi".into(),
                preset_name: "Pi".into(),
                title: "Add useful feature".into(),
                manual_title: false,
                branch: "feat/task".into(),
                repo,
                worktree,
                base_ref: Some("refs/heads/dev".into()),
                pty: crate::PtyId(1),
                shell_ptys: vec![],
                cli_titled: true,
                lead: false,
                started_by: None,
            };
            Self {
                root,
                session,
                git: wrapper,
                gh,
                remote,
            }
        }
        fn preview(&self) -> PublishPreview {
            preview(&self.git, &self.gh, "/usr/bin:/bin", &self.session).unwrap()
        }
        fn publish(&self, preview: &PublishPreview) -> Result<PublishedPr> {
            publish(
                &self.git,
                &self.gh,
                "/usr/bin:/bin",
                &self.session,
                preview,
                "dev",
                "Add useful feature",
            )
        }
        fn git(&self, args: &[&str]) -> String {
            text(git(&self.git, "/usr/bin:/bin", &self.session.worktree, args).unwrap())
        }
        fn edit(&self) {
            std::fs::write(self.session.worktree.join("tracked"), "feature\n").unwrap();
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn manual_task_name_seeds_future_commit_and_pr_defaults_only() {
        let mut f = Fixture::new();
        f.edit();
        let store = crate::session::SessionStore::new();
        store.insert(f.session.clone());
        let old_preview = f.preview();
        f.session = store
            .set_title(&f.session.id, "My chosen task name")
            .unwrap();
        let preview = f.preview();
        assert_eq!(old_preview.title, "Add useful feature");
        assert_eq!(preview.title, "My chosen task name");
        assert_eq!(preview.branch, old_preview.branch);
        assert_eq!(preview.tree, old_preview.tree);
        assert_eq!(f.git(&["log", "-1", "--format=%s"]), "base");
        publish(
            &f.git,
            &f.gh,
            "/usr/bin:/bin",
            &f.session,
            &preview,
            "dev",
            &preview.title,
        )
        .unwrap();
        assert_eq!(f.git(&["log", "-1", "--format=%s"]), "My chosen task name");
        let calls = std::fs::read_to_string(f.root.join("calls")).unwrap();
        assert!(calls.contains("--head feat/task --base dev --title My chosen task name --fill"));
        f.session = store.set_title(&f.session.id, "Later name").unwrap();
        assert_eq!(f.git(&["log", "-1", "--format=%s"]), "My chosen task name");
        assert_eq!(f.git(&["branch", "--show-current"]), "feat/task");
        assert_eq!(
            std::fs::read_to_string(f.root.join("calls")).unwrap(),
            calls
        );
    }

    #[test]
    fn preview_leaves_real_index_untouched_and_records_all_nonignored_changes() {
        let f = Fixture::new();
        f.edit();
        std::fs::write(f.session.worktree.join(".gitignore"), "secret\n").unwrap();
        std::fs::write(f.session.worktree.join("secret"), "do not publish").unwrap();
        std::fs::write(f.session.worktree.join("new file"), "new\n").unwrap();
        let before = f.git(&["write-tree"]);
        let p = f.preview();
        assert_eq!(p.target.as_deref(), Some("dev"));
        assert_eq!(p.files, [".gitignore", "new file", "tracked"]);
        assert_eq!(f.git(&["write-tree"]), before);
        assert_eq!(f.git(&["log", "-1", "--format=%s"]), "base");
    }
    #[test]
    fn confirmed_publish_commits_pushes_and_targets_recorded_dev() {
        let f = Fixture::new();
        f.edit();
        let p = f.preview();
        let published = f.publish(&p).unwrap();
        assert_eq!(published.url, "https://github.com/test/repo/pull/1");
        assert_eq!(published.repository, "github.com/test/repo");
        assert_eq!(published.number, Some(1));
        assert_eq!(published.head, f.git(&["rev-parse", "HEAD"]));
        assert_eq!(f.git(&["log", "-1", "--format=%s"]), "Add useful feature");
        assert!(f.git(&["status", "--porcelain"]).is_empty());
        let remote_head = text(
            git(
                Path::new("/usr/bin/git"),
                "/usr/bin:/bin",
                &f.remote,
                &["rev-parse", "refs/heads/feat/task"],
            )
            .unwrap(),
        );
        assert_eq!(remote_head, f.git(&["rev-parse", "HEAD"]));
        // The card resumes watching checks when this local ref moves.
        assert_eq!(
            worktree::pushed_head(&f.git, "/usr/bin:/bin", &f.session.worktree, "feat/task")
                .unwrap(),
            Some(remote_head)
        );
        assert_eq!(
            worktree::pushed_head(&f.git, "/usr/bin:/bin", &f.session.worktree, "missing").unwrap(),
            None
        );
        let calls = std::fs::read_to_string(f.root.join("calls")).unwrap();
        assert!(calls.contains("pr create --repo github.com/test/repo --head feat/task --base dev --title Add useful feature --fill"));
        assert!(!calls.contains("merge"));
        assert!(f.session.worktree.exists());
    }
    #[test]
    fn stale_preview_refuses_before_staging_or_committing() {
        let f = Fixture::new();
        f.edit();
        let p = f.preview();
        std::fs::write(f.session.worktree.join("tracked"), "different\n").unwrap();
        let before = f.git(&["write-tree"]);
        assert!(
            f.publish(&p)
                .unwrap_err()
                .to_string()
                .contains("changed after")
        );
        assert_eq!(f.git(&["write-tree"]), before);
        assert_eq!(f.git(&["rev-parse", "HEAD"]), p.head);
    }
    #[test]
    fn missing_recorded_base_requires_explicit_target() {
        let mut f = Fixture::new();
        for reference in [None, Some("refs/heads/deleted".into())] {
            f.session.base_ref = reference;
            assert_eq!(f.preview().target, None);
        }
        let p = f.preview();
        assert!(publish(&f.git, &f.gh, "/usr/bin:/bin", &f.session, &p, "", "title").is_err());
    }
    #[test]
    fn existing_pr_is_reused_without_duplicate_creation() {
        let f = Fixture::new();
        f.edit();
        std::fs::write(f.root.join("existing"), "").unwrap();
        let published = f.publish(&f.preview()).unwrap();
        assert_eq!(published.number, Some(1));
        assert_eq!(published.head, f.git(&["rev-parse", "HEAD"]));
        assert!(
            !std::fs::read_to_string(f.root.join("calls"))
                .unwrap()
                .contains("pr create")
        );
    }
    #[test]
    fn same_named_branch_from_a_fork_is_not_reused() {
        let f = Fixture::new();
        f.edit();
        std::fs::write(f.root.join("existing"), "").unwrap();
        let script = std::fs::read_to_string(&f.gh)
            .unwrap()
            .replace("\"login\":\"test\"", "\"login\":\"fork\"");
        std::fs::write(&f.gh, script).unwrap();
        f.publish(&f.preview()).unwrap();
        assert!(
            std::fs::read_to_string(f.root.join("calls"))
                .unwrap()
                .contains("pr create")
        );
    }
    #[test]
    fn pr_failure_keeps_commit_and_push_and_retry_does_not_commit_twice() {
        let f = Fixture::new();
        f.edit();
        std::fs::write(f.root.join("fail"), "").unwrap();
        assert!(f.publish(&f.preview()).is_err());
        let head = f.git(&["rev-parse", "HEAD"]);
        assert_eq!(f.git(&["log", "-1", "--format=%s"]), "Add useful feature");
        std::fs::remove_file(f.root.join("fail")).unwrap();
        f.publish(&f.preview()).unwrap();
        assert_eq!(f.git(&["rev-parse", "HEAD"]), head);
    }
    #[test]
    fn commit_hook_failure_keeps_work_and_does_not_push() {
        use std::os::unix::fs::PermissionsExt;
        let f = Fixture::new();
        f.edit();
        let hook = f.session.repo.join(".git/hooks/pre-commit");
        std::fs::write(&hook, "#!/bin/sh\necho 'tests failed' >&2\nexit 1\n").unwrap();
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        let p = f.preview();
        assert!(
            f.publish(&p)
                .unwrap_err()
                .to_string()
                .contains("tests failed")
        );
        assert_eq!(f.git(&["rev-parse", "HEAD"]), p.head);
        assert!(!f.git(&["diff", "--cached", "--name-only"]).is_empty());
    }
    #[test]
    fn changed_remote_and_unknown_target_refuse_without_commit() {
        let f = Fixture::new();
        f.edit();
        let p = f.preview();
        assert!(
            publish(
                &f.git,
                &f.gh,
                "/usr/bin:/bin",
                &f.session,
                &p,
                "not-a-branch",
                "title"
            )
            .is_err()
        );
        f.git(&[
            "remote",
            "set-url",
            "origin",
            "https://github.com/test/other.git",
        ]);
        assert!(f.publish(&p).is_err());
        assert_eq!(f.git(&["rev-parse", "HEAD"]), p.head);
    }

    #[test]
    fn recorded_main_also_targets_main_not_dev() {
        let mut f = Fixture::new();
        f.session.base_ref = Some("refs/remotes/origin/main".into());
        f.edit();
        let p = f.preview();
        assert_eq!(p.target.as_deref(), Some("main"));
        publish(
            &f.git,
            &f.gh,
            "/usr/bin:/bin",
            &f.session,
            &p,
            "main",
            &p.title,
        )
        .unwrap();
        assert!(
            std::fs::read_to_string(f.root.join("calls"))
                .unwrap()
                .contains("--base main --title Add useful feature")
        );
    }
    #[test]
    fn switched_source_branch_cannot_publish_even_with_same_head_and_files() {
        let f = Fixture::new();
        f.edit();
        let p = f.preview();
        f.git(&["switch", "-c", "unrelated"]);
        assert!(
            f.publish(&p)
                .unwrap_err()
                .to_string()
                .contains("changed after")
        );
        assert_eq!(f.git(&["log", "-1", "--format=%s"]), "base");
    }
    #[test]
    fn dirty_submodule_content_is_not_silently_omitted() {
        assert!(dirty_submodule(
            b"1 .M S.M. 160000 160000 160000 hash hash sub\0"
        ));
        assert!(dirty_submodule(
            b"1 .M S..U 160000 160000 160000 hash hash sub\0"
        ));
        assert!(!dirty_submodule(
            b"1 .M SC.. 160000 160000 160000 hash hash sub\0"
        ));
        assert!(!dirty_submodule(
            b"1 .M N... 100644 100644 100644 hash hash file\0? new file\0"
        ));
    }
    #[test]
    fn unfinished_merge_refuses_before_staging() {
        let f = Fixture::new();
        f.edit();
        let marker = f.git(&["rev-parse", "--git-path", "MERGE_HEAD"]);
        std::fs::write(
            f.session.worktree.join(marker),
            f.git(&["rev-parse", "HEAD"]),
        )
        .unwrap();
        assert!(
            preview(&f.git, &f.gh, "/usr/bin:/bin", &f.session)
                .unwrap_err()
                .to_string()
                .contains("Finish or abort")
        );
        assert!(f.git(&["diff", "--cached", "--name-only"]).is_empty());
    }
    #[test]
    fn subprocess_timeout_is_bounded_even_with_inherited_pipes() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "sleep 10 & wait"]);
        let started = Instant::now();
        assert!(run_with_timeout(command, Duration::from_millis(100)).is_err());
        assert!(started.elapsed() < Duration::from_secs(3));
    }
    #[test]
    fn recorded_base_only_never_default_or_current_project() {
        assert_eq!(
            base_name(Some("refs/remotes/origin/dev")),
            Some("dev".into())
        );
        assert_eq!(base_name(Some("refs/heads/main")), Some("main".into()));
        assert_eq!(
            base_name(Some("refs/heads/release/v2")),
            Some("release/v2".into())
        );
        assert_eq!(base_name(None), None);
        assert_eq!(base_name(Some("refs/remotes/origin/HEAD")), None);
        assert_eq!(base_name(Some("refs/remotes/upstream/dev")), None);
    }
    #[test]
    fn explicit_remote_mapping() {
        for url in [
            "git@github.com:org/repo.git",
            "https://github.com/org/repo.git",
            "ssh://git@github.com/org/repo.git",
        ] {
            assert_eq!(repository(url).unwrap(), "github.com/org/repo");
        }
        assert_eq!(
            repository("https://ghe.example/org/repo").unwrap(),
            "ghe.example/org/repo"
        );
        for url in [
            "/tmp/repo",
            "file:///repo",
            "https://github.com/repo",
            "https://user:secret@github.com/org/repo",
            "-bad",
        ] {
            assert!(repository(url).is_err());
        }
    }
    #[test]
    fn pr_number_comes_only_from_a_pull_url() {
        assert_eq!(pr_number("https://github.com/org/repo/pull/42"), Some(42));
        assert_eq!(pr_number("https://ghe.example/org/repo/pull/7/"), Some(7));
        for url in [
            "https://github.com/org/repo",
            "https://github.com/org/repo/pull/",
            "https://github.com/org/repo/pull/0",
            "https://github.com/org/repo/pull/12/files",
        ] {
            assert_eq!(pr_number(url), None, "{url}");
        }
    }
    fn items(json: &str) -> Vec<RollupItem> {
        serde_json::from_str(json).unwrap()
    }
    #[test]
    fn checks_fail_fast_and_finish_only_when_every_check_is_done() {
        use ChecksState::*;
        assert_eq!(classify(&[]), NoChecks);
        let run = |status: &str, conclusion: &str| {
            format!(
                r#"{{"__typename":"CheckRun","status":"{status}","conclusion":"{conclusion}"}}"#
            )
        };
        let done = run("COMPLETED", "SUCCESS");
        let skipped = run("COMPLETED", "SKIPPED");
        let cancelled = run("COMPLETED", "CANCELLED");
        let running = run("IN_PROGRESS", "");
        let failed = run("COMPLETED", "FAILURE");
        let status = |state: &str| format!(r#"{{"__typename":"StatusContext","state":"{state}"}}"#);
        let case = |parts: &[&str]| classify(&items(&format!("[{}]", parts.join(","))));
        assert_eq!(case(&[&done, &skipped, &cancelled]), Passed);
        assert_eq!(case(&[&done, &running]), Pending);
        assert_eq!(case(&[&running, &failed]), Failed);
        assert_eq!(case(&[&run("COMPLETED", "TIMED_OUT")]), Failed);
        assert_eq!(case(&[&run("QUEUED", "")]), Pending);
        assert_eq!(case(&[&done, &status("SUCCESS")]), Passed);
        assert_eq!(case(&[&status("PENDING")]), Pending);
        assert_eq!(case(&[&done, &status("ERROR")]), Failed);
    }
    #[test]
    fn checks_read_one_pr_view_scoped_to_the_repository() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!(
            "shika-checks-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let gh = root.join("gh");
        std::fs::write(
            &gh,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}/calls'\necho '{{\"headRefOid\":\"abc\",\"state\":\"MERGED\",\"statusCheckRollup\":[{{\"__typename\":\"CheckRun\",\"status\":\"COMPLETED\",\"conclusion\":\"FAILURE\"}}]}}'\n",
                root.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
        let checks = checks(&gh, "/usr/bin:/bin", &root, "github.com/test/repo", 42).unwrap();
        assert_eq!(
            checks,
            PrChecks {
                head: "abc".into(),
                open: false,
                state: ChecksState::Failed,
            }
        );
        let calls = std::fs::read_to_string(root.join("calls")).unwrap();
        assert_eq!(
            calls.trim(),
            "pr view 42 --repo github.com/test/repo --json headRefOid,state,statusCheckRollup"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
