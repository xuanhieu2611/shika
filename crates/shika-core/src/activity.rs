//! Optional Pi lifecycle metadata. No terminal bytes or conversation data.
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde::Deserialize;

use crate::pty::{PtyEvent, PtyId, PtySink, SpawnRequest};

const MAX_REPORT_BYTES: u64 = 128;
const EXTENSION: &str = include_str!("pi-activity.ts");
static NEXT: AtomicU64 = AtomicU64::new(0);

/// Latest authoritative lifecycle report, not a history of transitions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentActivity {
    pub seq: u64,
    pub state: AgentActivityState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentActivityState {
    Idle,
    Working,
    /// Reserved for integrations that can reliably identify a blocked run.
    /// The Pi bridge does not emit this state.
    Blocked,
}

#[derive(Debug, Clone)]
pub(crate) struct ActivityBridge(Arc<Bridge>);

#[derive(Debug)]
struct Bridge {
    directory: PathBuf,
    active: AtomicBool,
    latest: Mutex<Option<AgentActivity>>,
}

impl ActivityBridge {
    /// Installation errors are optional. The owner already exists before any
    /// file write, so even a partial installation is removed on failure.
    pub(crate) fn install() -> Option<Self> {
        Self::install_in(&std::env::temp_dir()).ok()
    }

    fn install_in(root: &Path) -> io::Result<Self> {
        let root = fs::canonicalize(root)?;
        let bridge = loop {
            let directory = root.join(format!(
                "shika-pi-activity-{}-{}-{}",
                std::process::id(),
                crate::session::new_id(&[]),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match fs::DirBuilder::new().mode(0o700).create(&directory) {
                Ok(()) => {
                    break Self(Arc::new(Bridge {
                        directory,
                        active: AtomicBool::new(true),
                        latest: Mutex::new(None),
                    }));
                }
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(err) => return Err(err),
            }
        };
        if bridge.extension_path().to_str().is_none() || bridge.metadata_path().to_str().is_none() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Pi extension path is not UTF-8",
            ));
        }
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(bridge.extension_path())?;
        std::io::Write::write_all(&mut file, EXTENSION.as_bytes())?;
        Ok(bridge)
    }

    fn extension_path(&self) -> PathBuf {
        self.0.directory.join("activity.ts")
    }

    pub(crate) fn metadata_path(&self) -> PathBuf {
        self.0.directory.join("latest.json")
    }

    pub(crate) fn wire(&self, request: &mut SpawnRequest) {
        request.args.push("--extension".into());
        request
            .args
            .push(self.extension_path().to_string_lossy().into_owned());
        request.env.push((
            "SHIKA_ACTIVITY_FILE".into(),
            self.metadata_path().to_string_lossy().into_owned(),
        ));
    }

    /// Bounded read, with no core operations lock. A missing, malformed,
    /// oversized or regressed report means fallback, not an invented state.
    pub(crate) fn read(&self) -> Option<AgentActivity> {
        if !self.0.active.load(Ordering::Acquire) {
            return None;
        }
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(self.metadata_path())
            .ok()?;
        if !file.metadata().ok()?.is_file() {
            return None;
        }
        let report = read_report(file)?;
        let mut latest = self.0.latest.lock().unwrap_or_else(|e| e.into_inner());
        if !self.0.active.load(Ordering::Acquire) {
            return None;
        }
        if let Some(previous) = *latest
            && (report.seq < previous.seq || (report.seq == previous.seq && report != previous))
        {
            return None;
        }
        *latest = Some(report);
        Some(report)
    }

    /// Explicit cleanup means clones held by a poll or a PTY sink cannot keep
    /// files alive after close, quit, or process exit. Idempotent with Drop.
    pub(crate) fn cleanup(&self) {
        if self.0.active.swap(false, Ordering::AcqRel) {
            let _ = fs::remove_dir_all(&self.0.directory);
        }
    }
}

