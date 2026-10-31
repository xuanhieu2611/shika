use super::*;

#[test]
fn safe_close_refuses_work_and_an_empty_task_deletes_its_branch() {
    let scratch = Scratch::new();
    let repo = scratch.repo("demo");
    let core = core_with_fake_cli(&scratch);
    let project = core.add_project(&repo).unwrap().project;
    let session = create_fake_session(&core, &project.id);
    let state = core.session_git_state(&session.id, false).unwrap();
    assert!(!state.has_own_commits && !state.unpushed && !state.pushed);
    assert!(!state.requires_confirmation());
    assert_eq!(
        core.session_close(&session.id, true),
        Err(Error::CloseNeedsConfirmation)
    );
    git(
        &session.worktree,
        &["commit", "--allow-empty", "-m", "task"],
    );
    assert!(
        core.session_git_state(&session.id, false)
            .unwrap()
            .can_push()
    );
    assert_eq!(
        core.session_close(&session.id, false),
        Err(Error::CloseNeedsConfirmation)
    );
    assert!(session.worktree.exists());
    core.session_discard(&session.id).unwrap();
    let empty = create_fake_session(&core, &project.id);
    core.session_close(&empty.id, false).unwrap();
    assert!(!empty.worktree.exists());
    assert!(
        git(&repo, &["branch", "--list", &empty.branch])
            .trim()
            .is_empty()
    );
}

#[test]
fn manual_push_keeps_card_then_safe_close_keeps_pushed_branch() {
    let scratch = Scratch::new();
    let repo = scratch.repo("demo");
    local_remote(&scratch, &repo);
    let core = core_with_fake_cli(&scratch);
    let project = core.add_project(&repo).unwrap().project;
    let session = create_fake_session(&core, &project.id);
    git(
        &session.worktree,
        &["commit", "--allow-empty", "-m", "task"],
    );
    git(&session.worktree, &["push", "-u", "origin", "HEAD"]);
    assert!(core.session(&session.id).is_some());
    assert!(session.worktree.exists());
    let state = core.session_git_state(&session.id, false).unwrap();
    assert!(state.pushed && state.has_own_commits && !state.unpushed);
    core.session_close(&session.id, false).unwrap();
    assert!(!session.worktree.exists());
    assert!(
        !git(&repo, &["branch", "--list", &session.branch])
            .trim()
            .is_empty()
    );
}

#[test]
fn shell_push_without_upstream_counts_as_pushed() {
    let scratch = Scratch::new();
    let repo = scratch.repo("demo");
    local_remote(&scratch, &repo);
    let core = core_with_fake_cli(&scratch);
    let project = core.add_project(&repo).unwrap().project;
    let session = create_fake_session(&core, &project.id);
    git(
        &session.worktree,
        &["commit", "--allow-empty", "-m", "task"],
    );
    git(&session.worktree, &["push", "origin", "HEAD"]);
    let state = core.session_git_state(&session.id, false).unwrap();
    assert!(state.pushed && state.has_own_commits && !state.unpushed);
    assert!(!state.requires_confirmation());
    git(
        &session.worktree,
        &["commit", "--allow-empty", "-m", "more"],
    );
    let state = core.session_git_state(&session.id, false).unwrap();
    assert!(state.unpushed && !state.pushed);
    assert_eq!(
        core.session_close(&session.id, false),
        Err(Error::CloseNeedsConfirmation)
    );
    git(&session.worktree, &["push", "origin", "HEAD"]);
    core.session_close(&session.id, false).unwrap();
    assert!(!session.worktree.exists());
    assert!(
        !git(&repo, &["branch", "--list", &session.branch])
            .trim()
            .is_empty()
    );
}

#[test]
fn push_choice_preserves_failed_task_and_success_keeps_branch() {
    let scratch = Scratch::new();
    let repo = scratch.repo("demo");
    let core = core_with_fake_cli(&scratch);
    let project = core.add_project(&repo).unwrap().project;
    let session = create_fake_session(&core, &project.id);
    assert_eq!(
        core.session_push_and_close(&session.id),
        Err(Error::NothingToPush)
    );
    git(
        &session.worktree,
        &["commit", "--allow-empty", "-m", "task"],
    );
    fs::write(session.worktree.join("untracked"), "wip").unwrap();
    assert_eq!(
        core.session_push_and_close(&session.id),
        Err(Error::PushDirty)
    );
    fs::remove_file(session.worktree.join("untracked")).unwrap();
    assert!(matches!(
        core.session_push_and_close(&session.id),
        Err(Error::Push(_))
    ));
    assert!(core.session(&session.id).is_some());
    assert!(session.worktree.exists());
    assert_eq!(core.worktree_journal().unwrap().len(), 1);
    core.write(session.pty.unwrap(), b"still alive\r").unwrap();
    let remote = local_remote(&scratch, &repo);
    // A rejecting bare remote exercises a real push error, not a fake git.
    let hook = remote.join("hooks/pre-receive");
    fs::write(&hook, "#!/bin/sh\nexit 1\n").unwrap();
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(matches!(
        core.session_push_and_close(&session.id),
        Err(Error::Push(_))
    ));
    assert!(core.session(&session.id).is_some());
    fs::remove_file(hook).unwrap();
    core.session_push_and_close(&session.id).unwrap();
    assert!(core.session(&session.id).is_none());
    assert!(!session.worktree.exists());
    assert!(
        !git(&repo, &["branch", "--list", &session.branch])
            .trim()
            .is_empty()
    );
    assert!(
        git(
            &remote,
            &["show-ref", &format!("refs/heads/{}", session.branch)]
        )
        .contains(&session.branch)
    );
}

#[test]
fn custom_default_branch_and_stale_origin_head_are_checked_safely() {
    let scratch = Scratch::new();
    let repo = scratch.repo("demo");
    git(&repo, &["branch", "-m", "development"]);
    git(
        &repo,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/missing",
        ],
    );
    let core = core_with_fake_cli(&scratch);
    let project = core.add_project(&repo).unwrap().project;
    let session = create_fake_session(&core, &project.id);
    assert!(!core.session_git_state(&session.id, false).unwrap().unpushed);
    git(
        &session.worktree,
        &["commit", "--allow-empty", "-m", "task"],
    );
    assert!(core.session_git_state(&session.id, false).unwrap().unpushed);
    // Git normally forbids two checkouts of a branch. An explicit user
    // override must still never make core treat task commits as empty.
    git(
        &repo,
        &["checkout", "--ignore-other-worktrees", &session.branch],
    );
    assert!(matches!(
        core.session_close(&session.id, false),
        Err(Error::GitStatus(_))
    ));
    assert!(session.worktree.exists());
    git(&repo, &["checkout", "development"]);
    core.session_discard(&session.id).unwrap();
}
