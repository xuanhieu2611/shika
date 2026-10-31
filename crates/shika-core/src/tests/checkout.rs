use super::*;

/// The switch CLIs, plus a git that logs every call to the returned file
/// before running the real one, so a test can prove no git ran.
fn core_with_logged_git(scratch: &Scratch) -> (Core, PathBuf) {
    let log = scratch.path.join("git.log");
    let wrapper = write_exec(
        &scratch.path,
        "git",
        &format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nexec /usr/bin/git \"$@\"\n",
            log.display()
        ),
    );
    let lookup = |name: &str| Some(scratch.path.join("bin").join(name));
    let env = PathEnv::from_lookup(
        "/usr/bin:/bin".into(),
        None,
        &[
            ("claude", lookup("claude")),
            ("agent", None),
            ("pi", None),
            ("codex", None),
            ("git", Some(wrapper)),
        ],
    );
    write_exec(&scratch.path, "claude", SWITCH_CLI);
    let core = Core::open_with(scratch.path.join("data"), env).unwrap();
    (core, log)
}

fn git_log(log: &Path) -> String {
    fs::read_to_string(log).unwrap_or_default()
}

fn start_checkout(core: &Core, project_id: &str, preset: &str) -> (Session, String) {
    let (sink, rx) = channel_sink();
    let session = core
        .create_checkout_session(project_id, preset, PtySize::new(30, 90), sink)
        .unwrap();
    let report = collect_until(&rx, "end-report", Duration::from_secs(5));
    (session, report)
}

#[test]
fn a_checkout_session_runs_in_the_repository_root_with_no_worktree_branch_or_journal() {
    let scratch = Scratch::new();
    let repo = scratch.repo("demo");
    git(&repo, &["checkout", "-b", "dev"]);
    fs::write(repo.join("wip.txt"), "the author's edit\n").unwrap();
    let core = core_with_switch_clis(&scratch);
    let project = core.add_project(&repo).unwrap().project;
    let before = git_snapshot(&repo);

    let (session, report) = start_checkout(&core, &project.id, "codex");
    assert_eq!(session.kind, SessionKind::Checkout);
    assert!(session.is_checkout() && !session.is_lead());
    assert_eq!(session.worktree, repo);
    assert_eq!(session.repo, repo);
    assert_eq!(session.branch, "");
    assert_eq!(session.base_ref, None);
    assert_eq!(session.started_by, None);
    assert_eq!(session.title, "New Codex");
    assert!(!session.named());
    let args = report_args(&report);
    let trust = format!(
        r#"projects={{"{}"={{trust_level="trusted"}}}}"#,
        repo.display()
    );
    assert_eq!(
        args,
        [
            "--dangerously-bypass-approvals-and-sandbox",
            "-c",
            trust.as_str()
        ],
        "the preset's flags, the trust override on the root, and no prompt"
    );
    assert!(
        report.contains(&format!("cwd:{}", repo.display())),
        "{report}"
    );
    assert!(
        report.lines().any(|line| line.trim() == "sock:")
            && report.lines().any(|line| line.trim() == "token:"),
        "{report}"
    );

    assert!(core.worktree_journal().unwrap().is_empty());
    assert!(core.leftovers_list().unwrap().is_empty());
    assert!(!repo.join(".worktrees").exists());
    assert_eq!(git_snapshot(&repo), before, "starting wrote nothing to git");
    assert_eq!(core.checkout_session(&project.id), Some(session.clone()));
    assert_eq!(
        core.checkout_head(&project.id).unwrap(),
        CheckoutHead::Branch("dev".into())
    );

    let (shell_sink, shell_rx) = channel_sink();
    let shell = core
        .open_shell(&session.id, PtySize::default(), shell_sink)
        .unwrap();
    core.write(shell, b"pwd -P\r").unwrap();
    let printed = collect_until(
        &shell_rx,
        &repo.display().to_string(),
        Duration::from_secs(5),
    );
    assert!(printed.contains(&repo.display().to_string()), "{printed}");
    core.session_close(&session.id, false).unwrap();
    assert_eq!(core.write(shell, b"x"), Err(Error::UnknownPty));
    assert!(repo.join("wip.txt").exists());
}

