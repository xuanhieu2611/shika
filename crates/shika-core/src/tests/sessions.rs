use super::*;

#[test]
fn core_can_be_shared_with_a_background_executor() {
    fn shared<T: Send + Sync>() {}
    shared::<Core>();
}

#[test]
fn the_data_dir_is_the_one_tauri_used() {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap();
    assert_eq!(
        app_data_dir().unwrap(),
        home.join("Library/Application Support/com.hieule.shika")
    );
}

#[test]
fn files_written_by_the_tauri_build_load_from_an_injected_dir() {
    let scratch = Scratch::new();
    let data = scratch.path.join("data");
    fs::create_dir_all(&data).unwrap();
    fs::write(
        data.join("projects.json"),
        "[\n  {\n    \"id\": \"18db38e2f78faa00\",\n    \"name\": \"sample-project\",\n    \"path\": \"/Users/x/code/sample-project\"\n  }\n]\n",
    )
    .unwrap();
    fs::write(
        data.join("worktrees.json"),
        "[\n  {\n    \"projectId\": \"18db38e2f78faa00\",\n    \"branch\": \"shika-draft-1\",\n    \"path\": \"/Users/x/code/sample-project/.worktrees/shika-draft-1\"\n  }\n]\n",
    )
    .unwrap();
    let core = Core::open(&data).unwrap();
    assert_eq!(core.data_dir(), data);
    assert_eq!(
        core.projects().unwrap(),
        [Project {
            id: "18db38e2f78faa00".into(),
            name: "sample-project".into(),
            path: PathBuf::from("/Users/x/code/sample-project"),
            base_branch: None,
            approved_preparation: None,
            worktree_setup_reviewed: false,
            lead: None,
        }]
    );
    assert_eq!(
        core.worktree_journal().unwrap(),
        [JournalEntry {
            project_id: "18db38e2f78faa00".into(),
            branch: "shika-draft-1".into(),
            path: PathBuf::from("/Users/x/code/sample-project/.worktrees/shika-draft-1"),
            base_ref: None,
            started_by_lead: false,
            recovery: None,
        }]
    );

    // A missing directory is created and starts empty.
    let fresh = Core::open(scratch.path.join("new").join("data")).unwrap();
    assert!(fresh.projects().unwrap().is_empty());
    assert!(fresh.data_dir().is_dir());
}

