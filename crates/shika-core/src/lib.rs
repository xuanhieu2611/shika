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
//! - **Blocking.** [`Core::add_project`], [`Core::project_preparation_draft`],
//!   [`Core::save_project_preparation`], [`Core::create_session`],
//!   [`Core::create_session_with_preparation`], [`Core::start_lead`],
//!   [`Core::forget_lead`], [`Core::discover_lead_session`], [`Core::set_lead_session`],
//!   [`Core::open_shell`], [`Core::session_dirty`], [`Core::session_git_state`],
//!   [`Core::session_diff_stat`], [`Core::session_diff`], [`Core::session_file_diff`],
//!   [`Core::session_publish_preview`], [`Core::session_publish`],
//!   [`Core::session_rename_from_prompt`], [`Core::session_apply_cli_title`],
//!   [`Core::session_switch_agent`], [`Core::token_usage`],
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
//!   [`Core::workers_of`], [`Core::saved_lead`], [`Core::lost_workers`], [`Core::mark_workers_reported`], [`Core::lead_has_changes`], [`Core::lead_loss`], [`Core::saved_lead_session_exists`], [`Core::start_usage_collection`], [`Core::record_token_usage`], [`Core::recover_token_usage`],
//!   [`Core::write`], and [`Core::resize`]. They read a
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
mod background;
mod cli_title;
pub mod control;
mod diff;
mod error;
mod lead;
mod lead_chat;
mod lead_chat_store;
mod lead_transport;
mod memory;
mod path_env;
mod preparation;
#[cfg(test)]
mod preparation_tests;
mod projects;
mod pty;
mod publish;
mod session;
mod settings;
mod usage;
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
pub use lead::{FreshReason, LeadConversation, LeadLaunch, LeadLoss, LeadStart, LostWorker};
pub use lead_chat_store::{ChatContent, ChatOrigin, ChatRecord};
pub use lead_transport::{
    ChatEvent, ChatQuestion, ChatQuestionItem, ChatQuestionOption, ChatStatus,
};
pub use memory::{MAX_NOTE_CHARS, MAX_TOTAL_BYTES, MemoryNote};
pub use path_env::{LoginShellError, PathEnv};
pub use preparation::{PreparationConfig, PreparationControl, PreparationDraft, PreparationEvent};
pub use projects::{Project, ProjectAdded, SavedLead};
pub use pty::{PtyEvent, PtyExit, PtyId, PtySink, PtySize};
pub use publish::{ChecksState, MAX_PR_BODY_CHARS, PrChecks, PublishPreview, PublishedPr};
pub use session::{DiffStat, LaunchOptions, Session, SessionGitState, WorkerEnv, task_title};
pub use settings::{
    AgentSettings, Appearance, Background, BackgroundEffect, BackgroundFraming, Changes, Column,
    DEFAULT_DARK_THEME, DEFAULT_LIGHT_THEME, FontSize, KeyOverrides, Settings, ThemeMode,
    ThemeSettings, Translucency,
};
pub use usage::{
    UsageCalendar, UsageCalendarDay, UsageDay, UsageReport, UsageTotals, UsageWeek, format_tokens,
};
pub use worktree::normalize_prefix as normalize_branch_prefix;
pub use worktree::{JournalEntry, KnownBranches, LeadWorktree};

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
/// is named after it, so it must not change. Local bundles are
/// `com.hieule.shika.dev` but share this directory.
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
    memory: memory::MemoryStore,
    env: OnceLock<PathEnv>,
    sessions: SessionStore,
    ptys: PtyHub,
    chats: Mutex<HashMap<String, std::sync::Arc<lead_transport::ChatTransport>>>,
    operations: Mutex<()>,
    /// Where the agent CLIs keep their session titles. None without a home
    /// directory, which only means cards keep their prompt names.
    cli_home: Option<CliHome>,
    /// Opt-in app-owned worker. Tests never read the user's provider data.
    usage: OnceLock<usage::Collector>,
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
            memory: memory::MemoryStore::open(&data_dir),
            data_dir,
            env: OnceLock::new(),
            sessions: SessionStore::new(),
            ptys: PtyHub::new(),
            chats: Mutex::new(HashMap::new()),
            operations: Mutex::new(()),
            cli_home: CliHome::detect(),
            usage: OnceLock::new(),
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

    /// Save `bytes`, already decoded by the app, as a new background copy and
    /// return its file name for [`Background::file`]. `original` is the
    /// picked file's name. Blocking: writes a file.
    pub fn store_background(&self, bytes: &[u8], original: &str) -> Result<String> {
        background::store(&self.data_dir, bytes, original)
    }

    /// Where the copy named `file` lives, or None for a name Shika never
    /// writes.
    pub fn background_path(&self, file: &str) -> Option<PathBuf> {
        background::path(&self.data_dir, file)
    }

    /// Remove every background copy but `keep`, the one settings name.
    /// Blocking: reads and removes files.
    pub fn prune_backgrounds(&self, keep: Option<&str>) {
        background::prune(&self.data_dir, keep)
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
        self.memory.delete(id);
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

    /// Unsaved draft for Settings or first-New onboarding. Existing config
    /// wins; suggestions never become launch behavior without an explicit save.
    pub fn project_preparation_draft(&self, id: &str) -> Result<PreparationDraft> {
        preparation::draft(
            &self.projects.get(id)?.path,
            &self.git()?,
            self.path_env().path(),
        )
    }

    /// Local onboarding state only, never a substitute for parsed-config consent.
    pub fn preparation_onboarding_pending(&self, id: &str) -> Result<bool> {
        Ok(self.projects.get(id)?.preparation_onboarding_pending())
    }

    /// Explicitly continue without preparation. Refuse a config that appeared
    /// since preview; remember the choice locally without writing the repo.
    pub fn skip_preparation_onboarding(&self, id: &str) -> Result<()> {
        let _guard = self.operations.lock().unwrap_or_else(|e| e.into_inner());
        let project = self.projects.get(id)?;
        if preparation::load(&project.path)?.is_some() {
            return Err(Error::Preparation(
                "Worktree configuration changed. Cancel and start New again to review it.".into(),
            ));
        }
        self.projects.review_preparation(id)
    }

    /// Explicit save plus consent for the exact displayed configuration. No
    /// worktree or command is started here; the normal launch fences still apply.
    pub fn save_and_approve_project_preparation(
        &self,
        id: &str,
        expected: Option<&PreparationConfig>,
        config: &PreparationConfig,
    ) -> Result<()> {
        self.save_preparation_edit(id, expected, Some(config), true)
    }

    /// Explicit repository configuration edit, not execution approval. None
    /// disables preparation without changing running sessions or local consent.
    pub fn save_project_preparation(
        &self,
        id: &str,
        expected: Option<&PreparationConfig>,
        config: Option<&PreparationConfig>,
    ) -> Result<()> {
        self.save_preparation_edit(id, expected, config, false)
    }

    fn save_preparation_edit(
        &self,
        id: &str,
        expected: Option<&PreparationConfig>,
        config: Option<&PreparationConfig>,
        approve: bool,
    ) -> Result<()> {
        let project = self.projects.get(id)?;
        let bytes = preparation::validate_edit(
            &project.path,
            config,
            &self.git()?,
            self.path_env().path(),
        )?;
        let _guard = self.operations.lock().unwrap_or_else(|e| e.into_inner());
        // Project removal or an external path edit may have won during validation.
        if self.projects.get(id)?.path != project.path {
            return Err(Error::Preparation(
                "Project changed. Cancel and reopen setup before saving.".into(),
            ));
        }
        preparation::save(&project.path, expected, bytes)?;
        if let (true, Some(config)) = (approve, config) {
            // Reread the committed config before storing consent, just like the
            // ordinary approval dialog. A later change is fenced at launch.
            if preparation::load(&project.path)?.as_ref() != Some(config) {
                return Err(Error::PreparationNeedsApproval);
            }
            self.projects.approve_preparation(id, config.clone()).map_err(|e| Error::Preparation(format!("Setup was saved, but local approval could not be saved: {e}. Cancel and start New again to review.")))
        } else {
            self.projects.review_preparation(id)
        }
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
                started_by_lead: options.started_by.is_some(),
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
            // Only a Lead-started worker's agent gets the control variables;
            // its shell tabs never do.
            let (pty, activity) = self.open_agent(
                &preset,
                program,
                &draft.path,
                size,
                options.control.as_ref(),
                agents::launch_flags(&preset, &draft.path),
                options.prompt.as_deref(),
                sink,
            )?;
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
                pty: Some(pty),
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

    /// Opens an agent PTY for `preset` in `worktree`: `args` (the preset's
    /// launch flags, see `agents::launch_flags` and `agents::lead_flags`), Pi's
    /// activity extension, then `prompt` as the last positional argument. `control` gives a Lead or a Lead-started worker its
    /// `SHIKA_SOCKET`, `SHIKA_TOKEN`, and the `shika` command on `PATH`. On
    /// failure the bridge is removed and nothing runs. Blocking: spawns a
    /// process.
    #[allow(clippy::too_many_arguments)]
    fn open_agent(
        &self,
        preset: &CliPreset,
        program: PathBuf,
        worktree: &Path,
        size: PtySize,
        control: Option<&WorkerEnv>,
        args: Vec<String>,
        prompt: Option<&str>,
        sink: impl PtySink,
    ) -> Result<(PtyId, Option<activity::ActivityBridge>)> {
        let env = self.path_env();
        let usage = self
            .cli_home
            .as_ref()
            .and_then(|home| self.usage.get()?.register(home, &preset.id, worktree));
        let mut sink = sink;
        let sink = move |pty, event| {
            if let Some(source) = &usage {
                source.notify();
            }
            sink.send(pty, event);
        };
        // Optional, session-owned shim. Installation failure keeps the
        // original native CLI launch, with no config changes.
        let activity = (preset.id == "pi")
            .then(activity::ActivityBridge::install)
            .flatten();
        let (search_path, control_vars) = match control {
            Some(control) => (
                session::control_path(&control.bin_dir, env.path()),
                session::control_env(&control.socket, &control.token),
            ),
            None => (env.path().to_string(), Vec::new()),
        };
        let mut request = SpawnRequest {
            program,
            args,
            cwd: worktree.to_path_buf(),
            path: search_path,
            size,
            env: control_vars,
        };
        if let Some(activity) = &activity {
            activity.wire(&mut request);
        }
        // After the extension flags, so the prompt is the last argument.
        let opened =
            session::launch_args(std::mem::take(&mut request.args), prompt).and_then(|args| {
                request.args = args;
                self.ptys
                    .open(request, activity::activity_sink(activity.clone(), sink))
            });
        match opened {
            Ok(pty) => Ok((pty, activity)),
            Err(err) => {
                if let Some(activity) = &activity {
                    activity.cleanup();
                }
                Err(err)
            }
        }
    }

    /// Starts `preset_id` in this session's existing worktree. A prompt that
    /// has text is the CLI's single positional argument after the preset's
    /// launch flags. Empty or whitespace starts the CLI the way New does,
    /// with no positional argument. Only after that process is running does
    /// this stop the current agent PTY. Shell PTYs stay. The branch, title,
    /// base ref, worktree, and `started_by` stay. A named task gets
    /// `cli_titled`, so this prompt and the new CLI's session title cannot
    /// rename the branch or replace the display name; a task still on its
    /// draft branch is named later as New would name it (the returned
    /// session's `cli_titled` says which). No preparation, commit, or push. A Lead
    /// is refused. `control` is the worker env for a Lead-started worker's
    /// new agent PTY, and None for a card the author created. If the start
    /// fails, the current agent keeps running. Blocking: spawns a process.
    pub fn session_switch_agent(
        &self,
        id: &str,
        preset_id: &str,
        prompt: Option<&str>,
        size: PtySize,
        sink: impl PtySink,
        control: Option<WorkerEnv>,
    ) -> Result<Session> {
        let prompt = prompt.filter(|prompt| !prompt.trim().is_empty());
        if let Some(prompt) = prompt {
            session::check_prompt(prompt)?;
        }
        let env = self.path_env();
        let _guard = self
            .operations
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let session = self.sessions.get(id).ok_or(Error::UnknownSession)?;
        refuse_lead(&session)?;
        let preset = agents::presets_from(env)
            .into_iter()
            .find(|preset| preset.id == preset_id)
            .ok_or(Error::UnknownCli)?;
        let program = preset
            .path
            .clone()
            .ok_or_else(|| Error::CliNotFound(preset.name.clone()))?;
        let (pty, activity) = self.open_agent(
            &preset,
            program,
            &session.worktree,
            size,
            control.as_ref(),
            agents::launch_flags(&preset, &session.worktree),
            prompt,
            sink,
        )?;
        let switched =
            self.sessions
                .retarget_agent(id, preset.id.clone(), preset.name.clone(), pty);
        let (session, old_pty) = match switched {
            Ok(pair) => pair,
            Err(err) => {
                self.ptys.close(pty);
                if let Some(activity) = &activity {
                    activity.cleanup();
                }
                return Err(err);
            }
        };
        self.sessions.replace_activity(id, activity);
        self.ptys.close(old_pty);
        Ok(session)
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

    /// Starts the app's single background usage worker and restart recovery.
    /// No provider reads on this caller. Explicit so tests remain isolated.
    pub fn start_usage_collection(&self) {
        self.usage
            .get_or_init(|| usage::Collector::start(self.data_dir.clone()));
    }

    /// Coalesces a recovery request. No provider I/O or operations lock.
    pub fn record_token_usage(&self, preset_id: &str, worktree: &Path) {
        if let (Some(collector), Some(home)) = (self.usage.get(), &self.cli_home) {
            collector.request(home, preset_id, worktree);
        }
    }

    /// Signal paced source recovery. The caller performs no file I/O.
    pub fn recover_token_usage(&self) {
        if let Some(collector) = self.usage.get() {
            collector.recover();
        }
    }

    /// Metadata-only collection diagnostic. True when no source reads, queued
    /// jobs, or recovery passes remain. Collection gaps are reported separately
    /// by [`Self::token_usage`]; this performs no provider I/O.
    pub fn usage_collection_idle(&self) -> bool {
        self.usage.get().is_none_or(|collector| collector.idle())
    }

    /// Indexed saved totals. Call off the UI thread; never opens provider files.
    pub fn token_usage(&self) -> UsageReport {
        let mut report = usage::report(&self.data_dir, usage::local_today());
        report.pending |= self
            .usage
            .get()
            .is_some_and(|collector| collector.pending());
        report
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
            None,
        )
    }

    /// A Lead's `shika pr`: preview and publish in one hold of the operation
    /// lock, with the Create PR dialog's defaults (recorded base as target,
    /// task title) unless `title` or `body` replace them. There is no dialog,
    /// so a task without a recorded target is [`Error::PublishNoTarget`].
    /// The same rechecks, partial-success rules, and PR reuse apply.
    pub fn session_publish_now(
        &self,
        id: &str,
        title: Option<&str>,
        body: Option<&str>,
    ) -> Result<PublishedPr> {
        let _guard = self.operations.lock().unwrap_or_else(|e| e.into_inner());
        let session = self.sessions.get(id).ok_or(Error::UnknownSession)?;
        refuse_lead(&session)?;
        let session = self.ensure_session_branch(&session)?;
        let gh = self.path_env().resolve("gh").ok_or_else(|| {
            Error::Publish(
                "gh not found on PATH. Install GitHub CLI and run gh auth login in a shell.".into(),
            )
        })?;
        publish::publish_now(
            &self.git()?,
            &gh,
            self.path_env().path(),
            &session,
            title,
            body,
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

    /// Explicitly confirmed task discard. Stops all task PTYs, then deletes
    /// the tree and local branch. Leads are refused: use [`Core::stop_lead`]
    /// and [`Core::forget_lead`]. No commit is created. Blocking.
    pub fn session_discard(&self, id: &str) -> Result<()> {
        let _guard = self
            .operations
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let session = self.sessions.get(id).ok_or(Error::UnknownSession)?;
        refuse_lead(&session)?;
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
    /// task branch is deleted. Leads are refused: use [`Core::stop_lead`] and
    /// [`Core::forget_lead`]. The app asks on CloseNeedsConfirmation. Blocking.
    pub fn session_close(&self, id: &str, agent_working: bool) -> Result<()> {
        let _guard = self
            .operations
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let session = self.sessions.get(id).ok_or(Error::UnknownSession)?;
        refuse_lead(&session)?;
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
    /// A project's fixed Lead worktree is not listed while the project still
    /// has a saved Lead; [`Core::forget_lead`] removes it. Older
    /// `shika-lead-<id>` trees are ordinary leftovers.
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
                    && !self.is_saved_lead_worktree(entry)
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
            // A Lead's detached worktree: nothing to delete but the tree. A
            // symbolic link here is never followed; only the entry goes.
            if std::fs::symlink_metadata(&entry.path).is_ok() && !worktree::is_real_dir(&entry.path)
            {
                return self.journal.remove_path(&entry.path);
            }
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
        let chat = {
            self.chats
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&session.id)
        };
        if let Some(chat) = chat {
            chat.stop();
        }
        if let Some(pty) = session.pty {
            self.ptys.close(pty);
        }
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
            loop {
                let n = COUNTER.fetch_add(1, Ordering::Relaxed);
                let nanos = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos();
                let path = std::env::temp_dir()
                    .join(format!("shika-core-{}-{nanos}-{n}", std::process::id()));
                match fs::create_dir(&path) {
                    // `/var` is a symlink; git reports resolved paths.
                    Ok(()) => {
                        return Self {
                            path: path.canonicalize().unwrap(),
                        };
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
                    Err(err) => panic!("could not reserve core fixture: {err}"),
                }
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
    fn usage_sink_registers_before_spawn_and_recovers_after_close_and_restart() {
        let scratch = Scratch::new();
        let mut core = core_with_fake_cli(&scratch);
        assert!(core.usage_collection_idle()); // collection is opt-in
        let home = CliHome::at(scratch.path.join("usage-home"));
        core.cli_home = Some(home.clone());
        let held = rusqlite::Connection::open(core.data_dir().join("usage.sqlite")).unwrap();
        held.execute_batch("BEGIN EXCLUSIVE").unwrap();
        core.start_usage_collection();
        let config = home.claude_root().to_string_lossy().replace('\'', "'\\''");
        let data = core.data_dir().to_string_lossy().replace('\'', "'\\''");
        fs::write(scratch.path.join("bin/claude"), format!(
            "#!/bin/sh\ntest -d '{data}/usage-sources' || exit 42\nproject=$(printf '%s' \"$PWD\" | LC_ALL=C tr -c '[:alnum:]' '-')\nmkdir -p '{config}/projects/'\"$project\"\nprintf '%s\\n' '{{\"type\":\"assistant\",\"message\":{{\"id\":\"first\",\"usage\":{{\"input_tokens\":11,\"output_tokens\":7}}}}}}' > '{config}/projects/'\"$project\"'/session.jsonl'\nprintf 'usage_written\\n'\nwhile IFS= read -r line; do :; done\n"
        )).unwrap();
        let project = core
            .add_project(&scratch.repo("usage-repo"))
            .unwrap()
            .project;
        let (sink, rx) = channel_sink();
        let session = core
            .create_session(
                &project.id,
                "claude",
                PtySize::default(),
                sink,
                LaunchOptions::default(),
            )
            .unwrap();
        assert!(
            collect_until(&rx, "usage_written", Duration::from_secs(5)).contains("usage_written")
        );
        assert_eq!(
            fs::read_dir(core.data_dir().join("usage-sources"))
                .unwrap()
                .count(),
            1
        );
        let source = home
            .claude_project_dir(&session.worktree)
            .join("session.jsonl");
        // The held database makes collection impossible until after Close.
        // It must not block Close, lose the source, or depend on a live card.
        core.session_discard(&session.id).unwrap();
        assert!(!session.worktree.exists());
        drop(held);
        let deadline = std::time::Instant::now() + Duration::from_secs(6);
        while core.token_usage().claude != 18 && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(core.token_usage().claude, 18);
        let data = core.data_dir().to_path_buf();
        drop(core);
        use std::io::Write;
        fs::OpenOptions::new().append(true).open(source).unwrap().write_all(
            b"{\"type\":\"assistant\",\"message\":{\"id\":\"late\",\"usage\":{\"input_tokens\":2,\"output_tokens\":3}}}\n"
        ).unwrap();
        let mut reopened = Core::open(data).unwrap();
        reopened.cli_home = Some(home);
        reopened.start_usage_collection();
        let deadline = std::time::Instant::now() + Duration::from_secs(6);
        while reopened.token_usage().claude != 23 && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(reopened.token_usage().claude, 23);
        assert!(reopened.sessions().is_empty());
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
                worktree_setup_reviewed: false,
                lead: None,
            }]
        );
        assert_eq!(
            core.worktree_journal().unwrap(),
            [JournalEntry {
                project_id: "18db38e2f78faa00".into(),
                branch: "shika-draft-1".into(),
                path: PathBuf::from("/Users/x/code/sample-project/.worktrees/shika-draft-1"),
                base_ref: None,
                started_by_lead: false,
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
        core.write(session.pty.unwrap(), b"hello\r").unwrap();
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
        assert!(core.close_shell(&session.id, session.pty.unwrap()).is_err());
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
            core.write(session.pty.unwrap(), b"").is_ok(),
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
        assert_eq!(
            core.write(session.pty.unwrap(), b"x"),
            Err(Error::UnknownPty)
        );
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
        core.write(second.pty.unwrap(), b"still alive\r").unwrap();
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
        core.write(session.pty.unwrap(), b"still alive\r").unwrap();
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
                core.write(session.pty.unwrap(), b"").is_ok(),
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

    #[test]
    fn a_broken_lead_gitfile_never_refreshes_or_discards_the_main_checkout() {
        let scratch = Scratch::new();
        let core = core_with_reporting_cli(&scratch);
        let repo = scratch.repo("demo");
        let project = core.add_project(&repo).unwrap().project;
        git(&repo, &["branch", "dev"]);
        git(&repo, &["commit", "--allow-empty", "-m", "main only"]);
        core.set_project_base(&project.id, "dev").unwrap();
        let (lead, _) = start_lead(&core, &scratch, &project.id);
        core.stop_lead(&lead.id).unwrap();
        fs::remove_file(lead.worktree.join(".git")).unwrap();
        let before = git(&repo, &["rev-parse", "HEAD"]);
        let saved = core.saved_lead(&project.id).unwrap();
        let (sink, _) = channel_sink();
        assert_eq!(
            core.start_lead(
                &project.id,
                "claude",
                PtySize::default(),
                sink,
                lead_launch(&scratch)
            ),
            Err(Error::LeadWorktreeInvalid(lead.worktree.clone()))
        );
        assert_eq!(core.saved_lead(&project.id).unwrap(), saved);
        assert_eq!(core.lead_loss(&project.id), Ok(LeadLoss::default()));
        // Forget clears only the saved record and journal, not the broken
        // folder or anything in the main checkout around it.
        fs::write(lead.worktree.join("keep.txt"), "keep\n").unwrap();
        core.forget_lead(&project.id, true).unwrap();
        assert!(lead.worktree.join("keep.txt").exists());
        assert_eq!(git(&repo, &["rev-parse", "HEAD"]), before);
        assert_eq!(
            git(&repo, &["symbolic-ref", "--short", "HEAD"]).trim(),
            "main"
        );
    }

    #[test]
    fn stale_codex_discovery_cannot_write_after_stop_forget_or_replacement() {
        let scratch = Scratch::new();
        let repo = scratch.repo("demo");
        let core = core_with_reporting_cli(&scratch);
        let project = core.add_project(&repo).unwrap().project;
        let (first, _) = start_lead_with(&core, &scratch, &project.id, "codex");
        let late_result = || core.save_discovered_lead_session(&first.session, "old-thread".into());
        core.stop_lead(&first.session.id).unwrap();
        assert_eq!(late_result(), Ok(None));
        core.forget_lead(&project.id, false).unwrap();
        assert_eq!(late_result(), Ok(None));
        assert_eq!(core.saved_lead(&project.id).unwrap(), None);

        let (second, _) = start_lead_with(&core, &scratch, &project.id, "codex");
        assert_eq!(late_result(), Ok(None));
        assert_eq!(
            core.discover_lead_session(&first.session.id, first.started_at),
            Ok(None)
        );
        assert_eq!(
            core.saved_lead(&project.id).unwrap().unwrap().session_id,
            None
        );
        assert_eq!(
            core.save_discovered_lead_session(&second.session, "new-thread".into()),
            Ok(Some("new-thread".into()))
        );
        assert_eq!(
            core.save_discovered_lead_session(&second.session, "duplicate-result".into()),
            Ok(Some("new-thread".into()))
        );
        assert_eq!(late_result(), Ok(None));
        assert_eq!(
            core.saved_lead(&project.id)
                .unwrap()
                .unwrap()
                .session_id
                .as_deref(),
            Some("new-thread")
        );
        core.stop_lead(&second.session.id).unwrap();

        let (third, _) = start_lead_with(&core, &scratch, &project.id, "claude");
        let saved = core.saved_lead(&project.id).unwrap();
        assert_eq!(late_result(), Ok(None));
        assert_eq!(
            core.save_discovered_lead_session(&second.session, "old-codex".into()),
            Ok(None)
        );
        assert_eq!(core.saved_lead(&project.id).unwrap(), saved);
        core.stop_lead(&third.session.id).unwrap();
    }

    const LEAD_INSTRUCTIONS: &str = "You lead this project. Run \"shika help\" first.";
    const LEAD_BOOTSTRAP: &str = "Bootstrap: you are the Lead; read the guide.";

    /// Every CLI prints its arguments and environment (`SWITCH_CLI`), and the
    /// Core reads CLI files from a scratch home, never the real one.
    #[test]
    fn chat_interrupt_cancels_a_blocked_pipe_write_and_quit_reaps_the_child() {
        let scratch = Scratch::new();
        let mut core = core_with_switch_clis(&scratch);
        core.cli_home = Some(CliHome::at(scratch.path.join("home")));
        write_exec(&scratch.path, "claude", "#!/bin/sh\nsleep 30\n");
        let project = core
            .add_project(&scratch.repo("pipe-demo"))
            .unwrap()
            .project;
        let start = core
            .start_claude_lead(&project.id, lead_launch(&scratch))
            .unwrap();
        core.lead_chat_send(
            &start.session.id,
            "large input ".repeat(100_000),
            ChatOrigin::Author,
            Vec::new(),
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(7);
        loop {
            if core
                .lead_chat_events(&start.session.id)
                .iter()
                .any(|e| matches!(e, ChatEvent::Record(_)))
            {
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
        core.lead_chat_interrupt(&start.session.id);
        loop {
            if core
                .lead_chat_events(&start.session.id)
                .iter()
                .any(|e| matches!(e, ChatEvent::Status(ChatStatus::Ready)))
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "blocked stdin must be interruptible"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        let now = Instant::now();
        core.stop_lead_chats();
        assert!(now.elapsed() < Duration::from_secs(4));
    }

    #[test]
    fn claude_chat_has_no_pty_confirms_identity_and_keeps_history_until_forget() {
        let scratch = Scratch::new();
        let mut core = core_with_switch_clis(&scratch);
        core.cli_home = Some(CliHome::at(scratch.path.join("home")));
        write_exec(
            &scratch.path,
            "claude",
            r#"#!/bin/sh
id=
while [ "$#" -gt 0 ]; do
 case "$1" in --session-id|--resume) id="$2"; shift;; esac
 shift
done
while IFS= read -r line; do
 case "$line" in
 *shika-initialize*) printf '{"type":"control_response","response":{"request_id":"shika-initialize","subtype":"success"}}\n';;
 *'"type":"user"'*)
 printf '{"type":"system","subtype":"init","session_id":"%s"}\n' "$id"
 case "$line" in *please-interrupt*) sleep 30;; esac
 printf '{"type":"assistant","session_id":"%s","message":{"id":"m1","content":[{"type":"text","text":"private commentary"}]}}\n' "$id"
 printf '{"type":"assistant","session_id":"%s","message":{"id":"m1","content":[{"type":"tool_use","id":"t1","name":"Bash","input":{}}]}}\n' "$id"
 printf '{"type":"assistant","session_id":"%s","message":{"id":"m2","content":[{"type":"text","text":"Final reply"}]}}\n' "$id"
 printf '{"type":"result","session_id":"%s","subtype":"success","is_error":false,"result":"private aggregate"}\n' "$id"
 ;;
 esac
done
"#,
        );
        let project = core
            .add_project(&scratch.repo("chat-demo"))
            .unwrap()
            .project;
        core.remember(&project.id, "Keep this note").unwrap();
        let start = core
            .start_claude_lead(&project.id, lead_launch(&scratch))
            .unwrap();
        assert_eq!(start.session.pty, None);
        assert_eq!(
            core.saved_lead(&project.id).unwrap().unwrap().session_id,
            None
        );
        let history = core
            .saved_lead(&project.id)
            .unwrap()
            .unwrap()
            .chat_history
            .unwrap();
        let collect = |predicate: &dyn Fn(&ChatEvent) -> bool| {
            let deadline = Instant::now() + Duration::from_secs(7);
            let mut events = Vec::new();
            loop {
                let batch = core.lead_chat_events(&start.session.id);
                let done = batch.iter().any(predicate);
                events.extend(batch);
                if done {
                    return events;
                }
                assert!(Instant::now() < deadline, "chat event deadline");
                std::thread::sleep(Duration::from_millis(10));
            }
        };
        core.lead_chat_send(
            &start.session.id,
            "Hello".into(),
            ChatOrigin::Author,
            Vec::new(),
        )
        .unwrap();
        let events = collect(&|e| matches!(e, ChatEvent::Status(ChatStatus::Ready)));
        let confirmed = events
            .iter()
            .find_map(|e| {
                if let ChatEvent::Confirmed(id) = e {
                    Some(id)
                } else {
                    None
                }
            })
            .unwrap();
        core.confirm_lead_chat(&start.session.id, confirmed)
            .unwrap();
        let page = core.lead_chat_page(&project.id, None).unwrap();
        assert_eq!(page.len(), 2);
        assert_eq!(page[1].content.text(), Some("Final reply"));
        assert!(!format!("{page:?}").contains("private"));
        let (worker_sink, _worker_output) = channel_sink();
        let worker = core
            .create_session(
                &project.id,
                "claude",
                PtySize::default(),
                worker_sink,
                LaunchOptions {
                    started_by: Some(start.session.id.clone()),
                    ..Default::default()
                },
            )
            .unwrap();
        core.lead_chat_send(
            &start.session.id,
            "please-interrupt".into(),
            ChatOrigin::Author,
            Vec::new(),
        )
        .unwrap();
        collect(&|e| matches!(e, ChatEvent::Status(ChatStatus::Working)));
        core.lead_chat_interrupt(&start.session.id);
        collect(&|e| matches!(e, ChatEvent::Status(ChatStatus::Ready)));
        assert!(
            core.lead_chat_page(&project.id, None)
                .unwrap()
                .iter()
                .any(|r| r.content.text() == Some("Reply stopped"))
        );
        core.lead_chat_send(
            &start.session.id,
            "Continue".into(),
            ChatOrigin::Author,
            Vec::new(),
        )
        .unwrap();
        collect(&|e| matches!(e, ChatEvent::Status(ChatStatus::Ready)));
        core.stop_lead(&start.session.id).unwrap();
        assert!(
            core.write(worker.pty.unwrap(), b"still running\n").is_ok(),
            "chat interruption and Stop must not stop worker groups"
        );
        assert_eq!(
            core.saved_lead(&project.id)
                .unwrap()
                .unwrap()
                .chat_history
                .as_deref(),
            Some(history.as_str())
        );
        assert!(core.lead_chat_page(&project.id, None).unwrap().len() >= 5);
        assert_eq!(
            core.confirm_lead_chat(&start.session.id, confirmed),
            Err(Error::UnknownSession)
        );
        core.forget_lead(&project.id, false).unwrap();
        assert!(
            !core
                .data_dir()
                .join("leads")
                .join(&project.id)
                .join("chat")
                .exists()
        );
        assert_eq!(core.lead_memory(&project.id).unwrap().len(), 1);
    }

    /// The shape of `claude-2.1.302-background-local.jsonl`: after the
    /// author's turn settles, a background command finishes and the CLI opens
    /// a turn of its own with no input from Shika.
    #[test]
    fn a_turn_the_claude_cli_starts_itself_is_shown_saved_and_numbered() {
        let scratch = Scratch::new();
        let mut core = core_with_switch_clis(&scratch);
        core.cli_home = Some(CliHome::at(scratch.path.join("home")));
        write_exec(
            &scratch.path,
            "claude",
            r#"#!/bin/sh
id=
while [ "$#" -gt 0 ]; do
 case "$1" in --session-id|--resume) id="$2"; shift;; esac
 shift
done
turn() {
 printf '{"type":"system","subtype":"init","session_id":"%s"}\n' "$id"
 printf '{"type":"assistant","session_id":"%s","message":{"id":"%s-a","content":[{"type":"text","text":"private narration"}]}}\n' "$id" "$1"
 printf '{"type":"assistant","session_id":"%s","message":{"id":"%s-a","content":[{"type":"tool_use","id":"%s-t","name":"Bash","input":{}}]}}\n' "$id" "$1" "$1"
 printf '{"type":"user","session_id":"%s","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"%s-t","content":"private tool output"}]}}\n' "$id" "$1"
 printf '{"type":"assistant","session_id":"%s","message":{"id":"%s-b","content":[{"type":"text","text":"%s"}]}}\n' "$id" "$1" "$2"
 printf '{"type":"result","session_id":"%s","subtype":"success","is_error":false,"result":"private aggregate"}\n' "$id"
}
while IFS= read -r line; do
 case "$line" in
 *shika-initialize*) printf '{"type":"control_response","response":{"request_id":"shika-initialize","subtype":"success"}}\n';;
 *start-background*)
 turn m1 "Waiting on the worker."
 sleep 0.3
 printf '{"type":"system","subtype":"task_notification","session_id":"%s","status":"completed"}\n' "$id"
 turn m2 "The worker finished."
 ;;
 *'"type":"user"'*) turn m3 "Next reply";;
 esac
done
"#,
        );
        let project = core
            .add_project(&scratch.repo("background-demo"))
            .unwrap()
            .project;
        let start = core
            .start_claude_lead(&project.id, lead_launch(&scratch))
            .unwrap();
        let collect = |readies: usize| {
            let deadline = Instant::now() + Duration::from_secs(7);
            let mut events = Vec::new();
            loop {
                events.extend(core.lead_chat_events(&start.session.id));
                if events
                    .iter()
                    .filter(|e| matches!(e, ChatEvent::Status(ChatStatus::Ready)))
                    .count()
                    >= readies
                {
                    return events;
                }
                assert!(Instant::now() < deadline, "chat event deadline");
                std::thread::sleep(Duration::from_millis(10));
            }
        };
        core.lead_chat_send(
            &start.session.id,
            "start-background".into(),
            ChatOrigin::Author,
            Vec::new(),
        )
        .unwrap();
        let events = collect(2);
        let first_ready = events
            .iter()
            .position(|e| matches!(e, ChatEvent::Status(ChatStatus::Ready)))
            .unwrap();
        let after = &events[first_ready + 1..];
        let started = after
            .iter()
            .position(|e| matches!(e, ChatEvent::Started))
            .expect("the CLI's own turn must start a turn");
        assert!(matches!(
            after[started + 1],
            ChatEvent::Status(ChatStatus::Working)
        ));
        let texts = |events: &[ChatEvent]| {
            events
                .iter()
                .filter_map(|e| match e {
                    ChatEvent::Record(r) => r.content.text().map(str::to_string),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(texts(after), ["The worker finished."]);
        assert!(
            !after
                .iter()
                .any(|e| matches!(e, ChatEvent::Rejected(_) | ChatEvent::DeliveryFailed)),
            "a turn the CLI started has no delivery to fail"
        );

        let page = core.lead_chat_page(&project.id, None).unwrap();
        let shown = page
            .iter()
            .map(|r| (r.turn, r.content.clone()))
            .collect::<Vec<_>>();
        assert_eq!(
            shown,
            [
                (
                    1,
                    ChatContent::Author {
                        text: "start-background".into()
                    }
                ),
                (
                    1,
                    ChatContent::Lead {
                        text: "Waiting on the worker.".into()
                    }
                ),
                (
                    2,
                    ChatContent::Lead {
                        text: "The worker finished.".into()
                    }
                ),
            ]
        );
        assert!(!format!("{page:?}").contains("private"));
        let db = rusqlite::Connection::open_with_flags(
            core.data_dir()
                .join("leads")
                .join(&project.id)
                .join("chat/history.sqlite"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let turn_two = db
            .prepare("SELECT content FROM records WHERE turn=2 ORDER BY id")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .map(|r| serde_json::from_str::<ChatContent>(&r.unwrap()).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            turn_two,
            [
                ChatContent::Accepted,
                ChatContent::Lead {
                    text: "The worker finished.".into()
                },
                ChatContent::Settled
            ],
            "no author bubble and no Outgoing row for the CLI's own turn"
        );
        drop(db);

        core.lead_chat_send(
            &start.session.id,
            "Next".into(),
            ChatOrigin::Author,
            Vec::new(),
        )
        .unwrap();
        collect(1);
        let page = core.lead_chat_page(&project.id, None).unwrap();
        assert_eq!(
            page[page.len() - 2..]
                .iter()
                .map(|r| (r.turn, r.content.text().unwrap()))
                .collect::<Vec<_>>(),
            [(3, "Next"), (3, "Next reply")]
        );
        core.stop_lead(&start.session.id).unwrap();
    }

    fn core_with_reporting_cli(scratch: &Scratch) -> Core {
        let mut core = core_with_switch_clis(scratch);
        core.cli_home = Some(CliHome::at(scratch.path.join("home")));
        core
    }

    fn lead_launch(scratch: &Scratch) -> LeadLaunch {
        LeadLaunch {
            socket: scratch.path.join("control/sock"),
            token: "lead-token".into(),
            bin_dir: scratch.path.join("control/bin"),
            instructions: LEAD_INSTRUCTIONS.into(),
            cursor_bootstrap: Some(LEAD_BOOTSTRAP.into()),
        }
    }

    fn start_lead_with(
        core: &Core,
        scratch: &Scratch,
        project_id: &str,
        preset: &str,
    ) -> (LeadStart, String) {
        let (sink, rx) = channel_sink();
        let start = core
            .start_lead(
                project_id,
                preset,
                PtySize::new(30, 90),
                sink,
                lead_launch(scratch),
            )
            .unwrap();
        let report = collect_until(&rx, "end-report", Duration::from_secs(5));
        (start, report)
    }

    fn start_lead(core: &Core, scratch: &Scratch, project_id: &str) -> (Session, String) {
        let (start, report) = start_lead_with(core, scratch, project_id, "claude");
        (start.session, report)
    }

    fn branches(repo: &Path) -> String {
        git(repo, &["branch", "--format=%(refname)"])
    }

    #[test]
    fn a_lead_is_a_fixed_detached_worktree_with_its_env_and_no_branch() {
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

        let (start, report) = start_lead_with(&core, &scratch, &project.id, "claude");
        let lead = start.session.clone();
        assert!(lead.lead && lead.started_by.is_none());
        assert_eq!(lead.title, "Lead");
        assert_eq!(lead.branch, "");
        assert_eq!(lead.worktree, repo.join(".worktrees").join("shika-lead"));
        assert_eq!(start.worktree, LeadWorktree::Created);
        assert_eq!(
            start.conversation,
            LeadConversation::Fresh(FreshReason::NoSavedSession)
        );
        assert!(start.lost_workers.is_empty());
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

        // The preset's flags, a fresh Claude session id, the instructions as
        // the system prompt, and no positional prompt after them.
        let args = report_args(&report);
        assert_eq!(args[0], "--dangerously-skip-permissions");
        assert_eq!(args[1], "--session-id");
        let session_id = args[2].clone();
        assert_eq!(args[3], "--append-system-prompt");
        assert_eq!(args[4], LEAD_INSTRUCTIONS);
        assert_eq!(args.len(), 5, "{args:?}");
        assert_eq!(
            core.saved_lead(&project.id).unwrap(),
            Some(SavedLead {
                cli: "claude".into(),
                session_id: Some(session_id),
                reported_workers: Vec::new(),
                chat_history: None,
            })
        );
        // The control env is set and the shika command directory leads PATH.
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
            core.start_lead(
                &project.id,
                "claude",
                PtySize::default(),
                sink,
                lead_launch(&scratch)
            )
            .map(|_| ()),
            Err(Error::LeadExists)
        );
        assert_eq!(core.worktree_journal().unwrap().len(), 1);
        assert_eq!(core.sessions().len(), 1);
    }

    #[test]
    fn a_lead_needs_good_instructions_and_a_found_cli() {
        let scratch = Scratch::new();
        let repo = scratch.repo("demo");
        let core = core_with_reporting_cli(&scratch);
        let project = core.add_project(&repo).unwrap().project;
        for text in ["", "-x"] {
            let (sink, _rx) = channel_sink();
            let mut launch = lead_launch(&scratch);
            launch.instructions = text.into();
            assert!(matches!(
                core.start_lead(&project.id, "claude", PtySize::default(), sink, launch),
                Err(Error::InvalidPrompt(_))
            ));
        }
        let (sink, _rx) = channel_sink();
        let mut launch = lead_launch(&scratch);
        launch.cursor_bootstrap = Some("--help".into());
        assert!(matches!(
            core.start_lead(&project.id, "cursor", PtySize::default(), sink, launch),
            Err(Error::InvalidPrompt(_))
        ));
        let (sink, _rx) = channel_sink();
        assert!(matches!(
            core.start_lead(
                &project.id,
                "nope",
                PtySize::default(),
                sink,
                lead_launch(&scratch)
            ),
            Err(Error::UnknownCli)
        ));
        let core = core_with_fake_cli(&scratch);
        let (sink, _rx) = channel_sink();
        assert_eq!(
            core.start_lead(
                &project.id,
                "codex",
                PtySize::default(),
                sink,
                lead_launch(&scratch)
            )
            .map(|_| ()),
            Err(Error::CliNotFound("Codex".into()))
        );
        assert!(core.worktree_journal().unwrap().is_empty());
        assert!(!repo.join(".worktrees").exists());
        assert_eq!(core.saved_lead(&project.id).unwrap(), None);
    }

    #[test]
    fn memory_notes_ride_every_lead_start_and_outlive_stop_forget_and_a_cli_change() {
        let scratch = Scratch::new();
        let repo = scratch.repo("demo");
        let core = core_with_reporting_cli(&scratch);
        let project = core.add_project(&repo).unwrap().project;
        let prompt_of = |report: &str| report_args(report)[4].clone();

        // No notes: the instructions are exactly what the app passed.
        let (lead, report) = start_lead(&core, &scratch, &project.id);
        assert_eq!(prompt_of(&report), LEAD_INSTRUCTIONS);
        assert_eq!(core.remember(&project.id, "Use pnpm.\nNot npm."), Ok(1));
        assert_eq!(core.remember(&project.id, "Workers must not push."), Ok(2));
        core.stop_lead(&lead.id).unwrap();

        // Resumed or fresh, the notes follow the guide as the last section.
        // The fake CLI prints one `arg:` per line, so check the text itself.
        let both = |report: &str| {
            let text = report.replace("\r\n", "\n");
            text.contains(&format!("{LEAD_INSTRUCTIONS}\n"))
                && text.contains("\n\n## Memory\n")
                && text.contains("`shika remember`")
                && text.contains("- Use pnpm. Not npm.\n")
                && text.contains("- Workers must not push.\n")
        };
        let (lead, report) = start_lead(&core, &scratch, &project.id);
        assert!(both(&report), "{report}");
        core.stop_lead(&lead.id).unwrap();
        core.forget_lead(&project.id, false).unwrap();
        assert_eq!(core.lead_memory(&project.id).unwrap().len(), 2);
        let (lead, report) = start_lead(&core, &scratch, &project.id);
        assert!(both(&report), "{report}");
        core.stop_lead(&lead.id).unwrap();

        // Another CLI sees them too, and a forgotten note is gone from the
        // next start.
        core.forget_note(&project.id, 1).unwrap();
        let (start, report) = start_lead_with(&core, &scratch, &project.id, "codex");
        assert!(report.contains("Workers must not push."), "{report}");
        assert!(!report.contains("Use pnpm"), "{report}");
        core.stop_lead(&start.session.id).unwrap();

        // A corrupt file never blocks a start; the Lead just has no notes.
        let file = core.memory_path(&project.id).unwrap();
        fs::write(&file, "{ not json").unwrap();
        let (_, report) = start_lead(&core, &scratch, &project.id);
        assert_eq!(prompt_of(&report), LEAD_INSTRUCTIONS);
        assert!(matches!(
            core.remember(&project.id, "x"),
            Err(Error::MemoryUnreadable(_))
        ));
        assert_eq!(fs::read_to_string(&file).unwrap(), "{ not json");
    }

    #[test]
    fn memory_belongs_to_a_known_project_and_goes_with_it() {
        let scratch = Scratch::new();
        let repo = scratch.repo("demo");
        let core = core_with_reporting_cli(&scratch);
        assert_eq!(core.remember("nope", "x"), Err(Error::UnknownProject));
        let project = core.add_project(&repo).unwrap().project;
        assert!(core.lead_memory(&project.id).unwrap().is_empty());
        core.remember(&project.id, "Keep me.").unwrap();
        let file = core.memory_path(&project.id).unwrap();
        assert_eq!(
            file,
            core.data_dir()
                .join("leads")
                .join(&project.id)
                .join("memory.json")
        );
        assert!(file.exists());
        core.remove_project(&project.id).unwrap();
        assert!(!file.exists());
        assert!(!file.parent().unwrap().exists());
    }

    #[test]
    fn workers_get_the_prompt_last_an_owner_and_no_control_env() {
        let scratch = Scratch::new();
        let repo = scratch.repo("demo");
        let core = core_with_reporting_cli(&scratch);
        let project = core.add_project(&repo).unwrap().project;
        let (lead, _) = start_lead(&core, &scratch, &project.id);

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
        let (lead, _) = start_lead(&core, &scratch, &project.id);
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
    fn leads_can_only_be_stopped_and_forgotten_through_the_lead_lifecycle() {
        let scratch = Scratch::new();
        let repo = scratch.repo("demo");
        let core = core_with_reporting_cli(&scratch);
        let project = core.add_project(&repo).unwrap().project;
        let (lead, _) = start_lead(&core, &scratch, &project.id);
        let before = branches(&repo);
        let saved = core.saved_lead(&project.id).unwrap();
        assert_eq!(core.session_diff_stat(&lead.id), Ok(DiffStat::default()));
        assert_eq!(core.session_pushed_head(&lead.id), Ok(None));
        assert_eq!(core.session_refresh_branch(&lead.id).unwrap(), lead);

        // Neither a clean tree, uncommitted work, nor detached commits may
        // take the old task-close shortcut past Forget's safety checks.
        for state in ["clean", "dirty", "committed"] {
            if state == "dirty" {
                fs::write(lead.worktree.join("edit.txt"), "x\n").unwrap();
            } else if state == "committed" {
                git(&lead.worktree, &["add", "edit.txt"]);
                git(&lead.worktree, &["commit", "-m", "keep this"]);
            }
            for working in [false, true] {
                assert_eq!(
                    core.session_close(&lead.id, working),
                    Err(Error::LeadUnsupported)
                );
                assert_eq!(
                    core.close_session(&lead.id, working),
                    Err(Error::LeadUnsupported)
                );
            }
            assert_eq!(core.session_discard(&lead.id), Err(Error::LeadUnsupported));
            assert_eq!(core.session(&lead.id), Some(lead.clone()));
            assert_eq!(core.saved_lead(&project.id).unwrap(), saved);
            assert!(lead.worktree.exists());
        }
        core.stop_lead(&lead.id).unwrap();
        assert_eq!(
            core.forget_lead(&project.id, false),
            Err(Error::WorktreeHasChanges(None))
        );
        core.forget_lead(&project.id, true).unwrap();
        assert!(!lead.worktree.exists());
        assert!(core.worktree_journal().unwrap().is_empty());
        assert_eq!(core.saved_lead(&project.id).unwrap(), None);
        assert_eq!(branches(&repo), before);
    }

    #[test]
    fn the_fixed_lead_worktree_is_not_a_leftover_while_its_record_exists() {
        let scratch = Scratch::new();
        let repo = scratch.repo("demo");
        let core = core_with_reporting_cli(&scratch);
        let project = core.add_project(&repo).unwrap().project;
        let (lead, _) = start_lead(&core, &scratch, &project.id);
        assert!(core.leftovers_list().unwrap().is_empty());
        assert_eq!(
            core.leftover_remove(&lead.worktree),
            Err(Error::UnknownLeftover)
        );
        let before = branches(&repo);
        drop(core);

        // After a relaunch the Lead is not live, but its record keeps the
        // tree out of the list: Forget removes it, not Leftovers.
        let core = core_with_reporting_cli(&scratch);
        assert!(core.lead_for_project(&project.id).is_none());
        assert!(core.leftovers_list().unwrap().is_empty());
        assert_eq!(core.worktree_journal().unwrap().len(), 1);

        // Without a record the tree may be listed and removed by hand. Ids
        // stay unique against a journaled Lead folder from an earlier run.
        core.projects.set_lead(&project.id, None).unwrap();
        let leftovers = core.leftovers_list().unwrap();
        assert_eq!(leftovers.len(), 1);
        assert_eq!(leftovers[0].path, lead.worktree);
        assert_eq!(leftovers[0].branch, "");
        core.leftover_remove(&lead.worktree).unwrap();
        assert!(!lead.worktree.exists());
        assert!(core.leftovers_list().unwrap().is_empty());
        assert_eq!(branches(&repo), before);
        assert!(!git(&repo, &["worktree", "list"]).contains("shika-lead"));
    }

    #[test]
    fn removing_a_project_keeps_the_lead_tree_and_lists_it_once_the_record_is_gone() {
        let scratch = Scratch::new();
        let repo = scratch.repo("demo");
        let core = core_with_reporting_cli(&scratch);
        let project = core.add_project(&repo).unwrap().project;
        let (lead, _) = start_lead(&core, &scratch, &project.id);
        assert_eq!(core.remove_project(&project.id).unwrap().len(), 1);
        assert!(core.projects().unwrap().is_empty());
        assert!(lead.worktree.exists());
        // The project, and so its record, is gone: the tree is a leftover.
        let leftovers = core.leftovers_list().unwrap();
        assert_eq!(leftovers.len(), 1);
        assert_eq!(leftovers[0].path, lead.worktree);
    }

    #[test]
    fn an_old_per_id_lead_tree_stays_an_ordinary_leftover() {
        let scratch = Scratch::new();
        let repo = scratch.repo("demo");
        let core = core_with_reporting_cli(&scratch);
        let project = core.add_project(&repo).unwrap().project;
        let (lead, _) = start_lead(&core, &scratch, &project.id);
        let old = repo.join(".worktrees").join("shika-lead-18dabc");
        git(
            &repo,
            &["worktree", "add", "--detach", old.to_str().unwrap(), "main"],
        );
        core.journal
            .add(&JournalEntry {
                project_id: project.id.clone(),
                branch: String::new(),
                path: old.clone(),
                base_ref: None,
                started_by_lead: false,
            })
            .unwrap();
        let leftovers = core.leftovers_list().unwrap();
        assert_eq!(leftovers.len(), 1);
        assert_eq!(leftovers[0].path, old);
        assert!(core.taken_ids().unwrap().contains(&"18dabc".to_string()));
        core.leftover_remove(&old).unwrap();
        assert!(!old.exists());
        assert!(lead.worktree.exists());
    }

    /// A saved Claude conversation file, as Claude Code would have left it.
    fn claude_transcript(scratch: &Scratch, id: &str) {
        let dir = scratch.path.join("home/.claude/projects/-some-project");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(format!("{id}.jsonl")), "{}\n").unwrap();
    }

    /// The id after `--session-id` or `--resume` in a launch report.
    fn conversation_arg(report: &str, flag: &str) -> String {
        let args = report_args(report);
        let at = args.iter().position(|arg| arg == flag).unwrap();
        args[at + 1].clone()
    }

    #[test]
    fn restarting_a_lead_refreshes_its_clean_worktree_and_resumes_claude() {
        let scratch = Scratch::new();
        let repo = scratch.repo("demo");
        let core = core_with_reporting_cli(&scratch);
        let project = core.add_project(&repo).unwrap().project;
        let (first, report) = start_lead_with(&core, &scratch, &project.id, "claude");
        let id = conversation_arg(&report, "--session-id");
        let first_head = git(&repo, &["rev-parse", "main"]).trim().to_string();

        // Stopping keeps the tree, its journal entry, and the record.
        core.stop_lead(&first.session.id).unwrap();
        assert!(core.sessions().is_empty());
        assert!(core.lead_for_project(&project.id).is_none());
        assert!(first.session.worktree.exists());
        assert_eq!(core.worktree_journal().unwrap().len(), 1);
        assert!(core.leftovers_list().unwrap().is_empty());
        assert_eq!(
            core.saved_lead(&project.id).unwrap().unwrap().session_id,
            Some(id.clone())
        );

        git(&repo, &["commit", "--allow-empty", "-m", "second"]);
        let second_head = git(&repo, &["rev-parse", "main"]).trim().to_string();
        claude_transcript(&scratch, &id);
        let (again, report) = start_lead_with(&core, &scratch, &project.id, "claude");

        assert_eq!(again.conversation, LeadConversation::Resumed);
        assert_eq!(
            again.worktree,
            LeadWorktree::Refreshed {
                from: first_head[..10].to_string(),
                to: second_head[..10].to_string(),
            }
        );
        assert_eq!(again.session.worktree, first.session.worktree);
        assert_ne!(again.session.id, first.session.id);
        assert_eq!(
            git(&again.session.worktree, &["rev-parse", "HEAD"]).trim(),
            second_head
        );
        // The instructions go on every start, the id after `--resume`, and
        // there is still no positional prompt.
        assert_eq!(
            report_args(&report),
            [
                "--dangerously-skip-permissions",
                "--resume",
                &id,
                "--append-system-prompt",
                LEAD_INSTRUCTIONS
            ]
        );
        assert_eq!(
            core.saved_lead(&project.id).unwrap(),
            Some(SavedLead {
                cli: "claude".into(),
                session_id: Some(id),
                reported_workers: Vec::new(),
                chat_history: None,
            })
        );
        assert_eq!(core.worktree_journal().unwrap().len(), 1);
        assert!(again.started_at <= SystemTime::now());
    }

    #[test]
    fn a_stored_conversation_that_is_gone_or_from_another_cli_starts_fresh_and_says_why() {
        let scratch = Scratch::new();
        let repo = scratch.repo("demo");
        let core = core_with_reporting_cli(&scratch);
        let project = core.add_project(&repo).unwrap().project;
        let (first, report) = start_lead_with(&core, &scratch, &project.id, "claude");
        let old_id = conversation_arg(&report, "--session-id");
        core.stop_lead(&first.session.id).unwrap();

        // No transcript exists, so `--resume` would fail: new id instead.
        let (second, report) = start_lead_with(&core, &scratch, &project.id, "claude");
        assert_eq!(
            second.conversation,
            LeadConversation::Fresh(FreshReason::SessionMissing)
        );
        assert_eq!(second.worktree, LeadWorktree::Unchanged);
        let new_id = conversation_arg(&report, "--session-id");
        assert_ne!(new_id, old_id);
        assert_eq!(
            core.saved_lead(&project.id).unwrap().unwrap().session_id,
            Some(new_id.clone())
        );
        core.stop_lead(&second.session.id).unwrap();

        // The saved conversation belongs to Claude Code, not Pi.
        claude_transcript(&scratch, &new_id);
        let (third, _) = start_lead_with(&core, &scratch, &project.id, "pi");
        assert_eq!(
            third.conversation,
            LeadConversation::Fresh(FreshReason::CliChanged)
        );
        assert_eq!(core.saved_lead(&project.id).unwrap().unwrap().cli, "pi");
        core.stop_lead(&third.session.id).unwrap();

        // An id edited into something that could read as a flag is ignored.
        core.projects
            .set_lead(
                &project.id,
                Some(SavedLead {
                    cli: "claude".into(),
                    session_id: Some("--dangerously-bad".into()),
                    reported_workers: Vec::new(),
                    chat_history: None,
                }),
            )
            .unwrap();
        let (fourth, report) = start_lead_with(&core, &scratch, &project.id, "claude");
        assert_eq!(
            fourth.conversation,
            LeadConversation::Fresh(FreshReason::NoSavedSession)
        );
        assert!(!report_args(&report).contains(&"--dangerously-bad".to_string()));
    }

    #[test]
    fn pi_resumes_with_the_same_session_id_flag() {
        let scratch = Scratch::new();
        let repo = scratch.repo("demo");
        let core = core_with_reporting_cli(&scratch);
        let project = core.add_project(&repo).unwrap().project;
        let (first, report) = start_lead_with(&core, &scratch, &project.id, "pi");
        let args = report_args(&report);
        let at = args.iter().position(|arg| arg == "--session-id").unwrap();
        let id = args[at + 1].clone();
        assert_eq!(args[at + 2], "--append-system-prompt");
        assert_eq!(args[at + 3], LEAD_INSTRUCTIONS);
        assert_eq!(args[0], "--approve");
        core.stop_lead(&first.session.id).unwrap();

        // Pi's own folder name for the Lead's cwd: `--` + path with `/` as `-`.
        let cwd = first.session.worktree.canonicalize().unwrap();
        let folder = format!(
            "--{}--",
            cwd.to_string_lossy()
                .trim_start_matches('/')
                .replace('/', "-")
        );
        let dir = scratch.path.join("home/.pi/agent/sessions").join(folder);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join(format!("2026-10-15T18-05-14-237Z_{id}.jsonl")),
            "{}\n",
        )
        .unwrap();

        let (again, report) = start_lead_with(&core, &scratch, &project.id, "pi");
        assert_eq!(again.conversation, LeadConversation::Resumed);
        assert_eq!(conversation_arg(&report, "--session-id"), id);
    }

    #[test]
    fn codex_has_no_id_until_discovered_then_resumes_with_the_resume_subcommand() {
        let scratch = Scratch::new();
        let repo = scratch.repo("demo");
        let core = core_with_reporting_cli(&scratch);
        let project = core.add_project(&repo).unwrap().project;
        let (first, report) = start_lead_with(&core, &scratch, &project.id, "codex");
        let args = report_args(&report);
        assert_eq!(args[0], "--dangerously-bypass-approvals-and-sandbox");
        assert!(
            args.iter().any(|arg| arg.starts_with("projects={")),
            "{args:?}"
        );
        assert!(
            args.last()
                .unwrap()
                .starts_with("developer_instructions=\"You lead"),
            "{args:?}"
        );
        assert!(!args.contains(&"resume".to_string()));
        assert_eq!(
            core.saved_lead(&project.id).unwrap(),
            Some(SavedLead {
                cli: "codex".into(),
                session_id: None,
                reported_workers: Vec::new(),
                chat_history: None,
            })
        );
        // Nothing to find until Codex writes the conversation at the first message.
        assert_eq!(
            core.discover_lead_session(&first.session.id, first.started_at),
            Ok(None)
        );
        let launched_ms = first
            .started_at
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        let db = scratch.path.join("home/.codex/state_5.sqlite");
        fs::create_dir_all(db.parent().unwrap()).unwrap();
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE threads (id TEXT PRIMARY KEY, cwd TEXT NOT NULL, created_at INTEGER NOT NULL,
             created_at_ms INTEGER, archived INTEGER NOT NULL DEFAULT 0, agent_role TEXT);",
        )
        .unwrap();
        let cwd = first.session.worktree.canonicalize().unwrap();
        let cwd = cwd.to_string_lossy().into_owned();
        for (id, created) in [
            ("older-run", launched_ms - 60_000),
            ("this-run", launched_ms + 500),
        ] {
            conn.execute(
                "INSERT INTO threads (id, cwd, created_at, created_at_ms) VALUES (?1, ?2, ?3, ?4)",
                (id, &cwd, created / 1000, created),
            )
            .unwrap();
        }
        drop(conn);
        assert_eq!(
            core.discover_lead_session(&first.session.id, first.started_at),
            Ok(Some("this-run".to_string()))
        );
        assert_eq!(
            core.saved_lead(&project.id)
                .unwrap()
                .unwrap()
                .session_id
                .as_deref(),
            Some("this-run")
        );
        // Once saved, the answer is the saved id.
        assert_eq!(
            core.discover_lead_session(&first.session.id, SystemTime::now()),
            Ok(Some("this-run".to_string()))
        );
        core.stop_lead(&first.session.id).unwrap();

        let (again, report) = start_lead_with(&core, &scratch, &project.id, "codex");
        assert_eq!(again.conversation, LeadConversation::Resumed);
        let args = report_args(&report);
        assert_eq!(args[0], "resume");
        assert_eq!(args[1], "--dangerously-bypass-approvals-and-sandbox");
        assert_eq!(args.last().unwrap(), "this-run");
        assert!(
            args[args.len() - 2].starts_with("developer_instructions=\"You lead"),
            "{args:?}"
        );
    }

    #[test]
    fn discovery_ignores_stale_leads_and_clis_whose_id_shika_chose() {
        let scratch = Scratch::new();
        let repo = scratch.repo("demo");
        let core = core_with_reporting_cli(&scratch);
        let project = core.add_project(&repo).unwrap().project;
        assert_eq!(core.discover_lead_session("missing", UNIX_EPOCH), Ok(None));
        let (start, report) = start_lead_with(&core, &scratch, &project.id, "claude");
        let id = conversation_arg(&report, "--session-id");
        assert_eq!(
            core.discover_lead_session(&start.session.id, start.started_at),
            Ok(Some(id))
        );
        core.set_lead_session(&project.id, None).unwrap();
        assert_eq!(
            core.discover_lead_session(&start.session.id, start.started_at),
            Ok(None)
        );
        assert!(matches!(
            core.set_lead_session(&project.id, Some("--x")),
            Err(Error::InvalidPrompt(_))
        ));
        core.set_lead_session(&project.id, Some("abc-1")).unwrap();
        assert_eq!(
            core.saved_lead(&project.id)
                .unwrap()
                .unwrap()
                .session_id
                .as_deref(),
            Some("abc-1")
        );
        assert_eq!(
            core.set_lead_session("missing", None),
            Err(Error::UnknownProject)
        );
    }

    /// An `agent` stand-in: `create-chat` runs `create`, anything else is a
    /// launch that prints its arguments like `SWITCH_CLI`.
    fn cursor_cli(scratch: &Scratch, create: &str) {
        let launch = SWITCH_CLI.trim_start_matches("#!/bin/sh\n");
        write_exec(
            &scratch.path,
            "agent",
            &format!("#!/bin/sh\nif [ \"$1\" = create-chat ]; then\n{create}\nfi\n{launch}"),
        );
    }

    #[test]
    fn cursor_gets_a_new_chat_id_and_a_bootstrap_prompt_only_when_fresh() {
        let scratch = Scratch::new();
        let repo = scratch.repo("demo");
        let core = core_with_reporting_cli(&scratch);
        let project = core.add_project(&repo).unwrap().project;
        let chat = "5c0ffee0-0000-4000-8000-000000000001";
        cursor_cli(&scratch, &format!("echo {chat}\nexit 0"));

        let (first, report) = start_lead_with(&core, &scratch, &project.id, "cursor");
        assert!(first.bootstrap_sent);
        assert_eq!(
            report_args(&report),
            [
                "--yolo",
                "--trust",
                "--sandbox",
                "disabled",
                "--resume",
                chat,
                LEAD_BOOTSTRAP
            ]
        );
        assert_eq!(
            core.saved_lead(&project.id).unwrap(),
            Some(SavedLead {
                cli: "cursor".into(),
                session_id: Some(chat.into()),
                reported_workers: Vec::new(),
                chat_history: None,
            })
        );
        core.stop_lead(&first.session.id).unwrap();

        // Cursor keeps nothing Shika can read, so the chat is assumed to
        // exist. A resume carries no positional prompt.
        let (again, report) = start_lead_with(&core, &scratch, &project.id, "cursor");
        assert_eq!(again.conversation, LeadConversation::Resumed);
        assert!(!again.bootstrap_sent);
        assert_eq!(
            report_args(&report),
            [
                "--yolo",
                "--trust",
                "--sandbox",
                "disabled",
                "--resume",
                chat
            ]
        );
    }

    #[test]
    fn a_failed_create_chat_launches_cursor_with_the_bootstrap_and_no_id() {
        for create in ["echo boom >&2\nexit 1", "echo --resume\nexit 0", "exit 0"] {
            let scratch = Scratch::new();
            let repo = scratch.repo("demo");
            let core = core_with_reporting_cli(&scratch);
            let project = core.add_project(&repo).unwrap().project;
            cursor_cli(&scratch, create);
            let (first, report) = start_lead_with(&core, &scratch, &project.id, "cursor");
            assert_eq!(
                report_args(&report),
                ["--yolo", "--trust", "--sandbox", "disabled", LEAD_BOOTSTRAP],
                "{create}"
            );
            assert_eq!(
                core.saved_lead(&project.id).unwrap(),
                Some(SavedLead {
                    cli: "cursor".into(),
                    session_id: None,
                    reported_workers: Vec::new(),
                    chat_history: None,
                })
            );
            core.stop_lead(&first.session.id).unwrap();
            let (again, _) = start_lead_with(&core, &scratch, &project.id, "cursor");
            assert_eq!(
                again.conversation,
                LeadConversation::Fresh(FreshReason::NoSavedSession)
            );
        }
    }

    #[test]
    fn a_dirty_lead_worktree_is_kept_and_reported_on_restart() {
        let scratch = Scratch::new();
        let repo = scratch.repo("demo");
        let core = core_with_reporting_cli(&scratch);
        let project = core.add_project(&repo).unwrap().project;
        let (first, _) = start_lead_with(&core, &scratch, &project.id, "claude");
        core.stop_lead(&first.session.id).unwrap();
        assert_eq!(core.lead_has_changes(&project.id), Ok(false));
        fs::write(first.session.worktree.join("notes.txt"), "mine\n").unwrap();
        assert_eq!(core.lead_has_changes(&project.id), Ok(true));
        git(&repo, &["commit", "--allow-empty", "-m", "second"]);

        let (again, _) = start_lead_with(&core, &scratch, &project.id, "claude");
        assert_eq!(again.worktree, LeadWorktree::KeptDirty);
        assert!(again.session.worktree.join("notes.txt").exists());
        assert_ne!(
            git(&again.session.worktree, &["rev-parse", "HEAD"]),
            git(&repo, &["rev-parse", "main"])
        );
    }

    #[test]
    fn a_lead_folder_that_is_not_a_worktree_fails_the_start_and_is_never_deleted() {
        let scratch = Scratch::new();
        let repo = scratch.repo("demo");
        let core = core_with_reporting_cli(&scratch);
        let project = core.add_project(&repo).unwrap().project;
        let path = repo.join(".worktrees").join("shika-lead");
        fs::create_dir_all(&path).unwrap();
        fs::write(path.join("mine.txt"), "keep\n").unwrap();

        let (sink, _rx) = channel_sink();
        let result = core.start_lead(
            &project.id,
            "claude",
            PtySize::default(),
            sink,
            lead_launch(&scratch),
        );
        assert_eq!(
            result.map(|_| ()),
            Err(Error::LeadWorktreeInvalid(path.clone()))
        );
        assert!(path.join("mine.txt").exists());
        assert!(core.worktree_journal().unwrap().is_empty());
        assert_eq!(core.saved_lead(&project.id).unwrap(), None);
        assert!(core.sessions().is_empty());
        assert_eq!(
            Error::LeadWorktreeInvalid(path.clone()).to_string(),
            format!(
                "{} exists but is not a worktree of this repository. Move or delete it, then start the Lead again.",
                path.display()
            )
        );
    }

    #[test]
    fn a_failed_launch_removes_a_tree_it_created_and_keeps_an_older_one() {
        let scratch = Scratch::new();
        let repo = scratch.repo("demo");
        let core = core_with_reporting_cli(&scratch);
        let project = core.add_project(&repo).unwrap().project;
        let program = scratch.path.join("bin/claude");
        let script = fs::read_to_string(&program).unwrap();
        fs::remove_file(&program).unwrap();
        let try_start = || {
            let (sink, _rx) = channel_sink();
            core.start_lead(
                &project.id,
                "claude",
                PtySize::default(),
                sink,
                lead_launch(&scratch),
            )
            .map(|_| ())
        };

        // The tree this start made is untouched, so it goes.
        assert!(matches!(try_start(), Err(Error::OpenPty(_))));
        assert!(!repo.join(".worktrees/shika-lead").exists());
        assert!(core.worktree_journal().unwrap().is_empty());
        assert_eq!(core.saved_lead(&project.id).unwrap(), None);
        assert!(core.sessions().is_empty());

        // An existing tree may hold the author's notes: it stays, with its record.
        fs::write(&program, script).unwrap();
        fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).unwrap();
        let (first, _) = start_lead_with(&core, &scratch, &project.id, "claude");
        core.stop_lead(&first.session.id).unwrap();
        let saved = core.saved_lead(&project.id).unwrap();
        fs::remove_file(&program).unwrap();
        assert!(matches!(try_start(), Err(Error::OpenPty(_))));
        assert!(first.session.worktree.exists());
        assert_eq!(core.worktree_journal().unwrap().len(), 1);
        assert_eq!(core.saved_lead(&project.id).unwrap(), saved);
        assert!(core.leftovers_list().unwrap().is_empty());
    }

    #[test]
    fn forgetting_a_lead_removes_its_tree_journal_entry_and_record() {
        let scratch = Scratch::new();
        let repo = scratch.repo("demo");
        let core = core_with_reporting_cli(&scratch);
        let project = core.add_project(&repo).unwrap().project;
        let before = branches(&repo);
        // Nothing to forget is not an error.
        core.forget_lead(&project.id, false).unwrap();
        assert_eq!(
            core.forget_lead("missing", false),
            Err(Error::UnknownProject)
        );

        let (first, _) = start_lead_with(&core, &scratch, &project.id, "claude");
        let tree = first.session.worktree.clone();
        // A live Lead is stopped first.
        assert_eq!(core.forget_lead(&project.id, true), Err(Error::LeadRunning));
        core.stop_lead(&first.session.id).unwrap();

        // Edits need the author's say-so.
        fs::write(tree.join("notes.txt"), "mine\n").unwrap();
        assert_eq!(
            core.forget_lead(&project.id, false),
            Err(Error::WorktreeHasChanges(None))
        );
        assert!(tree.exists());
        assert!(core.saved_lead(&project.id).unwrap().is_some());
        assert_eq!(core.worktree_journal().unwrap().len(), 1);

        core.forget_lead(&project.id, true).unwrap();
        assert!(!tree.exists());
        assert!(core.worktree_journal().unwrap().is_empty());
        assert_eq!(core.saved_lead(&project.id).unwrap(), None);
        assert!(!git(&repo, &["worktree", "list"]).contains("shika-lead"));
        assert_eq!(branches(&repo), before);

        // Clean, it needs no confirmation, and the next start is fresh.
        let (second, _) = start_lead_with(&core, &scratch, &project.id, "claude");
        assert_eq!(
            second.conversation,
            LeadConversation::Fresh(FreshReason::NoSavedSession)
        );
        assert_eq!(second.worktree, LeadWorktree::Created);
        core.stop_lead(&second.session.id).unwrap();
        core.forget_lead(&project.id, false).unwrap();
        assert!(!tree.exists());
    }

    #[test]
    fn forgetting_a_lead_never_deletes_a_folder_that_is_not_its_worktree() {
        let scratch = Scratch::new();
        let repo = scratch.repo("demo");
        let core = core_with_reporting_cli(&scratch);
        let project = core.add_project(&repo).unwrap().project;
        let path = repo.join(".worktrees").join("shika-lead");
        fs::create_dir_all(&path).unwrap();
        fs::write(path.join("mine.txt"), "keep\n").unwrap();
        core.projects
            .set_lead(
                &project.id,
                Some(SavedLead {
                    cli: "claude".into(),
                    session_id: None,
                    reported_workers: Vec::new(),
                    chat_history: None,
                }),
            )
            .unwrap();
        core.journal
            .add(&JournalEntry {
                project_id: project.id.clone(),
                branch: String::new(),
                path: path.clone(),
                base_ref: None,
                started_by_lead: false,
            })
            .unwrap();
        core.forget_lead(&project.id, true).unwrap();
        assert!(path.join("mine.txt").exists());
        assert!(core.worktree_journal().unwrap().is_empty());
        assert_eq!(core.saved_lead(&project.id).unwrap(), None);
    }

    #[test]
    fn stopping_needs_a_live_lead_and_leaves_its_workers_running() {
        let scratch = Scratch::new();
        let repo = scratch.repo("demo");
        let core = core_with_reporting_cli(&scratch);
        let project = core.add_project(&repo).unwrap().project;
        let (lead, _) = start_lead(&core, &scratch, &project.id);
        let (sink, _rx) = channel_sink();
        let worker = core
            .create_session(
                &project.id,
                "claude",
                PtySize::default(),
                sink,
                LaunchOptions {
                    prompt: Some("Fix it".into()),
                    started_by: Some(lead.id.clone()),
                    ..LaunchOptions::default()
                },
            )
            .unwrap();
        assert_eq!(core.stop_lead(&worker.id), Err(Error::UnknownSession));
        assert_eq!(core.stop_lead("missing"), Err(Error::UnknownSession));
        core.stop_lead(&lead.id).unwrap();
        assert_eq!(core.sessions(), vec![worker.clone()]);
        assert_eq!(core.stop_lead(&lead.id), Err(Error::UnknownSession));
        assert!(core.write(worker.pty.unwrap(), b"").is_ok());
        assert!(core.lost_workers(&project.id).unwrap().is_empty());
    }

    #[test]
    fn a_restarted_lead_reports_workers_it_started_that_lost_their_cards() {
        let scratch = Scratch::new();
        let repo = scratch.repo("demo");
        let core = core_with_reporting_cli(&scratch);
        let project = core.add_project(&repo).unwrap().project;
        let (lead, _) = start_lead(&core, &scratch, &project.id);
        let launch = |started_by: Option<String>| {
            let (sink, _rx) = channel_sink();
            core.create_session(
                &project.id,
                "claude",
                PtySize::default(),
                sink,
                LaunchOptions {
                    prompt: Some("Do it".into()),
                    started_by,
                    ..LaunchOptions::default()
                },
            )
            .unwrap()
        };
        let worker = launch(Some(lead.id.clone()));
        let author = launch(None);
        let closed = launch(Some(lead.id.clone()));
        core.session_discard(&closed.id).unwrap();
        let journal = core.worktree_journal().unwrap();
        let flagged: Vec<_> = journal
            .iter()
            .filter(|entry| entry.started_by_lead)
            .map(|entry| entry.branch.clone())
            .collect();
        assert_eq!(flagged, std::slice::from_ref(&worker.branch));
        assert!(core.lost_workers(&project.id).unwrap().is_empty());
        drop(core);

        // A relaunch: no live sessions, journal intact.
        let core = core_with_reporting_cli(&scratch);
        let expected = vec![LostWorker {
            branch: worker.branch.clone(),
            path: worker.worktree.clone(),
        }];
        assert_eq!(core.lost_workers(&project.id).unwrap(), expected);
        assert!(core.lost_workers("other").unwrap().is_empty());
        let (start, _) = start_lead_with(&core, &scratch, &project.id, "claude");
        assert_eq!(start.lost_workers, expected);
        assert!(!start.bootstrap_sent, "only a fresh Cursor start has one");
        assert!(author.worktree.exists());

        // Once the summary named them, a later relaunch does not repeat them,
        // but the journal still lists them.
        core.mark_workers_reported(&project.id, std::slice::from_ref(&worker.worktree))
            .unwrap();
        core.stop_lead(&start.session.id).unwrap();
        let (again, _) = start_lead_with(&core, &scratch, &project.id, "claude");
        assert!(again.lost_workers.is_empty());
        assert_eq!(core.lost_workers(&project.id).unwrap(), expected);
        assert_eq!(
            core.saved_lead(&project.id)
                .unwrap()
                .unwrap()
                .reported_workers,
            std::slice::from_ref(&worker.worktree)
        );
        assert_eq!(
            core.mark_workers_reported("other", &[]),
            Err(Error::UnknownProject)
        );

        // Once the author cleans it up, it is not reported again.
        core.leftover_remove(&worker.worktree).unwrap();
        assert!(core.lost_workers(&project.id).unwrap().is_empty());
    }

    #[test]
    fn a_lead_refuses_branch_diff_publish_and_shell_operations() {
        let scratch = Scratch::new();
        let repo = scratch.repo("demo");
        let core = core_with_reporting_cli(&scratch);
        let project = core.add_project(&repo).unwrap().project;
        let (lead, _) = start_lead(&core, &scratch, &project.id);
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
            core.session_publish_now(&lead.id, None, None),
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
        let (sink, _rx) = channel_sink();
        assert_eq!(
            core.session_switch_agent(
                &lead.id,
                "claude",
                Some("Continue the backend."),
                PtySize::default(),
                sink,
                None,
            ),
            Err(Error::LeadUnsupported)
        );
        assert_eq!(core.session(&lead.id), Some(lead));
    }

    /// Prints every launch argument, then idles. A prompt must arrive as one
    /// positional argument after the preset's own flags. An empty prompt adds none.
    const SWITCH_CLI: &str = "#!/bin/sh\nlast=\nfor a in \"$@\"; do\n  printf 'arg:%s\\n' \"$a\"\n  last=$a\ndone\nprintf 'last:%s\\n' \"$last\"\nprintf 'cwd:%s\\n' \"$(pwd -P)\"\nprintf 'sock:%s\\n' \"$SHIKA_SOCKET\"\nprintf 'token:%s\\n' \"$SHIKA_TOKEN\"\nprintf 'path:%s\\n' \"$PATH\"\nprintf 'end-report\\n'\nwhile IFS= read -r line; do :; done\n";

    const SWITCH_PROMPT: &str = "Continue the backend.";

    fn write_exec(dir: &Path, name: &str, body: &str) -> PathBuf {
        let bin = dir.join("bin");
        fs::create_dir_all(&bin).unwrap();
        let path = bin.join(name);
        fs::write(&path, body).unwrap();
        let mut perms = fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&path, perms).unwrap();
        path
    }

    fn core_with_switch_clis(scratch: &Scratch) -> Core {
        let claude = write_exec(&scratch.path, "claude", SWITCH_CLI);
        let agent = write_exec(&scratch.path, "agent", SWITCH_CLI);
        let pi = write_exec(&scratch.path, "pi", SWITCH_CLI);
        let codex = write_exec(&scratch.path, "codex", SWITCH_CLI);
        let env = PathEnv::from_lookup(
            "/usr/bin:/bin".into(),
            None,
            &[
                ("claude", Some(claude)),
                ("agent", Some(agent)),
                ("pi", Some(pi)),
                ("codex", Some(codex)),
            ],
        );
        Core::open_with(scratch.path.join("data"), env).unwrap()
    }

    fn report_args(report: &str) -> Vec<String> {
        report
            .lines()
            .filter_map(|line| line.trim().strip_prefix("arg:").map(str::to_string))
            .collect()
    }

    fn switch_agent(
        core: &Core,
        id: &str,
        preset: &str,
        prompt: Option<&str>,
        control: Option<WorkerEnv>,
    ) -> (Session, String) {
        let (sink, rx) = channel_sink();
        let session = core
            .session_switch_agent(id, preset, prompt, PtySize::new(30, 90), sink, control)
            .unwrap();
        let report = collect_until(&rx, "end-report", Duration::from_secs(5));
        (session, report)
    }

    #[test]
    fn switching_the_agent_keeps_the_worktree_branch_shells_and_naming() {
        let scratch = Scratch::new();
        let repo = scratch.repo("demo");
        let core = core_with_switch_clis(&scratch);
        let project = core.add_project(&repo).unwrap().project;
        let (sink, _rx) = channel_sink();
        let session = core
            .create_session(
                &project.id,
                "claude",
                PtySize::new(30, 90),
                sink,
                LaunchOptions::default(),
            )
            .unwrap();
        let (shell_sink, _shell_rx) = channel_sink();
        let shell = core
            .open_shell(&session.id, PtySize::new(30, 90), shell_sink)
            .unwrap();
        fs::write(session.worktree.join("wip.txt"), "keep\n").unwrap();
        fs::create_dir_all(repo.join(".shika")).unwrap();
        fs::write(
            repo.join(preparation::CONFIG_PATH),
            "{\"setup-worktree\":[\"touch ran-setup\"]}",
        )
        .unwrap();
        let branch = session.branch.clone();
        let title = session.title.clone();
        let worktree = session.worktree.clone();
        let base = session.base_ref.clone();
        let old_pty = session.pty;
        assert_eq!(branch, format!("shika-draft-{}", session.id));
        assert!(!session.cli_titled);

        let (switched, report) =
            switch_agent(&core, &session.id, "cursor", Some(SWITCH_PROMPT), None);
        assert_eq!(
            report_args(&report),
            vec![
                "--yolo".to_string(),
                "--trust".to_string(),
                "--sandbox".to_string(),
                "disabled".to_string(),
                SWITCH_PROMPT.to_string(),
            ],
            "{report}"
        );
        assert!(
            report.contains(&format!("cwd:{}", worktree.display())),
            "{report}"
        );
        assert!(
            report.contains("sock:\r\n") && report.contains("token:\r\n"),
            "{report}"
        );
        assert!(!report.contains("shika report"), "{report}");
        assert!(!session.worktree.join("ran-setup").exists());
        assert_eq!(
            fs::read_to_string(worktree.join("wip.txt")).unwrap(),
            "keep\n"
        );
        assert_eq!(
            git(&worktree, &["symbolic-ref", "--short", "HEAD"]).trim(),
            branch
        );
        assert_eq!(switched.branch, branch);
        // Unnamed, the title follows the CLI; a named task keeps its own.
        assert_eq!(title, "New Claude Code");
        assert_eq!(switched.title, "New Cursor CLI");
        assert!(!switched.manual_title);
        // Still on its draft branch, the task is named later as New would.
        assert!(!switched.cli_titled);
        assert_eq!(switched.worktree, worktree);
        assert_eq!(switched.base_ref, base);
        assert_eq!(switched.started_by, None);
        assert_eq!(switched.preset_id, "cursor");
        assert_eq!(switched.preset_name, "Cursor CLI");
        assert_ne!(switched.pty, old_pty);
        assert_eq!(switched.shell_ptys, vec![shell]);
        assert!(!switched.lead);
        assert_eq!(
            core.write(old_pty.unwrap(), b"still there\n"),
            Err(Error::UnknownPty)
        );
        assert!(core.write(shell, b"echo shells-stay\n").is_ok());
        assert!(
            core.write(switched.pty.unwrap(), b"echo reviewer\n")
                .is_ok()
        );
        let journal = core
            .worktree_journal()
            .unwrap()
            .into_iter()
            .find(|entry| entry.path == worktree)
            .unwrap();
        assert_eq!(journal.branch, branch);

        let cursor_pty = switched.pty;
        let (again, report) = switch_agent(&core, &session.id, "claude", Some(SWITCH_PROMPT), None);
        assert_eq!(
            report_args(&report),
            vec![
                "--dangerously-skip-permissions".to_string(),
                SWITCH_PROMPT.to_string(),
            ],
            "{report}"
        );
        assert_eq!(again.branch, branch);
        assert_eq!(again.title, title);
        assert!(!again.cli_titled);
        assert_eq!(again.preset_id, "claude");
        assert_ne!(again.pty, cursor_pty);
        assert_eq!(again.shell_ptys, vec![shell]);
        assert_eq!(
            core.write(cursor_pty.unwrap(), b"x"),
            Err(Error::UnknownPty)
        );
        assert!(core.write(shell, b"echo still\n").is_ok());

        let (pi, report) = switch_agent(&core, &session.id, "pi", Some(SWITCH_PROMPT), None);
        let args = report_args(&report);
        assert_eq!(
            args.first().map(String::as_str),
            Some("--approve"),
            "{report}"
        );
        assert!(args.iter().any(|arg| arg == "--extension"), "{report}");
        assert_eq!(
            args.last().map(String::as_str),
            Some(SWITCH_PROMPT),
            "{report}"
        );
        assert!(
            !args
                .iter()
                .any(|arg| arg == "--worktree" || arg.starts_with("--worktree=")),
            "{report}"
        );
        assert_eq!(pi.branch, branch);
        assert_eq!(pi.shell_ptys, vec![shell]);
        assert!(core.write(shell, b"echo after-pi\n").is_ok());

        let (blank, report) = switch_agent(&core, &session.id, "claude", None, None);
        assert_eq!(
            report_args(&report),
            vec!["--dangerously-skip-permissions".to_string()],
            "{report}"
        );
        assert_eq!(blank.branch, branch);
        assert_eq!(blank.title, title);
        assert!(!blank.cli_titled);
        assert_eq!(blank.shell_ptys, vec![shell]);

        let (spaces, report) = switch_agent(&core, &session.id, "claude", Some("  \n"), None);
        assert_eq!(
            report_args(&report),
            vec!["--dangerously-skip-permissions".to_string()],
            "{report}"
        );
        assert_eq!(spaces.branch, branch);
        assert_eq!(spaces.title, title);
        assert!(!spaces.cli_titled);

        // The first line typed into the new CLI names the draft task.
        let renamed = core
            .session_rename_from_prompt(&session.id, "Add the export button")
            .unwrap();
        assert_ne!(renamed.branch, branch);
        assert_eq!(renamed.title, "Add the export button");
        assert_eq!(
            git(&worktree, &["symbolic-ref", "--short", "HEAD"]).trim(),
            renamed.branch
        );
        let (later, _) = switch_agent(&core, &session.id, "cursor", Some(SWITCH_PROMPT), None);
        assert_eq!(later.branch, renamed.branch);
        assert_eq!(later.title, renamed.title);
        assert!(later.cli_titled);

        fs::remove_file(repo.join(preparation::CONFIG_PATH)).unwrap();
        let (sink, _rx) = channel_sink();
        let named = core
            .create_session(
                &project.id,
                "claude",
                PtySize::new(30, 90),
                sink,
                LaunchOptions::default(),
            )
            .unwrap();
        let named = core
            .session_rename_from_prompt(&named.id, "Fix the login bug")
            .unwrap();
        let named_branch = named.branch.clone();
        let named_title = named.title.clone();
        assert_ne!(named_branch, format!("shika-draft-{}", named.id));
        assert!(!named.cli_titled);
        let (kept, _) = switch_agent(&core, &named.id, "cursor", Some(SWITCH_PROMPT), None);
        assert_eq!(kept.branch, named_branch);
        assert_eq!(kept.title, named_title);
        assert!(kept.cli_titled);
        assert_eq!(core.session_apply_cli_title(&named.id).unwrap(), None);
        assert_eq!(
            git(&kept.worktree, &["symbolic-ref", "--short", "HEAD"]).trim(),
            named_branch
        );
    }

    #[test]
    fn a_failed_agent_switch_leaves_the_original_process_running() {
        let scratch = Scratch::new();
        let repo = scratch.repo("demo");
        let claude = write_exec(&scratch.path, "claude", SWITCH_CLI);
        let env = PathEnv::from_lookup(
            "/usr/bin:/bin".into(),
            None,
            &[
                ("claude", Some(claude)),
                (
                    "agent",
                    Some(scratch.path.join("bin").join("missing-agent")),
                ),
            ],
        );
        let core = Core::open_with(scratch.path.join("data"), env).unwrap();
        let project = core.add_project(&repo).unwrap().project;
        let (sink, _rx) = channel_sink();
        let session = core
            .create_session(
                &project.id,
                "claude",
                PtySize::new(30, 90),
                sink,
                LaunchOptions::default(),
            )
            .unwrap();
        let old = session.pty;
        let (sink, _rx) = channel_sink();
        assert!(
            core.session_switch_agent(
                &session.id,
                "cursor",
                Some(SWITCH_PROMPT),
                PtySize::new(30, 90),
                sink,
                None,
            )
            .is_err()
        );
        let stayed = core.session(&session.id).unwrap();
        assert_eq!(stayed.pty, old);
        assert_eq!(stayed.preset_id, "claude");
        assert!(!stayed.cli_titled);
        assert_eq!(stayed.branch, session.branch);
        assert!(core.write(old.unwrap(), b"still running\n").is_ok());

        for prompt in ["-x", "a\0b"] {
            let (sink, _rx) = channel_sink();
            assert!(
                matches!(
                    core.session_switch_agent(
                        &session.id,
                        "claude",
                        Some(prompt),
                        PtySize::default(),
                        sink,
                        None,
                    ),
                    Err(Error::InvalidPrompt(_))
                ),
                "{prompt:?}"
            );
        }
        let (sink, _rx) = channel_sink();
        assert_eq!(
            core.session_switch_agent(
                &session.id,
                "nope",
                Some(SWITCH_PROMPT),
                PtySize::default(),
                sink,
                None,
            ),
            Err(Error::UnknownCli)
        );
        let stayed = core.session(&session.id).unwrap();
        assert_eq!(stayed.pty, old);
        assert!(!stayed.cli_titled);
        assert!(core.write(old.unwrap(), b"still running\n").is_ok());
    }

    #[test]
    fn a_worker_switch_keeps_its_owner_and_control_env_without_the_new_task_footer() {
        let scratch = Scratch::new();
        let repo = scratch.repo("demo");
        let core = core_with_switch_clis(&scratch);
        let project = core.add_project(&repo).unwrap().project;
        let (lead, _) = start_lead(&core, &scratch, &project.id);
        let control = scratch.path.join("control");
        let env = WorkerEnv {
            socket: control.join("sock"),
            token: "worker-token".into(),
            bin_dir: control.join("bin"),
        };
        let (sink, _rx) = channel_sink();
        let worker = core
            .create_session(
                &project.id,
                "claude",
                PtySize::new(30, 90),
                sink,
                LaunchOptions {
                    prompt: Some("Fix the login bug".into()),
                    started_by: Some(lead.id.clone()),
                    control: Some(env.clone()),
                    ..LaunchOptions::default()
                },
            )
            .unwrap();
        let (switched, report) =
            switch_agent(&core, &worker.id, "cursor", Some(SWITCH_PROMPT), Some(env));
        assert_eq!(switched.started_by.as_deref(), Some(lead.id.as_str()));
        assert_eq!(switched.branch, worker.branch);
        assert_eq!(
            report_args(&report).last().map(String::as_str),
            Some(SWITCH_PROMPT)
        );
        assert!(!report.contains("shika report"), "{report}");
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
    }

    #[test]
    fn a_re_added_project_claims_the_lead_tree_so_leftovers_never_offers_it() {
        let scratch = Scratch::new();
        let repo = scratch.repo("demo");
        let core = core_with_reporting_cli(&scratch);
        let old = core.add_project(&repo).unwrap().project;
        let (lead, _) = start_lead(&core, &scratch, &old.id);
        core.stop_lead(&lead.id).unwrap();
        core.remove_project(&old.id).unwrap();
        let new = core.add_project(&repo).unwrap().project;
        assert_ne!(old.id, new.id);
        let (lead, _) = start_lead(&core, &scratch, &new.id);
        fs::write(lead.worktree.join("notes.txt"), "mine\n").unwrap();
        core.stop_lead(&lead.id).unwrap();
        assert!(core.saved_lead(&new.id).unwrap().is_some());
        let entries = core.journal.list().unwrap();
        let entry = entries
            .iter()
            .find(|entry| entry.path == lead.worktree)
            .unwrap();
        assert_eq!(entry.project_id, new.id);
        assert_eq!(
            entries.iter().filter(|e| e.path == lead.worktree).count(),
            1
        );
        assert!(core.leftovers_list().unwrap().is_empty());
        assert_eq!(
            core.leftover_remove(&lead.worktree),
            Err(Error::UnknownLeftover)
        );
        assert!(lead.worktree.join("notes.txt").exists());
    }

    #[test]
    fn commits_in_the_lead_tree_are_kept_by_refresh_and_need_discard_to_forget() {
        let scratch = Scratch::new();
        let repo = scratch.repo("demo");
        let core = core_with_reporting_cli(&scratch);
        let project = core.add_project(&repo).unwrap().project;
        let (lead, _) = start_lead(&core, &scratch, &project.id);
        fs::write(lead.worktree.join("plan.md"), "plan\n").unwrap();
        git(&lead.worktree, &["add", "plan.md"]);
        git(&lead.worktree, &["commit", "-q", "-m", "lead commit"]);
        let commit = git(&lead.worktree, &["rev-parse", "HEAD"])
            .trim()
            .to_string();
        core.stop_lead(&lead.id).unwrap();
        assert_eq!(
            core.lead_loss(&project.id),
            Ok(LeadLoss {
                uncommitted: false,
                unreferenced_commits: true
            })
        );
        assert_eq!(core.lead_has_changes(&project.id), Ok(true));

        // Not moved, and the summary says why.
        git(&repo, &["commit", "--allow-empty", "-m", "second"]);
        let (again, _) = start_lead_with(&core, &scratch, &project.id, "claude");
        assert_eq!(again.worktree, LeadWorktree::KeptCommits);
        let head = git(&again.session.worktree, &["rev-parse", "HEAD"]);
        assert_eq!(head.trim(), commit);
        core.stop_lead(&again.session.id).unwrap();

        // Forget asks first, and discards only when told to.
        assert_eq!(
            core.forget_lead(&project.id, false),
            Err(Error::WorktreeHasChanges(None))
        );
        assert!(lead.worktree.exists());
        core.forget_lead(&project.id, true).unwrap();
        assert!(!lead.worktree.exists());
    }

    #[test]
    fn a_lead_commit_that_a_branch_reaches_does_not_block_refresh_or_forget() {
        let scratch = Scratch::new();
        let repo = scratch.repo("demo");
        let core = core_with_reporting_cli(&scratch);
        let project = core.add_project(&repo).unwrap().project;
        let (lead, _) = start_lead(&core, &scratch, &project.id);
        fs::write(lead.worktree.join("plan.md"), "plan\n").unwrap();
        git(&lead.worktree, &["add", "plan.md"]);
        git(&lead.worktree, &["commit", "-q", "-m", "lead commit"]);
        git(&lead.worktree, &["branch", "keep-it"]);
        core.stop_lead(&lead.id).unwrap();
        assert_eq!(core.lead_loss(&project.id), Ok(LeadLoss::default()));
        git(&repo, &["commit", "--allow-empty", "-m", "second"]);
        let (again, _) = start_lead_with(&core, &scratch, &project.id, "claude");
        assert!(matches!(again.worktree, LeadWorktree::Refreshed { .. }));
        core.stop_lead(&again.session.id).unwrap();
        core.forget_lead(&project.id, false).unwrap();
    }

    #[test]
    fn a_symlink_at_the_lead_path_is_never_followed_by_forget_or_refresh() {
        let scratch = Scratch::new();
        let repo = scratch.repo("demo");
        let core = core_with_reporting_cli(&scratch);
        let project = core.add_project(&repo).unwrap().project;
        let (lead, _) = start_lead(&core, &scratch, &project.id);
        core.stop_lead(&lead.id).unwrap();
        let (sink, _rx) = channel_sink();
        let task = core
            .create_session(
                &project.id,
                "claude",
                PtySize::default(),
                sink,
                LaunchOptions::default(),
            )
            .unwrap();
        fs::write(task.worktree.join("work.txt"), "precious\n").unwrap();
        // The Lead folder replaced by a link to the task's tree.
        git(
            &repo,
            &[
                "worktree",
                "remove",
                "--force",
                lead.worktree.to_str().unwrap(),
            ],
        );
        std::os::unix::fs::symlink(&task.worktree, &lead.worktree).unwrap();
        assert_eq!(core.lead_loss(&project.id), Ok(LeadLoss::default()));
        // A start refuses it and does not touch the target.
        let (sink, _rx) = channel_sink();
        assert_eq!(
            core.start_lead(
                &project.id,
                "claude",
                PtySize::new(30, 90),
                sink,
                lead_launch(&scratch)
            )
            .map(|_| ()),
            Err(Error::LeadWorktreeInvalid(lead.worktree.clone()))
        );
        core.forget_lead(&project.id, true).unwrap();
        assert!(task.worktree.join("work.txt").exists());
        assert!(core.saved_lead(&project.id).unwrap().is_none());
        // Even a stale journal entry for the link is dropped, not followed.
        core.journal
            .add(&JournalEntry {
                project_id: "gone".into(),
                branch: String::new(),
                path: lead.worktree.clone(),
                base_ref: None,
                started_by_lead: false,
            })
            .unwrap();
        core.leftover_remove(&lead.worktree).unwrap();
        assert!(task.worktree.join("work.txt").exists());
    }
}
