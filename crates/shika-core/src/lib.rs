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
//!   [`Core::session_diff_stat`],
//!   [`Core::session_rename_from_prompt`], [`Core::session_apply_cli_title`],
//!   [`Core::session_discard`],
//!   [`Core::session_push_and_close`], [`Core::session_close`], [`Core::leftover_remove`],
//!   [`Core::remove_project`], [`Core::project_base`], [`Core::set_project_base`],
//!   [`Core::prefetch_base`],
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
mod cli_title;
mod error;
mod path_env;
mod preparation;
#[cfg(test)]
mod preparation_tests;
mod projects;
mod pty;
mod session;
mod settings;
mod worktree;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

pub use agents::{CliCatalog, CliPreset};
pub use error::{Error, Result};
pub use path_env::{LoginShellError, PathEnv};
pub use preparation::{PreparationConfig, PreparationControl, PreparationEvent};
pub use projects::{Project, ProjectAdded};
pub use pty::{PtyEvent, PtyExit, PtyId, PtySink, PtySize};
pub use session::{DiffStat, Session, SessionGitState};
pub use settings::{Appearance, FontSize, Settings, Translucency};
pub use worktree::JournalEntry;
pub use worktree::normalize_prefix as normalize_branch_prefix;

use cli_title::CliHome;
use projects::ProjectDb;
use pty::{PtyHub, SpawnRequest};
use session::SessionStore;
use settings::SettingsFile;
use worktree::Journal;

/// A project's base branch as the app shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectBase {
    /// As saved in `projects.json`. None means the remote default.
    pub configured: Option<String>,
    /// The branch New starts from now, such as `dev` or `main`. None when the
    /// configured branch is missing, or the start is a detached HEAD.
    pub name: Option<String>,
    /// The branch New would start from with no base set.
    pub default_name: Option<String>,
}

/// A fetch of one project's base branch, started at the picker or at New.
struct BaseFetch {
    branch: String,
    started: Instant,
    done: bool,
}

