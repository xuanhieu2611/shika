use super::*;
use crate::diff::Collapse;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

struct Scratch {
    path: PathBuf,
}

impl Scratch {
    fn new() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("shika-worktree-{nanos}-{n}"));
        fs::create_dir_all(&path).unwrap();
        Self { path }
    }

    fn repo(&self, name: &str) -> PathBuf {
        let dir = self.path.join(name);
        fs::create_dir_all(&dir).unwrap();
        let status = Command::new("git")
            .arg("init")
            .current_dir(&dir)
            .status()
            .unwrap();
        assert!(status.success());
        let status = Command::new("git")
            .arg("-C")
            .arg(&dir)
            .args(["commit", "--allow-empty", "-m", "init"])
            .env("GIT_AUTHOR_NAME", "Shika")
            .env("GIT_AUTHOR_EMAIL", "shika@example.com")
            .env("GIT_COMMITTER_NAME", "Shika")
            .env("GIT_COMMITTER_EMAIL", "shika@example.com")
            .status()
            .unwrap();
        assert!(status.success(), "empty commit failed");
        dir
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn git() -> PathBuf {
    PathBuf::from("git")
}

fn porcelain(repo: &Path) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["status", "--porcelain"])
        .output()
        .unwrap();
    assert!(output.status.success());
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn git_ok(repo: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .env("GIT_AUTHOR_NAME", "Shika")
        .env("GIT_AUTHOR_EMAIL", "shika@example.com")
        .env("GIT_COMMITTER_NAME", "Shika")
        .env("GIT_COMMITTER_EMAIL", "shika@example.com")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn known_branches_lists_local_and_origin_once_and_skips_head() {
    let scratch = Scratch::new();
    let repo = scratch.path.join("demo");
    fs::create_dir_all(&repo).unwrap();
    git_ok(&repo, &["init", "-b", "main"]);
    git_ok(&repo, &["commit", "--allow-empty", "-m", "init"]);
    git_ok(&repo, &["branch", "staging"]);
    let remote = scratch.path.join("remote.git");
    fs::create_dir_all(&remote).unwrap();
    git_ok(&remote, &["init", "--bare"]);
    git_ok(
        &repo,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );
    git_ok(&repo, &["push", "origin", "main"]);
    git_ok(&repo, &["remote", "set-head", "origin", "main"]);
    git_ok(&repo, &["switch", "-c", "dev"]);
    git_ok(&repo, &["commit", "--allow-empty", "-m", "dev"]);
    git_ok(&repo, &["push", "origin", "dev"]);
    git_ok(&repo, &["switch", "main"]);
    git_ok(&repo, &["branch", "-D", "dev"]);

    let raw = Command::new("git")
        .arg("-C")
        .arg(&repo)
        .args(["for-each-ref", "--format=%(refname)", "refs/remotes/origin"])
        .output()
        .unwrap();
    let raw = String::from_utf8_lossy(&raw.stdout);
    assert!(
        raw.contains("refs/remotes/origin/HEAD"),
        "origin/HEAD was not there to omit: {raw}"
    );

    let found = known_branches(&git(), "", &repo).unwrap();
    assert_eq!(found.names, vec!["dev", "main", "staging"]);
    assert_eq!(found.checked_out.as_deref(), Some("main"));
    assert!(found.has_origin);
    assert!(!found.names.iter().any(|name| name == "HEAD"));
}

fn rev(dir: &Path, rev: &str) -> String {
    rev_parse(&git(), "", dir, rev).unwrap()
}

#[test]
fn a_missing_lead_worktree_is_created_detached_from_the_start() {
    let scratch = Scratch::new();
    let repo = scratch.repo("demo");
    let before = Command::new("git")
        .arg("-C")
        .arg(&repo)
        .args(["branch", "--format=%(refname)"])
        .output()
        .unwrap()
        .stdout;

    let outcome = prepare_lead_worktree(&git(), "", &repo, "main").unwrap();

    assert_eq!(outcome, LeadWorktree::Created);
    let path = lead_worktree_path(&repo);
    assert_eq!(path, repo.join(".worktrees").join("shika-lead"));
    assert_eq!(rev(&path, "HEAD"), rev(&repo, "main"));
    assert_eq!(head_branch(&git(), "", &path).unwrap(), None);
    let after = Command::new("git")
        .arg("-C")
        .arg(&repo)
        .args(["branch", "--format=%(refname)"])
        .output()
        .unwrap()
        .stdout;
    assert_eq!(before, after, "no branch is created");
    // The start `HEAD` means the main checkout's, never the Lead's own.
    git_ok(&repo, &["commit", "--allow-empty", "-m", "second"]);
    assert!(matches!(
        prepare_lead_worktree(&git(), "", &repo, "HEAD").unwrap(),
        LeadWorktree::Refreshed { .. }
    ));
    assert_eq!(rev(&path, "HEAD"), rev(&repo, "HEAD"));
}

#[test]
fn a_clean_lead_worktree_moves_to_the_latest_base_and_says_from_and_to() {
    let scratch = Scratch::new();
    let repo = scratch.repo("demo");
    prepare_lead_worktree(&git(), "", &repo, "main").unwrap();
    let path = lead_worktree_path(&repo);
    let first = rev(&repo, "main");
    assert_eq!(
        prepare_lead_worktree(&git(), "", &repo, "main").unwrap(),
        LeadWorktree::Unchanged
    );

    git_ok(&repo, &["commit", "--allow-empty", "-m", "second"]);
    let second = rev(&repo, "main");
    assert_eq!(
        prepare_lead_worktree(&git(), "", &repo, "main").unwrap(),
        LeadWorktree::Refreshed {
            from: first[..10].to_string(),
            to: second[..10].to_string(),
        }
    );
    assert_eq!(rev(&path, "HEAD"), second);
    assert_eq!(head_branch(&git(), "", &path).unwrap(), None);
}

#[test]
fn a_dirty_lead_worktree_is_left_exactly_as_it_is() {
    let scratch = Scratch::new();
    let repo = scratch.repo("demo");
    prepare_lead_worktree(&git(), "", &repo, "main").unwrap();
    let path = lead_worktree_path(&repo);
    let first = rev(&path, "HEAD");
    git_ok(&repo, &["commit", "--allow-empty", "-m", "second"]);

    for dirty in ["untracked.txt", "README"] {
        fs::write(path.join(dirty), "notes\n").unwrap();
        if dirty == "README" {
            git_ok(&path, &["add", "README"]);
        }
        assert_eq!(
            prepare_lead_worktree(&git(), "", &repo, "main").unwrap(),
            LeadWorktree::KeptDirty,
            "{dirty}"
        );
        assert_eq!(rev(&path, "HEAD"), first, "{dirty}");
        assert!(path.join(dirty).exists(), "{dirty}");
        git_ok(&path, &["reset", "-q"]);
        fs::remove_file(path.join(dirty)).unwrap();
    }
}

#[test]
fn a_lead_folder_that_is_not_this_repos_worktree_is_an_error_and_is_never_deleted() {
    let scratch = Scratch::new();
    let repo = scratch.repo("demo");
    let path = lead_worktree_path(&repo);

    // A plain folder with work in it.
    fs::create_dir_all(&path).unwrap();
    fs::write(path.join("mine.txt"), "keep\n").unwrap();
    assert_eq!(
        prepare_lead_worktree(&git(), "", &repo, "main"),
        Err(Error::LeadWorktreeInvalid(path.clone()))
    );
    assert!(path.join("mine.txt").exists());

    // A file, a symlink to a real worktree, and another repo's worktree.
    fs::remove_dir_all(&path).unwrap();
    fs::write(&path, "a file").unwrap();
    assert!(matches!(
        prepare_lead_worktree(&git(), "", &repo, "main"),
        Err(Error::LeadWorktreeInvalid(_))
    ));
    fs::remove_file(&path).unwrap();

    let other = scratch.repo("other");
    let foreign = scratch.path.join("foreign");
    git_ok(
        &other,
        &["worktree", "add", "--detach", foreign.to_str().unwrap()],
    );
    std::os::unix::fs::symlink(&foreign, &path).unwrap();
    assert!(matches!(
        prepare_lead_worktree(&git(), "", &repo, "main"),
        Err(Error::LeadWorktreeInvalid(_))
    ));
    fs::remove_file(&path).unwrap();
    git_ok(&repo, &["worktree", "add", "--detach", "../real", "main"]);
    std::os::unix::fs::symlink(scratch.path.join("real"), &path).unwrap();
    assert!(matches!(
        prepare_lead_worktree(&git(), "", &repo, "main"),
        Err(Error::LeadWorktreeInvalid(_))
    ));
    fs::remove_file(&path).unwrap();
    fs::create_dir_all(&path).unwrap();
    fs::write(
        path.join(".git"),
        "gitdir: ../../../other/.git/worktrees/x\n",
    )
    .unwrap();
    assert!(matches!(
        prepare_lead_worktree(&git(), "", &repo, "main"),
        Err(Error::LeadWorktreeInvalid(_))
    ));
    assert!(path.join(".git").exists());
    assert!(foreign.exists());
}

#[test]
fn stale_lead_registration_never_authorizes_git_in_another_checkout() {
    for damage in [
        "missing", "garbage", "main", "foreign", "sibling", "symlink",
    ] {
        let scratch = Scratch::new();
        // A colon makes GIT_CEILING_DIRECTORIES insufficient by itself.
        let repo = scratch.repo("demo:lead");
        git_ok(&repo, &["branch", "dev"]);
        git_ok(&repo, &["commit", "--allow-empty", "-m", "main only"]);
        prepare_lead_worktree(&git(), "", &repo, "dev").unwrap();
        let path = lead_worktree_path(&repo);
        let gitfile = path.join(".git");
        let sibling = scratch.path.join("sibling");
        git_ok(
            &repo,
            &[
                "worktree",
                "add",
                "--detach",
                sibling.to_str().unwrap(),
                "main",
            ],
        );
        let foreign = scratch.repo("foreign");
        let before = rev(&repo, "HEAD");
        let sibling_before = rev(&sibling, "HEAD");
        let foreign_before = rev(&foreign, "HEAD");
        fs::remove_file(&gitfile).unwrap();
        match damage {
            "missing" => {}
            "garbage" => fs::write(&gitfile, "not a gitfile\n").unwrap(),
            "main" => fs::write(
                &gitfile,
                format!("gitdir: {}\n", repo.join(".git").display()),
            )
            .unwrap(),
            "foreign" => fs::write(
                &gitfile,
                format!("gitdir: {}\n", foreign.join(".git").display()),
            )
            .unwrap(),
            "sibling" => fs::write(&gitfile, fs::read(sibling.join(".git")).unwrap()).unwrap(),
            "symlink" => std::os::unix::fs::symlink(sibling.join(".git"), &gitfile).unwrap(),
            _ => unreachable!(),
        }
        assert!(is_registered_worktree(&git(), "", &repo, &path).unwrap());
        assert!(
            !is_verified_worktree(&git(), "", &repo, &path).unwrap(),
            "{damage}"
        );
        assert_eq!(
            prepare_lead_worktree(&git(), "", &repo, "dev"),
            Err(Error::LeadWorktreeInvalid(path.clone())),
            "{damage}"
        );
        assert!(path.exists());
        assert_eq!(rev(&repo, "HEAD"), before, "{damage}");
        assert_eq!(
            head_branch(&git(), "", &repo).unwrap().as_deref(),
            Some("main"),
            "{damage}"
        );
        assert_eq!(rev(&sibling, "HEAD"), sibling_before, "{damage}");
        assert_eq!(rev(&foreign, "HEAD"), foreign_before, "{damage}");
    }
}

#[test]
fn a_lead_worktree_whose_folder_was_deleted_is_created_again() {
    let scratch = Scratch::new();
    let repo = scratch.repo("demo");
    prepare_lead_worktree(&git(), "", &repo, "main").unwrap();
    let path = lead_worktree_path(&repo);
    fs::remove_dir_all(&path).unwrap();
    assert_eq!(
        prepare_lead_worktree(&git(), "", &repo, "main").unwrap(),
        LeadWorktree::Created
    );
    assert!(path.join(".git").exists());
}

#[test]
fn a_start_that_does_not_resolve_creates_nothing() {
    let scratch = Scratch::new();
    let repo = scratch.repo("demo");
    assert!(matches!(
        prepare_lead_worktree(&git(), "", &repo, "refs/remotes/origin/gone"),
        Err(Error::CreateWorktree(_))
    ));
    assert!(!lead_worktree_path(&repo).exists());
}

#[test]
fn head_branch_distinguishes_detached_head_from_git_failure() {
    let scratch = Scratch::new();
    let repo = scratch.repo("demo");
    assert!(head_branch(&git(), "", &repo).unwrap().is_some());
    assert!(
        Command::new("git")
            .current_dir(&repo)
            .args(["checkout", "--detach"])
            .status()
            .unwrap()
            .success()
    );
    assert_eq!(head_branch(&git(), "", &repo).unwrap(), None);
    assert!(matches!(
        head_branch(&git(), "", &scratch.path),
        Err(Error::GitStatus(Some(_)))
    ));
}

#[test]
fn slugs_collapse_trim_and_stop_at_a_word() {
    assert_eq!(
        branch_slug("  Fix / Login__Flow!  ", "").as_deref(),
        Some("fix-login-flow")
    );
    assert_eq!(branch_slug("?!", ""), None);
    assert_eq!(branch_slug(&"A".repeat(80), ""), Some("a".repeat(48)));
    assert_eq!(
        branch_slug(&format!("{} xx", "A".repeat(47)), ""),
        Some("a".repeat(47))
    );
    assert_eq!(
        branch_slug(
            "i'm not sure if this is expected but i tried to use Shika",
            ""
        )
        .as_deref(),
        Some("i-m-not-sure-if-this-is-expected-but-i-tried-to")
    );
}

#[test]
fn slugs_leave_out_the_project_name() {
    assert_eq!(
        branch_slug("Shika background opacity and blur", "shika").as_deref(),
        Some("background-opacity-and-blur")
    );
    assert_eq!(
        branch_slug("Fix Sample Project tracker", "sample-project").as_deref(),
        Some("fix-tracker")
    );
    // Only whole words, and never the whole name.
    assert_eq!(
        branch_slug("Shikari support", "shika").as_deref(),
        Some("shikari-support")
    );
    assert_eq!(branch_slug("Shika", "shika").as_deref(), Some("shika"));
    assert_eq!(
        branch_slug("Setting Placement Query", "shika").as_deref(),
        Some("setting-placement-query")
    );
}

#[test]
fn prefixes_are_made_safe_for_git() {
    assert_eq!(normalize_prefix(""), "");
    assert_eq!(normalize_prefix("  "), "");
    assert_eq!(normalize_prefix("dev"), "dev/");
    assert_eq!(normalize_prefix("dev/"), "dev/");
    assert_eq!(normalize_prefix("/dev//feat/"), "dev/feat/");
    assert_eq!(normalize_prefix("dev-"), "dev-");
    assert_eq!(normalize_prefix("dev_"), "dev_");
    assert_eq!(normalize_prefix("De v~^:?*[\\"), "Dev/");
    assert_eq!(normalize_prefix("-.hidden/x..y/z.lock/"), "hidden/x.y/z/");
    assert_eq!(normalize_prefix("./../"), "");
    for raw in ["dev", "a.b", "-x", "x.lock", "Sample User/", "@{x"] {
        let prefix = normalize_prefix(raw);
        if !prefix.is_empty() {
            assert!(
                is_valid_branch(&git(), "", Path::new("."), &format!("{prefix}fix")).unwrap(),
                "{raw} gave {prefix}"
            );
        }
    }
}

#[test]
fn rename_skips_names_taken_locally_or_on_a_remote() {
    let scratch = Scratch::new();
    let repo = scratch.repo("demo");
    let draft = create_draft(&git(), "", &repo, "one", "HEAD").unwrap();
    let run = |args: &[&str]| {
        let status = Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    };
    run(&["branch", "fix-login"]);
    run(&["update-ref", "refs/remotes/origin/fix-login-2", "HEAD"]);
    let renamed = rename_branch(&git(), "", &draft.path, &draft.branch, "fix-login").unwrap();
    assert_eq!(renamed, "fix-login-3");
    assert_eq!(
        head_branch(&git(), "", &draft.path).unwrap().as_deref(),
        Some("fix-login-3")
    );
    // Asking for the name it already has changes nothing.
    assert_eq!(
        rename_branch(&git(), "", &draft.path, &renamed, "fix-login-3").unwrap(),
        "fix-login-3"
    );
    assert!(!is_published(&git(), "", &draft.path, &renamed).unwrap());
    run(&["update-ref", "refs/remotes/origin/fix-login-3", "HEAD"]);
    assert!(is_published(&git(), "", &draft.path, &renamed).unwrap());

    let prefixed = rename_branch(&git(), "", &draft.path, &renamed, "dev/fix-login").unwrap();
    assert_eq!(prefixed, "dev/fix-login");
    assert!(!is_published(&git(), "", &draft.path, &prefixed).unwrap());
    remove_draft(&git(), "", &repo, &draft.path, &prefixed, true).unwrap();
}

#[test]
fn exclude_is_written_once_and_hides_the_directory() {
    let scratch = Scratch::new();
    let repo = scratch.repo("demo");
    ensure_excluded(&git(), "", &repo).unwrap();
    ensure_excluded(&git(), "", &repo).unwrap();
    let exclude = exclude_file(&git(), "", &repo).unwrap();
    let text = fs::read_to_string(exclude).unwrap();
    assert_eq!(text.matches(".worktrees/").count(), 1);

    fs::create_dir_all(repo.join(".worktrees")).unwrap();
    fs::write(repo.join(".worktrees").join("note"), "hi").unwrap();
    assert!(
        !porcelain(&repo).contains(".worktrees"),
        "status was {}",
        porcelain(&repo)
    );
}

#[test]
fn draft_worktree_stays_out_of_main_status_and_can_be_removed() {
    let scratch = Scratch::new();
    let repo = scratch.repo("demo");
    let draft = create_draft(&git(), "", &repo, "one", "HEAD").unwrap();

    assert_eq!(draft.branch, "shika-draft-one");
    assert!(draft.path.join(".git").exists());
    assert!(
        !porcelain(&repo).contains(".worktrees"),
        "status was {}",
        porcelain(&repo)
    );

    let listed = Command::new("git")
        .arg("-C")
        .arg(&repo)
        .args(["worktree", "list", "--porcelain"])
        .output()
        .unwrap();
    let listed = String::from_utf8_lossy(&listed.stdout);
    assert!(listed.contains(&draft.path.display().to_string()));

    assert!(!is_dirty(&git(), "", &draft.path).unwrap());
    remove_draft(&git(), "", &repo, &draft.path, &draft.branch, false).unwrap();
    assert!(!draft.path.exists());
    let listed = Command::new("git")
        .arg("-C")
        .arg(&repo)
        .args(["worktree", "list", "--porcelain"])
        .output()
        .unwrap();
    let listed = String::from_utf8_lossy(&listed.stdout);
    assert!(!listed.contains("shika-draft-one"));
    let branches = Command::new("git")
        .arg("-C")
        .arg(&repo)
        .args(["branch", "--list", "shika-draft-one"])
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&branches.stdout).trim().is_empty());
}

