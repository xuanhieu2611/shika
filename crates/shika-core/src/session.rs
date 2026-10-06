use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

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

pub(crate) struct SessionStore {
    sessions: Mutex<Vec<Session>>,
}

impl SessionStore {
    pub(crate) fn new() -> Self {
        Self {
            sessions: Mutex::new(Vec::new()),
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

    pub(crate) fn rename(&self, id: &str, branch: String, title: String) -> Result<Session> {
        let mut sessions = self.sessions.lock().unwrap_or_else(|err| err.into_inner());
        let session = sessions
            .iter_mut()
            .find(|session| session.id == id)
            .ok_or(Error::UnknownSession)?;
        session.branch = branch;
        session.title = title;
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
        session.title = title;
        session.cli_titled = true;
        Ok(session.clone())
    }

    pub(crate) fn remove(&self, id: &str) {
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
        gone
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
    fn ids_do_not_repeat() {
        let first = new_id(&[]);
        let second = new_id(std::slice::from_ref(&first));
        assert_ne!(first, second);
        assert!(first.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
