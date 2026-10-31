use super::*;

#[test]
fn prompt_rename_is_unique_and_keeps_folder_and_journal_consistent() {
    let scratch = Scratch::new();
    let repo = scratch.repo("demo");
    let core = core_with_fake_cli(&scratch);
    let project = core.add_project(&repo).unwrap().project;
    let first = create_fake_session(&core, &project.id);
    let second = create_fake_session(&core, &project.id);
    let one = core
        .session_rename_from_prompt(&first.id, "Fix Login / Flow!")
        .unwrap();
    let two = core
        .session_rename_from_prompt(&second.id, "Fix Login / Flow!")
        .unwrap();
    assert_eq!(one.branch, "fix-login-flow");
    assert_eq!(two.branch, "fix-login-flow-2");
    assert_eq!(one.title, "Fix Login / Flow!");
    assert_eq!(one.worktree, first.worktree);
    assert_eq!(
        git(&one.worktree, &["branch", "--show-current"]).trim(),
        one.branch
    );
    let journal = core.worktree_journal().unwrap();
    assert_eq!(journal[0].branch, one.branch);
    assert_eq!(journal[0].path, first.worktree);
    assert_eq!(
        core.session_rename_from_prompt(&first.id, "second prompt")
            .unwrap(),
        one
    );
    core.session_discard(&first.id).unwrap();
    assert!(core.session(&second.id).is_some());
    core.write(second.pty.unwrap(), b"still alive\r").unwrap();
    core.session_discard(&second.id).unwrap();
}

fn write_claude_title(home: &Path, worktree: &Path, title: &str) {
    let encoded: String = worktree
        .to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let dir = home.join(".claude/projects").join(encoded);
    fs::create_dir_all(&dir).unwrap();
    let line = serde_json::json!({ "type": "ai-title", "aiTitle": title });
    fs::write(dir.join("session.jsonl"), format!("{line}\n")).unwrap();
}

#[test]
fn manual_task_names_leave_git_and_ownership_unchanged() {
    let scratch = Scratch::new();
    let repo = scratch.repo("demo");
    let mut core = core_with_fake_cli(&scratch);
    let home = scratch.path.join("home");
    core.cli_home = Some(CliHome::at(home.clone()));
    let project = core.add_project(&repo).unwrap().project;
    let original = create_fake_session(&core, &project.id);
    let journal = core.worktree_journal().unwrap();
    let head = git(&original.worktree, &["rev-parse", "HEAD"]);
    let index = git(&original.worktree, &["write-tree"]);
    let mut expected = original.clone();
    expected.title = "My display name".into();
    expected.manual_title = true;
    // Name mutation must not wait for a Git operation or run any Git command.
    let guard = core.operations.lock().unwrap();
    assert_eq!(
        core.session_set_title(&original.id, " My display name ")
            .unwrap(),
        expected
    );
    drop(guard);
    assert_eq!(core.worktree_journal().unwrap(), journal);
    assert_eq!(git(&original.worktree, &["rev-parse", "HEAD"]), head);
    assert_eq!(git(&original.worktree, &["write-tree"]), index);
    assert_eq!(
        git(&original.worktree, &["branch", "--show-current"]).trim(),
        original.branch
    );
    let prompted = core
        .session_rename_from_prompt(&original.id, "First prompt")
        .unwrap();
    assert_eq!(prompted.title, "My display name");
    assert_eq!(prompted.branch, "first-prompt");
    write_claude_title(&home, &original.worktree, "CLI summary");
    let titled = core.session_apply_cli_title(&original.id).unwrap().unwrap();
    assert_eq!(titled.title, "My display name");
    assert_eq!(titled.branch, "cli-summary");
    assert!(titled.cli_titled);
    let before = core.worktree_journal().unwrap();
    core.session_set_title(&original.id, "Revised name")
        .unwrap();
    assert_eq!(core.session(&original.id).unwrap().branch, "cli-summary");
    assert_eq!(core.worktree_journal().unwrap(), before);
    assert_eq!(core.session_apply_cli_title(&original.id).unwrap(), None);
    core.session_discard(&original.id).unwrap();
}

