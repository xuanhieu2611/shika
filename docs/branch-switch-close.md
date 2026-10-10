# Closing a task after a branch switch

Contributor guide for safe-close recovery. [tasks-and-worktrees.md](tasks-and-worktrees.md) has the ordinary Close rules; [branch-naming.md](branch-naming.md) explains task branch identity; [keyboard-flow.md](keyboard-flow.md) and [terminal-tabs.md](terminal-tabs.md) cover focus and PTY ownership. Appearance follows [design/DESIGN.md](../design/DESIGN.md).

## Why this exists

An agent asked to improve a branch name may run `git switch -c better-name` instead of `git branch -m better-name`. These are different operations:

- Rename: one task branch changes its name. Shika can adopt it with explicit reflog proof.
- Switch: the original branch remains, and the worktree checks out another branch. Shika must not assume ownership of that branch.

Previously, any switch blocked Close and required returning to the task branch, even after the new branch was pushed and its PR merged. The new recovery flow allows explicit closure without transferring branch ownership or deleting either branch.

## User experience

Close task is Cmd+Shift+W or the Close task button. Plain `c` does nothing. A Lead's `shika close <task>` can open this confirmation for a worker it started; the command blocks until the author confirms or cancels ([shika-cli.md](shika-cli.md#shika-close-task)).

Normal Close first checks branch identity as before. When it detects a mismatch, the app runs a separate background verification:

- Both recorded and current local branch refs must exist and resolve to commits.
- HEAD must be attached to a branch different from the recorded branch.
- The worktree must have no uncommitted tracked or non-ignored untracked work.
- Neither branch may contain commits outside the accepted base and all local remote-tracking refs.

If verification succeeds, a confirmation names both branches and says: "No unpublished work was found. Closing removes the worktree and stops all task terminals. Both local branches will be kept." An active agent gets an additional warning. Enter or Close task confirms; Escape or Cancel leaves the task and terminals running and restores the opening focus.

Confirmation removes the worktree, journal entry, and card and stops all task-owned PTYs. It does not delete either local branch or modify remote branches. The card title, recorded branch, and folder are not renamed by this recovery path.

If verification fails, a toast explains the risk or Git failure and the card remains. There is no Push or Discard shortcut in this recovery flow. Commit and push in the shell, integrate the work, or return to the recorded task branch to use the existing close choices. Committing alone does not publish work. Missing refs, detached HEAD, and ambiguous rename history are not silently adopted.

## Safety model and limits

This checks each branch's commit reachability, not just whether their tips match or whether the current branch has an upstream. An original branch may hold unpublished commits hidden by switching to a clean branch. Both must be checked independently.

`worktree::switched_close_tips` uses full `refs/heads/...` names and read-only Git commands (`GIT_OPTIONAL_LOCKS=0`). For each branch it asks whether any commits remain after excluding all remote-tracking refs and the recorded base, falling back to the existing default-base resolver if necessary. As with ordinary close, an integrated commit need not also exist on a same-named remote branch. A local base that is the branch being checked is not accepted as evidence: only remote-tracking refs count in that case. This avoids treating unpublished local `main` commits as already integrated merely because `main` is the base.

Remote-tracking refs are local evidence, not a live server guarantee. This flow does not fetch, contact a hosting API, query PR state, or require network access. Stale refs and manual ref edits have the same limitations as existing close checks. Squash/rebase integration does not prove original commit reachability; retain/push the original branch when verification refuses. Ignored files follow existing worktree removal semantics and are not publication evidence.

A preview binds confirmation to both branch names and their commit IDs. Core repeats the complete checks before stopping PTYs and after stopping them. Changed tips, changed HEAD, new dirty work, deleted refs, or unreadable Git state prevent cleanup. A failure after stopping PTYs retains the card, worktree, and journal but the processes may already be stopped. Removal never uses `--force`, and no branch deletion command is issued.

The core operations lock serializes Shika's lifecycle actions, not external Git commands. Git verification and worktree removal are not an atomic transaction with arbitrary external writers. Do not claim this is a sandbox or protection against deliberately racing external processes. Preserve the final rechecks and non-forced removal.

