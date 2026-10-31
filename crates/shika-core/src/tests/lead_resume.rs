use super::*;

/// A saved Claude conversation file, as Claude Code would have left it.
fn claude_transcript(scratch: &Scratch, id: &str) {
    let dir = scratch.path.join("home/.claude/projects/-some-project");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join(format!("{id}.jsonl")), "{}\n").unwrap();
}

/// The id after `--session-id` or `--resume` in a launch report.
fn conversation_arg(report: &str, flag: &str) -> String {
    let args = report_args(report);
    let at = args.iter().position(|arg| arg == flag).unwrap();
    args[at + 1].clone()
}

#[test]
fn restarting_a_lead_refreshes_its_clean_worktree_and_resumes_claude() {
    let scratch = Scratch::new();
    let repo = scratch.repo("demo");
    let core = core_with_reporting_cli(&scratch);
    let project = core.add_project(&repo).unwrap().project;
    let (first, report) = start_lead_with(&core, &scratch, &project.id, "claude");
    let id = conversation_arg(&report, "--session-id");
    let first_head = git(&repo, &["rev-parse", "main"]).trim().to_string();

    // Stopping keeps the tree, its journal entry, and the record.
    core.stop_lead(&first.session.id).unwrap();
    assert!(core.sessions().is_empty());
    assert!(core.lead_for_project(&project.id).is_none());
    assert!(first.session.worktree.exists());
    assert_eq!(core.worktree_journal().unwrap().len(), 1);
    assert!(core.leftovers_list().unwrap().is_empty());
    assert_eq!(
        core.saved_lead(&project.id).unwrap().unwrap().session_id,
        Some(id.clone())
    );

    git(&repo, &["commit", "--allow-empty", "-m", "second"]);
    let second_head = git(&repo, &["rev-parse", "main"]).trim().to_string();
    claude_transcript(&scratch, &id);
    let (again, report) = start_lead_with(&core, &scratch, &project.id, "claude");

    assert_eq!(again.conversation, LeadConversation::Resumed);
    assert_eq!(
        again.worktree,
        LeadWorktree::Refreshed {
            from: first_head[..10].to_string(),
            to: second_head[..10].to_string(),
        }
    );
    assert_eq!(again.session.worktree, first.session.worktree);
    assert_ne!(again.session.id, first.session.id);
    assert_eq!(
        git(&again.session.worktree, &["rev-parse", "HEAD"]).trim(),
        second_head
    );
    // The instructions go on every start, the id after `--resume`, and
    // there is still no positional prompt.
    assert_eq!(
        report_args(&report),
        [
            "--dangerously-skip-permissions",
            "--resume",
            &id,
            "--append-system-prompt",
            LEAD_INSTRUCTIONS
        ]
    );
    assert_eq!(
        core.saved_lead(&project.id).unwrap(),
        Some(SavedLead {
            cli: "claude".into(),
            session_id: Some(id),
            reported_workers: Vec::new(),
            chat_history: None,
        })
    );
    assert_eq!(core.worktree_journal().unwrap().len(), 1);
    assert!(again.started_at <= SystemTime::now());
}

