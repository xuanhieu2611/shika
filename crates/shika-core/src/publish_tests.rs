use super::*;
struct Fixture {
    root: PathBuf,
    session: Session,
    git: PathBuf,
    gh: PathBuf,
    remote: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        use std::os::unix::fs::PermissionsExt;
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "shika-publish-test-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let repo = root.join("repo");
        std::fs::create_dir(&repo).unwrap();
        let real = Path::new("/usr/bin/git");
        git(real, "/usr/bin:/bin", &repo, &["init", "-b", "main"]).unwrap();
        git(
            real,
            "/usr/bin:/bin",
            &repo,
            &["config", "user.name", "Test User"],
        )
        .unwrap();
        git(
            real,
            "/usr/bin:/bin",
            &repo,
            &["config", "user.email", "test@example.com"],
        )
        .unwrap();
        std::fs::write(repo.join("tracked"), "base\n").unwrap();
        git(real, "/usr/bin:/bin", &repo, &["add", "."]).unwrap();
        git(real, "/usr/bin:/bin", &repo, &["commit", "-m", "base"]).unwrap();
        git(real, "/usr/bin:/bin", &repo, &["branch", "dev"]).unwrap();
        let remote = root.join("remote.git");
        git(
            real,
            "/usr/bin:/bin",
            &repo,
            &[
                "clone",
                "--bare",
                repo.to_str().unwrap(),
                remote.to_str().unwrap(),
            ],
        )
        .unwrap();
        git(
            real,
            "/usr/bin:/bin",
            &repo,
            &[
                "remote",
                "add",
                "origin",
                "https://github.com/test/repo.git",
            ],
        )
        .unwrap();
        let worktree = root.join("task");
        git(
            real,
            "/usr/bin:/bin",
            &repo,
            &[
                "worktree",
                "add",
                "-b",
                "feat/task",
                worktree.to_str().unwrap(),
                "dev",
            ],
        )
        .unwrap();
        let wrapper = root.join("git");
        // Only transport is fake; all staging, commits, and refs are real.
        std::fs::write(&wrapper, format!("#!/bin/sh\nif [ \"$3\" = fetch ]; then\n  dir=$2; shift 3; shift; shift\n  exec /usr/bin/git -C \"$dir\" fetch --no-tags '{}' \"$@\"\nfi\nif [ \"$3\" = push ]; then\n  dir=$2; shift 3\n  exec /usr/bin/git -C \"$dir\" -c remote.origin.pushurl='{}' push \"$@\"\nfi\nexec /usr/bin/git \"$@\"\n", remote.display(), remote.display())).unwrap();
        let gh = root.join("gh");
        std::fs::write(&gh, format!("#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}/calls'\ncase \"$1 $2\" in\n 'repo view') echo '{{\"nameWithOwner\":\"test/repo\"}}';;\n 'api --hostname') printf 'main\\ndev\\n';;\n 'pr list') if [ -f '{}/existing' ]; then echo '[{{\"url\":\"https://github.com/test/repo/pull/1\",\"headRepository\":{{\"name\":\"repo\"}},\"headRepositoryOwner\":{{\"login\":\"test\"}}}}]'; else echo '[]'; fi;;\n 'pr create') if [ -f '{}/fail' ]; then echo 'GitHub unavailable' >&2; exit 1; fi; echo 'https://github.com/test/repo/pull/1';;\n *) exit 9;;\nesac\n", root.display(), root.display(), root.display())).unwrap();
        for path in [&wrapper, &gh] {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let session = Session {
            id: "task".into(),
            project_id: "project".into(),
            preset_id: "pi".into(),
            preset_name: "Pi".into(),
            title: "Add useful feature".into(),
            manual_title: false,
            branch: "feat/task".into(),
            repo,
            worktree,
            base_ref: Some("refs/heads/dev".into()),
            pty: Some(crate::PtyId(1)),
            shell_ptys: vec![],
            cli_titled: true,
            prompt_named: false,
            agent_started: std::time::UNIX_EPOCH,
            kind: crate::SessionKind::Task,
            started_by: None,
        };
        Self {
            root,
            session,
            git: wrapper,
            gh,
            remote,
        }
    }
    fn preview(&self) -> PublishPreview {
        preview(&self.git, &self.gh, "/usr/bin:/bin", &self.session).unwrap()
    }
    fn publish(&self, preview: &PublishPreview) -> Result<PublishedPr> {
        publish(
            &self.git,
            &self.gh,
            "/usr/bin:/bin",
            &self.session,
            preview,
            "dev",
            "Add useful feature",
            None,
        )
    }
    fn git(&self, args: &[&str]) -> String {
        text(git(&self.git, "/usr/bin:/bin", &self.session.worktree, args).unwrap())
    }
    fn edit(&self) {
        std::fs::write(self.session.worktree.join("tracked"), "feature\n").unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn manual_task_name_seeds_future_commit_and_pr_defaults_only() {
    let mut f = Fixture::new();
    f.edit();
    let store = crate::session::SessionStore::new();
    store.insert(f.session.clone());
    let old_preview = f.preview();
    f.session = store
        .set_title(&f.session.id, "My chosen task name")
        .unwrap();
    let preview = f.preview();
    assert_eq!(old_preview.title, "Add useful feature");
    assert_eq!(preview.title, "My chosen task name");
    assert_eq!(preview.branch, old_preview.branch);
    assert_eq!(preview.tree, old_preview.tree);
    assert_eq!(f.git(&["log", "-1", "--format=%s"]), "base");
    publish(
        &f.git,
        &f.gh,
        "/usr/bin:/bin",
        &f.session,
        &preview,
        "dev",
        &preview.title,
        None,
    )
    .unwrap();
    assert_eq!(f.git(&["log", "-1", "--format=%s"]), "My chosen task name");
    let calls = std::fs::read_to_string(f.root.join("calls")).unwrap();
    assert!(calls.contains("--head feat/task --base dev --title My chosen task name --fill"));
    f.session = store.set_title(&f.session.id, "Later name").unwrap();
    assert_eq!(f.git(&["log", "-1", "--format=%s"]), "My chosen task name");
    assert_eq!(f.git(&["branch", "--show-current"]), "feat/task");
    assert_eq!(
        std::fs::read_to_string(f.root.join("calls")).unwrap(),
        calls
    );
}

#[test]
fn preview_leaves_real_index_untouched_and_records_all_nonignored_changes() {
    let f = Fixture::new();
    f.edit();
    std::fs::write(f.session.worktree.join(".gitignore"), "secret\n").unwrap();
    std::fs::write(f.session.worktree.join("secret"), "do not publish").unwrap();
    std::fs::write(f.session.worktree.join("new file"), "new\n").unwrap();
    let before = f.git(&["write-tree"]);
    let p = f.preview();
    let calls = std::fs::read_to_string(f.root.join("calls")).unwrap();
    assert!(calls.contains("repo view github.com/test/repo --json nameWithOwner"));
    assert!(!calls.contains("repo view --repo"));
    assert_eq!(p.target.as_deref(), Some("dev"));
    assert_eq!(p.files, [".gitignore", "new file", "tracked"]);
    assert_eq!(f.git(&["write-tree"]), before);
    assert_eq!(f.git(&["log", "-1", "--format=%s"]), "base");
}
#[test]
fn confirmed_publish_commits_pushes_and_targets_recorded_dev() {
    let f = Fixture::new();
    f.edit();
    let p = f.preview();
    let published = f.publish(&p).unwrap();
    assert_eq!(published.url, "https://github.com/test/repo/pull/1");
    assert_eq!(published.repository, "github.com/test/repo");
    assert_eq!(published.number, Some(1));
    assert_eq!(published.head, f.git(&["rev-parse", "HEAD"]));
    assert_eq!(f.git(&["log", "-1", "--format=%s"]), "Add useful feature");
    assert!(f.git(&["status", "--porcelain"]).is_empty());
    let remote_head = text(
        git(
            Path::new("/usr/bin/git"),
            "/usr/bin:/bin",
            &f.remote,
            &["rev-parse", "refs/heads/feat/task"],
        )
        .unwrap(),
    );
    assert_eq!(remote_head, f.git(&["rev-parse", "HEAD"]));
    // The card resumes watching checks when this local ref moves.
    assert_eq!(
        worktree::pushed_head(&f.git, "/usr/bin:/bin", &f.session.worktree, "feat/task").unwrap(),
        Some(remote_head)
    );
    assert_eq!(
        worktree::pushed_head(&f.git, "/usr/bin:/bin", &f.session.worktree, "missing").unwrap(),
        None
    );
    let calls = std::fs::read_to_string(f.root.join("calls")).unwrap();
    assert!(calls.contains("pr create --repo github.com/test/repo --head feat/task --base dev --title Add useful feature --fill"));
    assert!(!calls.contains("merge"));
    assert!(f.session.worktree.exists());
}
#[test]
fn stale_preview_refuses_before_staging_or_committing() {
    let f = Fixture::new();
    f.edit();
    let p = f.preview();
    std::fs::write(f.session.worktree.join("tracked"), "different\n").unwrap();
    let before = f.git(&["write-tree"]);
    assert!(
        f.publish(&p)
            .unwrap_err()
            .to_string()
            .contains("changed after")
    );
    assert_eq!(f.git(&["write-tree"]), before);
    assert_eq!(f.git(&["rev-parse", "HEAD"]), p.head);
}
#[test]
fn missing_recorded_base_requires_explicit_target() {
    let mut f = Fixture::new();
    for reference in [None, Some("refs/heads/deleted".into())] {
        f.session.base_ref = reference;
        assert_eq!(f.preview().target, None);
    }
    let p = f.preview();
    assert!(
        publish(
            &f.git,
            &f.gh,
            "/usr/bin:/bin",
            &f.session,
            &p,
            "",
            "title",
            None
        )
        .is_err()
    );
}
#[test]
fn existing_pr_is_reused_without_duplicate_creation() {
    let f = Fixture::new();
    f.edit();
    std::fs::write(f.root.join("existing"), "").unwrap();
    let published = f.publish(&f.preview()).unwrap();
    assert_eq!(published.number, Some(1));
    assert_eq!(published.head, f.git(&["rev-parse", "HEAD"]));
    assert!(
        !std::fs::read_to_string(f.root.join("calls"))
            .unwrap()
            .contains("pr create")
    );
}
#[test]
fn same_named_branch_from_a_fork_is_not_reused() {
    let f = Fixture::new();
    f.edit();
    std::fs::write(f.root.join("existing"), "").unwrap();
    let script = std::fs::read_to_string(&f.gh)
        .unwrap()
        .replace("\"login\":\"test\"", "\"login\":\"fork\"");
    std::fs::write(&f.gh, script).unwrap();
    f.publish(&f.preview()).unwrap();
    assert!(
        std::fs::read_to_string(f.root.join("calls"))
            .unwrap()
            .contains("pr create")
    );
}
#[test]
fn pr_failure_keeps_commit_and_push_and_retry_does_not_commit_twice() {
    let f = Fixture::new();
    f.edit();
    std::fs::write(f.root.join("fail"), "").unwrap();
    assert!(f.publish(&f.preview()).is_err());
    let head = f.git(&["rev-parse", "HEAD"]);
    assert_eq!(f.git(&["log", "-1", "--format=%s"]), "Add useful feature");
    std::fs::remove_file(f.root.join("fail")).unwrap();
    f.publish(&f.preview()).unwrap();
    assert_eq!(f.git(&["rev-parse", "HEAD"]), head);
}
#[test]
fn commit_hook_failure_keeps_work_and_does_not_push() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    f.edit();
    let hook = f.session.repo.join(".git/hooks/pre-commit");
    std::fs::write(&hook, "#!/bin/sh\necho 'tests failed' >&2\nexit 1\n").unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    let p = f.preview();
    assert!(
        f.publish(&p)
            .unwrap_err()
            .to_string()
            .contains("tests failed")
    );
    assert_eq!(f.git(&["rev-parse", "HEAD"]), p.head);
    assert!(!f.git(&["diff", "--cached", "--name-only"]).is_empty());
}
#[test]
fn changed_remote_and_unknown_target_refuse_without_commit() {
    let f = Fixture::new();
    f.edit();
    let p = f.preview();
    assert!(
        publish(
            &f.git,
            &f.gh,
            "/usr/bin:/bin",
            &f.session,
            &p,
            "not-a-branch",
            "title",
            None,
        )
        .is_err()
    );
    f.git(&[
        "remote",
        "set-url",
        "origin",
        "https://github.com/test/other.git",
    ]);
    assert!(f.publish(&p).is_err());
    assert_eq!(f.git(&["rev-parse", "HEAD"]), p.head);
}

#[test]
fn recorded_main_also_targets_main_not_dev() {
    let mut f = Fixture::new();
    f.session.base_ref = Some("refs/remotes/origin/main".into());
    f.edit();
    let p = f.preview();
    assert_eq!(p.target.as_deref(), Some("main"));
    publish(
        &f.git,
        &f.gh,
        "/usr/bin:/bin",
        &f.session,
        &p,
        "main",
        &p.title,
        None,
    )
    .unwrap();
    assert!(
        std::fs::read_to_string(f.root.join("calls"))
            .unwrap()
            .contains("--base main --title Add useful feature")
    );
}
#[test]
fn publish_now_uses_the_dialog_defaults() {
    let f = Fixture::new();
    f.edit();
    let published = publish_now(&f.git, &f.gh, "/usr/bin:/bin", &f.session, None, None).unwrap();
    assert_eq!(published.url, "https://github.com/test/repo/pull/1");
    assert_eq!(published.head, f.git(&["rev-parse", "HEAD"]));
    assert_eq!(f.git(&["log", "-1", "--format=%s"]), "Add useful feature");
    assert!(f.git(&["status", "--porcelain"]).is_empty());
    let calls = std::fs::read_to_string(f.root.join("calls")).unwrap();
    assert!(calls.contains(
        "pr create --repo github.com/test/repo --head feat/task --base dev --title Add useful feature --fill"
    ));
}
#[test]
fn publish_writes_a_dialog_body_instead_of_fill() {
    let f = Fixture::new();
    f.edit();
    let p = f.preview();
    let target = p.target.clone().unwrap();
    publish(
        &f.git,
        &f.gh,
        "/usr/bin:/bin",
        &f.session,
        &p,
        &target,
        &p.title,
        Some("Notes for review"),
    )
    .unwrap();
    let calls = std::fs::read_to_string(f.root.join("calls")).unwrap();
    assert!(calls.contains("--body Notes for review\n"));
    assert!(!calls.contains("--fill"));
}
#[test]
fn publish_now_takes_a_title_and_a_body_instead_of_fill() {
    let f = Fixture::new();
    f.edit();
    publish_now(
        &f.git,
        &f.gh,
        "/usr/bin:/bin",
        &f.session,
        Some("Lead title"),
        Some("Why it changed"),
    )
    .unwrap();
    assert_eq!(f.git(&["log", "-1", "--format=%s"]), "Lead title");
    let calls = std::fs::read_to_string(f.root.join("calls")).unwrap();
    assert!(calls.contains("--base dev --title Lead title --body Why it changed\n"));
    assert!(!calls.contains("--fill"));
}
#[test]
fn publish_now_ignores_a_blank_body() {
    let f = Fixture::new();
    f.edit();
    publish_now(
        &f.git,
        &f.gh,
        "/usr/bin:/bin",
        &f.session,
        None,
        Some("  \n"),
    )
    .unwrap();
    let calls = std::fs::read_to_string(f.root.join("calls")).unwrap();
    assert!(calls.contains("--title Add useful feature --fill"));
    assert!(!calls.contains("--body"));
}
#[test]
fn publish_now_refuses_a_bad_body_before_committing() {
    let f = Fixture::new();
    f.edit();
    let long = "x".repeat(MAX_PR_BODY_CHARS + 1);
    for body in ["nul\0inside", long.as_str()] {
        let err =
            publish_now(&f.git, &f.gh, "/usr/bin:/bin", &f.session, None, Some(body)).unwrap_err();
        assert!(err.to_string().contains("PR body"), "{err}");
    }
    assert_eq!(f.git(&["log", "-1", "--format=%s"]), "base");
    assert!(
        !std::fs::read_to_string(f.root.join("calls"))
            .unwrap()
            .contains("pr create")
    );
}
#[test]
fn publish_now_without_a_target_refuses_and_changes_nothing() {
    let mut f = Fixture::new();
    f.edit();
    for (reference, name) in [
        (None, None),
        (Some("refs/heads/deleted"), Some("deleted")),
        (Some("refs/remotes/origin/HEAD"), None),
    ] {
        f.session.base_ref = reference.map(str::to_string);
        assert_eq!(
            publish_now(&f.git, &f.gh, "/usr/bin:/bin", &f.session, None, None),
            Err(Error::PublishNoTarget(name.map(str::to_string)))
        );
    }
    assert_eq!(f.git(&["log", "-1", "--format=%s"]), "base");
    assert!(!f.git(&["status", "--porcelain"]).is_empty());
    let calls = std::fs::read_to_string(f.root.join("calls")).unwrap();
    assert!(!calls.contains("pr list") && !calls.contains("pr create"));
}
#[test]
fn switched_source_branch_cannot_publish_even_with_same_head_and_files() {
    let f = Fixture::new();
    f.edit();
    let p = f.preview();
    f.git(&["switch", "-c", "unrelated"]);
    assert!(
        f.publish(&p)
            .unwrap_err()
            .to_string()
            .contains("changed after")
    );
    assert_eq!(f.git(&["log", "-1", "--format=%s"]), "base");
}
#[test]
fn dirty_submodule_content_is_not_silently_omitted() {
    assert!(dirty_submodule(
        b"1 .M S.M. 160000 160000 160000 hash hash sub\0"
    ));
    assert!(dirty_submodule(
        b"1 .M S..U 160000 160000 160000 hash hash sub\0"
    ));
    assert!(!dirty_submodule(
        b"1 .M SC.. 160000 160000 160000 hash hash sub\0"
    ));
    assert!(!dirty_submodule(
        b"1 .M N... 100644 100644 100644 hash hash file\0? new file\0"
    ));
}
#[test]
fn unfinished_merge_refuses_before_staging() {
    let f = Fixture::new();
    f.edit();
    let marker = f.git(&["rev-parse", "--git-path", "MERGE_HEAD"]);
    std::fs::write(
        f.session.worktree.join(marker),
        f.git(&["rev-parse", "HEAD"]),
    )
    .unwrap();
    assert!(
        preview(&f.git, &f.gh, "/usr/bin:/bin", &f.session)
            .unwrap_err()
            .to_string()
            .contains("Finish or abort")
    );
    assert!(f.git(&["diff", "--cached", "--name-only"]).is_empty());
}
#[test]
fn subprocess_timeout_is_bounded_even_with_inherited_pipes() {
    let mut command = Command::new("/bin/sh");
    command.args(["-c", "sleep 10 & wait"]);
    let started = Instant::now();
    assert!(run_with_timeout(command, Duration::from_millis(100)).is_err());
    assert!(started.elapsed() < Duration::from_secs(3));
}
#[test]
fn recorded_base_only_never_default_or_current_project() {
    assert_eq!(
        base_name(Some("refs/remotes/origin/dev")),
        Some("dev".into())
    );
    assert_eq!(base_name(Some("refs/heads/main")), Some("main".into()));
    assert_eq!(
        base_name(Some("refs/heads/release/v2")),
        Some("release/v2".into())
    );
    assert_eq!(base_name(None), None);
    assert_eq!(base_name(Some("refs/remotes/origin/HEAD")), None);
    assert_eq!(base_name(Some("refs/remotes/upstream/dev")), None);
}
#[test]
fn explicit_remote_mapping() {
    for url in [
        "git@github.com:org/repo.git",
        "https://github.com/org/repo.git",
        "ssh://git@github.com/org/repo.git",
    ] {
        assert_eq!(repository(url).unwrap(), "github.com/org/repo");
    }
    assert_eq!(
        repository("https://ghe.example/org/repo").unwrap(),
        "ghe.example/org/repo"
    );
    for url in [
        "/tmp/repo",
        "file:///repo",
        "https://github.com/repo",
        "https://user:secret@github.com/org/repo",
        "-bad",
    ] {
        assert!(repository(url).is_err());
    }
}
#[test]
fn pr_number_comes_only_from_a_pull_url() {
    assert_eq!(pr_number("https://github.com/org/repo/pull/42"), Some(42));
    assert_eq!(pr_number("https://ghe.example/org/repo/pull/7/"), Some(7));
    for url in [
        "https://github.com/org/repo",
        "https://github.com/org/repo/pull/",
        "https://github.com/org/repo/pull/0",
        "https://github.com/org/repo/pull/12/files",
    ] {
        assert_eq!(pr_number(url), None, "{url}");
    }
}
fn items(json: &str) -> Vec<RollupItem> {
    serde_json::from_str(json).unwrap()
}
#[test]
fn checks_fail_fast_and_finish_only_when_every_check_is_done() {
    use ChecksState::*;
    assert_eq!(classify(&[]), NoChecks);
    let run = |status: &str, conclusion: &str| {
        format!(r#"{{"__typename":"CheckRun","status":"{status}","conclusion":"{conclusion}"}}"#)
    };
    let done = run("COMPLETED", "SUCCESS");
    let skipped = run("COMPLETED", "SKIPPED");
    let cancelled = run("COMPLETED", "CANCELLED");
    let running = run("IN_PROGRESS", "");
    let failed = run("COMPLETED", "FAILURE");
    let status = |state: &str| format!(r#"{{"__typename":"StatusContext","state":"{state}"}}"#);
    let case = |parts: &[&str]| classify(&items(&format!("[{}]", parts.join(","))));
    assert_eq!(case(&[&done, &skipped, &cancelled]), Passed);
    assert_eq!(case(&[&done, &running]), Pending);
    assert_eq!(case(&[&running, &failed]), Failed);
    assert_eq!(case(&[&run("COMPLETED", "TIMED_OUT")]), Failed);
    assert_eq!(case(&[&run("QUEUED", "")]), Pending);
    assert_eq!(case(&[&done, &status("SUCCESS")]), Passed);
    assert_eq!(case(&[&status("PENDING")]), Pending);
    assert_eq!(case(&[&done, &status("ERROR")]), Failed);
}
#[test]
fn checks_read_one_pr_view_scoped_to_the_repository() {
    use std::os::unix::fs::PermissionsExt;
    let root = std::env::temp_dir().join(format!(
        "shika-checks-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let gh = root.join("gh");
    std::fs::write(
        &gh,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}/calls'\necho '{{\"headRefOid\":\"abc\",\"state\":\"MERGED\",\"statusCheckRollup\":[{{\"__typename\":\"CheckRun\",\"status\":\"COMPLETED\",\"conclusion\":\"FAILURE\"}}]}}'\n",
            root.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    let checks = checks(&gh, "/usr/bin:/bin", &root, "github.com/test/repo", 42).unwrap();
    assert_eq!(
        checks,
        PrChecks {
            head: "abc".into(),
            open: false,
            state: ChecksState::Failed,
        }
    );
    let calls = std::fs::read_to_string(root.join("calls")).unwrap();
    assert_eq!(
        calls.trim(),
        "pr view 42 --repo github.com/test/repo --json headRefOid,state,statusCheckRollup"
    );
    let _ = std::fs::remove_dir_all(&root);
}
