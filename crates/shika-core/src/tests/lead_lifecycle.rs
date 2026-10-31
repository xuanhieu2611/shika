use super::*;

#[test]
fn a_lead_folder_that_is_not_a_worktree_fails_the_start_and_is_never_deleted() {
    let scratch = Scratch::new();
    let repo = scratch.repo("demo");
    let core = core_with_reporting_cli(&scratch);
    let project = core.add_project(&repo).unwrap().project;
    let path = repo.join(".worktrees").join("shika-lead");
    fs::create_dir_all(&path).unwrap();
    fs::write(path.join("mine.txt"), "keep\n").unwrap();

    let (sink, _rx) = channel_sink();
    let result = core.start_lead(
        &project.id,
        "claude",
        PtySize::default(),
        sink,
        lead_launch(&scratch),
    );
    assert_eq!(
        result.map(|_| ()),
        Err(Error::LeadWorktreeInvalid(path.clone()))
    );
    assert!(path.join("mine.txt").exists());
    assert!(core.worktree_journal().unwrap().is_empty());
    assert_eq!(core.saved_lead(&project.id).unwrap(), None);
    assert!(core.sessions().is_empty());
    assert_eq!(
        Error::LeadWorktreeInvalid(path.clone()).to_string(),
        format!(
            "{} exists but is not a worktree of this repository. Move or delete it, then start the Lead again.",
            path.display()
        )
    );
}

#[test]
fn a_failed_launch_removes_a_tree_it_created_and_keeps_an_older_one() {
    let scratch = Scratch::new();
    let repo = scratch.repo("demo");
    let core = core_with_reporting_cli(&scratch);
    let project = core.add_project(&repo).unwrap().project;
    let program = scratch.path.join("bin/claude");
    let script = fs::read_to_string(&program).unwrap();
    fs::remove_file(&program).unwrap();
    let try_start = || {
        let (sink, _rx) = channel_sink();
        core.start_lead(
            &project.id,
            "claude",
            PtySize::default(),
            sink,
            lead_launch(&scratch),
        )
        .map(|_| ())
    };

    // The tree this start made is untouched, so it goes.
    assert!(matches!(try_start(), Err(Error::OpenPty(_))));
    assert!(!repo.join(".worktrees/shika-lead").exists());
    assert!(core.worktree_journal().unwrap().is_empty());
    assert_eq!(core.saved_lead(&project.id).unwrap(), None);
    assert!(core.sessions().is_empty());

    // An existing tree may hold the author's notes: it stays, with its record.
    fs::write(&program, script).unwrap();
    fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).unwrap();
    let (first, _) = start_lead_with(&core, &scratch, &project.id, "claude");
    core.stop_lead(&first.session.id).unwrap();
    let saved = core.saved_lead(&project.id).unwrap();
    fs::remove_file(&program).unwrap();
    assert!(matches!(try_start(), Err(Error::OpenPty(_))));
    assert!(first.session.worktree.exists());
    assert_eq!(core.worktree_journal().unwrap().len(), 1);
    assert_eq!(core.saved_lead(&project.id).unwrap(), saved);
    assert!(core.leftovers_list().unwrap().is_empty());
}

