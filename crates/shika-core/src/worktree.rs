use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result, first_line};

/// Variables that would make git follow another checkout instead of the
/// directory it runs in. Removed from every git call and every PTY.
pub(crate) const GIT_REDIRECTS: [&str; 6] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_PREFIX",
    "GIT_COMMON_DIR",
    "GIT_OBJECT_DIRECTORY",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Draft {
    pub branch: String,
    pub path: PathBuf,
}

/// One Shika worktree on disk. `worktrees.json` keeps these so a quit or a
/// crash can list what was left behind.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct JournalEntry {
    pub project_id: String,
    pub branch: String,
    pub path: PathBuf,
}

pub struct Journal {
    path: PathBuf,
    lock: Mutex<()>,
}

impl Journal {
    pub fn open(path: PathBuf) -> Self {
        Self {
            path,
            lock: Mutex::new(()),
        }
    }

    pub fn list(&self) -> Result<Vec<JournalEntry>> {
        let _guard = self.lock.lock().unwrap_or_else(|err| err.into_inner());
        load(&self.path)
    }

    pub fn add(&self, entry: &JournalEntry) -> Result<()> {
        let _guard = self.lock.lock().unwrap_or_else(|err| err.into_inner());
        let mut entries = load(&self.path)?;
        entries.push(entry.clone());
        save(&self.path, &entries)
    }

    pub fn rename_branch(&self, path: &Path, branch: &str) -> Result<()> {
        let _guard = self.lock.lock().unwrap_or_else(|err| err.into_inner());
        let mut entries = load(&self.path)?;
        let entry = entries
            .iter_mut()
            .find(|entry| entry.path == path)
            .ok_or(Error::ReadJournal)?;
        entry.branch = branch.to_string();
        save(&self.path, &entries)
    }

    pub fn remove_path(&self, path: &Path) -> Result<()> {
        let _guard = self.lock.lock().unwrap_or_else(|err| err.into_inner());
        let mut entries = load(&self.path)?;
        entries.retain(|entry| entry.path != path);
        save(&self.path, &entries)
    }
}

pub fn create_draft(git: &Path, path_env: &str, repo: &Path, id: &str) -> Result<Draft> {
    ensure_excluded(git, path_env, repo)?;
    let branch = format!("shika-draft-{id}");
    let path = repo.join(".worktrees").join(&branch);
    if path.exists() {
        return Err(Error::DraftExists);
    }
    fs::create_dir_all(repo.join(".worktrees")).map_err(|_| Error::CreateWorktree(None))?;
    let output = git_cmd(git, path_env, repo)
        .args(["worktree", "add", "-b", &branch])
        .arg(&path)
        .output()
        .map_err(|_| Error::CreateWorktree(None))?;
    if !output.status.success() {
        let _ = fs::remove_dir(repo.join(".worktrees"));
        return Err(Error::CreateWorktree(first_line(&output.stderr)));
    }
    Ok(Draft { branch, path })
}

pub fn is_dirty(git: &Path, path_env: &str, worktree: &Path) -> Result<bool> {
    let output = git_cmd(git, path_env, worktree)
        .args(["status", "--porcelain"])
        .output()
        .map_err(|_| Error::GitStatus(None))?;
    if !output.status.success() {
        return Err(Error::GitStatus(first_line(&output.stderr)));
    }
    Ok(!String::from_utf8_lossy(&output.stdout).trim().is_empty())
}

/// ASCII branch slug, limited to 48 characters before collision suffixes.
pub fn prompt_slug(prompt: &str, id: &str) -> String {
    let mut slug = String::new();
    let mut dash = false;
    for c in prompt.chars() {
        if c.is_ascii_alphanumeric() {
            if dash && !slug.is_empty() && slug.len() < 48 {
                slug.push('-');
            }
            dash = false;
            if slug.len() < 48 {
                slug.push(c.to_ascii_lowercase());
            }
        } else {
            dash = true;
        }
        if slug.len() >= 48 {
            break;
        }
    }
    let slug = slug.trim_end_matches('-').to_string();
    if slug.is_empty() {
        format!("task-{id}")
    } else {
        slug
    }
}

pub fn rename_from_prompt(
    git: &Path,
    path_env: &str,
    worktree: &Path,
    prompt: &str,
    id: &str,
) -> Result<String> {
    let slug = prompt_slug(prompt, id);
    let mut suffix = 1;
    loop {
        let branch = if suffix == 1 {
            slug.clone()
        } else {
            format!("{slug}-{suffix}")
        };
        let exists = git_cmd(git, path_env, worktree)
            .args([
                "show-ref",
                "--verify",
                "--quiet",
                &format!("refs/heads/{branch}"),
            ])
            .status()
            .map_err(|_| Error::RenameBranch(None))?;
        if !exists.success() {
            if exists.code() != Some(1) {
                return Err(Error::RenameBranch(None));
            }
            let output = git_cmd(git, path_env, worktree)
                .args(["branch", "-m", &branch])
                .output()
                .map_err(|_| Error::RenameBranch(None))?;
            if output.status.success() {
                return Ok(branch);
            }
            return Err(Error::RenameBranch(first_line(&output.stderr)));
        }
        suffix += 1;
    }
}

