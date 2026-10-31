use super::*;

#[test]
fn a_lead_is_a_fixed_detached_worktree_with_its_env_and_no_branch() {
    let scratch = Scratch::new();
    let repo = scratch.repo("demo");
    let core = core_with_reporting_cli(&scratch);
    let project = core.add_project(&repo).unwrap().project;
    // An unapproved preparation config blocks New but must not touch a Lead.
    fs::create_dir_all(repo.join(".shika")).unwrap();
    fs::write(
        repo.join(preparation::CONFIG_PATH),
        "{\"setup-worktree\":[\"touch ran-setup\"]}",
    )
    .unwrap();
    let before = branches(&repo);

    let (start, report) = start_lead_with(&core, &scratch, &project.id, "claude");
    let lead = start.session.clone();
    assert!(lead.is_lead() && lead.started_by.is_none());
    assert_eq!(lead.title, "Lead");
    assert_eq!(lead.branch, "");
    assert_eq!(lead.worktree, repo.join(".worktrees").join("shika-lead"));
    assert_eq!(start.worktree, LeadWorktree::Created);
    assert_eq!(
        start.conversation,
        LeadConversation::Fresh(FreshReason::NoSavedSession)
    );
    assert!(start.lost_workers.is_empty());
    assert_eq!(
        git(&lead.worktree, &["rev-parse", "--abbrev-ref", "HEAD"]).trim(),
        "HEAD"
    );
    assert_eq!(
        git(&lead.worktree, &["rev-parse", "HEAD"]),
        git(&repo, &["rev-parse", "main"])
    );
    assert_eq!(branches(&repo), before);
    assert!(!lead.worktree.join("ran-setup").exists());
    let journal = core.worktree_journal().unwrap();
    assert_eq!(journal.len(), 1);
    assert_eq!(journal[0].path, lead.worktree);
    assert_eq!(journal[0].branch, "");
    assert!(
        fs::read_to_string(repo.join(".git/info/exclude"))
            .unwrap()
            .contains(".worktrees/")
    );
    assert_eq!(core.lead_for_project(&project.id), Some(lead.clone()));
    assert!(core.lead_for_project("other").is_none());

    // The preset's flags, a fresh Claude session id, the instructions as
    // the system prompt, and no positional prompt after them.
    let args = report_args(&report);
    assert_eq!(args[0], "--dangerously-skip-permissions");
    assert_eq!(args[1], "--session-id");
    let session_id = args[2].clone();
    assert_eq!(args[3], "--append-system-prompt");
    assert_eq!(args[4], LEAD_INSTRUCTIONS);
    assert_eq!(args.len(), 5, "{args:?}");
    assert_eq!(
        core.saved_lead(&project.id).unwrap(),
        Some(SavedLead {
            cli: "claude".into(),
            session_id: Some(session_id),
            reported_workers: Vec::new(),
            chat_history: None,
        })
    );
    // The control env is set and the shika command directory leads PATH.
    let control = scratch.path.join("control");
    assert!(
        report.contains(&format!("sock:{}", control.join("sock").display())),
        "{report}"
    );
    assert!(report.contains("token:lead-token"), "{report}");
    assert!(
        report.contains(&format!("path:{}:", control.join("bin").display())),
        "{report}"
    );
    assert!(core.taken_ids().unwrap().contains(&lead.id));

    // One Lead per project, and a refusal creates nothing.
    let (sink, _rx) = channel_sink();
    assert_eq!(
        core.start_lead(
            &project.id,
            "claude",
            PtySize::default(),
            sink,
            lead_launch(&scratch)
        )
        .map(|_| ()),
        Err(Error::LeadExists)
    );
    assert_eq!(core.worktree_journal().unwrap().len(), 1);
    assert_eq!(core.sessions().len(), 1);
}

#[test]
fn a_lead_needs_good_instructions_and_a_found_cli() {
    let scratch = Scratch::new();
    let repo = scratch.repo("demo");
    let core = core_with_reporting_cli(&scratch);
    let project = core.add_project(&repo).unwrap().project;
    for text in ["", "-x"] {
        let (sink, _rx) = channel_sink();
        let mut launch = lead_launch(&scratch);
        launch.instructions = text.into();
        assert!(matches!(
            core.start_lead(&project.id, "claude", PtySize::default(), sink, launch),
            Err(Error::InvalidPrompt(_))
        ));
    }
    let (sink, _rx) = channel_sink();
    let mut launch = lead_launch(&scratch);
    launch.cursor_bootstrap = Some("--help".into());
    assert!(matches!(
        core.start_lead(&project.id, "cursor", PtySize::default(), sink, launch),
        Err(Error::InvalidPrompt(_))
    ));
    let (sink, _rx) = channel_sink();
    assert!(matches!(
        core.start_lead(
            &project.id,
            "nope",
            PtySize::default(),
            sink,
            lead_launch(&scratch)
        ),
        Err(Error::UnknownCli)
    ));
    let core = core_with_fake_cli(&scratch);
    let (sink, _rx) = channel_sink();
    assert_eq!(
        core.start_lead(
            &project.id,
            "codex",
            PtySize::default(),
            sink,
            lead_launch(&scratch)
        )
        .map(|_| ()),
        Err(Error::CliNotFound("Codex".into()))
    );
    assert!(core.worktree_journal().unwrap().is_empty());
    assert!(!repo.join(".worktrees").exists());
    assert_eq!(core.saved_lead(&project.id).unwrap(), None);
}