#[test]
fn forgetting_a_lead_removes_its_tree_journal_entry_and_record() {
    let scratch = Scratch::new();
    let repo = scratch.repo("demo");
    let core = core_with_reporting_cli(&scratch);
    let project = core.add_project(&repo).unwrap().project;
    let before = branches(&repo);
    // Nothing to forget is not an error.
    core.forget_lead(&project.id, false).unwrap();
    assert_eq!(
        core.forget_lead("missing", false),
        Err(Error::UnknownProject)
    );

    let (first, _) = start_lead_with(&core, &scratch, &project.id, "claude");
    let tree = first.session.worktree.clone();
    // A live Lead is stopped first.
    assert_eq!(core.forget_lead(&project.id, true), Err(Error::LeadRunning));
    core.stop_lead(&first.session.id).unwrap();

    // Edits need the author's say-so.
    fs::write(tree.join("notes.txt"), "mine\n").unwrap();
    assert_eq!(
        core.forget_lead(&project.id, false),
        Err(Error::WorktreeHasChanges(None))
    );
    assert!(tree.exists());
    assert!(core.saved_lead(&project.id).unwrap().is_some());
    assert_eq!(core.worktree_journal().unwrap().len(), 1);

    core.forget_lead(&project.id, true).unwrap();
    assert!(!tree.exists());
    assert!(core.worktree_journal().unwrap().is_empty());
    assert_eq!(core.saved_lead(&project.id).unwrap(), None);
    assert!(!git(&repo, &["worktree", "list"]).contains("shika-lead"));
    assert_eq!(branches(&repo), before);

    // Clean, it needs no confirmation, and the next start is fresh.
    let (second, _) = start_lead_with(&core, &scratch, &project.id, "claude");
    assert_eq!(
        second.conversation,
        LeadConversation::Fresh(FreshReason::NoSavedSession)
    );
    assert_eq!(second.worktree, LeadWorktree::Created);
    core.stop_lead(&second.session.id).unwrap();
    core.forget_lead(&project.id, false).unwrap();
    assert!(!tree.exists());
}

#[test]
fn forgetting_a_lead_never_deletes_a_folder_that_is_not_its_worktree() {
    let scratch = Scratch::new();
    let repo = scratch.repo("demo");
    let core = core_with_reporting_cli(&scratch);
    let project = core.add_project(&repo).unwrap().project;
    let path = repo.join(".worktrees").join("shika-lead");
    fs::create_dir_all(&path).unwrap();
    fs::write(path.join("mine.txt"), "keep\n").unwrap();
    core.projects
        .set_lead(
            &project.id,
            Some(SavedLead {
                cli: "claude".into(),
                session_id: None,
                reported_workers: Vec::new(),
                chat_history: None,
            }),
        )
        .unwrap();
    core.journal
        .add(&JournalEntry {
            project_id: project.id.clone(),
            branch: String::new(),
            path: path.clone(),
            base_ref: None,
            started_by_lead: false,
            recovery: None,
        })
        .unwrap();
    core.forget_lead(&project.id, true).unwrap();
    assert!(path.join("mine.txt").exists());
    assert!(core.worktree_journal().unwrap().is_empty());
    assert_eq!(core.saved_lead(&project.id).unwrap(), None);
}

#[test]
fn stopping_needs_a_live_lead_and_leaves_its_workers_running() {
    let scratch = Scratch::new();
    let repo = scratch.repo("demo");
    let core = core_with_reporting_cli(&scratch);
    let project = core.add_project(&repo).unwrap().project;
    let (lead, _) = start_lead(&core, &scratch, &project.id);
    let (sink, _rx) = channel_sink();
    let worker = core
        .create_session(
            &project.id,
            "claude",
            PtySize::default(),
            sink,
            LaunchOptions {
                prompt: Some("Fix it".into()),
                started_by: Some(lead.id.clone()),
                ..LaunchOptions::default()
            },
        )
        .unwrap();
    assert_eq!(core.stop_lead(&worker.id), Err(Error::UnknownSession));
    assert_eq!(core.stop_lead("missing"), Err(Error::UnknownSession));
    core.stop_lead(&lead.id).unwrap();
    assert_eq!(core.sessions(), vec![worker.clone()]);
    assert_eq!(core.stop_lead(&lead.id), Err(Error::UnknownSession));
    assert!(core.write(worker.pty.unwrap(), b"").is_ok());
    assert!(core.lost_workers(&project.id).unwrap().is_empty());
}

