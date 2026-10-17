//! Opt-in, isolated Core/PTy contention measurements. No real provider requests.
//! Run: cargo run -p shika-core --release --example usage_performance
//! This measures delivery to a sink, not GPUI rendering or native notifications.

use std::fs::{self, File};
use std::io::{BufRead, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::json;
use shika_core::{Core, LaunchOptions, PathEnv, PtyEvent, PtySize};

const ARCHIVES: usize = 32;
const HUGE_BYTES: usize = 16 * 1024 * 1024;
const BULK_BYTES: usize = 8 * 1024 * 1024;
const PROBES: usize = 200;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("--pty-child") => pty_child(),
        Some("--case") => measure(Path::new(&args[2]), args[3] == "on"),
        _ => run(),
    }
}

fn run() {
    let scratch = reserve();
    let executable = std::env::current_exe().unwrap();
    println!(
        "Core/PTy synthetic benchmark: archives={ARCHIVES}, giant_record_bytes={HUGE_BYTES}, pty_bulk_bytes={BULK_BYTES}, probes={PROBES}"
    );
    println!("Not a native rendering, notification, real-account, or wakeup-counter measurement.");
    for mode in ["off", "on"] {
        let root = scratch.join(mode);
        fs::create_dir(&root).unwrap();
        prepare(&root, &executable);
        let result = Command::new(&executable)
            .args(["--case", root.to_str().unwrap(), mode])
            .env_clear()
            .env("HOME", root.join("home"))
            .env("CLAUDE_CONFIG_DIR", root.join("claude"))
            .env("CODEX_HOME", root.join("codex"))
            .env("SHELL", root.join("login-shell"))
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
            .env("TMPDIR", &root)
            .status()
            .unwrap();
        assert!(result.success(), "benchmark case failed: {mode}");
    }
    fs::remove_dir_all(scratch).unwrap();
}

fn reserve() -> PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    for sequence in 0..1000 {
        let root = std::env::temp_dir().join(format!(
            "shika-usage-perf-{}-{stamp}-{sequence}",
            std::process::id()
        ));
        match fs::create_dir(&root) {
            Ok(()) => return root.canonicalize().unwrap(),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => panic!("reserve fixture: {error}"),
        }
    }
    panic!("could not reserve fixture");
}

fn script(path: &Path, contents: String) {
    fs::write(path, contents).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

fn quoted(path: &Path) -> String {
    format!("'{}'", path.to_str().unwrap().replace('\'', "'\\''"))
}

fn prepare(root: &Path, executable: &Path) {
    for name in [
        "home",
        "claude",
        "codex",
        "bin",
        "repo",
        "data/usage-sources",
    ] {
        fs::create_dir_all(root.join(name)).unwrap();
    }
    script(
        &root.join("login-shell"),
        format!(
            "#!/bin/sh\nprintf '%s' {}\n",
            quoted(&root.join("bin")) + ":/usr/bin:/bin"
        ),
    );
    script(
        &root.join("bin/claude"),
        format!("#!/bin/sh\nexec {} --pty-child\n", quoted(executable)),
    );
    git(&root.join("repo"), &["init", "-b", "main"]);
    git(
        &root.join("repo"),
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "--allow-empty",
            "-m",
            "fixture",
        ],
    );
    // Path-only source journal fixtures model closed sessions at startup.
    for index in 0..=ARCHIVES {
        let directory = root.join(format!("archive-{index}"));
        fs::create_dir(&directory).unwrap();
        fs::write(
            root.join(format!("data/usage-sources/{index}.json")),
            serde_json::to_vec(&json!({"cli":"claude","directories":[directory]})).unwrap(),
        )
        .unwrap();
        let mut file = File::create(directory.join("session.jsonl")).unwrap();
        let bytes = if index == ARCHIVES {
            HUGE_BYTES
        } else {
            64 * 1024
        };
        let records = if index == ARCHIVES { 1 } else { 4 };
        for record in 0..records {
            write!(file, "{{\"type\":\"assistant\",\"message\":{{\"id\":\"archive-{index}-{record}\",\"content\":[{{\"type\":\"text\",\"text\":\"").unwrap();
            let block = [b'x'; 64 * 1024];
            for _ in 0..bytes / block.len() {
                file.write_all(&block).unwrap();
            }
            file.write_all(b"\"}],\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}}\n")
                .unwrap();
        }
    }
}

