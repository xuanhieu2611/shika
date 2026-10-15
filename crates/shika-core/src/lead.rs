//! A project's Lead: the agent CLI the author talks to, which starts workers.
//!
//! A Lead outlives the app. Every project has one fixed worktree for it,
//! `<repo>/.worktrees/shika-lead`, and a saved record in `projects.json`
//! (`lead`: the CLI preset and the CLI's own conversation id). The next
//! [`Core::start_lead`] moves that worktree to the latest base when it is clean
//! and resumes the conversation when the CLI still has it. Shika stores no
//! transcript; the CLI does.
//!
//! The Lead instructions travel in the CLI's system prompt on every start
//! (`agents::lead_flags`), so a resumed Lead reads the current text, and the
//! app does not type anything into a fresh one. Cursor has no such flag and
//! gets a positional bootstrap prompt on a fresh start only.
//!
//! The project's memory notes (`memory.rs`) are appended to the instructions on
//! every start, so a fresh conversation begins with what the last one learned.
//!
//! The decisions, the per-CLI facts, and the traps: `docs/lead-agent.md`.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::agents::{self, CliPreset, Conversation};
use crate::control::random_bytes;
use crate::error::{Error, Result};
use crate::memory::{self, MemoryNote};
use crate::projects::{Project, SavedLead};
use crate::pty::{CONTROL_VARS, PtySink, PtySize};
use crate::session::{self, Session, WorkerEnv};
use crate::worktree::{self, GIT_REDIRECTS, JournalEntry, LeadWorktree};
use crate::{Core, PreparationControl};

/// How long Cursor's `create-chat` may take before the Lead starts without a
/// conversation id.
const CREATE_CHAT_TIMEOUT: Duration = Duration::from_secs(10);

/// What a Lead is started with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeadLaunch {
    /// Exported to the Lead as `SHIKA_SOCKET`.
    pub socket: PathBuf,
    /// Exported to the Lead as `SHIKA_TOKEN`.
    pub token: String,
    /// Prepended to the Lead's `PATH`; holds the `shika` command.
    pub bin_dir: PathBuf,
    /// The Lead's instructions. Passed on every start as the CLI's system
    /// prompt, so keep the text stable: it must not hold temporary paths or
    /// tokens. Refused when empty or starting with `-`, like a launch prompt.
    pub instructions: String,
    /// Cursor CLI has no system prompt flag. On a fresh Cursor start this is
    /// its one positional prompt. Ignored for every other CLI and when
    /// resuming. Validated like [`LaunchOptions::prompt`](crate::LaunchOptions).
    pub cursor_bootstrap: Option<String>,
}

/// Whether the Lead continued its conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeadConversation {
    /// The CLI was started on the saved conversation id.
    Resumed,
    /// A new conversation, and why.
    Fresh(FreshReason),
}

/// Why a Lead started a new conversation instead of resuming one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FreshReason {
    /// Nothing was saved: a first Lead, a forgotten one, or a saved record
    /// without an id (Codex before its first message, Cursor without a chat).
    NoSavedSession,
    /// The saved id is no longer stored by its CLI.
    SessionMissing,
    /// The saved conversation belongs to another CLI than the one chosen now.
    CliChanged,
}

/// What Forget would throw away from a Lead's worktree.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LeadLoss {
    /// Uncommitted changes.
    pub uncommitted: bool,
    /// Commits that are on no branch, tag, or remote-tracking ref.
    pub unreferenced_commits: bool,
}

impl LeadLoss {
    /// Whether anything would be lost.
    pub fn any(self) -> bool {
        self.uncommitted || self.unreferenced_commits
    }
}

/// A worker a Lead started whose card did not survive: its worktree is still
/// journaled but no live session owns it (the app quit). The tree is a
/// leftover the author can clean up, or a branch the Lead can look at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LostWorker {
    pub branch: String,
    pub path: PathBuf,
}

