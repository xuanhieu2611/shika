//! One short-lived metadata-only app-server probe. No thread/start, turn/start,
//! login, inference, credential reads, or persistent child.
use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{
    Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

use super::{CodexLimits, QuotaSnapshot, QuotaStatus, now};
use serde_json::{Value, json};

const TIMEOUT: Duration = Duration::from_secs(8);
const MAX_LINE: usize = 64 * 1024;
const MAX_TOTAL: usize = 1024 * 1024;

#[derive(Default)]
pub(super) struct Probes {
    stopped: AtomicBool,
    children: Mutex<BTreeSet<u32>>,
}
impl Probes {
    pub(super) fn stop(&self) {
        let children = self.children.lock().unwrap_or_else(|e| e.into_inner());
        self.stopped.store(true, Ordering::Release);
        for pid in children.iter() {
            unsafe {
                libc::kill(-(*pid as i32), libc::SIGKILL);
            }
        }
    }
}
struct Probe<'a>(Child, &'a Probes);
impl Drop for Probe<'_> {
    fn drop(&mut self) {
        // Reap the whole owned process group even when a descendant retains a pipe.
        {
            let mut children = self.1.children.lock().unwrap_or_else(|e| e.into_inner());
            unsafe {
                libc::kill(-(self.0.id() as i32), libc::SIGKILL);
            }
            // Remove before reaping so quit cannot signal a recycled process id,
            // and never make quit wait behind the child's OS cleanup.
            children.remove(&self.0.id());
        }
        let _ = self.0.wait();
    }
}

pub(super) fn read(
    program: &Path,
    path_env: &str,
    root: &Path,
    probes: &Probes,
) -> Result<QuotaSnapshot, QuotaStatus> {
    read_with_timeout(program, path_env, root, TIMEOUT, probes)
}

fn nonblocking(fd: i32) -> Result<(), QuotaStatus> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(QuotaStatus::Unavailable);
    }
    Ok(())
}

fn read_with_timeout(
    program: &Path,
    path_env: &str,
    root: &Path,
    timeout: Duration,
    probes: &Probes,
) -> Result<QuotaSnapshot, QuotaStatus> {
    // Empty, private cwd prevents project config/plugins from affecting a quota read.
    let scratch = super::claude::Source::create().map_err(|_| QuotaStatus::Unavailable)?;
    let cwd = scratch.path();
    let mut command = Command::new(program);
    command
        .args(["app-server", "--listen", "stdio://"])
        .current_dir(cwd)
        .env("PWD", cwd)
        .env("PATH", path_env)
        .env("CODEX_HOME", root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0);
    for key in crate::worktree::GIT_REDIRECTS
        .iter()
        .chain(crate::pty::CONTROL_VARS.iter())
    {
        command.env_remove(key);
    }
    let mut probe = {
        // Serialize spawn/registration with quit, not with terminal/core operations.
        let mut children = probes.children.lock().unwrap_or_else(|e| e.into_inner());
        if probes.stopped.load(Ordering::Acquire) {
            return Err(QuotaStatus::Unavailable);
        }
        let child = command.spawn().map_err(|_| QuotaStatus::Unavailable)?;
        children.insert(child.id());
        Probe(child, probes)
    };
    let mut input = probe.0.stdin.take().ok_or(QuotaStatus::Unavailable)?;
    let mut output = probe.0.stdout.take().ok_or(QuotaStatus::Unavailable)?;
    nonblocking(input.as_raw_fd())?;
    nonblocking(output.as_raw_fd())?;
    let deadline = Instant::now() + timeout;
    let mut protocol = Protocol::new();
    let mut bytes = Vec::new();
    let mut total = 0;
    loop {
        if probes.stopped.load(Ordering::Acquire) {
            return Err(QuotaStatus::Unavailable);
        }
        if Instant::now() >= deadline {
            return Err(QuotaStatus::TimedOut);
        }
        if protocol.written < protocol.pending.len() {
            match input.write(&protocol.pending[protocol.written..]) {
                Ok(0) => return Err(QuotaStatus::Unavailable),
                Ok(n) => protocol.written += n,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(_) => return Err(QuotaStatus::Unavailable),
            }
        }
        let mut chunk = [0; 8192];
        match output.read(&mut chunk) {
            Ok(0) => return Err(QuotaStatus::Unavailable),
            Ok(n) => {
                total += n;
                if total > MAX_TOTAL {
                    return Err(QuotaStatus::Unavailable);
                }
                bytes.extend_from_slice(&chunk[..n]);
                while let Some(end) = bytes.iter().position(|b| *b == b'\n') {
                    if end > MAX_LINE {
                        return Err(QuotaStatus::Unavailable);
                    }
                    let line: Vec<_> = bytes.drain(..=end).collect();
                    let value: Value =
                        serde_json::from_slice(&line).map_err(|_| QuotaStatus::Unavailable)?;
                    if let Some(snapshot) = protocol.reply(&value)? {
                        return Ok(snapshot);
                    }
                }
                if bytes.len() > MAX_LINE {
                    return Err(QuotaStatus::Unavailable);
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(10))
            }
            Err(_) => return Err(QuotaStatus::Unavailable),
        }
    }
}

struct Protocol {
    pending: Vec<u8>,
    written: usize,
    expected: u64,
}

impl Protocol {
    fn new() -> Self {
        Self {
            pending: frame(
                json!({"id":1,"method":"initialize","params":{"clientInfo":{"name":"shika_quota","version":"1"}}}),
            ),
            written: 0,
            expected: 1,
        }
    }