pub fn git_state(
    git: &Path,
    path_env: &str,
    repo: &Path,
    worktree: &Path,
    agent_working: bool,
) -> Result<crate::SessionGitState> {
    let dirty = is_dirty(git, path_env, worktree)?;
    let upstream = git_cmd(git, path_env, worktree)
        .args([
            "rev-parse",
            "--abbrev-ref",
            "--symbolic-full-name",
            "@{upstream}",
        ])
        .output()
        .map_err(|_| Error::GitStatus(None))?;
    let default = default_branch(git, path_env, repo, worktree)?;
    let has_own_commits = commits_not_in(git, path_env, worktree, &[&default])?;
    let (unpushed, pushed) = if upstream.status.success() {
        let upstream = String::from_utf8_lossy(&upstream.stdout).trim().to_string();
        let unpushed = commits_not_in(git, path_env, worktree, &[&upstream])?;
        (unpushed, !unpushed)
    } else {
        // `git push origin HEAD` sets no upstream but still updates a
        // remote-tracking ref, so any remote branch holding the commits counts.
        let unpushed = commits_not_in(git, path_env, worktree, &[&default, "--remotes"])?;
        (unpushed, has_own_commits && !unpushed)
    };
    Ok(crate::SessionGitState {
        dirty,
        unpushed,
        pushed,
        has_own_commits,
        agent_working,
    })
}

fn default_branch(git: &Path, path_env: &str, repo: &Path, worktree: &Path) -> Result<String> {
    let remote = git_cmd(git, path_env, repo)
        .args(["symbolic-ref", "--quiet", "refs/remotes/origin/HEAD"])
        .output()
        .map_err(|_| Error::GitStatus(None))?;
    if remote.status.success() {
        let reference = String::from_utf8_lossy(&remote.stdout).trim().to_string();
        let valid = git_cmd(git, path_env, repo)
            .args(["rev-parse", "--verify", &reference])
            .output()
            .map_err(|_| Error::GitStatus(None))?;
        if valid.status.success() {
            return Ok(reference);
        }
    }
    for candidate in ["refs/heads/main", "refs/heads/master"] {
        let exists = git_cmd(git, path_env, repo)
            .args(["rev-parse", "--verify", candidate])
            .output()
            .map_err(|_| Error::GitStatus(None))?;
        if exists.status.success() {
            return Ok(String::from_utf8_lossy(&exists.stdout).trim().to_string());
        }
    }
    // A custom default branch can be the main checkout's HEAD. Refuse if
    // that checkout is on the task branch: it cannot prove anything is safe.
    let main_ref = git_cmd(git, path_env, repo)
        .args(["symbolic-ref", "--quiet", "HEAD"])
        .output()
        .map_err(|_| Error::GitStatus(None))?;
    let task_ref = git_cmd(git, path_env, worktree)
        .args(["symbolic-ref", "--quiet", "HEAD"])
        .output()
        .map_err(|_| Error::GitStatus(None))?;
    if main_ref.status.success() && task_ref.status.success() && main_ref.stdout == task_ref.stdout
    {
        return Err(Error::GitStatus(Some(
            "Could not identify the default branch safely.".into(),
        )));
    }
    let head = git_cmd(git, path_env, repo)
        .args(["rev-parse", "--verify", "HEAD"])
        .output()
        .map_err(|_| Error::GitStatus(None))?;
    if head.status.success() {
        return Ok(String::from_utf8_lossy(&head.stdout).trim().to_string());
    }
    Err(Error::GitStatus(None))
}

fn commits_not_in(git: &Path, path_env: &str, worktree: &Path, excluded: &[&str]) -> Result<bool> {
    let output = git_cmd(git, path_env, worktree)
        .args(["rev-list", "--count", "HEAD", "--not"])
        .args(excluded)
        .output()
        .map_err(|_| Error::GitStatus(None))?;
    if !output.status.success() {
        return Err(Error::GitStatus(first_line(&output.stderr)));
    }
    let count = String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse::<u64>()
        .map_err(|_| Error::GitStatus(None))?;
    Ok(count > 0)
}

pub fn push(git: &Path, path_env: &str, worktree: &Path) -> Result<()> {
    let output = git_cmd(git, path_env, worktree)
        .args(["push", "-u", "origin", "HEAD"])
        .output()
        .map_err(|_| Error::Push(None))?;
    if !output.status.success() {
        return Err(Error::Push(first_line(&output.stderr)));
    }
    Ok(())
}

/// Remove only the directory, preserving an already pushed local branch.
pub fn remove_worktree(
    git: &Path,
    path_env: &str,
    repo: &Path,
    worktree: &Path,
    force: bool,
) -> Result<()> {
    let mut cmd = git_cmd(git, path_env, repo);
    cmd.args(["worktree", "remove"]);
    if force {
        cmd.arg("--force");
    }
    let output = cmd
        .arg("--")
        .arg(worktree)
        .output()
        .map_err(|_| Error::RemoveWorktree(None))?;
    if !output.status.success() && worktree.exists() {
        let line = first_line(&output.stderr);
        if String::from_utf8_lossy(&output.stderr).contains("use --force") {
            return Err(Error::WorktreeHasChanges(line));
        }
        return Err(Error::RemoveWorktree(line));
    }
    Ok(())
}

