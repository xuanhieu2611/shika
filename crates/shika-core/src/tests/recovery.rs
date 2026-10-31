use super::*;

#[test]
fn recovery_keeps_dirty_files_index_commits_base_and_skips_setup() {
    let scratch = Scratch::new();
    let repo = scratch.repo("recovery");
    let core = core_with_fake_cli(&scratch);
    let project = core.add_project(&repo).unwrap().project;
    let session = create_fake_session(&core, &project.id);
    fs::write(session.worktree.join("committed.txt"), "commit").unwrap();
    git(&session.worktree, &["add", "."]);
    git(&session.worktree, &["commit", "-m", "task"]);
    fs::write(session.worktree.join("staged.txt"), "staged").unwrap();
    git(&session.worktree, &["add", "staged.txt"]);
    fs::write(session.worktree.join("draft.txt"), "unfinished").unwrap();
    core.session_set_title(&session.id, "Keep my task name")
        .unwrap();
    core.checkpoint_sessions().unwrap();
    let snapshot = git_snapshot(&session.worktree);
    drop(core);
    // Invalid setup must not be loaded or executed by recovery.
    fs::create_dir_all(repo.join(".shika")).unwrap();
    fs::write(repo.join(".shika/worktrees.json"), "not configuration").unwrap();
    let core = core_with_fake_cli(&scratch);
    let (sink, rx) = channel_sink();
    let reopened = core
        .reopen_leftover(&session.worktree, "claude", false, PtySize::default(), sink)
        .unwrap();
    let output = collect_until(&rx, "cwd:", Duration::from_secs(3));
    assert!(output.contains(&format!("cwd:{}", session.worktree.display())));
    assert!(!output.contains("--resume"));
    assert_eq!(reopened.title, "Keep my task name");
    assert_eq!(reopened.branch, session.branch);
    assert_eq!(reopened.base_ref, session.base_ref);
    assert_eq!(git_snapshot(&session.worktree), snapshot);
    assert!(reopened.started_by.is_none());
    assert!(core.leftovers_list().unwrap().is_empty());
    assert_eq!(
        core.leftover_remove(&session.worktree),
        Err(Error::UnknownLeftover)
    );
    let (sink, _) = channel_sink();
    assert_eq!(
        core.reopen_leftover(&session.worktree, "claude", false, PtySize::default(), sink),
        Err(Error::UnknownLeftover)
    );
    // Recovery must never rename an existing branch from new input.
    assert_eq!(
        core.session_rename_from_prompt(&reopened.id, "New prompt")
            .unwrap()
            .branch,
        session.branch
    );
    core.session_discard(&reopened.id).unwrap();
    assert!(core.worktree_journal().unwrap().is_empty());
}

#[test]
fn recovery_checkpoints_exact_conversation_and_resumes_again_without_a_poller() {
    let scratch = Scratch::new();
    let mut core = core_with_fake_cli(&scratch);
    let home = CliHome::at(scratch.path.join("provider-home"));
    core.cli_home = Some(home.clone());
    let project = core.add_project(&scratch.repo("recovery")).unwrap().project;
    let session = create_fake_session(&core, &project.id);
    let dir = home.claude_project_dir(&session.worktree);
    fs::create_dir_all(&dir).unwrap();
    // Deliberately not a transcript: discovering ids must not parse contents.
    fs::write(
        dir.join("exact-conversation.jsonl"),
        "private content is not read",
    )
    .unwrap();
    core.checkpoint_sessions().unwrap();
    let entry = &core.worktree_journal().unwrap()[0];
    assert_eq!(
        entry.recovery.as_ref().unwrap().conversation.as_deref(),
        Some("exact-conversation")
    );
    drop(core);
    let mut core = core_with_fake_cli(&scratch);
    core.cli_home = Some(home.clone());
    let (sink, rx) = channel_sink();
    let reopened = core
        .reopen_leftover(&session.worktree, "claude", true, PtySize::default(), sink)
        .unwrap();
    assert!(
        collect_until(&rx, "cwd:", Duration::from_secs(3)).contains("--resume exact-conversation")
    );
    core.checkpoint_sessions().unwrap();
    drop(core);
    let mut core = core_with_fake_cli(&scratch);
    core.cli_home = Some(home);
    let preview = core.leftover_recovery(&reopened.worktree).unwrap();
    assert_eq!(
        preview.recovery.unwrap().conversation.as_deref(),
        Some("exact-conversation")
    );
    // Missing provider history must expose fresh fallback, never --resume latest.
    fs::remove_file(dir.join("exact-conversation.jsonl")).unwrap();
    assert!(
        core.leftover_recovery(&session.worktree)
            .unwrap()
            .recovery
            .unwrap()
            .conversation
            .is_none()
    );
    let before = core.worktree_journal().unwrap();
    let (sink, _) = channel_sink();
    assert!(matches!(
        core.reopen_leftover(&session.worktree, "claude", true, PtySize::default(), sink),
        Err(Error::RecoverWorktree(_))
    ));
    assert_eq!(core.worktree_journal().unwrap(), before);
    let (sink, _) = channel_sink();
    core.reopen_leftover(&session.worktree, "claude", false, PtySize::default(), sink)
        .unwrap();
}