fn git(root: &Path, args: &[&str]) {
    let output = Command::new("/usr/bin/git")
        .arg("-C")
        .arg(root)
        .args(args)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git fixture failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn pty_child() {
    // Remove local echo so round-trip measurements require the child's reply.
    let status = Command::new("/bin/stty").arg("-echo").status().unwrap();
    assert!(status.success());
    println!("PERF_READY");
    let input = std::io::stdin();
    let mut output = std::io::stdout().lock();
    for line in input.lock().lines() {
        let line = line.unwrap();
        if line == "bulk" {
            let block = [b'b'; 64 * 1024];
            for _ in 0..BULK_BYTES / block.len() {
                output.write_all(&block).unwrap();
            }
            writeln!(output, "\nPERF_BULK_DONE").unwrap();
        } else {
            writeln!(output, "PERF_REPLY:{line}").unwrap();
        }
        output.flush().unwrap();
    }
}

fn until(rx: &Receiver<PtyEvent>, marker: &str) -> usize {
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut bytes = 0;
    let mut tail = Vec::new();
    loop {
        match rx
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .expect("PTY timed out")
        {
            PtyEvent::Output(data) => {
                bytes += data.len();
                tail.extend_from_slice(&data);
                if tail
                    .windows(marker.len())
                    .any(|chunk| chunk == marker.as_bytes())
                {
                    return bytes;
                }
                if tail.len() > marker.len() {
                    tail.drain(..tail.len() - marker.len());
                }
            }
            PtyEvent::Exit(exit) => panic!("fixture CLI exited: {exit:?}"),
        }
    }
}

fn resources() -> (f64, u64) {
    // getrusage covers this process, excluding the stand-in CLI child.
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
    assert_eq!(
        unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) },
        0
    );
    let usage = unsafe { usage.assume_init() };
    let seconds = usage.ru_utime.tv_sec as f64
        + usage.ru_utime.tv_usec as f64 / 1e6
        + usage.ru_stime.tv_sec as f64
        + usage.ru_stime.tv_usec as f64 / 1e6;
    #[cfg(target_os = "macos")]
    let rss = usage.ru_maxrss as u64;
    #[cfg(not(target_os = "macos"))]
    let rss = usage.ru_maxrss as u64 * 1024;
    (seconds, rss)
}

fn measure(root: &Path, enabled: bool) {
    let env = PathEnv::capture(&["claude"]);
    assert!(env.get("claude").unwrap().starts_with(root));
    let core = Core::open_with(root.join("data"), env).unwrap();
    let project = core.add_project(&root.join("repo")).unwrap().project;
    let before = resources();
    let collection_start = Instant::now();
    if enabled {
        core.start_usage_collection();
    }
    let (tx, rx) = mpsc::channel();
    let session = core
        .create_session(
            &project.id,
            "claude",
            PtySize::default(),
            move |_, event| {
                let _ = tx.send(event);
            },
            LaunchOptions::default(),
        )
        .unwrap();
    until(&rx, "PERF_READY");
    // Ensure the first coalesced recovery job can start before throughput work.
    std::thread::sleep(Duration::from_millis(350));
    let bulk_start = Instant::now();
    core.write(session.pty.unwrap(), b"bulk\n").unwrap();
    let bulk_received = until(&rx, "PERF_BULK_DONE");
    let bulk_seconds = bulk_start.elapsed().as_secs_f64();
    assert!(bulk_received >= BULK_BYTES);
    let mut samples = Vec::new();
    for index in 0..PROBES {
        let start = Instant::now();
        core.write(session.pty.unwrap(), format!("probe-{index}\n").as_bytes())
            .unwrap();
        until(&rx, &format!("PERF_REPLY:probe-{index}\r\n"));
        samples.push(start.elapsed().as_secs_f64() * 1000.0);
        std::thread::sleep(Duration::from_millis(20));
    }
    samples.sort_by(f64::total_cmp);
    let active = resources();
    let active_seconds = collection_start.elapsed().as_secs_f64();
    let tokens_after_probes = core.token_usage().claude;
    let expected = (ARCHIVES * 4 * 2 + 2) as u64;
    let mut all_receipts_seconds = None;
    // Wait for the bounded settling burst to finish, without scheduling reads.
    // Worker idleness is a metadata diagnostic, independent of collection
    // warnings and native rendering/notification acceptance.
    if enabled {
        let deadline = Instant::now() + Duration::from_secs(90);
        let mut idle_samples = 0;
        loop {
            let report = core.token_usage();
            if report.claude == expected && all_receipts_seconds.is_none() {
                all_receipts_seconds = Some(collection_start.elapsed().as_secs_f64());
            }
            if report.claude == expected && core.usage_collection_idle() {
                idle_samples += 1;
            } else {
                idle_samples = 0;
            }
            if idle_samples >= 3 {
                assert!(!report.unreadable);
                assert!(!report.pending, "valid fixtures have a collection issue");
                break;
            }
            assert!(Instant::now() < deadline, "usage backlog never settled");
            std::thread::sleep(Duration::from_millis(200));
        }
    }
    let settled_seconds = collection_start.elapsed().as_secs_f64();
    let idle_before = resources();
    std::thread::sleep(Duration::from_secs(3));
    let idle_after = resources();
    println!(
        "{}",
        json!({"usage_enabled":enabled,"bulk_mib_per_second":BULK_BYTES as f64 / 1048576.0 / bulk_seconds,"round_trip_ms":{"p50":samples[PROBES/2],"p95":samples[PROBES*95/100],"p99":samples[PROBES*99/100],"max":samples[PROBES-1]},"active_wall_seconds":active_seconds,"active_process_cpu_seconds":active.0-before.0,"peak_process_rss_mib":idle_after.1 as f64 / 1048576.0,"tokens_after_probes":tokens_after_probes,"all_receipts_seconds":all_receipts_seconds,"backlog_settled_seconds":settled_seconds,"total_process_cpu_seconds":idle_before.0-before.0,"idle_3s_process_cpu_seconds":idle_after.0-idle_before.0,"idle_pending":core.token_usage().pending})
    );
    core.session_discard(&session.id).unwrap();
    // Keep the receiving end alive until Core hangs up all owned PTYs.
    drop(core);
    drop(rx);
}
