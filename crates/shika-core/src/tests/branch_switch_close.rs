use super::*;

#[test]
fn switching_to_a_pr_branch_after_a_rename_can_recover_before_close() {
    let scratch = Scratch::new();
    let repo = scratch.repo("demo");
    let core = core_with_fake_cli(&scratch);
    let project = core.add_project(&repo).unwrap().project;
    let session = create_fake_session(&core, &project.id);
    git(&session.worktree, &["branch", "-m", "conflict-resolver"]);
    let session = core.session_refresh_branch(&session.id).unwrap();
    git(
        &session.worktree,
        &["checkout", "-b", "existing-pr", "main"],
    );
    let expected = Error::TaskBranchChanged {
        expected: "conflict-resolver".into(),
        current: "existing-pr".into(),
    };
    assert_eq!(
        core.session_git_state(&session.id, false),
        Err(expected.clone())
    );
    assert_eq!(
        core.session_close(&session.id, false),
        Err(expected.clone())
    );
    assert_eq!(core.session_discard(&session.id), Err(expected.clone()));
    assert_eq!(core.session_push_and_close(&session.id), Err(expected));
    assert!(session.worktree.exists());
    assert!(branch_exists(&repo, "conflict-resolver"));
    git(&session.worktree, &["switch", "conflict-resolver"]);
    core.session_close(&session.id, false).unwrap();
    assert!(!session.worktree.exists());
    assert!(branch_exists(&repo, "existing-pr"));
}

#[test]
fn switched_close_preserves_both_branches_after_push_or_merge() {
    for (merge, upstream) in [(false, true), (true, true), (false, false)] {
        let scratch = Scratch::new();
        let (repo, _remote) = repo_with_dev_on_origin(&scratch);
        let core = core_with_fake_cli(&scratch);
        let project = core.add_project(&repo).unwrap().project;
        let session = create_fake_session(&core, &project.id);
        git(&session.worktree, &["switch", "-c", "better-name"]);
        git(
            &session.worktree,
            &["commit", "--allow-empty", "-m", "task"],
        );
        if upstream {
            git(&session.worktree, &["push", "-u", "origin", "HEAD"]);
        } else {
            git(&session.worktree, &["push", "origin", "HEAD"]);
        }
        if merge {
            git(&repo, &["merge", "--ff-only", "better-name"]);
            git(&repo, &["push", "origin", "main"]);
        }
        let (sink, _rx) = channel_sink();
        let shell = core
            .open_shell(&session.id, PtySize::default(), sink)
            .unwrap();
        let preview = core.session_switched_close_check(&session.id).unwrap();
        assert_eq!(preview.recorded, session.branch);
        assert_eq!(preview.current, "better-name");
        assert_eq!(core.session(&session.id).unwrap().branch, session.branch);
        core.session_close_switched(&session.id, &preview).unwrap();
        assert!(!session.worktree.exists());
        assert!(branch_exists(&repo, &session.branch));
        assert!(branch_exists(&repo, "better-name"));
        assert!(core.sessions().is_empty());
        assert!(core.journal.list().unwrap().is_empty());
        assert!(core.write(shell, b"echo still-running\n").is_err());
    }
}

#[test]
fn switched_close_accepts_independently_published_or_integrated_branches() {
    for integrated in [true, false] {
        let scratch = Scratch::new();
        let (repo, _remote) = repo_with_dev_on_origin(&scratch);
        let core = core_with_fake_cli(&scratch);
        let project = core.add_project(&repo).unwrap().project;
        let session = create_fake_session(&core, &project.id);
        git(
            &session.worktree,
            &["commit", "--allow-empty", "-m", "original work"],
        );
        if integrated {
            git(&repo, &["merge", "--ff-only", &session.branch]);
            git(&repo, &["push", "origin", "main"]);
        } else {
            git(&session.worktree, &["push", "origin", "HEAD"]);
        }
        git(&session.worktree, &["switch", "-c", "review/other", "main"]);
        git(
            &session.worktree,
            &["commit", "--allow-empty", "-m", "other work"],
        );
        git(&session.worktree, &["push", "origin", "HEAD"]);
        let preview = core.session_switched_close_check(&session.id).unwrap();
        core.session_close_switched(&session.id, &preview).unwrap();
        assert!(branch_exists(&repo, &session.branch));
        assert!(branch_exists(&repo, "review/other"));
    }
}

#[test]
fn switched_close_accepts_empty_local_tasks_without_a_remote() {
    let scratch = Scratch::new();
    let repo = scratch.repo("demo");
    let core = core_with_fake_cli(&scratch);
    let project = core.add_project(&repo).unwrap().project;
    let session = create_fake_session(&core, &project.id);
    git(&session.worktree, &["switch", "-c", "other"]);
    let preview = core.session_switched_close_check(&session.id).unwrap();
    core.session_close_switched(&session.id, &preview).unwrap();
    assert!(branch_exists(&repo, &session.branch));
    assert!(branch_exists(&repo, "other"));
}

