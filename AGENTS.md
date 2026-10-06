# Shika

Mac desktop app for running a few coding agents at once. The author adds a repo, presses New, and gets a git worktree plus the CLI they already pay for. They write the prompt, test, and push. Shika does not wrap a model API.

Update this file when behavior, a checklist item, or a toolchain trap changes. Keep it short. `PRD.md` and `PLAN.md` stay the long spec.

## Commit attribution

Never add yourself or any AI agent (Cursor, Claude, Codex, Pi, or others) as a commit author or co-author. Do not add AI `Co-authored-by` trailers or agent attribution to commit messages. Keep authorship with the human author. This applies to every agent, including merge commits.

## Spec

`PRD.md` is the spec. `PLAN.md` is the build order and records later decisions from the author. Where they disagree, `PLAN.md` wins. Do not relitigate either file.

In particular:

- Desktop app, pure Rust on GPUI, Mac only. No kanban, no flat session list, no project tabs, no resizable split, no second visible terminal, no editor, no conversation history.
- GPUI comes from the `zed-industries/zed` git repo at one pinned `rev`. The terminal engine is `alacritty_terminal`, and its types stay inside one module of `shika-terminal`. Never copy from Zed's `terminal` or `terminal_view` crates; they are GPL-3.0.
- This build launches Claude Code, Codex, Cursor CLI, and Pi. Kiro waits.
- Shika creates the worktree. Never pass Cursor's `--worktree`. Do not invent a CLI flag; the launch args in `PLAN.md` were taken from each binary's `--help`.
- A `git push` in the shell does not remove the card. The author closes the card. A push typed inside the agent CLI is ignored.
- Close asks before throwing away uncommitted work or unpushed commits: discard, or push when the tree is clean. The app does not commit.
- Projects persist. Live sessions do not come back after a relaunch. Quit does not delete worktrees.
- One terminal view per live PTY, kept alive when hidden, so a full PTY buffer cannot stall the CLI. New creates only the pinned agent tab. `+` or Cmd+T adds independent shell tabs in that worktree; each keeps running until closed. Tabs belong to the task, with one visible terminal and no splits. Before changing tabs, read `docs/terminal-tabs.md` for the decision, ownership/lifecycle map, startup and focus traps, debugging, and contributor guardrails.

## Where the build is

The GPUI app is implemented in `crates/shika`. The Tauri, React, xterm.js, Node, and Vite app has been removed. The app includes project persistence, draft worktrees, cards, the keyboard map, task-scoped terminal tabs, task branch naming from the CLI's own session title, a two-second quiet timer, native notifications with the system alert sound, discard-or-push close, and explicit leftovers cleanup.

Optional preparation: `.shika/worktrees.json` in the main checkout declares `setup-worktree` commands, literal ignored `copy-files`, and `timeout-seconds` (default 600). New asks for local approval, again on parsed configuration changes. Copy/setup finish before the CLI starts, with two setup slots, output in the existing terminal, cancel, and fresh-worktree retry. Setup typing is suppressed; startup query replies must still reach the agent. Active preparations stay journaled but out of disposable leftovers. Failures remove only provably untouched trees; changed or unverifiable work stays for explicit cleanup. Quit/project removal cancel setup and retain trees. No configuration means the existing flow, no pooling, inferred installs, or execution of Cursor/Codex configuration. Commands are trusted code, not a sandbox, and must not daemonize. Before modifying this feature, read `docs/worktree-preparation.md`: decisions, launch/cleanup lifecycle, symbol map, input-phase traps, debugging fixture, and extension guardrails.

Branch names: the first prompt line names the branch at once. About a second later the CLI's own session title, read from Claude Code's, Codex's, Cursor's, or Pi's private files, renames it once, without the project name and with the optional Settings prefix. Pi has a title only when one was set. Those files are not a public API, so reading them is best effort; on any failure the prompt name stays. A branch already on a remote is never renamed by Shika. External Git renames are followed only with an explicit reflog rename chain and no surviving original branch; the card and journal update, while real branch switches still block Close. `PLAN.md` has the rules; `docs/branch-naming.md` explains the why, the code map, the CLI file formats, and how to debug it.

Close reports branch switches separately from git-status failures and names the current and recorded task branches. Return to the task branch in that card's shell before closing; switching to an existing PR branch does not transfer task ownership.

Asking is not parsed. Ready means the output became quiet or the process exited, including a non-zero exit. Notifications are titled `{project} - {task}` and route clicks to the matching card. A card notifies once per turn: a turn starts when the user types or pastes into the agent, and it notifies only if output kept arriving at least a second after the user's last input. Echoes and the redraws that focus, clicks, scrolling, or a resize do not notify and do not mark the card Working. A draft typed into the agent, without Enter, leaves a Ready card Ready, so Close still sees an idle agent. Permission denial is reported in the app.