#[test]
fn a_stored_conversation_that_is_gone_or_from_another_cli_starts_fresh_and_says_why() {
    let scratch = Scratch::new();
    let repo = scratch.repo("demo");
    let core = core_with_reporting_cli(&scratch);
    let project = core.add_project(&repo).unwrap().project;
    let (first, report) = start_lead_with(&core, &scratch, &project.id, "claude");
    let old_id = conversation_arg(&report, "--session-id");
    core.stop_lead(&first.session.id).unwrap();

    // No transcript exists, so `--resume` would fail: new id instead.
    let (second, report) = start_lead_with(&core, &scratch, &project.id, "claude");
    assert_eq!(
        second.conversation,
        LeadConversation::Fresh(FreshReason::SessionMissing)
    );
    assert_eq!(second.worktree, LeadWorktree::Unchanged);
    let new_id = conversation_arg(&report, "--session-id");
    assert_ne!(new_id, old_id);
    assert_eq!(
        core.saved_lead(&project.id).unwrap().unwrap().session_id,
        Some(new_id.clone())
    );
    core.stop_lead(&second.session.id).unwrap();

    // The saved conversation belongs to Claude Code, not Pi.
    claude_transcript(&scratch, &new_id);
    let (third, _) = start_lead_with(&core, &scratch, &project.id, "pi");
    assert_eq!(
        third.conversation,
        LeadConversation::Fresh(FreshReason::CliChanged)
    );
    assert_eq!(core.saved_lead(&project.id).unwrap().unwrap().cli, "pi");
    core.stop_lead(&third.session.id).unwrap();

    // An id edited into something that could read as a flag is ignored.
    core.projects
        .set_lead(
            &project.id,
            Some(SavedLead {
                cli: "claude".into(),
                session_id: Some("--dangerously-bad".into()),
                reported_workers: Vec::new(),
                chat_history: None,
            }),
        )
        .unwrap();
    let (fourth, report) = start_lead_with(&core, &scratch, &project.id, "claude");
    assert_eq!(
        fourth.conversation,
        LeadConversation::Fresh(FreshReason::NoSavedSession)
    );
    assert!(!report_args(&report).contains(&"--dangerously-bad".to_string()));
}

#[test]
fn pi_resumes_with_the_same_session_id_flag() {
    let scratch = Scratch::new();
    let repo = scratch.repo("demo");
    let core = core_with_reporting_cli(&scratch);
    let project = core.add_project(&repo).unwrap().project;
    let (first, report) = start_lead_with(&core, &scratch, &project.id, "pi");
    let args = report_args(&report);
    let at = args.iter().position(|arg| arg == "--session-id").unwrap();
    let id = args[at + 1].clone();
    assert_eq!(args[at + 2], "--append-system-prompt");
    assert_eq!(args[at + 3], LEAD_INSTRUCTIONS);
    assert_eq!(args[0], "--approve");
    core.stop_lead(&first.session.id).unwrap();

    // Pi's own folder name for the Lead's cwd: `--` + path with `/` as `-`.
    let cwd = first.session.worktree.canonicalize().unwrap();
    let folder = format!(
        "--{}--",
        cwd.to_string_lossy()
            .trim_start_matches('/')
            .replace('/', "-")
    );
    let dir = scratch.path.join("home/.pi/agent/sessions").join(folder);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join(format!("2026-10-15T18-05-14-237Z_{id}.jsonl")),
        "{}\n",
    )
    .unwrap();

    let (again, report) = start_lead_with(&core, &scratch, &project.id, "pi");
    assert_eq!(again.conversation, LeadConversation::Resumed);
    assert_eq!(conversation_arg(&report, "--session-id"), id);
}

