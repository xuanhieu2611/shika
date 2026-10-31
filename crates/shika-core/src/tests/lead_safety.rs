use super::*;

#[test]
fn a_broken_lead_gitfile_never_refreshes_or_discards_the_main_checkout() {
    let scratch = Scratch::new();
    let core = core_with_reporting_cli(&scratch);
    let repo = scratch.repo("demo");
    let project = core.add_project(&repo).unwrap().project;
    git(&repo, &["branch", "dev"]);
    git(&repo, &["commit", "--allow-empty", "-m", "main only"]);
    core.set_project_base(&project.id, "dev").unwrap();
    let (lead, _) = start_lead(&core, &scratch, &project.id);
    core.stop_lead(&lead.id).unwrap();
    fs::remove_file(lead.worktree.join(".git")).unwrap();
    let before = git(&repo, &["rev-parse", "HEAD"]);
    let saved = core.saved_lead(&project.id).unwrap();
    let (sink, _) = channel_sink();
    assert_eq!(
        core.start_lead(
            &project.id,
            "claude",
            PtySize::default(),
            sink,
            lead_launch(&scratch)
        ),
        Err(Error::LeadWorktreeInvalid(lead.worktree.clone()))
    );
    assert_eq!(core.saved_lead(&project.id).unwrap(), saved);
    assert_eq!(core.lead_loss(&project.id), Ok(LeadLoss::default()));
    // Forget clears only the saved record and journal, not the broken
    // folder or anything in the main checkout around it.
    fs::write(lead.worktree.join("keep.txt"), "keep\n").unwrap();
    core.forget_lead(&project.id, true).unwrap();
    assert!(lead.worktree.join("keep.txt").exists());
    assert_eq!(git(&repo, &["rev-parse", "HEAD"]), before);
    assert_eq!(
        git(&repo, &["symbolic-ref", "--short", "HEAD"]).trim(),
        "main"
    );
}

#[test]
fn stale_codex_discovery_cannot_write_after_stop_forget_or_replacement() {
    let scratch = Scratch::new();
    let repo = scratch.repo("demo");
    let core = core_with_reporting_cli(&scratch);
    let project = core.add_project(&repo).unwrap().project;
    let (first, _) = start_lead_with(&core, &scratch, &project.id, "codex");
    let late_result = || core.save_discovered_lead_session(&first.session, "old-thread".into());
    core.stop_lead(&first.session.id).unwrap();
    assert_eq!(late_result(), Ok(None));
    core.forget_lead(&project.id, false).unwrap();
    assert_eq!(late_result(), Ok(None));
    assert_eq!(core.saved_lead(&project.id).unwrap(), None);

    let (second, _) = start_lead_with(&core, &scratch, &project.id, "codex");
    assert_eq!(late_result(), Ok(None));
    assert_eq!(
        core.discover_lead_session(&first.session.id, first.started_at),
        Ok(None)
    );
    assert_eq!(
        core.saved_lead(&project.id).unwrap().unwrap().session_id,
        None
    );
    assert_eq!(
        core.save_discovered_lead_session(&second.session, "new-thread".into()),
        Ok(Some("new-thread".into()))
    );
    assert_eq!(
        core.save_discovered_lead_session(&second.session, "duplicate-result".into()),
        Ok(Some("new-thread".into()))
    );
    assert_eq!(late_result(), Ok(None));
    assert_eq!(
        core.saved_lead(&project.id)
            .unwrap()
            .unwrap()
            .session_id
            .as_deref(),
        Some("new-thread")
    );
    core.stop_lead(&second.session.id).unwrap();

    let (third, _) = start_lead_with(&core, &scratch, &project.id, "claude");
    let saved = core.saved_lead(&project.id).unwrap();
    assert_eq!(late_result(), Ok(None));
    assert_eq!(
        core.save_discovered_lead_session(&second.session, "old-codex".into()),
        Ok(None)
    );
    assert_eq!(core.saved_lead(&project.id).unwrap(), saved);
    core.stop_lead(&third.session.id).unwrap();
}

#[test]
fn a_re_added_project_claims_the_lead_tree_so_leftovers_never_offers_it() {
    let scratch = Scratch::new();
    let repo = scratch.repo("demo");
    let core = core_with_reporting_cli(&scratch);
    let old = core.add_project(&repo).unwrap().project;
    let (lead, _) = start_lead(&core, &scratch, &old.id);
    core.stop_lead(&lead.id).unwrap();
    core.remove_project(&old.id).unwrap();
    let new = core.add_project(&repo).unwrap().project;
    assert_ne!(old.id, new.id);
    let (lead, _) = start_lead(&core, &scratch, &new.id);
    fs::write(lead.worktree.join("notes.txt"), "mine\n").unwrap();
    core.stop_lead(&lead.id).unwrap();
    assert!(core.saved_lead(&new.id).unwrap().is_some());
    let entries = core.journal.list().unwrap();
    let entry = entries
        .iter()
        .find(|entry| entry.path == lead.worktree)
        .unwrap();
    assert_eq!(entry.project_id, new.id);
    assert_eq!(
        entries.iter().filter(|e| e.path == lead.worktree).count(),
        1
    );
    assert!(core.leftovers_list().unwrap().is_empty());
    assert_eq!(
        core.leftover_remove(&lead.worktree),
        Err(Error::UnknownLeftover)
    );
    assert!(lead.worktree.join("notes.txt").exists());
}