    fn reply(&mut self, value: &Value) -> Result<Option<QuotaSnapshot>, QuotaStatus> {
        if value.get("id").and_then(Value::as_u64) != Some(self.expected) {
            return Ok(None);
        }
        if value.get("error").is_some() {
            return Err(QuotaStatus::Unavailable);
        }
        let result = value.get("result").ok_or(QuotaStatus::Unavailable)?;
        match self.expected {
            1 => {
                self.pending = frame(json!({"method":"initialized"}));
                self.pending.extend(frame(
                    json!({"id":2,"method":"account/read","params":{"refreshToken":false}}),
                ));
                self.expected = 2;
            }
            2 => {
                if result
                    .get("account")
                    .and_then(|a| a.get("type"))
                    .and_then(Value::as_str)
                    != Some("chatgpt")
                {
                    return Err(QuotaStatus::SignInRequired);
                }
                self.pending =
                    frame(json!({"id":3,"method":"account/rateLimits/read","params":{}}));
                self.expected = 3;
            }
            3 => return decode(result).map(Some),
            _ => return Err(QuotaStatus::Unavailable),
        }
        self.written = 0;
        Ok(None)
    }
}

fn frame(value: Value) -> Vec<u8> {
    let mut bytes = value.to_string().into_bytes();
    bytes.push(b'\n');
    bytes
}

fn decode(value: &Value) -> Result<QuotaSnapshot, QuotaStatus> {
    // Prefer the explicit general bucket on newer versions. Never combine model pools.
    let raw = value
        .get("rateLimitsByLimitId")
        .and_then(|m| m.get("codex"))
        .or_else(|| value.get("rateLimits"))
        .ok_or(QuotaStatus::Unavailable)?;
    let limits: CodexLimits =
        serde_json::from_value(raw.clone()).map_err(|_| QuotaStatus::Unavailable)?;
    let snapshot = limits.snapshot(now());
    if snapshot.is_empty() {
        return Err(QuotaStatus::Unavailable);
    }
    Ok(snapshot)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn protocol_reads_only_metadata_and_reaps_child() {
        let scratch = super::super::claude::Source::create().unwrap();
        let program = scratch.path().join("codex");
        let calls = scratch.path().join("calls");
        let pid = scratch.path().join("pid");
        fs::write(&program, format!(r#"#!/bin/sh
printf '%s' "$$" > '{}'
while IFS= read -r line; do
 printf '%s\n' "$line" >> '{}'
 case "$line" in
 *'"id":1'*) printf '%s\n' '{{"id":1,"result":{{}}}}';;
 *'"id":2'*) printf '%s\n' '{{"id":2,"result":{{"account":{{"type":"chatgpt"}}}}}}';;
 *'"id":3'*) printf '%s\n' '{{"id":3,"result":{{"rateLimits":{{"primary":{{"usedPercent":23,"windowDurationMins":300,"resetsAt":2000000000}},"secondary":{{"usedPercent":42,"windowDurationMins":10080,"resetsAt":2000000001}}}}}}}}';;
 esac
done
"#, pid.display(), calls.display())).unwrap();
        fs::set_permissions(&program, fs::Permissions::from_mode(0o700)).unwrap();
        let snapshot = read(
            &program,
            "/usr/bin:/bin",
            scratch.path(),
            &Probes::default(),
        )
        .unwrap();
        assert_eq!(snapshot.five_hour.unwrap().used_percent, 23.);
        assert_eq!(snapshot.weekly.unwrap().used_percent, 42.);
        let calls = fs::read_to_string(calls).unwrap();
        assert_eq!(calls.lines().count(), 4);
        assert!(
            !calls.contains("turn/start")
                && !calls.contains("thread/start")
                && !calls.contains("account/login")
        );
        let pid: i32 = fs::read_to_string(pid).unwrap().parse().unwrap();
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
    }

    #[test]
    fn quit_cancels_in_flight_probes_and_prevents_new_children() {
        let scratch = super::super::claude::Source::create().unwrap();
        let program = scratch.path().join("codex");
        fs::write(&program, "#!/bin/sh\nsleep 60\n").unwrap();
        fs::set_permissions(&program, fs::Permissions::from_mode(0o700)).unwrap();
        let probes = Probes::default();
        let start = Instant::now();
        std::thread::scope(|scope| {
            let worker = scope.spawn(|| read(&program, "/bin", scratch.path(), &probes));
            while probes.children.lock().unwrap().is_empty()
                && start.elapsed() < Duration::from_secs(2)
            {
                std::thread::sleep(Duration::from_millis(1));
            }
            assert!(!probes.children.lock().unwrap().is_empty());
            probes.stop();
            assert_eq!(worker.join().unwrap(), Err(QuotaStatus::Unavailable));
        });
        assert!(probes.children.lock().unwrap().is_empty());
        assert!(start.elapsed() < Duration::from_secs(3));
        assert_eq!(
            read(&program, "/bin", scratch.path(), &probes),
            Err(QuotaStatus::Unavailable)
        );
    }

    #[test]
    fn bounded_timeout_and_unknown_durations() {
        let scratch = super::super::claude::Source::create().unwrap();
        let program = scratch.path().join("codex");
        fs::write(&program, "#!/bin/sh\nsleep 60\n").unwrap();
        fs::set_permissions(&program, fs::Permissions::from_mode(0o700)).unwrap();
        let start = Instant::now();
        assert_eq!(
            read_with_timeout(
                &program,
                "/bin",
                scratch.path(),
                Duration::from_millis(50),
                &Probes::default()
            ),
            Err(QuotaStatus::TimedOut)
        );
        assert!(start.elapsed() < Duration::from_secs(2));
        assert!(decode(&json!({"rateLimits":{"primary":{"usedPercent":10,"windowDurationMins":15,"resetsAt":2000000000}}})).is_err());
    }
}
