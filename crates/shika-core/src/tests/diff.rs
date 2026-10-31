use super::*;

#[test]
fn diff_stat_reports_the_session_worktree() {
    let scratch = Scratch::new();
    let repo = scratch.repo("demo");
    let core = core_with_fake_cli(&scratch);
    let project = core.add_project(&repo).unwrap().project;
    let session = create_fake_session(&core, &project.id);
    assert_eq!(
        core.session_diff_stat(&session.id).unwrap(),
        DiffStat::default()
    );
    fs::write(session.worktree.join("a.txt"), "one\ntwo\n").unwrap();
    git(&session.worktree, &["add", "a.txt"]);
    git(&session.worktree, &["commit", "-m", "task"]);
    fs::write(session.worktree.join("b.txt"), "b\n").unwrap();
    assert_eq!(
        core.session_diff_stat(&session.id).unwrap(),
        DiffStat {
            files: 2,
            insertions: 3,
            deletions: 0,
        }
    );
    assert_eq!(
        core.session_diff_stat("missing"),
        Err(Error::UnknownSession)
    );
    core.session_discard(&session.id).unwrap();
}

#[test]
fn session_diff_reads_the_session_worktree_and_expands_a_file() {
    let scratch = Scratch::new();
    let repo = scratch.repo("demo");
    let core = core_with_fake_cli(&scratch);
    let project = core.add_project(&repo).unwrap().project;
    let session = create_fake_session(&core, &project.id);
    assert_eq!(
        core.session_diff(&session.id).unwrap(),
        SessionDiff::default()
    );
    fs::write(session.worktree.join("a.txt"), "one\ntwo\n").unwrap();
    git(&session.worktree, &["add", "a.txt"]);
    git(&session.worktree, &["commit", "-m", "task"]);
    fs::write(session.worktree.join("b.txt"), "b\n").unwrap();
    let diff = core.session_diff(&session.id).unwrap();
    assert_eq!(diff.stat, core.session_diff_stat(&session.id).unwrap());
    let paths: Vec<(&str, FileStatus)> = diff
        .files
        .iter()
        .map(|file| (file.path.as_str(), file.status))
        .collect();
    assert_eq!(
        paths,
        [
            ("a.txt", FileStatus::Added),
            ("b.txt", FileStatus::Untracked)
        ]
    );
    let again = core
        .session_file_diff(&session.id, &diff.files[0].key)
        .unwrap();
    assert_eq!(again.as_ref(), Some(&diff.files[0]));
    assert_eq!(core.session_diff("missing"), Err(Error::UnknownSession));
    assert_eq!(
        core.session_file_diff("missing", &diff.files[0].key),
        Err(Error::UnknownSession)
    );
    core.session_discard(&session.id).unwrap();
}

#[test]
fn a_session_worktree_without_its_git_file_is_an_error_not_the_main_checkout() {
    let scratch = Scratch::new();
    let repo = scratch.repo("demo");
    let core = core_with_fake_cli(&scratch);
    let project = core.add_project(&repo).unwrap().project;
    let session = create_fake_session(&core, &project.id);
    fs::write(session.worktree.join("b.txt"), "b\n").unwrap();
    let key = core.session_diff(&session.id).unwrap().files[0].key.clone();
    let dot_git = session.worktree.join(".git");
    let saved = fs::read(&dot_git).unwrap();
    fs::remove_file(&dot_git).unwrap();
    // The main checkout around `.worktrees/` is clean: reading it instead
    // would show "No changes" and hide the card's stat.
    match core.session_diff(&session.id) {
        Err(Error::ReadChanges(Some(line))) => {
            assert!(line.contains("not a git repository"), "{line}")
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        core.session_diff_stat(&session.id),
        Err(Error::GitStatus(_))
    ));
    // An untracked file is read from the worktree without git.
    assert!(core.session_file_diff(&session.id, &key).is_ok());
    fs::write(&dot_git, saved).unwrap();
    assert_eq!(core.session_diff(&session.id).unwrap().files.len(), 1);
    core.session_discard(&session.id).unwrap();
}
