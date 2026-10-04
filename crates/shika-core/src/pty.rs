use std::collections::HashMap;
use std::fmt;
use std::io::{ErrorKind, Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;

use portable_pty::{Child, ChildKiller, CommandBuilder, MasterPty, native_pty_system};

use crate::error::{Error, Result};
use crate::worktree::GIT_REDIRECTS;

const READ_CHUNK: usize = 64 * 1024;

/// One live PTY. Ids are never reused within a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PtyId(pub(crate) u64);

impl fmt::Display for PtyId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "pty-{}", self.0)
    }
}

/// Terminal size in character cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PtySize {
    pub rows: u16,
    pub cols: u16,
}

impl PtySize {
    pub fn new(rows: u16, cols: u16) -> Self {
        Self { rows, cols }
    }

    /// Anything under 2x2 is a view that has not been laid out yet.
    fn usable(self) -> bool {
        self.rows >= 2 && self.cols >= 2
    }
}

impl Default for PtySize {
    /// The size the Tauri build opened every PTY at before the first fit.
    fn default() -> Self {
        Self {
            rows: 32,
            cols: 100,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PtyExit {
    /// The process exit code, or 1 when a signal ended it.
    pub code: u32,
    /// The signal's name when one ended the process.
    pub signal: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PtyEvent {
    /// Raw bytes exactly as the PTY produced them. Not decoded, not split on
    /// UTF-8 or escape boundaries.
    Output(Vec<u8>),
    /// The process ended. Always the last event for that PTY, sent after the
    /// last output.
    Exit(PtyExit),
}

/// Where one PTY's output goes. Called on that PTY's own reader thread.
///
/// The reader thread reads as fast as the PTY produces, whether or not the
/// terminal is visible, so a CLI never blocks on a full PTY buffer. `send`
/// must therefore never block: hand the event to an unbounded channel and
/// wake the UI from there. Core never touches the UI thread.
pub trait PtySink: Send + 'static {
    fn send(&mut self, pty: PtyId, event: PtyEvent);
}

impl<F> PtySink for F
where
    F: FnMut(PtyId, PtyEvent) + Send + 'static,
{
    fn send(&mut self, pty: PtyId, event: PtyEvent) {
        self(pty, event)
    }
}

pub struct SpawnRequest {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub path: String,
    pub size: PtySize,
    pub env: Vec<(String, String)>,
}

struct LivePty {
    master: Box<dyn MasterPty + Send>,
    input: Sender<Vec<u8>>,
    killer: Box<dyn ChildKiller + Send + Sync>,
    exited: Arc<AtomicBool>,
}

impl Drop for LivePty {
    fn drop(&mut self) {
        // Once the reader thread has reaped the child its pid may belong to
        // another process, so only signal a child that is still ours.
        if !self.exited.load(Ordering::Acquire) {
            let _ = self.killer.kill();
        }
    }
}

pub struct PtyHub {
    next: AtomicU64,
    inner: Mutex<HashMap<PtyId, LivePty>>,
}

impl PtyHub {
    pub fn new() -> Self {
        Self {
            next: AtomicU64::new(1),
            inner: Mutex::new(HashMap::new()),
        }
    }

    /// Spawns the program on a new PTY. `sink` gets every byte and then the
    /// exit, from a reader thread that lives as long as the process.
    pub fn open(&self, request: SpawnRequest, sink: impl PtySink) -> Result<PtyId> {
        let id = PtyId(self.next.fetch_add(1, Ordering::Relaxed));
        let live = spawn_pty(id, request, sink)?;
        self.inner
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .insert(id, live);
        Ok(id)
    }

    /// Queues input for the PTY. Never blocks: a writer thread per PTY does
    /// the write, so a CLI that stops reading cannot stall the caller.
    pub fn write(&self, id: PtyId, bytes: &[u8]) -> Result<()> {
        let guard = self.inner.lock().unwrap_or_else(|err| err.into_inner());
        let live = guard.get(&id).ok_or(Error::UnknownPty)?;
        live.input.send(bytes.to_vec()).map_err(|_| Error::WritePty)
    }

    pub fn resize(&self, id: PtyId, size: PtySize) -> Result<()> {
        if !size.usable() {
            return Ok(());
        }
        let guard = self.inner.lock().unwrap_or_else(|err| err.into_inner());
        let live = guard.get(&id).ok_or(Error::UnknownPty)?;
        live.master
            .resize(native_size(size))
            .map_err(|_| Error::ResizePty)
    }

    /// Hangs up the PTY. Its sink still gets the exit.
    pub fn close(&self, id: PtyId) {
        let live = self
            .inner
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .remove(&id);
        drop(live);
    }
}

fn spawn_pty(id: PtyId, request: SpawnRequest, sink: impl PtySink) -> Result<LivePty> {
    if request
        .args
        .iter()
        .any(|arg| arg == "--worktree" || arg.starts_with("--worktree="))
    {
        return Err(Error::OpenPty(None));
    }
    let size = if request.size.usable() {
        request.size
    } else {
        PtySize::new(24, 80)
    };
    let pty = native_pty_system();
    let pair = pty
        .openpty(native_size(size))
        .map_err(|err| pty_error(&err))?;
    let mut cmd = CommandBuilder::new(&request.program);
    cmd.args(&request.args);
    configure_child(&mut cmd, &request);
    let child = pair
        .slave
        .spawn_command(cmd)
        .map_err(|err| pty_error(&err))?;
    // Only the child may hold the slave, or the reader never sees EOF.
    drop(pair.slave);
    let reader = pair
        .master
        .try_clone_reader()
        .map_err(|err| pty_error(&err))?;
    let writer = pair.master.take_writer().map_err(|err| pty_error(&err))?;
    let killer = child.clone_killer();
    let exited = Arc::new(AtomicBool::new(false));
    let (input, queued) = mpsc::channel();
    thread::Builder::new()
        .name(format!("shika-{id}-write"))
        .spawn(move || feed(writer, queued))
        .map_err(|err| pty_error(&err))?;
    let reaped = exited.clone();
    thread::Builder::new()
        .name(format!("shika-{id}-read"))
        .spawn(move || pump(id, reader, child, sink, reaped))
        .map_err(|err| pty_error(&err))?;
    Ok(LivePty {
        master: pair.master,
        input,
        killer,
        exited,
    })
}

fn configure_child(cmd: &mut CommandBuilder, request: &SpawnRequest) {
    cmd.cwd(request.cwd.as_os_str());
    if !request.path.is_empty() {
        cmd.env("PATH", &request.path);
    }
    cmd.env("TERM", "xterm-256color");
    if cmd.get_env("LANG").is_none() {
        cmd.env("LANG", "en_US.UTF-8");
    }
    // The app process is standing somewhere else. Point PWD at the worktree
    // so a login shell reports that directory instead of the parent's.
    cmd.env("PWD", request.cwd.as_os_str());
    for (key, value) in &request.env {
        cmd.env(key, value);
    }
    // Inherited git variables would make status and diff follow the main
    // checkout even though the shell's cwd is the worktree.
    for key in GIT_REDIRECTS {
        cmd.env_remove(key);
    }
}

fn feed(mut writer: Box<dyn Write + Send>, queued: Receiver<Vec<u8>>) {
    for bytes in queued {
        if writer
            .write_all(&bytes)
            .and_then(|_| writer.flush())
            .is_err()
        {
            break;
        }
    }
}

fn pump(
    id: PtyId,
    mut reader: Box<dyn Read + Send>,
    mut child: Box<dyn Child + Send + Sync>,
    mut sink: impl PtySink,
    exited: Arc<AtomicBool>,
) {
    let mut buf = vec![0u8; READ_CHUNK];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => sink.send(id, PtyEvent::Output(buf[..n].to_vec())),
            Err(err) if err.kind() == ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
    let status = child.wait();
    exited.store(true, Ordering::Release);
    let exit = match status {
        Ok(status) => PtyExit {
            code: status.exit_code(),
            signal: status.signal().map(str::to_string),
        },
        Err(_) => PtyExit {
            code: 1,
            signal: None,
        },
    };
    sink.send(id, PtyEvent::Exit(exit));
}

fn native_size(size: PtySize) -> portable_pty::PtySize {
    portable_pty::PtySize {
        rows: size.rows,
        cols: size.cols,
        pixel_width: 0,
        pixel_height: 0,
    }
}

fn pty_error(err: &impl fmt::Display) -> Error {
    let text = err.to_string();
    let line = text.lines().next().unwrap_or("").trim();
    if line.is_empty() {
        Error::OpenPty(None)
    } else {
        Error::OpenPty(Some(line.to_string()))
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::ffi::OsStr;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;
    use std::sync::mpsc::RecvTimeoutError;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    pub(crate) type Events = mpsc::Receiver<PtyEvent>;

    /// A sink that forwards into a channel, the way the app is expected to.
    pub(crate) fn channel_sink() -> (impl PtySink, Events) {
        let (tx, rx) = mpsc::channel();
        let sink = move |_: PtyId, event: PtyEvent| {
            let _ = tx.send(event);
        };
        (sink, rx)
    }

    fn scratch(prefix: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("{prefix}-{nanos:x}-{n}"));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn request(program: &str, args: &[&str], cwd: PathBuf) -> SpawnRequest {
        SpawnRequest {
            program: PathBuf::from(program),
            args: args.iter().map(|arg| (*arg).to_string()).collect(),
            cwd,
            path: "/usr/bin:/bin".into(),
            size: PtySize::new(24, 80),
            env: Vec::new(),
        }
    }

    #[test]
    fn refuses_a_worktree_flag() {
        let hub = PtyHub::new();
        let (sink, _rx) = channel_sink();
        let mut req = request("/bin/sh", &["--worktree"], std::env::temp_dir());
        assert_eq!(hub.open(req, sink).unwrap_err(), Error::OpenPty(None));
        let (sink, _rx) = channel_sink();
        req = request("/bin/sh", &["--worktree=x"], std::env::temp_dir());
        assert_eq!(hub.open(req, sink).unwrap_err(), Error::OpenPty(None));
        assert_eq!(
            Error::OpenPty(None).to_string(),
            "Could not open the terminal."
        );
    }

    #[test]
    fn git_environment_cannot_redirect_the_worktree() {
        let mut cmd = CommandBuilder::new("/bin/zsh");
        cmd.env("GIT_DIR", "/repo/.git");
        cmd.env("GIT_WORK_TREE", "/repo");
        cmd.env("GIT_INDEX_FILE", "/repo/index");
        cmd.env("GIT_PREFIX", "src/");
        cmd.env("GIT_COMMON_DIR", "/repo/.git");
        cmd.env("GIT_OBJECT_DIRECTORY", "/repo/.git/objects");
        let worktree = PathBuf::from("/repo/.worktrees/task");
        configure_child(
            &mut cmd,
            &SpawnRequest {
                program: PathBuf::from("/bin/zsh"),
                args: vec!["-il".into()],
                cwd: worktree.clone(),
                path: "/usr/bin:/bin".into(),
                size: PtySize::default(),
                env: Vec::new(),
            },
        );
        for key in GIT_REDIRECTS {
            assert!(cmd.get_env(key).is_none(), "{key} was still set");
        }
        assert_eq!(
            cmd.get_cwd().map(|dir| dir.as_os_str()),
            Some(worktree.as_os_str())
        );
        assert_eq!(
            cmd.get_env("PWD"),
            Some(OsStr::new("/repo/.worktrees/task"))
        );
        assert_eq!(cmd.get_env("PATH"), Some(OsStr::new("/usr/bin:/bin")));
    }

    #[test]
    fn a_real_child_sees_no_git_redirects_and_pwd_on_the_worktree() {
        let dir = scratch("shika-env");
        let hub = PtyHub::new();
        let (sink, rx) = channel_sink();
        let mut req = request("/usr/bin/env", &[], dir.clone());
        // Even a variable handed in on purpose is scrubbed.
        req.env = GIT_REDIRECTS
            .iter()
            .map(|key| ((*key).to_string(), "/elsewhere".to_string()))
            .collect();
        hub.open(req, sink).unwrap();

        let (bytes, exit) = collect_to_exit(&rx, Duration::from_secs(5));
        let got = String::from_utf8_lossy(&bytes);
        let _ = fs::remove_dir_all(&dir);
        assert_eq!(exit.map(|exit| exit.code), Some(0), "{got}");
        for key in GIT_REDIRECTS {
            assert!(!got.contains(&format!("{key}=")), "{key} leaked: {got}");
        }
        assert!(got.contains(&format!("PWD={}", dir.display())), "{got}");
        assert!(got.contains("PATH=/usr/bin:/bin"), "{got}");
        assert!(got.contains("TERM=xterm-256color"), "{got}");
    }

    #[test]
    fn output_arrives_as_raw_bytes_and_the_exit_code_follows() {
        let hub = PtyHub::new();
        let (sink, rx) = channel_sink();
        hub.open(
            request(
                "/bin/sh",
                &["-c", r"printf '\377\376raw\033[31m'; exit 3"],
                std::env::temp_dir(),
            ),
            sink,
        )
        .unwrap();

        let (bytes, exit) = collect_to_exit(&rx, Duration::from_secs(5));
        let wanted: &[u8] = b"\xff\xferaw\x1b[31m";
        assert!(
            bytes.windows(wanted.len()).any(|window| window == wanted),
            "{bytes:?}"
        );
        assert_eq!(
            exit,
            Some(PtyExit {
                code: 3,
                signal: None
            })
        );
        assert!(rx.recv_timeout(Duration::from_millis(100)).is_err());
    }

    #[test]
    fn resize_reaches_the_child() {
        let hub = PtyHub::new();
        let (sink, rx) = channel_sink();
        let id = hub
            .open(
                request(
                    "/bin/sh",
                    &["-c", "printf 'ready\\n'; IFS= read -r line; stty size"],
                    std::env::temp_dir(),
                ),
                sink,
            )
            .unwrap();
        let ready = collect_until(&rx, "ready", Duration::from_secs(3));
        assert!(ready.contains("ready"), "{ready}");

        hub.resize(id, PtySize::new(40, 132)).unwrap();
        // A size nobody can draw in is ignored rather than sent to the CLI.
        hub.resize(id, PtySize::new(1, 0)).unwrap();
        hub.write(id, b"\r").unwrap();
        let got = collect_until(&rx, "40 132", Duration::from_secs(3));
        assert!(got.contains("40 132"), "{got}");
    }

    #[test]
    fn a_gone_pty_is_an_error() {
        let hub = PtyHub::new();
        assert_eq!(hub.write(PtyId(999), b"x").unwrap_err(), Error::UnknownPty);
        assert_eq!(
            hub.resize(PtyId(999), PtySize::default()).unwrap_err(),
            Error::UnknownPty
        );
        hub.close(PtyId(999));
    }

    #[test]
    fn close_hangs_up_and_the_exit_is_still_reported() {
        let hub = PtyHub::new();
        let (sink, rx) = channel_sink();
        let id = hub
            .open(
                request(
                    "/bin/sh",
                    &["-c", "printf 'ready\\n'; exec sleep 30"],
                    std::env::temp_dir(),
                ),
                sink,
            )
            .unwrap();
        let ready = collect_until(&rx, "ready", Duration::from_secs(3));
        assert!(ready.contains("ready"), "{ready}");
        let started = Instant::now();
        hub.close(id);
        let (_, exit) = collect_to_exit(&rx, Duration::from_secs(5));
        let exit = exit.expect("no exit after close");
        assert!(exit.signal.is_some(), "{exit:?}");
        assert!(started.elapsed() < Duration::from_secs(5));
        assert_eq!(hub.write(id, b"x").unwrap_err(), Error::UnknownPty);
    }

    #[test]
    fn output_is_drained_while_nobody_reads_it() {
        // A hidden terminal's consumer may not look at the channel for a
        // while. The CLI must still run to completion.
        let hub = PtyHub::new();
        let (tx, rx) = mpsc::channel();
        let done = Arc::new(AtomicBool::new(false));
        let seen = done.clone();
        hub.open(
            request(
                "/bin/sh",
                &["-c", "yes shika | head -c 4000000; exit 0"],
                std::env::temp_dir(),
            ),
            move |_: PtyId, event: PtyEvent| {
                if matches!(event, PtyEvent::Exit(_)) {
                    seen.store(true, Ordering::Release);
                }
                let _ = tx.send(event);
            },
        )
        .unwrap();
        let started = Instant::now();
        while !done.load(Ordering::Acquire) && started.elapsed() < Duration::from_secs(20) {
            thread::sleep(Duration::from_millis(20));
        }
        assert!(done.load(Ordering::Acquire), "the CLI stalled");
        let total: usize = rx
            .try_iter()
            .map(|event| match event {
                PtyEvent::Output(bytes) => bytes.len(),
                PtyEvent::Exit(_) => 0,
            })
            .sum();
        assert!(total >= 4_000_000, "only {total} bytes");
    }

    #[test]
    fn a_shell_sees_the_worktree_diff_not_the_main_checkout() {
        let scratch = scratch("shika-shell");
        let repo = scratch.join("repo");
        fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-b", "main"]);
        fs::write(repo.join("shared.txt"), "base\n").unwrap();
        git(&repo, &["add", "shared.txt"]);
        git(&repo, &["commit", "-m", "init"]);
        let worktree = repo.join(".worktrees").join("task");
        let worktree_arg = worktree.to_string_lossy().into_owned();
        git(&repo, &["worktree", "add", "-b", "task", &worktree_arg]);
        fs::write(repo.join("shared.txt"), "main change\n").unwrap();
        fs::write(repo.join("main-only.txt"), "only main\n").unwrap();
        fs::write(worktree.join("shared.txt"), "task change\n").unwrap();
        fs::write(worktree.join("task-only.txt"), "only task\n").unwrap();

        let script = "printf 'PWD:%s\\n' \"$(pwd -P)\"\ngit status --porcelain\ngit --no-pager diff -- shared.txt\nprintf 'SHIKA_MARK\\n'\n";
        let hub = PtyHub::new();
        let (sink, rx) = channel_sink();
        let mut req = request("/bin/zsh", &["-f", "-c", script], worktree.clone());
        req.path = std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".into());
        hub.open(req, sink).unwrap();
        let got = collect_until(&rx, "SHIKA_MARK", Duration::from_secs(5));
        drop(hub);

        let pwd = got
            .lines()
            .find_map(|line| line.strip_prefix("PWD:"))
            .unwrap_or("");
        assert_eq!(
            fs::canonicalize(pwd.trim()).unwrap(),
            fs::canonicalize(&worktree).unwrap(),
            "{got}"
        );
        assert!(got.contains("task change"), "{got}");
        assert!(got.contains("task-only.txt"), "{got}");
        assert!(!got.contains("main change"), "{got}");
        assert!(!got.contains("main-only"), "{got}");
        let _ = fs::remove_dir_all(&scratch);
    }

    fn git(repo: &std::path::Path, args: &[&str]) {
        let mut command = Command::new("git");
        command.arg("-C").arg(repo).args(args);
        for key in ["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE"] {
            command.env_remove(key);
        }
        let status = command
            .env("GIT_AUTHOR_NAME", "Shika")
            .env("GIT_AUTHOR_EMAIL", "shika@example.com")
            .env("GIT_COMMITTER_NAME", "Shika")
            .env("GIT_COMMITTER_EMAIL", "shika@example.com")
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed");
    }

    #[test]
    fn a_pty_round_trip_keeps_the_typed_line() {
        let scratch = scratch("shika-pty");
        let script = echo_line_script(&scratch);
        let hub = PtyHub::new();
        let (sink, rx) = channel_sink();
        let id = hub.open(echo_request(&script, &scratch), sink).unwrap();

        let ready = collect_until(&rx, "ready", Duration::from_secs(3));
        assert!(ready.contains("ready"), "{ready}");
        hub.write(id, b"ping\r").unwrap();
        let got = collect_until(&rx, "got:ping", Duration::from_secs(3));
        assert!(got.contains("got:ping"), "{got}");
        drop(hub);
        let _ = fs::remove_dir_all(&scratch);
    }

    #[test]
    fn a_draft_survives_while_another_pty_runs() {
        let scratch = scratch("shika-pty");
        let script = echo_line_script(&scratch);
        let hub = PtyHub::new();
        let (first_sink, first_rx) = channel_sink();
        let (second_sink, second_rx) = channel_sink();
        let first = hub
            .open(echo_request(&script, &scratch), first_sink)
            .unwrap();
        let second = hub
            .open(echo_request(&script, &scratch), second_sink)
            .unwrap();
        assert_ne!(first, second);

        let first_ready = collect_until(&first_rx, "ready", Duration::from_secs(3));
        let second_ready = collect_until(&second_rx, "ready", Duration::from_secs(3));
        assert!(first_ready.contains("ready"), "{first_ready}");
        assert!(second_ready.contains("ready"), "{second_ready}");

        hub.write(first, b"dra").unwrap();
        let echoed = collect_until(&first_rx, "dra", Duration::from_secs(3));
        assert!(echoed.contains("dra"), "{echoed}");

        hub.write(second, b"other\r").unwrap();
        let other = collect_until(&second_rx, "got:other", Duration::from_secs(3));
        assert!(other.contains("got:other"), "{other}");
        assert!(!other.contains("got:draft"), "{other}");

        hub.write(first, b"ft\r").unwrap();
        let got = collect_until(&first_rx, "got:draft", Duration::from_secs(3));
        assert!(got.contains("got:draft"), "{got}");
        assert!(!got.contains("got:other"), "{got}");

        drop(hub);
        let _ = fs::remove_dir_all(&scratch);
    }

    fn echo_line_script(dir: &std::path::Path) -> PathBuf {
        let script = dir.join("echo-line");
        fs::write(
            &script,
            "#!/bin/sh\nprintf 'ready\\n'\nIFS= read -r line\nprintf 'got:%s\\n' \"$line\"\n",
        )
        .unwrap();
        let mut perms = fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&script, perms).unwrap();
        script
    }

    fn echo_request(script: &std::path::Path, cwd: &std::path::Path) -> SpawnRequest {
        SpawnRequest {
            program: script.to_path_buf(),
            args: Vec::new(),
            cwd: cwd.to_path_buf(),
            path: "/usr/bin:/bin".into(),
            size: PtySize::new(24, 80),
            env: Vec::new(),
        }
    }

    pub(crate) fn collect_until(rx: &Events, needle: &str, timeout: Duration) -> String {
        let start = Instant::now();
        let mut all = Vec::new();
        while start.elapsed() < timeout {
            match rx.recv_timeout(Duration::from_millis(40)) {
                Ok(PtyEvent::Output(chunk)) => {
                    all.extend(chunk);
                    let text = String::from_utf8_lossy(&all);
                    if text.contains(needle) {
                        return text.into_owned();
                    }
                }
                Ok(PtyEvent::Exit(_)) => break,
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
        String::from_utf8_lossy(&all).into_owned()
    }

    pub(crate) fn collect_to_exit(rx: &Events, timeout: Duration) -> (Vec<u8>, Option<PtyExit>) {
        let start = Instant::now();
        let mut all = Vec::new();
        while start.elapsed() < timeout {
            match rx.recv_timeout(Duration::from_millis(40)) {
                Ok(PtyEvent::Output(chunk)) => all.extend(chunk),
                Ok(PtyEvent::Exit(exit)) => return (all, Some(exit)),
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
        (all, None)
    }
}
