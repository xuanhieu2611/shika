# Tasks and worktrees

The core lifecycle every other feature builds on: a project, a fresh worktree per task, the git checks Close relies on, and leftovers after quit. Read this before changing worktree creation, the base branch, the dirty or unpushed checks, Close, or leftovers.

Related guides: [branch-naming.md](branch-naming.md) for how the draft branch gets its name, [branch-switch-close.md](branch-switch-close.md) for Close after the user switched branches, [worktree-preparation.md](worktree-preparation.md) for optional setup before the CLI starts, [publishing.md](publishing.md) for Create PR, and [terminal-tabs.md](terminal-tabs.md) for PTY ownership. [MANUAL_CHECKS.md](../MANUAL_CHECKS.md) records acceptance evidence.

## Projects

- A project is a local git repository. A nested folder becomes the repository root (`git rev-parse --show-toplevel`), and Shika says so.
- Projects persist in `projects.json` as `{ id, name, path, baseBranch?, approvedPreparation? }`. The name is the folder name.
- Live sessions do not persist. Relaunch shows the projects and an empty terminal. Session ids, titles, status, and PTY ids are memory only.

## A new task

One agent, one fresh worktree. Shika never reuses an old worktree, because reuse is how stale files leak into the next task. Tasks in one repository are independent; Shika does not resolve conflicts between agents.

1. Append `.worktrees/` to `$(git rev-parse --git-path info/exclude)` if the line is missing. Never edit the user's `.gitignore`.
2. Create the draft from the main repository:

   ```sh
   git worktree add --no-track -b shika-draft-<id> <repo>/.worktrees/shika-draft-<id> <start>
   ```

   `--no-track` is required. Starting from `origin/dev` would otherwise make `origin/dev` the upstream, so a fresh card would look pushed and a plain `git push` would target `dev`.
3. Start the CLI in that folder with the login-shell `PATH`, `PWD` set to the worktree, and `GIT_DIR`, `GIT_WORK_TREE`, `GIT_INDEX_FILE`, `GIT_PREFIX`, `GIT_COMMON_DIR`, and `GIT_OBJECT_DIRECTORY` removed. The launch arguments are in [AGENTS.md](../AGENTS.md#clis-and-launch-arguments).

Shika creates the worktree. Never pass a CLI's own worktree flag, such as Cursor's or Codex's `--worktree`.

The folder keeps its `shika-draft-<id>` name for the life of the task, because the agent is already running inside it. Only the branch is renamed.

## The base branch

`<start>` is the project's base branch:

- With a base `B` set: `refs/remotes/origin/B`, else `refs/heads/B`. If neither exists, New fails with "Base branch B not found." It never falls back to another branch.
- With no base set: `refs/remotes/origin/HEAD` when it is valid, then local `main`, then `master`, then the main checkout's HEAD as the last resort.

Before creating, Shika fetches only that branch (`+refs/heads/B:refs/remotes/origin/B`), best effort: no terminal prompt, no askpass, ssh in batch mode, and a 4 second cap. The fetch starts when the picker opens, and New waits for it rather than starting another. On any failure or timeout, New uses the ref it already has.

The Base branch dialog (`b`, or a click on the branch in the project header) lists local heads and origin branches already on this machine and filters them as the field is typed. Up and Down move the highlight, and Enter saves it. A typed name that is not listed is fetched from origin first and saved only when that fetch finds it. `origin/dev` is saved as `dev`. Empty clears the base.

A running task keeps the full ref it started from, in memory and as `baseRef` in the journal. Changing the project's base later does not affect it.

## What a task compares against

The diff stat, Close's checks, and Create PR's default target use the task's `baseRef`. If it no longer resolves, or there is none (the HEAD fallback), they use the default branch: `origin/HEAD`, `main`, `master`, then the main checkout's HEAD, refusing when that checkout is on the task branch.

- **Dirty** means `git status --porcelain` is not empty.
- **Unpushed** means the branch has commits that are not on its upstream. A branch with no upstream is unpushed when it has commits that are in neither the base nor any remote-tracking branch.

Git reads for the diff stat use `GIT_OPTIONAL_LOCKS=0` and take no core lock.

## Push and Close

- **A push does not finish a task.** A `git push` in a shell tab, or typed inside the agent CLI, leaves the card, the session, and the worktree in place. The user closes the task when they want it gone.
- **Nothing to lose closes immediately.** If the agent is idle, the tree is clean, and the branch is already on a remote or has no commits of its own, Close removes the card and the worktree without asking. A pushed branch stays. An empty draft branch is deleted.
- **Otherwise Close asks.** If the agent is working or blocked, the tree is dirty, or commits are unpushed, the dialog offers:
  - **Discard changes** (`d`): stop every task PTY, `git worktree remove --force`, then `git branch -D`. Uncommitted files and unpushed commits are gone.
  - **Push changes** (`p`), only when the tree is clean and there is something to push: `git push -u origin HEAD`, then remove the card and the worktree and keep the local branch. A failed push keeps the card and shows the error.
  - **Escape** cancels. With a dirty tree it focuses the task's shell, opening one if needed, so the user can commit and close again.
- **Close never commits.** The only path that commits for the user is the confirmed Create PR flow in [publishing.md](publishing.md).
- **Branch identity.** Discard and Push act only on the task's own branch. External renames are followed as described in [branch-naming.md](branch-naming.md#external-branch-renames). After a real branch switch, Close offers the separate recovery in [branch-switch-close.md](branch-switch-close.md).

## Quit, crash, and leftovers

- Quit stops the PTYs and keeps every worktree. Shika never deletes work on its own.
- `worktrees.json` journals each task as `{ projectId, branch, path, baseRef? }`. It is for cleanup, not a session history.
- On the next launch, journaled worktrees with no live session are listed as leftovers. Each is removed only when the user asks. Removal follows a branch the user renamed since quit, and a path that no longer exists can still be forgotten.
- Worktrees still being prepared are not leftovers; see [worktree-preparation.md](worktree-preparation.md).

## Code map

| Where | What |
| --- | --- |
| `shika-core/src/worktree.rs` | `create_draft`, `resolve_base`, `configured_ref`, `known_branches`, `fetch_target`, `fetch_branch`, `is_dirty`, `git_state`, `diff_stat`, `push`, `remove_worktree`, `remove_draft`, and the journal (`Journal`) |
| `shika-core/src/lib.rs` | `Core::add_project`, `project_base`, `project_branches`, `prefetch_base`, `create_session`, `session_git_state`, `session_diff_stat`, `session_close`, `session_discard`, `session_push_and_close`, `leftovers_list`, `leftover_remove` |
| `shika-core/src/projects.rs` | `projects.json` |
| `shika-core/src/pty.rs`, `path_env.rs` | Child environment and the login-shell `PATH` |

## Guardrails

- Test only with disposable repositories, local bare remotes, and `--data-dir`. Never against personal projects or the normal app data.
- A change to what counts as dirty, unpushed, or "nothing to lose" changes what Close may delete. Add a regression test for each case it touches, including a branch with no upstream.
- Do not add automatic cleanup, worktree reuse, or a fallback to a different base branch.
- Update this guide and the short [AGENTS.md](../AGENTS.md) handoff when the behavior changes.