#[test]
fn the_cli_title_renames_the_branch_once_without_the_project_name() {
    let scratch = Scratch::new();
    let repo = scratch.repo("shika");
    let mut core = core_with_fake_cli(&scratch);
    let home = scratch.path.join("home");
    core.cli_home = Some(CliHome::at(home.clone()));
    let project = core.add_project(&repo).unwrap().project;
    let session = create_fake_session(&core, &project.id);

    // No title yet: nothing changes.
    assert_eq!(core.session_apply_cli_title(&session.id).unwrap(), None);
    let prompted = core
        .session_rename_from_prompt(&session.id, "okay we need a way to blur")
        .unwrap();
    assert_eq!(prompted.branch, "okay-we-need-a-way-to-blur");

    write_claude_title(
        &home,
        &session.worktree,
        "Shika background opacity and blur",
    );
    let titled = core.session_apply_cli_title(&session.id).unwrap().unwrap();
    assert_eq!(titled.branch, "background-opacity-and-blur");
    assert_eq!(titled.title, "Shika background opacity and blur");
    assert!(titled.cli_titled);
    assert_eq!(titled.worktree, session.worktree);
    assert_eq!(
        git(&titled.worktree, &["branch", "--show-current"]).trim(),
        titled.branch
    );
    assert_eq!(core.worktree_journal().unwrap()[0].branch, titled.branch);

    // Applied once. A later title or prompt does not rename again.
    write_claude_title(&home, &session.worktree, "Something else entirely");
    assert_eq!(core.session_apply_cli_title(&session.id).unwrap(), None);
    assert_eq!(
        core.session_rename_from_prompt(&session.id, "another prompt")
            .unwrap(),
        titled
    );
    core.session_discard(&session.id).unwrap();
}

#[test]
fn the_cli_title_uses_the_prefix_and_never_renames_a_pushed_branch() {
    let scratch = Scratch::new();
    let repo = scratch.repo("demo");
    local_remote(&scratch, &repo);
    let mut core = core_with_fake_cli(&scratch);
    let home = scratch.path.join("home");
    core.cli_home = Some(CliHome::at(home.clone()));
    core.save_settings(&Settings {
        branch_prefix: "dev".into(),
        ..Settings::default()
    })
    .unwrap();
    let project = core.add_project(&repo).unwrap().project;

    let fresh = create_fake_session(&core, &project.id);
    write_claude_title(&home, &fresh.worktree, "Fix login flow");
    let titled = core.session_apply_cli_title(&fresh.id).unwrap().unwrap();
    assert_eq!(titled.branch, "dev/fix-login-flow");

    // Pushed before the title arrived, without -u: the name stays.
    let pushed = create_fake_session(&core, &project.id);
    let pushed = core
        .session_rename_from_prompt(&pushed.id, "add a readme")
        .unwrap();
    assert_eq!(pushed.branch, "dev/add-a-readme");
    git(
        &pushed.worktree,
        &["commit", "--allow-empty", "-m", "readme"],
    );
    git(&pushed.worktree, &["push", "origin", "HEAD"]);
    write_claude_title(&home, &pushed.worktree, "Add project README");
    let kept = core.session_apply_cli_title(&pushed.id).unwrap().unwrap();
    assert_eq!(kept.branch, "dev/add-a-readme");
    assert_eq!(kept.title, "Add project README");

    // A worktree switched to another branch keeps it too.
    let switched = create_fake_session(&core, &project.id);
    git(&switched.worktree, &["switch", "-c", "mine"]);
    write_claude_title(&home, &switched.worktree, "Switch test");
    let kept = core.session_apply_cli_title(&switched.id).unwrap().unwrap();
    assert_eq!(kept.branch, switched.branch);
    assert_eq!(
        git(&switched.worktree, &["branch", "--show-current"]).trim(),
        "mine"
    );

    for session in [&fresh, &pushed] {
        core.session_discard(&session.id).unwrap();
    }
}
