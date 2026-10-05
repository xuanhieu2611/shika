/// Every failure the app can show. `Display` is the sentence the user sees,
/// worded as the Tauri build worded it. Variants that carry a detail append
/// the first line git or the PTY layer printed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("Could not open app data.")]
    AppData,

    #[error("That folder does not exist.")]
    FolderMissing,
    #[error("Choose a folder.")]
    NotAFolder,
    #[error("That folder is not a git repository.")]
    NotARepository,
    #[error("That project is not in the list.")]
    UnknownProject,
    #[error("Could not read projects.")]
    ReadProjects,
    #[error("Could not save projects.")]
    SaveProjects,

    #[error("Could not read worktrees.")]
    ReadJournal,
    #[error("Could not save worktrees.")]
    SaveJournal,

    #[error("Could not read settings.")]
    ReadSettings,
    #[error("Could not save settings.")]
    SaveSettings,

    #[error("Could not run git.{}", detail(.0))]
    Git(Option<String>),
    #[error("Could not update the git exclude.")]
    Exclude,
    #[error("Could not create the worktree. That draft already exists.")]
    DraftExists,
    #[error("Could not create the worktree.{}", detail(.0))]
    CreateWorktree(Option<String>),
    /// The project's configured base branch is on neither origin nor this
    /// clone. New never falls back to another branch.
    #[error("Base branch {0} not found.")]
    BaseBranchMissing(String),
    /// A base branch typed in the app that does not exist.
    #[error("No branch named {0} on origin or locally.")]
    NoSuchBranch(String),
    #[error("Could not read git status.{}", detail(.0))]
    GitStatus(Option<String>),
    #[error("Could not remove the worktree.{}", detail(.0))]
    RemoveWorktree(Option<String>),
    /// `git worktree remove` refused because the tree has changes. Only a
    /// confirmed close may retry with `--force`.
    #[error("Could not remove the worktree.{}", detail(.0))]
    WorktreeHasChanges(Option<String>),

    #[error("Could not rename the branch.{}", detail(.0))]
    RenameBranch(Option<String>),
    #[error("Could not push changes.{}", detail(.0))]
    Push(Option<String>),
    #[error("This task has work to keep. Choose discard or push before closing.")]
    CloseNeedsConfirmation,
    #[error("Commit changes in the shell before pushing.")]
    PushDirty,
    #[error("There are no commits to push.")]
    NothingToPush,
    #[error("That leftover is not in the journal, or is still in use.")]
    UnknownLeftover,

    #[error("That CLI is not available.")]
    UnknownCli,
    #[error("{0} was not found.")]
    CliNotFound(String),
    #[error("That conversation is gone.")]
    UnknownSession,

    #[error("That terminal is gone.")]
    UnknownPty,
    #[error("Could not open the terminal.{}", detail(.0))]
    OpenPty(Option<String>),
    #[error("Could not write to the terminal.")]
    WritePty,
    #[error("Could not resize the terminal.")]
    ResizePty,
    #[error("Could not prepare the shell.")]
    PrepareShell,
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

fn detail(line: &Option<String>) -> String {
    match line {
        Some(line) => format!(" {line}"),
        None => String::new(),
    }
}

/// First non-empty line of a tool's stderr, for the detail of an error.
pub(crate) fn first_line(text: &[u8]) -> Option<String> {
    let line = String::from_utf8_lossy(text)
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("")
        .trim()
        .to_string();
    if line.is_empty() { None } else { Some(line) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn details_follow_the_sentence() {
        assert_eq!(Error::Git(None).to_string(), "Could not run git.");
        assert_eq!(
            Error::CreateWorktree(Some("fatal: bad".into())).to_string(),
            "Could not create the worktree. fatal: bad"
        );
        assert_eq!(
            Error::CliNotFound("Claude Code".into()).to_string(),
            "Claude Code was not found."
        );
        assert_eq!(
            Error::BaseBranchMissing("dev".into()).to_string(),
            "Base branch dev not found."
        );
        assert_eq!(
            Error::NoSuchBranch("dev".into()).to_string(),
            "No branch named dev on origin or locally."
        );
        assert_eq!(
            first_line(b"\n  \n  fatal: x  \nmore"),
            Some("fatal: x".into())
        );
        assert_eq!(first_line(b" \n"), None);
    }
}
