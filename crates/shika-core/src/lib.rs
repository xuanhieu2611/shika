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
//!   [`Core::create_session_with_preparation`], [`Core::create_lead`],
//!   [`Core::open_shell`], [`Core::session_dirty`], [`Core::session_git_state`],
//!   [`Core::session_diff_stat`], [`Core::session_diff`], [`Core::session_file_diff`],
//!   [`Core::session_publish_preview`], [`Core::session_publish`],
//!   [`Core::session_rename_from_prompt`], [`Core::session_apply_cli_title`],
//!   [`Core::session_discard`],
//!   [`Core::session_push_and_close`], [`Core::session_close`], [`Core::leftover_remove`],
//!   [`Core::remove_project`], [`Core::project_base`], [`Core::project_branches`],
//!   [`Core::set_project_base`],
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
//!   [`Core::sessions`], [`Core::session`], [`Core::lead_for_project`],
//!   [`Core::workers_of`], [`Core::write`], and [`Core::resize`]. They read a
//!   small JSON file, take a short lock, or queue bytes, and are fine on the main thread. `write` never blocks on
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

mod activity;
mod agents;
mod cli_title;
pub mod control;
mod diff;
mod error;
mod path_env;
mod preparation;
#[cfg(test)]
mod preparation_tests;
mod projects;
mod pty;
mod publish;
mod session;
mod settings;
mod worktree;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

pub use activity::{AgentActivity, AgentActivityState};
pub use agents::{CliCatalog, CliPreset, picker_presets};
pub use diff::{
    Collapse, DiffLine, FileDiff, FileKey, FileStatus, Hunk, LineKind, ModeChange,
    RENDER_CAP_BYTES, SessionDiff, render_unified,
};
pub use error::{Error, Result};
pub use path_env::{LoginShellError, PathEnv};
pub use preparation::{PreparationConfig, PreparationControl, PreparationEvent};
pub use projects::{Project, ProjectAdded};
pub use pty::{PtyEvent, PtyExit, PtyId, PtySink, PtySize};
pub use publish::{ChecksState, PrChecks, PublishPreview, PublishedPr};
pub use session::{
    DiffStat, LaunchOptions, LeadEnv, Session, SessionGitState, WorkerEnv, task_title,
};
pub use settings::{
    AgentSettings, Appearance, Changes, Column, DEFAULT_DARK_THEME, DEFAULT_LIGHT_THEME, FontSize,
    KeyOverrides, Settings, ThemeMode, ThemeSettings, Translucency,
};
pub use worktree::normalize_prefix as normalize_branch_prefix;
pub use worktree::{JournalEntry, KnownBranches};

/// Verified branch-switch close preview. Confirmation is bound to these refs
/// and commit tips, not to whichever branch happens to be active later.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SwitchedBranchClose {
    session_id: String,
    pub recorded: String,
    pub current: String,
    recorded_tip: String,
    current_tip: String,
}

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

