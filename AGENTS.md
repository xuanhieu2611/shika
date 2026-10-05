# Shika

Mac desktop app for running a few coding agents at once. The author adds a repo, presses New, and gets a git worktree plus the CLI they already pay for. They write the prompt, test, and push. Shika does not wrap a model API.

Update this file when behavior, a checklist item, or a toolchain trap changes. Keep it short. `PRD.md` and `PLAN.md` stay the long spec.

## Spec

`PRD.md` is the spec. `PLAN.md` is the build order and records later decisions from the author. Where they disagree, `PLAN.md` wins. Do not relitigate either file.

In particular:

- Desktop app, pure Rust on GPUI, Mac only. No kanban, no flat session list, no project tabs, no resizable split, no second visible terminal, no editor, no conversation history.
- GPUI comes from the `zed-industries/zed` git repo at one pinned `rev`. The terminal engine is `alacritty_terminal`, and its types stay inside one module of `shika-terminal`. Never copy from Zed's `terminal` or `terminal_view` crates; they are GPL-3.0.
- This build launches Claude Code and Cursor CLI only. Codex, Pi, and Kiro wait.
- Shika creates the worktree. Never pass Cursor's `--worktree`. Do not invent a CLI flag; the launch args in `PLAN.md` were taken from each binary's `--help`.
- A `git push` in the shell does not remove the card. The author closes the card. A push typed inside the agent CLI is ignored.
- Close asks before throwing away uncommitted work or unpushed commits: discard, or push when the tree is clean. The app does not commit.
- Projects persist. Live sessions do not come back after a relaunch. Quit does not delete worktrees.
- One terminal view per live PTY, kept alive when hidden, so a full PTY buffer cannot stall the CLI. The shell PTY is created the first time it is opened, then kept.

## Where the build is

The GPUI app is implemented in `crates/shika`. The Tauri, React, xterm.js, Node, and Vite app has been removed. The app includes project persistence, draft worktrees, cards, the keyboard map, shell toggle, task branch naming from the CLI's own session title, a two-second quiet timer, native notifications, discard-or-push close, and explicit leftovers cleanup.

Branch names: the first prompt line names the branch at once. About a second later the CLI's own session title, read from Claude Code's or Cursor's private files, renames it once, without the project name and with the optional Settings prefix. Those files are not a public API, so reading them is best effort; on any failure the prompt name stays. A branch already on a remote is never renamed. `PLAN.md` has the rules; `docs/branch-naming.md` explains the why, the code map, the CLI file formats, and how to debug it.

Asking is not parsed. Ready means the output became quiet or the process exited, including a non-zero exit. Notifications are titled `{project} - {task}` and route clicks to the matching card. A card notifies once per turn: a turn starts when the user types or pastes into the agent, and it notifies only if output kept arriving at least a second after the user's last input. Echoes and the redraws that focus, clicks, scrolling, or a resize cause never notify. They can still flip the card to Working for two seconds. Permission denial is reported in the app.

`MANUAL_CHECKS.md` records integrated acceptance results and remaining checks. `crates/shika-terminal/MANUAL_CHECKS.md` records terminal-specific checks. Implementation and passing unit tests do not establish every GUI check.

Public code is MIT licensed. JetBrains Mono retains its separate OFL license. Original web mockups were removed; runtime icons live in `assets/macos/`.

## Using the app

`a` or Add project picks a git repo. A nested folder becomes the repo root. `n` opens the CLI picker (`j` / `k`, `1` / `2`, Enter, Escape). The new card is selected and the CLI is already in `<repo>/.worktrees/shika-draft-<id>`.

`j` / `k` or the arrows move through project headers and every card, including cards hidden by the three-card cap. Enter focuses the terminal. Ctrl+Q returns to the cards. Escape is typed into the terminal. `g` toggles the agent and the shell without moving focus; Enter again types in whichever one is showing. Clicking Agent or Shell focuses that terminal. `c` closes.

The shell is the user's login shell, with its cwd on the worktree. `git status` and `git diff` there are that task, not the main checkout. The author commits and pushes there.

Typing in the terminal, a text field, the picker, or the close dialog does not run the card keys.

Cmd-, (also the Shika menu and the gear at the right of the title bar) opens Settings. Hovering the gear shows Settings ⌘,. The dialog sets background opacity, blur radius, whether translucency covers the sidebar alone or the sidebar and terminal, and the branch prefix. Opacity is 0 to 100%, blur radius 0 to 255. `j` / `k` choose a row, `h` / `l` or the arrows step by 5, and typing digits (or clicking the number) edits it: Enter applies, Escape cancels the edit, and out-of-range values are clamped. On the prefix row, Enter or a click edits the text, Enter applies, and Escape cancels. Escape closes the dialog. Changes apply and save at once. At 100% opacity the window is opaque and blur does nothing. The title bar uses that same opacity, so the blur shows through it. Cards, dialogs, the toast, and cells with their own background color stay opaque.

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
                        keys, shell toggle, translucency, .app bundle
```

`assets/fonts/` holds JetBrains Mono and its OFL license. `assets/macos/` holds the bundle metadata and icon. `scripts/bundle-app.sh` builds a local ad hoc signed `.app`.

App data: `~/Library/Application Support/com.hieule.shika/`, the same directory used before the migration. `projects.json` and `worktrees.json` keep working there. `settings.json` holds the appearance and `branchPrefix`; a missing file or field takes the default (opaque, no prefix). Sessions, titles, status, and PTY ids are memory only.

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

Never test against the author's normal `projects.json`. Use disposable git repositories and local bare remotes, with `open -n target/debug/Shika.app --args --data-dir /absolute/test/data`. Before sending synthetic keystrokes, confirm Shika's own window is in front. `--diagnostics-file /absolute/report` writes CLI discovery there and metadata-only native notification diagnostics to `/absolute/report.notifications`.

Translucency uses GPUI's `Transparent` window background, not `Blurred`, because GPUI's blur has one fixed strength. The blur radius is set with the private `CGSSetWindowBackgroundBlurRadius` in `crates/shika/src/appearance.rs`, as Ghostty, WezTerm, and winit do. The terminal paints its default background with the configured alpha; its parent must not paint a second translucent fill under it.

No em dashes in code, docs, or visible text.
