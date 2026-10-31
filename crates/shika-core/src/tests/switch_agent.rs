use super::*;

const SWITCH_PROMPT: &str = "Continue the backend.";

#[test]
fn switching_the_agent_keeps_the_worktree_branch_shells_and_naming() {
    let scratch = Scratch::new();
    let repo = scratch.repo("demo");
    let core = core_with_switch_clis(&scratch);
    let project = core.add_project(&repo).unwrap().project;
    let (sink, _rx) = channel_sink();
    let session = core
        .create_session(
            &project.id,
            "claude",
            PtySize::new(30, 90),
            sink,
            LaunchOptions::default(),
        )
        .unwrap();
    let (shell_sink, _shell_rx) = channel_sink();
    let shell = core
        .open_shell(&session.id, PtySize::new(30, 90), shell_sink)
        .unwrap();
    fs::write(session.worktree.join("wip.txt"), "keep\n").unwrap();
    fs::create_dir_all(repo.join(".shika")).unwrap();
    fs::write(
        repo.join(preparation::CONFIG_PATH),
        "{\"setup-worktree\":[\"touch ran-setup\"]}",
    )
    .unwrap();
    let branch = session.branch.clone();
    let title = session.title.clone();
    let worktree = session.worktree.clone();
    let base = session.base_ref.clone();
    let old_pty = session.pty;
    assert_eq!(branch, format!("shika-draft-{}", session.id));
    assert!(!session.cli_titled);

    let (switched, report) = switch_agent(&core, &session.id, "cursor", Some(SWITCH_PROMPT), None);
    assert_eq!(
        report_args(&report),
        vec![
            "--yolo".to_string(),
            "--trust".to_string(),
            "--sandbox".to_string(),
            "disabled".to_string(),
            SWITCH_PROMPT.to_string(),
        ],
        "{report}"
    );
    assert!(
        report.contains(&format!("cwd:{}", worktree.display())),
        "{report}"
    );
    assert!(
        report.contains("sock:\r\n") && report.contains("token:\r\n"),
        "{report}"
    );
    assert!(!report.contains("shika report"), "{report}");
    assert!(!session.worktree.join("ran-setup").exists());
    assert_eq!(
        fs::read_to_string(worktree.join("wip.txt")).unwrap(),
        "keep\n"
    );
    assert_eq!(
        git(&worktree, &["symbolic-ref", "--short", "HEAD"]).trim(),
        branch
    );
    assert_eq!(switched.branch, branch);
    // Unnamed, the title follows the CLI; a named task keeps its own.
    assert_eq!(title, "New Claude Code");
    assert_eq!(switched.title, "New Cursor CLI");
    assert!(!switched.manual_title);
    // Still on its draft branch, the task is named later as New would.
    assert!(!switched.cli_titled);
    assert_eq!(switched.worktree, worktree);
    assert_eq!(switched.base_ref, base);
    assert_eq!(switched.started_by, None);
    assert_eq!(switched.preset_id, "cursor");
    assert_eq!(switched.preset_name, "Cursor CLI");
    assert_ne!(switched.pty, old_pty);
    assert_eq!(switched.shell_ptys, vec![shell]);
    assert!(!switched.is_lead());
    assert_eq!(
        core.write(old_pty.unwrap(), b"still there\n"),
        Err(Error::UnknownPty)
    );
    assert!(core.write(shell, b"echo shells-stay\n").is_ok());
    assert!(
        core.write(switched.pty.unwrap(), b"echo reviewer\n")
            .is_ok()
    );
    let journal = core
        .worktree_journal()
        .unwrap()
        .into_iter()
        .find(|entry| entry.path == worktree)
        .unwrap();
    assert_eq!(journal.branch, branch);

    let cursor_pty = switched.pty;
    let (again, report) = switch_agent(&core, &session.id, "claude", Some(SWITCH_PROMPT), None);
    assert_eq!(
        report_args(&report),
        vec![
            "--dangerously-skip-permissions".to_string(),
            SWITCH_PROMPT.to_string(),
        ],
        "{report}"
    );
    assert_eq!(again.branch, branch);
    assert_eq!(again.title, title);
    assert!(!again.cli_titled);
    assert_eq!(again.preset_id, "claude");
    assert_ne!(again.pty, cursor_pty);
    assert_eq!(again.shell_ptys, vec![shell]);
    assert_eq!(
        core.write(cursor_pty.unwrap(), b"x"),
        Err(Error::UnknownPty)
    );
    assert!(core.write(shell, b"echo still\n").is_ok());

    let (pi, report) = switch_agent(&core, &session.id, "pi", Some(SWITCH_PROMPT), None);
    let args = report_args(&report);
    assert_eq!(
        args.first().map(String::as_str),
        Some("--approve"),
        "{report}"
    );
    assert!(args.iter().any(|arg| arg == "--extension"), "{report}");
    assert_eq!(
        args.last().map(String::as_str),
        Some(SWITCH_PROMPT),
        "{report}"
    );
    assert!(
        !args
            .iter()
            .any(|arg| arg == "--worktree" || arg.starts_with("--worktree=")),
        "{report}"
    );
    assert_eq!(pi.branch, branch);
    assert_eq!(pi.shell_ptys, vec![shell]);
    assert!(core.write(shell, b"echo after-pi\n").is_ok());

    let (blank, report) = switch_agent(&core, &session.id, "claude", None, None);
    assert_eq!(
        report_args(&report),
        vec!["--dangerously-skip-permissions".to_string()],
        "{report}"
    );
    assert_eq!(blank.branch, branch);
    assert_eq!(blank.title, title);
    assert!(!blank.cli_titled);
    assert_eq!(blank.shell_ptys, vec![shell]);

    let (spaces, report) = switch_agent(&core, &session.id, "claude", Some("  \n"), None);
    assert_eq!(
        report_args(&report),
        vec!["--dangerously-skip-permissions".to_string()],
        "{report}"
    );
    assert_eq!(spaces.branch, branch);
    assert_eq!(spaces.title, title);
    assert!(!spaces.cli_titled);

    // The first line typed into the new CLI names the draft task.
    let renamed = core
        .session_rename_from_prompt(&session.id, "Add the export button")
        .unwrap();
    assert_ne!(renamed.branch, branch);
    assert_eq!(renamed.title, "Add the export button");
    assert_eq!(
        git(&worktree, &["symbolic-ref", "--short", "HEAD"]).trim(),
        renamed.branch
    );
    let (later, _) = switch_agent(&core, &session.id, "cursor", Some(SWITCH_PROMPT), None);
    assert_eq!(later.branch, renamed.branch);
    assert_eq!(later.title, renamed.title);
    assert!(later.cli_titled);

    fs::remove_file(repo.join(preparation::CONFIG_PATH)).unwrap();
    let (sink, _rx) = channel_sink();
    let named = core
        .create_session(
            &project.id,
            "claude",
            PtySize::new(30, 90),
            sink,
            LaunchOptions::default(),
        )
        .unwrap();
    let named = core
        .session_rename_from_prompt(&named.id, "Fix the login bug")
        .unwrap();
    let named_branch = named.branch.clone();
    let named_title = named.title.clone();
    assert_ne!(named_branch, format!("shika-draft-{}", named.id));
    assert!(!named.cli_titled);
    let (kept, _) = switch_agent(&core, &named.id, "cursor", Some(SWITCH_PROMPT), None);
    assert_eq!(kept.branch, named_branch);
    assert_eq!(kept.title, named_title);
    assert!(kept.cli_titled);
    assert_eq!(core.session_apply_cli_title(&named.id).unwrap(), None);
    assert_eq!(
        git(&kept.worktree, &["symbolic-ref", "--short", "HEAD"]).trim(),
        named_branch
    );
}