#[test]
fn legacy_recovery_starts_fresh_and_failed_spawn_keeps_the_journal() {
    let scratch = Scratch::new();
    let core = core_with_fake_cli(&scratch);
    let project = core.add_project(&scratch.repo("recovery")).unwrap().project;
    let session = create_fake_session(&core, &project.id);
    let mut entry = core.worktree_journal().unwrap().remove(0);
    entry.recovery = None;
    core.journal.replace(&entry).unwrap();
    drop(core);
    let core = core_with_fake_cli(&scratch);
    assert!(
        core.leftover_recovery(&session.worktree)
            .unwrap()
            .recovery
            .is_none()
    );
    fs::remove_file(scratch.path.join("bin/claude")).unwrap();
    let (sink, _) = channel_sink();
    assert!(
        core.reopen_leftover(&session.worktree, "claude", false, PtySize::default(), sink)
            .is_err()
    );
    assert_eq!(core.worktree_journal().unwrap(), vec![entry]);
    assert!(core.sessions().is_empty());
    fake_cli(&scratch.path);
    let (sink, _) = channel_sink();
    core.reopen_leftover(&session.worktree, "claude", false, PtySize::default(), sink)
        .unwrap();
}

#[test]
fn recovery_refuses_missing_broken_redirected_and_symlink_worktrees() {
    let scratch = Scratch::new();
    let repo = scratch.repo("recovery");
    let core = core_with_fake_cli(&scratch);
    let project = core.add_project(&repo).unwrap().project;
    let session = create_fake_session(&core, &project.id);
    drop(core);
    let core = core_with_fake_cli(&scratch);
    let gitfile = fs::read(session.worktree.join(".git")).unwrap();
    let journal = core.worktree_journal().unwrap();
    fs::remove_file(session.worktree.join(".git")).unwrap();
    assert!(core.leftover_recovery(&session.worktree).is_err());
    fs::write(
        session.worktree.join(".git"),
        format!("gitdir: {}\n", repo.join(".git").display()),
    )
    .unwrap();
    assert!(core.leftover_recovery(&session.worktree).is_err());
    fs::write(session.worktree.join(".git"), gitfile).unwrap();
    let moved = session.worktree.with_extension("saved");
    fs::rename(&session.worktree, &moved).unwrap();
    assert!(core.leftover_recovery(&session.worktree).is_err());
    std::os::unix::fs::symlink(&moved, &session.worktree).unwrap();
    assert!(core.leftover_recovery(&session.worktree).is_err());
    assert_eq!(core.worktree_journal().unwrap(), journal);
    assert!(core.sessions().is_empty());
}

#[test]
fn concurrent_recovery_starts_only_once_and_releases_old_lead_ownership() {
    let scratch = Scratch::new();
    let core = core_with_fake_cli(&scratch);
    let project = core.add_project(&scratch.repo("recovery")).unwrap().project;
    let session = create_fake_session(&core, &project.id);
    let mut entry = core.worktree_journal().unwrap().remove(0);
    entry.started_by_lead = true;
    core.journal.replace(&entry).unwrap();
    drop(core);
    let core = Arc::new(core_with_fake_cli(&scratch));
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let threads: Vec<_> = (0..2)
        .map(|_| {
            let core = core.clone();
            let barrier = barrier.clone();
            let path = session.worktree.clone();
            std::thread::spawn(move || {
                let (sink, _) = channel_sink();
                barrier.wait();
                core.reopen_leftover(&path, "claude", false, PtySize::default(), sink)
            })
        })
        .collect();
    let results: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|r| matches!(r, Err(Error::UnknownLeftover)))
            .count(),
        1
    );
    let reopened = core.sessions().remove(0);
    assert!(reopened.started_by.is_none());
    assert!(!core.worktree_journal().unwrap()[0].started_by_lead);
}