impl Drop for Bridge {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

fn read_report(file: File) -> Option<AgentActivity> {
    let mut bytes = Vec::new();
    file.take(MAX_REPORT_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > MAX_REPORT_BYTES {
        return None;
    }
    let report: AgentActivity = serde_json::from_slice(&bytes).ok()?;
    // Pi uses JavaScript safe integers, starting at one.
    (report.seq > 0 && report.seq <= 9_007_199_254_740_991).then_some(report)
}

/// Own a share through process exit, including launches that fail before the
/// session is inserted. Does not inspect or change terminal output.
pub(crate) fn activity_sink(
    bridge: Option<ActivityBridge>,
    mut sink: impl PtySink,
) -> impl PtySink {
    move |pty: PtyId, event: PtyEvent| {
        if matches!(event, PtyEvent::Exit(_))
            && let Some(bridge) = &bridge
        {
            bridge.cleanup();
        }
        sink.send(pty, event);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pty::PtySize;
    use std::os::unix::fs::{PermissionsExt, symlink};

    #[test]
    fn launch_is_private_unique_and_child_only() {
        let first = ActivityBridge::install().unwrap();
        let second = ActivityBridge::install().unwrap();
        assert_ne!(first.metadata_path(), second.metadata_path());
        assert!(first.metadata_path().is_absolute());
        assert_eq!(
            fs::metadata(&first.0.directory)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        let mut request = SpawnRequest {
            program: "/bin/sh".into(),
            args: vec!["--approve".into()],
            cwd: "/".into(),
            path: "/bin".into(),
            size: PtySize::default(),
            env: vec![],
        };
        let parent = std::env::var_os("SHIKA_ACTIVITY_FILE");
        first.wire(&mut request);
        assert_eq!(
            request.args,
            [
                "--approve",
                "--extension",
                first.extension_path().to_str().unwrap()
            ]
        );
        assert_eq!(
            request.env,
            [(
                "SHIKA_ACTIVITY_FILE".into(),
                first.metadata_path().to_string_lossy().into_owned()
            )]
        );
        assert_eq!(std::env::var_os("SHIKA_ACTIVITY_FILE"), parent);
        assert_eq!(
            fs::read_to_string(first.extension_path()).unwrap(),
            EXTENSION
        );
        assert!(!first.metadata_path().exists());
    }

    #[test]
    fn reports_are_bounded_validated_and_ordered() {
        let bridge = ActivityBridge::install().unwrap();
        let path = bridge.metadata_path();
        assert_eq!(bridge.read(), None);
        for invalid in [
            "",
            "{}",
            "{\"seq\":0,\"state\":\"idle\"}",
            "{\"seq\":1,\"state\":\"unknown\"}",
            "{\"seq\":1,\"state\":\"idle\",\"prompt\":\"secret\"}",
            "{\"seq\":9007199254740992,\"state\":\"idle\"}",
        ] {
            fs::write(&path, invalid).unwrap();
            assert_eq!(bridge.read(), None);
        }
        fs::write(&path, vec![b' '; MAX_REPORT_BYTES as usize + 1]).unwrap();
        assert_eq!(bridge.read(), None);
        fs::write(&path, "{\"seq\":2,\"state\":\"working\"}").unwrap();
        let expected = Some(AgentActivity {
            seq: 2,
            state: AgentActivityState::Working,
        });
        assert_eq!(bridge.read(), expected);
        assert_eq!(bridge.read(), expected);
        for invalid in [
            "{\"seq\":1,\"state\":\"idle\"}",
            "{\"seq\":2,\"state\":\"idle\"}",
        ] {
            fs::write(&path, invalid).unwrap();
            assert_eq!(bridge.read(), None);
        }
        fs::write(&path, "{\"seq\":3,\"state\":\"idle\"}").unwrap();
        assert_eq!(
            bridge.read(),
            Some(AgentActivity {
                seq: 3,
                state: AgentActivityState::Idle
            })
        );
        fs::remove_file(&path).unwrap();
        symlink(bridge.extension_path(), &path).unwrap();
        assert_eq!(bridge.read(), None);
    }

    #[test]
    fn cleanup_is_shared_idempotent_and_covers_failed_install() {
        let bridge = ActivityBridge::install().unwrap();
        let directory = bridge.0.directory.clone();
        let clone = bridge.clone();
        drop(bridge);
        assert!(directory.exists());
        clone.cleanup();
        assert!(!directory.exists());
        assert_eq!(clone.read(), None);
        clone.cleanup();
        drop(clone);
        let bridge = ActivityBridge::install().unwrap();
        let directory = bridge.0.directory.clone();
        drop(bridge);
        assert!(!directory.exists());
        assert!(ActivityBridge::install_in(Path::new("/nonexistent/shika-test-root")).is_err());
    }

    #[test]
    fn core_pi_launch_reports_and_snapshot_clones_do_not_retain_files() {
        let fixture = ActivityBridge::install().unwrap();
        let repo = fixture.0.directory.join("repo");
        fs::create_dir(&repo).unwrap();
        for args in [
            vec!["init", "-b", "main"],
            vec![
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.test",
                "commit",
                "--allow-empty",
                "-m",
                "fixture",
            ],
        ] {
            let output = std::process::Command::new("/usr/bin/git")
                .args(args)
                .current_dir(&repo)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        let binary = fixture.0.directory.join("pi");
        fs::write(&binary, "#!/bin/sh\n[ \"$1\" = --approve ] && [ \"$2\" = --extension ] && [ -f \"$3\" ] || exit 12\nprintf '{\"seq\":1,\"state\":\"working\"}' > \"$SHIKA_ACTIVITY_FILE\"\nwhile read line; do :; done\n").unwrap();
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();
        let core = crate::Core::open_with(
            fixture.0.directory.join("data"),
            crate::PathEnv::from_lookup("/bin:/usr/bin".into(), None, &[("pi", Some(binary))]),
        )
        .unwrap();
        let project = core.add_project(&repo).unwrap().project;
        let session = core
            .create_session(&project.id, "pi", PtySize::default(), |_, _| {})
            .unwrap();
        let snapshot = session.clone();
        let bridge = core.sessions.activity(&session.id).unwrap();
        let directory = bridge.0.directory.clone();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while core.session_activity(&session.id).is_none() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(
            core.session_activity(&session.id),
            Some(AgentActivity {
                seq: 1,
                state: AgentActivityState::Working
            })
        );
        core.session_discard(&session.id).unwrap();
        assert!(!directory.exists());
        assert_eq!(core.session_activity(&snapshot.id), None);
        assert_eq!(bridge.read(), None);
    }

    #[test]
    fn polling_does_not_take_the_operations_lock() {
        let bridge = ActivityBridge::install().unwrap();
        let core = crate::Core::open(bridge.0.directory.join("data")).unwrap();
        fs::write(bridge.metadata_path(), "{\"seq\":1,\"state\":\"idle\"}").unwrap();
        core.sessions.remember_activity("pi", bridge.clone());
        let _operations = core.operations.lock().unwrap();
        assert_eq!(
            core.session_activity("pi"),
            Some(AgentActivity {
                seq: 1,
                state: AgentActivityState::Idle
            })
        );
        assert_eq!(core.session_activity("missing"), None);
    }

    #[test]
    fn extension_lifecycle_continuation_reload_and_unsupported_version() {
        // No Node dependency for production or Rust-only contributors. When
        // Node 24+ is present, execute the actual bundled TypeScript factory.
        let bridge = ActivityBridge::install().unwrap();
        let script = r#"
            import { readFileSync, existsSync, rmSync } from 'node:fs';
            import { stripTypeScriptTypes } from 'node:module';
            import assert from 'node:assert/strict';
            const source = readFileSync(process.argv[1], 'utf8');
            const path = process.argv[2];
            process.env.SHIKA_ACTIVITY_FILE = path;
            let generation = 0;
            async function load(version) {
                const code = source.replace('import * as host from "@earendil-works/pi-coding-agent";', `const host = { VERSION: '${version}' };`);
                const js = stripTypeScriptTypes(code);
                const module = await import('data:text/javascript;base64,' + Buffer.from(js + `\n// ${generation++}`).toString('base64'));
                const handlers = new Map();
                module.default({ on: (event, handler) => handlers.set(event, handler) });
                return handlers;
            }
            for (const version of ['0.80.3', '0.79.99', 'unknown']) {
                assert.equal((await load(version)).size, 0);
                assert.equal(existsSync(path), false);
            }
            let handlers = await load('1.0.0');
            assert.equal(existsSync(path), false); // factory starts no resources
            function report(seq, state) {
                const bytes = readFileSync(path);
                assert.ok(bytes.length <= 128);
                assert.deepEqual(JSON.parse(bytes), { seq, state });
                assert.equal(existsSync(path + '.next'), false);
            }
            handlers.get('session_start')(); report(1, 'idle');
            handlers.get('agent_start')(); report(2, 'working');
            assert.equal(handlers.has('agent_end'), false);
            report(2, 'working'); // continuation remains working
            handlers.get('agent_settled')(); report(3, 'idle');
            handlers = await load('0.80.4');
            handlers.get('session_start')(); report(4, 'idle'); // reload
            handlers.get('agent_start')(); report(5, 'working');
            handlers.get('session_shutdown')(); report(6, 'idle');
            handlers.get('session_shutdown')(); report(6, 'idle');
            rmSync(path);
            handlers.get('session_start')(); report(7, 'idle');
            rmSync(process.argv[3], { recursive: true });
            handlers.get('agent_start')(); // write failures never escape
            handlers.get('session_shutdown')();
        "#;
        let Ok(version) = std::process::Command::new("node")
            .current_dir("/")
            .arg("--version")
            .output()
        else {
            eprintln!("Skipping Pi factory execution: Node unavailable");
            return;
        };
        let major = String::from_utf8_lossy(&version.stdout)
            .trim()
            .trim_start_matches('v')
            .split('.')
            .next()
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(0);
        if major < 24 {
            eprintln!("Skipping Pi factory execution: Node 24+ required");
            return;
        }
        let output = std::process::Command::new("node")
            .current_dir("/")
            .args(["--input-type=module", "--no-warnings", "-e", script])
            .arg(bridge.extension_path())
            .arg(bridge.metadata_path())
            .arg(&bridge.0.directory)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn a_real_child_receives_the_shim_args_and_env() {
        let bridge = ActivityBridge::install().unwrap();
        let mut request = SpawnRequest {
            program: "/bin/sh".into(),
            // A mock Pi accepts its native args and the explicit extension.
            args: vec!["-c".into(), "test \"$1\" = --approve && test \"$2\" = --extension && test -f \"$3\" && printf '{\"seq\":1,\"state\":\"working\"}' > \"$SHIKA_ACTIVITY_FILE\"; read line".into(), "mock-pi".into(), "--approve".into()],
            cwd: std::env::temp_dir(), path: "/bin:/usr/bin".into(),
            size: PtySize::default(), env: vec![],
        };
        bridge.wire(&mut request);
        let hub = crate::pty::PtyHub::new();
        let (send, receive) = std::sync::mpsc::channel();
        let pty = hub
            .open(
                request,
                activity_sink(Some(bridge.clone()), move |_, event| {
                    let _ = send.send(event);
                }),
            )
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while bridge.read().is_none() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(
            bridge.read(),
            Some(AgentActivity {
                seq: 1,
                state: AgentActivityState::Working
            })
        );
        hub.write(pty, b"done\n").unwrap();
        loop {
            if let PtyEvent::Exit(exit) = receive
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap()
            {
                assert_eq!(exit.code, 0);
                break;
            }
        }
        assert!(!bridge.0.directory.exists());
        hub.close(pty);
    }

    #[test]
    fn exit_cleans_before_forwarding_even_with_other_owners() {
        let bridge = ActivityBridge::install().unwrap();
        let directory = bridge.0.directory.clone();
        let mut sink = activity_sink(Some(bridge.clone()), move |_, event| {
            if matches!(event, PtyEvent::Exit(_)) {
                assert!(!directory.exists());
            }
        });
        sink.send(PtyId(1), PtyEvent::Output(vec![1, 2, 3]));
        assert!(bridge.0.directory.exists());
        sink.send(
            PtyId(1),
            PtyEvent::Exit(crate::pty::PtyExit {
                code: 1,
                signal: None,
            }),
        );
        assert_eq!(bridge.read(), None);
    }
}