#[test]
fn a_session_runs_in_its_worktree_and_close_removes_it() {
    let scratch = Scratch::new();
    let repo = scratch.repo("demo");
    let core = core_with_fake_cli(&scratch);
    let added = core.add_project(&repo.join(".git").join("..")).unwrap();
    let catalog = core.cli_catalog();
    assert!(catalog.presets[0].found());
    assert!(!catalog.presets[1].found());

    let (sink, rx) = channel_sink();
    let session = core
        .create_session(
            &added.project.id,
            "claude",
            PtySize::new(30, 90),
            sink,
            LaunchOptions::default(),
        )
        .unwrap();
    assert_eq!(session.title, "New Claude Code");
    assert_eq!(session.branch, format!("shika-draft-{}", session.id));
    assert_eq!(
        session.worktree,
        repo.join(".worktrees").join(&session.branch)
    );
    assert_eq!(core.sessions(), std::slice::from_ref(&session));
    assert_eq!(core.worktree_journal().unwrap().len(), 1);
    let exclude = fs::read_to_string(repo.join(".git/info/exclude")).unwrap();
    assert!(exclude.contains(".worktrees/"), "{exclude}");
    assert!(!repo.join(".gitignore").exists());

    let got = collect_until(&rx, "cwd:", Duration::from_secs(5));
    let got = got + &collect_until(&rx, "\n", Duration::from_secs(2));
    assert!(got.contains("args:--dangerously-skip-permissions"), "{got}");
    assert!(
        got.contains(&format!("cwd:{}", session.worktree.display())),
        "{got}"
    );
    core.write(session.pty.unwrap(), b"hello\r").unwrap();
    let echoed = collect_until(&rx, "got:hello", Duration::from_secs(3));
    assert!(echoed.contains("got:hello"), "{echoed}");

    let (shell_sink, _shell_rx) = channel_sink();
    let shell = core
        .open_shell(&session.id, PtySize::default(), shell_sink)
        .unwrap();
    assert_eq!(core.sessions.get(&session.id).unwrap().shell_ptys, [shell]);
    let (again_sink, _again_rx) = channel_sink();
    let again = core
        .open_shell(&session.id, PtySize::default(), again_sink)
        .unwrap();
    assert_ne!(again, shell);
    assert!(core.close_shell(&session.id, session.pty.unwrap()).is_err());
    core.close_shell(&session.id, shell).unwrap();
    assert_eq!(core.sessions.get(&session.id).unwrap().shell_ptys, [again]);
    assert!(core.write(shell, b"exit\r").is_err());
    core.write(again, b"echo independent\r").unwrap();

    assert!(!core.session_dirty(&session.id).unwrap());
    fs::write(session.worktree.join("wip.txt"), "wip\n").unwrap();
    assert!(core.session_dirty(&session.id).unwrap());
    assert!(matches!(
        core.close_session(&session.id, false),
        Err(Error::WorktreeHasChanges(_))
    ));
    assert!(session.worktree.exists());
    assert!(
        core.write(session.pty.unwrap(), b"").is_ok(),
        "a refused close kept the agent"
    );

    core.close_session(&session.id, true).unwrap();
    assert!(!session.worktree.exists());
    assert!(core.sessions().is_empty());
    assert!(core.worktree_journal().unwrap().is_empty());
    assert!(
        git(&repo, &["branch", "--list", &session.branch])
            .trim()
            .is_empty()
    );
    assert_eq!(
        core.write(session.pty.unwrap(), b"x"),
        Err(Error::UnknownPty)
    );
    assert_eq!(core.write(shell, b"x"), Err(Error::UnknownPty));
    assert_eq!(core.write(again, b"x"), Err(Error::UnknownPty));
    let (_, exit) = collect_to_exit(&rx, Duration::from_secs(5));
    assert!(exit.is_some(), "the agent was not hung up");
    assert_eq!(
        core.close_session(&session.id, true),
        Err(Error::UnknownSession)
    );
}

#[test]
fn a_missing_cli_or_project_creates_nothing() {
    let scratch = Scratch::new();
    let repo = scratch.repo("demo");
    let core = core_with_fake_cli(&scratch);
    let added = core.add_project(&repo).unwrap();

    let (sink, _rx) = channel_sink();
    assert_eq!(
        core.create_session(
            &added.project.id,
            "cursor",
            PtySize::default(),
            sink,
            LaunchOptions::default()
        )
        .unwrap_err(),
        Error::CliNotFound("Cursor CLI".into())
    );
    let (sink, _rx) = channel_sink();
    assert_eq!(
        core.create_session(
            &added.project.id,
            "kiro",
            PtySize::default(),
            sink,
            LaunchOptions::default()
        )
        .unwrap_err(),
        Error::UnknownCli
    );
    let (sink, _rx) = channel_sink();
    assert_eq!(
        core.create_session(
            "missing",
            "claude",
            PtySize::default(),
            sink,
            LaunchOptions::default()
        )
        .unwrap_err(),
        Error::UnknownProject
    );
    assert!(!repo.join(".worktrees").exists());
    assert!(core.sessions().is_empty());
}

#[test]
fn removing_a_project_hangs_up_its_sessions_and_keeps_the_worktree() {
    let scratch = Scratch::new();
    let repo = scratch.repo("demo");
    let core = core_with_fake_cli(&scratch);
    let added = core.add_project(&repo).unwrap();
    let (sink, rx) = channel_sink();
    let session = core
        .create_session(
            &added.project.id,
            "claude",
            PtySize::default(),
            sink,
            LaunchOptions::default(),
        )
        .unwrap();

    let gone = core.remove_project(&added.project.id).unwrap();

    assert_eq!(gone, std::slice::from_ref(&session));
    assert!(core.sessions().is_empty());
    assert!(core.projects().unwrap().is_empty());
    assert!(session.worktree.exists());
    assert_eq!(core.worktree_journal().unwrap().len(), 1);
    let (_, exit) = collect_to_exit(&rx, Duration::from_secs(5));
    assert!(exit.is_some(), "the agent was not hung up");
}