#[test]
fn a_dirty_worktree_is_kept_until_removal_is_forced() {
    let scratch = Scratch::new();
    let repo = scratch.repo("demo");
    let draft = create_draft(&git(), "", &repo, "one", "HEAD").unwrap();
    fs::write(draft.path.join("note.txt"), "wip\n").unwrap();

    assert!(is_dirty(&git(), "", &draft.path).unwrap());
    let refused = remove_draft(&git(), "", &repo, &draft.path, &draft.branch, false);
    assert!(
        matches!(refused, Err(Error::WorktreeHasChanges(_))),
        "{refused:?}"
    );
    assert!(draft.path.exists());

    remove_draft(&git(), "", &repo, &draft.path, &draft.branch, true).unwrap();
    assert!(!draft.path.exists());
    let branches = Command::new("git")
        .arg("-C")
        .arg(&repo)
        .args(["branch", "--list", "shika-draft-one"])
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&branches.stdout).trim().is_empty());
}

fn run(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "Shika")
        .env("GIT_AUTHOR_EMAIL", "shika@example.com")
        .env("GIT_COMMITTER_NAME", "Shika")
        .env("GIT_COMMITTER_EMAIL", "shika@example.com")
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?} failed");
}

fn commit_all(dir: &Path, message: &str) {
    run(dir, &["add", "-A"]);
    run(dir, &["commit", "-m", message]);
}

