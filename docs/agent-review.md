# Review with another agent

The author implements with one CLI, then starts a second CLI in that same worktree to review the files and the git diff. The second process does not receive the first agent's conversation. This is not a way to resume the same conversation.

Read [AGENTS.md](../AGENTS.md), [design/DESIGN.md](../design/DESIGN.md), [keyboard-flow.md](keyboard-flow.md), [terminal-tabs.md](terminal-tabs.md), [agent-activity.md](agent-activity.md), and [branch-naming.md](branch-naming.md) before changing it. The confirmation follows the busy and recheck pattern in [publishing.md](publishing.md).

## What it does

Agent > Review with another agent…, or Cmd+Shift+A, opens a 400px dialog for the selected ordinary task. Both do nothing while the app is busy or any overlay is open. There is no header button and no card context-menu item.

The dialog is available when the agent is Ready, Waiting, or its CLI has exited. It is refused, with a toast, while the agent is Working or Asking, while setup is running, while the task is closing, and while the author has unsent typed text in that agent terminal. Shell-tab drafts do not count. The Lead is refused the same way Create PR is: "The Lead cannot review with another agent." A worker the Lead started may switch. It keeps `started_by`, and the new agent PTY still receives that worker's control env so `shika report` works. The new-task report footer is not added to this prompt.

The prompt starts as: "Review the changes in this worktree against the base branch. Look for bugs. You have no earlier conversation. Do not rewrite the work unless you find a defect that should be fixed." It is editable. The list is the same CLIs New would list: on the login-shell PATH and turned on in Settings, including the current preset, because a fresh process of the same CLI is valid. `j`/`k` or the arrows move. Tab edits the prompt (while it is focused, `j`/`k` type, and Up/Down still move). Enter confirms. Escape cancels and restores focus. Cancel starts nothing. A click selects a row and does not confirm.

Confirming rechecks. If the turn became active, the current process keeps running and the dialog shows the error. Otherwise Shika starts the new agent PTY in the same worktree, passing the prompt as the CLI's single positional argument after the preset's existing launch flags from `agents::launch_flags`. It does not invent flags, pass `--worktree`, run worktree preparation, commit, push, or close. Only after that process is running does it stop the old agent PTY. Shell PTYs stay up the whole time. If the start fails, the old agent keeps running and the dialog shows the error.

The pinned agent tab stays the pinned agent tab. Its label becomes the new preset name. The old agent scrollback goes away with the old PTY. One terminal view is created for the new PTY and focused. Hidden shell views stay alive. The launch prompt counts as a submission, so the card becomes Working under the existing activity rules. The diff stat, the Changes panel, and the base ref stay as they are.

`session_rename_from_prompt` is not called. The new CLI's session title does not rename the branch or replace the display name. On a successful switch the session is marked already titled (`cli_titled`) without renaming, including while the branch is still `shika-draft-<id>`, so the review prompt cannot become the branch name. A branch already on a remote stays as it is.

## Where it lives

| Piece | Location |
| --- | --- |
| Start the new PTY, then stop the old agent | `Core::session_switch_agent` in `crates/shika-core/src/lib.rs` |
| Keep branch, title, shells, owner; set `cli_titled` | `SessionStore::retarget_agent` in `crates/shika-core/src/session.rs` |
| Dialog, menu, Cmd+Shift+A | `Overlay::Review`, `Shika::review_agent`, `confirm_review`, `install_switched_agent` in `crates/shika/src/main.rs` |
| Launch prompt that does not name the task | `HostState::seed_launch_prompt` with `name: false`, then `TitleWatch::finish` |
| Reserved chord | `cmd-shift-a` in `crates/shika/src/shortcuts.rs` |

## Guardrails

- Do not copy a transcript, write a recap, or show a commit-subject list. The reviewer sees the worktree and the diff.
- Do not call `hang_up` for the switch. That also stops shell PTYs. Close only the previous agent PTY, and only after the new one is open.
- Do not replace the agent terminal view until the new process is running. A failed start drops the unused view and leaves the original pane in place.
- Do not let the review prompt or the new CLI title rename the branch. Mark `cli_titled` and finish the title watch. `seed_launch_prompt(..., false)` does not set a card title.
- Replace `Activity` before starting the launch turn. An exited activity ignores later submissions, so a switch after the CLI has exited would stay idle.
- Apply that launch turn immediately and clear `launch_turn` on the new host, so the tick does not count the same submission twice.
- Reuse the worker's existing control token. A new token would not be recognized by `shika report`.
- Do not add a `shika` command for this. It is an author menu and shortcut, not a Lead command.
