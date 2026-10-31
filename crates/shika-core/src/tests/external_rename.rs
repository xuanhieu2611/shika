use super::*;

#[test]
fn external_rename_updates_the_card_record_and_journal_without_changing_the_title() {
    let scratch = Scratch::new();
    let repo = scratch.repo("demo");
    let core = core_with_fake_cli(&scratch);
    let project = core.add_project(&repo).unwrap().project;
    let session = create_fake_session(&core, &project.id);
    git(&session.worktree, &["branch", "-m", "first-rename"]);
    git(
        &session.worktree,
        &["commit", "--allow-empty", "-m", "task"],
    );
    git(&session.worktree, &["branch", "-m", "renamed-for-pr"]);
    let refreshed = core.session_refresh_branch(&session.id).unwrap();
    assert_eq!(refreshed.branch, "renamed-for-pr");
    assert_eq!(refreshed.title, session.title);
    assert_eq!(refreshed.worktree, session.worktree);
    assert_eq!(refreshed.base_ref, session.base_ref);
    assert_eq!(core.worktree_journal().unwrap()[0].branch, refreshed.branch);
    assert_eq!(
        core.session_close(&session.id, false),
        Err(Error::CloseNeedsConfirmation)
    );
    core.session_discard(&session.id).unwrap();
    assert!(!session.worktree.exists());
    assert!(!branch_exists(&repo, "renamed-for-pr"));
}

#[test]
fn an_external_rename_is_not_published_in_memory_when_the_journal_cannot_save() {
    let scratch = Scratch::new();
    let repo = scratch.repo("demo");
    let core = core_with_fake_cli(&scratch);
    let project = core.add_project(&repo).unwrap().project;
    let session = create_fake_session(&core, &project.id);
    git(&session.worktree, &["branch", "-m", "renamed"]);
    let journal = core.data_dir().join("worktrees.json");
    let contents = fs::read(&journal).unwrap();
    fs::remove_file(&journal).unwrap();
    fs::create_dir(&journal).unwrap();
    assert!(core.session_refresh_branch(&session.id).is_err());
    assert!(core.session_close(&session.id, false).is_err());
    assert_eq!(core.session(&session.id).unwrap().branch, session.branch);
    assert!(session.worktree.exists());
    fs::remove_dir(&journal).unwrap();
    fs::write(&journal, contents).unwrap();
    core.session_discard(&session.id).unwrap();
    assert!(!branch_exists(&repo, "renamed"));
}

#[test]
fn renamed_dirty_task_still_requires_confirmation_and_discard_uses_the_new_name() {
    let scratch = Scratch::new();
    let repo = scratch.repo("demo");
    let core = core_with_fake_cli(&scratch);
    let project = core.add_project(&repo).unwrap().project;
    let session = create_fake_session(&core, &project.id);
    git(&session.worktree, &["branch", "-m", "renamed-dirty"]);
    fs::write(session.worktree.join("unsaved"), "work").unwrap();
    assert!(core.session_git_state(&session.id, false).unwrap().dirty);
    assert!(matches!(
        core.session_close(&session.id, false),
        Err(Error::WorktreeHasChanges(_))
    ));
    assert!(session.worktree.exists());
    assert_eq!(
        core.session_push_and_close(&session.id),
        Err(Error::PushDirty)
    );
    core.session_discard(&session.id).unwrap();
    assert!(!branch_exists(&repo, "renamed-dirty"));
}

#[test]
fn renamed_empty_task_closes_and_deletes_the_new_branch_without_a_poll() {
    let scratch = Scratch::new();
    let repo = scratch.repo("demo");
    let core = core_with_fake_cli(&scratch);
    let project = core.add_project(&repo).unwrap().project;
    let session = create_fake_session(&core, &project.id);
    git(&session.worktree, &["branch", "-m", "renamed-empty"]);
    core.session_close(&session.id, false).unwrap();
    assert!(!session.worktree.exists());
    assert!(!branch_exists(&repo, "renamed-empty"));
    assert!(core.worktree_journal().unwrap().is_empty());
}