#[test]
fn switched_close_refuses_unpublished_work_on_either_branch() {
    for original in [true, false] {
        let scratch = Scratch::new();
        let (repo, _remote) = repo_with_dev_on_origin(&scratch);
        let core = core_with_fake_cli(&scratch);
        let project = core.add_project(&repo).unwrap().project;
        let session = create_fake_session(&core, &project.id);
        if original {
            git(
                &session.worktree,
                &["commit", "--allow-empty", "-m", "hidden task"],
            );
        }
        git(&session.worktree, &["switch", "-c", "other", "main"]);
        if !original {
            git(
                &session.worktree,
                &["commit", "--allow-empty", "-m", "other task"],
            );
        }
        assert!(matches!(
            core.session_switched_close_check(&session.id),
            Err(Error::SwitchedCloseUnsafe(_))
        ));
        assert!(session.worktree.exists());
        assert!(branch_exists(&repo, &session.branch));
        assert!(!core.sessions().is_empty());
        assert!(
            core.write(session.pty.unwrap(), b"").is_ok(),
            "refusal kept the agent"
        );
    }
}

#[test]
fn switched_close_rechecks_confirmation_and_refuses_dirty_detached_or_missing_refs() {
    for change in [
        "dirty",
        "current-commit",
        "original-commit",
        "switch",
        "detach",
        "delete",
    ] {
        let scratch = Scratch::new();
        let (repo, _remote) = repo_with_dev_on_origin(&scratch);
        let core = core_with_fake_cli(&scratch);
        let project = core.add_project(&repo).unwrap().project;
        let session = create_fake_session(&core, &project.id);
        git(&session.worktree, &["switch", "-c", "other"]);
        let preview = core.session_switched_close_check(&session.id).unwrap();
        match change {
            "dirty" => fs::write(session.worktree.join("untracked"), "keep me").unwrap(),
            "current-commit" => {
                git(
                    &session.worktree,
                    &["commit", "--allow-empty", "-m", "later"],
                );
                git(&session.worktree, &["push", "origin", "HEAD"]);
            }
            "original-commit" => {
                git(&session.worktree, &["switch", &session.branch]);
                git(
                    &session.worktree,
                    &["commit", "--allow-empty", "-m", "later"],
                );
                git(&session.worktree, &["push", "origin", "HEAD"]);
                git(&session.worktree, &["switch", "other"]);
            }
            "switch" => {
                git(&session.worktree, &["switch", "-c", "third"]);
            }
            "detach" => {
                git(&session.worktree, &["switch", "--detach"]);
            }
            "delete" => {
                git(&repo, &["branch", "-D", &session.branch]);
            }
            _ => unreachable!(),
        }
        assert!(
            core.session_close_switched(&session.id, &preview).is_err(),
            "{change}"
        );
        assert!(session.worktree.exists(), "{change}");
        assert!(!core.sessions().is_empty());
        assert!(!core.journal.list().unwrap().is_empty());
        if change == "dirty" {
            assert!(core.session_switched_close_check(&session.id).is_err());
        }
    }
}

#[test]
fn switched_close_does_not_treat_local_main_as_proof_of_publication() {
    let scratch = Scratch::new();
    let (repo, _remote) = repo_with_dev_on_origin(&scratch);
    let core = core_with_fake_cli(&scratch);
    let project = core.add_project(&repo).unwrap().project;
    let session = create_fake_session(&core, &project.id);
    git(&repo, &["switch", "dev"]);
    git(&session.worktree, &["switch", "main"]);
    git(
        &session.worktree,
        &["commit", "--allow-empty", "-m", "unpublished main"],
    );
    assert!(core.session_switched_close_check(&session.id).is_err());
    assert!(session.worktree.exists());
}

#[test]
fn safe_close_never_deletes_commits_hidden_by_a_shell_branch_switch() {
    let scratch = Scratch::new();
    let repo = scratch.repo("demo");
    let core = core_with_fake_cli(&scratch);
    let project = core.add_project(&repo).unwrap().project;
    let session = create_fake_session(&core, &project.id);
    git(
        &session.worktree,
        &["commit", "--allow-empty", "-m", "task"],
    );
    git(
        &session.worktree,
        &["checkout", "-b", "temporary-check", "main"],
    );
    assert!(matches!(
        core.session_close(&session.id, false),
        Err(Error::TaskBranchChanged { .. })
    ));
    assert!(matches!(
        core.session_push_and_close(&session.id),
        Err(Error::TaskBranchChanged { .. })
    ));
    assert!(session.worktree.exists());
    assert!(
        !git(&repo, &["branch", "--list", &session.branch])
            .trim()
            .is_empty()
    );
    git(&session.worktree, &["checkout", &session.branch]);
    core.session_discard(&session.id).unwrap();
}