#[test]
fn memory_notes_ride_every_lead_start_and_outlive_stop_forget_and_a_cli_change() {
    let scratch = Scratch::new();
    let repo = scratch.repo("demo");
    let core = core_with_reporting_cli(&scratch);
    let project = core.add_project(&repo).unwrap().project;
    let prompt_of = |report: &str| report_args(report)[4].clone();

    // No notes: the instructions are exactly what the app passed.
    let (lead, report) = start_lead(&core, &scratch, &project.id);
    assert_eq!(prompt_of(&report), LEAD_INSTRUCTIONS);
    assert_eq!(core.remember(&project.id, "Use pnpm.\nNot npm."), Ok(1));
    assert_eq!(core.remember(&project.id, "Workers must not push."), Ok(2));
    core.stop_lead(&lead.id).unwrap();

    // Resumed or fresh, the notes follow the guide as the last section.
    // The fake CLI prints one `arg:` per line, so check the text itself.
    let both = |report: &str| {
        let text = report.replace("\r\n", "\n");
        text.contains(&format!("{LEAD_INSTRUCTIONS}\n"))
            && text.contains("\n\n## Memory\n")
            && text.contains("`shika remember`")
            && text.contains("- Use pnpm. Not npm.\n")
            && text.contains("- Workers must not push.\n")
    };
    let (lead, report) = start_lead(&core, &scratch, &project.id);
    assert!(both(&report), "{report}");
    core.stop_lead(&lead.id).unwrap();
    core.forget_lead(&project.id, false).unwrap();
    assert_eq!(core.lead_memory(&project.id).unwrap().len(), 2);
    let (lead, report) = start_lead(&core, &scratch, &project.id);
    assert!(both(&report), "{report}");
    core.stop_lead(&lead.id).unwrap();

    // Another CLI sees them too, and a forgotten note is gone from the
    // next start.
    core.forget_note(&project.id, 1).unwrap();
    let (start, report) = start_lead_with(&core, &scratch, &project.id, "codex");
    assert!(report.contains("Workers must not push."), "{report}");
    assert!(!report.contains("Use pnpm"), "{report}");
    core.stop_lead(&start.session.id).unwrap();

    // A corrupt file never blocks a start; the Lead just has no notes.
    let file = core.memory_path(&project.id).unwrap();
    fs::write(&file, "{ not json").unwrap();
    let (_, report) = start_lead(&core, &scratch, &project.id);
    assert_eq!(prompt_of(&report), LEAD_INSTRUCTIONS);
    assert!(matches!(
        core.remember(&project.id, "x"),
        Err(Error::MemoryUnreadable(_))
    ));
    assert_eq!(fs::read_to_string(&file).unwrap(), "{ not json");
}

#[test]
fn memory_belongs_to_a_known_project_and_goes_with_it() {
    let scratch = Scratch::new();
    let repo = scratch.repo("demo");
    let core = core_with_reporting_cli(&scratch);
    assert_eq!(core.remember("nope", "x"), Err(Error::UnknownProject));
    let project = core.add_project(&repo).unwrap().project;
    assert!(core.lead_memory(&project.id).unwrap().is_empty());
    core.remember(&project.id, "Keep me.").unwrap();
    let file = core.memory_path(&project.id).unwrap();
    assert_eq!(
        file,
        core.data_dir()
            .join("leads")
            .join(&project.id)
            .join("memory.json")
    );
    assert!(file.exists());
    core.remove_project(&project.id).unwrap();
    assert!(!file.exists());
    assert!(!file.parent().unwrap().exists());
}

