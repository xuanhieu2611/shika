use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::activity::ActivityBridge;
use crate::error::{Error, Result};
use crate::pty::{PtyId, PtySize, SpawnRequest};

/// One card: an agent CLI running in its own worktree. Memory only; a
/// relaunch starts with none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub id: String,
    pub project_id: String,
    pub preset_id: String,
    pub preset_name: String,
    pub title: String,
    /// A user-supplied display name wins over prompt/CLI titles. Memory only;
    /// automatic branch naming remains independent.
    pub manual_title: bool,
    pub branch: String,
    /// The project's repository root, kept so closing never depends on the
    /// project list.
    pub repo: PathBuf,
    pub worktree: PathBuf,
    /// The ref the task branch started from, such as `refs/remotes/origin/dev`.
    /// Close and the diff stat measure the task against it, so changing the
    /// project's base later leaves this card alone. None when it started from
    /// the main checkout's HEAD; the default branch is used then.
    pub base_ref: Option<String>,
    pub pty: PtyId,
    pub shell_ptys: Vec<PtyId>,
    /// Whether the CLI's own session title has named the card and branch.
    pub cli_titled: bool,
}

/// Git facts for the close dialog. `agent_working` comes from the UI's
/// coarse status, because a live process can be idle and ready to check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionGitState {
    pub dirty: bool,
    pub unpushed: bool,
    pub pushed: bool,
    pub has_own_commits: bool,
    pub agent_working: bool,
}

impl SessionGitState {
    pub fn requires_confirmation(self) -> bool {
        self.dirty || self.unpushed || self.agent_working
    }

    pub fn can_push(self) -> bool {
        !self.dirty && self.unpushed
    }
}

/// What a task changed against where its branch left the default branch:
/// committed work, uncommitted edits, and untracked files. Binary files
/// count as files with no lines.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DiffStat {
    pub files: usize,
    pub insertions: usize,
    pub deletions: usize,
}

/// Normalize a manual task name. Unicode is allowed; controls and line breaks
/// are not. The limit matches the card's existing 80-character title cap.
pub fn task_title(text: &str) -> Result<String> {
    if text
        .chars()
        .any(|ch| ch.is_control() || matches!(ch, '\u{2028}' | '\u{2029}'))
    {
        return Err(Error::InvalidTaskTitle(
            "Use a single-line task name.".into(),
        ));
    }
    let title = text.trim();
    if title.is_empty() {
        return Err(Error::InvalidTaskTitle("Enter a task name.".into()));
    }
    if title.chars().count() > 80 {
        return Err(Error::InvalidTaskTitle(
            "Use 80 characters or fewer.".into(),
        ));
    }
    Ok(title.into())
}

pub(crate) struct SessionStore {
    sessions: Mutex<Vec<Session>>,
    // Separate ownership from public Session snapshots: retained UI snapshots
    // must not retain files after a session ends.
    activity: Mutex<HashMap<String, ActivityBridge>>,
}

impl SessionStore {
    pub(crate) fn new() -> Self {
        Self {
            sessions: Mutex::new(Vec::new()),
            activity: Mutex::new(HashMap::new()),
        }
    }

