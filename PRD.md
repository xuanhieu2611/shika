# Shika MVP

Shika is a keyboard-first Mac app for running a few coding agents at once. It is named for Shikamaru: the person who keeps track of everyone on the field while you give directions.

Shika manages projects, agents, and git worktrees. The user writes the prompt, answers questions, tests, and pushes. Shika creates the worktree, starts the CLI they already pay for, and removes the live session and the worktree when the user closes the task. A push keeps the card open.

This is an early-stage app built for daily use on real repositories, with signed Mac downloads planned after the core workflow is validated.

## Decisions already made

Do not relitigate these.

- Desktop app, not a terminal app. One monitor, fullscreen. The user already uses a terminal and coding tools. Existing terminal multiplexers handle the usual shell workflow; Shika manages coding-agent tasks.
- Pure Rust on GPUI, Zed's GPU UI framework. Rust owns worktrees, processes, and the window. The product is terminals and keyboard, so the same process that reads each PTY parses it and draws it on the GPU. No web view, no Chromium, no bridge into JavaScript. This is lighter than the Electron shell Codex and T3 Code use, and it is what terminal multiplexer users expect.
- Mac first. Windows can come from the same project later. Do not build Windows in the MVP.
- The user brings their own CLIs and subscriptions: Claude Code, Codex, Cursor CLI, and Pi in this build. Kiro waits. Shika does not wrap a model API.
- Permissions are bypassed. Shika never asks the user to approve a shell command. Launch each CLI with its current flag for that, after checking `--help`. Do not invent a flag.
- One agent, one fresh worktree. Do not reuse an old worktree. Reuse is how stale files leak into the next task.
- Old conversations are not shown. Remembering them for a future prompt is a later backend problem, not this UI.
- Tasks in one repo are independent. They must not need the same files. The app does not solve merge conflicts between agents.

## The loop

This is the only workflow the MVP has to get right.

1. The user is in a project and presses New.
2. They pick a CLI. Shika creates a branch and a worktree, then opens that CLI in the terminal, already in the worktree.
3. They type the prompt in the agent's own terminal and press enter. They can switch away before pressing enter. The draft stays, and the card says it is waiting.
4. They leave. They do not read the agent's thinking. They are often in a browser, fullscreen, with Shika hidden.
5. They come back when an agent asks a question or when it is finished.
6. A question is answered in the agent's terminal. Sometimes that is a multiple-choice list. Sometimes they type. Shika does not replace the agent's question UI.
7. When it is finished, they read the result in that same terminal, then open Shika's shell in that worktree and run `git status`, `git diff`, tests, commit, and push themselves.
8. A successful `git push` in Shika's shell leaves the card open. Close removes the card and worktree. If the agent is working, the tree is dirty, or commits are unpushed, Close offers Discard changes and, when clean with something to push, Push changes. Shika does not commit. Escape cancels and a dirty tree returns to the shell.

Switching cards does not stop an agent and does not delete anything.

## Layout

`PLAN.md` records later decisions: the column is 540px by default, and since 2026-10-06 it can be resized by dragging its edge and hidden with Cmd+B. The original text follows.

- Left column, 280px. Every added project is visible, including a project with no agents, so "is anyone on this repo?" is obvious.
- Under each project, up to three cards. If there are more, show the count and let the rest be reached from the keyboard. People rarely run more than three.
- A card is one agent. It shows the CLI name, the task name, and the status. The task name is the prompt they submitted, shortened. Until they submit, the card is "New Claude Code" (or whichever CLI) and the status is waiting.
- Right side: the selected agent's real terminal. If nothing is selected, the right side is empty. There is always at most one terminal on screen.
- The worktree path is shown only in the terminal header, not on the cards.
- A control on the terminal switches between the agent and a shell in the same worktree.

Grouped by project is required. Do not build a flat session list. Do not build one-project-at-a-time tabs. Do not build a kanban.

Statuses, in this order of attention:

| Status | Meaning |
| --- | --- |
| Asking you | The agent is waiting for a reply |
| Ready to check | The agent finished the prompt |
| Working | The agent is running and does not need a reply |
| Waiting | The CLI is open and the prompt has not been sent |

A line at the top of the column counts them: how many agents, how many working, how many asking, how many ready.

The working view stays quiet. A few lines are enough. Do not stream a wall of thinking into a custom chat. The terminal is where the user reads the agent.

## Keyboard

The app has to be usable without the mouse. Focus starts on the cards.

| Key | Action |
| --- | --- |
| `j` / `k` or arrows | Move between every card, including hidden cards. Project headers are skipped |
| `a` | Add a project with the folder picker |
| `n` | New agent on the selected card's project. If nothing is selected, the first project |
| `Enter` | Focus the terminal |
| `Ctrl+Q` | Focus the cards again |
| `c` | Close the selected conversation, with the confirm rule above |

Typing in the terminal or in a text field must not trigger these keys. `Ctrl+Q` still leaves the terminal, so `Escape` reaches the program.

## Projects and worktrees