/// A repository with a three-line tracked file, and a draft made from it.
fn repo_with_draft(scratch: &Scratch) -> (PathBuf, Draft) {
    let repo = scratch.repo("demo");
    fs::write(repo.join("a.txt"), "one\ntwo\nthree\n").unwrap();
    commit_all(&repo, "base");
    let draft = create_draft(&git(), "", &repo, "one", "HEAD").unwrap();
    (repo, draft)
}

fn task(repo: &Path) -> Baseline<'_> {
    Baseline::Task {
        repo,
        recorded: None,
    }
}

fn stat(repo: &Path, draft: &Draft) -> crate::DiffStat {
    diff_stat(&git(), "", &draft.path, task(repo)).unwrap()
}

#[test]
fn a_head_baseline_counts_only_uncommitted_work_in_the_checkout() {
    let scratch = Scratch::new();
    let repo = scratch.repo("demo");
    // Ignore Shika's own folder, as New does, so tasks never show here.
    fs::write(repo.join(".git/info/exclude"), ".worktrees/\n").unwrap();
    fs::write(repo.join("a.txt"), "one\ntwo\n").unwrap();
    commit_all(&repo, "the author's own commit");
    let draft = create_draft(&git(), "", &repo, "one", "HEAD").unwrap();
    fs::write(draft.path.join("task.txt"), "task\n").unwrap();
    assert_eq!(
        diff_stat(&git(), "", &repo, Baseline::Head).unwrap(),
        counts(0, 0, 0),
        "commits are the author's, and a task's files are not the checkout's"
    );
    fs::write(repo.join("a.txt"), "one\n").unwrap();
    fs::write(repo.join("new.txt"), "x\ny\n").unwrap();
    assert_eq!(
        diff_stat(&git(), "", &repo, Baseline::Head).unwrap(),
        counts(2, 2, 1)
    );
    let changes = session_diff(&git(), "", &repo, Baseline::Head).unwrap();
    assert_eq!(changes.files.len(), 2);
    assert!(changes.files.iter().all(|file| file.key.base == "HEAD"));
    git_ok(&repo, &["add", "-A"]);
    git_ok(&repo, &["commit", "-m", "the author commits"]);
    assert_eq!(
        diff_stat(&git(), "", &repo, Baseline::Head).unwrap(),
        counts(0, 0, 0),
        "after the author commits, the stat goes away"
    );
    assert_eq!(porcelain(&repo), "", "reading never staged anything");
}