#[test]
fn a_failed_agent_switch_retains_the_previous_recovery_hint() {
    let scratch = Scratch::new();
    let core = core_with_fake_cli(&scratch);
    let project = core.add_project(&scratch.repo("recovery")).unwrap().project;
    let session = create_fake_session(&core, &project.id);
    let mut entry = core.worktree_journal().unwrap().remove(0);
    entry.recovery.as_mut().unwrap().conversation = Some("keep-exact-id".into());
    core.journal.replace(&entry).unwrap();
    fs::remove_file(scratch.path.join("bin/claude")).unwrap();
    let (sink, _) = channel_sink();
    assert!(
        core.session_switch_agent(&session.id, "claude", None, PtySize::default(), sink, None)
            .is_err()
    );
    assert_eq!(core.worktree_journal().unwrap(), vec![entry]);
    assert_eq!(core.session(&session.id).unwrap().pty, session.pty);
}

#[test]
fn recovery_follows_verified_renames_but_never_adopts_branch_switches() {
    let scratch = Scratch::new();
    let repo = scratch.repo("recovery");
    let core = core_with_fake_cli(&scratch);
    let project = core.add_project(&repo).unwrap().project;
    let session = create_fake_session(&core, &project.id);
    drop(core);
    let core = core_with_fake_cli(&scratch);
    git(&session.worktree, &["checkout", "-b", "other"]);
    assert!(matches!(
        core.leftover_recovery(&session.worktree),
        Err(Error::TaskBranchChanged { .. })
    ));
    git(&session.worktree, &["checkout", &session.branch]);
    git(&session.worktree, &["branch", "-m", "renamed-task"]);
    let (sink, _) = channel_sink();
    let reopened = core
        .reopen_leftover(&session.worktree, "claude", false, PtySize::default(), sink)
        .unwrap();
    assert_eq!(reopened.branch, "renamed-task");
    assert_eq!(core.worktree_journal().unwrap()[0].branch, "renamed-task");
}

#[test]
fn relaunch_has_projects_no_sessions_and_removable_leftovers() {
    let scratch = Scratch::new();
    let repo = scratch.repo("demo");
    let core = core_with_fake_cli(&scratch);
    let project = core.add_project(&repo).unwrap().project;
    let session = create_fake_session(&core, &project.id);
    assert!(core.leftovers_list().unwrap().is_empty());
    assert_eq!(
        core.leftover_remove(&session.worktree),
        Err(Error::UnknownLeftover)
    );
    drop(core);
    assert!(session.worktree.exists());
    let core = core_with_fake_cli(&scratch);
    assert_eq!(core.projects().unwrap(), std::slice::from_ref(&project));
    assert!(core.sessions().is_empty());
    assert_eq!(core.leftovers_list().unwrap()[0].path, session.worktree);
    core.remove_project(&project.id).unwrap();
    core.leftover_remove(&session.worktree).unwrap();
    assert!(!session.worktree.exists());
    assert!(core.leftovers_list().unwrap().is_empty());
}

#[test]
fn a_missing_leftover_path_can_be_forgotten_without_real_app_data() {
    let scratch = Scratch::new();
    let repo = scratch.repo("demo");
    let core = core_with_fake_cli(&scratch);
    let project = core.add_project(&repo).unwrap().project;
    let session = create_fake_session(&core, &project.id);
    drop(core);
    fs::remove_dir_all(&session.worktree).unwrap();
    let core = core_with_fake_cli(&scratch);
    core.leftover_remove(&session.worktree).unwrap();
    assert!(core.worktree_journal().unwrap().is_empty());
    assert!(
        git(&repo, &["branch", "--list", &session.branch])
            .trim()
            .is_empty()
    );
}

#[test]
fn detached_leftover_is_refused_and_kept_for_inspection() {
    let scratch = Scratch::new();
    let repo = scratch.repo("demo");
    let core = core_with_fake_cli(&scratch);
    let project = core.add_project(&repo).unwrap().project;
    let session = create_fake_session(&core, &project.id);
    git(&session.worktree, &["checkout", "--detach"]);
    drop(core);
    let core = core_with_fake_cli(&scratch);
    assert!(matches!(
        core.leftover_remove(&session.worktree),
        Err(Error::GitStatus(_))
    ));
    assert!(session.worktree.exists());
    assert_eq!(core.leftovers_list().unwrap().len(), 1);
}