Ready cards show a diff stat such as `2 files +64 −3`, fetched in the background each time the card turns Ready and hidden on failure or with no changed files: `Core::session_diff_stat` compares the worktree to the merge base with the ref the card started from (its base branch), so it counts commits and uncommitted edits, plus untracked files that are not ignored. Close checks for own and unpushed commits against that same ref. If it no longer resolves, both use the default branch. Binary files count as files with no lines. Without either it falls back to HEAD, so only uncommitted work counts. It reads only, with `GIT_OPTIONAL_LOCKS=0`, and takes no core lock.

`MANUAL_CHECKS.md` records integrated acceptance results and remaining checks. `crates/shika-terminal/MANUAL_CHECKS.md` records terminal-specific checks. Implementation and passing unit tests do not establish every GUI check.

Public code is MIT licensed. JetBrains Mono retains its separate OFL license. `design/DESIGN.md` is the visual spec. `design/Shika v3.dc.html` is the reference prototype. Runtime icons live in `assets/macos/`.

## Design

Before any UI work, read `design/DESIGN.md` and follow it. That file is the source of truth for color, type, spacing, layout, focus, motion, copy, and icons.

## Using the app

`a` or Add project picks a git repo. A nested folder becomes the repo root. `n` opens the CLI picker (`j` / `k`, `1` / `2` / `3` / `4`, Enter, Escape). The new card is selected and the CLI starts in `<repo>/.worktrees/shika-draft-<id>`, after approval and setup when preparation is configured.

The column is 540px. Its 48px top row is the title bar: traffic lights, the wordmark, the gear, and New agent; the terminal header fills the same row on the right, and empty space in both drags the window. The `+` on a project header, or its empty box, opens the picker for that project. A project's Remove appears when its header is hovered. Clicking a status chip selects the first card with that status, in row order.

`j` / `k` or the arrows move through every card, including cards hidden by the three-card cap, and skip project headers. An empty project is not a stop. `n` or Cmd+N opens the picker for the selected card's project, or the first project when nothing is selected, and Tab there changes the project. Enter focuses the terminal. Ctrl+Q returns to the cards. Escape is typed into the terminal. Clicking a terminal tab focuses it. Cmd+T adds a shell, Cmd+W closes the selected shell and does nothing on the agent, and Ctrl+Tab / Ctrl+Shift+Tab cycle tabs. Cmd+1 through Cmd+9 jump to a tab, with the agent on Cmd+1. Each card retains its selected tab. Shell close stops its processes but leaves the task and worktree intact; Close task stops every owned PTY through the existing safe-close flow. Cmd+] / Cmd+[ move to the next/previous agent in row order, skipping project headers, wrapping, and keeping terminal focus when invoked there. Cmd+N opens New in the current project from either surface. These shortcuts do nothing while a dialog or picker is open. The Agent menu lists them; New, new shell, and shell close also have tooltips. Picker cancel, Settings dismissal, and Base branch completion or cancel restore the focus that opened them. Keyboard selection scrolls into view without pinning manual scrolling. `c` closes. `b`, or a click on the branch after the project path, opens the Base branch dialog for the selected project.

New starts the task branch from the project's base branch: `origin/<base>`, else the local one, else New fails with "Base branch dev not found." With no base set it is `origin/HEAD`, then `main`, `master`, then the main checkout's HEAD. Opening the picker fetches that one branch from origin (never prompts, 4 second cap, failures ignored), and New waits for it. The worktree is created with `--no-track`, so a fresh card has no upstream. The picker footer names the base. In the dialog, Enter applies once the branch exists on origin or locally, empty goes back to the default, and Escape cancels. Running cards keep the base they started from.

The shell is the user's login shell, with its cwd on the worktree. `git status` and `git diff` there are that task, not the main checkout. The author commits and pushes there.

Dropping a file (a screenshot thumbnail, an image from Finder) on the terminal pastes its escaped path and focuses that terminal. Claude Code and Cursor turn a pasted image path into an attachment.

Typing in the terminal, a text field, the picker, or the close dialog does not run the card keys. `docs/keyboard-flow.md` explains the Command flow layer, focus restoration, shell startup typeahead, selected-row scrolling, code map, and debugging checks for contributors.