#[test]
fn commits_in_the_lead_tree_are_kept_by_refresh_and_need_discard_to_forget() {
    let scratch = Scratch::new();
    let repo = scratch.repo("demo");
    let core = core_with_reporting_cli(&scratch);
    let project = core.add_project(&repo).unwrap().project;
    let (lead, _) = start_lead(&core, &scratch, &project.id);
    fs::write(lead.worktree.join("plan.md"), "plan\n").unwrap();
    git(&lead.worktree, &["add", "plan.md"]);
    git(&lead.worktree, &["commit", "-q", "-m", "lead commit"]);
    let commit = git(&lead.worktree, &["rev-parse", "HEAD"])
        .trim()
        .to_string();
    core.stop_lead(&lead.id).unwrap();
    assert_eq!(
        core.lead_loss(&project.id),
        Ok(LeadLoss {
            uncommitted: false,
            unreferenced_commits: true
        })
    );
    assert_eq!(core.lead_has_changes(&project.id), Ok(true));

    // Not moved, and the summary says why.
    git(&repo, &["commit", "--allow-empty", "-m", "second"]);
    let (again, _) = start_lead_with(&core, &scratch, &project.id, "claude");
    assert_eq!(again.worktree, LeadWorktree::KeptCommits);
    let head = git(&again.session.worktree, &["rev-parse", "HEAD"]);
    assert_eq!(head.trim(), commit);
    core.stop_lead(&again.session.id).unwrap();

    // Forget asks first, and discards only when told to.
    assert_eq!(
        core.forget_lead(&project.id, false),
        Err(Error::WorktreeHasChanges(None))
    );
    assert!(lead.worktree.exists());
    core.forget_lead(&project.id, true).unwrap();
    assert!(!lead.worktree.exists());
}

#[test]
fn a_lead_commit_that_a_branch_reaches_does_not_block_refresh_or_forget() {
    let scratch = Scratch::new();
    let repo = scratch.repo("demo");
    let core = core_with_reporting_cli(&scratch);
    let project = core.add_project(&repo).unwrap().project;
    let (lead, _) = start_lead(&core, &scratch, &project.id);
    fs::write(lead.worktree.join("plan.md"), "plan\n").unwrap();
    git(&lead.worktree, &["add", "plan.md"]);
    git(&lead.worktree, &["commit", "-q", "-m", "lead commit"]);
    git(&lead.worktree, &["branch", "keep-it"]);
    core.stop_lead(&lead.id).unwrap();
    assert_eq!(core.lead_loss(&project.id), Ok(LeadLoss::default()));
    git(&repo, &["commit", "--allow-empty", "-m", "second"]);
    let (again, _) = start_lead_with(&core, &scratch, &project.id, "claude");
    assert!(matches!(again.worktree, LeadWorktree::Refreshed { .. }));
    core.stop_lead(&again.session.id).unwrap();
    core.forget_lead(&project.id, false).unwrap();
}

#[test]
fn a_symlink_at_the_lead_path_is_never_followed_by_forget_or_refresh() {
    let scratch = Scratch::new();
    let repo = scratch.repo("demo");
    let core = core_with_reporting_cli(&scratch);
    let project = core.add_project(&repo).unwrap().project;
    let (lead, _) = start_lead(&core, &scratch, &project.id);
    core.stop_lead(&lead.id).unwrap();
    let (sink, _rx) = channel_sink();
    let task = core
        .create_session(
            &project.id,
            "claude",
            PtySize::default(),
            sink,
            LaunchOptions::default(),
        )
        .unwrap();
    fs::write(task.worktree.join("work.txt"), "precious\n").unwrap();
    // The Lead folder replaced by a link to the task's tree.
    git(
        &repo,
        &[
            "worktree",
            "remove",
            "--force",
            lead.worktree.to_str().unwrap(),
        ],
    );
    std::os::unix::fs::symlink(&task.worktree, &lead.worktree).unwrap();
    assert_eq!(core.lead_loss(&project.id), Ok(LeadLoss::default()));
    // A start refuses it and does not touch the target.
    let (sink, _rx) = channel_sink();
    assert_eq!(
        core.start_lead(
            &project.id,
            "claude",
            PtySize::new(30, 90),
            sink,
            lead_launch(&scratch)
        )
        .map(|_| ()),
        Err(Error::LeadWorktreeInvalid(lead.worktree.clone()))
    );
    core.forget_lead(&project.id, true).unwrap();
    assert!(task.worktree.join("work.txt").exists());
    assert!(core.saved_lead(&project.id).unwrap().is_none());
    // Even a stale journal entry for the link is dropped, not followed.
    core.journal
        .add(&JournalEntry {
            project_id: "gone".into(),
            branch: String::new(),
            path: lead.worktree.clone(),
            base_ref: None,
            started_by_lead: false,
            recovery: None,
        })
        .unwrap();
    core.leftover_remove(&lead.worktree).unwrap();
    assert!(task.worktree.join("work.txt").exists());
}
