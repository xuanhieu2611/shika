use super::*;

#[test]
fn a_dev_base_starts_from_origin_dev_without_an_upstream() {
    let scratch = Scratch::new();
    let (repo, _remote) = repo_with_dev_on_origin(&scratch);
    let core = core_with_fake_cli(&scratch);
    let project = core.add_project(&repo).unwrap().project;
    assert_eq!(
        core.project_base(&project.id).unwrap(),
        ProjectBase {
            configured: None,
            name: Some("main".into()),
            default_name: Some("main".into()),
        }
    );
    let saved = core.set_project_base(&project.id, " origin/dev ").unwrap();
    assert_eq!(saved.base_branch.as_deref(), Some("dev"));
    assert_eq!(
        core.projects().unwrap()[0].base_branch.as_deref(),
        Some("dev")
    );
    assert_eq!(
        core.project_base(&project.id).unwrap(),
        ProjectBase {
            configured: Some("dev".into()),
            name: Some("dev".into()),
            default_name: Some("main".into()),
        }
    );

    let session = create_fake_session(&core, &project.id);
    assert_eq!(session.base_ref.as_deref(), Some("refs/remotes/origin/dev"));
    assert_eq!(
        rev(&session.worktree, "HEAD"),
        rev(&repo, "refs/remotes/origin/dev")
    );
    assert!(!has_upstream(&session.worktree));
    assert_eq!(
        core.worktree_journal().unwrap()[0].base_ref.as_deref(),
        Some("refs/remotes/origin/dev")
    );
    // Measured against main, dev's own commit would make this card look
    // pushed and full of changes.
    let state = core.session_git_state(&session.id, false).unwrap();
    assert!(!state.dirty && !state.has_own_commits && !state.unpushed && !state.pushed);
    assert_eq!(
        core.session_diff_stat(&session.id).unwrap(),
        DiffStat::default()
    );
    core.session_close(&session.id, false).unwrap();
    assert!(!session.worktree.exists());
    assert!(!branch_exists(&repo, &session.branch));
}

#[test]
fn a_commit_on_a_dev_card_is_its_own_and_unpushed() {
    let scratch = Scratch::new();
    let (repo, _remote) = repo_with_dev_on_origin(&scratch);
    let core = core_with_fake_cli(&scratch);
    let project = core.add_project(&repo).unwrap().project;
    core.set_project_base(&project.id, "dev").unwrap();
    let session = create_fake_session(&core, &project.id);
    fs::write(session.worktree.join("task.txt"), "one\ntwo\n").unwrap();
    git(&session.worktree, &["add", "task.txt"]);
    git(&session.worktree, &["commit", "-m", "task"]);

    assert_eq!(
        git(
            &session.worktree,
            &["rev-list", "--count", "refs/remotes/origin/dev..HEAD"]
        )
        .trim(),
        "1"
    );
    let state = core.session_git_state(&session.id, false).unwrap();
    assert!(state.has_own_commits && state.unpushed && !state.pushed);
    assert!(state.can_push());
    assert_eq!(
        core.session_diff_stat(&session.id).unwrap(),
        DiffStat {
            files: 1,
            insertions: 2,
            deletions: 0,
        }
    );
    assert_eq!(
        core.session_close(&session.id, false),
        Err(Error::CloseNeedsConfirmation)
    );
    // Push and close sends the task branch, never dev.
    let dev = rev(&repo, "refs/remotes/origin/dev");
    core.session_push_and_close(&session.id).unwrap();
    git(&repo, &["fetch", "origin"]);
    assert_eq!(rev(&repo, "refs/remotes/origin/dev"), dev);
    assert!(branch_exists(&repo, &session.branch));
}

#[test]
fn a_local_only_base_branch_works() {
    let scratch = Scratch::new();
    let repo = scratch.repo("demo");
    git(&repo, &["switch", "-c", "staging"]);
    git(&repo, &["commit", "--allow-empty", "-m", "staging work"]);
    git(&repo, &["switch", "main"]);
    let core = core_with_fake_cli(&scratch);
    let project = core.add_project(&repo).unwrap().project;
    core.set_project_base(&project.id, "staging").unwrap();

    let session = create_fake_session(&core, &project.id);
    assert_eq!(session.base_ref.as_deref(), Some("refs/heads/staging"));
    assert_eq!(rev(&session.worktree, "HEAD"), rev(&repo, "staging"));
    assert!(!has_upstream(&session.worktree));
    let state = core.session_git_state(&session.id, false).unwrap();
    assert!(!state.has_own_commits && !state.unpushed && !state.pushed);
    core.session_close(&session.id, false).unwrap();
    assert!(!branch_exists(&repo, &session.branch));
}