Cmd-, (also the Shika menu and the gear in the top row of the column, left of New agent) opens Settings. Hovering the gear shows Settings ⌘,. The dialog sets background opacity, blur radius, whether translucency covers the sidebar alone or the sidebar and terminal, the terminal font size, the branch prefix, and whether a notification plays a sound. Opacity is 0 to 100%, blur radius 0 to 255, and font size is 8 to 32 (default 14). `j` / `k` choose a row. `h` / `l` or the arrows step opacity and blur by 5, and font size by 1. Typing digits (or clicking the number) edits a value: Enter applies, Escape cancels the edit, and out-of-range values are clamped. A font size can include one decimal, such as 12.5. On the prefix row, Enter or a click edits the text, Enter applies, and Escape cancels. On the sound row, `h` turns the alert off and `l` turns it on. The banner still posts either way. Escape closes the dialog. Changes apply and save at once. At 100% opacity the window is opaque and blur does nothing. Below that, the column and resting cards frost, unless macOS Reduce transparency is on. The selected card, dialogs, picker, toast, and tooltips stay solid, because the blur only reaches what is behind the window and GPUI has no backdrop blur. The terminal background stays at or above 85% when frost covers it, and CLI-colored cells stay opaque. The title bar uses the sidebar opacity, so the blur shows through it. Light and dark follow the system appearance. The values are in `design/DESIGN.md`. The font size changes the terminal text. The chrome stays at its own sizes.

## Code

The Cargo workspace has three crates:

```
Cargo.toml              workspace, members crates/*
crates/shika-core/      projects.json, worktrees.json, settings.json, worktree add/remove,
                        login-shell PATH, agent presets, PTY processes and env, sessions.
                        No UI, no GPUI.
crates/shika-terminal/  alacritty_terminal wrapper and the GPUI terminal view. PTY bytes in,
                        input bytes and resizes out. Spawns no process.
crates/shika/           the GPUI app: window, cards, picker, close dialog, settings, toast,
                        keys, terminal tabs, translucency, .app bundle
```

`assets/fonts/` holds JetBrains Mono and its OFL license. `assets/macos/` holds the bundle metadata and icon. `scripts/bundle-app.sh` builds a local ad hoc signed `.app`. `site/` is the static landing page; see `site/README.md`.

App data: `~/Library/Application Support/com.hieule.shika/`, the same directory used before the migration. `projects.json` and `worktrees.json` keep working there. `projects.json` holds each project's optional `baseBranch` and locally consented `approvedPreparation`; `worktrees.json` records each worktree's `baseRef`. `settings.json` holds the appearance, the terminal font size, `branchPrefix`, and `notificationSound`; a missing file or field takes the default (opaque, 14, no prefix, sound on). Sessions, titles, status, and PTY ids are memory only.

Worktrees live at `<repo>/.worktrees/<branch>`. `.worktrees/` is appended to that repo's `info/exclude`. Do not edit the user's `.gitignore` when the exclude file works.

Every PTY gets the login-shell `PATH`, `PWD` set to the worktree, and these variables removed so git cannot follow another checkout: `GIT_DIR`, `GIT_WORK_TREE`, `GIT_INDEX_FILE`, `GIT_PREFIX`, `GIT_COMMON_DIR`, `GIT_OBJECT_DIRECTORY`.

## Toolchain

Cargo only. A fresh shell often has no `cargo`.

```sh
source "$HOME/.cargo/env"
```

`~/.zshenv` is a Nix symlink; rustup must not try to edit it. GPUI needs full Xcode, which is installed. The separate Metal Toolchain component is not installed, so the pinned platform dependency currently uses `runtime_shaders`. Do not remove that feature without installing the component and verifying the build.

```sh
cargo test --workspace
cargo run -p shika
./scripts/bundle-app.sh --debug
open target/debug/Shika.app
```

`cargo run` inherits a terminal `PATH` and hides the Dock-launch bug. GUI apps do not see Homebrew, nvm, or `~/.local/bin`. `path_env` runs the login shell once at startup from a short `PATH`, with the environment cleared, because Nix's `__NIX_DARWIN_SET_ENVIRONMENT_DONE` stops a login shell from rebuilding `PATH`. Claude and Cursor on this machine are under `~/.local/bin`. The real check is a built `.app` opened with `open`, not only `cargo run`.

Claude Code saves no transcript, and so writes no session title, when it inherits `CLAUDE_CODE_CHILD_SESSION`. A Shika started with `cargo run` from inside a Claude Code session passes that variable to its PTYs, so branch titles fall back to the prompt there. Launch with `open` to test naming.

Preparation integration tests currently have a known parallel PID/timestamp temp-directory collision. `docs/worktree-preparation.md` records diagnosis and a serial workaround, not a fix; retain parallel workspace validation.

Never test against the author's normal `projects.json`. Use disposable git repositories and local bare remotes, with `open -n target/debug/Shika.app --args --data-dir /absolute/test/data`. Before sending synthetic keystrokes, confirm Shika's own window is in front. `--diagnostics-file /absolute/report` writes CLI discovery there and metadata-only native notification diagnostics to `/absolute/report.notifications`.

Translucency uses GPUI's `Transparent` window background, not `Blurred`, because GPUI's blur has one fixed strength. The blur radius is set with the private `CGSSetWindowBackgroundBlurRadius` in `crates/shika/src/appearance.rs`, as Ghostty, WezTerm, and winit do. The terminal paints its default background with the configured alpha; its parent must not paint a second translucent fill under it.

No em dashes in code, docs, or visible text.