#[test]
fn a_failed_agent_switch_leaves_the_original_process_running() {
    let scratch = Scratch::new();
    let repo = scratch.repo("demo");
    let claude = write_exec(&scratch.path, "claude", SWITCH_CLI);
    let env = PathEnv::from_lookup(
        "/usr/bin:/bin".into(),
        None,
        &[
            ("claude", Some(claude)),
            (
                "agent",
                Some(scratch.path.join("bin").join("missing-agent")),
            ),
        ],
    );
    let core = Core::open_with(scratch.path.join("data"), env).unwrap();
    let project = core.add_project(&repo).unwrap().project;
    let (sink, _rx) = channel_sink();
    let session = core
        .create_session(
            &project.id,
            "claude",
            PtySize::new(30, 90),
            sink,
            LaunchOptions::default(),
        )
        .unwrap();
    let old = session.pty;
    let (sink, _rx) = channel_sink();
    assert!(
        core.session_switch_agent(
            &session.id,
            "cursor",
            Some(SWITCH_PROMPT),
            PtySize::new(30, 90),
            sink,
            None,
        )
        .is_err()
    );
    let stayed = core.session(&session.id).unwrap();
    assert_eq!(stayed.pty, old);
    assert_eq!(stayed.preset_id, "claude");
    assert!(!stayed.cli_titled);
    assert_eq!(stayed.branch, session.branch);
    assert!(core.write(old.unwrap(), b"still running\n").is_ok());

    for prompt in ["-x", "a\0b"] {
        let (sink, _rx) = channel_sink();
        assert!(
            matches!(
                core.session_switch_agent(
                    &session.id,
                    "claude",
                    Some(prompt),
                    PtySize::default(),
                    sink,
                    None,
                ),
                Err(Error::InvalidPrompt(_))
            ),
            "{prompt:?}"
        );
    }
    let (sink, _rx) = channel_sink();
    assert_eq!(
        core.session_switch_agent(
            &session.id,
            "nope",
            Some(SWITCH_PROMPT),
            PtySize::default(),
            sink,
            None,
        ),
        Err(Error::UnknownCli)
    );
    let stayed = core.session(&session.id).unwrap();
    assert_eq!(stayed.pty, old);
    assert!(!stayed.cli_titled);
    assert!(core.write(old.unwrap(), b"still running\n").is_ok());
}

#[test]
fn a_worker_switch_keeps_its_owner_and_control_env_without_the_new_task_footer() {
    let scratch = Scratch::new();
    let repo = scratch.repo("demo");
    let core = core_with_switch_clis(&scratch);
    let project = core.add_project(&repo).unwrap().project;
    let (lead, _) = start_lead(&core, &scratch, &project.id);
    let control = scratch.path.join("control");
    let env = WorkerEnv {
        socket: control.join("sock"),
        token: "worker-token".into(),
        bin_dir: control.join("bin"),
    };
    let (sink, _rx) = channel_sink();
    let worker = core
        .create_session(
            &project.id,
            "claude",
            PtySize::new(30, 90),
            sink,
            LaunchOptions {
                prompt: Some("Fix the login bug".into()),
                started_by: Some(lead.id.clone()),
                control: Some(env.clone()),
                ..LaunchOptions::default()
            },
        )
        .unwrap();
    let (switched, report) =
        switch_agent(&core, &worker.id, "cursor", Some(SWITCH_PROMPT), Some(env));
    assert_eq!(switched.started_by.as_deref(), Some(lead.id.as_str()));
    assert_eq!(switched.branch, worker.branch);
    assert_eq!(
        report_args(&report).last().map(String::as_str),
        Some(SWITCH_PROMPT)
    );
    assert!(!report.contains("shika report"), "{report}");
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
}
