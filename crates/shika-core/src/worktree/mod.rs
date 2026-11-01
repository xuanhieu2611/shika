//! Everything that runs git for a task: creating drafts, the fixed Lead
//! worktree, base resolution and fetch, the journal, dirty and unpushed
//! checks, branch reads and renames, diffs, and removal. Each child holds one
//! of those jobs; this module re-exports them so callers name
//! `worktree::<item>` as before.

mod base;
mod branch;
mod close;
mod fetch;
mod git;
mod identity;
mod journal;
mod lead;
mod lifecycle;
mod task_diff;

pub use base::*;
pub use branch::*;
pub use close::*;
pub use fetch::*;
pub(crate) use git::*;
pub use identity::*;
pub use journal::*;
pub use lead::*;
pub use lifecycle::*;
pub use task_diff::*;

#[cfg(test)]
mod tests;