#[test]
fn a_launch_base_applies_to_that_task_only() {
    let scratch = Scratch::new();
    let repo = scratch.repo("demo");
    git(&repo, &["switch", "-c", "staging"]);
    git(&repo, &["commit", "--allow-empty", "-m", "staging work"]);
    git(&repo, &["switch", "main"]);
    let core = core_with_fake_cli(&scratch);
    let project = core.add_project(&repo).unwrap().project;

    let (sink, _rx) = channel_sink();
    let session = core
        .create_session(
            &project.id,
            "claude",
            PtySize::new(30, 90),
            sink,
            LaunchOptions {
                base: Some("staging".into()),
                ..LaunchOptions::default()
            },
        )
        .unwrap();
    assert_eq!(session.base_ref.as_deref(), Some("refs/heads/staging"));
    assert_eq!(rev(&session.worktree, "HEAD"), rev(&repo, "staging"));
    // The project's saved base is untouched, so the next New is plain.
    assert_eq!(core.projects().unwrap()[0].base_branch, None);
    let plain = create_fake_session(&core, &project.id);
    assert_eq!(rev(&plain.worktree, "HEAD"), rev(&repo, "main"));

    // A base that exists nowhere fails before anything is created.
    let journal = core.worktree_journal().unwrap();
    let (sink, _rx) = channel_sink();
    let result = core.create_session(
        &project.id,
        "claude",
        PtySize::new(30, 90),
        sink,
        LaunchOptions {
            base: Some("nope".into()),
            ..LaunchOptions::default()
        },
    );
    assert!(matches!(result, Err(Error::NoSuchBranch(_))));
    assert_eq!(core.worktree_journal().unwrap(), journal);
}

#[test]
fn a_missing_base_branch_is_refused_and_creates_nothing() {
    let scratch = Scratch::new();
    let repo = scratch.repo("demo");
    git(&repo, &["branch", "staging"]);
    let core = core_with_fake_cli(&scratch);
    let project = core.add_project(&repo).unwrap().project;
    for typed in ["nope", "a..b", "-x", "*"] {
        assert_eq!(
            core.set_project_base(&project.id, typed),
            Err(Error::NoSuchBranch(typed.into()))
        );
    }
    assert_eq!(core.projects().unwrap()[0].base_branch, None);

    // Set, then deleted: New refuses instead of starting from main.
    core.set_project_base(&project.id, "staging").unwrap();
    git(&repo, &["branch", "-D", "staging"]);
    let (sink, _rx) = channel_sink();
    assert_eq!(
        core.create_session(
            &project.id,
            "claude",
            PtySize::default(),
            sink,
            LaunchOptions::default()
        )
        .unwrap_err(),
        Error::BaseBranchMissing("staging".into())
    );
    assert!(core.sessions().is_empty());
    assert!(core.worktree_journal().unwrap().is_empty());
    assert!(!repo.join(".worktrees").exists());
    let base = core.project_base(&project.id).unwrap();
    assert_eq!(base.configured.as_deref(), Some("staging"));
    assert_eq!(base.name, None);

    // Empty clears it, back to the default branch.
    assert_eq!(
        core.set_project_base(&project.id, "  ")
            .unwrap()
            .base_branch,
        None
    );
    let session = create_fake_session(&core, &project.id);
    assert_eq!(session.base_ref.as_deref(), Some("refs/heads/main"));
    core.session_discard(&session.id).unwrap();
}

#[test]
fn an_unset_base_uses_origin_head_not_the_main_checkout() {
    let scratch = Scratch::new();
    let (repo, _remote) = repo_with_dev_on_origin(&scratch);
    git(&repo, &["switch", "-c", "elsewhere"]);
    git(&repo, &["commit", "--allow-empty", "-m", "local only"]);
    let core = core_with_fake_cli(&scratch);
    let project = core.add_project(&repo).unwrap().project;
    assert_eq!(
        core.project_base(&project.id).unwrap().name.as_deref(),
        Some("main")
    );

    let session = create_fake_session(&core, &project.id);
    assert_eq!(
        session.base_ref.as_deref(),
        Some("refs/remotes/origin/main")
    );
    assert_eq!(
        rev(&session.worktree, "HEAD"),
        rev(&repo, "refs/remotes/origin/main")
    );
    assert_ne!(rev(&session.worktree, "HEAD"), rev(&repo, "elsewhere"));
    assert!(!has_upstream(&session.worktree));
    let state = core.session_git_state(&session.id, false).unwrap();
    assert!(!state.has_own_commits && !state.pushed);
    core.session_close(&session.id, false).unwrap();
    assert!(!branch_exists(&repo, &session.branch));
}

