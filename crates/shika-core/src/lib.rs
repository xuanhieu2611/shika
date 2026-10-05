//! Shika's backend, with no UI and no Tauri: saved projects, draft worktrees,
//! the login-shell PATH, the agent CLI presets, PTY processes, and the live
//! sessions that tie them together. The GPUI app drives everything through
//! [`Core`].
//!
//! # Threads
//!
//! [`Core`] is `Send + Sync`; share it as `Arc<Core>`. Its methods fall into
//! two groups.
//!
//! - **Blocking.** [`Core::add_project`], [`Core::create_session`],
//!   [`Core::open_shell`], [`Core::session_dirty`], [`Core::session_git_state`],
//!   [`Core::session_rename_from_prompt`], [`Core::session_discard`],
//!   [`Core::session_push_and_close`], [`Core::session_close`], [`Core::leftover_remove`],
//!   [`Core::remove_project`],
//!   and the first call to [`Core::path_env`] or [`Core::cli_catalog`]. These
//!   run git or the user's login shell and can take seconds (the login shell
//!   is given up to 20). Call them from a background executor, never from the
//!   GPUI main thread. There are no async variants on purpose: the work is
//!   process spawning and file IO, the app already has a background executor,
//!   and a plain blocking call keeps core free of any async runtime.
//! - **Quick.** [`Core::open`], [`Core::projects`], [`Core::settings`],
//!   [`Core::save_settings`],
//!   [`Core::worktree_journal`], [`Core::leftovers_list`],
//!   [`Core::sessions`], [`Core::session`], [`Core::write`], and
//!   [`Core::resize`]. They read a small JSON file, take a short lock, or
//!   queue bytes, and are fine on the main thread. `write` never blocks on
//!   the PTY: each PTY has its own writer thread.
//!
//! The login-shell PATH is captured once, on first use. To keep the picker
//! instant, call [`Core::cli_catalog`] on a background executor at startup.
//!
//! # PTY output
//!
//! Every PTY gets its own reader thread that reads for as long as the process
//! lives, whether or not its terminal is on screen, so a hidden CLI can never
//! stall on a full PTY buffer. The thread hands each chunk of raw bytes, then
//! the exit, to the [`PtySink`] given when the PTY was opened. The sink is
//! supplied before the process starts, so no early output can be missed.
//! Core never wakes the UI; the sink does that, typically by pushing into an
//! unbounded channel whose receiving task runs on the GPUI executor and feeds
//! `shika-terminal`.

mod agents;
mod error;
mod path_env;
mod projects;
mod pty;
mod session;
mod settings;
mod worktree;

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

pub use agents::{CliCatalog, CliPreset};
pub use error::{Error, Result};
pub use path_env::{LoginShellError, PathEnv};
pub use projects::{Project, ProjectAdded};
pub use pty::{PtyEvent, PtyExit, PtyId, PtySink, PtySize};
pub use session::{Session, SessionGitState, ShellOpen};
pub use settings::{Appearance, Settings, Translucency};
pub use worktree::JournalEntry;

use projects::ProjectDb;
use pty::{PtyHub, SpawnRequest};
use session::SessionStore;
use settings::SettingsFile;
use worktree::Journal;

/// The bundle identifier the Tauri build shipped with. Its data directory
/// is named after it, so it must not change.
pub const APP_IDENTIFIER: &str = "com.hieule.shika";

/// `~/Library/Application Support/com.hieule.shika`, the directory Tauri 2's
/// `app_data_dir` resolved to: `dirs::data_dir()` joined with the bundle
/// identifier, with no override in `tauri.conf.json`.
pub fn app_data_dir() -> Result<PathBuf> {
    dirs::data_dir()
        .map(|dir| dir.join(APP_IDENTIFIER))
        .ok_or(Error::AppData)
}

pub struct Core {
    data_dir: PathBuf,
    projects: ProjectDb,
    journal: Journal,
    settings: SettingsFile,
    env: OnceLock<PathEnv>,
    sessions: SessionStore,
    ptys: PtyHub,
    operations: Mutex<()>,
}

impl Core {
    /// Opens `projects.json`, `worktrees.json`, and `settings.json` in
    /// `data_dir`, creating the directory if needed. Pass [`app_data_dir`] in
    /// the app and a temporary directory in tests. Quick: the login shell runs later, on first use.
    pub fn open(data_dir: impl Into<PathBuf>) -> Result<Self> {
        let data_dir = data_dir.into();
        std::fs::create_dir_all(&data_dir).map_err(|_| Error::AppData)?;
        Ok(Self {
            projects: ProjectDb::open(data_dir.join("projects.json")),
            journal: Journal::open(data_dir.join("worktrees.json")),
            settings: SettingsFile::open(data_dir.join("settings.json")),
            data_dir,
            env: OnceLock::new(),
            sessions: SessionStore::new(),
            ptys: PtyHub::new(),
            operations: Mutex::new(()),
        })
    }