#[test]
fn a_project_has_at_most_one_checkout_session() {
    let scratch = Scratch::new();
    let core = core_with_switch_clis(&scratch);
    let project = core.add_project(&scratch.repo("demo")).unwrap().project;
    let other = core.add_project(&scratch.repo("other")).unwrap().project;
    let (first, _) = start_checkout(&core, &project.id, "claude");
    let (sink, _rx) = channel_sink();
    assert_eq!(
        core.create_checkout_session(&project.id, "codex", PtySize::default(), sink),
        Err(Error::CheckoutExists)
    );
    assert_eq!(core.sessions(), std::slice::from_ref(&first));
    // Another project, and worktree tasks in this one, are unaffected.
    let (elsewhere, _) = start_checkout(&core, &other.id, "claude");
    let task = create_fake_session(&core, &project.id);
    assert_eq!(task.kind, SessionKind::Task);
    assert_eq!(core.checkout_session(&project.id).unwrap().id, first.id);
    assert_eq!(core.checkout_session(&other.id).unwrap().id, elsewhere.id);
    // Once it has stopped, the project can start one again.
    core.session_stop_checkout(&first.id).unwrap();
    assert_eq!(core.checkout_session(&project.id), None);
    let (again, _) = start_checkout(&core, &project.id, "pi");
    assert_ne!(again.id, first.id);

    // A refused start creates nothing.
    let (sink, _rx) = channel_sink();
    assert_eq!(
        core.create_checkout_session(&project.id, "kiro", PtySize::default(), sink),
        Err(Error::UnknownCli)
    );
    let (sink, _rx) = channel_sink();
    assert_eq!(
        core.create_checkout_session("missing", "claude", PtySize::default(), sink),
        Err(Error::UnknownProject)
    );
    // `elsewhere`, `task`, and `again`.
    assert_eq!(core.sessions().len(), 3);
}

#[test]
fn a_folder_that_is_not_its_own_checkout_starts_nothing() {
    let scratch = Scratch::new();
    let repo = scratch.repo("demo");
    let core = core_with_switch_clis(&scratch);
    let project = core.add_project(&repo).unwrap().project;
    fs::rename(repo.join(".git"), repo.join("moved.git")).unwrap();
    let (sink, _rx) = channel_sink();
    assert!(matches!(
        core.create_checkout_session(&project.id, "claude", PtySize::default(), sink),
        Err(Error::GitStatus(_))
    ));
    assert!(core.sessions().is_empty());
}

#[test]
fn closing_a_dirty_checkout_runs_no_git_and_never_discards() {
    let scratch = Scratch::new();
    let repo = scratch.repo("demo");
    fs::write(repo.join("tracked.txt"), "one\n").unwrap();
    git(&repo, &["add", "tracked.txt"]);
    git(&repo, &["commit", "-m", "tracked"]);
    git(&repo, &["checkout", "-b", "dev"]);
    git(&repo, &["commit", "--allow-empty", "-m", "unpushed"]);
    fs::write(repo.join("tracked.txt"), "one\nedited\n").unwrap();
    fs::write(repo.join("staged.txt"), "staged\n").unwrap();
    git(&repo, &["add", "staged.txt"]);
    fs::write(repo.join("untracked.txt"), "new\n").unwrap();
    let (core, log) = core_with_logged_git(&scratch);
    let project = core.add_project(&repo).unwrap().project;
    let before = git_snapshot(&repo);

    let (session, _) = start_checkout(&core, &project.id, "claude");
    let pty = session.pty.unwrap();
    for refused in [
        core.session_discard(&session.id),
        core.session_push_and_close(&session.id),
        core.close_session(&session.id, true),
        core.session_switched_close_check(&session.id).map(|_| ()),
        core.session_git_state(&session.id, false).map(|_| ()),
        core.session_publish_preview(&session.id).map(|_| ()),
        core.session_publish_now(&session.id, None, None)
            .map(|_| ()),
        core.session_pr_checks(&session.id, "o/r", 1).map(|_| ()),
    ] {
        assert_eq!(refused, Err(Error::CheckoutUnsupported));
    }
    fs::remove_file(&log).ok();

    // Working: the app must ask first, and the agent keeps running.
    assert_eq!(
        core.session_close(&session.id, true),
        Err(Error::CloseNeedsConfirmation)
    );
    assert!(core.write(pty, b"").is_ok());
    // Idle: it stops and goes, with no git at all.
    core.session_close(&session.id, false).unwrap();
    assert_eq!(git_log(&log), "", "close ran git");
    assert!(core.sessions().is_empty());
    assert_eq!(core.write(pty, b"x"), Err(Error::UnknownPty));
    assert_eq!(
        git_snapshot(&repo),
        before,
        "the author's work is as it was"
    );
    assert_eq!(
        fs::read_to_string(repo.join("tracked.txt")).unwrap(),
        "one\nedited\n"
    );
    assert!(repo.join("untracked.txt").exists());

    // The confirmed close after "Stop the agent?" is the same: no git.
    let (session, _) = start_checkout(&core, &project.id, "claude");
    fs::remove_file(&log).ok();
    assert_eq!(
        core.session_close(&session.id, true),
        Err(Error::CloseNeedsConfirmation)
    );
    core.session_stop_checkout(&session.id).unwrap();
    assert_eq!(git_log(&log), "");
    assert!(core.sessions().is_empty());
    assert_eq!(git_snapshot(&repo), before);
    assert_eq!(
        core.session_stop_checkout(&session.id),
        Err(Error::UnknownSession)
    );

    // Stopping is never a way around a task's close checks.
    let task = create_fake_session(&core, &project.id);
    assert_eq!(
        core.session_stop_checkout(&task.id),
        Err(Error::NotCheckout)
    );
    assert!(core.session(&task.id).is_some());
    assert!(task.worktree.exists());
}