#[test]
fn renamed_task_can_push_and_close_or_close_after_a_push_and_merge() {
    for close_with_push in [true, false] {
        let scratch = Scratch::new();
        let (repo, _remote) = repo_with_dev_on_origin(&scratch);
        let core = core_with_fake_cli(&scratch);
        let project = core.add_project(&repo).unwrap().project;
        let session = create_fake_session(&core, &project.id);
        git(
            &session.worktree,
            &["commit", "--allow-empty", "-m", "task"],
        );
        git(&session.worktree, &["branch", "-m", "renamed-pushed"]);
        if close_with_push {
            core.session_push_and_close(&session.id).unwrap();
        } else {
            git(&session.worktree, &["push", "-u", "origin", "HEAD"]);
            git(&repo, &["merge", "--ff-only", "renamed-pushed"]);
            git(&repo, &["push", "origin", "main"]);
            core.session_close(&session.id, false).unwrap();
        }
        assert!(!session.worktree.exists());
        assert!(branch_exists(&repo, "renamed-pushed"));
        assert!(core.sessions().is_empty());
    }
}

#[test]
fn missing_original_ref_is_not_proof_of_a_rename_and_detach_is_not_adopted() {
    let scratch = Scratch::new();
    let repo = scratch.repo("demo");
    let core = core_with_fake_cli(&scratch);
    let project = core.add_project(&repo).unwrap().project;
    let session = create_fake_session(&core, &project.id);
    git(&session.worktree, &["checkout", "-b", "unrelated", "main"]);
    git(&repo, &["branch", "-D", &session.branch]);
    for detached in [false, true] {
        if detached {
            git(&session.worktree, &["checkout", "--detach"]);
        }
        assert!(matches!(
            core.session_refresh_branch(&session.id),
            Err(Error::TaskBranchChanged { .. })
        ));
        assert!(matches!(
            core.session_close(&session.id, false),
            Err(Error::TaskBranchChanged { .. })
        ));
        assert!(matches!(
            core.session_discard(&session.id),
            Err(Error::TaskBranchChanged { .. })
        ));
        assert!(session.worktree.exists());
        assert_eq!(core.session(&session.id).unwrap().branch, session.branch);
    }
}

#[test]
fn a_copied_branch_does_not_inherit_the_tasks_rename_identity() {
    let scratch = Scratch::new();
    let repo = scratch.repo("demo");
    let core = core_with_fake_cli(&scratch);
    let project = core.add_project(&repo).unwrap().project;
    let session = create_fake_session(&core, &project.id);
    git(&session.worktree, &["branch", "-m", "renamed"]);
    git(&session.worktree, &["branch", "-c", "copied"]);
    git(&session.worktree, &["switch", "copied"]);
    git(&repo, &["branch", "-D", "renamed"]);
    assert!(matches!(
        core.session_close(&session.id, false),
        Err(Error::TaskBranchChanged { .. })
    ));
    assert!(session.worktree.exists());
    assert_eq!(core.session(&session.id).unwrap().branch, session.branch);
}

#[test]
fn recreated_original_branch_and_missing_rename_history_keep_close_blocked() {
    for recreate in [true, false] {
        let scratch = Scratch::new();
        let repo = scratch.repo("demo");
        let core = core_with_fake_cli(&scratch);
        let project = core.add_project(&repo).unwrap().project;
        let session = create_fake_session(&core, &project.id);
        git(&session.worktree, &["branch", "-m", "renamed"]);
        if recreate {
            git(&repo, &["branch", &session.branch, "main"]);
        } else {
            git(
                &session.worktree,
                &["reflog", "expire", "--expire=now", "refs/heads/renamed"],
            );
        }
        assert!(matches!(
            core.session_close(&session.id, false),
            Err(Error::TaskBranchChanged { .. })
        ));
        assert!(session.worktree.exists());
    }
}