    pub(crate) fn remember_activity(&self, id: &str, bridge: ActivityBridge) {
        self.activity
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id.into(), bridge);
    }

    /// Clone under a short metadata lock; filesystem reads happen after it.
    pub(crate) fn activity(&self, id: &str) -> Option<ActivityBridge> {
        self.activity
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(id)
            .cloned()
    }

    fn forget_activity(&self, id: &str) {
        let bridge = self
            .activity
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id);
        if let Some(bridge) = bridge {
            bridge.cleanup();
        }
    }

    pub(crate) fn insert(&self, session: Session) {
        self.sessions
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .push(session);
    }

    pub(crate) fn all(&self) -> Vec<Session> {
        self.sessions
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .clone()
    }

    pub(crate) fn ids(&self) -> Vec<String> {
        self.sessions
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .iter()
            .map(|session| session.id.clone())
            .collect()
    }

    pub(crate) fn get(&self, id: &str) -> Option<Session> {
        self.sessions
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .iter()
            .find(|session| session.id == id)
            .cloned()
    }

    pub(crate) fn remember_shell(&self, id: &str, pty: PtyId) -> Result<()> {
        let mut sessions = self.sessions.lock().unwrap_or_else(|err| err.into_inner());
        let session = sessions
            .iter_mut()
            .find(|session| session.id == id)
            .ok_or(Error::UnknownSession)?;
        session.shell_ptys.push(pty);
        Ok(())
    }

    pub(crate) fn forget_shell(&self, id: &str, pty: PtyId) -> Result<()> {
        let mut sessions = self.sessions.lock().unwrap_or_else(|err| err.into_inner());
        let session = sessions
            .iter_mut()
            .find(|s| s.id == id)
            .ok_or(Error::UnknownSession)?;
        if !session.shell_ptys.contains(&pty) {
            return Err(Error::UnknownPty);
        }
        session.shell_ptys.retain(|shell| *shell != pty);
        Ok(())
    }

    /// Pure metadata mutation under the short session lock, never the Git
    /// operations lock. Automatic naming checks the override under this lock.
    pub(crate) fn set_title(&self, id: &str, title: &str) -> Result<Session> {
        let title = task_title(title)?;
        let mut sessions = self.sessions.lock().unwrap_or_else(|err| err.into_inner());
        let session = sessions
            .iter_mut()
            .find(|session| session.id == id)
            .ok_or(Error::UnknownSession)?;
        session.title = title;
        session.manual_title = true;
        Ok(session.clone())
    }

    pub(crate) fn rename(&self, id: &str, branch: String, title: String) -> Result<Session> {
        let mut sessions = self.sessions.lock().unwrap_or_else(|err| err.into_inner());
        let session = sessions
            .iter_mut()
            .find(|session| session.id == id)
            .ok_or(Error::UnknownSession)?;
        session.branch = branch;
        if !session.manual_title {
            session.title = title;
        }
        Ok(session.clone())
    }

    pub(crate) fn apply_cli_title(
        &self,
        id: &str,
        branch: String,
        title: String,
    ) -> Result<Session> {
        let mut sessions = self.sessions.lock().unwrap_or_else(|err| err.into_inner());
        let session = sessions
            .iter_mut()
            .find(|session| session.id == id)
            .ok_or(Error::UnknownSession)?;
        session.branch = branch;
        if !session.manual_title {
            session.title = title;
        }
        session.cli_titled = true;
        Ok(session.clone())
    }

    pub(crate) fn remove(&self, id: &str) {
        self.forget_activity(id);
        self.sessions
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .retain(|session| session.id != id);
    }

    pub(crate) fn remove_project(&self, project_id: &str) -> Vec<Session> {
        let mut sessions = self.sessions.lock().unwrap_or_else(|err| err.into_inner());
        let (gone, kept) = sessions
            .drain(..)
            .partition(|session| session.project_id == project_id);
        *sessions = kept;
        for session in &gone {
            self.forget_activity(&session.id);
        }
        gone
    }
}

impl Drop for SessionStore {
    fn drop(&mut self) {
        // PTY reader closures can outlive Core while reaping the process.
        for bridge in self
            .activity
            .get_mut()
            .unwrap_or_else(|e| e.into_inner())
            .values()
        {
            bridge.cleanup();
        }
    }
}

pub(crate) fn shell_request(
    shell: &Path,
    worktree: &Path,
    path: &str,
    size: PtySize,
) -> SpawnRequest {
    SpawnRequest {
        program: shell.to_path_buf(),
        args: vec!["-il".to_string()],
        cwd: worktree.to_path_buf(),
        path: path.to_string(),
        size,
        env: Vec::new(),
    }
}

