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

/// Most characters a generated name keeps, before the prefix and any
/// collision suffix.
const SLUG_MAX: usize = 48;

/// Lowercase ASCII words joined by `-`, cut at a word boundary. Whole-word
/// copies of the project's own name are left out, unless nothing else is
/// left: "Shika background blur" in project `shika` is `background-blur`.
/// None when the text has no ASCII letters or digits.
pub fn branch_slug(text: &str, project: &str) -> Option<String> {
    fn words(text: &str) -> Vec<String> {
        text.split(|c: char| !c.is_ascii_alphanumeric())
            .filter(|word| !word.is_empty())
            .map(str::to_ascii_lowercase)
            .collect()
    }
    let mut words_left = words(text);
    let name = words(project);
    if !name.is_empty() {
        let mut kept = Vec::new();
        let mut at = 0;
        while at < words_left.len() {
            if words_left[at..].starts_with(&name) {
                at += name.len();
            } else {
                kept.push(words_left[at].clone());
                at += 1;
            }
        }
        if !kept.is_empty() {
            words_left = kept;
        }
    }
    let mut slug = String::new();
    for word in words_left {
        let needed = if slug.is_empty() {
            word.len()
        } else {
            word.len() + 1
        };
        if slug.len() + needed > SLUG_MAX {
            if slug.is_empty() {
                slug.push_str(&word[..SLUG_MAX]);
            }
            break;
        }
        if !slug.is_empty() {
            slug.push('-');
        }
        slug.push_str(&word);
    }
    (!slug.is_empty()).then_some(slug)
}

/// The branch prefix setting, made safe for git: path parts of ASCII
/// letters, digits, `.`, `_`, and `-`, joined by `/`. A prefix that does not
/// already end in `-` or `_` gets a `/`, so `hieu` and `hieu/` both give
/// `hieu/`. Empty means no prefix.
pub fn normalize_prefix(raw: &str) -> String {
    let parts: Vec<String> = raw
        .split('/')
        .map(|part| {
            let mut part: String = part
                .chars()
                .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
                .collect();
            while part.contains("..") {
                part = part.replace("..", ".");
            }
            loop {
                let trimmed = part
                    .trim_start_matches(['.', '-'])
                    .trim_end_matches('.')
                    .trim_end_matches(".lock");
                if trimmed.len() == part.len() {
                    break part;
                }
                part = trimmed.to_string();
            }
        })
        .filter(|part| !part.is_empty())
        .collect();
    let mut prefix = parts.join("/");
    if !prefix.is_empty() && !prefix.ends_with(['-', '_']) {
        prefix.push('/');
    }
    prefix
}

/// Whether git accepts `name` as a branch name.
pub fn is_valid_branch(git: &Path, path_env: &str, worktree: &Path, name: &str) -> Result<bool> {
    let status = git_cmd(git, path_env, worktree)
        .args(["check-ref-format", "--branch", name])
        .output()
        .map_err(|_| Error::RenameBranch(None))?
        .status;
    Ok(status.success())
}

/// The branch checked out in `worktree`, or None on a detached HEAD.
pub fn head_branch(git: &Path, path_env: &str, worktree: &Path) -> Result<Option<String>> {
    let output = git_cmd(git, path_env, worktree)
        .args(["symbolic-ref", "--quiet", "--short", "HEAD"])
        .output()
        .map_err(|_| Error::GitStatus(None))?;
    if !output.status.success() {
        return Ok(None);
    }
    Ok(Some(
        String::from_utf8_lossy(&output.stdout).trim().to_string(),
    ))
}

/// Whether `branch` already exists on a remote: it has an upstream, or a
/// remote-tracking branch has its name, as after `git push origin HEAD`.
/// Renaming it then would leave the old name behind on the remote.
pub fn is_published(git: &Path, path_env: &str, worktree: &Path, branch: &str) -> Result<bool> {
    let upstream = git_cmd(git, path_env, worktree)
        .args(["rev-parse", "--abbrev-ref", "--symbolic-full-name"])
        .arg(format!("{branch}@{{upstream}}"))
        .output()
        .map_err(|_| Error::GitStatus(None))?;
    if upstream.status.success() {
        return Ok(true);
    }
    refs_exist(
        git,
        path_env,
        worktree,
        &[format!("refs/remotes/*/{branch}")],
    )
    .map_err(|_| Error::GitStatus(None))
}

fn refs_exist(git: &Path, path_env: &str, worktree: &Path, patterns: &[String]) -> Result<bool> {
    let output = git_cmd(git, path_env, worktree)
        .args(["for-each-ref", "--count=1", "--format=%(refname)"])
        .args(patterns)
        .output()
        .map_err(|_| Error::RenameBranch(None))?;
    if !output.status.success() {
        return Err(Error::RenameBranch(first_line(&output.stderr)));
    }
    Ok(!output.stdout.trim_ascii().is_empty())
}

/// Renames `current` to `name`, or to `name-2`, `name-3`, and so on when a
/// local branch or a remote-tracking branch already uses the name. Returns
/// the name the branch has afterwards. Only the local ref changes; nothing
/// is sent to a remote.
pub fn rename_branch(
    git: &Path,
    path_env: &str,
    worktree: &Path,
    current: &str,
    name: &str,
) -> Result<String> {
    let mut suffix = 1;
    loop {
        let branch = if suffix == 1 {
            name.to_string()
        } else {
            format!("{name}-{suffix}")
        };
        if branch == current {
            return Ok(branch);
        }
        let taken = refs_exist(
            git,
            path_env,
            worktree,
            &[
                format!("refs/heads/{branch}"),
                format!("refs/remotes/*/{branch}"),
            ],
        )?;
        if !taken {
            let output = git_cmd(git, path_env, worktree)
                .args(["branch", "-m", "--", current, &branch])
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
            branch_slug("Fix Job Hunting tracker", "job-hunting").as_deref(),
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
        assert_eq!(normalize_prefix("hieu"), "hieu/");
        assert_eq!(normalize_prefix("hieu/"), "hieu/");
        assert_eq!(normalize_prefix("/hieu//feat/"), "hieu/feat/");
        assert_eq!(normalize_prefix("hieu-"), "hieu-");
        assert_eq!(normalize_prefix("hieu_"), "hieu_");
        assert_eq!(normalize_prefix("Hi eu~^:?*[\\"), "Hieu/");
        assert_eq!(normalize_prefix("-.hidden/x..y/z.lock/"), "hidden/x.y/z/");
        assert_eq!(normalize_prefix("./../"), "");
        for raw in ["hieu", "a.b", "-x", "x.lock", "Hieu Le/", "@{x"] {
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
        let draft = create_draft(&git(), "", &repo, "one").unwrap();
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

        let prefixed = rename_branch(&git(), "", &draft.path, &renamed, "hieu/fix-login").unwrap();
        assert_eq!(prefixed, "hieu/fix-login");
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