#[test]
fn a_restarted_lead_reports_workers_it_started_that_lost_their_cards() {
    let scratch = Scratch::new();
    let repo = scratch.repo("demo");
    let core = core_with_reporting_cli(&scratch);
    let project = core.add_project(&repo).unwrap().project;
    let (lead, _) = start_lead(&core, &scratch, &project.id);
    let launch = |started_by: Option<String>| {
        let (sink, _rx) = channel_sink();
        core.create_session(
            &project.id,
            "claude",
            PtySize::default(),
            sink,
            LaunchOptions {
                prompt: Some("Do it".into()),
                started_by,
                ..LaunchOptions::default()
            },
        )
        .unwrap()
    };
    let worker = launch(Some(lead.id.clone()));
    let author = launch(None);
    let closed = launch(Some(lead.id.clone()));
    core.session_discard(&closed.id).unwrap();
    let journal = core.worktree_journal().unwrap();
    let flagged: Vec<_> = journal
        .iter()
        .filter(|entry| entry.started_by_lead)
        .map(|entry| entry.branch.clone())
        .collect();
    assert_eq!(flagged, std::slice::from_ref(&worker.branch));
    assert!(core.lost_workers(&project.id).unwrap().is_empty());
    drop(core);

    // A relaunch: no live sessions, journal intact.
    let core = core_with_reporting_cli(&scratch);
    let expected = vec![LostWorker {
        branch: worker.branch.clone(),
        path: worker.worktree.clone(),
    }];
    assert_eq!(core.lost_workers(&project.id).unwrap(), expected);
    assert!(core.lost_workers("other").unwrap().is_empty());
    let (start, _) = start_lead_with(&core, &scratch, &project.id, "claude");
    assert_eq!(start.lost_workers, expected);
    assert!(!start.bootstrap_sent, "only a fresh Cursor start has one");
    assert!(author.worktree.exists());

    // Once the summary named them, a later relaunch does not repeat them,
    // but the journal still lists them.
    core.mark_workers_reported(&project.id, std::slice::from_ref(&worker.worktree))
        .unwrap();
    core.stop_lead(&start.session.id).unwrap();
    let (again, _) = start_lead_with(&core, &scratch, &project.id, "claude");
    assert!(again.lost_workers.is_empty());
    assert_eq!(core.lost_workers(&project.id).unwrap(), expected);
    assert_eq!(
        core.saved_lead(&project.id)
            .unwrap()
            .unwrap()
            .reported_workers,
        std::slice::from_ref(&worker.worktree)
    );
    assert_eq!(
        core.mark_workers_reported("other", &[]),
        Err(Error::UnknownProject)
    );

    // Once the author cleans it up, it is not reported again.
    core.leftover_remove(&worker.worktree).unwrap();
    assert!(core.lost_workers(&project.id).unwrap().is_empty());
}

#[test]
fn a_lead_refuses_branch_diff_publish_and_shell_operations() {
    let scratch = Scratch::new();
    let repo = scratch.repo("demo");
    let core = core_with_reporting_cli(&scratch);
    let project = core.add_project(&repo).unwrap().project;
    let (lead, _) = start_lead(&core, &scratch, &project.id);
    let refused = Err(Error::LeadUnsupported);
    assert_eq!(core.session_rename_from_prompt(&lead.id, "x"), refused);
    assert_eq!(
        core.session_apply_cli_title(&lead.id),
        Err(Error::LeadUnsupported)
    );
    assert_eq!(
        core.session_push_and_close(&lead.id),
        Err(Error::LeadUnsupported)
    );
    assert!(matches!(
        core.session_publish_preview(&lead.id),
        Err(Error::LeadUnsupported)
    ));
    assert!(matches!(
        core.session_publish_now(&lead.id, None, None),
        Err(Error::LeadUnsupported)
    ));
    assert!(matches!(
        core.session_diff(&lead.id),
        Err(Error::LeadUnsupported)
    ));
    assert!(matches!(
        core.session_switched_close_check(&lead.id),
        Err(Error::LeadUnsupported)
    ));
    let (sink, _rx) = channel_sink();
    assert_eq!(
        core.open_shell(&lead.id, PtySize::default(), sink),
        Err(Error::LeadUnsupported)
    );
    let (sink, _rx) = channel_sink();
    assert_eq!(
        core.session_switch_agent(
            &lead.id,
            "claude",
            Some("Continue the backend."),
            PtySize::default(),
            sink,
            None,
        ),
        Err(Error::LeadUnsupported)
    );
    assert_eq!(core.session(&lead.id), Some(lead));
}
