//! One-shot model metadata from the installed CLI. No prompt, Lead worktree,
//! Shika credentials, or active conversation is involved.
use std::collections::HashSet;
use std::io::{Read, Write};
use std::os::{
    fd::AsRawFd,
    unix::{fs::DirBuilderExt, process::CommandExt},
};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::valid_lead_model_id;
use crate::{CliPreset, Core};

const TIMEOUT: Duration = Duration::from_secs(8);
const MAX_OUTPUT: usize = 4 * 1024 * 1024;
const MAX_MODELS: usize = 256;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LeadModelChoice {
    pub id: String,
    pub label: String,
}

impl Core {
    /// Blocking, metadata-only. None when the installed CLI cannot be queried.
    pub fn lead_models(&self, preset: &CliPreset) -> Option<Vec<LeadModelChoice>> {
        let mut models = read_models(&preset.id, preset.path.as_deref()?, self.path_env().path())?;
        let mut seen = HashSet::new();
        models.retain(|m| seen.insert(m.id.clone()));
        models.truncate(MAX_MODELS);
        (!models.is_empty()).then_some(models)
    }
}

fn read_models(cli: &str, program: &Path, path: &str) -> Option<Vec<LeadModelChoice>> {
    let cwd = Scratch::new()?;
    let args: &[&str] = match cli {
        "pi" => &["--list-models"],
        "claude" => &[
            "-p",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--verbose",
            "--no-session-persistence",
        ],
        "codex" => &["app-server", "--listen", "stdio://"],
        "cursor" => &["acp"],
        _ => return None,
    };
    let mut probe = Probe::spawn(program, args, &cwd.0, path, cli != "pi")?;
    match cli {
        "pi" => {
            while probe.read()? {}
            // EOF may race with exit, or a child may close stdout then hang.
            loop {
                if let Some(status) = probe.child.try_wait().ok()? {
                    return status
                        .success()
                        .then(|| pi_models(&String::from_utf8_lossy(&probe.bytes)));
                }
                probe.pause()?;
            }
        }
        "claude" => {
            probe.send(json!({"type":"control_request", "request_id":"shika-models", "request":{"subtype":"initialize"}}))?;
            let response = probe.receive(|v| v["response"]["request_id"] == "shika-models")?;
            let models = &response["response"]["response"]["models"];
            Some(parse_models(models, "value", "displayName", |v| {
                v["value"] != "default"
            }))
        }
        "codex" => codex_models(&mut probe),
        "cursor" => {
            probe.send(json!({"jsonrpc":"2.0", "id":1, "method":"initialize", "params":{"protocolVersion":1, "clientCapabilities":{}}}))?;
            let init = probe.receive(|v| v["id"] == 1)?;
            if init["result"]["protocolVersion"] != 1 {
                return None;
            }
            probe.send(json!({"jsonrpc":"2.0", "id":2, "method":"session/new", "params":{"cwd":cwd.0, "mcpServers":[]}}))?;
            let response = probe.receive(|v| v["id"] == 2)?;
            Some(parse_models(
                &response["result"]["models"]["availableModels"],
                "modelId",
                "name",
                |_| true,
            ))
        }
        _ => None,
    }
}

fn codex_models(probe: &mut Probe) -> Option<Vec<LeadModelChoice>> {
    probe.send(json!({"id":1, "method":"initialize", "params":{"clientInfo":{"name":"shika", "version":env!("CARGO_PKG_VERSION")}}}))?;
    probe.receive(|v| v["id"] == 1)?.get("result")?;
    probe.send(json!({"method":"initialized"}))?;
    let mut models = Vec::new();
    let mut cursor: Option<String> = None;
    for id in 2..=5 {
        probe.send(json!({"id":id, "method":"model/list", "params":{"limit":100, "includeHidden":false, "cursor":cursor}}))?;
        let response = probe.receive(|v| v["id"] == id)?;
        let result = response.get("result")?;
        models.extend(parse_models(&result["data"], "model", "displayName", |v| {
            v["hidden"] != true
        }));
        let next = result["nextCursor"]
            .as_str()
            .filter(|s| !s.is_empty() && s.len() <= 1024);
        if next.is_none() || next == cursor.as_deref() || models.len() >= MAX_MODELS {
            break;
        }
        cursor = next.map(str::to_owned);
    }
    Some(models)
}