#[test]
fn a_checkout_session_is_named_without_renaming_the_checked_out_branch() {
    let scratch = Scratch::new();
    let repo = scratch.repo("shika");
    git(&repo, &["checkout", "-b", "dev"]);
    let mut core = core_with_switch_clis(&scratch);
    let home = scratch.path.join("home");
    core.cli_home = Some(CliHome::at(home.clone()));
    let project = core.add_project(&repo).unwrap().project;
    let conversations = CliHome::at(home.clone()).claude_project_dir(&repo);
    fs::create_dir_all(&conversations).unwrap();
    let title = |name: &str| serde_json::json!({ "type": "ai-title", "aiTitle": name }).to_string();
    // The author's own conversation in this folder, from before.
    fs::write(
        conversations.join("authors-own.jsonl"),
        title("The author's own chat"),
    )
    .unwrap();
    std::thread::sleep(Duration::from_millis(20));
    let branches = git(&repo, &["for-each-ref", "refs/heads"]);

    let (session, _) = start_checkout(&core, &project.id, "claude");
    assert_eq!(
        core.session_apply_cli_title(&session.id).unwrap(),
        None,
        "a conversation from before the session never names it"
    );
    let named = core
        .session_rename_from_prompt(&session.id, "Where is the notification code?\nand more")
        .unwrap();
    assert_eq!(named.title, "Where is the notification code?");
    assert_eq!(named.branch, "");
    assert!(named.prompt_named && named.named());
    assert_eq!(
        core.session_rename_from_prompt(&session.id, "a second prompt")
            .unwrap(),
        named
    );

    fs::write(
        conversations.join("ours.jsonl"),
        title("Notification code location"),
    )
    .unwrap();
    let titled = core.session_apply_cli_title(&session.id).unwrap().unwrap();
    assert_eq!(titled.title, "Notification code location");
    assert_eq!(titled.branch, "");
    assert!(titled.cli_titled);
    assert_eq!(core.session_apply_cli_title(&session.id).unwrap(), None);
    let renamed = core.session_set_title(&session.id, "Mine").unwrap();
    assert_eq!(renamed.title, "Mine");
    assert_eq!(core.session_refresh_branch(&session.id).unwrap(), renamed);
    assert_eq!(core.session_pushed_head(&session.id).unwrap(), None);

    assert_eq!(git(&repo, &["for-each-ref", "refs/heads"]), branches);
    assert_eq!(git(&repo, &["branch", "--show-current"]).trim(), "dev");
    assert!(core.worktree_journal().unwrap().is_empty());
}