#[test]
fn codex_has_no_id_until_discovered_then_resumes_with_the_resume_subcommand() {
    let scratch = Scratch::new();
    let repo = scratch.repo("demo");
    let core = core_with_reporting_cli(&scratch);
    let project = core.add_project(&repo).unwrap().project;
    let (first, report) = start_lead_with(&core, &scratch, &project.id, "codex");
    let args = report_args(&report);
    assert_eq!(args[0], "--dangerously-bypass-approvals-and-sandbox");
    assert!(
        args.iter().any(|arg| arg.starts_with("projects={")),
        "{args:?}"
    );
    assert!(
        args.last()
            .unwrap()
            .starts_with("developer_instructions=\"You lead"),
        "{args:?}"
    );
    assert!(!args.contains(&"resume".to_string()));
    assert_eq!(
        core.saved_lead(&project.id).unwrap(),
        Some(SavedLead {
            cli: "codex".into(),
            session_id: None,
            reported_workers: Vec::new(),
            chat_history: None,
        })
    );
    // Nothing to find until Codex writes the conversation at the first message.
    assert_eq!(
        core.discover_lead_session(&first.session.id, first.started_at),
        Ok(None)
    );
    let launched_ms = first
        .started_at
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    let db = scratch.path.join("home/.codex/state_5.sqlite");
    fs::create_dir_all(db.parent().unwrap()).unwrap();
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute_batch(
        "CREATE TABLE threads (id TEXT PRIMARY KEY, cwd TEXT NOT NULL, created_at INTEGER NOT NULL,
             created_at_ms INTEGER, archived INTEGER NOT NULL DEFAULT 0, agent_role TEXT);",
    )
    .unwrap();
    let cwd = first.session.worktree.canonicalize().unwrap();
    let cwd = cwd.to_string_lossy().into_owned();
    for (id, created) in [
        ("older-run", launched_ms - 60_000),
        ("this-run", launched_ms + 500),
    ] {
        conn.execute(
            "INSERT INTO threads (id, cwd, created_at, created_at_ms) VALUES (?1, ?2, ?3, ?4)",
            (id, &cwd, created / 1000, created),
        )
        .unwrap();
    }
    drop(conn);
    assert_eq!(
        core.discover_lead_session(&first.session.id, first.started_at),
        Ok(Some("this-run".to_string()))
    );
    assert_eq!(
        core.saved_lead(&project.id)
            .unwrap()
            .unwrap()
            .session_id
            .as_deref(),
        Some("this-run")
    );
    // Once saved, the answer is the saved id.
    assert_eq!(
        core.discover_lead_session(&first.session.id, SystemTime::now()),
        Ok(Some("this-run".to_string()))
    );
    core.stop_lead(&first.session.id).unwrap();

    let (again, report) = start_lead_with(&core, &scratch, &project.id, "codex");
    assert_eq!(again.conversation, LeadConversation::Resumed);
    let args = report_args(&report);
    assert_eq!(args[0], "resume");
    assert_eq!(args[1], "--dangerously-bypass-approvals-and-sandbox");
    assert_eq!(args.last().unwrap(), "this-run");
    assert!(
        args[args.len() - 2].starts_with("developer_instructions=\"You lead"),
        "{args:?}"
    );
}

#[test]
fn discovery_ignores_stale_leads_and_clis_whose_id_shika_chose() {
    let scratch = Scratch::new();
    let repo = scratch.repo("demo");
    let core = core_with_reporting_cli(&scratch);
    let project = core.add_project(&repo).unwrap().project;
    assert_eq!(core.discover_lead_session("missing", UNIX_EPOCH), Ok(None));
    let (start, report) = start_lead_with(&core, &scratch, &project.id, "claude");
    let id = conversation_arg(&report, "--session-id");
    assert_eq!(
        core.discover_lead_session(&start.session.id, start.started_at),
        Ok(Some(id))
    );
    core.set_lead_session(&project.id, None).unwrap();
    assert_eq!(
        core.discover_lead_session(&start.session.id, start.started_at),
        Ok(None)
    );
    assert!(matches!(
        core.set_lead_session(&project.id, Some("--x")),
        Err(Error::InvalidPrompt(_))
    ));
    core.set_lead_session(&project.id, Some("abc-1")).unwrap();
    assert_eq!(
        core.saved_lead(&project.id)
            .unwrap()
            .unwrap()
            .session_id
            .as_deref(),
        Some("abc-1")
    );
    assert_eq!(
        core.set_lead_session("missing", None),
        Err(Error::UnknownProject)
    );
}

/// An `agent` stand-in: `create-chat` runs `create`, anything else is a
/// launch that prints its arguments like `SWITCH_CLI`.
fn cursor_cli(scratch: &Scratch, create: &str) {
    let launch = SWITCH_CLI.trim_start_matches("#!/bin/sh\n");
    write_exec(
        &scratch.path,
        "agent",
        &format!("#!/bin/sh\nif [ \"$1\" = create-chat ]; then\n{create}\nfi\n{launch}"),
    );
}

