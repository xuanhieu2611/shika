use super::*;
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::process::Command;
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

struct Fixture {
    root: PathBuf,
    repo: PathBuf,
    core: Arc<Core>,
    project: Project,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "shika-preparation-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let repo = root.join("repo with spaces");
        fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "-b", "main"]);
        git(&repo, &["config", "user.name", "Test"]);
        git(&repo, &["config", "user.email", "test@invalid.example"]);
        git(&repo, &["config", "core.hooksPath", "/dev/null"]);
        git(&repo, &["config", "commit.gpgsign", "false"]);
        fs::write(repo.join(".gitignore"), ".env.local\nlocal/\n*.generated\n").unwrap();
        fs::write(repo.join("tracked.txt"), "original\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-m", "initial"]);
        let cli = root.join("claude");
        fs::write(&cli, "#!/bin/sh\nprintf 'agent-started\\n'\ncat\n").unwrap();
        fs::set_permissions(&cli, fs::Permissions::from_mode(0o755)).unwrap();
        let env = PathEnv::from_lookup("/usr/bin:/bin".into(), None, &[("claude", Some(cli))]);
        let core = Arc::new(Core::open_with(root.join("data"), env).unwrap());
        let project = core.add_project(&repo).unwrap().project;
        Self {
            root,
            repo,
            core,
            project,
        }
    }

    fn config(&self, commands: &[&str], copies: &[&str], timeout: u64) -> PreparationConfig {
        let config = PreparationConfig {
            commands: commands.iter().map(|s| s.to_string()).collect(),
            copy_files: copies.iter().map(|s| s.to_string()).collect(),
            timeout_seconds: timeout,
        };
        fs::create_dir_all(self.repo.join(".shika")).unwrap();
        fs::write(
            self.repo.join(preparation::CONFIG_PATH),
            serde_json::to_vec_pretty(&config).unwrap(),
        )
        .unwrap();
        config
    }

    fn approve(&self, config: &PreparationConfig) {
        self.core
            .approve_preparation(&self.project.id, config)
            .unwrap();
    }

    fn launch(&self) -> Result<Session> {
        self.core
            .create_session(&self.project.id, "claude", PtySize::default(), |_, _| {})
    }

    fn running(
        &self,
        commands: &[&str],
        preserve: bool,
    ) -> (PreparationControl, thread::JoinHandle<Result<Session>>) {
        let config = self.config(commands, &[], 10);
        self.approve(&config);
        let control = PreparationControl::default();
        let job_control = control.clone();
        let core = self.core.clone();
        let project = self.project.id.clone();
        let (ready, seen) = mpsc::channel();
        let job = thread::spawn(move || {
            core.create_session_with_preparation(
                &project,
                "claude",
                PtySize::default(),
                |_, _| {},
                job_control,
                move |event| {
                    if let PreparationEvent::Output(bytes) = event
                        && bytes == b"setup-running\n"
                    {
                        let _ = ready.send(());
                    }
                },
            )
        });
        seen.recv_timeout(Duration::from_secs(5))
            .expect("setup never ran");
        if preserve {
            self.core.cancel_preparations();
        }
        (control, job)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.core.cancel_preparations();
        for session in self.core.sessions() {
            let _ = self.core.cancel_session_start(&session.id);
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

#[test]
fn adding_a_project_does_not_execute_setup_and_launch_requires_consent() {
    let f = Fixture::new();
    let config = f.config(&["touch \"$SHIKA_PROJECT_ROOT/ran.generated\""], &[], 10);
    assert!(!f.repo.join("ran.generated").exists());
    assert_eq!(f.launch(), Err(Error::PreparationNeedsApproval));
    assert!(!f.repo.join(".worktrees").exists());
    assert!(f.core.sessions().is_empty());
    assert!(!f.core.preparation_approved(&f.project.id, &config).unwrap());
}

#[test]
fn consent_persists_but_changed_configuration_requires_new_consent() {
    let f = Fixture::new();
    let first = f.config(&["true"], &[], 10);
    f.approve(&first);
    let reopened = Core::open(f.root.join("data")).unwrap();
    assert!(
        reopened
            .preparation_approved(&f.project.id, &first)
            .unwrap()
    );
    let second = f.config(&["echo changed"], &[], 10);
    assert!(!f.core.preparation_approved(&f.project.id, &second).unwrap());
    assert_eq!(
        f.core.approve_preparation(&f.project.id, &first),
        Err(Error::PreparationNeedsApproval)
    );
    assert_eq!(f.launch(), Err(Error::PreparationNeedsApproval));
    assert!(!f.repo.join(".worktrees").exists());
}

#[test]
fn local_files_are_independent_and_commands_finish_before_agent_launch() {
    let f = Fixture::new();
    fs::write(f.repo.join(".env.local"), "local-secret\n").unwrap();
    fs::set_permissions(f.repo.join(".env.local"), fs::Permissions::from_mode(0o600)).unwrap();
    fs::create_dir(f.repo.join("local")).unwrap();
    fs::write(f.repo.join("local/config"), "nested\n").unwrap();
    let config = f.config(&[
        "test \"$(cat .env.local)\" = local-secret && test \"$(cat local/config)\" = nested",
        "test \"$PWD\" = \"$SHIKA_WORKTREE_PATH\" && test \"$SHIKA_PROJECT_ROOT\" = \"$ROOT_WORKTREE_PATH\" && printf prepared > ready.generated",
    ], &[".env.local", "local/config"], 10);
    f.approve(&config);
    let session = f.launch().unwrap();
    assert_eq!(
        fs::read_to_string(session.worktree.join("ready.generated")).unwrap(),
        "prepared"
    );
    assert_eq!(
        fs::metadata(session.worktree.join(".env.local"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    fs::write(session.worktree.join(".env.local"), "task-local\n").unwrap();
    assert_eq!(
        fs::read_to_string(f.repo.join(".env.local")).unwrap(),
        "local-secret\n"
    );
    assert!(!f.core.session_dirty(&session.id).unwrap());
    f.core.session_close(&session.id, false).unwrap();
}

#[test]
fn failed_command_stops_sequence_never_launches_agent_and_cleans_untouched_tree() {
    let f = Fixture::new();
    let config = f.config(
        &[
            "printf failed > failed.generated; exit 7",
            "touch \"$SHIKA_PROJECT_ROOT/should-not-run.generated\"",
        ],
        &[],
        10,
    );
    f.approve(&config);
    let error = f.launch().unwrap_err();
    assert!(error.to_string().contains("exited with 7"), "{error}");
    assert!(f.core.sessions().is_empty());
    assert!(f.core.worktree_journal().unwrap().is_empty());
    assert!(!f.repo.join("should-not-run.generated").exists());
    assert_eq!(
        git(&f.repo, &["worktree", "list", "--porcelain"])
            .matches("worktree ")
            .count(),
        1
    );
}

#[test]
fn failure_preserves_tracked_edits_untracked_files_and_commits_in_leftovers() {
    for command in [
        "echo changed > tracked.txt; exit 1",
        "echo keep > new-file.txt; exit 1",
        "echo committed > tracked.txt; git add tracked.txt; git commit -m setup; exit 1",
    ] {
        let f = Fixture::new();
        let config = f.config(&[command], &[], 10);
        f.approve(&config);
        let error = f.launch().unwrap_err();
        assert!(error.to_string().contains("Worktree kept"), "{error}");
        assert!(f.core.sessions().is_empty());
        let leftovers = f.core.leftovers_list().unwrap();
        assert_eq!(leftovers.len(), 1);
        assert!(leftovers[0].path.exists());
        f.core.leftover_remove(&leftovers[0].path).unwrap();
        assert!(f.core.worktree_journal().unwrap().is_empty());
    }
}

#[test]
fn changed_configuration_during_setup_never_launches_an_agent() {
    let f = Fixture::new();
    let config = f.config(&["true"], &[], 10);
    f.approve(&config);
    let path = f.repo.join(preparation::CONFIG_PATH);
    let result = f.core.create_session_with_preparation(
        &f.project.id,
        "claude",
        PtySize::default(),
        |_, _| {},
        PreparationControl::default(),
        move |event| {
            if let PreparationEvent::Stage(stage) = event
                && stage.starts_with("Running setup")
            {
                fs::write(&path, "{\"setup-worktree\":[\"echo unapproved\"]}").unwrap();
            }
        },
    );
    assert_eq!(result, Err(Error::PreparationNeedsApproval));
    assert!(f.core.sessions().is_empty());
    assert!(f.core.worktree_journal().unwrap().is_empty());
}

#[test]
fn unsafe_or_missing_copy_sources_fail_before_an_agent_starts() {
    for path in [
        "../outside",
        "/etc/passwd",
        ".git/config",
        ".worktrees/task/file",
        "./.env.local",
        "tracked.txt",
        ".env.local",
        "local",
    ] {
        let f = Fixture::new();
        let config = f.config(&[], &[path], 10);
        let loaded = f.core.project_preparation(&f.project.id);
        if let Ok(Some(_)) = loaded {
            f.approve(&config);
            assert!(f.launch().is_err(), "accepted {path}");
        } else {
            assert!(loaded.is_err(), "accepted {path}");
        }
        assert!(f.core.sessions().is_empty());
        assert!(f.core.worktree_journal().unwrap().is_empty());
    }
}

#[test]
fn source_symlink_and_symlinked_parent_never_copy_outside_files() {
    for parent in [false, true] {
        let f = Fixture::new();
        let secret = f.root.join("outside");
        fs::create_dir(&secret).unwrap();
        fs::write(secret.join("secret"), "outside-private").unwrap();
        let relative = if parent {
            symlink(&secret, f.repo.join("local")).unwrap();
            "local/secret"
        } else {
            symlink(secret.join("secret"), f.repo.join(".env.local")).unwrap();
            ".env.local"
        };
        let config = f.config(&[], &[relative], 10);
        f.approve(&config);
        assert!(f.launch().is_err());
        assert!(f.core.sessions().is_empty());
        assert_eq!(
            fs::read_to_string(secret.join("secret")).unwrap(),
            "outside-private"
        );
    }
}

#[test]
fn symlinked_configuration_is_rejected() {
    let f = Fixture::new();
    fs::create_dir(f.repo.join(".shika")).unwrap();
    let config = f.root.join("untrusted.json");
    fs::write(&config, "{}").unwrap();
    symlink(&config, f.repo.join(preparation::CONFIG_PATH)).unwrap();
    assert!(f.core.project_preparation(&f.project.id).is_err());
    assert!(f.launch().is_err());
}

#[test]
fn oversized_and_nonregular_configuration_is_rejected_without_blocking() {
    let f = Fixture::new();
    f.config(&[], &[], 10);
    let path = f.repo.join(preparation::CONFIG_PATH);
    fs::write(&path, vec![b' '; 64 * 1024 + 1]).unwrap();
    let error = f.core.project_preparation(&f.project.id).unwrap_err();
    assert!(error.to_string().contains("exceeds 64 KiB"), "{error}");
    fs::remove_file(&path).unwrap();
    let name = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
    // SAFETY: the NUL-terminated name is inside our disposable repository.
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    let error = f.core.project_preparation(&f.project.id).unwrap_err();
    assert!(error.to_string().contains("regular file"), "{error}");
    assert!(f.core.worktree_journal().unwrap().is_empty());
}

#[test]
fn a_file_ignored_only_in_the_source_is_not_copied_into_the_task() {
    let f = Fixture::new();
    fs::write(f.repo.join(".gitignore"), "source-only\n").unwrap();
    fs::write(f.repo.join("source-only"), "private\n").unwrap();
    let config = f.config(&[], &["source-only"], 10);
    f.approve(&config);
    let error = f.launch().unwrap_err();
    assert!(
        error.to_string().contains("both the project and task"),
        "{error}"
    );
    assert!(f.core.sessions().is_empty());
    assert!(f.core.worktree_journal().unwrap().is_empty());
    assert_eq!(
        fs::read_to_string(f.repo.join("source-only")).unwrap(),
        "private\n"
    );
}

#[test]
fn cancellation_before_start_allocates_no_worktree() {
    let f = Fixture::new();
    let config = f.config(&["true"], &[], 10);
    f.approve(&config);
    let control = PreparationControl::default();
    control.cancel();
    assert_eq!(
        f.core.create_session_with_preparation(
            &f.project.id,
            "claude",
            PtySize::default(),
            |_, _| {},
            control,
            |_| {}
        ),
        Err(Error::PreparationCancelled)
    );
    assert!(f.core.sessions().is_empty());
    assert!(f.core.worktree_journal().unwrap().is_empty());
    assert!(!f.repo.join(".worktrees").exists());
}

#[test]
fn a_completed_launch_abandoned_by_its_ui_retains_the_journaled_worktree() {
    let f = Fixture::new();
    let config = f.config(&["printf prepared > ready.generated"], &[], 10);
    f.approve(&config);
    let session = f.launch().unwrap();
    f.core.cancel_session_start(&session.id).unwrap();
    assert!(f.core.sessions().is_empty());
    let leftovers = f.core.leftovers_list().unwrap();
    assert_eq!(leftovers.len(), 1);
    assert_eq!(leftovers[0].path, session.worktree);
    assert_eq!(
        fs::read_to_string(session.worktree.join("ready.generated")).unwrap(),
        "prepared"
    );
    f.core.leftover_remove(&session.worktree).unwrap();
}

#[test]
fn background_descendants_are_stopped_before_the_agent_starts() {
    let f = Fixture::new();
    let config = f.config(
        &["(sleep 1; touch \"$SHIKA_PROJECT_ROOT/late.generated\") & printf done"],
        &[],
        10,
    );
    f.approve(&config);
    let session = f.launch().unwrap();
    thread::sleep(Duration::from_millis(1200));
    assert!(
        !f.repo.join("late.generated").exists(),
        "background writer survived setup"
    );
    f.core.session_close(&session.id, false).unwrap();
}

#[test]
fn timeout_stops_setup_and_preserves_no_live_session() {
    let f = Fixture::new();
    let config = f.config(&["sleep 30"], &[], 1);
    f.approve(&config);
    let start = Instant::now();
    let error = f.launch().unwrap_err();
    assert!(error.to_string().contains("timed out"), "{error}");
    assert!(start.elapsed() < Duration::from_secs(5));
    assert!(f.core.sessions().is_empty());
    assert!(f.core.worktree_journal().unwrap().is_empty());
}

#[test]
fn cancellation_kills_setup_descendants_and_removes_clean_pending_worktree() {
    let f = Fixture::new();
    let (control, job) = f.running(&["(sleep 1; touch \"$SHIKA_PROJECT_ROOT/late.generated\") & printf 'setup-running\\n'; sleep 30"], false);
    assert!(
        f.core.leftovers_list().unwrap().is_empty(),
        "active setup appeared as disposable leftover"
    );
    let pending = f.core.worktree_journal().unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(
        f.core.leftover_remove(&pending[0].path),
        Err(Error::UnknownLeftover)
    );
    control.cancel();
    assert_eq!(job.join().unwrap(), Err(Error::PreparationCancelled));
    assert!(f.core.worktree_journal().unwrap().is_empty());
    thread::sleep(Duration::from_millis(1200));
    assert!(
        !f.repo.join("late.generated").exists(),
        "background writer survived cancel"
    );
}

#[test]
fn shutdown_preserves_pending_worktree_and_stops_its_setup() {
    let f = Fixture::new();
    let (_, job) = f.running(&["printf 'setup-running\\n'; sleep 30"], true);
    assert!(job.join().unwrap().is_err());
    let leftovers = f.core.leftovers_list().unwrap();
    assert_eq!(leftovers.len(), 1);
    assert!(leftovers[0].path.exists());
    assert!(f.core.sessions().is_empty());
}

#[test]
fn removing_project_cancels_setup_and_preserves_worktree() {
    let f = Fixture::new();
    let (_, job) = f.running(&["printf 'setup-running\\n'; sleep 30"], false);
    f.core.remove_project(&f.project.id).unwrap();
    assert!(job.join().unwrap().is_err());
    assert!(f.core.projects().unwrap().is_empty());
    assert_eq!(f.core.leftovers_list().unwrap().len(), 1);
}

#[test]
fn slow_setup_does_not_hold_operation_lock_or_block_another_session() {
    let f = Fixture::new();
    let (control, job) = f.running(&["printf 'setup-running\\n'; sleep 30"], false);
    let other = f.root.join("other");
    fs::create_dir(&other).unwrap();
    git(&other, &["init", "-b", "main"]);
    git(
        &other,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@invalid.example",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--allow-empty",
            "-m",
            "initial",
        ],
    );
    let project = f.core.add_project(&other).unwrap().project;
    let start = Instant::now();
    let session = f
        .core
        .create_session(&project.id, "claude", PtySize::default(), |_, _| {})
        .unwrap();
    f.core.session_close(&session.id, false).unwrap();
    assert!(start.elapsed() < Duration::from_secs(3));
    control.cancel();
    assert_eq!(job.join().unwrap(), Err(Error::PreparationCancelled));
}
