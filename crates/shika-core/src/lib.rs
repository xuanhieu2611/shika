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
//!   [`Core::create_session_with_preparation`], [`Core::create_checkout_session`],
//!   [`Core::checkout_head`], [`Core::session_stop_checkout`], [`Core::start_lead`],
//!   [`Core::forget_lead`], [`Core::discover_lead_session`], [`Core::set_lead_session`],
//!   [`Core::open_shell`], [`Core::session_dirty`], [`Core::session_git_state`],
//!   [`Core::session_diff_stat`], [`Core::session_diff`], [`Core::session_file_diff`],
//!   [`Core::session_publish_preview`], [`Core::session_publish`],
//!   [`Core::session_rename_from_prompt`], [`Core::session_apply_cli_title`],
//!   [`Core::session_switch_agent`], [`Core::token_usage`],
//!   [`Core::session_discard`],
//!   [`Core::session_push_and_close`], [`Core::session_close`], [`Core::leftover_remove`],
//!   [`Core::leftover_recovery`], [`Core::reopen_leftover`], [`Core::checkpoint_sessions`],
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
//!   [`Core::sessions`], [`Core::session`], [`Core::checkout_session`], [`Core::lead_for_project`],
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
mod recovery;
mod session;
mod settings;
mod usage;
mod worktree;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

pub use activity::{AgentActivity, AgentActivityState};
pub use agents::{CliCatalog, CliPreset, picker_presets, terminal_choice};
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
pub use session::{
    DiffStat, LaunchOptions, Session, SessionGitState, SessionKind, WorkerEnv, task_title,
};
pub use settings::{
    AgentSettings, Appearance, Background, BackgroundEffect, BackgroundFraming, Changes, Column,
    DEFAULT_DARK_THEME, DEFAULT_LIGHT_THEME, FontSize, KeyOverrides, Settings, ThemeMode,
    ThemeSettings, Translucency,
};
pub use usage::{
    UsageCalendar, UsageCalendarDay, UsageDay, UsageReport, UsageTotals, UsageWeek, format_tokens,
};
pub use worktree::normalize_prefix as normalize_branch_prefix;
pub use worktree::{CheckoutHead, JournalEntry, KnownBranches, LeadWorktree, RecoveryMetadata};

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
    if session.is_lead() {
        Err(Error::LeadUnsupported)
    } else {
        Ok(())
    }
}

/// For the operations only a task, with its own branch and worktree, has:
/// branch-switch close, discard, push, and publishing.
fn require_task(session: &Session) -> Result<()> {
    match session.kind {
        SessionKind::Task => Ok(()),
        SessionKind::Lead => Err(Error::LeadUnsupported),
        SessionKind::Checkout => Err(Error::CheckoutUnsupported),
    }
}

