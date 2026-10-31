use super::*;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::pty::tests::{channel_sink, collect_to_exit, collect_until};

mod base_branch;
mod branch_switch_close;
mod checkout;
mod close;
mod diff;
mod external_rename;
mod lead;
mod lead_chat;
mod lead_lifecycle;
mod lead_resume;
mod lead_safety;
mod naming;
mod recovery;
mod sessions;
mod switch_agent;
mod terminal_tasks;
mod usage;

struct Scratch {
    path: PathBuf,
}

impl Scratch {
    fn new() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        loop {
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path =
                std::env::temp_dir().join(format!("shika-core-{}-{nanos}-{n}", std::process::id()));
            match fs::create_dir(&path) {
                // `/var` is a symlink; git reports resolved paths.
                Ok(()) => {
                    return Self {
                        path: path.canonicalize().unwrap(),
                    };
                }
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(err) => panic!("could not reserve core fixture: {err}"),
            }
        }
    }

    fn repo(&self, name: &str) -> PathBuf {
        let dir = self.path.join(name);
        fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "-b", "main"]);
        git(&dir, &["commit", "--allow-empty", "-m", "init"]);
        dir
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn git(repo: &Path, args: &[&str]) -> String {
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
    assert!(output.status.success(), "git {args:?} failed");
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// A stand-in for the agent CLI: prints its arguments, then echoes lines.
fn fake_cli(dir: &Path) -> PathBuf {
    let bin = dir.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let cli = bin.join("claude");
    fs::write(
        &cli,
        "#!/bin/sh\nprintf 'args:%s\\n' \"$*\"\nprintf 'cwd:%s\\n' \"$(pwd -P)\"\nwhile IFS= read -r line; do printf 'got:%s\\n' \"$line\"; done\n",
    )
    .unwrap();
    let mut perms = fs::metadata(&cli).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&cli, perms).unwrap();
    cli
}

fn core_with_fake_cli(scratch: &Scratch) -> Core {
    let cli = fake_cli(&scratch.path);
    let env = PathEnv::from_lookup(
        "/usr/bin:/bin".into(),
        None,
        &[("claude", Some(cli)), ("agent", None)],
    );
    Core::open_with(scratch.path.join("data"), env).unwrap()
}

fn create_fake_session(core: &Core, project_id: &str) -> Session {
    let (sink, _rx) = channel_sink();
    core.create_session(
        project_id,
        "claude",
        PtySize::new(30, 90),
        sink,
        LaunchOptions::default(),
    )
    .unwrap()
}

fn local_remote(scratch: &Scratch, repo: &Path) -> PathBuf {
    let remote = scratch.path.join("remote.git");
    fs::create_dir_all(&remote).unwrap();
    git(&remote, &["init", "--bare"]);
    git(repo, &["remote", "add", "origin", remote.to_str().unwrap()]);
    git(repo, &["push", "-u", "origin", "main"]);
    remote
}

/// `demo` on main, pushed to a bare `origin` whose HEAD is main, with a
/// `dev` one commit ahead of main that exists only on origin.
fn repo_with_dev_on_origin(scratch: &Scratch) -> (PathBuf, PathBuf) {
    let repo = scratch.repo("demo");
    let remote = local_remote(scratch, &repo);
    git(&repo, &["remote", "set-head", "origin", "main"]);
    git(&repo, &["switch", "-c", "dev"]);
    fs::write(repo.join("dev.txt"), "dev\n").unwrap();
    git(&repo, &["add", "dev.txt"]);
    git(&repo, &["commit", "-m", "dev work"]);
    git(&repo, &["push", "origin", "dev"]);
    git(&repo, &["switch", "main"]);
    git(&repo, &["branch", "-D", "dev"]);
    (repo, remote)
}

fn rev(dir: &Path, reference: &str) -> String {
    git(dir, &["rev-parse", reference]).trim().to_string()
}

fn has_upstream(worktree: &Path) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(worktree)
        .args(["rev-parse", "--abbrev-ref", "@{upstream}"])
        .output()
        .unwrap()
        .status
        .success()
}

fn branch_exists(repo: &Path, branch: &str) -> bool {
    !git(repo, &["branch", "--list", branch]).trim().is_empty()
}

const LEAD_INSTRUCTIONS: &str = "You lead this project. Run \"shika help\" first.";
const LEAD_BOOTSTRAP: &str = "Bootstrap: you are the Lead; read the guide.";

