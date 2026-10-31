use super::*;

fn terminal_task(core: &Core, project: &str) -> Session {
    core.create_terminal_with_preparation(
        project,
        PtySize::new(30, 90),
        |_, _| {},
        PreparationControl::default(),
        |_| {},
    )
    .unwrap()
}

#[test]
fn terminal_tasks_need_no_agent_and_keep_shell_commands_out_of_naming() {
    let scratch = Scratch::new();
    let repo = scratch.repo("terminal-repo");
    let core = Core::open_with(
        scratch.path.join("data"),
        PathEnv::from_lookup("/usr/bin:/bin".into(), None, &[]),
    )
    .unwrap();
    assert!(picker_presets(&core.cli_catalog().presets, |_| true).is_empty());
    let project = core.add_project(&repo).unwrap().project;
    let session = terminal_task(&core, &project.id);
    assert!(session.is_terminal());
    assert!(session.on_draft_branch());
    assert!(session.title.starts_with("Terminal "));
    assert_eq!(session.base_ref.as_deref(), Some("refs/heads/main"));
    assert!(!has_upstream(&session.worktree));
    assert_eq!(
        core.session_rename_from_prompt(&session.id, "echo do-not-name")
            .unwrap(),
        session
    );
    assert_eq!(core.session_apply_cli_title(&session.id).unwrap(), None);
    let renamed = core
        .session_set_title(&session.id, "Investigate locally")
        .unwrap();
    assert_eq!(renamed.branch, session.branch);
    fs::write(session.worktree.join("local.txt"), "keep\n").unwrap();
    assert_eq!(core.session_diff_stat(&session.id).unwrap().files, 1);
    assert!(core.session_close(&session.id, false).is_err());
    assert!(session.worktree.exists());
    core.session_discard(&session.id).unwrap();
    assert!(!session.worktree.exists());
    assert_eq!(
        core.write(session.pty.unwrap(), b"x"),
        Err(Error::UnknownPty)
    );
}

#[test]
fn terminal_tasks_reopen_as_shells_without_agents_or_setup() {
    let scratch = Scratch::new();
    let repo = scratch.repo("terminal-recovery");
    let core = Core::open_with(
        scratch.path.join("data"),
        PathEnv::from_lookup("/usr/bin:/bin".into(), None, &[]),
    )
    .unwrap();
    let project = core.add_project(&repo).unwrap().project;
    let session = terminal_task(&core, &project.id);
    core.session_set_title(&session.id, "Local investigation")
        .unwrap();
    fs::write(session.worktree.join("local.txt"), "keep\n").unwrap();
    core.checkpoint_sessions().unwrap();
    core.cancel_session_start(&session.id).unwrap();
    fs::create_dir_all(repo.join(".shika")).unwrap();
    fs::write(
        repo.join(preparation::CONFIG_PATH),
        "{\"setup-worktree\":[\"exit 42\"]}",
    )
    .unwrap();
    let preview = core.leftover_recovery(&session.worktree).unwrap();
    assert_eq!(preview.recovery.as_ref().unwrap().cli, "terminal");
    assert_eq!(preview.recovery.as_ref().unwrap().conversation, None);
    let (sink, rx) = channel_sink();
    let recovered = core
        .reopen_leftover(
            &session.worktree,
            "terminal",
            false,
            PtySize::new(30, 90),
            sink,
        )
        .unwrap();
    assert!(recovered.is_terminal());
    assert_eq!(recovered.title, "Local investigation");
    assert_eq!(recovered.branch, session.branch);
    assert_eq!(
        fs::read_to_string(recovered.worktree.join("local.txt")).unwrap(),
        "keep\n"
    );
    core.write(recovered.pty.unwrap(), b"printf 'shell-%s\\n' ready\r")
        .unwrap();
    let output = collect_until(&rx, "shell-ready", Duration::from_secs(5));
    assert!(output.contains("shell-ready"), "{output}");
    core.session_discard(&recovered.id).unwrap();
}

#[test]
fn starting_an_agent_keeps_the_terminal_and_existing_shells_running() {
    let scratch = Scratch::new();
    let repo = scratch.repo("terminal-attach");
    let core = core_with_fake_cli(&scratch);
    let project = core.add_project(&repo).unwrap().project;
    let session = terminal_task(&core, &project.id);
    let journal = core.worktree_journal().unwrap();
    assert!(
        core.session_switch_agent(
            &session.id,
            "missing",
            None,
            PtySize::default(),
            |_, _| {},
            None
        )
        .is_err()
    );
    assert_eq!(core.session(&session.id), Some(session.clone()));
    assert_eq!(core.worktree_journal().unwrap(), journal);
    assert!(
        core.write(session.pty.unwrap(), b"printf alive\\n\r")
            .is_ok()
    );
    let extra = core
        .open_shell(&session.id, PtySize::default(), |_, _| {})
        .unwrap();
    let switched = core
        .session_switch_agent(
            &session.id,
            "claude",
            None,
            PtySize::default(),
            |_, _| {},
            None,
        )
        .unwrap();
    assert!(!switched.is_terminal());
    assert_eq!(switched.worktree, session.worktree);
    assert_eq!(switched.branch, session.branch);
    assert_eq!(switched.shell_ptys, vec![extra, session.pty.unwrap()]);
    assert!(!switched.cli_titled);
    assert!(
        core.write(session.pty.unwrap(), b"printf kept\\n\r")
            .is_ok()
    );
    let named = core
        .session_rename_from_prompt(&session.id, "Investigate startup")
        .unwrap();
    assert_eq!(named.branch, "investigate-startup");
    core.session_discard(&session.id).unwrap();
    for pty in [session.pty.unwrap(), extra, switched.pty.unwrap()] {
        assert_eq!(core.write(pty, b"x"), Err(Error::UnknownPty));
    }
}