/// Removes the worktree, then deletes its local branch. Without `force`, a
/// tree with changes is refused with [`Error::WorktreeHasChanges`].
pub fn remove_draft(
    git: &Path,
    path_env: &str,
    repo: &Path,
    worktree: &Path,
    branch: &str,
    force: bool,
) -> Result<()> {
    remove_worktree(git, path_env, repo, worktree, force)?;
    let deleted = git_cmd(git, path_env, repo)
        .args(["branch", "-D", "--", branch])
        .output()
        .map_err(|_| Error::RemoveWorktree(None))?;
    if !deleted.status.success() {
        let stderr = String::from_utf8_lossy(&deleted.stderr);
        if !stderr.contains("not found") {
            return Err(Error::RemoveWorktree(first_line(&deleted.stderr)));
        }
    }
    Ok(())
}

fn ensure_excluded(git: &Path, path_env: &str, repo: &Path) -> Result<()> {
    let exclude = exclude_file(git, path_env, repo)?;
    let mut text = if exclude.exists() {
        fs::read_to_string(&exclude).map_err(|_| Error::Exclude)?
    } else {
        String::new()
    };
    if text.lines().any(|line| {
        let line = line.trim();
        line == ".worktrees/" || line == ".worktrees"
    }) {
        return Ok(());
    }
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(".worktrees/\n");
    if let Some(parent) = exclude.parent() {
        fs::create_dir_all(parent).map_err(|_| Error::Exclude)?;
    }
    fs::write(exclude, text).map_err(|_| Error::Exclude)
}

fn exclude_file(git: &Path, path_env: &str, repo: &Path) -> Result<PathBuf> {
    let output = git_cmd(git, path_env, repo)
        .args(["rev-parse", "--absolute-git-dir"])
        .output()
        .map_err(|_| Error::Git(None))?;
    if !output.status.success() {
        return Err(Error::Git(first_line(&output.stderr)));
    }
    let dir = String::from_utf8_lossy(&output.stdout);
    let dir = dir.trim();
    if dir.is_empty() {
        return Err(Error::Git(None));
    }
    Ok(PathBuf::from(dir).join("info").join("exclude"))
}

pub(crate) fn git_cmd(program: &Path, path_env: &str, repo: &Path) -> Command {
    let mut cmd = Command::new(program);
    if !path_env.is_empty() {
        cmd.env("PATH", path_env);
    }
    for key in GIT_REDIRECTS {
        cmd.env_remove(key);
    }
    cmd.arg("-C").arg(repo);
    cmd
}

fn load(path: &Path) -> Result<Vec<JournalEntry>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = fs::read_to_string(path).map_err(|_| Error::ReadJournal)?;
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }
    serde_json::from_str(&text).map_err(|_| Error::ReadJournal)
}

fn save(path: &Path, entries: &[JournalEntry]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|_| Error::SaveJournal)?;
    }
    let mut json = serde_json::to_string_pretty(entries).map_err(|_| Error::SaveJournal)?;
    json.push('\n');
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, json).map_err(|_| Error::SaveJournal)?;
    fs::rename(&tmp, path).map_err(|_| Error::SaveJournal)
}

#[cfg(test)]
mod tests {
    use super::*;
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

    #[test]
    fn slugs_collapse_trim_limit_and_fallback() {
        assert_eq!(prompt_slug("  Fix / Login__Flow!  ", "1"), "fix-login-flow");
        assert_eq!(prompt_slug("?!", "abc"), "task-abc");
        assert_eq!(prompt_slug(&"A".repeat(80), "1"), "a".repeat(48));
        assert_eq!(
            prompt_slug(&format!("{} xx", "A".repeat(47)), "1"),
            "a".repeat(47)
        );
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
        let draft = create_draft(&git(), "", &repo, "one").unwrap();

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
        let draft = create_draft(&git(), "", &repo, "one").unwrap();
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

    #[test]
    fn journal_records_and_forgets_a_draft() {
        let scratch = Scratch::new();
        let journal = Journal::open(scratch.path.join("worktrees.json"));
        let entry = JournalEntry {
            project_id: "p1".into(),
            branch: "shika-draft-one".into(),
            path: "/tmp/demo/.worktrees/shika-draft-one".into(),
        };
        journal.add(&entry).unwrap();
        let again = Journal::open(scratch.path.join("worktrees.json"));
        again
            .add(&JournalEntry {
                project_id: "p1".into(),
                branch: "shika-draft-two".into(),
                path: "/tmp/demo/.worktrees/shika-draft-two".into(),
            })
            .unwrap();
        again.remove_path(&entry.path).unwrap();
        let text = fs::read_to_string(scratch.path.join("worktrees.json")).unwrap();
        assert!(!text.contains("shika-draft-one"));
        assert!(text.contains("shika-draft-two"));
        assert!(text.contains("\"projectId\": \"p1\""), "{text}");
        assert_eq!(again.list().unwrap().len(), 1);
    }
}
