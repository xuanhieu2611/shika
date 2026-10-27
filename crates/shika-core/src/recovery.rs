//! Explicit recovery of an owned task worktree. Never reruns setup or git writes
//! that change files, refs, or the index. Provider history stays with the CLI.
use super::*;
use std::time::UNIX_EPOCH;

impl RecoveryMetadata {
    pub(crate) fn new(cli: &str, started: SystemTime) -> Self {
        Self {
            cli: cli.into(),
            started_ms: started
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
            conversation: None,
            title: None,
        }
    }
}

impl Core {
    fn recovery_entry(&self, path: &Path) -> Result<(JournalEntry, Project)> {
        let mut entry = self
            .leftovers_list()?
            .into_iter()
            .find(|e| e.path == path)
            .ok_or(Error::UnknownLeftover)?;
        if entry.branch.is_empty() {
            return Err(Error::RecoverWorktree(
                "Detached Lead worktrees cannot be reopened as tasks.".into(),
            ));
        }
        let parent = path
            .parent()
            .filter(|p| p.file_name().is_some_and(|n| n == ".worktrees"))
            .ok_or(Error::UnknownLeftover)?;
        let repo = parent.parent().ok_or(Error::UnknownLeftover)?;
        let project = self
            .projects
            .list()?
            .into_iter()
            .find(|p| p.path == repo)
            .ok_or_else(|| Error::RecoverWorktree("Add its original project first.".into()))?;
        let git = self.git()?;
        let env = self.path_env();
        if !worktree::is_verified_worktree(&git, env.path(), &project.path, path)? {
            return Err(Error::RecoverWorktree("The folder is missing or no longer its original Git worktree. Nothing was changed.".into()));
        }
        let head = worktree::head_branch(&git, env.path(), path)?.ok_or_else(|| {
            Error::RecoverWorktree(
                "The worktree has detached HEAD. Restore its task branch first.".into(),
            )
        })?;
        if head != entry.branch {
            if !worktree::was_renamed(&git, env.path(), path, &entry.branch, &head)? {
                return Err(Error::TaskBranchChanged {
                    expected: entry.branch,
                    current: head,
                });
            }
            entry.branch = head;
        }
        Ok((entry, project))
    }

    fn recovery_conversation(&self, entry: &JournalEntry) -> Option<String> {
        let hint = entry.recovery.as_ref()?;
        let home = self.cli_home.as_ref()?;
        let id = hint.conversation.clone().or_else(|| {
            home.task_conversation(
                &hint.cli,
                &entry.path,
                UNIX_EPOCH.checked_add(Duration::from_millis(hint.started_ms))?,
            )
        })?;
        (agents::valid_conversation_id(&id)
            && home.task_session_exists(&hint.cli, &id, &entry.path))
        .then_some(id)
    }

    /// Read-only recovery preview. Blocking: local Git and provider metadata.
    /// An absent conversation is an explicit fresh-agent fallback, not a picker
    /// for the provider's most recent conversation in some other directory.
    pub fn leftover_recovery(&self, path: &Path) -> Result<JournalEntry> {
        let (mut entry, _) = self.recovery_entry(path)?;
        let conversation = self.recovery_conversation(&entry);
        if let Some(hint) = &mut entry.recovery {
            hint.conversation = conversation;
        }
        Ok(entry)
    }