## Implementation map

| Piece | Location and symbols | Responsibility |
| --- | --- | --- |
| Identity protection | `crates/shika-core/src/lib.rs`: `ensure_session_branch` | Ordinary close, discard, and push still require recorded branch identity or proven rename |
| Verification | `crates/shika-core/src/worktree.rs`: `switched_close_tips` | Dirty check, resolve both refs, test reachability, return commit tips |
| Preview | `crates/shika-core/src/lib.rs`: `SwitchedBranchClose`, `session_switched_close_check`, `switched_close_check_locked` | Return an opaque confirmation snapshot without changing ownership |
| Cleanup | `Core::session_close_switched`, `hang_up`, `forget_session` | Recheck preview, stop all owned PTYs, recheck, remove only worktree, forget session |
| Refusal copy | `crates/shika-core/src/error.rs`: `SwitchedCloseUnsafe` | Explain why safe recovery is unavailable |
| UI dispatch | `crates/shika/src/main.rs`: `Shika::close`, `finish_close` | On `TaskBranchChanged`, verify off-thread; action 3 confirms recovery |
| Dialog and keys | `Overlay::SwitchedClose`, `Shika::key`, overlay rendering, `cancel_overlay` | Branch facts in mono, existing themed dialog/buttons, Enter confirms, Escape restores focus |

No persistence schema, dependency, CLI launch argument, branch-refresh policy, or terminal ownership format changes.

## Debugging

In the affected task shell, collect only repository metadata relevant to the failure:

```sh
git status --short
git branch --show-current
git show-ref --verify refs/heads/task-name
git show-ref --verify refs/heads/better-name
git for-each-ref --format='%(refname)' refs/remotes/
git rev-list --max-count=1 refs/heads/task-name --not --remotes origin/main --
git rev-list --max-count=1 refs/heads/better-name --not --remotes origin/main --
```

Replace names and `origin/main` with the actual recorded branch, current branch, and task base. Any output from the last two commands means commits remain outside those refs. When the base is the same local ref being checked, omit that base argument to mirror the safety rule.

- Current branch pushed but closure refused: inspect the original branch too.
- Original branch missing after an unobserved rename: return to the renamed task branch so reflog adoption can run, or investigate the missing ref. Do not fabricate rename proof.
- PR merged but closure refused: inspect local remote-tracking refs and whether the merge rewrote commits. PR status alone is not the safety proof.
- Confirmation becomes stale: Cancel and Close again to obtain fresh verification.
- Cleanup fails after confirmation: retain the worktree/journal; inspect Git's error and PTY state. Do not retry with force automatically.

Use disposable repositories, local bare remotes, and an isolated `--data-dir` for reproduction. Do not publish personal paths, real prompts, repository URLs, transcript content, or credentials in issue reports or fixtures.

## Tests and extension guardrails

`crates/shika-core/src/lib.rs` tests beginning `switched_close_` cover pushed branches with and without upstreams, merged work, independently published branches, integrated original work, empty local tasks, hidden unpublished commits on either branch, dirty files, missing refs, detached HEAD, changed tips/HEAD after preview, local-main false publication, branch/journal preservation, and shell teardown.

Run `cargo fmt --all --check`, `cargo test --workspace`, and `cargo clippy --workspace --all-targets -- -D warnings`. Native dialog rendering, focus, keys, and light/dark acceptance remain separate in [MANUAL_CHECKS.md](../MANUAL_CHECKS.md#branch-switch-close).

When extending:

- Do not weaken `ensure_session_branch` or silently adopt arbitrary checkouts.
- Never delete either branch through this recovery path, even if it looks empty.
- Keep verification in core, off the UI thread, and repeat it at confirmation.
- Do not enable ordinary Discard/Push on switched tasks without a separate ownership and safety design.
- Bind confirmation to stable task identity and branch tips; do not rely only on UI card labels.
- Fail closed on missing refs or command errors; do not infer publication from equal commits, a missing old name, or PR status alone.
- Record intended behavior separately from tested GUI evidence. Use synthetic, non-personal examples for open-source documentation and tests.