#[test]
fn a_checkout_diff_counts_only_uncommitted_work_against_head() {
    let scratch = Scratch::new();
    let repo = scratch.repo("demo");
    fs::write(repo.join("a.txt"), "one\ntwo\n").unwrap();
    git(&repo, &["add", "a.txt"]);
    git(&repo, &["commit", "-m", "base"]);
    git(&repo, &["checkout", "-b", "dev"]);
    fs::write(repo.join("a.txt"), "one\ntwo\nthree\n").unwrap();
    git(&repo, &["commit", "-am", "the author's unpushed commit"]);
    let core = core_with_switch_clis(&scratch);
    let project = core.add_project(&repo).unwrap().project;
    let (session, _) = start_checkout(&core, &project.id, "claude");
    assert_eq!(
        core.session_diff_stat(&session.id).unwrap(),
        DiffStat::default()
    );
    assert!(core.session_diff(&session.id).unwrap().files.is_empty());

    // A task's files are not the checkout's.
    let task = create_fake_session(&core, &project.id);
    fs::write(task.worktree.join("task.txt"), "task\n").unwrap();
    assert_eq!(
        core.session_diff_stat(&session.id).unwrap(),
        DiffStat::default()
    );

    fs::write(repo.join("a.txt"), "one\n").unwrap();
    fs::write(repo.join("new.txt"), "x\n").unwrap();
    let stat = core.session_diff_stat(&session.id).unwrap();
    assert_eq!(
        stat,
        DiffStat {
            files: 2,
            insertions: 1,
            deletions: 2
        }
    );
    let diff = core.session_diff(&session.id).unwrap();
    assert_eq!(diff.files.len(), 2);
    let tracked = diff
        .files
        .iter()
        .find(|file| file.status != FileStatus::Untracked)
        .unwrap();
    let again = core
        .session_file_diff(&session.id, &tracked.key)
        .unwrap()
        .unwrap();
    assert_eq!(again.key, tracked.key);
    assert_eq!(
        git(&repo, &["status", "--porcelain"]),
        " M a.txt\n?? new.txt\n",
        "reading staged nothing"
    );

    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-m", "the author commits"]);
    assert_eq!(
        core.session_diff_stat(&session.id).unwrap(),
        DiffStat::default()
    );
}

#[test]
fn a_checkout_session_switches_agent_in_place_and_stays_a_checkout() {
    let scratch = Scratch::new();
    let repo = scratch.repo("demo");
    let core = core_with_switch_clis(&scratch);
    let project = core.add_project(&repo).unwrap().project;
    let (session, _) = start_checkout(&core, &project.id, "claude");
    let (switched, report) = switch_agent(&core, &session.id, "pi", None, None);
    assert_eq!(switched.kind, SessionKind::Checkout);
    assert_eq!(switched.worktree, repo);
    assert_eq!(switched.branch, "");
    assert_eq!(switched.title, "New Pi");
    assert!(
        !switched.cli_titled,
        "an unnamed checkout keeps naming open"
    );
    assert!(switched.agent_started >= session.agent_started);
    assert!(
        report.contains(&format!("cwd:{}", repo.display())),
        "{report}"
    );
    assert_eq!(
        core.write(session.pty.unwrap(), b"x"),
        Err(Error::UnknownPty)
    );
    core.session_rename_from_prompt(&session.id, "Find the release docs")
        .unwrap();
    let (named, _) = switch_agent(&core, &session.id, "claude", Some("Continue"), None);
    assert!(named.cli_titled, "a named checkout keeps its name");
    assert_eq!(named.title, "Find the release docs");
    assert_eq!(core.checkout_session(&project.id).unwrap().id, session.id);
}

#[test]
fn a_checkout_folder_is_never_a_usage_source() {
    let scratch = Scratch::new();
    let repo = scratch.repo("demo");
    let mut core = core_with_switch_clis(&scratch);
    let home = CliHome::at(scratch.path.join("usage-home"));
    core.cli_home = Some(home.clone());
    core.start_usage_collection();
    let project = core.add_project(&repo).unwrap().project;
    // The author's own Claude Code usage in this folder, outside Shika.
    let conversations = home.claude_project_dir(&repo);
    fs::create_dir_all(&conversations).unwrap();
    fs::write(
        conversations.join("authors-own.jsonl"),
        "{\"type\":\"assistant\",\"message\":{\"id\":\"m\",\"usage\":{\"input_tokens\":50,\"output_tokens\":50}}}\n",
    )
    .unwrap();
    let (session, _) = start_checkout(&core, &project.id, "claude");
    core.record_token_usage("claude", &session.worktree);
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while !core.usage_collection_idle() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(core.usage_collection_idle());
    assert_eq!(
        fs::read_dir(core.data_dir().join("usage-sources"))
            .map(|dir| dir.count())
            .unwrap_or(0),
        0
    );
    assert_eq!(core.token_usage().claude, 0);
}