/// A fetch younger than this is fresh enough for New: it waits for one in
/// flight instead of starting another.
const FETCH_FRESH: Duration = Duration::from_secs(30);

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
    /// Where the agent CLIs keep their session titles. None without a home
    /// directory, which only means cards keep their prompt names.
    cli_home: Option<CliHome>,
    /// The latest base branch fetch per project id, and a signal when one ends.
    fetches: Mutex<HashMap<String, BaseFetch>>,
    fetched: Condvar,
    /// Journaled but not yet live. Their setup can be cancelled when a project
    /// is removed, and leftovers never offers a tree with an active writer.
    preparing: Mutex<HashMap<PathBuf, (String, PreparationControl)>>,
    preparation_slots: preparation::PreparationLimiter,
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
            cli_home: CliHome::detect(),
            fetches: Mutex::new(HashMap::new()),
            fetched: Condvar::new(),
            preparing: Mutex::new(HashMap::new()),
            preparation_slots: preparation::PreparationLimiter::default(),
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
        for (project, control) in self
            .preparing
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
        {
            if project == id {
                control.cancel_preserving_worktree();
            }
        }
        let gone = self.sessions.remove_project(id);
        for session in &gone {
            self.hang_up(session);
        }
        Ok(gone)
    }

    /// Called synchronously before app shutdown. Stops setup process groups,
    /// but never removes their worktrees, matching quit's preservation rule.
    pub fn cancel_preparations(&self) {
        for (_, control) in self
            .preparing
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
        {
            control.cancel_preserving_worktree();
        }
    }

    /// Reads the opt-in repository configuration without executing it. A
    /// missing file is the existing launch path, not inferred preparation.
    pub fn project_preparation(&self, id: &str) -> Result<Option<PreparationConfig>> {
        preparation::load(&self.projects.get(id)?.path)
    }

    pub fn preparation_approved(&self, id: &str, config: &PreparationConfig) -> Result<bool> {
        Ok(self.projects.get(id)?.approved_preparation.as_ref() == Some(config))
    }

    /// Consent is local and scoped to the parsed config. Re-read before saving
    /// so an approval dialog cannot approve a replacement it never displayed.
    pub fn approve_preparation(&self, id: &str, config: &PreparationConfig) -> Result<()> {
        if self.project_preparation(id)?.as_ref() != Some(config) {
            return Err(Error::PreparationNeedsApproval);
        }
        self.projects.approve_preparation(id, config.clone())
    }

    /// The project's base branch as New would resolve it now, without
    /// fetching. Blocking: runs git.
    pub fn project_base(&self, id: &str) -> Result<ProjectBase> {
        let project = self.projects.get(id)?;
        let git = self.git()?;
        let env = self.path_env().path();
        let default_name = worktree::resolve_base(&git, env, &project.path, None)?.name;
        let name = match project.base_branch.as_deref() {
            None => default_name.clone(),
            Some(branch) => worktree::resolve_base(&git, env, &project.path, Some(branch))
                .ok()
                .and_then(|base| base.name),
        };
        Ok(ProjectBase {
            configured: project.base_branch,
            name,
            default_name,
        })
    }

    /// Sets the branch new agents in this project start from, once it exists
    /// on origin or locally. A branch missing locally is fetched from origin
    /// first, in case it is new there. `origin/dev` is taken as `dev`. Empty
    /// clears it, back to the remote default. Running sessions keep the base
    /// they started from. Blocking: runs git and may fetch.
    pub fn set_project_base(&self, id: &str, branch: &str) -> Result<Project> {
        let project = self.projects.get(id)?;
        let branch = branch.trim();
        if branch.is_empty() {
            return self.projects.set_base_branch(id, None);
        }
        let git = self.git()?;
        let env = self.path_env().path();
        let repo = project.path.as_path();
        let mut names = vec![branch];
        if let Some(short) = branch.strip_prefix("origin/") {
            names.push(short);
        }
        for name in &names {
            if worktree::configured_ref(&git, env, repo, name)?.is_some() {
                return self.projects.set_base_branch(id, Some(name.to_string()));
            }
        }
        if worktree::has_origin(&git, env, repo) {
            for name in &names {
                if worktree::fetch_branch(&git, env, repo, name, worktree::FETCH_TIMEOUT)
                    && worktree::configured_ref(&git, env, repo, name)?.is_some()
                {
                    return self.projects.set_base_branch(id, Some(name.to_string()));
                }
            }
        }
        Err(Error::NoSuchBranch(branch.to_string()))
    }

    /// Fetches the project's base branch from origin, so New starts from the
    /// remote's latest. Call it when the picker opens: [`Core::create_session`]
    /// then waits for this fetch, up to its timeout, instead of starting
    /// another. Best effort; never fails because the fetch did. Blocking.
    pub fn prefetch_base(&self, id: &str) -> Result<()> {
        let project = self.projects.get(id)?;
        self.freshen_base(&project);
        Ok(())
    }

    /// Fetches the base branch unless a fetch of it started within
    /// [`FETCH_FRESH`]. One still running is waited for, up to its timeout.
    fn freshen_base(&self, project: &Project) {
        let Ok(git) = self.git() else {
            return;
        };
        let env = self.path_env().path();
        let Some(branch) =
            worktree::fetch_target(&git, env, &project.path, project.base_branch.as_deref())
        else {
            return;
        };
        {
            let mut fetches = self.fetches.lock().unwrap_or_else(|err| err.into_inner());
            let recent = fetches
                .get(&project.id)
                .filter(|fetch| fetch.branch == branch && fetch.started.elapsed() < FETCH_FRESH)
                .map(|fetch| fetch.started + worktree::FETCH_TIMEOUT + Duration::from_secs(1));
            if let Some(deadline) = recent {
                loop {
                    let finished = fetches
                        .get(&project.id)
                        .is_none_or(|fetch| fetch.done || fetch.branch != branch);
                    let now = Instant::now();
                    if finished || now >= deadline {
                        return;
                    }
                    fetches = match self.fetched.wait_timeout(fetches, deadline - now) {
                        Ok((guard, _)) => guard,
                        Err(err) => err.into_inner().0,
                    };
                }
            }
            fetches.insert(
                project.id.clone(),
                BaseFetch {
                    branch: branch.clone(),
                    started: Instant::now(),
                    done: false,
                },
            );
        }
        worktree::fetch_branch(&git, env, &project.path, &branch, worktree::FETCH_TIMEOUT);
        let mut fetches = self.fetches.lock().unwrap_or_else(|err| err.into_inner());
        if let Some(fetch) = fetches.get_mut(&project.id)
            && fetch.branch == branch
        {
            fetch.done = true;
        }
        drop(fetches);
        self.fetched.notify_all();
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

    /// Creates `<repo>/.worktrees/shika-draft-<id>` on a new branch from the
    /// project's base branch, journals it, and starts the CLI there on a PTY
    /// of `size` that feeds `sink`. The base is fetched from origin first,
    /// best effort and bounded (see [`Core::prefetch_base`]). A configured
    /// base that exists nowhere is an error. Failed launches remove only a
    /// provably untouched worktree; changed or unverifiable work is journaled
    /// for explicit cleanup. Blocking: runs git and optional setup commands.
    pub fn create_session(
        &self,
        project_id: &str,
        preset_id: &str,
        size: PtySize,
        sink: impl PtySink,
    ) -> Result<Session> {
        self.create_session_with_preparation(
            project_id,
            preset_id,
            size,
            sink,
            PreparationControl::default(),
            |_| {},
        )
    }

    /// Same launch, with cancellable preparation and live progress. Commands
    /// run off the app thread, outside the operation lock. At most two setups
    /// run at once. No agent PTY exists until all preparation succeeds.
    pub fn create_session_with_preparation(
        &self,
        project_id: &str,
        preset_id: &str,
        size: PtySize,
        sink: impl PtySink,
        control: PreparationControl,
        mut report: impl FnMut(PreparationEvent),
    ) -> Result<Session> {
        let env = self.path_env();
        let preset = agents::presets_from(env)
            .into_iter()
            .find(|preset| preset.id == preset_id)
            .ok_or(Error::UnknownCli)?;
        let program = preset
            .path
            .clone()
            .ok_or_else(|| Error::CliNotFound(preset.name.clone()))?;
        let config = self.project_preparation(project_id)?;
        if let Some(config) = &config
            && !self.preparation_approved(project_id, config)?
        {
            return Err(Error::PreparationNeedsApproval);
        }
        control.check()?;
        report(PreparationEvent::Stage("Creating worktree...".into()));
        // Neither fetching nor installation holds up close or discard.
        self.freshen_base(&self.projects.get(project_id)?);
        let (project, base, draft) = {
            let _guard = self.operations.lock().unwrap_or_else(|e| e.into_inner());
            control.check()?;
            let project = self.projects.get(project_id)?;
            if preparation::load(&project.path)? != config {
                return Err(Error::PreparationNeedsApproval);
            }
            let git = self.git()?;
            let mut ids = self.sessions.ids();
            ids.extend(
                self.journal
                    .list()?
                    .iter()
                    .filter_map(|e| e.branch.strip_prefix("shika-draft-").map(str::to_owned)),
            );
            let id = session::new_id(&ids);
            let base = worktree::resolve_base(
                &git,
                env.path(),
                &project.path,
                project.base_branch.as_deref(),
            )?;
            let draft = worktree::create_draft(&git, env.path(), &project.path, &id, base.start())?;
            let entry = JournalEntry {
                project_id: project.id.clone(),
                branch: draft.branch.clone(),
                path: draft.path.clone(),
                base_ref: base.reference.clone(),
            };
            if let Err(err) = self.journal.add(&entry) {
                let _ = worktree::remove_draft(
                    &git,
                    env.path(),
                    &project.path,
                    &draft.path,
                    &draft.branch,
                    true,
                );
                return Err(err);
            }
            self.preparing
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(draft.path.clone(), (project.id.clone(), control.clone()));
            (project, base, draft)
        };
        let prepared = (|| {
            if let Some(config) = &config {
                report(PreparationEvent::Stage("Waiting for setup slot...".into()));
                let _slot = self.preparation_slots.acquire(&control)?;
                preparation::prepare(
                    config,
                    &project.path,
                    &draft.path,
                    &self.git()?,
                    env.path(),
                    &control,
                    &mut report,
                )?;
            }
            control.check()
        })();
        let _guard = self.operations.lock().unwrap_or_else(|e| e.into_inner());
        let result = (|| {
            prepared?;
            control.check()?;
            self.projects.get(project_id)?;
            if preparation::load(&project.path)? != config {
                return Err(Error::PreparationNeedsApproval);
            }
            // A setup script must not move the task onto another branch.
            if worktree::head_branch(&self.git()?, env.path(), &draft.path)?.as_ref()
                != Some(&draft.branch)
            {
                return Err(Error::Preparation(
                    "Setup changed the task branch; return to it before launching".into(),
                ));
            }
            report(PreparationEvent::StartingAgent);
            let pty = self.ptys.open(
                SpawnRequest {
                    program,
                    args: preset.args.clone(),
                    cwd: draft.path.clone(),
                    path: env.path().to_string(),
                    size,
                    env: Vec::new(),
                },
                sink,
            )?;
            if control.is_cancelled() {
                self.ptys.close(pty);
                return Err(Error::PreparationCancelled);
            }
            let session = Session {
                id: draft.branch.trim_start_matches("shika-draft-").to_string(),
                project_id: project.id.clone(),
                preset_id: preset.id,
                title: format!("New {}", preset.name),
                preset_name: preset.name,
                branch: draft.branch.clone(),
                repo: project.path.clone(),
                worktree: draft.path.clone(),
                base_ref: base.reference.clone(),
                pty,
                shell_ptys: Vec::new(),
                cli_titled: false,
            };
            self.sessions.insert(session.clone());
            Ok(session)
        })();
        self.preparing
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&draft.path);
        match result {
            Ok(session) => Ok(session),
            Err(err) => {
                // Remove only a provably untouched failed task. Scripts can
                // produce valuable tracked edits or commits; preserve those
                // and any unverified tree in the journal for explicit cleanup.
                let git = self.git()?;
                let safe = !control.preserves_worktree()
                    && worktree::head_branch(&git, env.path(), &draft.path)
                        .ok()
                        .flatten()
                        .as_ref()
                        == Some(&draft.branch)
                    && worktree::git_state(
                        &git,
                        env.path(),
                        &project.path,
                        &draft.path,
                        base.reference.as_deref(),
                        false,
                    )
                    .is_ok_and(|state| !state.dirty && !state.has_own_commits && !state.pushed);
                if safe
                    && worktree::remove_draft(
                        &git,
                        env.path(),
                        &project.path,
                        &draft.path,
                        &draft.branch,
                        false,
                    )
                    .is_ok()
                {
                    self.journal.remove_path(&draft.path)?;
                    Err(err)
                } else {
                    Err(Error::Preparation(format!(
                        "{err} Worktree kept at {}; inspect it in Leftover worktrees.",
                        draft.path.display()
                    )))
                }
            }
        }
    }

    /// Starts a new independent login shell in the task's worktree.
    /// Blocking: shell discovery and PTY creation. Every shell is owned by
    /// the session and is stopped when that session closes.
    pub fn open_shell(&self, session_id: &str, size: PtySize, sink: impl PtySink) -> Result<PtyId> {
        let _guard = self
            .operations
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let session = self.sessions.get(session_id).ok_or(Error::UnknownSession)?;
        let env = self.path_env();
        let shell = path_env::user_shell();
        let request = session::shell_request(&shell, &session.worktree, env.path(), size);
        let pty = self.ptys.open(request, sink)?;
        if let Err(err) = self.sessions.remember_shell(session_id, pty) {
            self.ptys.close(pty);
            return Err(err);
        }
        Ok(pty)
    }

    /// Stops only this task's specified shell, never its agent or worktree.
    pub fn close_shell(&self, session_id: &str, pty: PtyId) -> Result<()> {
        let _guard = self
            .operations
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        self.sessions.forget_shell(session_id, pty)?;
        self.ptys.close(pty);
        Ok(())
    }

    /// Whether the worktree has uncommitted changes. Blocking: runs git.
    pub fn session_dirty(&self, id: &str) -> Result<bool> {
        let session = self.sessions.get(id).ok_or(Error::UnknownSession)?;
        let env = self.path_env();
        worktree::is_dirty(&self.git()?, env.path(), &session.worktree)
    }

    /// First submitted prompt names the task and branch, until the CLI's own
    /// title replaces both through [`Core::session_apply_cli_title`]. Its
    /// worktree folder stays put, because the CLI is already running inside
    /// it. Blocking.
    pub fn session_rename_from_prompt(&self, id: &str, prompt: &str) -> Result<Session> {
        let _guard = self
            .operations
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let session = self.sessions.get(id).ok_or(Error::UnknownSession)?;
        if session.branch != format!("shika-draft-{}", session.id) || session.cli_titled {
            return Ok(session);
        }
        let env = self.path_env();
        let git = self.git()?;
        let session = self.ensure_session_branch(&session)?;
        let prompt = prompt.lines().next().unwrap_or("");
        let base = worktree::branch_slug(prompt, &self.project_name(&session))
            .unwrap_or_else(|| format!("task-{id}"));
        let branch = self.rename_task_branch(&git, env.path(), &session, &base)?;
        let title = prompt.trim().chars().take(80).collect();
        self.sessions.rename(id, branch, title)
    }

    /// Names the card and branch after the title the session's CLI gave its
    /// own conversation, read from the CLI's files. Applies once. Ok(None)
    /// until the CLI has written a title, and after one was applied. The
    /// branch keeps its name when it is already on a remote or the worktree
    /// is on another branch; the card still takes the title. Blocking: reads
    /// the CLI's files and runs git.
    pub fn session_apply_cli_title(&self, id: &str) -> Result<Option<Session>> {
        let _guard = self
            .operations
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let session = self.sessions.get(id).ok_or(Error::UnknownSession)?;
        if session.cli_titled {
            return Ok(None);
        }
        let Some(title) = self
            .cli_home
            .as_ref()
            .and_then(|home| home.read(&session.preset_id, &session.worktree))
        else {
            return Ok(None);
        };
        let env = self.path_env();
        let git = self.git()?;
        let mut branch = session.branch.clone();
        let on_branch =
            worktree::head_branch(&git, env.path(), &session.worktree)?.as_ref() == Some(&branch);
        if on_branch
            && !worktree::is_published(&git, env.path(), &session.worktree, &branch)?
            && let Some(base) = worktree::branch_slug(&title, &self.project_name(&session))
        {
            branch = self.rename_task_branch(&git, env.path(), &session, &base)?;
        }
        self.sessions.apply_cli_title(id, branch, title).map(Some)
    }

    /// Renames the session's branch to the prefix setting plus `base`, and
    /// keeps the journal in step. Unreadable settings or a prefix git
    /// refuses mean no prefix.
    fn rename_task_branch(
        &self,
        git: &Path,
        path_env: &str,
        session: &Session,
        base: &str,
    ) -> Result<String> {
        let prefix = self
            .settings()
            .map(|settings| worktree::normalize_prefix(&settings.branch_prefix))
            .unwrap_or_default();
        let mut name = format!("{prefix}{base}");
        if !prefix.is_empty()
            && !worktree::is_valid_branch(git, path_env, &session.worktree, &name)?
        {
            name = base.to_string();
        }
        let branch =
            worktree::rename_branch(git, path_env, &session.worktree, &session.branch, &name)?;
        if branch == session.branch {
            return Ok(branch);
        }
        if let Err(err) = self.journal.rename_branch(&session.worktree, &branch) {
            // Keep the in-memory record and journal consistent if saving fails.
            let _ = worktree::git_cmd(git, path_env, &session.worktree)
                .args(["branch", "-m", "--", &branch, &session.branch])
                .output();
            return Err(err);
        }
        Ok(branch)
    }

    fn project_name(&self, session: &Session) -> String {
        self.projects
            .get(&session.project_id)
            .map(|project| project.name)
            .unwrap_or_default()
    }

    /// Blocking git facts, with the UI's coarse Working status.
    pub fn session_git_state(&self, id: &str, agent_working: bool) -> Result<SessionGitState> {
        let _guard = self
            .operations
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        self.session_git_state_locked(id, agent_working)
    }

    fn session_git_state_locked(&self, id: &str, agent_working: bool) -> Result<SessionGitState> {
        let session = self.sessions.get(id).ok_or(Error::UnknownSession)?;
        let session = self.ensure_session_branch(&session)?;
        worktree::git_state(
            &self.git()?,
            self.path_env().path(),
            &session.repo,
            &session.worktree,
            session.base_ref.as_deref(),
            agent_working,
        )
    }

    /// Blocking. What the task changed: committed and uncommitted work
    /// against where the branch left the base it started from. Takes no lock,
    /// so a background refresh never holds up close or discard.
    pub fn session_diff_stat(&self, id: &str) -> Result<DiffStat> {
        let session = self.sessions.get(id).ok_or(Error::UnknownSession)?;
        worktree::diff_stat(
            &self.git()?,
            self.path_env().path(),
            &session.repo,
            &session.worktree,
            session.base_ref.as_deref(),
        )
    }

    /// A launch completed after its UI was cancelled or dropped. Stop it,
    /// preserving the journal and any setup output; no user prompt was sent.
    pub fn cancel_session_start(&self, id: &str) -> Result<()> {
        let _guard = self.operations.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(session) = self.sessions.get(id) {
            self.hang_up(&session);
            self.sessions.remove(id);
        }
        Ok(())
    }

    /// Explicitly confirmed discard. Stops both PTYs, then deletes the tree
    /// and local branch. No commit is created. Blocking.
    pub fn session_discard(&self, id: &str) -> Result<()> {
        let _guard = self
            .operations
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let session = self.sessions.get(id).ok_or(Error::UnknownSession)?;
        let session = self.ensure_session_branch(&session)?;
        self.hang_up(&session);
        let session = self.ensure_session_branch(&session)?;
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
        let state = self.session_git_state_locked(id, false)?;
        let session = self.sessions.get(id).ok_or(Error::UnknownSession)?;
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
        let base = session.base_ref.as_deref();
        let after = worktree::git_state(&git, path, &session.repo, &session.worktree, base, false)?;
        if after.unpushed {
            return Err(Error::CloseNeedsConfirmation);
        }
        self.hang_up(&session);
        let session = self.ensure_session_branch(&session)?;
        let stopped =
            worktree::git_state(&git, path, &session.repo, &session.worktree, base, false)?;
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
        let state = self.session_git_state_locked(id, agent_working)?;
        let session = self.sessions.get(id).ok_or(Error::UnknownSession)?;
        if state.dirty {
            return Err(Error::WorktreeHasChanges(None));
        }
        if state.requires_confirmation() {
            return Err(Error::CloseNeedsConfirmation);
        }
        self.hang_up(&session);
        let session = self.ensure_session_branch(&session)?;
        let git = self.git()?;
        let path = self.path_env().path();
        let base = session.base_ref.as_deref();
        let state = worktree::git_state(&git, path, &session.repo, &session.worktree, base, false)?;
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

    /// Refresh external branch renames for the card. Blocking; callers should
    /// run this off the UI thread. Branch switches remain errors.
    pub fn session_refresh_branch(&self, id: &str) -> Result<Session> {
        let _guard = self
            .operations
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let session = self.sessions.get(id).ok_or(Error::UnknownSession)?;
        self.ensure_session_branch(&session)
    }

    /// Called under the operations lock. Save the journal before publishing
    /// the new name in memory; a save failure leaves all close paths blocked.
    fn ensure_session_branch(&self, session: &Session) -> Result<Session> {
        let git = self.git()?;
        let path = self.path_env().path();
        let head = worktree::head_branch(&git, path, &session.worktree)?;
        if head.as_ref() == Some(&session.branch) {
            return Ok(session.clone());
        }
        if let Some(branch) = head.as_ref()
            && worktree::was_renamed(&git, path, &session.worktree, &session.branch, branch)?
        {
            self.journal.rename_branch(&session.worktree, branch)?;
            return self
                .sessions
                .rename(&session.id, branch.clone(), session.title.clone());
        }
        Err(Error::TaskBranchChanged {
            expected: session.branch.clone(),
            current: head.unwrap_or_else(|| "detached HEAD".into()),
        })
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
        let preparing = self.preparing.lock().unwrap_or_else(|e| e.into_inner());
        Ok(self
            .journal
            .list()?
            .into_iter()
            .filter(|entry| {
                !live.iter().any(|session| session.worktree == entry.path)
                    && !preparing.contains_key(&entry.path)
            })
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
        for shell in &session.shell_ptys {
            self.ptys.close(*shell);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;
    use std::sync::Arc;
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
                base_branch: None,
                approved_preparation: None,
            }]
        );
        assert_eq!(
            core.worktree_journal().unwrap(),
            [JournalEntry {
                project_id: "18db38e2f78faa00".into(),
                branch: "shika-draft-1".into(),
                path: PathBuf::from("/Users/x/code/job-hunting/.worktrees/shika-draft-1"),
                base_ref: None,
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
        assert_eq!(core.sessions.get(&session.id).unwrap().shell_ptys, [shell]);
        let (again_sink, _again_rx) = channel_sink();
        let again = core
            .open_shell(&session.id, PtySize::default(), again_sink)
            .unwrap();
        assert_ne!(again, shell);
        assert!(core.close_shell(&session.id, session.pty).is_err());
        core.close_shell(&session.id, shell).unwrap();
        assert_eq!(core.sessions.get(&session.id).unwrap().shell_ptys, [again]);
        assert!(core.write(shell, b"exit\r").is_err());
        core.write(again, b"echo independent\r").unwrap();

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
        assert_eq!(core.write(shell, b"x"), Err(Error::UnknownPty));
        assert_eq!(core.write(again, b"x"), Err(Error::UnknownPty));
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
            core.create_session(&added.project.id, "kiro", PtySize::default(), sink)
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

    /// `demo` on main, pushed to a bare `origin` whose HEAD is main, with a
    /// `dev` one commit ahead of main that exists only on origin.
    fn repo_with_dev_on_origin(scratch: &Scratch) -> (PathBuf, PathBuf) {
        let repo = scratch.repo("demo");
        let remote = local_remote(scratch, &repo);
        git(&repo, &["remote", "set-head", "origin", "main"]);
        git(&repo, &["switch", "-c", "dev"]);
        fs::write(repo.join("dev.txt"), "dev\n").unwrap();
        git(&repo, &["add", "dev.txt"]);
        git(&repo, &["commit", "-m", "dev work"]);
        git(&repo, &["push", "origin", "dev"]);
        git(&repo, &["switch", "main"]);
        git(&repo, &["branch", "-D", "dev"]);
        (repo, remote)
    }

    fn rev(dir: &Path, reference: &str) -> String {
        git(dir, &["rev-parse", reference]).trim().to_string()
    }

    fn has_upstream(worktree: &Path) -> bool {
        Command::new("git")
            .arg("-C")
            .arg(worktree)
            .args(["rev-parse", "--abbrev-ref", "@{upstream}"])
            .output()
            .unwrap()
            .status
            .success()
    }

    fn branch_exists(repo: &Path, branch: &str) -> bool {
        !git(repo, &["branch", "--list", branch]).trim().is_empty()
    }

    #[test]
    fn a_dev_base_starts_from_origin_dev_without_an_upstream() {
        let scratch = Scratch::new();
        let (repo, _remote) = repo_with_dev_on_origin(&scratch);
        let core = core_with_fake_cli(&scratch);
        let project = core.add_project(&repo).unwrap().project;
        assert_eq!(
            core.project_base(&project.id).unwrap(),
            ProjectBase {
                configured: None,
                name: Some("main".into()),
                default_name: Some("main".into()),
            }
        );
        let saved = core.set_project_base(&project.id, " origin/dev ").unwrap();
        assert_eq!(saved.base_branch.as_deref(), Some("dev"));
        assert_eq!(
            core.projects().unwrap()[0].base_branch.as_deref(),
            Some("dev")
        );
        assert_eq!(
            core.project_base(&project.id).unwrap(),
            ProjectBase {
                configured: Some("dev".into()),
                name: Some("dev".into()),
                default_name: Some("main".into()),
            }
        );

        let session = create_fake_session(&core, &project.id);
        assert_eq!(session.base_ref.as_deref(), Some("refs/remotes/origin/dev"));
        assert_eq!(
            rev(&session.worktree, "HEAD"),
            rev(&repo, "refs/remotes/origin/dev")
        );
        assert!(!has_upstream(&session.worktree));
        assert_eq!(
            core.worktree_journal().unwrap()[0].base_ref.as_deref(),
            Some("refs/remotes/origin/dev")
        );
        // Measured against main, dev's own commit would make this card look
        // pushed and full of changes.
        let state = core.session_git_state(&session.id, false).unwrap();
        assert!(!state.dirty && !state.has_own_commits && !state.unpushed && !state.pushed);
        assert_eq!(
            core.session_diff_stat(&session.id).unwrap(),
            DiffStat::default()
        );
        core.session_close(&session.id, false).unwrap();
        assert!(!session.worktree.exists());
        assert!(!branch_exists(&repo, &session.branch));
    }

    #[test]
    fn a_commit_on_a_dev_card_is_its_own_and_unpushed() {
        let scratch = Scratch::new();
        let (repo, _remote) = repo_with_dev_on_origin(&scratch);
        let core = core_with_fake_cli(&scratch);
        let project = core.add_project(&repo).unwrap().project;
        core.set_project_base(&project.id, "dev").unwrap();
        let session = create_fake_session(&core, &project.id);
        fs::write(session.worktree.join("task.txt"), "one\ntwo\n").unwrap();
        git(&session.worktree, &["add", "task.txt"]);
        git(&session.worktree, &["commit", "-m", "task"]);

        assert_eq!(
            git(
                &session.worktree,
                &["rev-list", "--count", "refs/remotes/origin/dev..HEAD"]
            )
            .trim(),
            "1"
        );
        let state = core.session_git_state(&session.id, false).unwrap();
        assert!(state.has_own_commits && state.unpushed && !state.pushed);
        assert!(state.can_push());
        assert_eq!(
            core.session_diff_stat(&session.id).unwrap(),
            DiffStat {
                files: 1,
                insertions: 2,
                deletions: 0,
            }
        );
        assert_eq!(
            core.session_close(&session.id, false),
            Err(Error::CloseNeedsConfirmation)
        );
        // Push and close sends the task branch, never dev.
        let dev = rev(&repo, "refs/remotes/origin/dev");
        core.session_push_and_close(&session.id).unwrap();
        git(&repo, &["fetch", "origin"]);
        assert_eq!(rev(&repo, "refs/remotes/origin/dev"), dev);
        assert!(branch_exists(&repo, &session.branch));
    }

    #[test]
    fn a_local_only_base_branch_works() {
        let scratch = Scratch::new();
        let repo = scratch.repo("demo");
        git(&repo, &["switch", "-c", "staging"]);
        git(&repo, &["commit", "--allow-empty", "-m", "staging work"]);
        git(&repo, &["switch", "main"]);
        let core = core_with_fake_cli(&scratch);
        let project = core.add_project(&repo).unwrap().project;
        core.set_project_base(&project.id, "staging").unwrap();

        let session = create_fake_session(&core, &project.id);
        assert_eq!(session.base_ref.as_deref(), Some("refs/heads/staging"));
        assert_eq!(rev(&session.worktree, "HEAD"), rev(&repo, "staging"));
        assert!(!has_upstream(&session.worktree));
        let state = core.session_git_state(&session.id, false).unwrap();
        assert!(!state.has_own_commits && !state.unpushed && !state.pushed);
        core.session_close(&session.id, false).unwrap();
        assert!(!branch_exists(&repo, &session.branch));
    }

    #[test]
    fn a_missing_base_branch_is_refused_and_creates_nothing() {
        let scratch = Scratch::new();
        let repo = scratch.repo("demo");
        git(&repo, &["branch", "staging"]);
        let core = core_with_fake_cli(&scratch);
        let project = core.add_project(&repo).unwrap().project;
        for typed in ["nope", "a..b", "-x", "*"] {
            assert_eq!(
                core.set_project_base(&project.id, typed),
                Err(Error::NoSuchBranch(typed.into()))
            );
        }
        assert_eq!(core.projects().unwrap()[0].base_branch, None);

        // Set, then deleted: New refuses instead of starting from main.
        core.set_project_base(&project.id, "staging").unwrap();
        git(&repo, &["branch", "-D", "staging"]);
        let (sink, _rx) = channel_sink();
        assert_eq!(
            core.create_session(&project.id, "claude", PtySize::default(), sink)
                .unwrap_err(),
            Error::BaseBranchMissing("staging".into())
        );
        assert!(core.sessions().is_empty());
        assert!(core.worktree_journal().unwrap().is_empty());
        assert!(!repo.join(".worktrees").exists());
        let base = core.project_base(&project.id).unwrap();
        assert_eq!(base.configured.as_deref(), Some("staging"));
        assert_eq!(base.name, None);

        // Empty clears it, back to the default branch.
        assert_eq!(
            core.set_project_base(&project.id, "  ")
                .unwrap()
                .base_branch,
            None
        );
        let session = create_fake_session(&core, &project.id);
        assert_eq!(session.base_ref.as_deref(), Some("refs/heads/main"));
        core.session_discard(&session.id).unwrap();
    }

    #[test]
    fn an_unset_base_uses_origin_head_not_the_main_checkout() {
        let scratch = Scratch::new();
        let (repo, _remote) = repo_with_dev_on_origin(&scratch);
        git(&repo, &["switch", "-c", "elsewhere"]);
        git(&repo, &["commit", "--allow-empty", "-m", "local only"]);
        let core = core_with_fake_cli(&scratch);
        let project = core.add_project(&repo).unwrap().project;
        assert_eq!(
            core.project_base(&project.id).unwrap().name.as_deref(),
            Some("main")
        );

        let session = create_fake_session(&core, &project.id);
        assert_eq!(
            session.base_ref.as_deref(),
            Some("refs/remotes/origin/main")
        );
        assert_eq!(
            rev(&session.worktree, "HEAD"),
            rev(&repo, "refs/remotes/origin/main")
        );
        assert_ne!(rev(&session.worktree, "HEAD"), rev(&repo, "elsewhere"));
        assert!(!has_upstream(&session.worktree));
        let state = core.session_git_state(&session.id, false).unwrap();
        assert!(!state.has_own_commits && !state.pushed);
        core.session_close(&session.id, false).unwrap();
        assert!(!branch_exists(&repo, &session.branch));
    }

    #[test]
    fn changing_the_base_leaves_a_running_card_alone() {
        let scratch = Scratch::new();
        let (repo, _remote) = repo_with_dev_on_origin(&scratch);
        let core = core_with_fake_cli(&scratch);
        let project = core.add_project(&repo).unwrap().project;
        core.set_project_base(&project.id, "dev").unwrap();
        let on_dev = create_fake_session(&core, &project.id);
        core.set_project_base(&project.id, "").unwrap();

        let on_main = create_fake_session(&core, &project.id);
        assert_eq!(
            on_main.base_ref.as_deref(),
            Some("refs/remotes/origin/main")
        );
        let kept = core.session(&on_dev.id).unwrap();
        assert_eq!(kept.base_ref.as_deref(), Some("refs/remotes/origin/dev"));
        // Still measured against dev, not the project's new base, main.
        let state = core.session_git_state(&on_dev.id, false).unwrap();
        assert!(!state.has_own_commits && !state.pushed);
        assert_eq!(
            core.session_diff_stat(&on_dev.id).unwrap(),
            DiffStat::default()
        );
        core.session_close(&on_dev.id, false).unwrap();
        assert!(!branch_exists(&repo, &on_dev.branch));
        core.session_discard(&on_main.id).unwrap();
    }

    #[test]
    fn a_vanished_recorded_base_falls_back_to_the_default_branch() {
        let scratch = Scratch::new();
        let repo = scratch.repo("demo");
        git(&repo, &["branch", "staging"]);
        let core = core_with_fake_cli(&scratch);
        let project = core.add_project(&repo).unwrap().project;
        core.set_project_base(&project.id, "staging").unwrap();
        let session = create_fake_session(&core, &project.id);
        git(&repo, &["branch", "-D", "staging"]);
        git(
            &session.worktree,
            &["commit", "--allow-empty", "-m", "task"],
        );
        let state = core.session_git_state(&session.id, false).unwrap();
        assert!(state.has_own_commits && state.unpushed);
        core.session_discard(&session.id).unwrap();
    }

    #[test]
    fn new_fetches_the_base_and_a_dead_remote_does_not_block_it() {
        let scratch = Scratch::new();
        let (repo, remote) = repo_with_dev_on_origin(&scratch);
        // Someone else pushes to dev. New should start from that commit.
        let other = scratch.path.join("other");
        let cloned = Command::new("git")
            .args(["clone", "--quiet", "--branch", "dev"])
            .arg(&remote)
            .arg(&other)
            .status()
            .unwrap();
        assert!(cloned.success());
        git(&other, &["commit", "--allow-empty", "-m", "newer dev"]);
        git(&other, &["push", "origin", "dev"]);
        let newest = rev(&other, "HEAD");
        assert_ne!(rev(&repo, "refs/remotes/origin/dev"), newest);

        let core = core_with_fake_cli(&scratch);
        let project = core.add_project(&repo).unwrap().project;
        core.set_project_base(&project.id, "dev").unwrap();
        core.prefetch_base(&project.id).unwrap();
        let fresh = create_fake_session(&core, &project.id);
        assert_eq!(rev(&fresh.worktree, "HEAD"), newest);
        core.session_discard(&fresh.id).unwrap();

        // An unreachable origin: New uses the ref it already has.
        git(
            &repo,
            &[
                "remote",
                "set-url",
                "origin",
                "/nonexistent/shika-remote.git",
            ],
        );
        let core = core_with_fake_cli(&scratch);
        let started = Instant::now();
        let offline = create_fake_session(&core, &project.id);
        assert!(started.elapsed() < worktree::FETCH_TIMEOUT);
        assert_eq!(rev(&offline.worktree, "HEAD"), newest);
        assert_eq!(offline.base_ref.as_deref(), Some("refs/remotes/origin/dev"));
        core.session_discard(&offline.id).unwrap();
    }

    #[test]
    fn new_waits_for_the_picker_fetch_instead_of_starting_another() {
        let scratch = Scratch::new();
        let (repo, _remote) = repo_with_dev_on_origin(&scratch);
        // An origin that never answers, and counts how often it is asked.
        let log = scratch.path.join("upload-pack.log");
        let hang = scratch.path.join("hang.sh");
        fs::write(
            &hang,
            format!("#!/bin/sh\necho asked >> '{}'\nsleep 30\n", log.display()),
        )
        .unwrap();
        fs::set_permissions(&hang, fs::Permissions::from_mode(0o755)).unwrap();
        git(
            &repo,
            &["config", "remote.origin.uploadpack", hang.to_str().unwrap()],
        );
        let core = Arc::new(core_with_fake_cli(&scratch));
        let project = core.add_project(&repo).unwrap().project;

        let started = Instant::now();
        let picker = {
            let core = core.clone();
            let id = project.id.clone();
            std::thread::spawn(move || core.prefetch_base(&id).unwrap())
        };
        std::thread::sleep(Duration::from_millis(300));
        let session = create_fake_session(&core, &project.id);
        picker.join().unwrap();
        let took = started.elapsed();
        assert!(took >= worktree::FETCH_TIMEOUT, "{took:?}");
        assert!(
            took < worktree::FETCH_TIMEOUT + Duration::from_secs(2),
            "{took:?}"
        );
        assert_eq!(fs::read_to_string(&log).unwrap().lines().count(), 1);
        assert_eq!(
            session.base_ref.as_deref(),
            Some("refs/remotes/origin/main")
        );
        core.session_discard(&session.id).unwrap();
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

    fn write_claude_title(home: &Path, worktree: &Path, title: &str) {
        let encoded: String = worktree
            .to_string_lossy()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect();
        let dir = home.join(".claude/projects").join(encoded);
        fs::create_dir_all(&dir).unwrap();
        let line = serde_json::json!({ "type": "ai-title", "aiTitle": title });
        fs::write(dir.join("session.jsonl"), format!("{line}\n")).unwrap();
    }

    #[test]
    fn the_cli_title_renames_the_branch_once_without_the_project_name() {
        let scratch = Scratch::new();
        let repo = scratch.repo("shika");
        let mut core = core_with_fake_cli(&scratch);
        let home = scratch.path.join("home");
        core.cli_home = Some(CliHome::at(home.clone()));
        let project = core.add_project(&repo).unwrap().project;
        let session = create_fake_session(&core, &project.id);

        // No title yet: nothing changes.
        assert_eq!(core.session_apply_cli_title(&session.id).unwrap(), None);
        let prompted = core
            .session_rename_from_prompt(&session.id, "okay we need a way to blur")
            .unwrap();
        assert_eq!(prompted.branch, "okay-we-need-a-way-to-blur");

        write_claude_title(
            &home,
            &session.worktree,
            "Shika background opacity and blur",
        );
        let titled = core.session_apply_cli_title(&session.id).unwrap().unwrap();
        assert_eq!(titled.branch, "background-opacity-and-blur");
        assert_eq!(titled.title, "Shika background opacity and blur");
        assert!(titled.cli_titled);
        assert_eq!(titled.worktree, session.worktree);
        assert_eq!(
            git(&titled.worktree, &["branch", "--show-current"]).trim(),
            titled.branch
        );
        assert_eq!(core.worktree_journal().unwrap()[0].branch, titled.branch);

        // Applied once. A later title or prompt does not rename again.
        write_claude_title(&home, &session.worktree, "Something else entirely");
        assert_eq!(core.session_apply_cli_title(&session.id).unwrap(), None);
        assert_eq!(
            core.session_rename_from_prompt(&session.id, "another prompt")
                .unwrap(),
            titled
        );
        core.session_discard(&session.id).unwrap();
    }

    #[test]
    fn the_cli_title_uses_the_prefix_and_never_renames_a_pushed_branch() {
        let scratch = Scratch::new();
        let repo = scratch.repo("demo");
        local_remote(&scratch, &repo);
        let mut core = core_with_fake_cli(&scratch);
        let home = scratch.path.join("home");
        core.cli_home = Some(CliHome::at(home.clone()));
        core.save_settings(&Settings {
            branch_prefix: "hieu".into(),
            ..Settings::default()
        })
        .unwrap();
        let project = core.add_project(&repo).unwrap().project;

        let fresh = create_fake_session(&core, &project.id);
        write_claude_title(&home, &fresh.worktree, "Fix login flow");
        let titled = core.session_apply_cli_title(&fresh.id).unwrap().unwrap();
        assert_eq!(titled.branch, "hieu/fix-login-flow");

        // Pushed before the title arrived, without -u: the name stays.
        let pushed = create_fake_session(&core, &project.id);
        let pushed = core
            .session_rename_from_prompt(&pushed.id, "add a readme")
            .unwrap();
        assert_eq!(pushed.branch, "hieu/add-a-readme");
        git(
            &pushed.worktree,
            &["commit", "--allow-empty", "-m", "readme"],
        );
        git(&pushed.worktree, &["push", "origin", "HEAD"]);
        write_claude_title(&home, &pushed.worktree, "Add project README");
        let kept = core.session_apply_cli_title(&pushed.id).unwrap().unwrap();
        assert_eq!(kept.branch, "hieu/add-a-readme");
        assert_eq!(kept.title, "Add project README");

        // A worktree switched to another branch keeps it too.
        let switched = create_fake_session(&core, &project.id);
        git(&switched.worktree, &["switch", "-c", "mine"]);
        write_claude_title(&home, &switched.worktree, "Switch test");
        let kept = core.session_apply_cli_title(&switched.id).unwrap().unwrap();
        assert_eq!(kept.branch, switched.branch);
        assert_eq!(
            git(&switched.worktree, &["branch", "--show-current"]).trim(),
            "mine"
        );

        for session in [&fresh, &pushed] {
            core.session_discard(&session.id).unwrap();
        }
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
    fn external_rename_updates_the_card_record_and_journal_without_changing_the_title() {
        let scratch = Scratch::new();
        let repo = scratch.repo("demo");
        let core = core_with_fake_cli(&scratch);
        let project = core.add_project(&repo).unwrap().project;
        let session = create_fake_session(&core, &project.id);
        git(&session.worktree, &["branch", "-m", "first-rename"]);
        git(
            &session.worktree,
            &["commit", "--allow-empty", "-m", "task"],
        );
        git(&session.worktree, &["branch", "-m", "renamed-for-pr"]);
        let refreshed = core.session_refresh_branch(&session.id).unwrap();
        assert_eq!(refreshed.branch, "renamed-for-pr");
        assert_eq!(refreshed.title, session.title);
        assert_eq!(refreshed.worktree, session.worktree);
        assert_eq!(refreshed.base_ref, session.base_ref);
        assert_eq!(core.worktree_journal().unwrap()[0].branch, refreshed.branch);
        assert_eq!(
            core.session_close(&session.id, false),
            Err(Error::CloseNeedsConfirmation)
        );
        core.session_discard(&session.id).unwrap();
        assert!(!session.worktree.exists());
        assert!(!branch_exists(&repo, "renamed-for-pr"));
    }

    #[test]
    fn an_external_rename_is_not_published_in_memory_when_the_journal_cannot_save() {
        let scratch = Scratch::new();
        let repo = scratch.repo("demo");
        let core = core_with_fake_cli(&scratch);
        let project = core.add_project(&repo).unwrap().project;
        let session = create_fake_session(&core, &project.id);
        git(&session.worktree, &["branch", "-m", "renamed"]);
        let journal = core.data_dir().join("worktrees.json");
        let contents = fs::read(&journal).unwrap();
        fs::remove_file(&journal).unwrap();
        fs::create_dir(&journal).unwrap();
        assert!(core.session_refresh_branch(&session.id).is_err());
        assert!(core.session_close(&session.id, false).is_err());
        assert_eq!(core.session(&session.id).unwrap().branch, session.branch);
        assert!(session.worktree.exists());
        fs::remove_dir(&journal).unwrap();
        fs::write(&journal, contents).unwrap();
        core.session_discard(&session.id).unwrap();
        assert!(!branch_exists(&repo, "renamed"));
    }

    #[test]
    fn renamed_dirty_task_still_requires_confirmation_and_discard_uses_the_new_name() {
        let scratch = Scratch::new();
        let repo = scratch.repo("demo");
        let core = core_with_fake_cli(&scratch);
        let project = core.add_project(&repo).unwrap().project;
        let session = create_fake_session(&core, &project.id);
        git(&session.worktree, &["branch", "-m", "renamed-dirty"]);
        fs::write(session.worktree.join("unsaved"), "work").unwrap();
        assert!(core.session_git_state(&session.id, false).unwrap().dirty);
        assert!(matches!(
            core.session_close(&session.id, false),
            Err(Error::WorktreeHasChanges(_))
        ));
        assert!(session.worktree.exists());
        assert_eq!(
            core.session_push_and_close(&session.id),
            Err(Error::PushDirty)
        );
        core.session_discard(&session.id).unwrap();
        assert!(!branch_exists(&repo, "renamed-dirty"));
    }

    #[test]
    fn renamed_empty_task_closes_and_deletes_the_new_branch_without_a_poll() {
        let scratch = Scratch::new();
        let repo = scratch.repo("demo");
        let core = core_with_fake_cli(&scratch);
        let project = core.add_project(&repo).unwrap().project;
        let session = create_fake_session(&core, &project.id);
        git(&session.worktree, &["branch", "-m", "renamed-empty"]);
        core.session_close(&session.id, false).unwrap();
        assert!(!session.worktree.exists());
        assert!(!branch_exists(&repo, "renamed-empty"));
        assert!(core.worktree_journal().unwrap().is_empty());
    }

    #[test]
    fn renamed_task_can_push_and_close_or_close_after_a_push_and_merge() {
        for close_with_push in [true, false] {
            let scratch = Scratch::new();
            let (repo, _remote) = repo_with_dev_on_origin(&scratch);
            let core = core_with_fake_cli(&scratch);
            let project = core.add_project(&repo).unwrap().project;
            let session = create_fake_session(&core, &project.id);
            git(
                &session.worktree,
                &["commit", "--allow-empty", "-m", "task"],
            );
            git(&session.worktree, &["branch", "-m", "renamed-pushed"]);
            if close_with_push {
                core.session_push_and_close(&session.id).unwrap();
            } else {
                git(&session.worktree, &["push", "-u", "origin", "HEAD"]);
                git(&repo, &["merge", "--ff-only", "renamed-pushed"]);
                git(&repo, &["push", "origin", "main"]);
                core.session_close(&session.id, false).unwrap();
            }
            assert!(!session.worktree.exists());
            assert!(branch_exists(&repo, "renamed-pushed"));
            assert!(core.sessions().is_empty());
        }
    }

    #[test]
    fn missing_original_ref_is_not_proof_of_a_rename_and_detach_is_not_adopted() {
        let scratch = Scratch::new();
        let repo = scratch.repo("demo");
        let core = core_with_fake_cli(&scratch);
        let project = core.add_project(&repo).unwrap().project;
        let session = create_fake_session(&core, &project.id);
        git(&session.worktree, &["checkout", "-b", "unrelated", "main"]);
        git(&repo, &["branch", "-D", &session.branch]);
        for detached in [false, true] {
            if detached {
                git(&session.worktree, &["checkout", "--detach"]);
            }
            assert!(matches!(
                core.session_refresh_branch(&session.id),
                Err(Error::TaskBranchChanged { .. })
            ));
            assert!(matches!(
                core.session_close(&session.id, false),
                Err(Error::TaskBranchChanged { .. })
            ));
            assert!(matches!(
                core.session_discard(&session.id),
                Err(Error::TaskBranchChanged { .. })
            ));
            assert!(session.worktree.exists());
            assert_eq!(core.session(&session.id).unwrap().branch, session.branch);
        }
    }

    #[test]
    fn a_copied_branch_does_not_inherit_the_tasks_rename_identity() {
        let scratch = Scratch::new();
        let repo = scratch.repo("demo");
        let core = core_with_fake_cli(&scratch);
        let project = core.add_project(&repo).unwrap().project;
        let session = create_fake_session(&core, &project.id);
        git(&session.worktree, &["branch", "-m", "renamed"]);
        git(&session.worktree, &["branch", "-c", "copied"]);
        git(&session.worktree, &["switch", "copied"]);
        git(&repo, &["branch", "-D", "renamed"]);
        assert!(matches!(
            core.session_close(&session.id, false),
            Err(Error::TaskBranchChanged { .. })
        ));
        assert!(session.worktree.exists());
        assert_eq!(core.session(&session.id).unwrap().branch, session.branch);
    }

    #[test]
    fn recreated_original_branch_and_missing_rename_history_keep_close_blocked() {
        for recreate in [true, false] {
            let scratch = Scratch::new();
            let repo = scratch.repo("demo");
            let core = core_with_fake_cli(&scratch);
            let project = core.add_project(&repo).unwrap().project;
            let session = create_fake_session(&core, &project.id);
            git(&session.worktree, &["branch", "-m", "renamed"]);
            if recreate {
                git(&repo, &["branch", &session.branch, "main"]);
            } else {
                git(
                    &session.worktree,
                    &["reflog", "expire", "--expire=now", "refs/heads/renamed"],
                );
            }
            assert!(matches!(
                core.session_close(&session.id, false),
                Err(Error::TaskBranchChanged { .. })
            ));
            assert!(session.worktree.exists());
        }
    }

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
}