fn core_with_reporting_cli(scratch: &Scratch) -> Core {
    let mut core = core_with_switch_clis(scratch);
    core.cli_home = Some(CliHome::at(scratch.path.join("home")));
    core
}

fn lead_launch(scratch: &Scratch) -> LeadLaunch {
    LeadLaunch {
        socket: scratch.path.join("control/sock"),
        token: "lead-token".into(),
        bin_dir: scratch.path.join("control/bin"),
        instructions: LEAD_INSTRUCTIONS.into(),
        cursor_bootstrap: Some(LEAD_BOOTSTRAP.into()),
    }
}

fn start_lead_with(
    core: &Core,
    scratch: &Scratch,
    project_id: &str,
    preset: &str,
) -> (LeadStart, String) {
    let (sink, rx) = channel_sink();
    let start = core
        .start_lead(
            project_id,
            preset,
            PtySize::new(30, 90),
            sink,
            lead_launch(scratch),
        )
        .unwrap();
    let report = collect_until(&rx, "end-report", Duration::from_secs(5));
    (start, report)
}

fn start_lead(core: &Core, scratch: &Scratch, project_id: &str) -> (Session, String) {
    let (start, report) = start_lead_with(core, scratch, project_id, "claude");
    (start.session, report)
}

fn branches(repo: &Path) -> String {
    git(repo, &["branch", "--format=%(refname)"])
}

/// Prints every launch argument, then idles. A prompt must arrive as one
/// positional argument after the preset's own flags. An empty prompt adds none.
const SWITCH_CLI: &str = "#!/bin/sh\nlast=\nfor a in \"$@\"; do\n  printf 'arg:%s\\n' \"$a\"\n  last=$a\ndone\nprintf 'last:%s\\n' \"$last\"\nprintf 'cwd:%s\\n' \"$(pwd -P)\"\nprintf 'sock:%s\\n' \"$SHIKA_SOCKET\"\nprintf 'token:%s\\n' \"$SHIKA_TOKEN\"\nprintf 'path:%s\\n' \"$PATH\"\nprintf 'end-report\\n'\nwhile IFS= read -r line; do :; done\n";

fn write_exec(dir: &Path, name: &str, body: &str) -> PathBuf {
    let bin = dir.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let path = bin.join(name);
    fs::write(&path, body).unwrap();
    let mut perms = fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&path, perms).unwrap();
    path
}

fn core_with_switch_clis(scratch: &Scratch) -> Core {
    let claude = write_exec(&scratch.path, "claude", SWITCH_CLI);
    let agent = write_exec(&scratch.path, "agent", SWITCH_CLI);
    let pi = write_exec(&scratch.path, "pi", SWITCH_CLI);
    let codex = write_exec(&scratch.path, "codex", SWITCH_CLI);
    let env = PathEnv::from_lookup(
        "/usr/bin:/bin".into(),
        None,
        &[
            ("claude", Some(claude)),
            ("agent", Some(agent)),
            ("pi", Some(pi)),
            ("codex", Some(codex)),
        ],
    );
    Core::open_with(scratch.path.join("data"), env).unwrap()
}

fn report_args(report: &str) -> Vec<String> {
    report
        .lines()
        .filter_map(|line| line.trim().strip_prefix("arg:").map(str::to_string))
        .collect()
}

fn switch_agent(
    core: &Core,
    id: &str,
    preset: &str,
    prompt: Option<&str>,
    control: Option<WorkerEnv>,
) -> (Session, String) {
    let (sink, rx) = channel_sink();
    let session = core
        .session_switch_agent(id, preset, prompt, PtySize::new(30, 90), sink, control)
        .unwrap();
    let report = collect_until(&rx, "end-report", Duration::from_secs(5));
    (session, report)
}

/// Every file of the repository's git state that a write would change.
/// Read without optional locks, so taking it never refreshes the index.
fn git_snapshot(repo: &Path) -> String {
    let read = |args: &[&str]| {
        let output = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .env("GIT_OPTIONAL_LOCKS", "0")
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?}");
        String::from_utf8_lossy(&output.stdout).into_owned()
    };
    [
        format!(
            "{:?}",
            fs::metadata(repo.join(".git/index"))
                .and_then(|m| m.modified())
                .ok()
        ),
        read(&[
            "status",
            "--porcelain=v2",
            "--branch",
            "--untracked-files=all",
        ]),
        read(&["for-each-ref"]),
        read(&["worktree", "list", "--porcelain"]),
        read(&["stash", "list"]),
        fs::read_to_string(repo.join(".git/info/exclude")).unwrap_or_default(),
    ]
    .join("\n---\n")
}