/// What a session's diff stat and Changes are measured against: a task's
/// start, or a checkout session's `HEAD`.
fn baseline(session: &Session) -> worktree::Baseline<'_> {
    if session.is_checkout() {
        worktree::Baseline::Head
    } else {
        worktree::Baseline::Task {
            repo: &session.repo,
            recorded: session.base_ref.as_deref(),
        }
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
        report: impl FnMut(PreparationEvent),
    ) -> Result<Session> {
        self.create_task(
            project_id,
            Some(preset_id),
            size,
            sink,
            options,
            control,
            report,
        )
    }

    /// Create an ordinary worktree task with a pinned login shell, no agent.
    /// Uses exactly the same consent, preparation, cancellation and cleanup fences.
    pub fn create_terminal_with_preparation(
        &self,
        project_id: &str,
        size: PtySize,
        sink: impl PtySink,
        control: PreparationControl,
        report: impl FnMut(PreparationEvent),
    ) -> Result<Session> {
        self.create_task(
            project_id,
            None,
            size,
            sink,
            LaunchOptions::default(),
            control,
            report,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn create_task(
        &self,
        project_id: &str,
        preset_id: Option<&str>,
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
        let (preset, program) = match preset_id {
            Some(id) => {
                let preset = agents::presets_from(env)
                    .into_iter()
                    .find(|preset| preset.id == id)
                    .ok_or(Error::UnknownCli)?;
                let program = preset
                    .path
                    .clone()
                    .ok_or_else(|| Error::CliNotFound(preset.name.clone()))?;
                (preset, program)
            }
            None => (agents::terminal_choice(), path_env::user_shell()),
        };
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
                recovery: None,
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
            let started = SystemTime::now();
            self.journal
                .set_recovery(&draft.path, RecoveryMetadata::new(&preset.id, started))?;
            let (pty, activity) = if preset_id.is_none() {
                (
                    self.ptys.open(
                        session::shell_request(&program, &draft.path, env.path(), size),
                        sink,
                    )?,
                    None,
                )
            } else {
                self.open_agent(
                    &preset,
                    program,
                    &draft.path,
                    size,
                    options.control.as_ref(),
                    true,
                    agents::launch_flags(&preset, &draft.path),
                    options.prompt.as_deref(),
                    sink,
                )?
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
                title: if preset_id.is_none() {
                    format!(
                        "Terminal {}",
                        draft.branch.trim_start_matches("shika-draft-")
                    )
                } else {
                    format!("New {}", preset.name)
                },
                manual_title: false,
                preset_name: preset.name,
                branch: draft.branch.clone(),
                repo: project.path.clone(),
                worktree: draft.path.clone(),
                base_ref: base.reference.clone(),
                pty: Some(pty),
                shell_ptys: Vec::new(),
                cli_titled: false,
                prompt_named: false,
                agent_started: started,
                kind: SessionKind::Task,
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

    /// Starts `preset_id` in the project's own folder, on whatever the author
    /// has checked out: no worktree, branch, base fetch, preparation, or
    /// journal entry, and nothing in its git is written. The CLI gets the
    /// usual environment and the preset's launch flags (Codex's trust
    /// override names the repository root) and no prompt, so the session
    /// waits for the author. It is never a Lead's worker and gets no control
    /// variables. One per project: [`Error::CheckoutExists`] while the
    /// project has one, checked under the operations lock. A folder that is
    /// no longer the top of its own git checkout is refused before anything
    /// runs. Blocking: runs git and spawns a process.
    pub fn create_checkout_session(
        &self,
        project_id: &str,
        preset_id: &str,
        size: PtySize,
        sink: impl PtySink,
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
        let _guard = self.operations.lock().unwrap_or_else(|e| e.into_inner());
        let project = self.projects.get(project_id)?;
        if self.checkout_session(project_id).is_some() {
            return Err(Error::CheckoutExists);
        }
        worktree::checkout_head(&self.git()?, env.path(), &project.path)?;
        let id = session::new_id(&self.taken_ids()?);
        let started = SystemTime::now();
        // Not a usage source: the folder also holds the author's own
        // conversations, which Shika must not count.
        let (pty, activity) = self.open_agent(
            &preset,
            program,
            &project.path,
            size,
            None,
            false,
            agents::launch_flags(&preset, &project.path),
            None,
            sink,
        )?;
        let session = Session {
            id,
            project_id: project.id.clone(),
            preset_id: preset.id,
            title: format!("New {}", preset.name),
            manual_title: false,
            preset_name: preset.name,
            branch: String::new(),
            repo: project.path.clone(),
            worktree: project.path.clone(),
            base_ref: None,
            pty: Some(pty),
            shell_ptys: Vec::new(),
            cli_titled: false,
            prompt_named: false,
            agent_started: started,
            kind: SessionKind::Checkout,
            started_by: None,
        };
        if let Some(activity) = activity {
            self.sessions.remember_activity(&session.id, activity);
        }
        self.sessions.insert(session.clone());
        Ok(session)
    }

    /// The project's live checkout session, if it has one. Quick.
    pub fn checkout_session(&self, project_id: &str) -> Option<Session> {
        self.sessions
            .all()
            .into_iter()
            .find(|session| session.is_checkout() && session.project_id == project_id)
    }

    /// What the project's checkout has checked out: its branch, or the short
    /// commit when detached. For the checkout session's card and the picker
    /// footer, whether or not a session runs. Blocking but local: one or two
    /// read-only git processes bounded to the repository root, no lock.
    pub fn checkout_head(&self, project_id: &str) -> Result<CheckoutHead> {
        let project = self.projects.get(project_id)?;
        worktree::checkout_head(&self.git()?, self.path_env().path(), &project.path)
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
    /// `SHIKA_SOCKET`, `SHIKA_TOKEN`, and the `shika` command on `PATH`.
    /// `track_usage` registers `worktree` as a token usage source; it is
    /// false only for a checkout session. On failure the bridge is removed
    /// and nothing runs. Blocking: spawns a process.
    #[allow(clippy::too_many_arguments)]
    fn open_agent(
        &self,
        preset: &CliPreset,
        program: PathBuf,
        worktree: &Path,
        size: PtySize,
        control: Option<&WorkerEnv>,
        track_usage: bool,
        args: Vec<String>,
        prompt: Option<&str>,
        sink: impl PtySink,
    ) -> Result<(PtyId, Option<activity::ActivityBridge>)> {
        let env = self.path_env();
        let usage = self
            .cli_home
            .as_ref()
            .filter(|_| track_usage)
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
        let started = SystemTime::now();
        let previous_entry = if session.is_checkout() {
            None
        } else {
            Some(
                self.journal
                    .list()?
                    .into_iter()
                    .find(|e| e.path == session.worktree)
                    .ok_or(Error::UnknownLeftover)?,
            )
        };
        if previous_entry.is_some() {
            // Persist before spawn. A failed switch restores the old hint below.
            let mut hint = RecoveryMetadata::new(&preset.id, started);
            hint.title = Some(session.title.chars().take(80).collect());
            self.journal.set_recovery(&session.worktree, hint)?;
        }
        let opened = self.open_agent(
            &preset,
            program,
            &session.worktree,
            size,
            control.as_ref(),
            !session.is_checkout(),
            agents::launch_flags(&preset, &session.worktree),
            prompt,
            sink,
        );
        let (pty, activity) = match opened {
            Ok(opened) => opened,
            Err(error) => {
                if let Some(entry) = &previous_entry {
                    self.journal.replace(entry)?;
                }
                return Err(error);
            }
        };
        let switched =
            self.sessions
                .retarget_agent(id, preset.id.clone(), preset.name.clone(), pty, started);
        let (session, old_pty) = match switched {
            Ok(pair) => pair,
            Err(err) => {
                self.ptys.close(pty);
                if let Some(activity) = &activity {
                    activity.cleanup();
                }
                if let Some(entry) = &previous_entry {
                    self.journal.replace(entry)?;
                }
                return Err(err);
            }
        };
        self.sessions.replace_activity(id, activity);
        if !session.shell_ptys.contains(&old_pty) {
            self.ptys.close(old_pty);
        }
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
    /// it. A checkout session takes only the name: the branch is the
    /// author's, and no git runs. Blocking.
    pub fn session_rename_from_prompt(&self, id: &str, prompt: &str) -> Result<Session> {
        let _guard = self
            .operations
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let session = self.sessions.get(id).ok_or(Error::UnknownSession)?;
        refuse_lead(&session)?;
        if session.is_terminal() || session.named() || session.cli_titled {
            return Ok(session);
        }
        if session.is_checkout() {
            let title: String = prompt
                .lines()
                .next()
                .unwrap_or("")
                .trim()
                .chars()
                .take(80)
                .collect();
            if title.is_empty() {
                return Ok(session);
            }
            return self.sessions.name_from_prompt(id, title);
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
    /// is on another branch; the card still takes the title. A checkout
    /// session takes only the title, and only from a conversation begun
    /// since its agent started, because the author's own conversations share
    /// its folder; no git runs for it. Blocking: reads the CLI's files and
    /// runs git.
    pub fn session_apply_cli_title(&self, id: &str) -> Result<Option<Session>> {
        let _guard = self
            .operations
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let session = self.sessions.get(id).ok_or(Error::UnknownSession)?;
        refuse_lead(&session)?;
        if session.is_terminal() || session.cli_titled {
            return Ok(None);
        }
        let since = session.is_checkout().then_some(session.agent_started);
        let Some(title) = self
            .cli_home
            .as_ref()
            .and_then(|home| home.read(&session.preset_id, &session.worktree, since))
        else {
            return Ok(None);
        };
        if session.is_checkout() {
            return self
                .sessions
                .apply_cli_title(id, session.branch.clone(), title)
                .map(Some);
        }
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
        if session.is_checkout() {
            // Close does not consult git for the checkout; nothing reads this.
            return Err(Error::CheckoutUnsupported);
        }
        if session.is_lead() {
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
        require_task(&session)?;
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
    /// against where the branch left the base it started from. A checkout
    /// session counts uncommitted work only, against `HEAD`, read within the
    /// repository root. Takes no lock, so a background refresh never holds
    /// up close or discard.
    pub fn session_diff_stat(&self, id: &str) -> Result<DiffStat> {
        let session = self.sessions.get(id).ok_or(Error::UnknownSession)?;
        if session.is_lead() {
            // A Lead is not measured against a base; it shows no stat.
            return Ok(DiffStat::default());
        }
        worktree::diff_stat(
            &self.git()?,
            self.path_env().path(),
            &session.worktree,
            baseline(&session),
        )
    }

    /// Starts the app's single background usage worker and restart recovery.
    /// No provider reads on this caller. Explicit so tests remain isolated.
    pub fn start_usage_collection(&self) {
        self.usage
            .get_or_init(|| usage::Collector::start(self.data_dir.clone()));
    }

    /// Coalesces a recovery request. No provider I/O or operations lock. A
    /// project's own folder (a checkout session's) is never a usage source:
    /// the author's conversations outside Shika live there too.
    pub fn record_token_usage(&self, preset_id: &str, worktree: &Path) {
        if self
            .projects
            .list()
            .is_ok_and(|projects| projects.iter().any(|project| project.path == worktree))
        {
            return;
        }
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

    /// Blocking. The session's whole change for the Changes panel: every file's
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
            &session.worktree,
            baseline(&session),
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
        require_task(&session)?;
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
    /// `body` is the PR description. None or only whitespace leaves it to
    /// `gh --fill`.
    pub fn session_publish(
        &self,
        preview: &PublishPreview,
        target: &str,
        title: &str,
        body: Option<&str>,
    ) -> Result<PublishedPr> {
        let _guard = self.operations.lock().unwrap_or_else(|e| e.into_inner());
        let session = self
            .sessions
            .get(&preview.session_id)
            .ok_or(Error::UnknownSession)?;
        require_task(&session)?;
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
            body,
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
        require_task(&session)?;
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
        require_task(&session)?;
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
        if session.kind != SessionKind::Task {
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
    /// and [`Core::forget_lead`]. A checkout session is refused: its tree is
    /// the author's own work. No commit is created. Blocking.
    pub fn session_discard(&self, id: &str) -> Result<()> {
        let _guard = self
            .operations
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let session = self.sessions.get(id).ok_or(Error::UnknownSession)?;
        require_task(&session)?;
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
        require_task(&self.sessions.get(id).ok_or(Error::UnknownSession)?)?;
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
    /// [`Core::forget_lead`]. A checkout session runs no git at all: idle, it
    /// stops its PTYs and goes; with `agent_working` it is
    /// CloseNeedsConfirmation, and the confirmed close is
    /// [`Core::session_stop_checkout`]. The app asks on
    /// CloseNeedsConfirmation. Blocking.
    pub fn session_close(&self, id: &str, agent_working: bool) -> Result<()> {
        let _guard = self
            .operations
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let session = self.sessions.get(id).ok_or(Error::UnknownSession)?;
        refuse_lead(&session)?;
        if session.is_checkout() {
            if agent_working {
                return Err(Error::CloseNeedsConfirmation);
            }
            self.stop_checkout(&session);
            return Ok(());
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

    /// The confirmed close of a checkout session, the author's answer to
    /// "Stop the agent?": stops every PTY of the session and drops it.
    /// Nothing in git is read or written, and there is no journal entry or
    /// worktree to remove. Anything but a checkout session is refused with
    /// [`Error::NotCheckout`]; tasks close through [`Core::session_close`].
    /// Blocking only on the operations lock.
    pub fn session_stop_checkout(&self, id: &str) -> Result<()> {
        let _guard = self
            .operations
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let session = self.sessions.get(id).ok_or(Error::UnknownSession)?;
        if !session.is_checkout() {
            return Err(Error::NotCheckout);
        }
        self.stop_checkout(&session);
        Ok(())
    }

    /// Under the operations lock. No `forget_session`: nothing is journaled,
    /// and the journal must never be asked about the repository root.
    fn stop_checkout(&self, session: &Session) {
        self.hang_up(session);
        self.sessions.remove(&session.id);
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
        if session.kind != SessionKind::Task {
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
mod tests;