- A project is a local git repository the user adds by choosing a folder.
- The list is remembered across launches, in the app data directory, `~/Library/Application Support/com.hieule.shika/`.
- Worktrees live at `<repo>/.worktrees/<branch>`. Add `.worktrees/` to that repo's git exclude or info exclude so it is not committed. Do not rewrite the user's `.gitignore` if a local exclude works.
- Branch name comes from the prompt, slugified, and made unique if the name already exists.
- Create with `git worktree add -b <branch> <path>`. Remove with `git worktree remove` when the task is closed or pushed. If remove fails because of uncommitted work, the confirm step already happened, then force-remove only after they confirmed delete.
- The shell's current directory is the worktree. `git status` and `git diff` must show that task's files, not the main checkout.

## Agents

Ship Claude Code, Codex, Cursor CLI, and Pi. If the binary is not on `PATH`, say so on the card or in the picker and do not pretend it launched.

| Preset | Command to resolve |
| --- | --- |
| Claude Code | `claude` |
| Codex | `codex` |
| Cursor CLI | `agent` |
| Pi | `pi` |

GUI apps on Mac do not see the shell's `PATH`. Resolve binaries the way a login shell would (Homebrew, nvm, cargo, Volta), or launching from the Dock will fail while launching from a terminal works. This is a known trap. Test a Dock launch, not only a run from a terminal.

Each process is a real PTY. The UI embeds a terminal bound to that PTY: a Rust terminal engine, drawn by GPUI. A fake transcript is not the MVP.

Detecting "asking you" versus "working" is imperfect because every CLI draws its own UI. For the MVP:

- Waiting: process is up, the user has not pressed enter on the first prompt in Shika's terminal.
- Working: output is still arriving, or the process is not sitting at an idle prompt.
- Ready to check: about two seconds of quiet after work starts, or the process exits with any code.
- Asking you: best-effort. If it cannot be detected reliably, a macOS notification is still required when the process goes idle, and the user opens the terminal to see whether it asked a question or finished. Do not block the MVP on parsing every CLI's question widget.

When status becomes Asking you or Ready to check, post a macOS notification naming the project and the task. Clicking it focuses Shika and selects that card. The user is fullscreen in a browser while agents run, so the notification is how they know to come back.

## Close and push

- A push in the shell or the agent CLI leaves the card and worktree open.
- Close asks if the agent is working, the worktree is dirty, or commits are unpushed.
- Discard changes stops the PTYs, force-removes the worktree, and deletes the local branch.
- Push changes is available only for a clean tree with unpushed commits. It runs `git push -u origin HEAD`, removes the worktree on success, and keeps the local branch. A failed push leaves the card open.
- Escape cancels. For a dirty tree it focuses the shell so the user can commit.
- A clean task with nothing to lose closes immediately. Pushed branches stay; empty draft branches are deleted.
- Quit preserves worktrees. Relaunch restores projects but no live sessions. A leftovers screen lists journaled worktrees and removes them only on request.

## Out of scope

- Phone, web, Windows, Linux builds
- Homebrew cask
- Account, sync, telemetry
- A planner agent that splits work
- Pull requests, CI, review comments
- Reusing or archiving worktrees
- A history browser of closed conversations
- Showing more than one terminal at once
- A resizable divider (superseded 2026-10-06: the agent column is resizable, see `PLAN.md`)
- Editing code in Shika
- Installing Claude, Codex, or the other CLIs

## Distribution, after the MVP works

Not part of the first build. When the author wants to publish:

- Signed, notarized `.dmg` on a GitHub release, using an Apple Developer account.
- Send the release link. People open the disk image and drag Shika to Applications.
- Homebrew cask later.

## Build checklist

Do these in order. Stop when the "Done when" script passes.

1. GPUI app that opens a window on Mac. Rust for the UI, the terminal, git, and processes.
2. Add a project by picking a git repo. Show it in the left column after restart.
3. New agent: picker for Claude Code, Codex, Cursor CLI, and Pi, create the worktree, spawn the CLI in a PTY with approval prompts skipped, show the PTY in the terminal, select that card.
4. Type a prompt, press enter, switch to another card, switch back, and find the draft or the running session intact.
5. Cards show CLI, name, and status. A project with no agents still appears. More than three cards collapse.
6. Keyboard map above works, including Dock launch finding the CLIs.
7. Shell toggle, cwd is the worktree, `git status` and `git diff` work.
8. Branch rename from the first prompt, quiet Ready status, and Close with discard or push choices. A shell push keeps the card open.
9. Notification titled `{project} - {task}` when a session becomes ready or is asking; clicking it focuses Shika and selects the card.
10. Leftovers after quit or crash, removed only on request. Empty right pane when nothing is selected. Working state stays quiet.

## Done when

Use the eight-step script in `PLAN.md`, which is the current acceptance script. It checks two repositories, both CLIs, notifications and replies, shell status/diff/commit/push, a card staying after push, safe Close preserving its pushed branch, and CLI discovery from the built `.app` opened with `open`.

## Look

Quiet Mac app. Light chrome, dark terminal. Cards are the navigation, not a spreadsheet of rows. Status color is the only loud color: a warm mark for asking, a green mark for ready. Sentence case. No marketing page inside the app.

Early layout explorations are superseded by the decisions in `PLAN.md` and the current visual specification in `design/DESIGN.md`. The archived prototype is `design/Shika v3.dc.html`; it is not a standalone runnable demo.
