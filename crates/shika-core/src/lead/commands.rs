//! The short CLI commands a Lead start may run before the operations lock:
//! Cursor's `agent create-chat` for a terminal Lead and `agent --version`
//! for a chat one.
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::agents;
use crate::error::{Error, Result};
use crate::pty::CONTROL_VARS;
use crate::worktree::GIT_REDIRECTS;

/// How long Cursor's `create-chat` may take before the Lead starts without a
/// conversation id, and its `--version` before a chat Lead is refused.
const CREATE_CHAT_TIMEOUT: Duration = Duration::from_secs(10);

/// `agent create-chat`: a new empty Cursor chat, whose id it prints. The same
/// environment other children get (login-shell `PATH`, no git redirects, no
/// control variables), a 10 second limit, and None on any failure, in which
/// case the Lead starts without an id. Blocking.
pub(super) fn create_chat(program: &Path, path_env: &str, cwd: &Path) -> Option<String> {
    create_chat_within(program, path_env, cwd, CREATE_CHAT_TIMEOUT)
}

pub(super) fn create_chat_within(
    program: &Path,
    path_env: &str,
    cwd: &Path,
    timeout: Duration,
) -> Option<String> {
    let id = quick_output(program, "create-chat", path_env, cwd, timeout)?;
    let id = id.trim().to_string();
    agents::valid_conversation_id(&id).then_some(id)
}

/// Refuses a chat Lead on a Cursor CLI older than
/// [`crate::lead_transport::CURSOR_MINIMUM`], or whose version cannot be
/// read: its ACP mode was verified on that version only. Blocking.
pub(super) fn check_cursor_version(program: &Path, path_env: &str, name: &str) -> Result<()> {
    let cwd = std::env::temp_dir();
    let output = quick_output(program, "--version", path_env, &cwd, CREATE_CHAT_TIMEOUT);
    if output.is_some_and(|version| crate::lead_transport::cursor_version_supported(&version)) {
        return Ok(());
    }
    Err(Error::ChatCliTooOld {
        name: name.into(),
        minimum: crate::lead_transport::CURSOR_MINIMUM.into(),
    })
}

/// The stdout of `program arg`, run with the environment other children get
/// (login-shell `PATH`, no git redirects, no control variables) and `timeout`
/// at most. None on any failure. Blocking.
fn quick_output(
    program: &Path,
    arg: &str,
    path_env: &str,
    cwd: &Path,
    timeout: Duration,
) -> Option<String> {
    let mut cmd = Command::new(program);
    cmd.arg(arg)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if !path_env.is_empty() {
        cmd.env("PATH", path_env);
    }
    for key in GIT_REDIRECTS.iter().chain(CONTROL_VARS.iter()) {
        cmd.env_remove(key);
    }
    let mut child = cmd.spawn().ok()?;
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    if !status.success() {
        return None;
    }
    let mut out = String::new();
    std::io::Read::read_to_string(&mut child.stdout.take()?, &mut out).ok()?;
    Some(out)
}