    /// Like [`Core::open`], with a PATH already captured by
    /// [`PathEnv::capture`].
    pub fn open_with(data_dir: impl Into<PathBuf>, env: PathEnv) -> Result<Self> {
        let core = Self::open(data_dir)?;
        let _ = core.env.set(env);
        Ok(core)
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// The login-shell PATH and resolved CLIs. Blocking on the first call,
    /// which runs the login shell; later calls return at once.
    pub fn path_env(&self) -> &PathEnv {
        self.env
            .get_or_init(|| PathEnv::capture(&agents::binaries()))
    }

    /// The CLIs the picker offers, found or not. Blocking on first use, as
    /// [`Core::path_env`].
    pub fn cli_catalog(&self) -> CliCatalog {
        CliCatalog::from_env(self.path_env())
    }

    pub fn projects(&self) -> Result<Vec<Project>> {
        self.projects.list()
    }

    /// `settings.json`, or the defaults when it does not exist yet.
    pub fn settings(&self) -> Result<Settings> {
        self.settings.load()
    }

    pub fn save_settings(&self, settings: &Settings) -> Result<()> {
        self.settings.save(settings)
    }

    /// Saves the git root of a folder the user picked. A nested folder
    /// becomes its repository root, and the note says so. Blocking: runs git.
    pub fn add_project(&self, picked: &Path) -> Result<ProjectAdded> {
        let env = self.path_env();
        self.projects.add(&self.git()?, env.path(), picked)
    }

    /// Forgets a project. Its live sessions are hung up and returned so the
    /// app can drop their terminals. Their worktrees stay on disk and in the
    /// journal, as after a quit.
    pub fn remove_project(&self, id: &str) -> Result<Vec<Session>> {
        let _guard = self
            .operations
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        self.projects.remove(id)?;
        let gone = self.sessions.remove_project(id);
        for session in &gone {
            self.hang_up(session);
        }
        Ok(gone)
    }

    /// Every Shika worktree recorded on disk, including ones a quit or a
    /// crash left behind.
    pub fn worktree_journal(&self) -> Result<Vec<JournalEntry>> {
        self.journal.list()
    }

    pub fn sessions(&self) -> Vec<Session> {
        self.sessions.all()
    }

    pub fn session(&self, id: &str) -> Option<Session> {
        self.sessions.get(id)
    }

    /// Creates `<repo>/.worktrees/shika-draft-<id>` on a new branch, journals
    /// it, and starts the CLI there on a PTY of `size` that feeds `sink`.
    /// Anything that fails after the worktree exists removes it again.
    /// Blocking: runs git.
    pub fn create_session(
        &self,
        project_id: &str,
        preset_id: &str,
        size: PtySize,
        sink: impl PtySink,
    ) -> Result<Session> {
        let _guard = self
            .operations
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let project = self.projects.get(project_id)?;
        let env = self.path_env();
        let preset = agents::presets_from(env)
            .into_iter()
            .find(|preset| preset.id == preset_id)
            .ok_or(Error::UnknownCli)?;
        let program = preset
            .path
            .clone()
            .ok_or_else(|| Error::CliNotFound(preset.name.clone()))?;
        let git = self.git()?;
        let id = session::new_id(&self.sessions.ids());
        let repo = project.path.as_path();
        let draft = worktree::create_draft(&git, env.path(), repo, &id)?;
        let entry = JournalEntry {
            project_id: project.id.clone(),
            branch: draft.branch.clone(),
            path: draft.path.clone(),
        };
        if let Err(err) = self.journal.add(&entry) {
            let _ =
                worktree::remove_draft(&git, env.path(), repo, &draft.path, &draft.branch, true);
            return Err(err);
        }
        let opened = self.ptys.open(
            SpawnRequest {
                program,
                args: preset.args.clone(),
                cwd: draft.path.clone(),
                path: env.path().to_string(),
                size,
                env: Vec::new(),
            },
            sink,
        );
        let pty = match opened {
            Ok(pty) => pty,
            Err(err) => {
                let _ = self.journal.remove_path(&entry.path);
                let _ = worktree::remove_draft(
                    &git,
                    env.path(),
                    repo,
                    &draft.path,
                    &draft.branch,
                    true,
                );
                return Err(err);
            }
        };
        let session = Session {
            id,
            project_id: project.id,
            preset_id: preset.id,
            title: format!("New {}", preset.name),
            preset_name: preset.name,
            branch: draft.branch,
            repo: project.path,
            worktree: draft.path,
            pty,
            shell_pty: None,
        };
        self.sessions.insert(session.clone());
        Ok(session)
    }

    /// Starts the user's login shell in the session's worktree the first
    /// time, with the user's own startup files and no command hook.
    /// Later calls return the same shell and drop `sink`. Blocking: the first
    /// call to [`Core::path_env`] and a few small file writes.
    pub fn open_shell(
        &self,
        session_id: &str,
        size: PtySize,
        sink: impl PtySink,
    ) -> Result<ShellOpen> {
        let _guard = self
            .operations
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        if let Some(pty) = self.sessions.shell_pty(session_id) {
            return Ok(ShellOpen {
                pty,
                created: false,
            });
        }
        let session = self.sessions.get(session_id).ok_or(Error::UnknownSession)?;
        let env = self.path_env();
        let shell = path_env::user_shell();
        let request = session::shell_request(&shell, &session.worktree, env.path(), size);
        let pty = self.ptys.open(request, sink)?;
        let opened = match self.sessions.remember_shell(session_id, pty) {
            Ok(opened) => opened,
            Err(err) => {
                self.ptys.close(pty);
                return Err(err);
            }
        };
        // Another call won the race. Keep its shell and hang this one up.
        if !opened.created {
            self.ptys.close(pty);
        }
        Ok(opened)
    }

    /// Whether the worktree has uncommitted changes. Blocking: runs git.
    pub fn session_dirty(&self, id: &str) -> Result<bool> {
        let session = self.sessions.get(id).ok_or(Error::UnknownSession)?;
        let env = self.path_env();
        worktree::is_dirty(&self.git()?, env.path(), &session.worktree)
    }

    /// First submitted prompt names the task and branch. Its worktree folder
    /// stays put, because the CLI is already running inside it. Blocking.
    pub fn session_rename_from_prompt(&self, id: &str, prompt: &str) -> Result<Session> {
        let _guard = self
            .operations
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let session = self.sessions.get(id).ok_or(Error::UnknownSession)?;
        if session.branch != format!("shika-draft-{}", session.id) {
            return Ok(session);
        }
        let env = self.path_env();
        let git = self.git()?;
        self.ensure_session_branch(&session)?;
        let prompt = prompt.lines().next().unwrap_or("");
        let branch = worktree::rename_from_prompt(&git, env.path(), &session.worktree, prompt, id)?;
        if let Err(err) = self.journal.rename_branch(&session.worktree, &branch) {
            // Keep the in-memory record and journal consistent if saving fails.
            let _ = worktree::git_cmd(&git, env.path(), &session.worktree)
                .args(["branch", "-m", &session.branch])
                .output();
            return Err(err);
        }
        let title = prompt
            .lines()
            .next()
            .unwrap_or("")
            .trim()
            .chars()
            .take(80)
            .collect();
        self.sessions.rename(id, branch, title)
    }

    /// Blocking git facts, with the UI's coarse Working status.
    pub fn session_git_state(&self, id: &str, agent_working: bool) -> Result<SessionGitState> {
        let session = self.sessions.get(id).ok_or(Error::UnknownSession)?;
        self.ensure_session_branch(&session)?;
        worktree::git_state(
            &self.git()?,
            self.path_env().path(),
            &session.repo,
            &session.worktree,
            agent_working,
        )
    }

    /// Explicitly confirmed discard. Stops both PTYs, then deletes the tree
    /// and local branch. No commit is created. Blocking.
    pub fn session_discard(&self, id: &str) -> Result<()> {
        let _guard = self
            .operations
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let session = self.sessions.get(id).ok_or(Error::UnknownSession)?;
        self.hang_up(&session);
        worktree::remove_draft(
            &self.git()?,
            self.path_env().path(),
            &session.repo,
            &session.worktree,
            &session.branch,
            true,
        )?;
        self.forget_session(&session)
    }

    /// Explicit push choice. A failed push keeps the card, tree, and PTYs.
    /// Only clean worktrees with unpushed commits can use it. Blocking.
    pub fn session_push_and_close(&self, id: &str) -> Result<()> {
        let _guard = self
            .operations
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let session = self.sessions.get(id).ok_or(Error::UnknownSession)?;
        let state = self.session_git_state(id, false)?;
        if state.dirty {
            return Err(Error::PushDirty);
        }
        if !state.unpushed {
            return Err(Error::NothingToPush);
        }
        let git = self.git()?;
        let path = self.path_env().path();
        worktree::push(&git, path, &session.worktree)?;
        // Recheck after the push: an agent may have edited files while it ran.
        // Never force-remove changes just because pushing was successful.
        if worktree::is_dirty(&git, path, &session.worktree)? {
            return Err(Error::PushDirty);
        }
        let after = worktree::git_state(&git, path, &session.repo, &session.worktree, false)?;
        if after.unpushed {
            return Err(Error::CloseNeedsConfirmation);
        }
        self.hang_up(&session);
        let stopped = worktree::git_state(&git, path, &session.repo, &session.worktree, false)?;
        if stopped.dirty {
            return Err(Error::PushDirty);
        }
        if stopped.unpushed {
            return Err(Error::CloseNeedsConfirmation);
        }
        worktree::remove_worktree(&git, path, &session.repo, &session.worktree, false)?;
        self.forget_session(&session)
    }

    /// Close only when nothing can be lost. A pushed branch stays; an empty
    /// task branch is deleted. The app asks on CloseNeedsConfirmation. Blocking.
    pub fn session_close(&self, id: &str, agent_working: bool) -> Result<()> {
        let _guard = self
            .operations
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let session = self.sessions.get(id).ok_or(Error::UnknownSession)?;
        let state = self.session_git_state(id, agent_working)?;
        if state.dirty {
            return Err(Error::WorktreeHasChanges(None));
        }
        if state.requires_confirmation() {
            return Err(Error::CloseNeedsConfirmation);
        }
        self.hang_up(&session);
        let git = self.git()?;
        let path = self.path_env().path();
        let state = worktree::git_state(&git, path, &session.repo, &session.worktree, false)?;
        if state.dirty {
            return Err(Error::WorktreeHasChanges(None));
        }
        if state.unpushed {
            return Err(Error::CloseNeedsConfirmation);
        }
        if state.pushed {
            worktree::remove_worktree(&git, path, &session.repo, &session.worktree, false)?;
        } else {
            worktree::remove_draft(
                &git,
                path,
                &session.repo,
                &session.worktree,
                &session.branch,
                false,
            )?;
        }
        self.forget_session(&session)
    }

    /// Compatibility helper: confirmation is explicitly a discard choice.
    pub fn close_session(&self, id: &str, confirmed: bool) -> Result<()> {
        if confirmed {
            self.session_discard(id)
        } else {
            self.session_close(id, false)
        }
    }

    fn ensure_session_branch(&self, session: &Session) -> Result<()> {
        let output = worktree::git_cmd(&self.git()?, self.path_env().path(), &session.worktree)
            .args(["symbolic-ref", "--quiet", "--short", "HEAD"])
            .output()
            .map_err(|_| Error::GitStatus(None))?;
        if !output.status.success()
            || String::from_utf8_lossy(&output.stdout).trim() != session.branch
        {
            return Err(Error::GitStatus(Some(
                "The worktree branch changed. Return to the task branch before closing.".into(),
            )));
        }
        Ok(())
    }

    fn forget_session(&self, session: &Session) -> Result<()> {
        self.journal.remove_path(&session.worktree)?;
        self.sessions.remove(&session.id);
        Ok(())
    }

    /// Worktrees left by quit or crash. Missing paths remain listed so their
    /// stale journal entry can be removed on request. Never cleans automatically.
    pub fn leftovers_list(&self) -> Result<Vec<JournalEntry>> {
        let live = self.sessions.all();
        Ok(self
            .journal
            .list()?
            .into_iter()
            .filter(|entry| !live.iter().any(|session| session.worktree == entry.path))
            .collect())
    }

    /// Explicit leftover discard. The journal is the only source of paths.
    /// Live session paths and entries outside .worktrees are refused. Blocking.
    pub fn leftover_remove(&self, path: &Path) -> Result<()> {
        let _guard = self
            .operations
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let entry = self
            .leftovers_list()?
            .into_iter()
            .find(|entry| entry.path == path)
            .ok_or(Error::UnknownLeftover)?;
        let parent = entry
            .path
            .parent()
            .filter(|parent| parent.file_name().is_some_and(|name| name == ".worktrees"))
            .ok_or(Error::UnknownLeftover)?;
        let repo = parent.parent().ok_or(Error::UnknownLeftover)?;
        let git = self.git()?;
        let path_env = self.path_env().path();
        // Follow a branch the user may have manually renamed since quit.
        let branch = if entry.path.exists() {
            let output = worktree::git_cmd(&git, path_env, &entry.path)
                .args(["symbolic-ref", "--short", "HEAD"])
                .output()
                .map_err(|_| Error::GitStatus(None))?;
            if !output.status.success() {
                return Err(Error::GitStatus(error::first_line(&output.stderr)));
            }
            String::from_utf8_lossy(&output.stdout).trim().to_string()
        } else {
            entry.branch
        };
        worktree::remove_draft(&git, path_env, repo, &entry.path, &branch, true)?;
        self.journal.remove_path(&entry.path)
    }

    /// Sends input bytes to a PTY. Never blocks.
    pub fn write(&self, pty: PtyId, bytes: &[u8]) -> Result<()> {
        self.ptys.write(pty, bytes)
    }

    /// Resizes a PTY. A size under 2x2 is ignored.
    pub fn resize(&self, pty: PtyId, size: PtySize) -> Result<()> {
        self.ptys.resize(pty, size)
    }

    fn git(&self) -> Result<PathBuf> {
        self.path_env().resolve("git").ok_or(Error::Git(None))
    }

    fn hang_up(&self, session: &Session) {
        self.ptys.close(session.pty);
        if let Some(shell) = session.shell_pty {
            self.ptys.close(shell);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use crate::pty::tests::{channel_sink, collect_to_exit, collect_until};

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
            let path = std::env::temp_dir().join(format!("shika-core-{nanos}-{n}"));
            fs::create_dir_all(&path).unwrap();
            // `/var` is a symlink on macOS, and git reports resolved paths.
            Self {
                path: path.canonicalize().unwrap(),
            }
        }

        fn repo(&self, name: &str) -> PathBuf {
            let dir = self.path.join(name);
            fs::create_dir_all(&dir).unwrap();
            git(&dir, &["init", "-b", "main"]);
            git(&dir, &["commit", "--allow-empty", "-m", "init"]);
            dir
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn git(repo: &Path, args: &[&str]) -> String {
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
        assert!(output.status.success(), "git {args:?} failed");
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    /// A stand-in for the agent CLI: prints its arguments, then echoes lines.
    fn fake_cli(dir: &Path) -> PathBuf {
        let bin = dir.join("bin");
        fs::create_dir_all(&bin).unwrap();
        let cli = bin.join("claude");
        fs::write(
            &cli,
            "#!/bin/sh\nprintf 'args:%s\\n' \"$*\"\nprintf 'cwd:%s\\n' \"$(pwd -P)\"\nwhile IFS= read -r line; do printf 'got:%s\\n' \"$line\"; done\n",
        )
        .unwrap();
        let mut perms = fs::metadata(&cli).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&cli, perms).unwrap();
        cli
    }

    fn core_with_fake_cli(scratch: &Scratch) -> Core {
        let cli = fake_cli(&scratch.path);
        let env = PathEnv::from_lookup(
            "/usr/bin:/bin".into(),
            None,
            &[("claude", Some(cli)), ("agent", None)],
        );
        Core::open_with(scratch.path.join("data"), env).unwrap()
    }

    #[test]
    fn core_can_be_shared_with_a_background_executor() {
        fn shared<T: Send + Sync>() {}
        shared::<Core>();
    }

    #[test]
    fn the_data_dir_is_the_one_tauri_used() {
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap();
        assert_eq!(
            app_data_dir().unwrap(),
            home.join("Library/Application Support/com.hieule.shika")
        );
    }

    #[test]
    fn files_written_by_the_tauri_build_load_from_an_injected_dir() {
        let scratch = Scratch::new();
        let data = scratch.path.join("data");
        fs::create_dir_all(&data).unwrap();
        fs::write(
            data.join("projects.json"),
            "[\n  {\n    \"id\": \"18db38e2f78faa00\",\n    \"name\": \"job-hunting\",\n    \"path\": \"/Users/x/code/job-hunting\"\n  }\n]\n",
        )
        .unwrap();
        fs::write(
            data.join("worktrees.json"),
            "[\n  {\n    \"projectId\": \"18db38e2f78faa00\",\n    \"branch\": \"shika-draft-1\",\n    \"path\": \"/Users/x/code/job-hunting/.worktrees/shika-draft-1\"\n  }\n]\n",
        )
        .unwrap();
        let core = Core::open(&data).unwrap();
        assert_eq!(core.data_dir(), data);
        assert_eq!(
            core.projects().unwrap(),
            [Project {
                id: "18db38e2f78faa00".into(),
                name: "job-hunting".into(),
                path: PathBuf::from("/Users/x/code/job-hunting"),
            }]
        );
        assert_eq!(
            core.worktree_journal().unwrap(),
            [JournalEntry {
                project_id: "18db38e2f78faa00".into(),
                branch: "shika-draft-1".into(),
                path: PathBuf::from("/Users/x/code/job-hunting/.worktrees/shika-draft-1"),
            }]
        );

        // A missing directory is created and starts empty.
        let fresh = Core::open(scratch.path.join("new").join("data")).unwrap();
        assert!(fresh.projects().unwrap().is_empty());
        assert!(fresh.data_dir().is_dir());
    }

    #[test]
    fn a_session_runs_in_its_worktree_and_close_removes_it() {
        let scratch = Scratch::new();
        let repo = scratch.repo("demo");
        let core = core_with_fake_cli(&scratch);
        let added = core.add_project(&repo.join(".git").join("..")).unwrap();
        let catalog = core.cli_catalog();
        assert!(catalog.presets[0].found());
        assert!(!catalog.presets[1].found());

        let (sink, rx) = channel_sink();
        let session = core
            .create_session(&added.project.id, "claude", PtySize::new(30, 90), sink)
            .unwrap();
        assert_eq!(session.title, "New Claude Code");
        assert_eq!(session.branch, format!("shika-draft-{}", session.id));
        assert_eq!(
            session.worktree,
            repo.join(".worktrees").join(&session.branch)
        );
        assert_eq!(core.sessions(), std::slice::from_ref(&session));
        assert_eq!(core.worktree_journal().unwrap().len(), 1);
        let exclude = fs::read_to_string(repo.join(".git/info/exclude")).unwrap();
        assert!(exclude.contains(".worktrees/"), "{exclude}");
        assert!(!repo.join(".gitignore").exists());

        let got = collect_until(&rx, "cwd:", Duration::from_secs(5));
        let got = got + &collect_until(&rx, "\n", Duration::from_secs(2));
        assert!(got.contains("args:--dangerously-skip-permissions"), "{got}");
        assert!(
            got.contains(&format!("cwd:{}", session.worktree.display())),
            "{got}"
        );
        core.write(session.pty, b"hello\r").unwrap();
        let echoed = collect_until(&rx, "got:hello", Duration::from_secs(3));
        assert!(echoed.contains("got:hello"), "{echoed}");

        let (shell_sink, _shell_rx) = channel_sink();
        let shell = core
            .open_shell(&session.id, PtySize::default(), shell_sink)
            .unwrap();
        assert!(shell.created);
        let (again_sink, _again_rx) = channel_sink();
        let again = core
            .open_shell(&session.id, PtySize::default(), again_sink)
            .unwrap();
        assert_eq!(
            again,
            ShellOpen {
                pty: shell.pty,
                created: false
            }
        );

        assert!(!core.session_dirty(&session.id).unwrap());
        fs::write(session.worktree.join("wip.txt"), "wip\n").unwrap();
        assert!(core.session_dirty(&session.id).unwrap());
        assert!(matches!(
            core.close_session(&session.id, false),
            Err(Error::WorktreeHasChanges(_))
        ));
        assert!(session.worktree.exists());
        assert!(
            core.write(session.pty, b"").is_ok(),
            "a refused close kept the agent"
        );

        core.close_session(&session.id, true).unwrap();
        assert!(!session.worktree.exists());
        assert!(core.sessions().is_empty());
        assert!(core.worktree_journal().unwrap().is_empty());
        assert!(
            git(&repo, &["branch", "--list", &session.branch])
                .trim()
                .is_empty()
        );
        assert_eq!(core.write(session.pty, b"x"), Err(Error::UnknownPty));
        assert_eq!(core.write(shell.pty, b"x"), Err(Error::UnknownPty));
        let (_, exit) = collect_to_exit(&rx, Duration::from_secs(5));
        assert!(exit.is_some(), "the agent was not hung up");
        assert_eq!(
            core.close_session(&session.id, true),
            Err(Error::UnknownSession)
        );
    }

    #[test]
    fn a_missing_cli_or_project_creates_nothing() {
        let scratch = Scratch::new();
        let repo = scratch.repo("demo");
        let core = core_with_fake_cli(&scratch);
        let added = core.add_project(&repo).unwrap();

        let (sink, _rx) = channel_sink();
        assert_eq!(
            core.create_session(&added.project.id, "cursor", PtySize::default(), sink)
                .unwrap_err(),
            Error::CliNotFound("Cursor CLI".into())
        );
        let (sink, _rx) = channel_sink();
        assert_eq!(
            core.create_session(&added.project.id, "codex", PtySize::default(), sink)
                .unwrap_err(),
            Error::UnknownCli
        );
        let (sink, _rx) = channel_sink();
        assert_eq!(
            core.create_session("missing", "claude", PtySize::default(), sink)
                .unwrap_err(),
            Error::UnknownProject
        );
        assert!(!repo.join(".worktrees").exists());
        assert!(core.sessions().is_empty());
    }

    #[test]
    fn removing_a_project_hangs_up_its_sessions_and_keeps_the_worktree() {
        let scratch = Scratch::new();
        let repo = scratch.repo("demo");
        let core = core_with_fake_cli(&scratch);
        let added = core.add_project(&repo).unwrap();
        let (sink, rx) = channel_sink();
        let session = core
            .create_session(&added.project.id, "claude", PtySize::default(), sink)
            .unwrap();

        let gone = core.remove_project(&added.project.id).unwrap();

        assert_eq!(gone, std::slice::from_ref(&session));
        assert!(core.sessions().is_empty());
        assert!(core.projects().unwrap().is_empty());
        assert!(session.worktree.exists());
        assert_eq!(core.worktree_journal().unwrap().len(), 1);
        let (_, exit) = collect_to_exit(&rx, Duration::from_secs(5));
        assert!(exit.is_some(), "the agent was not hung up");
    }
    fn create_fake_session(core: &Core, project_id: &str) -> Session {
        let (sink, _rx) = channel_sink();
        core.create_session(project_id, "claude", PtySize::new(30, 90), sink)
            .unwrap()
    }

    fn local_remote(scratch: &Scratch, repo: &Path) -> PathBuf {
        let remote = scratch.path.join("remote.git");
        fs::create_dir_all(&remote).unwrap();
        git(&remote, &["init", "--bare"]);
        git(repo, &["remote", "add", "origin", remote.to_str().unwrap()]);
        git(repo, &["push", "-u", "origin", "main"]);
        remote
    }

    #[test]
    fn prompt_rename_is_unique_and_keeps_folder_and_journal_consistent() {
        let scratch = Scratch::new();
        let repo = scratch.repo("demo");
        let core = core_with_fake_cli(&scratch);
        let project = core.add_project(&repo).unwrap().project;
        let first = create_fake_session(&core, &project.id);
        let second = create_fake_session(&core, &project.id);
        let one = core
            .session_rename_from_prompt(&first.id, "Fix Login / Flow!")
            .unwrap();
        let two = core
            .session_rename_from_prompt(&second.id, "Fix Login / Flow!")
            .unwrap();
        assert_eq!(one.branch, "fix-login-flow");
        assert_eq!(two.branch, "fix-login-flow-2");
        assert_eq!(one.title, "Fix Login / Flow!");
        assert_eq!(one.worktree, first.worktree);
        assert_eq!(
            git(&one.worktree, &["branch", "--show-current"]).trim(),
            one.branch
        );
        let journal = core.worktree_journal().unwrap();
        assert_eq!(journal[0].branch, one.branch);
        assert_eq!(journal[0].path, first.worktree);
        assert_eq!(
            core.session_rename_from_prompt(&first.id, "second prompt")
                .unwrap(),
            one
        );
        core.session_discard(&first.id).unwrap();
        assert!(core.session(&second.id).is_some());
        core.write(second.pty, b"still alive\r").unwrap();
        core.session_discard(&second.id).unwrap();
    }

    #[test]
    fn safe_close_refuses_work_and_an_empty_task_deletes_its_branch() {
        let scratch = Scratch::new();
        let repo = scratch.repo("demo");
        let core = core_with_fake_cli(&scratch);
        let project = core.add_project(&repo).unwrap().project;
        let session = create_fake_session(&core, &project.id);
        let state = core.session_git_state(&session.id, false).unwrap();
        assert!(!state.has_own_commits && !state.unpushed && !state.pushed);
        assert!(!state.requires_confirmation());
        assert_eq!(
            core.session_close(&session.id, true),
            Err(Error::CloseNeedsConfirmation)
        );
        git(
            &session.worktree,
            &["commit", "--allow-empty", "-m", "task"],
        );
        assert!(
            core.session_git_state(&session.id, false)
                .unwrap()
                .can_push()
        );
        assert_eq!(
            core.session_close(&session.id, false),
            Err(Error::CloseNeedsConfirmation)
        );
        assert!(session.worktree.exists());
        core.session_discard(&session.id).unwrap();
        let empty = create_fake_session(&core, &project.id);
        core.session_close(&empty.id, false).unwrap();
        assert!(!empty.worktree.exists());
        assert!(
            git(&repo, &["branch", "--list", &empty.branch])
                .trim()
                .is_empty()
        );
    }

    #[test]
    fn manual_push_keeps_card_then_safe_close_keeps_pushed_branch() {
        let scratch = Scratch::new();
        let repo = scratch.repo("demo");
        local_remote(&scratch, &repo);
        let core = core_with_fake_cli(&scratch);
        let project = core.add_project(&repo).unwrap().project;
        let session = create_fake_session(&core, &project.id);
        git(
            &session.worktree,
            &["commit", "--allow-empty", "-m", "task"],
        );
        git(&session.worktree, &["push", "-u", "origin", "HEAD"]);
        assert!(core.session(&session.id).is_some());
        assert!(session.worktree.exists());
        let state = core.session_git_state(&session.id, false).unwrap();
        assert!(state.pushed && state.has_own_commits && !state.unpushed);
        core.session_close(&session.id, false).unwrap();
        assert!(!session.worktree.exists());
        assert!(
            !git(&repo, &["branch", "--list", &session.branch])
                .trim()
                .is_empty()
        );
    }

    #[test]
    fn shell_push_without_upstream_counts_as_pushed() {
        let scratch = Scratch::new();
        let repo = scratch.repo("demo");
        local_remote(&scratch, &repo);
        let core = core_with_fake_cli(&scratch);
        let project = core.add_project(&repo).unwrap().project;
        let session = create_fake_session(&core, &project.id);
        git(
            &session.worktree,
            &["commit", "--allow-empty", "-m", "task"],
        );
        git(&session.worktree, &["push", "origin", "HEAD"]);
        let state = core.session_git_state(&session.id, false).unwrap();
        assert!(state.pushed && state.has_own_commits && !state.unpushed);
        assert!(!state.requires_confirmation());
        git(
            &session.worktree,
            &["commit", "--allow-empty", "-m", "more"],
        );
        let state = core.session_git_state(&session.id, false).unwrap();
        assert!(state.unpushed && !state.pushed);
        assert_eq!(
            core.session_close(&session.id, false),
            Err(Error::CloseNeedsConfirmation)
        );
        git(&session.worktree, &["push", "origin", "HEAD"]);
        core.session_close(&session.id, false).unwrap();
        assert!(!session.worktree.exists());
        assert!(
            !git(&repo, &["branch", "--list", &session.branch])
                .trim()
                .is_empty()
        );
    }

    #[test]
    fn push_choice_preserves_failed_task_and_success_keeps_branch() {
        let scratch = Scratch::new();
        let repo = scratch.repo("demo");
        let core = core_with_fake_cli(&scratch);
        let project = core.add_project(&repo).unwrap().project;
        let session = create_fake_session(&core, &project.id);
        assert_eq!(
            core.session_push_and_close(&session.id),
            Err(Error::NothingToPush)
        );
        git(
            &session.worktree,
            &["commit", "--allow-empty", "-m", "task"],
        );
        fs::write(session.worktree.join("untracked"), "wip").unwrap();
        assert_eq!(
            core.session_push_and_close(&session.id),
            Err(Error::PushDirty)
        );
        fs::remove_file(session.worktree.join("untracked")).unwrap();
        assert!(matches!(
            core.session_push_and_close(&session.id),
            Err(Error::Push(_))
        ));
        assert!(core.session(&session.id).is_some());
        assert!(session.worktree.exists());
        assert_eq!(core.worktree_journal().unwrap().len(), 1);
        core.write(session.pty, b"still alive\r").unwrap();
        let remote = local_remote(&scratch, &repo);
        // A rejecting bare remote exercises a real push error, not a fake git.
        let hook = remote.join("hooks/pre-receive");
        fs::write(&hook, "#!/bin/sh\nexit 1\n").unwrap();
        fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(matches!(
            core.session_push_and_close(&session.id),
            Err(Error::Push(_))
        ));
        assert!(core.session(&session.id).is_some());
        fs::remove_file(hook).unwrap();
        core.session_push_and_close(&session.id).unwrap();
        assert!(core.session(&session.id).is_none());
        assert!(!session.worktree.exists());
        assert!(
            !git(&repo, &["branch", "--list", &session.branch])
                .trim()
                .is_empty()
        );
        assert!(
            git(
                &remote,
                &["show-ref", &format!("refs/heads/{}", session.branch)]
            )
            .contains(&session.branch)
        );
    }

    #[test]
    fn relaunch_has_projects_no_sessions_and_removable_leftovers() {
        let scratch = Scratch::new();
        let repo = scratch.repo("demo");
        let core = core_with_fake_cli(&scratch);
        let project = core.add_project(&repo).unwrap().project;
        let session = create_fake_session(&core, &project.id);
        assert!(core.leftovers_list().unwrap().is_empty());
        assert_eq!(
            core.leftover_remove(&session.worktree),
            Err(Error::UnknownLeftover)
        );
        drop(core);
        assert!(session.worktree.exists());
        let core = core_with_fake_cli(&scratch);
        assert_eq!(core.projects().unwrap(), std::slice::from_ref(&project));
        assert!(core.sessions().is_empty());
        assert_eq!(core.leftovers_list().unwrap()[0].path, session.worktree);
        core.remove_project(&project.id).unwrap();
        core.leftover_remove(&session.worktree).unwrap();
        assert!(!session.worktree.exists());
        assert!(core.leftovers_list().unwrap().is_empty());
    }

    #[test]
    fn a_missing_leftover_path_can_be_forgotten_without_real_app_data() {
        let scratch = Scratch::new();
        let repo = scratch.repo("demo");
        let core = core_with_fake_cli(&scratch);
        let project = core.add_project(&repo).unwrap().project;
        let session = create_fake_session(&core, &project.id);
        drop(core);
        fs::remove_dir_all(&session.worktree).unwrap();
        let core = core_with_fake_cli(&scratch);
        core.leftover_remove(&session.worktree).unwrap();
        assert!(core.worktree_journal().unwrap().is_empty());
        assert!(
            git(&repo, &["branch", "--list", &session.branch])
                .trim()
                .is_empty()
        );
    }
    #[test]
    fn custom_default_branch_and_stale_origin_head_are_checked_safely() {
        let scratch = Scratch::new();
        let repo = scratch.repo("demo");
        git(&repo, &["branch", "-m", "development"]);
        git(
            &repo,
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/missing",
            ],
        );
        let core = core_with_fake_cli(&scratch);
        let project = core.add_project(&repo).unwrap().project;
        let session = create_fake_session(&core, &project.id);
        assert!(!core.session_git_state(&session.id, false).unwrap().unpushed);
        git(
            &session.worktree,
            &["commit", "--allow-empty", "-m", "task"],
        );
        assert!(core.session_git_state(&session.id, false).unwrap().unpushed);
        // Git normally forbids two checkouts of a branch. An explicit user
        // override must still never make core treat task commits as empty.
        git(
            &repo,
            &["checkout", "--ignore-other-worktrees", &session.branch],
        );
        assert!(matches!(
            core.session_close(&session.id, false),
            Err(Error::GitStatus(_))
        ));
        assert!(session.worktree.exists());
        git(&repo, &["checkout", "development"]);
        core.session_discard(&session.id).unwrap();
    }

    #[test]
    fn detached_leftover_is_refused_and_kept_for_inspection() {
        let scratch = Scratch::new();
        let repo = scratch.repo("demo");
        let core = core_with_fake_cli(&scratch);
        let project = core.add_project(&repo).unwrap().project;
        let session = create_fake_session(&core, &project.id);
        git(&session.worktree, &["checkout", "--detach"]);
        drop(core);
        let core = core_with_fake_cli(&scratch);
        assert!(matches!(
            core.leftover_remove(&session.worktree),
            Err(Error::GitStatus(_))
        ));
        assert!(session.worktree.exists());
        assert_eq!(core.leftovers_list().unwrap().len(), 1);
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
            Err(Error::GitStatus(_))
        ));
        assert!(matches!(
            core.session_push_and_close(&session.id),
            Err(Error::GitStatus(_))
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
}
