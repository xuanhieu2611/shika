//! The best-effort base branch fetch before New: which branch, whether there
//! is an origin, and a fetch that can never prompt and gives up on time.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::branch::is_valid_branch;
use super::git::git_cmd;

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