fn choice(id: &str, label: &str) -> Option<LeadModelChoice> {
    if id.starts_with('-') || !valid_lead_model_id(id) {
        return None;
    }
    let label: String = label
        .chars()
        .filter(|c| !c.is_control())
        .take(120)
        .collect();
    Some(LeadModelChoice {
        id: id.into(),
        label: if label.trim().is_empty() {
            id.into()
        } else {
            label.trim().into()
        },
    })
}

fn parse_models(
    v: &Value,
    id: &str,
    label: &str,
    keep: impl Fn(&Value) -> bool,
) -> Vec<LeadModelChoice> {
    v.as_array()
        .into_iter()
        .flatten()
        .filter(|v| keep(v))
        .filter_map(|v| choice(v[id].as_str()?, v[label].as_str().unwrap_or_default()))
        .take(MAX_MODELS)
        .collect()
}

fn pi_models(text: &str) -> Vec<LeadModelChoice> {
    text.lines()
        .skip_while(|line| !line.starts_with("provider "))
        .skip(1)
        .filter_map(|line| {
            let fields: Vec<_> = line.split_whitespace().take(7).collect();
            let [provider, model, _, _, "yes" | "no", "yes" | "no"] = fields.as_slice() else {
                return None;
            };
            let id = format!("{provider}/{model}");
            choice(&id, &id)
        })
        .take(MAX_MODELS)
        .collect()
}

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Option<Self> {
        let path =
            std::env::temp_dir().join(format!("shika-models-{}", crate::control::new_token()));
        std::fs::DirBuilder::new().mode(0o700).create(&path).ok()?;
        Some(Self(path))
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Probe {
    child: Child,
    input: Option<ChildStdin>,
    output: ChildStdout,
    bytes: Vec<u8>,
    scanned: usize,
    total: usize,
    deadline: Instant,
}
impl Probe {
    fn spawn(program: &Path, args: &[&str], cwd: &Path, path: &str, rpc: bool) -> Option<Self> {
        let mut cmd = Command::new(program);
        cmd.args(args)
            .current_dir(cwd)
            .env("PATH", path)
            .env("PWD", cwd)
            .env("NO_COLOR", "1")
            .process_group(0)
            .stdin(if rpc { Stdio::piped() } else { Stdio::null() })
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        for key in crate::worktree::GIT_REDIRECTS
            .into_iter()
            .chain(crate::pty::CONTROL_VARS)
            .chain(["CLAUDE_CODE_CHILD_SESSION", "CLAUDECODE"])
        {
            cmd.env_remove(key);
        }
        let mut child = cmd.spawn().ok()?;
        let input = child.stdin.take();
        let output = child.stdout.take().expect("piped stdout");
        let probe = Self {
            child,
            input,
            output,
            bytes: Vec::new(),
            scanned: 0,
            total: 0,
            deadline: Instant::now() + TIMEOUT,
        };
        nonblocking(probe.output.as_raw_fd())?;
        if let Some(input) = &probe.input {
            nonblocking(input.as_raw_fd())?;
        }
        Some(probe)
    }
    fn pause(&self) -> Option<()> {
        if Instant::now() >= self.deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(10));
        Some(())
    }
    fn send(&mut self, value: Value) -> Option<()> {
        let mut frame = serde_json::to_vec(&value).ok()?;
        frame.push(b'\n');
        let mut remaining = frame.as_slice();
        while !remaining.is_empty() && Instant::now() < self.deadline {
            match self.input.as_mut()?.write(remaining) {
                Ok(n) if n > 0 => remaining = &remaining[n..],
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => self.pause()?,
                _ => return None,
            }
        }
        remaining.is_empty().then_some(())
    }
    /// False on EOF. The whole probe is byte- and time-bounded.
    fn read(&mut self) -> Option<bool> {
        let mut chunk = [0; 8192];
        loop {
            if Instant::now() >= self.deadline {
                return None;
            }
            match self.output.read(&mut chunk) {
                Ok(0) => return Some(false),
                Ok(n) => {
                    self.total += n;
                    if self.total > MAX_OUTPUT {
                        return None;
                    }
                    self.bytes.extend_from_slice(&chunk[..n]);
                    return Some(true);
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => self.pause()?,
                _ => return None,
            }
        }
    }
    fn receive(&mut self, matches: impl Fn(&Value) -> bool) -> Option<Value> {
        loop {
            while let Some(end) = self.bytes[self.scanned..].iter().position(|&b| b == b'\n') {
                let end = self.scanned + end + 1;
                let parsed = serde_json::from_slice(&self.bytes[self.scanned..end]);
                self.scanned = end;
                if let Ok(value) = parsed
                    && matches(&value)
                {
                    return Some(value);
                }
            }
            self.bytes.drain(..self.scanned);
            self.scanned = 0;
            if !self.read()? {
                return None;
            }
        }
    }
}
impl Drop for Probe {
    fn drop(&mut self) {
        // Kill the owned group before reaping so the PID cannot be reused.
        if self.child.try_wait().ok().flatten().is_none() {
            unsafe {
                libc::kill(-(self.child.id() as i32), libc::SIGKILL);
            }
            let _ = self.child.wait();
        }
    }
}
fn nonblocking(fd: i32) -> Option<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    (flags >= 0 && unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } >= 0)
        .then_some(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pi_list_keeps_distinct_providers_and_rejects_noise() {
        let models = pi_models(
            "startup message\nprovider  model  context max-out thinking images\nfirst m1 100K 20K yes no\nsecond m1 100K 20K no yes\nnot a table row\n",
        );
        assert_eq!(
            models.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            ["first/m1", "second/m1"]
        );
    }

    #[test]
    fn codex_probe_reads_pages_without_starting_a_thread() {
        use std::os::unix::fs::PermissionsExt;

        let fixture = Scratch::new().unwrap();
        let program = fixture.0.join("codex");
        std::fs::write(&program, r#"#!/bin/sh
while IFS= read -r line; do
    case "$line" in
      *'"method":"initialize"'*) printf '%s\n' '{"id":1,"result":{}}';;
      *'"method":"initialized"'*) ;;
      *'"id":2'*'"method":"model/list"'*) printf '%s\n' '{"id":2,"result":{"data":[{"model":"first"}],"nextCursor":"page-two"}}';;
      *'"id":3'*'"method":"model/list"'*)
          case "$line" in *'"cursor":"page-two"'*) printf '%s\n' '{"id":3,"result":{"data":[{"model":"second"}],"nextCursor":null}}';; *) exit 5;; esac;;
      *) exit 6;;
    esac
done
"#).unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        let models = read_models("codex", &program, "/usr/bin:/bin").unwrap();
        assert_eq!(
            models.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            ["first", "second"]
        );
    }

    #[test]
    fn json_list_uses_launch_id_and_skips_hidden_or_invalid_choices() {
        let models = parse_models(
            &json!([
                {"id":"internal", "model":"gpt-test", "displayName":"GPT Test"},
                {"model":"secret", "hidden":true}, {"model":"--help"}, {"model":"bad model"}
            ]),
            "model",
            "displayName",
            |v| v["hidden"] != true,
        );
        assert_eq!(
            models,
            vec![LeadModelChoice {
                id: "gpt-test".into(),
                label: "GPT Test".into()
            }]
        );
    }
}
