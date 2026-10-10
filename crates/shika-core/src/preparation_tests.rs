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
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let root = loop {
            let root = std::env::temp_dir().join(format!(
                "shika-preparation-{}-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            ));
            // Reserve exclusively. Never adopt another fixture's root when
            // concurrent clocks return the same timestamp or a stale path exists.
            match fs::create_dir(&root) {
                Ok(()) => break root,
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => panic!("Cannot allocate preparation fixture: {e}"),
            }
        };
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
        self.core.create_session(
            &self.project.id,
            "claude",
            PtySize::default(),
            |_, _| {},
            LaunchOptions::default(),
        )
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
                LaunchOptions::default(),
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
fn preparation_onboarding_preview_leaves_repo_and_local_preferences_unchanged() {
    let f = Fixture::new();
    fs::write(f.repo.join(".env.local"), "fixture-only").unwrap();
    let before = fs::read(f.root.join("data/projects.json")).unwrap();
    assert!(
        f.core
            .preparation_onboarding_pending(&f.project.id)
            .unwrap()
    );
    let draft = f.core.project_preparation_draft(&f.project.id).unwrap();
    assert_eq!(draft.config.copy_files, [".env.local"]);
    assert!(draft.existing.is_none());
    // Leaving a preview without saving/skipping is a read-only cancellation.
    assert_eq!(fs::read(f.root.join("data/projects.json")).unwrap(), before);
    assert!(
        f.core
            .preparation_onboarding_pending(&f.project.id)
            .unwrap()
    );
    assert!(!f.repo.join(".shika").exists());
    assert!(!f.repo.join(".worktrees").exists());
    assert!(f.core.sessions().is_empty());
    assert!(f.core.worktree_journal().unwrap().is_empty());
}

#[test]
fn preparation_onboarding_skip_is_per_project_persistent_and_not_consent() {
    let f = Fixture::new();
    fs::write(f.repo.join(".env.local"), "fixture-only").unwrap();
    let other = f.root.join("other project");
    fs::create_dir(&other).unwrap();
    git(&other, &["init", "-b", "main"]);
    let other = f.core.add_project(&other).unwrap().project;
    f.core.skip_preparation_onboarding(&f.project.id).unwrap();
    assert!(
        !f.core
            .preparation_onboarding_pending(&f.project.id)
            .unwrap()
    );
    assert!(f.core.preparation_onboarding_pending(&other.id).unwrap());
    assert!(!f.repo.join(".shika").exists());
    assert!(!f.repo.join(".worktrees").exists());
    let reopened = Core::open_with(f.root.join("data"), f.core.path_env().clone()).unwrap();
    assert!(
        !reopened
            .preparation_onboarding_pending(&f.project.id)
            .unwrap()
    );
    assert!(reopened.preparation_onboarding_pending(&other.id).unwrap());
    // Settings still offers suggestions after skipping. A later saved config
    // needs actual consent regardless of the local onboarding preference.
    assert_eq!(
        reopened
            .project_preparation_draft(&f.project.id)
            .unwrap()
            .config
            .copy_files,
        [".env.local"]
    );
    let session = f.launch().unwrap();
    assert!(!session.worktree.join(".env.local").exists());
    assert!(!f.repo.join(".shika").exists());
    let journal_before = f.core.worktree_journal().unwrap();
    let config = f.config(&["touch ran.generated"], &[], 600);
    assert!(
        !reopened
            .preparation_approved(&f.project.id, &config)
            .unwrap()
    );
    assert_eq!(f.launch(), Err(Error::PreparationNeedsApproval));
    assert_eq!(f.core.worktree_journal().unwrap(), journal_before);
}

#[test]
fn preparation_onboarding_skip_refuses_a_configuration_added_since_preview() {
    let f = Fixture::new();
    let before = fs::read(f.root.join("data/projects.json")).unwrap();
    let config = f.config(&["true"], &[], 600);
    assert!(f.core.skip_preparation_onboarding(&f.project.id).is_err());
    assert_eq!(fs::read(f.root.join("data/projects.json")).unwrap(), before);
    assert_eq!(
        f.core.project_preparation(&f.project.id).unwrap(),
        Some(config)
    );
    assert!(
        f.core
            .preparation_onboarding_pending(&f.project.id)
            .unwrap()
    );
    assert!(f.core.worktree_journal().unwrap().is_empty());
}

#[test]
fn preparation_onboarding_save_and_approve_starts_only_at_normal_launch() {
    let f = Fixture::new();
    fs::write(f.repo.join(".env.local"), "fixture-only").unwrap();
    let mut draft = f.core.project_preparation_draft(&f.project.id).unwrap();
    draft.config.commands = vec!["test -f .env.local && printf prepared > ready.generated".into()];
    f.core
        .save_and_approve_project_preparation(&f.project.id, None, &draft.config)
        .unwrap();
    assert!(
        !f.core
            .preparation_onboarding_pending(&f.project.id)
            .unwrap()
    );
    assert!(!f.repo.join(".worktrees").exists());
    assert!(!f.repo.join("ready.generated").exists());
    assert!(f.core.sessions().is_empty());
    assert!(f.core.worktree_journal().unwrap().is_empty());
    let reopened = Core::open_with(f.root.join("data"), f.core.path_env().clone()).unwrap();
    assert!(
        reopened
            .preparation_approved(&f.project.id, &draft.config)
            .unwrap()
    );
    assert!(
        !reopened
            .preparation_onboarding_pending(&f.project.id)
            .unwrap()
    );
    let first = f.launch().unwrap();
    assert_eq!(
        fs::read_to_string(first.worktree.join("ready.generated")).unwrap(),
        "prepared"
    );
    fs::write(f.repo.join(".env.local"), "updated-fixture").unwrap();
    let second = f.launch().unwrap();
    assert_ne!(first.worktree, second.worktree);
    assert_eq!(
        fs::read_to_string(second.worktree.join(".env.local")).unwrap(),
        "updated-fixture"
    );
    assert_eq!(
        fs::read_to_string(first.worktree.join(".env.local")).unwrap(),
        "fixture-only"
    );
    assert!(second.worktree.join("ready.generated").exists());
    // No implicit re-detection or weakened consent for subsequent agents.
    f.config(&["touch changed.generated"], &[], 600);
    assert_eq!(f.launch(), Err(Error::PreparationNeedsApproval));
    f.core
        .save_project_preparation(
            &f.project.id,
            f.core.project_preparation(&f.project.id).unwrap().as_ref(),
            None,
        )
        .unwrap();
    assert!(
        !f.core
            .preparation_onboarding_pending(&f.project.id)
            .unwrap()
    );
}

#[test]
fn preparation_onboarding_save_and_approve_rejects_invalid_or_stale_drafts() {
    let f = Fixture::new();
    let before = fs::read(f.root.join("data/projects.json")).unwrap();
    let mut draft = f.core.project_preparation_draft(&f.project.id).unwrap();
    draft.config.timeout_seconds = 0;
    assert!(
        f.core
            .save_and_approve_project_preparation(&f.project.id, None, &draft.config)
            .is_err()
    );
    assert!(!f.repo.join(".shika").exists());
    draft.config.timeout_seconds = 600;
    let external = f.config(&["true"], &[], 600);
    assert!(
        f.core
            .save_and_approve_project_preparation(&f.project.id, None, &draft.config)
            .is_err()
    );
    assert_eq!(
        f.core.project_preparation(&f.project.id).unwrap(),
        Some(external)
    );
    assert_eq!(fs::read(f.root.join("data/projects.json")).unwrap(), before);
    assert!(f.core.worktree_journal().unwrap().is_empty());
    assert!(f.core.sessions().is_empty());
    assert!(
        f.core
            .preparation_onboarding_pending(&f.project.id)
            .unwrap()
    );
}

#[test]
fn preparation_onboarding_approval_write_failure_never_runs_saved_commands() {
    let f = Fixture::new();
    let mut draft = f.core.project_preparation_draft(&f.project.id).unwrap();
    draft.config.commands = vec!["touch ran.generated".into()];
    // Repository and local consent are separate files. Simulate a failed local
    // atomic write after the repository configuration was successfully saved.
    fs::create_dir(f.root.join("data/projects.json.tmp")).unwrap();
    let error = f
        .core
        .save_and_approve_project_preparation(&f.project.id, None, &draft.config)
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("Setup was saved, but local approval")
    );
    assert_eq!(
        f.core.project_preparation(&f.project.id).unwrap(),
        Some(draft.config.clone())
    );
    assert!(
        !f.core
            .preparation_approved(&f.project.id, &draft.config)
            .unwrap()
    );
    assert_eq!(f.launch(), Err(Error::PreparationNeedsApproval));
    assert!(f.core.sessions().is_empty());
    assert!(f.core.worktree_journal().unwrap().is_empty());
    assert!(!f.repo.join("ran.generated").exists());
    assert!(!f.repo.join(".worktrees").exists());
}

#[test]
fn setup_suggestions_are_unsaved_and_never_used_by_new() {
    let f = Fixture::new();
    fs::write(f.repo.join(".env.local"), "fixture-only").unwrap();
    fs::write(
        f.repo.join("package.json"),
        r#"{"packageManager":"npm@10.8.0","scripts":{"preinstall":"touch ran.generated"}}"#,
    )
    .unwrap();
    fs::write(f.repo.join("package-lock.json"), "{}").unwrap();
    let draft = f.core.project_preparation_draft(&f.project.id).unwrap();
    assert!(draft.existing.is_none());
    assert_eq!(draft.config.copy_files, [".env.local"]);
    assert_eq!(draft.config.commands, ["npm ci"]);
    assert_eq!(draft.config.timeout_seconds, 600);
    assert!(draft.note.contains("not saved"));
    assert!(!f.repo.join(".shika").exists());
    assert!(!f.repo.join("ran.generated").exists());
    assert!(
        !f.core
            .preparation_approved(&f.project.id, &draft.config)
            .unwrap()
    );
    // Detection is a preview, not implicit core setup. Author-facing New offers
    // onboarding; programmatic launches never infer or save preparation.
    let session = f.launch().unwrap();
    assert!(!session.worktree.join(".env.local").exists());
    assert!(!f.repo.join(".shika").exists());
    f.core
        .save_project_preparation(&f.project.id, None, Some(&draft.config))
        .unwrap();
    assert_eq!(f.launch(), Err(Error::PreparationNeedsApproval));
}

#[test]
fn setup_suggestions_preserve_existing_configuration_even_when_empty() {
    let f = Fixture::new();
    fs::write(f.repo.join(".env.local"), "fixture-only").unwrap();
    fs::write(
        f.repo.join("package.json"),
        r#"{"packageManager":"pnpm@10.0.0"}"#,
    )
    .unwrap();
    let saved = f.config(&[], &[], 42);
    let draft = f.core.project_preparation_draft(&f.project.id).unwrap();
    assert_eq!(draft.existing, Some(saved.clone()));
    assert_eq!(draft.config, saved);
    // Existing config also avoids reading unsafe package metadata entirely.
    fs::remove_file(f.repo.join("package.json")).unwrap();
    symlink("missing", f.repo.join("package.json")).unwrap();
    assert!(
        f.core
            .project_preparation_draft(&f.project.id)
            .unwrap()
            .existing
            .is_some()
    );
}

#[test]
fn setup_suggestions_only_include_root_regular_ignored_dotenv_files() {
    let f = Fixture::new();
    fs::write(f.repo.join(".env"), "not ignored").unwrap();
    fs::write(f.repo.join(".env.local"), "ignored").unwrap();
    fs::write(f.repo.join(".env.production"), "not suggested").unwrap();
    let draft = f.core.project_preparation_draft(&f.project.id).unwrap();
    assert_eq!(draft.config.copy_files, [".env.local"]);
    fs::write(
        f.repo.join(".gitignore"),
        ".env\n.env.local\n.env.production\n*.generated\n",
    )
    .unwrap();
    assert_eq!(
        f.core
            .project_preparation_draft(&f.project.id)
            .unwrap()
            .config
            .copy_files,
        [".env", ".env.local"]
    );
    git(&f.repo, &["add", "-f", ".env.local"]);
    assert_eq!(
        f.core
            .project_preparation_draft(&f.project.id)
            .unwrap()
            .config
            .copy_files,
        [".env"]
    );
    fs::remove_file(f.repo.join(".env")).unwrap();
    symlink("tracked.txt", f.repo.join(".env")).unwrap();
    assert!(
        f.core
            .project_preparation_draft(&f.project.id)
            .unwrap()
            .config
            .copy_files
            .is_empty()
    );
    fs::remove_file(f.repo.join(".env")).unwrap();
    fs::create_dir(f.repo.join(".env")).unwrap();
    assert!(
        f.core
            .project_preparation_draft(&f.project.id)
            .unwrap()
            .config
            .copy_files
            .is_empty()
    );
    assert!(!f.repo.join(".shika").exists());
}

#[test]
fn setup_suggestions_use_unambiguous_npm_and_pnpm_metadata() {
    let f = Fixture::new();
    for (package, locks, command) in [
        ("{}", vec!["package-lock.json"], "npm ci"),
        ("{}", vec!["npm-shrinkwrap.json"], "npm ci"),
        (
            r#"{"packageManager":"npm@10.8.0"}"#,
            vec!["npm-shrinkwrap.json", "package-lock.json"],
            "npm ci",
        ),
        (
            "{}",
            vec!["pnpm-lock.yaml"],
            "pnpm install --frozen-lockfile",
        ),
        (
            r#"{"packageManager":"pnpm@10.0.0+sha512.example"}"#,
            vec!["pnpm-lock.yaml"],
            "pnpm install --frozen-lockfile",
        ),
        (r#"{"packageManager":"npm@10.8.0"}"#, vec![], "npm install"),
        (
            r#"{"packageManager":"pnpm@10.0.0"}"#,
            vec![],
            "pnpm install",
        ),
    ] {
        fs::write(f.repo.join("package.json"), package).unwrap();
        for lock in &locks {
            fs::write(f.repo.join(lock), "fixture lock").unwrap();
        }
        let draft = f.core.project_preparation_draft(&f.project.id).unwrap();
        assert_eq!(draft.config.commands, [command], "{package}: {locks:?}");
        if locks.is_empty() {
            assert!(draft.note.contains("may create one"));
        }
        for lock in locks {
            fs::remove_file(f.repo.join(lock)).unwrap();
        }
    }
    assert!(!f.repo.join(".shika").exists());
}

#[test]
fn setup_suggestions_skip_conflicts_unsupported_managers_and_unknown_toolchains() {
    let f = Fixture::new();
    fs::write(f.repo.join("Cargo.toml"), "[package]\nname = 'fixture'\n").unwrap();
    assert!(
        f.core
            .project_preparation_draft(&f.project.id)
            .unwrap()
            .config
            .commands
            .is_empty()
    );
    for (package, locks) in [
        ("{}", vec![]),
        ("{}", vec!["package-lock.json", "pnpm-lock.yaml"]),
        ("{}", vec!["package-lock.json", "yarn.lock"]),
        (
            r#"{"packageManager":"pnpm@10.0.0"}"#,
            vec!["package-lock.json"],
        ),
        (r#"{"packageManager":"npm@10.8.0"}"#, vec!["pnpm-lock.yaml"]),
        (
            r#"{"packageManager":"yarn@4.0.0"}"#,
            vec!["package-lock.json"],
        ),
        (r#"{"packageManager":"npm; touch ran.generated"}"#, vec![]),
        (r#"{"packageManager":42}"#, vec!["package-lock.json"]),
        ("{}", vec!["bun.lock"]),
        ("{}", vec!["bun.lockb"]),
    ] {
        fs::write(f.repo.join("package.json"), package).unwrap();
        for lock in &locks {
            fs::write(f.repo.join(lock), "fixture").unwrap();
        }
        let draft = f.core.project_preparation_draft(&f.project.id).unwrap();
        assert!(draft.config.commands.is_empty(), "{package}: {locks:?}");
        assert!(draft.note.contains("manually"));
        for lock in locks {
            fs::remove_file(f.repo.join(lock)).unwrap();
        }
    }
    assert!(!f.repo.join("ran.generated").exists());
}

#[test]
fn setup_suggestions_skip_invalid_unbounded_and_symlinked_package_metadata() {
    let f = Fixture::new();
    fs::write(f.repo.join("package-lock.json"), "{}").unwrap();
    for package in ["{", "[]", &" ".repeat(65537)] {
        fs::write(f.repo.join("package.json"), package).unwrap();
        let draft = f.core.project_preparation_draft(&f.project.id).unwrap();
        assert!(draft.config.commands.is_empty());
        assert!(draft.note.contains("safely"));
    }
    fs::remove_file(f.repo.join("package.json")).unwrap();
    let outside = f.root.join("outside.json");
    fs::write(&outside, r#"{"packageManager":"npm@10.8.0"}"#).unwrap();
    symlink(&outside, f.repo.join("package.json")).unwrap();
    assert!(
        f.core
            .project_preparation_draft(&f.project.id)
            .unwrap()
            .config
            .commands
            .is_empty()
    );
    fs::remove_file(f.repo.join("package.json")).unwrap();
    fs::create_dir(f.repo.join("package.json")).unwrap();
    assert!(
        f.core
            .project_preparation_draft(&f.project.id)
            .unwrap()
            .config
            .commands
            .is_empty()
    );
    fs::remove_dir(f.repo.join("package.json")).unwrap();
    fs::write(
        f.repo.join("package.json"),
        r#"{"packageManager":"npm@10.8.0"}"#,
    )
    .unwrap();
    fs::remove_file(f.repo.join("package-lock.json")).unwrap();
    symlink(&outside, f.repo.join("package-lock.json")).unwrap();
    // Unsafe lock metadata must not degrade into an unfrozen installer.
    assert!(
        f.core
            .project_preparation_draft(&f.project.id)
            .unwrap()
            .config
            .commands
            .is_empty()
    );
    assert!(!f.repo.join(".shika").exists());
}

#[test]
fn setup_editor_creates_configuration_without_running_or_approving_it() {
    let f = Fixture::new();
    assert!(!f.repo.join(".shika").exists());
    assert_eq!(f.core.project_preparation(&f.project.id).unwrap(), None);
    // Merely opening the editor must not write anything.
    assert!(!f.repo.join(".shika").exists());
    fs::write(f.repo.join(".env.local"), "fixture-only").unwrap();
    let config = PreparationConfig {
        copy_files: vec![".env.local".into()],
        commands: vec!["touch ran.generated".into()],
        timeout_seconds: 600,
    };
    f.core
        .save_project_preparation(&f.project.id, None, Some(&config))
        .unwrap();
    assert_eq!(
        f.core.project_preparation(&f.project.id).unwrap(),
        Some(config.clone())
    );
    assert!(!f.repo.join("ran.generated").exists());
    assert!(!f.repo.join(".worktrees").exists());
    assert!(!f.core.preparation_approved(&f.project.id, &config).unwrap());
    assert!(
        !f.core
            .preparation_onboarding_pending(&f.project.id)
            .unwrap()
    );
    assert_eq!(f.launch(), Err(Error::PreparationNeedsApproval));
    f.approve(&config);
    let session = f.launch().unwrap();
    assert_eq!(
        fs::read_to_string(session.worktree.join(".env.local")).unwrap(),
        "fixture-only"
    );
    assert!(session.worktree.join("ran.generated").exists());
    // Editing is not renewed consent and must not modify a running task.
    let changed = PreparationConfig {
        commands: vec!["true".into()],
        ..config.clone()
    };
    f.core
        .save_project_preparation(&f.project.id, Some(&config), Some(&changed))
        .unwrap();
    assert!(
        !f.core
            .preparation_approved(&f.project.id, &changed)
            .unwrap()
    );
    assert_eq!(f.launch(), Err(Error::PreparationNeedsApproval));
    fs::write(f.repo.join(".shika/notes.txt"), "unrelated").unwrap();
    f.core
        .save_project_preparation(&f.project.id, Some(&changed), None)
        .unwrap();
    assert!(!f.repo.join(preparation::CONFIG_PATH).exists());
    assert_eq!(
        fs::read_to_string(f.repo.join(".shika/notes.txt")).unwrap(),
        "unrelated"
    );
    assert!(session.worktree.join(".env.local").exists());
    f.launch().unwrap(); // Disabling restores the no-config launch path.
}

#[test]
fn setup_editor_empty_template_and_formatting_edits_do_not_infer_setup_or_consent() {
    let f = Fixture::new();
    let empty = PreparationConfig {
        commands: vec![],
        copy_files: vec![],
        timeout_seconds: 600,
    };
    f.core
        .save_project_preparation(&f.project.id, None, Some(&empty))
        .unwrap();
    assert_eq!(
        f.core.project_preparation(&f.project.id).unwrap(),
        Some(empty.clone())
    );
    assert!(!f.core.preparation_approved(&f.project.id, &empty).unwrap());
    f.approve(&empty);
    // Parsed equality, not JSON formatting, is the existing consent fence.
    fs::write(f.repo.join(preparation::CONFIG_PATH), "{}").unwrap();
    f.core
        .save_project_preparation(&f.project.id, Some(&empty), Some(&empty))
        .unwrap();
    assert!(f.core.preparation_approved(&f.project.id, &empty).unwrap());
    assert!(f.core.worktree_journal().unwrap().is_empty());
    assert_eq!(fs::read_dir(f.repo.join(".shika")).unwrap().count(), 1);
}

#[test]
fn setup_editor_refuses_stale_invalid_and_unsafe_saves() {
    let f = Fixture::new();
    let original = f.config(&["true"], &[], 600);
    let changed = PreparationConfig {
        commands: vec!["echo changed".into()],
        ..original.clone()
    };
    assert!(
        f.core
            .save_project_preparation(&f.project.id, None, Some(&changed))
            .is_err()
    );
    assert!(
        f.core
            .save_project_preparation(&f.project.id, None, None)
            .is_err()
    );
    for invalid in [
        PreparationConfig {
            timeout_seconds: 0,
            ..original.clone()
        },
        PreparationConfig {
            timeout_seconds: 3601,
            ..original.clone()
        },
        PreparationConfig {
            commands: vec![String::new()],
            ..original.clone()
        },
        PreparationConfig {
            commands: vec!["a".repeat(65536)],
            ..original.clone()
        },
        PreparationConfig {
            copy_files: vec!["../outside".into()],
            ..original.clone()
        },
        PreparationConfig {
            copy_files: vec!["tracked.txt".into()],
            ..original.clone()
        },
        PreparationConfig {
            copy_files: vec!["missing.env".into()],
            ..original.clone()
        },
    ] {
        assert!(
            f.core
                .save_project_preparation(&f.project.id, Some(&original), Some(&invalid))
                .is_err()
        );
        assert_eq!(
            f.core.project_preparation(&f.project.id).unwrap(),
            Some(original.clone())
        );
    }
    fs::write(f.repo.join(".env.local"), "fixture").unwrap();
    let duplicates = PreparationConfig {
        copy_files: vec![".env.local".into(), ".env.local".into()],
        ..original.clone()
    };
    assert!(
        f.core
            .save_project_preparation(&f.project.id, Some(&original), Some(&duplicates))
            .is_err()
    );
    let external = f.config(&["external edit"], &[], 600);
    assert!(
        f.core
            .save_project_preparation(&f.project.id, Some(&original), Some(&changed))
            .is_err()
    );
    assert!(
        f.core
            .save_project_preparation(&f.project.id, Some(&original), None)
            .is_err()
    );
    assert_eq!(
        f.core.project_preparation(&f.project.id).unwrap(),
        Some(external)
    );
    // An invalid external config must not be silently replaced either.
    fs::write(f.repo.join(preparation::CONFIG_PATH), "not json").unwrap();
    assert!(
        f.core
            .save_project_preparation(&f.project.id, None, Some(&changed))
            .is_err()
    );
    assert_eq!(
        fs::read_to_string(f.repo.join(preparation::CONFIG_PATH)).unwrap(),
        "not json"
    );
    assert_eq!(fs::read_dir(f.repo.join(".shika")).unwrap().count(), 1);
}

#[test]
fn setup_editor_never_follows_config_or_copy_source_links() {
    let f = Fixture::new();
    let config = PreparationConfig {
        copy_files: vec![],
        commands: vec![],
        timeout_seconds: 600,
    };
    let outside = f.root.join("outside");
    fs::create_dir(&outside).unwrap();
    symlink(&outside, f.repo.join(".shika")).unwrap();
    assert!(
        f.core
            .save_project_preparation(&f.project.id, None, Some(&config))
            .is_err()
    );
    assert!(fs::read_dir(&outside).unwrap().next().is_none());
    fs::remove_file(f.repo.join(".shika")).unwrap();
    fs::create_dir(f.repo.join(".shika")).unwrap();
    let target = outside.join("config");
    fs::write(&target, "{}").unwrap();
    symlink(&target, f.repo.join(preparation::CONFIG_PATH)).unwrap();
    assert!(
        f.core
            .save_project_preparation(&f.project.id, None, Some(&config))
            .is_err()
    );
    assert_eq!(fs::read_to_string(&target).unwrap(), "{}");
    fs::remove_file(f.repo.join(preparation::CONFIG_PATH)).unwrap();
    symlink(&target, f.repo.join(".env.local")).unwrap();
    let config = PreparationConfig {
        copy_files: vec![".env.local".into()],
        ..config
    };
    assert!(
        f.core
            .save_project_preparation(&f.project.id, None, Some(&config))
            .is_err()
    );
    assert!(!f.repo.join(preparation::CONFIG_PATH).exists());
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
        LaunchOptions::default(),
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
            LaunchOptions::default(),
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
        .create_session(
            &project.id,
            "claude",
            PtySize::default(),
            |_, _| {},
            LaunchOptions::default(),
        )
        .unwrap();
    f.core.session_close(&session.id, false).unwrap();
    assert!(start.elapsed() < Duration::from_secs(3));
    control.cancel();
    assert_eq!(job.join().unwrap(), Err(Error::PreparationCancelled));
}