    /// Reopen one leftover as an ordinary author-owned card. `resume` requires
    /// an exact available conversation for the chosen CLI; false starts fresh.
    /// Keeps branch, base, index and files. No fetch, setup, naming or Lead env.
    /// Rechecks ownership under the operations lock. Blocking.
    pub fn reopen_leftover(
        &self,
        path: &Path,
        preset_id: &str,
        resume: bool,
        size: PtySize,
        sink: impl PtySink,
    ) -> Result<Session> {
        let (preview, _) = self.recovery_entry(path)?;
        let conversation = if resume {
            Some(
                preview
                    .recovery
                    .as_ref()
                    .filter(|r| r.cli == preset_id)
                    .and_then(|_| self.recovery_conversation(&preview))
                    .ok_or_else(|| {
                        Error::RecoverWorktree(
                            "The conversation is unavailable. Start a fresh agent instead.".into(),
                        )
                    })?,
            )
        } else {
            None
        };
        let env = self.path_env();
        let terminal = preset_id == "terminal";
        let (preset, program) = if terminal {
            (agents::terminal_choice(), crate::path_env::user_shell())
        } else {
            let preset = agents::presets_from(env)
                .into_iter()
                .find(|p| p.id == preset_id)
                .ok_or(Error::UnknownCli)?;
            let program = preset
                .path
                .clone()
                .ok_or_else(|| Error::CliNotFound(preset.name.clone()))?;
            (preset, program)
        };
        let _guard = self.operations.lock().unwrap_or_else(|e| e.into_inner());
        let (entry, project) = self.recovery_entry(path)?;
        if entry != preview {
            return Err(Error::RecoverWorktree(
                "Recovery information changed. Open it again.".into(),
            ));
        }
        let id = session::new_id(&self.taken_ids()?);
        let started = SystemTime::now();
        let title = entry
            .recovery
            .as_ref()
            .and_then(|r| {
                r.title
                    .as_deref()
                    .and_then(|title| session::task_title(title).ok())
            })
            .unwrap_or_else(|| entry.branch.clone());
        let mut hint = RecoveryMetadata::new(preset_id, started);
        hint.conversation = conversation.clone();
        hint.title = Some(title.clone());
        // Journal before spawn, but retain the old hint if spawning fails.
        let mut updated = entry.clone();
        updated.project_id = project.id.clone();
        updated.started_by_lead = false;
        updated.recovery = Some(hint);
        self.journal.replace(&updated)?;
        let args = match &conversation {
            Some(id) => agents::task_resume_flags(&preset, path, id),
            None => agents::launch_flags(&preset, path),
        };
        let opened = if terminal {
            self.ptys
                .open(
                    session::shell_request(&program, path, env.path(), size),
                    sink,
                )
                .map(|pty| (pty, None))
        } else {
            self.open_agent(&preset, program, path, size, None, true, args, None, sink)
        };
        let (pty, activity) = match opened {
            Ok(opened) => opened,
            Err(err) => {
                // Restore the whole entry, including legacy absence of metadata.
                self.journal.replace(&entry)?;
                return Err(err);
            }
        };
        let session = Session {
            id,
            project_id: project.id,
            preset_id: preset.id,
            preset_name: preset.name,
            title,
            manual_title: true,
            branch: entry.branch,
            repo: project.path,
            worktree: path.into(),
            base_ref: entry.base_ref,
            pty: Some(pty),
            shell_ptys: Vec::new(),
            cli_titled: true,
            prompt_named: true,
            agent_started: started,
            kind: SessionKind::Task,
            started_by: None,
        };
        if let Some(activity) = activity {
            self.sessions.remember_activity(&session.id, activity);
        }
        self.sessions.insert(session.clone());
        Ok(session)
    }

    /// Best-effort checkpoint before an explicit quit. No transcript reads or
    /// terminal buffers, and no ongoing worker/poller. Blocking, off the UI.
    pub fn checkpoint_sessions(&self) -> Result<()> {
        for session in self
            .sessions
            .all()
            .into_iter()
            .filter(|s| s.kind == SessionKind::Task)
        {
            let Some(entry) = self
                .journal
                .list()?
                .into_iter()
                .find(|e| e.path == session.worktree)
            else {
                continue;
            };
            let mut hint = entry.recovery.clone().unwrap_or_else(|| {
                RecoveryMetadata::new(&session.preset_id, session.agent_started)
            });
            hint.conversation = self.recovery_conversation(&entry);
            hint.title = Some(session.title.chars().take(80).collect());
            let _guard = self.operations.lock().unwrap_or_else(|e| e.into_inner());
            if self
                .sessions
                .get(&session.id)
                .is_some_and(|live| live.pty == session.pty)
            {
                self.journal.set_recovery(&session.worktree, hint)?;
            }
        }
        Ok(())
    }
}