#[test]
fn workers_get_the_prompt_last_an_owner_and_no_control_env() {
    let scratch = Scratch::new();
    let repo = scratch.repo("demo");
    let core = core_with_reporting_cli(&scratch);
    let project = core.add_project(&repo).unwrap().project;
    let (lead, _) = start_lead(&core, &scratch, &project.id);

    let (sink, rx) = channel_sink();
    let worker = core
        .create_session(
            &project.id,
            "claude",
            PtySize::new(30, 90),
            sink,
            LaunchOptions {
                prompt: Some("Fix the login bug\nsecond line".into()),
                started_by: Some(lead.id.clone()),
                ..LaunchOptions::default()
            },
        )
        .unwrap();
    let report = collect_until(&rx, "end-report", Duration::from_secs(5));
    assert!(!worker.is_lead());
    assert_eq!(worker.started_by.as_deref(), Some(lead.id.as_str()));
    assert_eq!(worker.title, "New Claude Code");
    assert_eq!(worker.branch, format!("shika-draft-{}", worker.id));
    assert!(
        report.contains("sock:\r\n") && report.contains("token:\r\n"),
        "{report}"
    );
    assert!(
        !report.contains("lead-token") && !report.contains("control/bin"),
        "{report}"
    );
    assert_eq!(core.workers_of(&lead.id), vec![worker.clone()]);
    assert!(core.workers_of(&worker.id).is_empty());

    // Draft and lead ids never collide.
    assert_ne!(worker.id, lead.id);

    // Bad prompts are refused before anything is created.
    let journal = core.worktree_journal().unwrap();
    for prompt in ["", "  ", "--help", "-p"] {
        let (sink, _rx) = channel_sink();
        let result = core.create_session(
            &project.id,
            "claude",
            PtySize::default(),
            sink,
            LaunchOptions {
                prompt: Some(prompt.into()),
                started_by: None,
                ..LaunchOptions::default()
            },
        );
        assert!(matches!(result, Err(Error::InvalidPrompt(_))), "{prompt:?}");
    }
    assert_eq!(core.worktree_journal().unwrap(), journal);
    assert_eq!(core.sessions().len(), 2);
}

#[test]
fn only_a_lead_started_workers_agent_gets_the_control_env() {
    let scratch = Scratch::new();
    let repo = scratch.repo("demo");
    let core = core_with_reporting_cli(&scratch);
    let project = core.add_project(&repo).unwrap().project;
    let (lead, _) = start_lead(&core, &scratch, &project.id);
    let control = scratch.path.join("control");

    let (sink, rx) = channel_sink();
    let worker = core
        .create_session(
            &project.id,
            "claude",
            PtySize::new(30, 90),
            sink,
            LaunchOptions {
                prompt: Some("Fix the login bug".into()),
                started_by: Some(lead.id.clone()),
                control: Some(WorkerEnv {
                    socket: control.join("sock"),
                    token: "worker-token".into(),
                    bin_dir: control.join("bin"),
                }),
                ..LaunchOptions::default()
            },
        )
        .unwrap();
    let report = collect_until(&rx, "end-report", Duration::from_secs(5));
    assert!(report.contains("token:worker-token"), "{report}");
    assert!(
        report.contains(&format!("sock:{}", control.join("sock").display())),
        "{report}"
    );
    assert!(
        report.contains(&format!("path:{}:", control.join("bin").display())),
        "{report}"
    );
    assert!(!report.contains("lead-token"), "{report}");

    // Its shell tab is spawned with no control variables on purpose, so
    // the PTY layer scrubs whatever the app itself inherited.
    let env = core.path_env();
    let shell = session::shell_request(
        &path_env::user_shell(),
        &worker.worktree,
        env.path(),
        PtySize::default(),
    );
    assert!(shell.env.is_empty());
    assert!(!shell.path.contains("control/bin"));

    // A card the author creates gets none, even with a Lead around.
    let (sink, rx) = channel_sink();
    core.create_session(
        &project.id,
        "claude",
        PtySize::new(30, 90),
        sink,
        LaunchOptions {
            prompt: Some("Author task".into()),
            ..LaunchOptions::default()
        },
    )
    .unwrap();
    let report = collect_until(&rx, "end-report", Duration::from_secs(5));
    assert!(
        report.contains("sock:\r\n") && report.contains("token:\r\n"),
        "{report}"
    );
    assert!(!report.contains("control/bin"), "{report}");
}

