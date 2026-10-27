# Switch agent

One agent can do the first part of a feature and another the next part, in the same worktree. Claude Code on the UI, then Codex on the backend. Or one agent can do the work and another can review it. The author writes whichever prompt they want.

The second process sees the files and the git diff. It does not receive the first conversation, a recap, or a stored transcript. A continuation has to be written in the prompt. That missing conversation is intentional for a review, which is why the prompt is not locked to one.

Read [AGENTS.md](../AGENTS.md), [design/DESIGN.md](../design/DESIGN.md), [keyboard-flow.md](keyboard-flow.md), [terminal-tabs.md](terminal-tabs.md), [agent-activity.md](agent-activity.md), and [branch-naming.md](branch-naming.md) before changing it. The confirmation follows the busy and recheck pattern in [publishing.md](publishing.md).

## What it does

On a [Terminal task](terminal-tasks.md), this starts its first agent. The original pinned shell moves to an additional numbered Shell tab without stopping or recreating it; its programs, output and draft stay. Existing shells stay too. Terminal itself is not a choice in Switch agent. A still-generated branch can then be named by the agent as an ordinary draft would be.

Agent > Switch agent…, or Cmd+Shift+A, opens a 400px dialog for the selected ordinary task. Both do nothing while the app is busy or any overlay is open. There is no header button and no card context-menu item.

The dialog is available when the agent is Ready, Waiting, or its CLI has exited. It is refused, with a toast, while the agent is Working or Asking, while setup is running, while the task is closing, and while the author has unsent typed text in that agent terminal. Shell-tab drafts do not count. The Lead is refused the same way Create PR is: "The Lead cannot switch agent." A worker the Lead started may switch. It keeps `started_by`, and the new agent PTY still receives that worker's control env so `shika report` works. The new-task report footer is not added to this prompt.

The prompt field starts empty. Its hint is: "What should this agent do? Leave it empty to type in the terminal." Empty is allowed, including whitespace. Confirming with an empty prompt starts the CLI the way New does, with no positional argument, so the card stays Waiting until the author types. A non-empty prompt is the CLI's single positional argument after the preset's existing launch flags from `agents::launch_flags`, and it counts as a submission, so the card becomes Working. There is no prefilled review sentence. The list is the same CLIs New would list: on the login-shell PATH and turned on in Settings, including the current preset, because a fresh process of the same CLI is valid. `j`/`k` or the arrows move. Tab edits the prompt (while it is focused, `j`/`k` type, and Up/Down still move). Enter confirms. Escape cancels and restores focus. Cancel starts nothing. A click selects a row and does not confirm.

Confirming rechecks. If the turn became active, the current process keeps running and the dialog shows the error. Otherwise Shika starts the new agent PTY in the same worktree. It does not invent flags, pass `--worktree`, run worktree preparation, commit, push, or close. Only after that process is running does it stop the old agent PTY. Shell PTYs stay up the whole time. If the start fails, the old agent keeps running and the dialog shows the error.

The pinned agent tab stays the pinned agent tab. Its label becomes the new preset name. A task nobody has named yet reads "New <CLI>" for the new CLI. The old agent scrollback goes away with the old PTY. One terminal view is created for the new PTY and focused. Hidden shell views stay alive. The diff stat, the Changes panel, and the base ref stay as they are.

Naming depends on whether the task already has a name. A task whose branch is no longer `shika-draft-<id>` (or whose CLI title was applied) keeps its name: the session is marked already titled (`cli_titled`), so neither the switch prompt, a line typed after an empty switch, nor the new CLI's session title renames the branch or replaces the display name. A task still on its draft branch has nothing to protect, so it is named the way New names it: from the switch prompt, or from the first line typed after an empty switch, and then once from the new CLI's own session title. This covers picking the wrong CLI in New and switching before typing anything. A manual name still wins over automatic display titles, and a branch already on a remote stays as it is.

## Where it lives

| Piece | Location |
| --- | --- |
| Start the new PTY, then stop the old agent | `Core::session_switch_agent`, sharing `Core::open_agent` with New and the Lead, in `crates/shika-core/src/lib.rs` |
| Keep branch, title, shells, owner; set `cli_titled` | `SessionStore::retarget_agent` in `crates/shika-core/src/session.rs` |
| Dialog, menu, Cmd+Shift+A | `Overlay::SwitchAgent`, `Shika::switch_agent`, `confirm_switch`, `install_switched_agent` in `crates/shika/src/main.rs` |
| Replacement terminal while the process starts | `Card::next_agent`; the tick skips the card and `control_status` reports Starting |
| Empty prompt stays Waiting; a prompt submits; only a draft task is named | `SessionStore::retarget_agent` decides `cli_titled`; `switch_prompt`, `apply_switch_turn` (`HostState::seed_launch_prompt` with `name` true only for a draft), then `TitleWatch::finish` or a fresh `TitleWatch` |
| Reserved chord | `cmd-shift-a` in `crates/shika/src/shortcuts.rs` |

## Guardrails

- Do not copy a transcript, write a recap, or show a commit-subject list. The next process sees the worktree and the diff.
- Do not prefill a review sentence, and do not refuse an empty prompt. Empty is how New starts a CLI.
- Do not call `hang_up` for the switch. That also stops shell PTYs. Close only the previous agent PTY, and only after the new one is open.
- Do not replace the agent terminal view until the new process is running. A failed start drops the unused view and leaves the original pane in place.
- Keep the replacement in `Card::next_agent` until it is installed. Core stops the old agent before the UI swaps panes, so the tick must not sample the card meanwhile: the old CLI's exit would read as a finished turn, post a Ready notification, and ring the Lead.
- Do not let the prompt, a line typed after an empty switch, or the new CLI title rename a task that already has a name. For a named task, mark `cli_titled`, finish the naming capture, and finish the title watch. For a task still on `shika-draft-<id>`, leave all three open, as New does. `seed_launch_prompt` runs only when the prompt is non-empty.
- Replace `Activity` with a fresh one. An exited activity ignores later submissions, so a switch after the CLI has exited would stay idle. An empty prompt leaves that activity Waiting. A prompt calls `hold_for_launch` and `advance` immediately.
- Apply a prompted launch turn immediately and clear `launch_turn` on the new host, so the tick does not count the same submission twice.
- Reuse the worker's existing control token. A new token would not be recognized by `shika report`.
- Do not add a `shika` command for this. It is an author menu and shortcut, not a Lead command.