#[test]
fn checkout_head_names_the_branch_or_the_detached_commit() {
    let scratch = Scratch::new();
    let repo = scratch.repo("demo");
    git_ok(&repo, &["checkout", "-b", "dev"]);
    assert_eq!(
        checkout_head(&git(), "", &repo).unwrap(),
        CheckoutHead::Branch("dev".into())
    );
    git_ok(&repo, &["checkout", "--detach"]);
    let short = Command::new("git")
        .arg("-C")
        .arg(&repo)
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .unwrap();
    let short = String::from_utf8_lossy(&short.stdout).trim().to_string();
    let head = checkout_head(&git(), "", &repo).unwrap();
    assert_eq!(head, CheckoutHead::Detached(short.clone()));
    assert_eq!(head.label(), short);
    // A folder inside the repository is not a checkout of its own.
    let nested = repo.join("src");
    fs::create_dir_all(&nested).unwrap();
    assert!(checkout_head(&git(), "", &nested).is_err());
}

fn counts(files: usize, insertions: usize, deletions: usize) -> crate::DiffStat {
    crate::DiffStat {
        files,
        insertions,
        deletions,
    }
}

#[test]
fn a_clean_branch_changed_nothing() {
    let scratch = Scratch::new();
    let (repo, draft) = repo_with_draft(&scratch);
    assert_eq!(stat(&repo, &draft), crate::DiffStat::default());
}

#[test]
fn committed_work_counts_from_where_the_branch_left() {
    let scratch = Scratch::new();
    let (repo, draft) = repo_with_draft(&scratch);
    fs::write(draft.path.join("a.txt"), "one\n2\nthree\nfour\n").unwrap();
    commit_all(&draft.path, "task");
    // Later work on the default branch is not the task's.
    fs::write(repo.join("main.txt"), "main\n").unwrap();
    commit_all(&repo, "main moves on");
    assert_eq!(stat(&repo, &draft), counts(1, 2, 1));
}

#[test]
fn uncommitted_edits_count() {
    let scratch = Scratch::new();
    let (repo, draft) = repo_with_draft(&scratch);
    fs::write(draft.path.join("a.txt"), "one\ntwo\nthree\nfour\nfive\n").unwrap();
    assert_eq!(stat(&repo, &draft), counts(1, 2, 0));
    // Staged and committed work add up with what is still in the tree.
    run(&draft.path, &["add", "a.txt"]);
    fs::write(draft.path.join("b.txt"), "b\n").unwrap();
    commit_all(&draft.path, "task");
    fs::write(draft.path.join("b.txt"), "b\nc\n").unwrap();
    assert_eq!(stat(&repo, &draft), counts(2, 4, 0));
}

#[test]
fn untracked_files_count_their_lines_without_touching_the_index() {
    let scratch = Scratch::new();
    let (repo, draft) = repo_with_draft(&scratch);
    fs::create_dir_all(draft.path.join("src")).unwrap();
    fs::write(draft.path.join("src/new.rs"), "fn a() {}\nfn b() {}\n").unwrap();
    fs::write(draft.path.join("tail.txt"), "no newline").unwrap();
    fs::write(draft.path.join("empty.txt"), "").unwrap();
    fs::write(draft.path.join(".gitignore"), "*.log\n").unwrap();
    fs::write(draft.path.join("build.log"), "ignored\n").unwrap();
    assert_eq!(stat(&repo, &draft), counts(4, 4, 0));
    let status = porcelain(&draft.path);
    assert!(status.contains("?? src/"), "{status}");
    assert!(!status.contains("A "), "{status}");
}

#[test]
fn binary_files_count_as_files_without_lines() {
    let scratch = Scratch::new();
    let (repo, draft) = repo_with_draft(&scratch);
    fs::write(draft.path.join("logo.png"), [0x89, b'P', 0, 0, b'\n', 1]).unwrap();
    commit_all(&draft.path, "binary");
    fs::write(draft.path.join("loose.bin"), [1, 0, b'\n', b'\n']).unwrap();
    assert_eq!(stat(&repo, &draft), counts(2, 0, 0));
}

#[test]
fn deletions_count() {
    let scratch = Scratch::new();
    let (repo, draft) = repo_with_draft(&scratch);
    fs::remove_file(draft.path.join("a.txt")).unwrap();
    assert_eq!(stat(&repo, &draft), counts(1, 0, 3));
    commit_all(&draft.path, "delete");
    assert_eq!(stat(&repo, &draft), counts(1, 0, 3));
}