#[test]
fn leads_can_only_be_stopped_and_forgotten_through_the_lead_lifecycle() {
    let scratch = Scratch::new();
    let repo = scratch.repo("demo");
    let core = core_with_reporting_cli(&scratch);
    let project = core.add_project(&repo).unwrap().project;
    let (lead, _) = start_lead(&core, &scratch, &project.id);
    let before = branches(&repo);
    let saved = core.saved_lead(&project.id).unwrap();
    assert_eq!(core.session_diff_stat(&lead.id), Ok(DiffStat::default()));
    assert_eq!(core.session_pushed_head(&lead.id), Ok(None));
    assert_eq!(core.session_refresh_branch(&lead.id).unwrap(), lead);

    // Neither a clean tree, uncommitted work, nor detached commits may
    // take the old task-close shortcut past Forget's safety checks.
    for state in ["clean", "dirty", "committed"] {
        if state == "dirty" {
            fs::write(lead.worktree.join("edit.txt"), "x\n").unwrap();
        } else if state == "committed" {
            git(&lead.worktree, &["add", "edit.txt"]);
            git(&lead.worktree, &["commit", "-m", "keep this"]);
        }
        for working in [false, true] {
            assert_eq!(
                core.session_close(&lead.id, working),
                Err(Error::LeadUnsupported)
            );
            assert_eq!(
                core.close_session(&lead.id, working),
                Err(Error::LeadUnsupported)
            );
        }
        assert_eq!(core.session_discard(&lead.id), Err(Error::LeadUnsupported));
        assert_eq!(core.session(&lead.id), Some(lead.clone()));
        assert_eq!(core.saved_lead(&project.id).unwrap(), saved);
        assert!(lead.worktree.exists());
    }
    core.stop_lead(&lead.id).unwrap();
    assert_eq!(
        core.forget_lead(&project.id, false),
        Err(Error::WorktreeHasChanges(None))
    );
    core.forget_lead(&project.id, true).unwrap();
    assert!(!lead.worktree.exists());
    assert!(core.worktree_journal().unwrap().is_empty());
    assert_eq!(core.saved_lead(&project.id).unwrap(), None);
    assert_eq!(branches(&repo), before);
}

#[test]
fn the_fixed_lead_worktree_is_not_a_leftover_while_its_record_exists() {
    let scratch = Scratch::new();
    let repo = scratch.repo("demo");
    let core = core_with_reporting_cli(&scratch);
    let project = core.add_project(&repo).unwrap().project;
    let (lead, _) = start_lead(&core, &scratch, &project.id);
    assert!(core.leftovers_list().unwrap().is_empty());
    assert_eq!(
        core.leftover_remove(&lead.worktree),
        Err(Error::UnknownLeftover)
    );
    let before = branches(&repo);
    drop(core);

    // After a relaunch the Lead is not live, but its record keeps the
    // tree out of the list: Forget removes it, not Leftovers.
    let core = core_with_reporting_cli(&scratch);
    assert!(core.lead_for_project(&project.id).is_none());
    assert!(core.leftovers_list().unwrap().is_empty());
    assert_eq!(core.worktree_journal().unwrap().len(), 1);

    // Without a record the tree may be listed and removed by hand. Ids
    // stay unique against a journaled Lead folder from an earlier run.
    core.projects.set_lead(&project.id, None).unwrap();
    let leftovers = core.leftovers_list().unwrap();
    assert_eq!(leftovers.len(), 1);
    assert_eq!(leftovers[0].path, lead.worktree);
    assert_eq!(leftovers[0].branch, "");
    core.leftover_remove(&lead.worktree).unwrap();
    assert!(!lead.worktree.exists());
    assert!(core.leftovers_list().unwrap().is_empty());
    assert_eq!(branches(&repo), before);
    assert!(!git(&repo, &["worktree", "list"]).contains("shika-lead"));
}

#[test]
fn removing_a_project_keeps_the_lead_tree_and_lists_it_once_the_record_is_gone() {
    let scratch = Scratch::new();
    let repo = scratch.repo("demo");
    let core = core_with_reporting_cli(&scratch);
    let project = core.add_project(&repo).unwrap().project;
    let (lead, _) = start_lead(&core, &scratch, &project.id);
    assert_eq!(core.remove_project(&project.id).unwrap().len(), 1);
    assert!(core.projects().unwrap().is_empty());
    assert!(lead.worktree.exists());
    // The project, and so its record, is gone: the tree is a leftover.
    let leftovers = core.leftovers_list().unwrap();
    assert_eq!(leftovers.len(), 1);
    assert_eq!(leftovers[0].path, lead.worktree);
}

#[test]
fn an_old_per_id_lead_tree_stays_an_ordinary_leftover() {
    let scratch = Scratch::new();
    let repo = scratch.repo("demo");
    let core = core_with_reporting_cli(&scratch);
    let project = core.add_project(&repo).unwrap().project;
    let (lead, _) = start_lead(&core, &scratch, &project.id);
    let old = repo.join(".worktrees").join("shika-lead-18dabc");
    git(
        &repo,
        &["worktree", "add", "--detach", old.to_str().unwrap(), "main"],
    );
    core.journal
        .add(&JournalEntry {
            project_id: project.id.clone(),
            branch: String::new(),
            path: old.clone(),
            base_ref: None,
            started_by_lead: false,
            recovery: None,
        })
        .unwrap();
    let leftovers = core.leftovers_list().unwrap();
    assert_eq!(leftovers.len(), 1);
    assert_eq!(leftovers[0].path, old);
    assert!(core.taken_ids().unwrap().contains(&"18dabc".to_string()));
    core.leftover_remove(&old).unwrap();
    assert!(!old.exists());
    assert!(lead.worktree.exists());
}