/// The result of [`Core::start_lead`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeadStart {
    pub session: Session,
    pub conversation: LeadConversation,
    pub worktree: LeadWorktree,
    /// Workers a Lead started that have no live session, in journal order,
    /// except those an earlier restart summary already named (see
    /// [`Core::mark_workers_reported`]).
    pub lost_workers: Vec<LostWorker>,
    /// Cursor's bootstrap prompt was passed as the CLI's positional argument:
    /// a fresh Cursor start. The app counts it as the first submission; no
    /// other Lead start has a turn until the author types.
    pub bootstrap_sent: bool,
    /// Taken just before the CLI was spawned. Pass it to
    /// [`Core::discover_lead_session`].
    pub started_at: SystemTime,
}

impl Core {
    /// Starts a project's Lead in its fixed worktree
    /// (`<repo>/.worktrees/shika-lead`, detached, no branch, no preparation).
    ///
    /// - **Worktree.** Missing: created from the base New would use (fetched
    ///   first, best effort). Present, registered, clean, and with no commits
    ///   that are on no ref: moved to the latest base. Dirty, or holding such
    ///   commits: left untouched. Present but not a real directory that is a
    ///   worktree of this repository (a symbolic link included):
    ///   [`Error::LeadWorktreeInvalid`], and nothing is deleted. [`LeadStart::worktree`] says which.
    /// - **Conversation.** The project's saved Lead (`projects.json`) is
    ///   resumed when it names the same CLI, has an id, and the CLI still
    ///   stores that conversation; otherwise a new one starts and
    ///   [`LeadStart::conversation`] says why. The record is saved before this
    ///   returns, with the id Shika chose (Claude Code, Pi, Cursor) or without
    ///   one (Codex; see [`Core::discover_lead_session`]).
    /// - **Arguments.** The preset's flags, the instructions (with the
    ///   project's memory notes appended, if it has any and the file reads) as
    ///   the system prompt on every start, and no positional prompt, except Cursor's
    ///   bootstrap on a fresh start. See `agents::lead_flags`.
    /// - **Environment.** `PATH` starts with `bin_dir`, plus `SHIKA_SOCKET` and
    ///   `SHIKA_TOKEN`. Every other PTY has those removed.
    ///
    /// The session has `lead: true`, the title `Lead`, and an empty `branch`.
    /// The app must not rename, publish, diff-stat, or open shells for it; the
    /// Core methods that would refuse it with [`Error::LeadUnsupported`]. A
    /// second live Lead for a project is [`Error::LeadExists`]. A failed start
    /// removes a worktree it created and leaves an existing one alone.
    /// Blocking: runs git, may run `agent create-chat` (10 seconds at most),
    /// and spawns a process.
    pub fn start_lead(
        &self,
        project_id: &str,
        preset_id: &str,
        size: PtySize,
        sink: impl PtySink,
        launch: LeadLaunch,
    ) -> Result<LeadStart> {
        session::check_prompt(&launch.instructions)?;
        if let Some(bootstrap) = &launch.cursor_bootstrap {
            session::check_prompt(bootstrap)?;
        }
        let notes = self.memory.list(project_id).unwrap_or_default();
        let instructions = memory::instructions_with_notes(&launch.instructions, &notes);
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
        let project = self.projects.get(project_id)?;
        self.freshen_base(&project);
        // A process for Cursor, so before the lock like the fetch above.
        let (conversation, how) = self.choose_conversation(&project, &preset, &program);

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
        let started_at = SystemTime::now();
        let tree = worktree::prepare_lead_worktree(&git, env.path(), &project.path, base.start())?;
        let created = tree == LeadWorktree::Created;
        let path = worktree::lead_worktree_path(&project.path);
        // An entry left by an earlier incarnation of this project (removed
        // and added again) is claimed by the current one, or Leftovers would
        // offer the tree.
        let journaled = self.journal.list().map(|entries| {
            entries
                .iter()
                .find(|entry| entry.path == path)
                .map(|entry| entry.project_id.clone())
        });
        let journal = match journaled {
            Ok(Some(owner)) if owner == project.id => Ok(()),
            Ok(Some(_)) => self.journal.set_project(&path, &project.id),
            Ok(None) => self.journal.add(&JournalEntry {
                project_id: project.id.clone(),
                branch: String::new(),
                path: path.clone(),
                base_ref: base.reference.clone(),
                started_by_lead: false,
            }),
            Err(err) => Err(err),
        };
        if let Err(err) = journal {
            if created {
                let _ = worktree::remove_worktree(&git, env.path(), &project.path, &path, true);
            }
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
        let mut bootstrap_sent = false;
        let launched = (|| {
            let control = WorkerEnv {
                socket: launch.socket.clone(),
                token: launch.token.clone(),
                bin_dir: launch.bin_dir.clone(),
            };
            let args = agents::lead_flags(&preset, &path, &instructions, &conversation);
            let bootstrap = match &conversation {
                Conversation::Fresh { .. } if preset.id == "cursor" => {
                    launch.cursor_bootstrap.as_deref()
                }
                _ => None,
            };
            bootstrap_sent = bootstrap.is_some();
            let (pty, activity) = self.open_agent(
                &preset,
                program,
                &path,
                size,
                Some(&control),
                args,
                bootstrap,
                sink,
            )?;
            let saved = SavedLead {
                cli: preset.id.clone(),
                session_id: match &conversation {
                    Conversation::Fresh { id } => id.clone(),
                    Conversation::Resume { id } => Some(id.clone()),
                },
                reported_workers: project
                    .lead
                    .as_ref()
                    .map(|lead| {
                        let mut kept = lead.reported_workers.clone();
                        kept.retain(|path| path.exists());
                        kept
                    })
                    .unwrap_or_default(),
            };
            if let Err(err) = self.projects.set_lead(&project.id, Some(saved)) {
                self.ptys.close(pty);
                if let Some(activity) = &activity {
                    activity.cleanup();
                }
                return Err(err);
            }
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
        let session = match launched {
            Ok(session) => session,
            Err(err) => {
                // Nothing ran in a tree made for this start, so it is safe to
                // remove outright. An older tree may hold the author's work.
                if created
                    && worktree::remove_worktree(&git, env.path(), &project.path, &path, true)
                        .is_ok()
                {
                    self.journal.remove_path(&path)?;
                }
                return Err(err);
            }
        };
        let reported = self
            .projects
            .get(&project.id)
            .ok()
            .and_then(|project| project.lead)
            .map(|lead| lead.reported_workers)
            .unwrap_or_default();
        let lost_workers = self
            .lost_workers(&project.id)
            .unwrap_or_default()
            .into_iter()
            .filter(|worker| !reported.contains(&worker.path))
            .collect();
        Ok(LeadStart {
            session,
            conversation: how,
            worktree: tree,
            lost_workers,
            bootstrap_sent,
            started_at,
        })
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

    /// Stops a live Lead and keeps everything else: its fixed worktree, the
    /// journal entry, and the saved record, so the next [`Core::start_lead`]
    /// resumes it. The worktree is not checked; the Lead never codes, and a
    /// dirty tree is reported by the next start. Its workers stay live.
    /// [`Error::UnknownSession`] for an id that is not a live Lead. Blocking:
    /// waits for the operations lock.
    pub fn stop_lead(&self, lead_id: &str) -> Result<()> {
        let _guard = self.operations.lock().unwrap_or_else(|e| e.into_inner());
        let lead = self
            .sessions
            .get(lead_id)
            .filter(|session| session.lead)
            .ok_or(Error::UnknownSession)?;
        self.hang_up(&lead);
        self.sessions.remove(lead_id);
        Ok(())
    }

    /// The project's Lead memory notes in order. A missing file is no notes;
    /// one that does not parse is [`Error::MemoryUnreadable`]. Quick.
    pub fn lead_memory(&self, project_id: &str) -> Result<Vec<MemoryNote>> {
        self.memory.list(project_id)
    }

    /// Appends a note and returns its 1-based number. [`Error::MemoryRefused`]
    /// for an empty or long note or a full memory,
    /// [`Error::MemoryUnreadable`] for a corrupt file, which is left as it is.
    /// Quick.
    pub fn remember(&self, project_id: &str, text: &str) -> Result<usize> {
        self.projects.get(project_id)?;
        self.memory.add(project_id, text)
    }

    /// Removes note `number` (1-based); later notes move up.
    /// [`Error::MemoryRefused`] for a number there is no note for. Quick.
    pub fn forget_note(&self, project_id: &str, number: usize) -> Result<()> {
        self.memory.remove(project_id, number)
    }

    /// Where the project's memory file is (it may not exist yet). Quick.
    pub fn memory_path(&self, project_id: &str) -> Result<PathBuf> {
        self.memory.path(project_id)
    }

    /// The project's saved Lead record, if any. Quick.
    pub fn saved_lead(&self, project_id: &str) -> Result<Option<SavedLead>> {
        Ok(self.projects.get(project_id)?.lead)
    }

    /// Sets or clears the saved Lead's conversation id. [`Error::NoSavedLead`]
    /// without a saved Lead. Quick.
    pub fn set_lead_session(&self, project_id: &str, session_id: Option<&str>) -> Result<()> {
        if let Some(id) = session_id
            && !agents::valid_conversation_id(id)
        {
            return Err(Error::InvalidPrompt(
                "That conversation id is not valid.".into(),
            ));
        }
        self.projects
            .set_lead_session(project_id, session_id.map(str::to_string))
    }

    /// Finds the conversation id of a Lead whose CLI chooses its own (Codex)
    /// and saves it. Returns the saved id at once if there is one, `None` for
    /// a CLI whose id Shika chose or cannot find, and `None` until Codex has
    /// written the conversation: Codex creates it when the first message is
    /// submitted, not at launch, so poll this (every few seconds, off the UI
    /// thread) until it returns an id or the Lead closes. `since` is
    /// [`LeadStart::started_at`]: only a conversation in the Lead's worktree
    /// that began at or after it counts, and the newest wins. Blocking:
    /// reads Codex's database.
    pub fn discover_lead_session(
        &self,
        project_id: &str,
        since: SystemTime,
    ) -> Result<Option<String>> {
        let project = self.projects.get(project_id)?;
        let saved = project.lead.ok_or(Error::NoSavedLead)?;
        if saved.session_id.is_some() {
            return Ok(saved.session_id);
        }
        let Some(home) = self.cli_home.as_ref().filter(|_| saved.cli == "codex") else {
            return Ok(None);
        };
        let since_ms = since
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_millis() as i64);
        let worktree = worktree::lead_worktree_path(&project.path);
        let Some(id) = home
            .codex_session_since(&worktree, since_ms)
            .filter(|id| agents::valid_conversation_id(id))
        else {
            return Ok(None);
        };
        self.projects
            .set_lead_session(project_id, Some(id.clone()))?;
        Ok(Some(id))
    }

    /// Forgets the project's Lead: removes its fixed worktree and journal
    /// entry, then the saved record. The next Lead starts a new conversation.
    /// [`Error::LeadRunning`] while it is live (close it first). A tree with
    /// uncommitted changes or commits that are on no ref is
    /// [`Error::WorktreeHasChanges`] unless `discard` says the author agreed to
    /// lose them. Anything at the fixed path that is not a real directory
    /// registered as a worktree of this repository (a symbolic link, a plain
    /// folder, another repository's tree) is never touched; the record and
    /// journal entry still go. Blocking: runs git.
    pub fn forget_lead(&self, project_id: &str, discard: bool) -> Result<()> {
        let _guard = self.operations.lock().unwrap_or_else(|e| e.into_inner());
        if self.lead_for_project(project_id).is_some() {
            return Err(Error::LeadRunning);
        }
        let project = self.projects.get(project_id)?;
        let git = self.git()?;
        let env = self.path_env().path();
        let path = worktree::lead_worktree_path(&project.path);
        if lead_tree_present(&git, env, &project.path, &path)? {
            if !discard && lead_tree_loss(&git, env, &path)?.any() {
                return Err(Error::WorktreeHasChanges(None));
            }
            worktree::remove_worktree(&git, env, &project.path, &path, discard)?;
        }
        self.journal.remove_path(&path)?;
        self.projects.set_lead(project_id, None)
    }

    /// Workers a Lead started in this project whose worktree is journaled but
    /// has no live session, in journal order. Quick.
    pub fn lost_workers(&self, project_id: &str) -> Result<Vec<LostWorker>> {
        let live = self.sessions.all();
        let preparing = self.preparing.lock().unwrap_or_else(|e| e.into_inner());
        Ok(self
            .journal
            .list()?
            .into_iter()
            .filter(|entry| {
                entry.project_id == project_id
                    && entry.started_by_lead
                    && !live.iter().any(|session| session.worktree == entry.path)
                    && !preparing.contains_key(&entry.path)
            })
            .map(|entry| LostWorker {
                branch: entry.branch,
                path: entry.path,
            })
            .collect())
    }

    /// Records that the restart summary named these workers, so the next
    /// start does not repeat them. Entries whose worktree is gone are dropped.
    /// [`Error::NoSavedLead`] without a saved Lead. Quick.
    pub fn mark_workers_reported(&self, project_id: &str, paths: &[PathBuf]) -> Result<()> {
        self.projects.mark_workers_reported(project_id, paths)
    }

    /// Whether Forget would discard anything from the project's fixed Lead
    /// worktree: see [`Core::lead_loss`]. False when there is no tree.
    /// Blocking: runs git.
    pub fn lead_has_changes(&self, project_id: &str) -> Result<bool> {
        Ok(self.lead_loss(project_id)?.any())
    }

    /// What Forget would discard from the project's fixed Lead worktree:
    /// uncommitted changes and commits on no ref. Nothing when there is no
    /// tree, or when the fixed path is not a real directory registered as a
    /// worktree of this repository (such a path is never touched).
    /// Blocking: runs git.
    pub fn lead_loss(&self, project_id: &str) -> Result<LeadLoss> {
        let project = self.projects.get(project_id)?;
        let path = worktree::lead_worktree_path(&project.path);
        let git = self.git()?;
        let env = self.path_env().path();
        if !lead_tree_present(&git, env, &project.path, &path)? {
            return Ok(LeadLoss::default());
        }
        lead_tree_loss(&git, env, &path)
    }

    /// Whether the CLI still stores the saved Lead's conversation: `Some`
    /// when Shika can tell (Claude Code, Codex, Pi), `None` for a CLI it
    /// cannot read (Cursor), without a saved conversation id, or without a
    /// home directory. [`Error::NoSavedLead`] without a saved Lead.
    /// Blocking: reads the CLI's files.
    pub fn saved_lead_session_exists(&self, project_id: &str) -> Result<Option<bool>> {
        let project = self.projects.get(project_id)?;
        let saved = project.lead.ok_or(Error::NoSavedLead)?;
        let Some(id) = saved
            .session_id
            .as_deref()
            .filter(|id| agents::valid_conversation_id(id))
        else {
            return Ok(None);
        };
        let cwd = worktree::lead_worktree_path(&project.path);
        Ok(self
            .cli_home
            .as_ref()
            .and_then(|home| home.session_exists(&saved.cli, id, &cwd)))
    }

    /// Whether this journal entry is the fixed worktree of a project that
    /// still has a saved Lead. Leftovers never offers it: Forget does.
    pub(crate) fn is_saved_lead_worktree(&self, entry: &JournalEntry) -> bool {
        entry.branch.is_empty()
            && self.projects.list().is_ok_and(|projects| {
                projects.iter().any(|project| {
                    project.id == entry.project_id
                        && project.lead.is_some()
                        && worktree::lead_worktree_path(&project.path) == entry.path
                })
            })
    }

    /// Picks the conversation this start uses and why. Called before the
    /// operations lock: Cursor's new chat is a process.
    fn choose_conversation(
        &self,
        project: &Project,
        preset: &CliPreset,
        program: &Path,
    ) -> (Conversation, LeadConversation) {
        let fresh = |reason| {
            (
                Conversation::Fresh {
                    id: self.new_conversation_id(preset, program, &project.path),
                },
                LeadConversation::Fresh(reason),
            )
        };
        let Some(saved) = &project.lead else {
            return fresh(FreshReason::NoSavedSession);
        };
        if saved.cli != preset.id {
            return fresh(FreshReason::CliChanged);
        }
        let Some(id) = saved
            .session_id
            .as_deref()
            .filter(|id| agents::valid_conversation_id(id))
        else {
            return fresh(FreshReason::NoSavedSession);
        };
        // A CLI that keeps nothing Shika can read (Cursor), or no home
        // directory, is assumed to still have it.
        let cwd = worktree::lead_worktree_path(&project.path);
        let exists = self
            .cli_home
            .as_ref()
            .and_then(|home| home.session_exists(&preset.id, id, &cwd))
            .unwrap_or(true);
        if exists {
            (
                Conversation::Resume { id: id.to_string() },
                LeadConversation::Resumed,
            )
        } else {
            fresh(FreshReason::SessionMissing)
        }
    }

    /// The id a fresh conversation is started with: one Shika generates for
    /// Claude Code and Pi, one from `agent create-chat` for Cursor, none for
    /// Codex, whose id is found after launch.
    fn new_conversation_id(
        &self,
        preset: &CliPreset,
        program: &Path,
        repo: &Path,
    ) -> Option<String> {
        match preset.id.as_str() {
            "claude" | "pi" => Some(new_uuid()),
            "cursor" => create_chat(program, self.path_env().path(), repo),
            _ => None,
        }
    }
}

/// Whether the fixed Lead path is the Lead's worktree: a real directory (not a
/// symbolic link) that `git worktree list` names. Anything else is not the
/// Lead's and is never read, refreshed, or removed.
fn lead_tree_present(git: &Path, path_env: &str, repo: &Path, path: &Path) -> Result<bool> {
    Ok(worktree::is_real_dir(path) && worktree::is_registered_worktree(git, path_env, repo, path)?)
}

fn lead_tree_loss(git: &Path, path_env: &str, path: &Path) -> Result<LeadLoss> {
    Ok(LeadLoss {
        uncommitted: worktree::is_dirty(git, path_env, path)?,
        unreferenced_commits: worktree::has_unreferenced_commits(git, path_env, path)?,
    })
}

/// A random (version 4) UUID in lowercase hex, as Claude Code's `--session-id`
/// requires. Failing to read randomness panics, like [`crate::control::new_token`].
fn new_uuid() -> String {
    let mut bytes = random_bytes::<16>().expect("could not read /dev/urandom");
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    )
}

/// `agent create-chat`: a new empty Cursor chat, whose id it prints. The same
/// environment other children get (login-shell `PATH`, no git redirects, no
/// control variables), a 10 second limit, and None on any failure, in which
/// case the Lead starts without an id. Blocking.
fn create_chat(program: &Path, path_env: &str, cwd: &Path) -> Option<String> {
    create_chat_within(program, path_env, cwd, CREATE_CHAT_TIMEOUT)
}

fn create_chat_within(
    program: &Path,
    path_env: &str,
    cwd: &Path,
    timeout: Duration,
) -> Option<String> {
    let mut cmd = Command::new(program);
    cmd.arg("create-chat")
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if !path_env.is_empty() {
        cmd.env("PATH", path_env);
    }
    for key in GIT_REDIRECTS.iter().chain(CONTROL_VARS.iter()) {
        cmd.env_remove(key);
    }
    let mut child = cmd.spawn().ok()?;
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    if !status.success() {
        return None;
    }
    let mut out = String::new();
    std::io::Read::read_to_string(&mut child.stdout.take()?, &mut out).ok()?;
    let id = out.trim().to_string();
    agents::valid_conversation_id(&id).then_some(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// A directory reserved with `create_dir`, never adopted, removed on drop.
    struct Dir(PathBuf);

    impl Dir {
        fn new() -> Self {
            loop {
                let path = std::env::temp_dir().join(format!(
                    "shika-lead-test-{}-{}",
                    std::process::id(),
                    crate::control::new_token()
                ));
                match std::fs::create_dir(&path) {
                    Ok(()) => return Self(path),
                    Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
                    Err(err) => panic!("could not reserve a directory: {err}"),
                }
            }
        }

        fn script(&self, body: &str) -> PathBuf {
            let path = self.0.join("agent");
            std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            path
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn uuids_are_version_4_and_differ() {
        let (a, b) = (new_uuid(), new_uuid());
        assert_ne!(a, b);
        let parts: Vec<_> = a.split('-').map(str::len).collect();
        assert_eq!(parts, [8, 4, 4, 4, 12]);
        assert!(a.chars().all(|c| c == '-' || c.is_ascii_hexdigit()));
        assert!(!a.chars().any(|c| c.is_ascii_uppercase()));
        assert_eq!(a.as_bytes()[14], b'4', "{a}");
        assert!(matches!(a.as_bytes()[19], b'8' | b'9' | b'a' | b'b'), "{a}");
        assert!(agents::valid_conversation_id(&a));
    }

    #[test]
    fn create_chat_reads_one_id_given_the_login_path_and_the_repo_as_cwd() {
        let dir = Dir::new();
        let cwd = dir.0.canonicalize().unwrap();
        // Prints an id only when it got the subcommand, the PATH it was
        // handed, and the directory it was asked to run in.
        let program = dir.script(&format!(
            "[ \"$1\" = create-chat ] && [ \"$PATH\" = /login/path ] \
             && [ \"$(pwd -P)\" = '{}' ] && echo '  5c0ffee0-0000-4000-8000-000000000001  '",
            cwd.display()
        ));
        assert_eq!(
            create_chat_within(&program, "/login/path", &cwd, Duration::from_secs(5)).as_deref(),
            Some("5c0ffee0-0000-4000-8000-000000000001")
        );
        assert_eq!(
            create_chat_within(&program, "/other/path", &cwd, Duration::from_secs(5)),
            None
        );
    }

    #[test]
    fn create_chat_gives_up_on_failure_garbage_and_silence() {
        let cwd = std::env::temp_dir();
        let wait = Duration::from_secs(5);
        for (body, why) in [
            ("exit 1", "failure"),
            ("echo 5c0ffee0; exit 1", "failure with output"),
            ("exit 0", "no output"),
            ("echo --resume", "a flag"),
            ("echo 'two words'", "garbage"),
        ] {
            let dir = Dir::new();
            assert_eq!(
                create_chat_within(&dir.script(body), "", &cwd, wait),
                None,
                "{why}"
            );
        }
        assert_eq!(
            create_chat_within(Path::new("/no/such/agent"), "", &cwd, wait),
            None
        );
        let dir = Dir::new();
        let started = Instant::now();
        assert_eq!(
            create_chat_within(
                &dir.script("sleep 30"),
                "",
                &cwd,
                Duration::from_millis(200)
            ),
            None
        );
        assert!(started.elapsed() < Duration::from_secs(5));
    }
}