pub(crate) fn new_id(existing: &[String]) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let mut extra = 0u32;
    loop {
        let id = if extra == 0 {
            format!("{nanos:x}")
        } else {
            format!("{nanos:x}-{extra}")
        };
        if existing.iter().all(|existing_id| existing_id != &id) {
            return id;
        }
        extra += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pty_ids(count: u64) -> Vec<PtyId> {
        (1..=count).map(PtyId).collect()
    }

    fn sample(id: &str, project_id: &str, pty: PtyId) -> Session {
        Session {
            id: id.to_string(),
            project_id: project_id.to_string(),
            preset_id: "claude".to_string(),
            preset_name: "Claude Code".to_string(),
            title: "New Claude Code".to_string(),
            manual_title: false,
            branch: "shika-draft-1".to_string(),
            repo: PathBuf::from("/repo"),
            worktree: PathBuf::from("/repo/.worktrees/shika-draft-1"),
            base_ref: None,
            pty,
            shell_ptys: Vec::new(),
            cli_titled: false,
        }
    }

    #[test]
    fn manual_titles_win_in_both_automatic_update_orders() {
        let store = SessionStore::new();
        store.insert(sample("a", "repo", PtyId(1)));
        store.set_title("a", "  My task  ").unwrap();
        let prompted = store
            .rename("a", "prompt-branch".into(), "Prompt".into())
            .unwrap();
        assert_eq!(prompted.title, "My task");
        let titled = store
            .apply_cli_title("a", "cli-branch".into(), "CLI title".into())
            .unwrap();
        assert_eq!(titled.title, "My task");
        assert_eq!(titled.branch, "cli-branch");
        assert!(titled.cli_titled && titled.manual_title);
        let renamed = store.set_title("a", "Second name").unwrap();
        assert_eq!(renamed.title, "Second name");
        assert_eq!(renamed.branch, titled.branch);
        assert_eq!(store.set_title("gone", "Name"), Err(Error::UnknownSession));
    }

    #[test]
    fn task_names_are_trimmed_bounded_unicode_and_single_line() {
        assert_eq!(
            task_title("  Sửa lỗi đăng nhập  ").unwrap(),
            "Sửa lỗi đăng nhập"
        );
        assert!(task_title(&"é".repeat(80)).is_ok());
        for text in [
            "",
            "   ",
            "line\nline",
            "line\rline",
            "tab\there",
            "nul\0",
            "line\u{2028}line",
            "line\u{2029}line",
            &"é".repeat(81),
        ] {
            assert!(matches!(task_title(text), Err(Error::InvalidTaskTitle(_))));
        }
        let store = SessionStore::new();
        let original = sample("a", "repo", PtyId(1));
        store.insert(original.clone());
        assert!(store.set_title("a", " ").is_err());
        assert_eq!(store.get("a"), Some(original));
    }

    #[test]
    fn the_shell_opens_in_the_worktree() {
        let request = shell_request(
            Path::new("/bin/zsh"),
            Path::new("/repo/.worktrees/shika-draft-1"),
            "/usr/local/bin:/usr/bin",
            PtySize::new(40, 120),
        );
        assert_eq!(request.program, PathBuf::from("/bin/zsh"));
        assert_eq!(request.args, vec!["-il".to_string()]);
        assert_eq!(request.cwd, PathBuf::from("/repo/.worktrees/shika-draft-1"));
        assert_eq!(request.path, "/usr/local/bin:/usr/bin");
        assert_eq!(request.size, PtySize::new(40, 120));
    }

    #[test]
    fn shells_are_independent_and_only_owned_shells_can_be_removed() {
        let ids = pty_ids(4);
        let store = SessionStore::new();
        store.insert(sample("one", "project", ids[0]));
        store.remember_shell("one", ids[1]).unwrap();
        store.remember_shell("one", ids[2]).unwrap();
        assert_eq!(store.get("one").unwrap().shell_ptys, ids[1..3]);
        store.insert(sample("two", "project", ids[3]));
        assert_eq!(store.forget_shell("two", ids[1]), Err(Error::UnknownPty));
        assert_eq!(store.forget_shell("one", ids[0]), Err(Error::UnknownPty));
        assert!(store.forget_shell("missing", ids[1]).is_err());
        store.forget_shell("one", ids[1]).unwrap();
        assert_eq!(store.get("one").unwrap().shell_ptys, [ids[2]]);
        assert_eq!(
            store.remember_shell("missing", ids[3]),
            Err(Error::UnknownSession)
        );
    }

    #[test]
    fn a_removed_session_is_gone() {
        let ids = pty_ids(1);
        let store = SessionStore::new();
        store.insert(sample("one", "project", ids[0]));
        store.insert(sample("two", "project", ids[0]));
        store.remove("one");
        assert!(store.get("one").is_none());
        assert!(store.get("two").is_some());
    }

    #[test]
    fn removing_a_project_takes_only_its_sessions() {
        let ids = pty_ids(1);
        let store = SessionStore::new();
        store.insert(sample("one", "alpha", ids[0]));
        store.insert(sample("two", "beta", ids[0]));
        store.insert(sample("three", "alpha", ids[0]));
        let gone = store.remove_project("alpha");
        assert_eq!(
            gone.iter()
                .map(|session| session.id.as_str())
                .collect::<Vec<_>>(),
            ["one", "three"]
        );
        assert_eq!(store.ids(), ["two"]);
    }

    #[test]
    fn activity_ownership_cleans_on_close_project_removal_and_quit() {
        let store = SessionStore::new();
        let first = ActivityBridge::install().unwrap();
        let second = ActivityBridge::install().unwrap();
        let third = ActivityBridge::install().unwrap();
        let directories: Vec<_> = [&first, &second, &third]
            .into_iter()
            .map(|bridge| bridge.metadata_path().parent().unwrap().to_path_buf())
            .collect();
        store.insert(sample("one", "alpha", PtyId(1)));
        store.insert(sample("two", "beta", PtyId(2)));
        store.insert(sample("three", "beta", PtyId(3)));
        store.remember_activity("one", first.clone());
        store.remember_activity("two", second.clone());
        store.remember_activity("three", third.clone());
        assert!(store.activity("missing").is_none());
        store.remove("one");
        assert!(!directories[0].exists());
        assert!(directories[1].exists());
        store.remove_project("beta");
        assert!(!directories[1].exists());
        assert!(!directories[2].exists());
        let quit = ActivityBridge::install().unwrap();
        let directory = quit.metadata_path().parent().unwrap().to_path_buf();
        store.remember_activity("quit", quit.clone());
        drop(store);
        assert!(!directory.exists());
        assert_eq!(quit.read(), None);
    }

    #[test]
    fn ids_do_not_repeat() {
        let first = new_id(&[]);
        let second = new_id(std::slice::from_ref(&first));
        assert_ne!(first, second);
        assert!(first.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