#[test]
fn without_a_default_branch_only_uncommitted_work_counts() {
    let scratch = Scratch::new();
    let (repo, draft) = repo_with_draft(&scratch);
    fs::write(draft.path.join("b.txt"), "b\n").unwrap();
    commit_all(&draft.path, "task");
    // The main checkout on the task branch hides the default branch.
    run(&repo, &["branch", "-m", "trunk"]);
    run(
        &repo,
        &["checkout", "--ignore-other-worktrees", &draft.branch],
    );
    assert_eq!(stat(&repo, &draft), crate::DiffStat::default());
    fs::write(draft.path.join("b.txt"), "b\nc\n").unwrap();
    assert_eq!(stat(&repo, &draft), counts(1, 1, 0));
}

#[test]
fn journal_records_and_forgets_a_draft() {
    let scratch = Scratch::new();
    let journal = Journal::open(scratch.path.join("worktrees.json"));
    let entry = JournalEntry {
        project_id: "p1".into(),
        branch: "shika-draft-one".into(),
        path: "/tmp/demo/.worktrees/shika-draft-one".into(),
        base_ref: Some("refs/remotes/origin/dev".into()),
        started_by_lead: false,
        recovery: None,
    };
    journal.add(&entry).unwrap();
    let again = Journal::open(scratch.path.join("worktrees.json"));
    again
        .add(&JournalEntry {
            project_id: "p1".into(),
            branch: "shika-draft-two".into(),
            path: "/tmp/demo/.worktrees/shika-draft-two".into(),
            base_ref: None,
            started_by_lead: true,
            recovery: None,
        })
        .unwrap();
    again.remove_path(&entry.path).unwrap();
    let text = fs::read_to_string(scratch.path.join("worktrees.json")).unwrap();
    assert!(!text.contains("shika-draft-one"));
    assert!(text.contains("shika-draft-two"));
    assert!(text.contains("\"projectId\": \"p1\""), "{text}");
    // No base recorded leaves the field out, as in older files.
    assert!(!text.contains("baseRef"), "{text}");
    // Only a Lead-started worker writes the flag; older entries read false.
    assert_eq!(text.matches("\"startedByLead\": true").count(), 1, "{text}");
    assert_eq!(text.matches("startedByLead").count(), 1, "{text}");
    assert!(again.list().unwrap()[0].started_by_lead);
    let old: Vec<JournalEntry> =
        serde_json::from_str(r#"[{"projectId":"p","branch":"b","path":"/tmp/x/.worktrees/b"}]"#)
            .unwrap();
    assert!(!old[0].started_by_lead);
    assert_eq!(again.list().unwrap().len(), 1);
    again.add(&entry).unwrap();
    let text = fs::read_to_string(scratch.path.join("worktrees.json")).unwrap();
    assert!(
        text.contains("\"baseRef\": \"refs/remotes/origin/dev\""),
        "{text}"
    );
    assert_eq!(again.list().unwrap()[1], entry);
}

fn changes(repo: &Path, draft: &Draft) -> SessionDiff {
    session_diff(&git(), "", &draft.path, task(repo)).unwrap()
}

fn file<'a>(diff: &'a SessionDiff, path: &str) -> &'a FileDiff {
    diff.files
        .iter()
        .find(|file| file.path == path)
        .unwrap_or_else(|| panic!("{path} not in {:?}", paths(diff)))
}

fn paths(diff: &SessionDiff) -> Vec<&str> {
    diff.files.iter().map(|file| file.path.as_str()).collect()
}

/// Each line as `kind old new text`, for compact assertions.
fn rows(file: &FileDiff) -> Vec<String> {
    file.hunks
        .iter()
        .flat_map(|hunk| &hunk.lines)
        .map(|line| {
            let no = |n: Option<u32>| n.map_or("-".to_string(), |n| n.to_string());
            let kind = match line.kind {
                diff::LineKind::Context => ' ',
                diff::LineKind::Added => '+',
                diff::LineKind::Removed => '-',
                diff::LineKind::NoNewline => '\\',
            };
            format!("{kind} {} {} {}", no(line.old), no(line.new), line.text)
        })
        .collect()
}

fn numbered(count: usize, tag: &str) -> String {
    (0..count).map(|n| format!("{tag} {n}\n")).collect()
}

#[test]
fn changes_of_a_clean_task_are_empty() {
    let scratch = Scratch::new();
    let (repo, draft) = repo_with_draft(&scratch);
    assert_eq!(changes(&repo, &draft), SessionDiff::default());
}

#[test]
fn changes_show_edits_with_line_numbers() {
    let scratch = Scratch::new();
    let (repo, draft) = repo_with_draft(&scratch);
    fs::write(draft.path.join("a.txt"), "one\n2\nthree\nfour\n").unwrap();
    let diff = changes(&repo, &draft);
    let a = file(&diff, "a.txt");
    assert_eq!(a.status, FileStatus::Modified);
    assert_eq!(a.status.letter(), 'M');
    assert_eq!((a.insertions, a.deletions), (2, 1));
    assert_eq!(a.hunks.len(), 1);
    assert_eq!(a.hunks[0].header, "@@ -1,3 +1,4 @@");
    assert_eq!(
        rows(a),
        [
            "  1 1 one",
            "- 2 - two",
            "+ - 2 2",
            "  3 3 three",
            "+ - 4 four"
        ]
    );
    assert!(a.collapsed.is_none() && a.hidden_lines == 0 && a.old_path.is_none());
}

#[test]
fn changes_count_commits_and_edits_from_where_the_branch_left() {
    let scratch = Scratch::new();
    let (repo, draft) = repo_with_draft(&scratch);
    fs::write(draft.path.join("a.txt"), "one\ntwo\nthree\nfour\n").unwrap();
    fs::write(draft.path.join("added.txt"), "new\n").unwrap();
    commit_all(&draft.path, "task");
    fs::write(draft.path.join("added.txt"), "new\nmore\n").unwrap();
    // Later work on the default branch is not the task's.
    fs::write(repo.join("main.txt"), "main\n").unwrap();
    commit_all(&repo, "main moves on");
    let diff = changes(&repo, &draft);
    assert_eq!(paths(&diff), ["a.txt", "added.txt"]);
    let added = file(&diff, "added.txt");
    assert_eq!(added.status, FileStatus::Added);
    assert_eq!(rows(added), ["+ - 1 new", "+ - 2 more"]);
    assert_eq!(diff.stat, counts(2, 3, 0));
    assert_eq!(diff.stat, stat(&repo, &draft));
}

#[test]
fn deleted_files_show_their_old_lines() {
    let scratch = Scratch::new();
    let (repo, draft) = repo_with_draft(&scratch);
    fs::remove_file(draft.path.join("a.txt")).unwrap();
    let diff = changes(&repo, &draft);
    let a = file(&diff, "a.txt");
    assert_eq!(a.status, FileStatus::Deleted);
    assert_eq!(rows(a), ["- 1 - one", "- 2 - two", "- 3 - three"]);
    assert_eq!(diff.stat, counts(1, 0, 3));
}