#[test]
fn changing_the_base_leaves_a_running_card_alone() {
    let scratch = Scratch::new();
    let (repo, _remote) = repo_with_dev_on_origin(&scratch);
    let core = core_with_fake_cli(&scratch);
    let project = core.add_project(&repo).unwrap().project;
    core.set_project_base(&project.id, "dev").unwrap();
    let on_dev = create_fake_session(&core, &project.id);
    core.set_project_base(&project.id, "").unwrap();

    let on_main = create_fake_session(&core, &project.id);
    assert_eq!(
        on_main.base_ref.as_deref(),
        Some("refs/remotes/origin/main")
    );
    let kept = core.session(&on_dev.id).unwrap();
    assert_eq!(kept.base_ref.as_deref(), Some("refs/remotes/origin/dev"));
    // Still measured against dev, not the project's new base, main.
    let state = core.session_git_state(&on_dev.id, false).unwrap();
    assert!(!state.has_own_commits && !state.pushed);
    assert_eq!(
        core.session_diff_stat(&on_dev.id).unwrap(),
        DiffStat::default()
    );
    core.session_close(&on_dev.id, false).unwrap();
    assert!(!branch_exists(&repo, &on_dev.branch));
    core.session_discard(&on_main.id).unwrap();
}

#[test]
fn a_vanished_recorded_base_falls_back_to_the_default_branch() {
    let scratch = Scratch::new();
    let repo = scratch.repo("demo");
    git(&repo, &["branch", "staging"]);
    let core = core_with_fake_cli(&scratch);
    let project = core.add_project(&repo).unwrap().project;
    core.set_project_base(&project.id, "staging").unwrap();
    let session = create_fake_session(&core, &project.id);
    git(&repo, &["branch", "-D", "staging"]);
    git(
        &session.worktree,
        &["commit", "--allow-empty", "-m", "task"],
    );
    let state = core.session_git_state(&session.id, false).unwrap();
    assert!(state.has_own_commits && state.unpushed);
    core.session_discard(&session.id).unwrap();
}

#[test]
fn new_fetches_the_base_and_a_dead_remote_does_not_block_it() {
    let scratch = Scratch::new();
    let (repo, remote) = repo_with_dev_on_origin(&scratch);
    // Someone else pushes to dev. New should start from that commit.
    let other = scratch.path.join("other");
    let cloned = Command::new("git")
        .args(["clone", "--quiet", "--branch", "dev"])
        .arg(&remote)
        .arg(&other)
        .status()
        .unwrap();
    assert!(cloned.success());
    git(&other, &["commit", "--allow-empty", "-m", "newer dev"]);
    git(&other, &["push", "origin", "dev"]);
    let newest = rev(&other, "HEAD");
    assert_ne!(rev(&repo, "refs/remotes/origin/dev"), newest);

    let core = core_with_fake_cli(&scratch);
    let project = core.add_project(&repo).unwrap().project;
    core.set_project_base(&project.id, "dev").unwrap();
    core.prefetch_base(&project.id).unwrap();
    let fresh = create_fake_session(&core, &project.id);
    assert_eq!(rev(&fresh.worktree, "HEAD"), newest);
    core.session_discard(&fresh.id).unwrap();

    // An unreachable origin: New uses the ref it already has.
    git(
        &repo,
        &[
            "remote",
            "set-url",
            "origin",
            "/nonexistent/shika-remote.git",
        ],
    );
    let core = core_with_fake_cli(&scratch);
    let started = Instant::now();
    let offline = create_fake_session(&core, &project.id);
    assert!(started.elapsed() < worktree::FETCH_TIMEOUT);
    assert_eq!(rev(&offline.worktree, "HEAD"), newest);
    assert_eq!(offline.base_ref.as_deref(), Some("refs/remotes/origin/dev"));
    core.session_discard(&offline.id).unwrap();
}

#[test]
fn new_waits_for_the_picker_fetch_instead_of_starting_another() {
    let scratch = Scratch::new();
    let (repo, _remote) = repo_with_dev_on_origin(&scratch);
    // An origin that never answers, and counts how often it is asked.
    let log = scratch.path.join("upload-pack.log");
    let hang = scratch.path.join("hang.sh");
    fs::write(
        &hang,
        format!("#!/bin/sh\necho asked >> '{}'\nsleep 30\n", log.display()),
    )
    .unwrap();
    fs::set_permissions(&hang, fs::Permissions::from_mode(0o755)).unwrap();
    git(
        &repo,
        &["config", "remote.origin.uploadpack", hang.to_str().unwrap()],
    );
    let core = Arc::new(core_with_fake_cli(&scratch));
    let project = core.add_project(&repo).unwrap().project;

    let started = Instant::now();
    let picker = {
        let core = core.clone();
        let id = project.id.clone();
        std::thread::spawn(move || core.prefetch_base(&id).unwrap())
    };
    std::thread::sleep(Duration::from_millis(300));
    let session = create_fake_session(&core, &project.id);
    picker.join().unwrap();
    let took = started.elapsed();
    assert!(took >= worktree::FETCH_TIMEOUT, "{took:?}");
    assert!(
        took < worktree::FETCH_TIMEOUT + Duration::from_secs(2),
        "{took:?}"
    );
    assert_eq!(fs::read_to_string(&log).unwrap().lines().count(), 1);
    assert_eq!(
        session.base_ref.as_deref(),
        Some("refs/remotes/origin/main")
    );
    core.session_discard(&session.id).unwrap();
}