#[test]
fn cursor_gets_a_new_chat_id_and_a_bootstrap_prompt_only_when_fresh() {
    let scratch = Scratch::new();
    let repo = scratch.repo("demo");
    let core = core_with_reporting_cli(&scratch);
    let project = core.add_project(&repo).unwrap().project;
    let chat = "5c0ffee0-0000-4000-8000-000000000001";
    cursor_cli(&scratch, &format!("echo {chat}\nexit 0"));

    let (first, report) = start_lead_with(&core, &scratch, &project.id, "cursor");
    assert!(first.bootstrap_sent);
    assert_eq!(
        report_args(&report),
        [
            "--yolo",
            "--trust",
            "--sandbox",
            "disabled",
            "--resume",
            chat,
            LEAD_BOOTSTRAP
        ]
    );
    assert_eq!(
        core.saved_lead(&project.id).unwrap(),
        Some(SavedLead {
            cli: "cursor".into(),
            session_id: Some(chat.into()),
            reported_workers: Vec::new(),
            chat_history: None,
        })
    );
    core.stop_lead(&first.session.id).unwrap();

    // Cursor keeps nothing Shika can read, so the chat is assumed to
    // exist. A resume carries no positional prompt.
    let (again, report) = start_lead_with(&core, &scratch, &project.id, "cursor");
    assert_eq!(again.conversation, LeadConversation::Resumed);
    assert!(!again.bootstrap_sent);
    assert_eq!(
        report_args(&report),
        [
            "--yolo",
            "--trust",
            "--sandbox",
            "disabled",
            "--resume",
            chat
        ]
    );
}

#[test]
fn a_failed_create_chat_launches_cursor_with_the_bootstrap_and_no_id() {
    for create in ["echo boom >&2\nexit 1", "echo --resume\nexit 0", "exit 0"] {
        let scratch = Scratch::new();
        let repo = scratch.repo("demo");
        let core = core_with_reporting_cli(&scratch);
        let project = core.add_project(&repo).unwrap().project;
        cursor_cli(&scratch, create);
        let (first, report) = start_lead_with(&core, &scratch, &project.id, "cursor");
        assert_eq!(
            report_args(&report),
            ["--yolo", "--trust", "--sandbox", "disabled", LEAD_BOOTSTRAP],
            "{create}"
        );
        assert_eq!(
            core.saved_lead(&project.id).unwrap(),
            Some(SavedLead {
                cli: "cursor".into(),
                session_id: None,
                reported_workers: Vec::new(),
                chat_history: None,
            })
        );
        core.stop_lead(&first.session.id).unwrap();
        let (again, _) = start_lead_with(&core, &scratch, &project.id, "cursor");
        assert_eq!(
            again.conversation,
            LeadConversation::Fresh(FreshReason::NoSavedSession)
        );
    }
}

#[test]
fn a_dirty_lead_worktree_is_kept_and_reported_on_restart() {
    let scratch = Scratch::new();
    let repo = scratch.repo("demo");
    let core = core_with_reporting_cli(&scratch);
    let project = core.add_project(&repo).unwrap().project;
    let (first, _) = start_lead_with(&core, &scratch, &project.id, "claude");
    core.stop_lead(&first.session.id).unwrap();
    assert_eq!(core.lead_has_changes(&project.id), Ok(false));
    fs::write(first.session.worktree.join("notes.txt"), "mine\n").unwrap();
    assert_eq!(core.lead_has_changes(&project.id), Ok(true));
    git(&repo, &["commit", "--allow-empty", "-m", "second"]);

    let (again, _) = start_lead_with(&core, &scratch, &project.id, "claude");
    assert_eq!(again.worktree, LeadWorktree::KeptDirty);
    assert!(again.session.worktree.join("notes.txt").exists());
    assert_ne!(
        git(&again.session.worktree, &["rev-parse", "HEAD"]),
        git(&repo, &["rev-parse", "main"])
    );
}