fn refuse_lead(session: &Session) -> Result<()> {
    if session.lead {
        Err(Error::LeadUnsupported)
    } else {
        Ok(())
    }
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

    /// Branches already in this clone: local heads and `origin/*`, as short
    /// names with duplicates collapsed. Does not fetch. Blocking: runs git.
    pub fn project_branches(&self, id: &str) -> Result<KnownBranches> {
        let project = self.projects.get(id)?;
        let git = self.git()?;
        let env = self.path_env().path();
        worktree::known_branches(&git, env, &project.path)
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
        let name = self.existing_or_fetched_branch(&project, branch)?;
        self.projects.set_base_branch(id, Some(name))
    }

    /// The name of `branch` once it exists locally or on origin, fetching it
    /// from origin when it is new there. `origin/dev` is taken as `dev`.
    /// Blocking: runs git and may fetch.
    fn existing_or_fetched_branch(&self, project: &Project, branch: &str) -> Result<String> {
        let git = self.git()?;
        let env = self.path_env().path();
        let repo = project.path.as_path();
        let mut names = vec![branch];
        if let Some(short) = branch.strip_prefix("origin/") {
            names.push(short);
        }
        for name in &names {
            if worktree::configured_ref(&git, env, repo, name)?.is_some() {
                return Ok(name.to_string());
            }
        }
        if worktree::has_origin(&git, env, repo) {
            for name in &names {
                if worktree::fetch_branch(&git, env, repo, name, worktree::FETCH_TIMEOUT)
                    && worktree::configured_ref(&git, env, repo, name)?.is_some()
                {
                    return Ok(name.to_string());
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

    /// Best-effort Pi lifecycle metadata. Poll on a worker: this reads at most
    /// 129 bytes and never takes the core operations lock. None means use the
    /// terminal fallback (other agents, unsupported Pi, bad/missing metadata,
    /// or an ended session). This is a latest snapshot, not an event queue;
    /// consumers should deduplicate by seq. No blocked state is inferred.
    pub fn session_activity(&self, session_id: &str) -> Option<AgentActivity> {
        self.sessions.activity(session_id)?.read()
    }

    /// Creates `<repo>/.worktrees/shika-draft-<id>` on a new branch from the
    /// project's base branch, journals it, and starts the CLI there on a PTY
    /// of `size` that feeds `sink`. The base is fetched from origin first,
    /// best effort and bounded (see [`Core::prefetch_base`]). A configured
    /// base that exists nowhere is an error. Failed launches remove only a
    /// provably untouched worktree; changed or unverifiable work is journaled
    /// for explicit cleanup. `options` carries an optional initial prompt
    /// (passed to the CLI as its positional argument; branch naming from it
    /// is the caller's job through [`Core::session_rename_from_prompt`]) and
    /// the owning Lead. Blocking: runs git and optional setup commands.
    pub fn create_session(
        &self,
        project_id: &str,
        preset_id: &str,
        size: PtySize,
        sink: impl PtySink,
        options: LaunchOptions,
    ) -> Result<Session> {
        self.create_session_with_preparation(
            project_id,
            preset_id,
            size,
            sink,
            options,
            PreparationControl::default(),
            |_| {},
        )
    }

    /// Same launch, with cancellable preparation and live progress. Commands
    /// run off the app thread, outside the operation lock. At most two setups
    /// run at once. No agent PTY exists until all preparation succeeds.
    #[allow(clippy::too_many_arguments)]
    pub fn create_session_with_preparation(
        &self,
        project_id: &str,
        preset_id: &str,
        size: PtySize,
        sink: impl PtySink,
        options: LaunchOptions,
        control: PreparationControl,
        mut report: impl FnMut(PreparationEvent),
    ) -> Result<Session> {
        if let Some(prompt) = &options.prompt {
            session::check_prompt(prompt)?;
        }
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
        // Neither fetching nor installation holds up close or discard. A
        // per-launch base replaces the project's for this task only.
        let mut launch_project = self.projects.get(project_id)?;
        if let Some(base) = options.base.as_deref() {
            launch_project.base_branch =
                Some(self.existing_or_fetched_branch(&launch_project, base.trim())?);
        }
        let launch_base = launch_project.base_branch.clone();
        self.freshen_base(&launch_project);
        let (project, base, draft) = {
            let _guard = self.operations.lock().unwrap_or_else(|e| e.into_inner());
            control.check()?;
            let project = self.projects.get(project_id)?;
            if preparation::load(&project.path)? != config {
                return Err(Error::PreparationNeedsApproval);
            }
            let git = self.git()?;
            let id = session::new_id(&self.taken_ids()?);
            let base =
                worktree::resolve_base(&git, env.path(), &project.path, launch_base.as_deref())?;
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
            // Optional, session-owned shim. Installation failure keeps the
            // original native CLI launch, with no config changes.
            let activity = (preset.id == "pi")
                .then(activity::ActivityBridge::install)
                .flatten();
            // Only a Lead-started worker's agent gets the control variables;
            // its shell tabs never do.
            let (search_path, control_vars) = match &options.control {
                Some(control) => (
                    session::control_path(&control.bin_dir, env.path()),
                    session::control_env(&control.socket, &control.token),
                ),
                None => (env.path().to_string(), Vec::new()),
            };
            let mut request = SpawnRequest {
                program,
                args: agents::launch_flags(&preset, &draft.path),
                cwd: draft.path.clone(),
                path: search_path,
                size,
                env: control_vars,
            };
            if let Some(activity) = &activity {
                activity.wire(&mut request);
            }
            // After the extension flags, so the prompt is the last argument.
            request.args = match session::launch_args(
                std::mem::take(&mut request.args),
                options.prompt.as_deref(),
            ) {
                Ok(args) => args,
                Err(err) => {
                    if let Some(activity) = &activity {
                        activity.cleanup();
                    }
                    return Err(err);
                }
            };
            let pty = match self
                .ptys
                .open(request, activity::activity_sink(activity.clone(), sink))
            {
                Ok(pty) => pty,
                Err(err) => {
                    if let Some(activity) = &activity {
                        activity.cleanup();
                    }
                    return Err(err);
                }
            };
            if control.is_cancelled() {
                self.ptys.close(pty);
                if let Some(activity) = &activity {
                    activity.cleanup();
                }
                return Err(Error::PreparationCancelled);
            }
            let session = Session {
                id: draft.branch.trim_start_matches("shika-draft-").to_string(),
                project_id: project.id.clone(),
                preset_id: preset.id,
                title: format!("New {}", preset.name),
                manual_title: false,
                preset_name: preset.name,
                branch: draft.branch.clone(),
                repo: project.path.clone(),
                worktree: draft.path.clone(),
                base_ref: base.reference.clone(),
                pty,
                shell_ptys: Vec::new(),
                cli_titled: false,
                lead: false,
                started_by: options.started_by.clone(),
            };
            if let Some(activity) = activity {
                self.sessions.remember_activity(&session.id, activity);
            }
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

    /// Starts a project's Lead: an agent CLI in its own detached worktree,
    /// `<repo>/.worktrees/shika-lead-<id>`, created with `git worktree add
    /// --detach` from the same start point New uses (fetched first, best
    /// effort). No branch is created and preparation never runs. The CLI gets
    /// the preset's flags plus `lead_env.prompt` as its last argument, `PATH`
    /// starting with `lead_env.bin_dir`, and `SHIKA_SOCKET` and `SHIKA_TOKEN`.
    /// Every other PTY Shika opens has those two variables removed.
    ///
    /// The returned session has `lead: true`, the title `Lead`, and an EMPTY
    /// `branch` (also empty in the journal). The app must not rename, publish,
    /// diff-stat, or open shells for it; the Core methods that would refuse it
    /// with [`Error::LeadUnsupported`]. Close and discard work: they remove
    /// the worktree and have no branch to delete. A second Lead for a project
    /// is [`Error::LeadExists`]. Blocking: runs git.
    pub fn create_lead(
        &self,
        project_id: &str,
        preset_id: &str,
        size: PtySize,
        sink: impl PtySink,
        lead_env: LeadEnv,
    ) -> Result<Session> {
        session::check_prompt(&lead_env.prompt)?;
        let env = self.path_env();
        let preset = agents::presets_from(env)
            .into_iter()
            .find(|preset| preset.id == preset_id)
            .ok_or(Error::UnknownCli)?;
        let program = preset
            .path
            .clone()
            .ok_or_else(|| Error::CliNotFound(preset.name.clone()))?;
        if self.lead_for_project(project_id).is_some() {
            return Err(Error::LeadExists);
        }
        self.freshen_base(&self.projects.get(project_id)?);
        let _guard = self.operations.lock().unwrap_or_else(|e| e.into_inner());
        let project = self.projects.get(project_id)?;
        // Checked again under the lock: two starts cannot both pass.
        if self.lead_for_project(project_id).is_some() {
            return Err(Error::LeadExists);
        }
        let git = self.git()?;
        let id = session::new_id(&self.taken_ids()?);
        let base = worktree::resolve_base(
            &git,
            env.path(),
            &project.path,
            project.base_branch.as_deref(),
        )?;
        let path = worktree::create_lead(&git, env.path(), &project.path, &id, base.start())?;
        let entry = JournalEntry {
            project_id: project.id.clone(),
            branch: String::new(),
            path: path.clone(),
            base_ref: base.reference.clone(),
        };
        if let Err(err) = self.journal.add(&entry) {
            let _ = worktree::remove_worktree(&git, env.path(), &project.path, &path, true);
            return Err(err);
        }
        // Keeps leftovers from offering the tree while the launch completes.
        self.preparing
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(
                path.clone(),
                (project.id.clone(), PreparationControl::default()),
            );
        let launched = (|| {
            let activity = (preset.id == "pi")
                .then(activity::ActivityBridge::install)
                .flatten();
            let search_path = session::control_path(&lead_env.bin_dir, env.path());
            let mut request = SpawnRequest {
                program,
                args: agents::launch_flags(&preset, &path),
                cwd: path.clone(),
                path: search_path,
                size,
                env: session::control_env(&lead_env.socket, &lead_env.token),
            };
            if let Some(activity) = &activity {
                activity.wire(&mut request);
            }
            request.args =
                session::launch_args(std::mem::take(&mut request.args), Some(&lead_env.prompt))?;
            let pty = match self
                .ptys
                .open(request, activity::activity_sink(activity.clone(), sink))
            {
                Ok(pty) => pty,
                Err(err) => {
                    if let Some(activity) = &activity {
                        activity.cleanup();
                    }
                    return Err(err);
                }
            };
            let session = Session {
                id: id.clone(),
                project_id: project.id.clone(),
                preset_id: preset.id.clone(),
                preset_name: preset.name.clone(),
                title: "Lead".into(),
                manual_title: false,
                branch: String::new(),
                repo: project.path.clone(),
                worktree: path.clone(),
                base_ref: base.reference.clone(),
                pty,
                shell_ptys: Vec::new(),
                cli_titled: false,
                lead: true,
                started_by: None,
            };
            if let Some(activity) = activity {
                self.sessions.remember_activity(&session.id, activity);
            }
            self.sessions.insert(session.clone());
            Ok(session)
        })();
        self.preparing
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&path);
        if launched.is_err() {
            // Nothing ran in the tree, so it is safe to remove outright.
            if worktree::remove_worktree(&git, env.path(), &project.path, &path, true).is_ok() {
                self.journal.remove_path(&path)?;
            }
        }
        launched
    }

    /// The project's live Lead, if any. Quick.
    pub fn lead_for_project(&self, project_id: &str) -> Option<Session> {
        self.sessions
            .all()
            .into_iter()
            .find(|session| session.lead && session.project_id == project_id)
    }

    /// The live workers a Lead started, in creation order. Quick.
    pub fn workers_of(&self, lead_id: &str) -> Vec<Session> {
        self.sessions
            .all()
            .into_iter()
            .filter(|session| session.started_by.as_deref() == Some(lead_id))
            .collect()
    }

    /// Ids already used by a live session or a journaled worktree, whether a
    /// draft or a Lead, so a new id never collides with either. A draft's
    /// branch can be renamed, so the worktree folder name is the stable source.
    fn taken_ids(&self) -> Result<Vec<String>> {
        let mut ids = self.sessions.ids();
        for entry in self.journal.list()? {
            ids.extend(entry.branch.strip_prefix("shika-draft-").map(str::to_owned));
            let name = entry
                .path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            ids.extend(
                ["shika-draft-", "shika-lead-"]
                    .iter()
                    .find_map(|prefix| name.strip_prefix(prefix).map(str::to_owned)),
            );
        }
        Ok(ids)
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
        refuse_lead(&session)?;
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

    /// Set a manual display name without touching Git, PTYs, CLI files, or the
    /// journal. Future publish previews use it; automatic branch naming stays
    /// independent. A short metadata-only call, safe on the UI thread.
    pub fn session_set_title(&self, id: &str, title: &str) -> Result<Session> {
        self.sessions.set_title(id, title)
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
        refuse_lead(&session)?;
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
        refuse_lead(&session)?;
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
        if session.lead {
            // A detached Lead has no branch to compare: only its edits count.
            return Ok(SessionGitState {
                dirty: worktree::is_dirty(&self.git()?, self.path_env().path(), &session.worktree)?,
                unpushed: false,
                pushed: false,
                has_own_commits: false,
                agent_working,
            });
        }
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

    /// Separate recovery path; normal close/discard/push retain branch identity
    /// protection. This preview is read-only and never adopts the current ref.
    pub fn session_switched_close_check(&self, id: &str) -> Result<SwitchedBranchClose> {
        let _guard = self
            .operations
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        self.switched_close_check_locked(id)
    }

    fn switched_close_check_locked(&self, id: &str) -> Result<SwitchedBranchClose> {
        let session = self.sessions.get(id).ok_or(Error::UnknownSession)?;
        refuse_lead(&session)?;
        let git = self.git()?;
        let path = self.path_env().path();
        let current = worktree::head_branch(&git, path, &session.worktree)?
            .ok_or_else(|| Error::SwitchedCloseUnsafe("Detached HEAD cannot use branch-switch close. Return to the task branch before closing.".into()))?;
        if current == session.branch {
            return Err(Error::SwitchedCloseUnsafe(
                "The branch changed. Close again to check its current state.".into(),
            ));
        }
        let (recorded_tip, current_tip) =
            worktree::switched_close_tips(&git, path, &session, &current)?;
        Ok(SwitchedBranchClose {
            session_id: session.id,
            recorded: session.branch,
            current,
            recorded_tip,
            current_tip,
        })
    }

    /// Explicit confirmation stops all owned PTYs and removes only the clean
    /// worktree. Both branches survive. Unsafe work has no force/discard path.
    pub fn session_close_switched(&self, id: &str, preview: &SwitchedBranchClose) -> Result<()> {
        let _guard = self
            .operations
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let changed = || {
            Error::SwitchedCloseUnsafe(
                "The branches changed since confirmation. Close again to recheck.".into(),
            )
        };
        if self.switched_close_check_locked(id)? != *preview {
            return Err(changed());
        }
        let session = self.sessions.get(id).ok_or(Error::UnknownSession)?;
        self.hang_up(&session);
        if self.switched_close_check_locked(id)? != *preview {
            return Err(changed());
        }
        worktree::remove_worktree(
            &self.git()?,
            self.path_env().path(),
            &session.repo,
            &session.worktree,
            false,
        )?;
        self.forget_session(&session)
    }

    /// Blocking. What the task changed: committed and uncommitted work
    /// against where the branch left the base it started from. Takes no lock,
    /// so a background refresh never holds up close or discard.
    pub fn session_diff_stat(&self, id: &str) -> Result<DiffStat> {
        let session = self.sessions.get(id).ok_or(Error::UnknownSession)?;
        if session.lead {
            // A Lead is not measured against a base; it shows no stat.
            return Ok(DiffStat::default());
        }
        worktree::diff_stat(
            &self.git()?,
            self.path_env().path(),
            &session.repo,
            &session.worktree,
            session.base_ref.as_deref(),
        )
    }

    /// Blocking. The task's whole change for the Changes panel: every file's
    /// hunks, measured exactly as [`Core::session_diff_stat`] measures it, so
    /// `stat` matches the card. Reads only (`GIT_OPTIONAL_LOCKS=0`) and takes
    /// no lock. Caps keep a huge change cheap: a file with more than 2,000
    /// changed lines, or a patch or untracked file over 1 MiB, comes
    /// collapsed, and so does every file after about 50,000 parsed lines;
    /// [`FileDiff::collapsed`] says which cap. Expand one with
    /// [`Core::session_file_diff`]. A git failure is [`Error::ReadChanges`]
    /// with git's first error line, and so is a worktree whose `.git` is
    /// missing or broken: git never reads the main checkout in its place.
    pub fn session_diff(&self, id: &str) -> Result<SessionDiff> {
        let session = self.sessions.get(id).ok_or(Error::UnknownSession)?;
        refuse_lead(&session)?;
        worktree::session_diff(
            &self.git()?,
            self.path_env().path(),
            &session.repo,
            &session.worktree,
            session.base_ref.as_deref(),
        )
    }

    /// Blocking. One file of an earlier [`Core::session_diff`], read again
    /// from the tree without the task caps, against the same base, up to a
    /// hard limit of 100,000 lines or 16 MiB; past that the file keeps its
    /// first hunks and counts the rest in `hidden_lines`. Pass the file's
    /// [`FileDiff::key`] and replace that file with the result. One git
    /// process for a tracked file, none for an untracked one. None when the
    /// file no longer differs; refresh the whole diff then.
    pub fn session_file_diff(&self, id: &str, key: &FileKey) -> Result<Option<FileDiff>> {
        let session = self.sessions.get(id).ok_or(Error::UnknownSession)?;
        refuse_lead(&session)?;
        worktree::file_diff(&self.git()?, self.path_env().path(), &session.worktree, key)
    }

    /// Preview a confirmed commit/push/PR operation without changing the
    /// user's index. Blocking: requires authenticated GitHub CLI on login PATH.
    pub fn session_publish_preview(&self, id: &str) -> Result<PublishPreview> {
        let _guard = self.operations.lock().unwrap_or_else(|e| e.into_inner());
        let session = self.sessions.get(id).ok_or(Error::UnknownSession)?;
        refuse_lead(&session)?;
        let session = self.ensure_session_branch(&session)?;
        let gh = self.path_env().resolve("gh").ok_or_else(|| {
            Error::Publish(
                "gh not found on PATH. Install GitHub CLI and run gh auth login in a shell.".into(),
            )
        })?;
        publish::preview(&self.git()?, &gh, self.path_env().path(), &session)
    }

    /// Confirmed publish. Never merges, force-pushes, or closes the task.
    /// Partial success is retained for retry; a stale preview is refused.
    pub fn session_publish(
        &self,
        preview: &PublishPreview,
        target: &str,
        title: &str,
    ) -> Result<PublishedPr> {
        let _guard = self.operations.lock().unwrap_or_else(|e| e.into_inner());
        let session = self
            .sessions
            .get(&preview.session_id)
            .ok_or(Error::UnknownSession)?;
        refuse_lead(&session)?;
        let session = self.ensure_session_branch(&session)?;
        let gh = self.path_env().resolve("gh").ok_or_else(|| {
            Error::Publish(
                "gh not found on PATH. Install GitHub CLI and run gh auth login in a shell.".into(),
            )
        })?;
        publish::publish(
            &self.git()?,
            &gh,
            self.path_env().path(),
            &session,
            preview,
            target,
            title,
        )
    }

    /// Blocking, one bounded `gh pr view`. Read-only and takes no lock, like
    /// the diff stat, so a slow network never holds up close or publishing.
    pub fn session_pr_checks(&self, id: &str, repository: &str, number: u64) -> Result<PrChecks> {
        let session = self.sessions.get(id).ok_or(Error::UnknownSession)?;
        refuse_lead(&session)?;
        let gh = self
            .path_env()
            .resolve("gh")
            .ok_or_else(|| Error::Publish("gh not found on PATH.".into()))?;
        publish::checks(
            &gh,
            self.path_env().path(),
            &session.worktree,
            repository,
            number,
        )
    }

    /// Blocking but local: the commit `origin/<branch>` points at, which a
    /// push from the task's shell or agent moves. None when there is none.
    pub fn session_pushed_head(&self, id: &str) -> Result<Option<String>> {
        let session = self.sessions.get(id).ok_or(Error::UnknownSession)?;
        if session.lead {
            return Ok(None);
        }
        worktree::pushed_head(
            &self.git()?,
            self.path_env().path(),
            &session.worktree,
            &session.branch,
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
        if session.lead {
            return self.close_lead(&session, false, true);
        }
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
        refuse_lead(&self.sessions.get(id).ok_or(Error::UnknownSession)?)?;
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
        let session = self.sessions.get(id).ok_or(Error::UnknownSession)?;
        if session.lead {
            return self.close_lead(&session, agent_working, false);
        }
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

    /// Called under the operations lock. A Lead has no branch to keep or push:
    /// closing it stops its PTY and removes the detached worktree. Without
    /// `discard`, edits or a working agent need confirmation first. Commits
    /// made on the detached HEAD are not preserved; the Lead never codes.
    fn close_lead(&self, lead: &Session, agent_working: bool, discard: bool) -> Result<()> {
        let git = self.git()?;
        let path = self.path_env().path();
        if !discard {
            if worktree::is_dirty(&git, path, &lead.worktree)? {
                return Err(Error::WorktreeHasChanges(None));
            }
            if agent_working {
                return Err(Error::CloseNeedsConfirmation);
            }
        }
        self.hang_up(lead);
        if !discard && worktree::is_dirty(&git, path, &lead.worktree)? {
            return Err(Error::WorktreeHasChanges(None));
        }
        worktree::remove_worktree(&git, path, &lead.repo, &lead.worktree, discard)?;
        self.forget_session(lead)
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
        if session.lead {
            return Ok(session);
        }
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
        if entry.branch.is_empty() {
            // A Lead's detached worktree: nothing to delete but the tree.
            worktree::remove_worktree(&git, path_env, repo, &entry.path, true)?;
            return self.journal.remove_path(&entry.path);
        }
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
            "[\n  {\n    \"id\": \"18db38e2f78faa00\",\n    \"name\": \"sample-project\",\n    \"path\": \"/Users/x/code/sample-project\"\n  }\n]\n",
        )
        .unwrap();
        fs::write(
            data.join("worktrees.json"),
            "[\n  {\n    \"projectId\": \"18db38e2f78faa00\",\n    \"branch\": \"shika-draft-1\",\n    \"path\": \"/Users/x/code/sample-project/.worktrees/shika-draft-1\"\n  }\n]\n",
        )
        .unwrap();
        let core = Core::open(&data).unwrap();
        assert_eq!(core.data_dir(), data);
        assert_eq!(
            core.projects().unwrap(),
            [Project {
                id: "18db38e2f78faa00".into(),
                name: "sample-project".into(),
                path: PathBuf::from("/Users/x/code/sample-project"),
                base_branch: None,
                approved_preparation: None,
            }]
        );
        assert_eq!(
            core.worktree_journal().unwrap(),
            [JournalEntry {
                project_id: "18db38e2f78faa00".into(),
                branch: "shika-draft-1".into(),
                path: PathBuf::from("/Users/x/code/sample-project/.worktrees/shika-draft-1"),
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
            .create_session(
                &added.project.id,
                "claude",
                PtySize::new(30, 90),
                sink,
                LaunchOptions::default(),
            )
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
            core.create_session(
                &added.project.id,
                "cursor",
                PtySize::default(),
                sink,
                LaunchOptions::default()
            )
            .unwrap_err(),
            Error::CliNotFound("Cursor CLI".into())
        );
        let (sink, _rx) = channel_sink();
        assert_eq!(
            core.create_session(
                &added.project.id,
                "kiro",
                PtySize::default(),
                sink,
                LaunchOptions::default()
            )
            .unwrap_err(),
            Error::UnknownCli
        );
        let (sink, _rx) = channel_sink();
        assert_eq!(
            core.create_session(
                "missing",
                "claude",
                PtySize::default(),
                sink,
                LaunchOptions::default()
            )
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
            .create_session(
                &added.project.id,
                "claude",
                PtySize::default(),
                sink,
                LaunchOptions::default(),
            )
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
        core.create_session(
            project_id,
            "claude",
            PtySize::new(30, 90),
            sink,
            LaunchOptions::default(),
        )
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
    fn a_launch_base_applies_to_that_task_only() {
        let scratch = Scratch::new();
        let repo = scratch.repo("demo");
        git(&repo, &["switch", "-c", "staging"]);
        git(&repo, &["commit", "--allow-empty", "-m", "staging work"]);
        git(&repo, &["switch", "main"]);
        let core = core_with_fake_cli(&scratch);
        let project = core.add_project(&repo).unwrap().project;

        let (sink, _rx) = channel_sink();
        let session = core
            .create_session(
                &project.id,
                "claude",
                PtySize::new(30, 90),
                sink,
                LaunchOptions {
                    base: Some("staging".into()),
                    ..LaunchOptions::default()
                },
            )
            .unwrap();
        assert_eq!(session.base_ref.as_deref(), Some("refs/heads/staging"));
        assert_eq!(rev(&session.worktree, "HEAD"), rev(&repo, "staging"));
        // The project's saved base is untouched, so the next New is plain.
        assert_eq!(core.projects().unwrap()[0].base_branch, None);
        let plain = create_fake_session(&core, &project.id);
        assert_eq!(rev(&plain.worktree, "HEAD"), rev(&repo, "main"));

        // A base that exists nowhere fails before anything is created.
        let journal = core.worktree_journal().unwrap();
        let (sink, _rx) = channel_sink();
        let result = core.create_session(
            &project.id,
            "claude",
            PtySize::new(30, 90),
            sink,
            LaunchOptions {
                base: Some("nope".into()),
                ..LaunchOptions::default()
            },
        );
        assert!(matches!(result, Err(Error::NoSuchBranch(_))));
        assert_eq!(core.worktree_journal().unwrap(), journal);
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
            core.create_session(
                &project.id,
                "claude",
                PtySize::default(),
                sink,
                LaunchOptions::default()
            )
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
    fn manual_task_names_leave_git_and_ownership_unchanged() {
        let scratch = Scratch::new();
        let repo = scratch.repo("demo");
        let mut core = core_with_fake_cli(&scratch);
        let home = scratch.path.join("home");
        core.cli_home = Some(CliHome::at(home.clone()));
        let project = core.add_project(&repo).unwrap().project;
        let original = create_fake_session(&core, &project.id);
        let journal = core.worktree_journal().unwrap();
        let head = git(&original.worktree, &["rev-parse", "HEAD"]);
        let index = git(&original.worktree, &["write-tree"]);
        let mut expected = original.clone();
        expected.title = "My display name".into();
        expected.manual_title = true;
        // Name mutation must not wait for a Git operation or run any Git command.
        let guard = core.operations.lock().unwrap();
        assert_eq!(
            core.session_set_title(&original.id, " My display name ")
                .unwrap(),
            expected
        );
        drop(guard);
        assert_eq!(core.worktree_journal().unwrap(), journal);
        assert_eq!(git(&original.worktree, &["rev-parse", "HEAD"]), head);
        assert_eq!(git(&original.worktree, &["write-tree"]), index);
        assert_eq!(
            git(&original.worktree, &["branch", "--show-current"]).trim(),
            original.branch
        );
        let prompted = core
            .session_rename_from_prompt(&original.id, "First prompt")
            .unwrap();
        assert_eq!(prompted.title, "My display name");
        assert_eq!(prompted.branch, "first-prompt");
        write_claude_title(&home, &original.worktree, "CLI summary");
        let titled = core.session_apply_cli_title(&original.id).unwrap().unwrap();
        assert_eq!(titled.title, "My display name");
        assert_eq!(titled.branch, "cli-summary");
        assert!(titled.cli_titled);
        let before = core.worktree_journal().unwrap();
        core.session_set_title(&original.id, "Revised name")
            .unwrap();
        assert_eq!(core.session(&original.id).unwrap().branch, "cli-summary");
        assert_eq!(core.worktree_journal().unwrap(), before);
        assert_eq!(core.session_apply_cli_title(&original.id).unwrap(), None);
        core.session_discard(&original.id).unwrap();
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
            branch_prefix: "dev".into(),
            ..Settings::default()
        })
        .unwrap();
        let project = core.add_project(&repo).unwrap().project;

        let fresh = create_fake_session(&core, &project.id);
        write_claude_title(&home, &fresh.worktree, "Fix login flow");
        let titled = core.session_apply_cli_title(&fresh.id).unwrap().unwrap();
        assert_eq!(titled.branch, "dev/fix-login-flow");

        // Pushed before the title arrived, without -u: the name stays.
        let pushed = create_fake_session(&core, &project.id);
        let pushed = core
            .session_rename_from_prompt(&pushed.id, "add a readme")
            .unwrap();
        assert_eq!(pushed.branch, "dev/add-a-readme");
        git(
            &pushed.worktree,
            &["commit", "--allow-empty", "-m", "readme"],
        );
        git(&pushed.worktree, &["push", "origin", "HEAD"]);
        write_claude_title(&home, &pushed.worktree, "Add project README");
        let kept = core.session_apply_cli_title(&pushed.id).unwrap().unwrap();
        assert_eq!(kept.branch, "dev/add-a-readme");
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
    fn switched_close_preserves_both_branches_after_push_or_merge() {
        for (merge, upstream) in [(false, true), (true, true), (false, false)] {
            let scratch = Scratch::new();
            let (repo, _remote) = repo_with_dev_on_origin(&scratch);
            let core = core_with_fake_cli(&scratch);
            let project = core.add_project(&repo).unwrap().project;
            let session = create_fake_session(&core, &project.id);
            git(&session.worktree, &["switch", "-c", "better-name"]);
            git(
                &session.worktree,
                &["commit", "--allow-empty", "-m", "task"],
            );
            if upstream {
                git(&session.worktree, &["push", "-u", "origin", "HEAD"]);
            } else {
                git(&session.worktree, &["push", "origin", "HEAD"]);
            }
            if merge {
                git(&repo, &["merge", "--ff-only", "better-name"]);
                git(&repo, &["push", "origin", "main"]);
            }
            let (sink, _rx) = channel_sink();
            let shell = core
                .open_shell(&session.id, PtySize::default(), sink)
                .unwrap();
            let preview = core.session_switched_close_check(&session.id).unwrap();
            assert_eq!(preview.recorded, session.branch);
            assert_eq!(preview.current, "better-name");
            assert_eq!(core.session(&session.id).unwrap().branch, session.branch);
            core.session_close_switched(&session.id, &preview).unwrap();
            assert!(!session.worktree.exists());
            assert!(branch_exists(&repo, &session.branch));
            assert!(branch_exists(&repo, "better-name"));
            assert!(core.sessions().is_empty());
            assert!(core.journal.list().unwrap().is_empty());
            assert!(core.write(shell, b"echo still-running\n").is_err());
        }
    }

    #[test]
    fn switched_close_accepts_independently_published_or_integrated_branches() {
        for integrated in [true, false] {
            let scratch = Scratch::new();
            let (repo, _remote) = repo_with_dev_on_origin(&scratch);
            let core = core_with_fake_cli(&scratch);
            let project = core.add_project(&repo).unwrap().project;
            let session = create_fake_session(&core, &project.id);
            git(
                &session.worktree,
                &["commit", "--allow-empty", "-m", "original work"],
            );
            if integrated {
                git(&repo, &["merge", "--ff-only", &session.branch]);
                git(&repo, &["push", "origin", "main"]);
            } else {
                git(&session.worktree, &["push", "origin", "HEAD"]);
            }
            git(&session.worktree, &["switch", "-c", "review/other", "main"]);
            git(
                &session.worktree,
                &["commit", "--allow-empty", "-m", "other work"],
            );
            git(&session.worktree, &["push", "origin", "HEAD"]);
            let preview = core.session_switched_close_check(&session.id).unwrap();
            core.session_close_switched(&session.id, &preview).unwrap();
            assert!(branch_exists(&repo, &session.branch));
            assert!(branch_exists(&repo, "review/other"));
        }
    }

    #[test]
    fn switched_close_accepts_empty_local_tasks_without_a_remote() {
        let scratch = Scratch::new();
        let repo = scratch.repo("demo");
        let core = core_with_fake_cli(&scratch);
        let project = core.add_project(&repo).unwrap().project;
        let session = create_fake_session(&core, &project.id);
        git(&session.worktree, &["switch", "-c", "other"]);
        let preview = core.session_switched_close_check(&session.id).unwrap();
        core.session_close_switched(&session.id, &preview).unwrap();
        assert!(branch_exists(&repo, &session.branch));
        assert!(branch_exists(&repo, "other"));
    }

    #[test]
    fn switched_close_refuses_unpublished_work_on_either_branch() {
        for original in [true, false] {
            let scratch = Scratch::new();
            let (repo, _remote) = repo_with_dev_on_origin(&scratch);
            let core = core_with_fake_cli(&scratch);
            let project = core.add_project(&repo).unwrap().project;
            let session = create_fake_session(&core, &project.id);
            if original {
                git(
                    &session.worktree,
                    &["commit", "--allow-empty", "-m", "hidden task"],
                );
            }
            git(&session.worktree, &["switch", "-c", "other", "main"]);
            if !original {
                git(
                    &session.worktree,
                    &["commit", "--allow-empty", "-m", "other task"],
                );
            }
            assert!(matches!(
                core.session_switched_close_check(&session.id),
                Err(Error::SwitchedCloseUnsafe(_))
            ));
            assert!(session.worktree.exists());
            assert!(branch_exists(&repo, &session.branch));
            assert!(!core.sessions().is_empty());
            assert!(
                core.write(session.pty, b"").is_ok(),
                "refusal kept the agent"
            );
        }
    }

    #[test]
    fn switched_close_rechecks_confirmation_and_refuses_dirty_detached_or_missing_refs() {
        for change in [
            "dirty",
            "current-commit",
            "original-commit",
            "switch",
            "detach",
            "delete",
        ] {
            let scratch = Scratch::new();
            let (repo, _remote) = repo_with_dev_on_origin(&scratch);
            let core = core_with_fake_cli(&scratch);
            let project = core.add_project(&repo).unwrap().project;
            let session = create_fake_session(&core, &project.id);
            git(&session.worktree, &["switch", "-c", "other"]);
            let preview = core.session_switched_close_check(&session.id).unwrap();
            match change {
                "dirty" => fs::write(session.worktree.join("untracked"), "keep me").unwrap(),
                "current-commit" => {
                    git(
                        &session.worktree,
                        &["commit", "--allow-empty", "-m", "later"],
                    );
                    git(&session.worktree, &["push", "origin", "HEAD"]);
                }
                "original-commit" => {
                    git(&session.worktree, &["switch", &session.branch]);
                    git(
                        &session.worktree,
                        &["commit", "--allow-empty", "-m", "later"],
                    );
                    git(&session.worktree, &["push", "origin", "HEAD"]);
                    git(&session.worktree, &["switch", "other"]);
                }
                "switch" => {
                    git(&session.worktree, &["switch", "-c", "third"]);
                }
                "detach" => {
                    git(&session.worktree, &["switch", "--detach"]);
                }
                "delete" => {
                    git(&repo, &["branch", "-D", &session.branch]);
                }
                _ => unreachable!(),
            }
            assert!(
                core.session_close_switched(&session.id, &preview).is_err(),
                "{change}"
            );
            assert!(session.worktree.exists(), "{change}");
            assert!(!core.sessions().is_empty());
            assert!(!core.journal.list().unwrap().is_empty());
            if change == "dirty" {
                assert!(core.session_switched_close_check(&session.id).is_err());
            }
        }
    }

    #[test]
    fn switched_close_does_not_treat_local_main_as_proof_of_publication() {
        let scratch = Scratch::new();
        let (repo, _remote) = repo_with_dev_on_origin(&scratch);
        let core = core_with_fake_cli(&scratch);
        let project = core.add_project(&repo).unwrap().project;
        let session = create_fake_session(&core, &project.id);
        git(&repo, &["switch", "dev"]);
        git(&session.worktree, &["switch", "main"]);
        git(
            &session.worktree,
            &["commit", "--allow-empty", "-m", "unpublished main"],
        );
        assert!(core.session_switched_close_check(&session.id).is_err());
        assert!(session.worktree.exists());
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

    /// Prints what the launch handed the CLI, then idles like `fake_cli`.
    const REPORTING_CLI: &str = "#!/bin/sh\nfor a in \"$@\"; do last=\"$a\"; done\nprintf 'last:%s\\n' \"$last\"\nprintf 'sock:%s\\n' \"$SHIKA_SOCKET\"\nprintf 'token:%s\\n' \"$SHIKA_TOKEN\"\nprintf 'path:%s\\n' \"$PATH\"\nprintf 'end-report\\n'\nwhile IFS= read -r line; do :; done\n";

    fn core_with_reporting_cli(scratch: &Scratch) -> Core {
        let core = core_with_fake_cli(scratch);
        fs::write(scratch.path.join("bin").join("claude"), REPORTING_CLI).unwrap();
        core
    }

    fn lead_env(scratch: &Scratch) -> LeadEnv {
        LeadEnv {
            socket: scratch.path.join("control/sock"),
            token: "lead-token".into(),
            bin_dir: scratch.path.join("control/bin"),
            prompt: "You lead this project. Run shika help.".into(),
        }
    }

    fn create_lead(core: &Core, scratch: &Scratch, project_id: &str) -> (Session, String) {
        let (sink, rx) = channel_sink();
        let lead = core
            .create_lead(
                project_id,
                "claude",
                PtySize::new(30, 90),
                sink,
                lead_env(scratch),
            )
            .unwrap();
        let report = collect_until(&rx, "end-report", Duration::from_secs(5));
        (lead, report)
    }

    fn branches(repo: &Path) -> String {
        git(repo, &["branch", "--format=%(refname)"])
    }

    #[test]
    fn a_lead_is_a_detached_worktree_with_its_env_and_no_branch() {
        let scratch = Scratch::new();
        let repo = scratch.repo("demo");
        let core = core_with_reporting_cli(&scratch);
        let project = core.add_project(&repo).unwrap().project;
        // An unapproved preparation config blocks New but must not touch a Lead.
        fs::create_dir_all(repo.join(".shika")).unwrap();
        fs::write(
            repo.join(preparation::CONFIG_PATH),
            "{\"setup-worktree\":[\"touch ran-setup\"]}",
        )
        .unwrap();
        let before = branches(&repo);

        let (lead, report) = create_lead(&core, &scratch, &project.id);
        assert!(lead.lead && lead.started_by.is_none());
        assert_eq!(lead.title, "Lead");
        assert_eq!(lead.branch, "");
        assert_eq!(
            lead.worktree,
            repo.join(".worktrees")
                .join(format!("shika-lead-{}", lead.id))
        );
        assert_eq!(
            git(&lead.worktree, &["rev-parse", "--abbrev-ref", "HEAD"]).trim(),
            "HEAD"
        );
        assert_eq!(
            git(&lead.worktree, &["rev-parse", "HEAD"]),
            git(&repo, &["rev-parse", "main"])
        );
        assert_eq!(branches(&repo), before);
        assert!(!lead.worktree.join("ran-setup").exists());
        let journal = core.worktree_journal().unwrap();
        assert_eq!(journal.len(), 1);
        assert_eq!(journal[0].path, lead.worktree);
        assert_eq!(journal[0].branch, "");
        assert!(
            fs::read_to_string(repo.join(".git/info/exclude"))
                .unwrap()
                .contains(".worktrees/")
        );
        assert_eq!(core.lead_for_project(&project.id), Some(lead.clone()));
        assert!(core.lead_for_project("other").is_none());

        // Preset flags first, the prompt last; the control env is set and
        // the shika command directory leads PATH.
        assert!(
            report.contains("last:You lead this project. Run shika help."),
            "{report}"
        );
        let control = scratch.path.join("control");
        assert!(
            report.contains(&format!("sock:{}", control.join("sock").display())),
            "{report}"
        );
        assert!(report.contains("token:lead-token"), "{report}");
        assert!(
            report.contains(&format!("path:{}:", control.join("bin").display())),
            "{report}"
        );
        assert!(core.taken_ids().unwrap().contains(&lead.id));

        // One Lead per project, and a refusal creates nothing.
        let (sink, _rx) = channel_sink();
        assert_eq!(
            core.create_lead(
                &project.id,
                "claude",
                PtySize::default(),
                sink,
                lead_env(&scratch)
            ),
            Err(Error::LeadExists)
        );
        assert_eq!(core.worktree_journal().unwrap().len(), 1);
        assert_eq!(core.sessions().len(), 1);
    }

    #[test]
    fn a_lead_needs_a_good_prompt_and_a_found_cli() {
        let scratch = Scratch::new();
        let repo = scratch.repo("demo");
        let core = core_with_reporting_cli(&scratch);
        let project = core.add_project(&repo).unwrap().project;
        for prompt in ["", "-x"] {
            let (sink, _rx) = channel_sink();
            let mut env = lead_env(&scratch);
            env.prompt = prompt.into();
            assert!(matches!(
                core.create_lead(&project.id, "claude", PtySize::default(), sink, env),
                Err(Error::InvalidPrompt(_))
            ));
        }
        let (sink, _rx) = channel_sink();
        assert_eq!(
            core.create_lead(
                &project.id,
                "codex",
                PtySize::default(),
                sink,
                lead_env(&scratch)
            ),
            Err(Error::CliNotFound("Codex".into()))
        );
        assert!(core.worktree_journal().unwrap().is_empty());
        assert!(!repo.join(".worktrees").exists());
    }

    #[test]
    fn workers_get_the_prompt_last_an_owner_and_no_control_env() {
        let scratch = Scratch::new();
        let repo = scratch.repo("demo");
        let core = core_with_reporting_cli(&scratch);
        let project = core.add_project(&repo).unwrap().project;
        let (lead, _) = create_lead(&core, &scratch, &project.id);

        let (sink, rx) = channel_sink();
        let worker = core
            .create_session(
                &project.id,
                "claude",
                PtySize::new(30, 90),
                sink,
                LaunchOptions {
                    prompt: Some("Fix the login bug\nsecond line".into()),
                    started_by: Some(lead.id.clone()),
                    ..LaunchOptions::default()
                },
            )
            .unwrap();
        let report = collect_until(&rx, "end-report", Duration::from_secs(5));
        assert!(!worker.lead);
        assert_eq!(worker.started_by.as_deref(), Some(lead.id.as_str()));
        assert_eq!(worker.title, "New Claude Code");
        assert_eq!(worker.branch, format!("shika-draft-{}", worker.id));
        assert!(
            report.contains("sock:\r\n") && report.contains("token:\r\n"),
            "{report}"
        );
        assert!(
            !report.contains("lead-token") && !report.contains("control/bin"),
            "{report}"
        );
        assert_eq!(core.workers_of(&lead.id), vec![worker.clone()]);
        assert!(core.workers_of(&worker.id).is_empty());

        // Draft and lead ids never collide.
        assert_ne!(worker.id, lead.id);

        // Bad prompts are refused before anything is created.
        let journal = core.worktree_journal().unwrap();
        for prompt in ["", "  ", "--help", "-p"] {
            let (sink, _rx) = channel_sink();
            let result = core.create_session(
                &project.id,
                "claude",
                PtySize::default(),
                sink,
                LaunchOptions {
                    prompt: Some(prompt.into()),
                    started_by: None,
                    ..LaunchOptions::default()
                },
            );
            assert!(matches!(result, Err(Error::InvalidPrompt(_))), "{prompt:?}");
        }
        assert_eq!(core.worktree_journal().unwrap(), journal);
        assert_eq!(core.sessions().len(), 2);
    }

    #[test]
    fn only_a_lead_started_workers_agent_gets_the_control_env() {
        let scratch = Scratch::new();
        let repo = scratch.repo("demo");
        let core = core_with_reporting_cli(&scratch);
        let project = core.add_project(&repo).unwrap().project;
        let (lead, _) = create_lead(&core, &scratch, &project.id);
        let control = scratch.path.join("control");

        let (sink, rx) = channel_sink();
        let worker = core
            .create_session(
                &project.id,
                "claude",
                PtySize::new(30, 90),
                sink,
                LaunchOptions {
                    prompt: Some("Fix the login bug".into()),
                    started_by: Some(lead.id.clone()),
                    control: Some(WorkerEnv {
                        socket: control.join("sock"),
                        token: "worker-token".into(),
                        bin_dir: control.join("bin"),
                    }),
                    ..LaunchOptions::default()
                },
            )
            .unwrap();
        let report = collect_until(&rx, "end-report", Duration::from_secs(5));
        assert!(report.contains("token:worker-token"), "{report}");
        assert!(
            report.contains(&format!("sock:{}", control.join("sock").display())),
            "{report}"
        );
        assert!(
            report.contains(&format!("path:{}:", control.join("bin").display())),
            "{report}"
        );
        assert!(!report.contains("lead-token"), "{report}");

        // Its shell tab is spawned with no control variables on purpose, so
        // the PTY layer scrubs whatever the app itself inherited.
        let env = core.path_env();
        let shell = session::shell_request(
            &path_env::user_shell(),
            &worker.worktree,
            env.path(),
            PtySize::default(),
        );
        assert!(shell.env.is_empty());
        assert!(!shell.path.contains("control/bin"));

        // A card the author creates gets none, even with a Lead around.
        let (sink, rx) = channel_sink();
        core.create_session(
            &project.id,
            "claude",
            PtySize::new(30, 90),
            sink,
            LaunchOptions {
                prompt: Some("Author task".into()),
                ..LaunchOptions::default()
            },
        )
        .unwrap();
        let report = collect_until(&rx, "end-report", Duration::from_secs(5));
        assert!(
            report.contains("sock:\r\n") && report.contains("token:\r\n"),
            "{report}"
        );
        assert!(!report.contains("control/bin"), "{report}");
    }

    #[test]
    fn closing_a_lead_removes_the_tree_and_leaves_branches_alone() {
        let scratch = Scratch::new();
        let repo = scratch.repo("demo");
        let core = core_with_reporting_cli(&scratch);
        let project = core.add_project(&repo).unwrap().project;
        let (lead, _) = create_lead(&core, &scratch, &project.id);
        let before = branches(&repo);

        let state = core.session_git_state(&lead.id, true).unwrap();
        assert!(!state.dirty && !state.unpushed && !state.pushed && state.agent_working);
        assert_eq!(core.session_diff_stat(&lead.id), Ok(DiffStat::default()));
        assert_eq!(core.session_pushed_head(&lead.id), Ok(None));
        assert_eq!(core.session_refresh_branch(&lead.id).unwrap(), lead);

        assert_eq!(
            core.session_close(&lead.id, true),
            Err(Error::CloseNeedsConfirmation)
        );
        fs::write(lead.worktree.join("edit.txt"), "x\n").unwrap();
        assert_eq!(core.session_dirty(&lead.id), Ok(true));
        assert_eq!(
            core.session_close(&lead.id, false),
            Err(Error::WorktreeHasChanges(None))
        );
        assert!(lead.worktree.exists());
        assert_eq!(core.sessions().len(), 1);

        fs::remove_file(lead.worktree.join("edit.txt")).unwrap();
        core.session_close(&lead.id, false).unwrap();
        assert!(!lead.worktree.exists());
        assert!(core.sessions().is_empty());
        assert!(core.worktree_journal().unwrap().is_empty());
        assert_eq!(branches(&repo), before);
        assert!(core.lead_for_project(&project.id).is_none());
        assert!(!git(&repo, &["worktree", "list"]).contains("shika-lead"));

        // A new Lead may start once the old one is gone; discard removes
        // even a dirty tree.
        let (again, _) = create_lead(&core, &scratch, &project.id);
        fs::write(again.worktree.join("edit.txt"), "x\n").unwrap();
        core.session_discard(&again.id).unwrap();
        assert!(!again.worktree.exists());
        assert!(core.worktree_journal().unwrap().is_empty());
        assert_eq!(branches(&repo), before);
    }

    #[test]
    fn a_lead_left_behind_is_a_leftover_without_a_branch_to_delete() {
        let scratch = Scratch::new();
        let repo = scratch.repo("demo");
        let core = core_with_reporting_cli(&scratch);
        let project = core.add_project(&repo).unwrap().project;
        let (lead, _) = create_lead(&core, &scratch, &project.id);
        assert!(core.leftovers_list().unwrap().is_empty());
        assert_eq!(
            core.leftover_remove(&lead.worktree),
            Err(Error::UnknownLeftover)
        );
        let before = branches(&repo);
        drop(core);

        let core = core_with_reporting_cli(&scratch);
        let leftovers = core.leftovers_list().unwrap();
        assert_eq!(leftovers.len(), 1);
        assert_eq!(leftovers[0].path, lead.worktree);
        assert_eq!(leftovers[0].branch, "");
        // Ids stay unique against a journaled Lead from an earlier run.
        assert!(core.taken_ids().unwrap().contains(&lead.id));
        core.leftover_remove(&lead.worktree).unwrap();
        assert!(!lead.worktree.exists());
        assert!(core.leftovers_list().unwrap().is_empty());
        assert_eq!(branches(&repo), before);
        assert!(!git(&repo, &["worktree", "list"]).contains("shika-lead"));
    }

    #[test]
    fn a_lead_refuses_branch_diff_publish_and_shell_operations() {
        let scratch = Scratch::new();
        let repo = scratch.repo("demo");
        let core = core_with_reporting_cli(&scratch);
        let project = core.add_project(&repo).unwrap().project;
        let (lead, _) = create_lead(&core, &scratch, &project.id);
        let refused = Err(Error::LeadUnsupported);
        assert_eq!(core.session_rename_from_prompt(&lead.id, "x"), refused);
        assert_eq!(
            core.session_apply_cli_title(&lead.id),
            Err(Error::LeadUnsupported)
        );
        assert_eq!(
            core.session_push_and_close(&lead.id),
            Err(Error::LeadUnsupported)
        );
        assert!(matches!(
            core.session_publish_preview(&lead.id),
            Err(Error::LeadUnsupported)
        ));
        assert!(matches!(
            core.session_diff(&lead.id),
            Err(Error::LeadUnsupported)
        ));
        assert!(matches!(
            core.session_switched_close_check(&lead.id),
            Err(Error::LeadUnsupported)
        ));
        let (sink, _rx) = channel_sink();
        assert_eq!(
            core.open_shell(&lead.id, PtySize::default(), sink),
            Err(Error::LeadUnsupported)
        );
        assert_eq!(core.session(&lead.id), Some(lead));
    }
}