#[test]
fn renames_with_and_without_edits() {
    let scratch = Scratch::new();
    let repo = scratch.repo("demo");
    let body = numbered(10, "line");
    fs::write(repo.join("same.txt"), &body).unwrap();
    fs::write(repo.join("edit.txt"), numbered(10, "other")).unwrap();
    commit_all(&repo, "base");
    let draft = create_draft(&git(), "", &repo, "one", "HEAD").unwrap();
    run(&draft.path, &["mv", "same.txt", "moved.txt"]);
    run(&draft.path, &["mv", "edit.txt", "edited.txt"]);
    let edited = numbered(10, "other").replace("other 5", "changed");
    fs::write(draft.path.join("edited.txt"), edited).unwrap();
    let diff = changes(&repo, &draft);
    let moved = file(&diff, "moved.txt");
    assert_eq!(moved.status, FileStatus::Renamed);
    assert_eq!(moved.old_path.as_deref(), Some("same.txt"));
    assert!(moved.hunks.is_empty() && moved.collapsed.is_none());
    let edited = file(&diff, "edited.txt");
    assert_eq!(edited.status.letter(), 'R');
    assert_eq!(edited.old_path.as_deref(), Some("edit.txt"));
    assert_eq!((edited.insertions, edited.deletions), (1, 1));
    assert!(rows(edited).contains(&"+ - 6 changed".to_string()));
    assert_eq!(diff.stat, stat(&repo, &draft));
}

#[test]
fn binary_mode_only_and_empty_files_have_no_lines() {
    let scratch = Scratch::new();
    let repo = scratch.repo("demo");
    fs::write(repo.join("run.sh"), "echo hi\n").unwrap();
    fs::write(repo.join("logo.png"), [0x89, b'P', 0, 1, b'\n']).unwrap();
    commit_all(&repo, "base");
    let draft = create_draft(&git(), "", &repo, "one", "HEAD").unwrap();
    run(&draft.path, &["update-index", "--chmod=+x", "run.sh"]);
    fs::set_permissions(
        draft.path.join("run.sh"),
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .unwrap();
    fs::write(draft.path.join("logo.png"), [0x89, b'P', 0, 2, b'\n']).unwrap();
    fs::write(draft.path.join("empty.txt"), "").unwrap();
    run(&draft.path, &["add", "empty.txt"]);
    let diff = changes(&repo, &draft);
    let mode = file(&diff, "run.sh");
    assert_eq!(
        mode.mode_change,
        Some(diff::ModeChange {
            old: "100644".into(),
            new: "100755".into()
        })
    );
    assert!(mode.hunks.is_empty() && !mode.binary);
    let logo = file(&diff, "logo.png");
    assert!(logo.binary && logo.hunks.is_empty() && logo.collapsed.is_none());
    assert_eq!((logo.insertions, logo.deletions), (0, 0));
    let empty = file(&diff, "empty.txt");
    assert_eq!(empty.status, FileStatus::Added);
    assert!(empty.hunks.is_empty() && empty.collapsed.is_none());
    assert_eq!(diff.stat, counts(3, 0, 0));
    assert_eq!(diff.stat, stat(&repo, &draft));
}

#[test]
fn untracked_files_show_as_added_without_following_symlinks() {
    let scratch = Scratch::new();
    let (repo, draft) = repo_with_draft(&scratch);
    fs::write(draft.path.join("new.txt"), "a\nb").unwrap();
    fs::write(draft.path.join("blob.bin"), [1, 0, b'\n', b'\n']).unwrap();
    fs::write(draft.path.join("empty.txt"), "").unwrap();
    std::os::unix::fs::symlink("/etc/hosts", draft.path.join("link")).unwrap();
    fs::write(draft.path.join(".gitignore"), "*.log\n").unwrap();
    fs::write(draft.path.join("build.log"), "ignored\n").unwrap();
    let diff = changes(&repo, &draft);
    assert_eq!(
        paths(&diff),
        [".gitignore", "blob.bin", "empty.txt", "link", "new.txt"]
    );
    assert!(diff.files.iter().all(|f| f.status == FileStatus::Untracked));
    let new = file(&diff, "new.txt");
    assert_eq!(new.hunks[0].header, "@@ -0,0 +1,2 @@");
    assert_eq!(
        rows(new),
        ["+ - 1 a", "+ - 2 b", "\\ - - \\ No newline at end of file"]
    );
    let blob = file(&diff, "blob.bin");
    assert!(blob.binary && blob.hunks.is_empty());
    assert!(file(&diff, "empty.txt").hunks.is_empty());
    let link = file(&diff, "link");
    assert_eq!(link.hunks[0].header, "@@ -0,0 +1 @@");
    assert_eq!(rows(link)[0], "+ - 1 /etc/hosts");
    assert_eq!(diff.stat, counts(5, 4, 0));
    assert_eq!(diff.stat, stat(&repo, &draft));
    // Reading the diff stages nothing.
    assert!(porcelain(&draft.path).contains("?? new.txt"));
}

#[test]
fn line_endings_and_invalid_utf8_come_through() {
    let scratch = Scratch::new();
    let repo = scratch.repo("demo");
    fs::write(repo.join("tail.txt"), "a\nb\n").unwrap();
    fs::write(repo.join("crlf.txt"), "x\r\ny\r\n").unwrap();
    commit_all(&repo, "base");
    let draft = create_draft(&git(), "", &repo, "one", "HEAD").unwrap();
    fs::write(draft.path.join("tail.txt"), "a\nb").unwrap();
    fs::write(draft.path.join("crlf.txt"), "x\r\nY\r\n").unwrap();
    fs::write(draft.path.join("latin1.txt"), b"caf\xe9\n").unwrap();
    run(&draft.path, &["add", "latin1.txt"]);
    let diff = changes(&repo, &draft);
    assert_eq!(
        rows(file(&diff, "tail.txt")),
        [
            "  1 1 a",
            "- 2 - b",
            "+ - 2 b",
            "\\ - - \\ No newline at end of file"
        ]
    );
    assert_eq!(
        rows(file(&diff, "crlf.txt")),
        ["  1 1 x\r", "- 2 - y\r", "+ - 2 Y\r"]
    );
    assert_eq!(rows(file(&diff, "latin1.txt")), ["+ - 1 caf\u{fffd}"]);
    assert_eq!(diff.stat, stat(&repo, &draft));
}

#[test]
fn a_typechange_is_two_entries_and_one_file() {
    let scratch = Scratch::new();
    let (repo, draft) = repo_with_draft(&scratch);
    fs::remove_file(draft.path.join("a.txt")).unwrap();
    std::os::unix::fs::symlink("target", draft.path.join("a.txt")).unwrap();
    let diff = changes(&repo, &draft);
    let statuses: Vec<FileStatus> = diff.files.iter().map(|f| f.status).collect();
    assert_eq!(statuses, [FileStatus::Deleted, FileStatus::Added]);
    assert_eq!(diff.stat, counts(1, 1, 3));
    assert_eq!(diff.stat, stat(&repo, &draft));
    let added = file_diff(&git(), "", &draft.path, &diff.files[1].key)
        .unwrap()
        .unwrap();
    assert_eq!(added.status, FileStatus::Added);
}

#[test]
fn unusual_paths_round_trip() {
    let scratch = Scratch::new();
    let repo = scratch.repo("demo");
    let names = [
        "sp ace.txt",
        "dir b/x b/y.txt",
        "ünï cødé.txt",
        "q\"uote.txt",
        "tab\there.txt",
        "new\nline.txt",
        "star*.txt",
        "back\\slash.txt",
    ];
    fs::create_dir_all(repo.join("dir b/x b")).unwrap();
    for name in names {
        fs::write(repo.join(name), "one\n").unwrap();
    }
    fs::write(repo.join("star.txt"), "one\n").unwrap();
    fs::write(repo.join("m b x"), "m\n").unwrap();
    fs::write(repo.join("old b x.txt"), numbered(5, "r")).unwrap();
    commit_all(&repo, "base");
    let draft = create_draft(&git(), "", &repo, "one", "HEAD").unwrap();
    for name in names {
        fs::write(draft.path.join(name), "one\ntwo\n").unwrap();
    }
    fs::set_permissions(
        draft.path.join("m b x"),
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .unwrap();
    for dir in ["new b", "bin b", "untracked ü b"] {
        fs::create_dir_all(draft.path.join(dir)).unwrap();
    }
    run(&draft.path, &["mv", "old b x.txt", "new b/ü x.txt"]);
    fs::write(draft.path.join("bin b/ary.bin"), [0, 1]).unwrap();
    run(&draft.path, &["add", "bin b/ary.bin"]);
    fs::write(draft.path.join("untracked ü b/c.txt"), "u\n").unwrap();
    let diff = changes(&repo, &draft);
    for name in names {
        let changed = file(&diff, name);
        assert_eq!(changed.status, FileStatus::Modified, "{name}");
        assert_eq!(rows(changed), ["  1 1 one", "+ - 2 two"], "{name}");
    }
    assert!(file(&diff, "m b x").mode_change.is_some());
    assert_eq!(
        file(&diff, "new b/ü x.txt").old_path.as_deref(),
        Some("old b x.txt")
    );
    assert!(file(&diff, "bin b/ary.bin").binary);
    assert_eq!(rows(file(&diff, "untracked ü b/c.txt")), ["+ - 1 u"]);
    assert_eq!(diff.stat, stat(&repo, &draft));
    // Expanding reads the same file back by its exact bytes, and a `*`
    // matches only itself.
    for changed in &diff.files {
        let again = file_diff(&git(), "", &draft.path, &changed.key)
            .unwrap()
            .unwrap_or_else(|| panic!("{} gone", changed.path));
        assert_eq!(&again, changed);
    }
}

#[test]
fn a_file_over_the_line_cap_collapses_and_expands() {
    let scratch = Scratch::new();
    let (repo, draft) = repo_with_draft(&scratch);
    fs::write(draft.path.join("big.txt"), numbered(2_001, "big")).unwrap();
    fs::write(draft.path.join("small.txt"), "s\n").unwrap();
    commit_all(&draft.path, "task");
    // An untracked file over 1 MiB is not read at all.
    let huge = "y".repeat(1023) + "\n";
    fs::write(draft.path.join("huge.log.txt"), huge.repeat(1100)).unwrap();
    let diff = changes(&repo, &draft);
    let big = file(&diff, "big.txt");
    assert_eq!(big.collapsed, Some(Collapse::Lines));
    assert!(big.hunks.is_empty());
    assert_eq!((big.insertions, big.hidden_lines), (2_001, 2_001));
    let huge_file = file(&diff, "huge.log.txt");
    assert_eq!(huge_file.collapsed, Some(Collapse::Size(1100 * 1024)));
    assert_eq!(huge_file.hidden_lines, 1_100);
    // A file over its own cap leaves the others alone.
    assert_eq!(file(&diff, "small.txt").collapsed, None);
    assert_eq!(diff.stat, stat(&repo, &draft));

    let expanded = file_diff(&git(), "", &draft.path, &big.key)
        .unwrap()
        .unwrap();
    assert_eq!(expanded.collapsed, None);
    assert_eq!((expanded.hidden_lines, rows(&expanded).len()), (0, 2_001));
    assert_eq!(rows(&expanded)[2_000], "+ - 2001 big 2000");
    let expanded = file_diff(&git(), "", &draft.path, &huge_file.key)
        .unwrap()
        .unwrap();
    assert_eq!((expanded.hidden_lines, rows(&expanded).len()), (0, 1_100));
    // A file that no longer differs has nothing to expand.
    run(&draft.path, &["reset", "--hard", "HEAD~1"]);
    fs::remove_file(draft.path.join("huge.log.txt")).unwrap();
    assert_eq!(file_diff(&git(), "", &draft.path, &big.key).unwrap(), None);
    assert_eq!(
        file_diff(&git(), "", &draft.path, &huge_file.key).unwrap(),
        None
    );
}

#[test]
fn the_total_cap_collapses_every_file_past_it() {
    let scratch = Scratch::new();
    let (repo, draft) = repo_with_draft(&scratch);
    // 26 files of 1,990 lines pass 50,000 lines in the 26th.
    for n in 0..26 {
        fs::write(
            draft.path.join(format!("f{n:02}.txt")),
            numbered(1_990, "x"),
        )
        .unwrap();
    }
    fs::write(draft.path.join("y.txt"), numbered(2_001, "y")).unwrap();
    fs::write(draft.path.join("z.txt"), "z\n").unwrap();
    commit_all(&draft.path, "task");
    let diff = changes(&repo, &draft);
    let collapsed: Vec<_> = diff.files.iter().map(|f| f.collapsed).collect();
    // f00 to f24 fit; f25 and z.txt are past the cap. y.txt is past it
    // too, but over its own line cap, which names it.
    let mut expected = vec![None; 25];
    expected.push(Some(Collapse::Budget));
    expected.push(Some(Collapse::Lines));
    expected.push(Some(Collapse::Budget));
    assert_eq!(collapsed, expected);
    let small = file(&diff, "z.txt");
    assert_eq!(small.hidden_lines, 1);
    assert_eq!(diff.stat, counts(28, 26 * 1_990 + 2_001 + 1, 0));
    assert_eq!(diff.stat, stat(&repo, &draft));
    // A file past the budget expands like a large one.
    let expanded = file_diff(&git(), "", &draft.path, &small.key)
        .unwrap()
        .unwrap();
    assert_eq!(expanded.collapsed, None);
    assert_eq!(rows(&expanded), ["+ - 1 z"]);
}

#[test]
fn expand_stops_at_the_hard_limit() {
    let scratch = Scratch::new();
    let (repo, draft) = repo_with_draft(&scratch);
    fs::write(draft.path.join("gen.txt"), numbered(120_000, "g")).unwrap();
    commit_all(&draft.path, "task");
    let diff = changes(&repo, &draft);
    let generated = file(&diff, "gen.txt");
    assert_eq!(generated.collapsed, Some(Collapse::Lines));
    let expanded = file_diff(&git(), "", &draft.path, &generated.key)
        .unwrap()
        .unwrap();
    assert_eq!(expanded.collapsed, None);
    assert_eq!(rows(&expanded).len(), 100_000);
    assert_eq!(expanded.hidden_lines, 20_000);
    assert_eq!(expanded.insertions, 120_000);
}

#[test]
fn the_diff_stat_agrees_on_a_mixed_tree() {
    let scratch = Scratch::new();
    let repo = scratch.repo("demo");
    fs::write(repo.join("keep.txt"), numbered(50, "k")).unwrap();
    fs::write(repo.join("gone.txt"), numbered(5, "g")).unwrap();
    fs::write(repo.join("move.txt"), numbered(20, "m")).unwrap();
    fs::write(repo.join("swap"), "s\n").unwrap();
    commit_all(&repo, "base");
    let draft = create_draft(&git(), "", &repo, "one", "HEAD").unwrap();
    fs::write(draft.path.join("keep.txt"), numbered(60, "K")).unwrap();
    commit_all(&draft.path, "task");
    fs::remove_file(draft.path.join("gone.txt")).unwrap();
    run(&draft.path, &["mv", "move.txt", "moved.txt"]);
    fs::remove_file(draft.path.join("swap")).unwrap();
    std::os::unix::fs::symlink("keep.txt", draft.path.join("swap")).unwrap();
    fs::write(draft.path.join("big.txt"), numbered(3_000, "b")).unwrap();
    run(&draft.path, &["add", "big.txt"]);
    fs::write(draft.path.join("loose.txt"), "l\nm").unwrap();
    fs::write(draft.path.join("loose.bin"), [0, 0]).unwrap();
    let diff = changes(&repo, &draft);
    assert_eq!(diff.stat, stat(&repo, &draft));
    let sum = |f: fn(&FileDiff) -> usize| diff.files.iter().map(f).sum::<usize>();
    assert_eq!(sum(|f| f.insertions), diff.stat.insertions);
    assert_eq!(sum(|f| f.deletions), diff.stat.deletions);
}

/// The reads that must fail, not fall through to the main checkout, when
/// a task worktree has lost its `.git` file.
fn assert_reads_fail(repo: &Path, draft: &Draft, key: &FileKey) {
    let stat = diff_stat(&git(), "", &draft.path, task(repo));
    assert!(matches!(stat, Err(Error::GitStatus(_))), "{stat:?}");
    let changes = session_diff(&git(), "", &draft.path, task(repo));
    assert!(matches!(changes, Err(Error::ReadChanges(_))), "{changes:?}");
    let expanded = file_diff(&git(), "", &draft.path, key);
    assert!(
        matches!(expanded, Err(Error::ReadChanges(_))),
        "{expanded:?}"
    );
}

#[test]
fn a_worktree_without_its_git_file_never_reads_the_main_checkout() {
    let scratch = Scratch::new();
    let (repo, draft) = repo_with_draft(&scratch);
    fs::write(draft.path.join("a.txt"), "one\n").unwrap();
    let key = file(&changes(&repo, &draft), "a.txt").key.clone();
    // The main checkout around `.worktrees/` is clean, so reading it
    // instead would look like "No changes".
    fs::remove_file(draft.path.join(".git")).unwrap();
    assert_reads_fail(&repo, &draft, &key);
    let changes = session_diff(&git(), "", &draft.path, task(&repo)).unwrap_err();
    assert!(
        changes.to_string().contains("not a git repository"),
        "{changes}"
    );
    // A broken `.git` file fails in git itself.
    fs::write(draft.path.join(".git"), "gitdir: /nonexistent/dir\n").unwrap();
    assert_reads_fail(&repo, &draft, &key);
}

#[test]
fn a_colon_in_the_path_still_stops_at_the_worktree() {
    // Git splits `GIT_CEILING_DIRECTORIES` at `:`, so this ceiling is
    // wrong; the top-level check still catches the main checkout.
    let scratch = Scratch::new();
    let repo = scratch.repo("a:b");
    fs::write(repo.join("a.txt"), "one\n").unwrap();
    commit_all(&repo, "base");
    let draft = create_draft(&git(), "", &repo, "one", "HEAD").unwrap();
    fs::write(draft.path.join("a.txt"), "two\n").unwrap();
    assert_eq!(stat(&repo, &draft), counts(1, 1, 1));
    let key = file(&changes(&repo, &draft), "a.txt").key.clone();
    fs::remove_file(draft.path.join(".git")).unwrap();
    assert_reads_fail(&repo, &draft, &key);
}

#[test]
fn a_worktree_reached_through_a_symlink_still_reads() {
    let scratch = Scratch::new();
    let (repo, draft) = repo_with_draft(&scratch);
    fs::write(draft.path.join("a.txt"), "one\n").unwrap();
    let link = scratch.path.join("link");
    std::os::unix::fs::symlink(&draft.path, &link).unwrap();
    let linked = Draft {
        branch: draft.branch.clone(),
        path: link,
    };
    assert_eq!(stat(&repo, &linked), counts(1, 0, 2));
    assert_eq!(changes(&repo, &linked).stat, counts(1, 0, 2));
}

/// Timing on a large change: 200 files with 150 lines replaced in each,
/// 60,000 changed lines. Run with
/// `cargo test -p shika-core --release large_diff_timing -- --ignored --nocapture`.
#[test]
#[ignore]
fn large_diff_timing() {
    let scratch = Scratch::new();
    let repo = scratch.repo("demo");
    for n in 0..200 {
        fs::write(
            repo.join(format!("f{n:03}.rs")),
            numbered(300, "let base ="),
        )
        .unwrap();
    }
    commit_all(&repo, "base");
    let draft = create_draft(&git(), "", &repo, "one", "HEAD").unwrap();
    for n in 0..200 {
        let text: String = (0..300)
            .map(|i| {
                if i % 2 == 0 {
                    format!("let task = {i};\n")
                } else {
                    format!("let base = {i}\n")
                }
            })
            .collect();
        fs::write(draft.path.join(format!("f{n:03}.rs")), text).unwrap();
    }
    let mut best = (Duration::MAX, Duration::MAX);
    let mut collapsed = 0;
    for _ in 0..5 {
        let started = Instant::now();
        let diff = changes(&repo, &draft);
        let full = started.elapsed();
        let started = Instant::now();
        let numstat = stat(&repo, &draft);
        let quick = started.elapsed();
        assert_eq!(diff.stat, numstat);
        assert_eq!(diff.stat.insertions + diff.stat.deletions, 60_000);
        best = (best.0.min(full), best.1.min(quick));
        collapsed = diff.files.iter().filter(|f| f.collapsed.is_some()).count();
    }
    eprintln!(
        "session_diff {:?}, diff_stat {:?}, {collapsed} of 200 files past the total cap",
        best.0, best.1
    );
}
