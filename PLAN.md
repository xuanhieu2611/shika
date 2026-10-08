# Shika MVP plan

Build the Mac app described in `PRD.md`, in checklist order, and stop when the "Done when" script at the bottom passes.

The PRD is the spec, except where this file records a later decision from the author. Those decisions:

- A `git push` in the shell does not remove the card. The user removes it with Close.
- Close asks what to do with work that is not on the remote: discard it, or push it.
- This build launches Claude Code, Codex, Cursor CLI, and Pi. Kiro waits.
- Shika creates the worktree. Never pass Cursor's `--worktree`.
- Shika is a pure Rust app on GPUI, not Tauri 2 with a web view. Decided 2026-10-04. See "Stack change".
- The agent column is 540px by default, replacing the 280px column. The window opens at 1400x880 with a 960x600 minimum. The 48px top row is the title bar: traffic lights, the wordmark, the settings gear, and New agent on the column side, the terminal header on the other. Ready cards show a diff stat. Decided by the author 2026-10-04, following the designer's v3 prototype. The headline and status chips from that prototype were removed 2026-10-05 (author decision 9).
- The agent column can be resized and hidden, so the author can give one agent the whole window. Dragging its right edge sets the width from 320 to 800px; the terminal keeps at least 420px, so a narrow window shows less than the saved width without changing it. Double-clicking the edge restores 540px. A drag stops at the minimum width and never hides the column; only Cmd+B, the View menu, and a column icon button (in the column's top row, or after the traffic lights when hidden) hide and show it, so it cannot be lost by accident. Hiding moves no focus: from the cards, `j` / `k` still change the agent on screen. Width and hidden state are saved in `settings.json` as `column`. It is still one terminal on screen, with no other splits. Decided by the author 2026-10-06; supersedes "fixed" and "no drag handle" below and in the PRD.
- Each project has an optional base branch, the branch New starts from, for repos where work happens on a branch such as `dev` and merges to `main` later. Unset, New starts from the remote default branch. The project header shows it, and `b` or a click on it opens the Base branch dialog. A card measures its diff stat and its close checks against the base it started from, so changing the base later does not touch running cards. Decided by the author 2026-10-05. See "Worktree".
- A Settings dialog (Cmd-,) sets background opacity, blur radius, whether translucency covers the sidebar alone or the sidebar and terminal, the terminal font size (8 to 32, default 14), and whether a notification plays the system alert sound (on by default). The title bar uses that same opacity, so the window blur shows through it. Saved in `settings.json`. The default is opaque. Decided 2026-10-04.
- Settings gets themes. A Theme row (System, Light, Dark) and a Light theme and a Dark theme pick from the catalog in `shika-terminal`. System follows macOS and uses the matching pick; Light and Dark force that side, and set `NSApp.appearance` so the traffic lights, menus, and `window.appearance()` match. Saved in `settings.json` as `theme` (`mode`, `light`, `dark`, ids as strings); missing or unknown values are System, `shika-light`, and `shika-dark`, and an unknown or wrong-side id paints that side's Shika theme without being rewritten. Shika Light and Shika Dark keep their hand-tuned chrome. Every other theme's chrome is derived from its palette and UI surfaces, with Shika's layout and hierarchy, secondary and status text meeting 4.5:1 on the column, and solid popups. `h` / `l` step a pick through that side of the catalog, wrapping, and apply and save at once. Decided by the author 2026-10-07.
- `j` / `k` and the arrows move through cards only, including cards hidden by the three-card cap, and skip project headers. An empty project is not a keyboard stop. Cmd+N opens the picker for the selected card's project, or the first project when nothing is selected, and Tab in the picker changes the project. `b` follows the selected card. An empty project's base branch is the click on its header label. Decided by the author 2026-10-05. Plain `n` no longer opens the picker; see the 2026-10-07 decision.
- New and Close are Command shortcuts from either the cards or a terminal. Cmd+N opens the picker. Cmd+Shift+W closes the selected task through the existing safe-close flow, including cancelling setup and removing a failed card. Plain `n` and `c` do nothing, so a letter typed on the cards cannot open New or destroy a task. `j` / `k`, Enter, `a`, `b`, and `r` stay card-only. A clean idle task still closes immediately. Decided by the author 2026-10-07.

## Confirmed PR publishing

Later author decision: Create PR stages all non-ignored task changes, commits if needed, pushes, and creates a PR through the user's authenticated GitHub CLI, after a preview and explicit confirmation. The target is the task's recorded starting base, not the project's current base or GitHub's default. An unmappable/deleted base requires explicit target selection. The editable task title supplies the commit/PR title; the description comes from commits through `gh --fill`, with no model call. No merge, force-push, automatic tests, or task cleanup. Completed steps survive failures and retries reuse an existing open PR. Close remains unchanged and never commits. This supersedes the blanket exclusions of PR creation and committing on behalf of the user below and in `PRD.md`, only for this confirmed flow.

Create PR appears beside the terminal metadata, in the Agent menu, and on Cmd+Shift+P. It is guarded against busy/overlay conflicts and active/blocked turns. Preview preserves the real index and confirmation refuses a changed tree, HEAD, branch, or origin. See `docs/publishing.md` for lifecycle, GitHub assumptions, retry semantics, code map, and guardrails.

## Task-scoped terminal tabs

Later author decision: replace the fixed Agent/Shell control with optional terminal tabs per task. New creates only the pinned CLI tab. `+` or Cmd+T creates an independent login shell in that task's worktree; multiple shells support tools such as Neovim, lazygit, and dev servers. Shell tabs have stable numbered labels and individual close controls. Closing a shell stops its processes without closing the agent, changing git, or deleting the worktree. The agent tab cannot be individually closed; Close task keeps the existing safe-close flow and stops every owned PTY.

Each card retains its selected tab. Hidden terminals keep draining; only one terminal is visible. Tabs are memory only, with no global tabs, splits, session restoration, or built-in editor. Cmd+W closes the selected shell and does nothing on the agent, so it never starts Close task. Ctrl+Tab / Ctrl+Shift+Tab cycle tabs and wrap. Cmd+1 through Cmd+9 jump to a tab, with the agent on Cmd+1, and do nothing when that tab is absent. Existing Cmd+] / Cmd+[ still navigate tasks; Cmd+N still creates an agent; Cmd+Shift+W closes the task. Dirty Close cancellation keeps an already-selected shell or opens/selects the first shell. New shell typing queues before binding, and startup completion does not reclaim focus.

The 48px terminal title row contains connected tabs (the active tab opens into the terminal), `+` after the last tab, and Close task. A metadata row beneath it, on the terminal background, carries the worktree path and focus hint. Existing theme, spacing, and type tokens are reused. See `docs/terminal-tabs.md` for rationale, architecture, lifecycle, debugging, and extension guardrails; `docs/keyboard-flow.md` for cross-surface keyboard behavior; and `MANUAL_CHECKS.md` for validation.

## Agent activity and turn timer

Later author decision: keep the real CLI and use Herdr-style hybrid detection, not a conversation frontend. Reliable lifecycle reports take precedence; otherwise read agent-specific working/idle/permission chrome from the live terminal screen, independent of scrollback. Pi gets a Shika-owned, session-local extension with metadata-only lifecycle reports; other CLIs currently use screen detection. Do not install global hooks or modify repository configuration. Unknown screen output may settle an already-started turn after quiet, but bytes alone cannot start one. Ordinary text questions at the end of a response may be Ready to check; only a recognizable blocking dialog becomes Asking you.

Waiting is initial idle. Working is a running turn. Asking you is a visible permission/question blocker; Ready to check is a finished turn or process exit. Confirm ambiguous idle transitions to avoid partial redraw flicker. Each turn notifies once, on its first blocker or completion. A finished result still becomes unseen after a blocker was seen. Active and blocked turns both require safe-close confirmation.

The timer measures elapsed turn time, not time since a status change or model compute time. It includes blocked time and is shown only while Working. Drafts, focus, clicks, scrolling, resize, Enter during an active turn, and transient Ready/Working flicker never reset its start. A nonempty submitted line from initial idle or Ready is a candidate new turn; bracketed paste newlines and terminal protocol reports are not submissions. Naming is independent of activity. Screen rules are best effort and version-sensitive, not evidence that every real-provider GUI check passed. See `docs/agent-activity.md` and `MANUAL_CHECKS.md`.

## Optional worktree preparation

Later author decision: keep fresh task worktrees and add opt-in preparation, rather than pooling or compiler-cache work. Repository configuration lives at `.shika/worktrees.json`, with ordered `setup-worktree` commands, selected ignored `copy-files`, and an optional `timeout-seconds` (default 600). No inferred package manager, automatic build, shared mutable dependency directory, or automatic execution of another app's configuration.

Adding a project does not run code. New requires locally persisted approval for the parsed configuration; changes ask again. Copy and setup finish before the agent PTY opens. Stages and output use the existing terminal, input is suppressed during setup, and cards offer cancel or fresh-worktree retry. Two preparations can run concurrently without holding the core operation lock. Failed untouched tasks are removed; changed or unverifiable work remains journaled for explicit leftovers cleanup. Quit and project removal cancel setup while retaining worktrees. Existing branch, CLI flags, shell, notification, and safe-close rules remain unchanged.

See `docs/worktree-preparation.md` for the decision rationale and benchmark caveats, configuration, launch/cleanup lifecycle, trust boundaries, symbol map, debugging fixture, and extension guardrails. `MANUAL_CHECKS.md` records acceptance evidence, not a guarantee that every GUI case was tested.

## Stack change, 2026-10-04

Shika moves from Tauri 2, React, and xterm.js to a pure Rust app on GPUI, Zed's GPU UI framework. Everything, including the UI, is Rust.

Why: the product is terminals and keyboard. Its users come from terminal multiplexers like Herdr and dmux and expect terminal speed. A web view gives too narrow a surface for precise keyboard and IME handling, is a poor host for xterm's WebGL renderer, and makes every PTY byte cross an IPC bridge into JavaScript. GPUI draws on the GPU with Metal, and the same Rust process that reads the PTY parses and paints it. Comparable agent apps already ship on GPUI: Paneflow, Arbor, Ghostex, Codux, herdr-gpui. The code is small today, about 2,100 lines of TypeScript and 2,650 of Rust, so the switch is cheapest now.

Only the stack changes. Every product behavior in this file and in `PRD.md` stays the same.

The Tauri build served as the reference implementation of PRD checklist items 1 to 7 during the GPUI port. It has now been removed.

### Migration order

1. **Terminal proof.** A GPUI window with real terminals on `alacritty_terminal` and `portable-pty`. Claude Code's and Cursor's TUIs are usable. Check typing latency, IME, selection and copy, bracketed paste, scrollback, mouse reporting and the wheel, truecolor, resize, a flood of output such as `seq 1 2000000`, and two or more terminals where the hidden ones keep draining their PTY so the CLI never stalls.
2. **Core.** The Tauri-free backend in `crates/shika-core`, with the existing Rust tests ported.
3. **App.** Port PRD checklist items 1 to 7, everything the Tauri build already does, to GPUI in `crates/shika`, matching `design/`. Then the Dock-launch check from a built `.app` opened with `open`.
4. **Delete the Tauri and web code.** `src/`, `src-tauri/`, `index.html`, `vite.config.ts`, `tsconfig*.json`, `package.json`, `package-lock.json`, `node_modules`, `dist`.
5. **Continue** with PRD checklist items 8 to 10 and the "Done when" script, unchanged in intent.

Steps 1 and 2 run in parallel.

### Implementation record, 2026-10-04

The Cargo workspace now contains the core, terminal, and GPUI app. The app implements the grouped cards, native picker, keyboard controls, shell toggle, branch naming, quiet status timer, native notifications with click routing, safe close choices, and leftovers cleanup. The former Tauri and web app and its build dependencies have been removed. The GPUI commit pin and author decisions above are unchanged.

`scripts/bundle-app.sh --debug` produces `target/debug/Shika.app` with the bundle identifier, icon, and JetBrains Mono plus OFL. The built app has been opened with isolated test data and finds both real CLI binaries. The "Done when" flow passed using isolated app data, two disposable repositories, local bare remotes, and the real authenticated CLIs. Native notification delivery and a user click were verified. Acceptance evidence and the separate terminal feel and IME checks are recorded in `MANUAL_CHECKS.md` and `crates/shika-terminal/MANUAL_CHECKS.md`. Final validation passed 108 workspace tests, formatting, strict Clippy, and bundle signature verification.

## How a new task is named

Pressing New creates the worktree before the prompt exists, because the CLI has to open in that folder so you can type there and switch away before pressing Enter.

The folder starts as `<repo>/.worktrees/shika-draft-<id>` on branch `shika-draft-<id>`. The first line you submit in that terminal becomes the card title, and Shika renames the git branch to a slug of that line with `git branch -m`. The folder stays where it is. The agent is already running inside it, so moving the directory out from under the process is not reliable. The path is only shown in the terminal header.

Then the CLI's own session title replaces both, once (decided 2026-10-05). Claude Code, Codex, and Cursor CLI each name their session with their own model call about a second after the first prompt, and save it on disk. Pi does not. It only stores a name when the user runs `/name`, the process was started with `--name`, or an extension sets one. Shika reads that title, so it sends no extra request:

- Claude Code: the last `{"type":"ai-title","aiTitle":...}` line in `<config>/projects/<cwd>/<session>.jsonl`. `<config>` is `CLAUDE_CONFIG_DIR` or `~/.claude`, and `<cwd>` is the worktree path with every character other than an ASCII letter or digit replaced by `-`.
- Codex: `name` in the highest `$CODEX_HOME` or `~/.codex/state_<n>.sqlite`, table `threads`, for the row whose `cwd` is the worktree. `title` in that table is often the raw first message, so it is not read. `name` stays empty until Codex names the thread.
- Cursor CLI: `title` in `~/.cursor/chats/<md5 of the cwd>/<agent>/meta.json`, whose `cwd` must equal the worktree. It is null until the chat is named.
- Pi: the last `{"type":"session_info","name":...}` line in the newest session file under `PI_CODING_AGENT_SESSION_DIR`, or `<PI_CODING_AGENT_DIR or ~/.pi/agent>/sessions/--<path>--/`. `<path>` is the worktree path with its leading separator removed and `/`, `\`, and `:` replaced by `-`. No `session_info` name means the prompt name stays.

These are private files, not a public API. Shika only reads them. Anything missing, unreadable, or unexpected means no title, and the card keeps its prompt name. Starting at the first submitted line, the app checks every two seconds, one check at a time, for up to two minutes.

Branch names are lowercase ASCII words joined by `-`, at most 48 characters, cut at a word boundary. Whole-word copies of the project name are left out ("Shika background opacity and blur" in `shika` becomes `background-opacity-and-blur`). The card shows the CLI's title as written. A name already taken by a local or remote-tracking branch gets `-2`, `-3`, and so on. Shika never renames a branch that is on a remote (it has an upstream, or a remote-tracking branch has its name), or a worktree switched to another branch; the card still takes the title. Renaming before a push is local only and changes no commits.

The optional branch prefix in Settings (empty by default, like `dev/`) goes in front of every name Shika picks. It is cleaned for git, gets a `/` unless it ends in `-` or `_`, and is dropped if git still refuses the name.

You do not name the task in a separate field.

## Author decisions

1. **Push does not finish the task.** A successful `git push` in Shika's shell leaves the card, the session, and the worktree in place. The user closes the card when they want it gone. A push typed inside the agent CLI is also ignored.
2. **Close is a choice when work would be lost.** If the agent is still working, the worktree has uncommitted changes, or the branch has commits that are not on the remote, Shika asks:
   - **Discard changes** stops the agent, deletes the worktree, and deletes the local branch. Uncommitted files and unpushed commits go away.
   - **Push changes** runs `git push -u origin HEAD` only when the worktree is clean and there is something to push. On success, Shika then removes the card and the worktree and leaves the local branch, because that branch is now on the remote. If the push fails, the card stays and the error is shown.
   - If the worktree is dirty, Push is not offered. Close does not commit. Escape leaves the card and focuses the shell so the user can commit and push, then close again.
   - Escape always cancels.
   **Branch-switch recovery (later decision).** A switch is not a rename and never transfers task branch ownership. On a mismatch, Close offers a separate confirmation only when the recorded and current local branches both exist, HEAD is attached, the tree is clean, and neither branch has commits outside the accepted base and local remote-tracking refs. A local base equal to the checked branch cannot prove integration. Confirmation is bound to both names and commit tips, rechecked before and after stopping all task PTYs. Remove only the worktree/card/journal entry, non-forced; preserve both local branches and all remote refs. Unsafe or unverifiable recovery stays blocked, with no switched-task Push or Discard action. The user can resolve work in the shell or return to the task branch for existing choices. This does not fetch or consult PR APIs. `docs/branch-switch-close.md` records the implementation and safety limits.

3. **Nothing to lose closes immediately.** If the agent is not working, the worktree is clean, and the branch is already on the remote or has no commits of its own, Close removes the card and the worktree without asking. A pushed branch stays. An empty draft branch is deleted.
4. **Projects persist. Live sessions do not restore.** Relaunch shows the project list and an empty terminal. A journal of Shika worktrees is kept so a quit or crash can list leftovers. Nothing is deleted automatically. The user removes leftovers from that list.
5. **One terminal view per live PTY, hidden when not selected.** Switching cards does not kill processes. Output keeps flowing into the hidden view so the CLI does not block on a full PTY buffer. Shell PTYs are created only on request, then kept until their individual tab or the task closes.
6. **Status stays coarse.** The later Agent activity and turn timer decision above supersedes output-only quiet detection. Prefer lifecycle reports and live agent UI chrome; quiet output is only a fallback within a started turn. Drafts and terminal interactions are not turns. Ordinary text questions can remain Ready to check, recognizable blocking dialogs are Asking you, and a non-zero exit is still Ready to check.
7. **Fonts.** The system UI font, San Francisco, for the chrome. JetBrains Mono, bundled with the app under its OFL license, for the terminal, as in `design/`. Menlo if it fails to load. SF Mono is out: GPUI loads only its regular weight. Shika Light and Shika Dark follow the system appearance, and the terminal shares the column's background in both (light chrome with a dark terminal until 2026-10-07). Warm mark for asking, green mark for ready. A resizable, hideable column, 540px by default (280px until 2026-10-04; fixed until 2026-10-06), at most three visible cards per project.
8. **Extra keyboard keys the PRD table does not list, because the app has to work without a mouse.** `a` adds a project. Cmd+N opens the CLI picker from the cards or a terminal. Cmd+Shift+W closes the selected task from either surface. Plain `n` and `c` do nothing. In the picker, `j` / `k`, Enter, and `1`–`4` choose, Escape cancels. In the close dialog, `d` discards, `p` pushes when that action is available, Escape cancels. The 2026-10-07 decision supersedes the PRD rows for `n` and `c`.
9. **No summary at the top of the column, 2026-10-05.** The headline ("2 agents across 1 project") and the per-status count chips added nothing the cards do not already show. Status lives at the right end of each card: moving pixels while Working, a grey dot while Waiting, and a green dot until a Ready result has been seen.

## CLIs in this build

Checked from each binary's `--help`. Claude Code and Cursor CLI on 2026-10-03. Codex CLI 0.160.0 and Pi 1.0.0 on 2026-10-05. Do not invent a flag. Do not pass `agent --worktree` or `codex --worktree`.

| Preset | Binary | Launch args |
| --- | --- | --- |
| Claude Code | `claude` | `--dangerously-skip-permissions` |
| Codex | `codex` | `--dangerously-bypass-approvals-and-sandbox` |
| Cursor CLI | `agent` | `--yolo --trust --sandbox disabled` |
| Pi | `pi` | `--approve` |

Claude Code and Cursor CLI are installed under `~/.local/bin`. Codex is there too. Pi is on the Node path (`pi` from `@earendil-works/pi-coding-agent`). Pi does not ask before a tool call. `--approve` skips the project-trust prompt on a new worktree. Kiro is not in this build.

Rust is installed with rustup for this machine. `~/.zshenv` is a Nix store symlink, so rustup must not try to edit it. The toolchain is on `PATH` after `source "$HOME/.cargo/env"`. GPUI needs full Xcode, not only the Command Line Tools, for the Metal shader compiler. It is installed. The GPUI app needs no Node. The Tauri and web build files have been removed. The current platform dependency enables `runtime_shaders` because the separate Xcode Metal Toolchain component is not installed. The app must resolve `claude`, `codex`, `agent`, and `pi` through a login shell, or a Finder launch will not see `~/.local/bin` or the Node path.

## Stack

- Pure Rust, one Cargo workspace, one window. Mac only.
- UI: GPUI from the `zed-industries/zed` git repo, pinned to one commit `rev`. Not crates.io `gpui` 0.2.2, which is stale, and not the `gpui-ce` fork. Plain GPUI first. Add `gpui-component` only if it saves real work later.
- Terminal engine: `alacritty_terminal`, wrapped behind Shika's own types. Alacritty types never leave one module, so a later swap to libghostty-vt stays cheap.
- `portable-pty` for the agent and the shell.
- GPUI's native path prompt (`cx.prompt_for_paths`, or the equivalent at the pinned `rev`) for the folder picker.
- Native macOS notifications use Apple's `UNUserNotificationCenter` through `objc2-user-notifications`, including delegate callbacks for card selection.
- No database, no account, no extra crates for status parsing.
- No Node, npm, Vite, TypeScript app, or web view. HTML in `design/` remains a visual reference.

Licensing: never copy from Zed's `terminal` or `terminal_view` crates. They are GPL-3.0. Only `gpui` is Apache-2.0. `alacritty_terminal` is Apache-2.0.

Confirm GPUI names against the pinned `rev`, not against docs for another version.

## Shape of the app

```
Cargo.toml                    workspace
crates/shika-core/            no UI code, no GPUI
  projects                    load/save projects.json
  worktree                    exclude, add, rename branch, remove, dirty and unpushed checks, worktrees.json
  path_env                    login-shell PATH and absolute CLI paths
  agents                      Claude Code, Codex, Cursor CLI, and Pi
  pty                         spawn, write, resize, read, exit, child env
  session                     create, shell, git state, discard, push and close, close, leftovers
crates/shika-terminal/        alacritty_terminal wrapper and the GPUI terminal view
                              PTY bytes in; encoded input bytes and resizes out. Spawns no process.
crates/shika/                 the GPUI app
                              window, project headers, cards, picker, close dialog, toast,
                              keyboard map, shell toggle, .app bundle
design/                       DESIGN.md, Shika v3.dc.html, logo artwork
```

Persisted in `~/Library/Application Support/com.hieule.shika/`, the same directory the Tauri build used, so existing files keep working:

- `projects.json` - `{ id, name, path, baseBranch?, approvedPreparation? }[]`. Name is the folder name. `baseBranch` is the short name the user typed, such as `dev`; missing means the remote default. `approvedPreparation` is local consent for the parsed `.shika/worktrees.json` configuration, never repository-provided consent.
- `worktrees.json` - journal of `{ projectId, branch, path, baseRef? }` for crash cleanup. `baseRef` is the ref the branch started from. Not a session history.
- `settings.json` - `{ appearance: { opacity, blur, translucency }, fontSize, branchPrefix, notificationSound }`. Opacity 0 to 100 percent, blur radius 0 to 255, translucency `sidebar` or `sidebarAndTerminal`. `notificationSound` plays the system alert with the banner and defaults to true.

In memory only: session id, CLI preset, card title, status, both PTY ids, whether the first prompt has been sent.

Operations `shika-core` gives the app:

- `projects_list`, `project_add`, `project_remove`
- `cli_presets`: absolute path or "not found", plus the argv
- `session_create`: worktree and agent PTY
- `pty_write`, `pty_resize`
- `session_rename_from_prompt`: slug, unique branch, `git branch -m`
- `open_shell`: a new independent shell PTY in the task worktree on every call
- `close_shell`: stop one owned shell without closing the agent or task
- `session_git_state`: dirty, unpushed, agent still working
- `session_discard`: kill PTYs, force-remove worktree, delete local branch
- `session_push_and_close`: `git push -u origin HEAD`, then remove the worktree and keep the branch
- `session_close`: remove the card and worktree when there is nothing to lose
- `leftovers_list`, `leftover_remove`

PTY output, PTY exit, and status changes reach the app as events inside the process. There is no IPC bridge.

### PATH

On startup, run the user's login shell once:

```sh
"$SHELL" -ilc 'printf %s "$PATH"'
```

Put that PATH on every child. Resolve `claude` and `agent` inside that same environment and store absolute paths. Also put the resolved PATH on the PTY environment so the CLI can find `git`, `node`, and itself. `cargo run` from a terminal inherits a terminal PATH and hides this bug. The real test is opening the built `.app` from Finder or `open`.

### Worktree

- Project path must be a git repo (`git rev-parse --show-toplevel`). If they pick a nested folder, use the toplevel and say so.
- Append `.worktrees/` to `$(git rev-parse --git-path info/exclude)` if the line is missing. Do not edit `.gitignore`.
- Create with `git worktree add --no-track -b <branch> <path> <start>` from the main repo (decided by the author 2026-10-05, replacing a plain `git worktree add -b <branch> <path>`, which started from whatever the main checkout had out).
  - `<start>` for a configured base `B` is `refs/remotes/origin/B`, else `refs/heads/B`. If neither exists, New fails with "Base branch B not found." It never falls back to another branch.
  - With no base set, `<start>` is the remote default (`refs/remotes/origin/HEAD` when it is valid), then local `main`, then `master`, then the main checkout's HEAD as the last resort.
  - `--no-track` is required. Starting from `origin/dev` would otherwise make `origin/dev` the upstream, so a fresh card would look pushed and a plain `git push` would target dev.
  - Before creating, fetch only that branch from origin (`+refs/heads/B:refs/remotes/origin/B`), best effort: no terminal prompt, no askpass, ssh in batch mode, a 4 second cap. The fetch starts when the picker opens, and New waits for it rather than starting another. On any failure or timeout, New uses the ref it already has.
  - The session keeps the full ref it started from, in memory and as `baseRef` in the journal. Diff stat, own commits, and unpushed-without-upstream compare against it. If it no longer resolves, or there is none (the HEAD fallback), they use the default branch: `origin/HEAD`, `main`, `master`, then the main checkout's HEAD, refusing when that checkout is on the task branch.
  - The Base branch dialog lists branches already on this machine, local heads and origin, and filters them as the field is typed. Up and Down move the highlight. Enter saves the highlighted branch. A typed name that is not listed is fetched from origin first, and is saved only when that fetch finds it. `origin/dev` is saved as `dev`. Empty clears it.
- Slug: lowercase, non-alphanumerics to `-`, collapse dashes, trim, max 48 characters. Empty slug becomes `task-<id>`. Collision gets `-2`, `-3`.
- Dirty means `git status --porcelain` is non-empty.
- Unpushed means the branch has commits that are not on its upstream. A branch with no upstream is unpushed when it has commits that are not in the base it started from (else the default branch) or on any remote-tracking branch.
- Discard uses `git worktree remove --force` and then `git branch -D`.
- A normal close of a clean, already-pushed task uses `git worktree remove` and leaves the branch.
- A normal close of an empty draft uses `git worktree remove` and `git branch -D`.

### Shell

A shell tab is the user's login shell with its cwd set to the worktree. No command hook and no push watcher. `git status` and `git diff` in that shell show this task's files. The user commits and pushes there themselves.

### Status and notifications

- Card title before the first Enter: `New Claude Code` or `New Cursor CLI`. Status: Waiting.
- First Enter submits whatever line was buffered from keystrokes. Then Working, and the title is that line, shortened to about 80 characters.
- Quiet for ~2s after output has arrived: Ready to check, and a notification named `{project} - {task}`. The banner plays the system alert sound unless Settings turns that off.
- Process exit: Ready to check, same notification if one was not just posted.
- One notification per turn (author, 2026-10-04). A turn starts when the user types or pastes into the agent. Reading the result, focusing, scrolling, or typing a draft must not notify again.
- Asking you, if implemented: only flip it when the tail of recent output clearly looks like a question, and post the same kind of notification. Otherwise leave the status at Ready to check. The user reads the real terminal either way.
- Clicking the notification focuses the window and selects that card.

### Keyboard and focus

Focus starts on the cards. `j` / `k` and arrows move through every card, including cards hidden by the three-card cap, and skip project headers. The three visible cards follow the selection. A project with no agents is still a row, but it is not a keyboard stop. Cmd+N opens the picker for the selected card's project, or the first project when nothing is selected, and Tab in the picker changes the project. `b` follows the selected card. An empty project's base branch is the click on its header label.

`Enter` focuses the terminal. `Ctrl+Q` returns to the cards. `Escape` is typed into the terminal. Cmd+Shift+W closes the selected task from the cards or a terminal. Plain `c` does nothing.

Ignore the plain-key map while the terminal is focused, except `Ctrl+Q`. Also ignore it while a text field is focused, and while the picker or close dialog is open.

The native shortcut layer works from cards or a terminal: `Ctrl+Tab` / `Ctrl+Shift+Tab` cycle the selected task's tabs, `Cmd+1` through `Cmd+9` jump to a tab (`Cmd+1` is the agent), `Cmd+]` / `Cmd+[` select the next/previous agent in current row order (skip project headers, wrap, and preserve terminal versus card focus), `Cmd+N` opens New for the current project, and `Cmd+Shift+W` closes the selected task. `Cmd+W` closes a shell and does nothing on the agent. These actions are blocked while busy or while any overlay is open. The Agent menu exposes task and terminal-tab actions; New, new shell, and shell close have tooltips. `j` / `k` uses that same card-only order. Picker cancel, Settings dismissal, and Base branch completion/cancel restore the previous focus. Close cancel keeps its shell-routing rule. Selection changes reveal the selected row with minimal scrolling; ordinary redraws do not override manual scrolling. Shell focus moves immediately to the new view, which queues typeahead during startup; completion does not reclaim focus if the user left.

### Layout

- Left column, 540px by default, resizable and hideable (see the decisions at the top). Every project. Under each, up to three cards. Further cards show as a count and stay reachable from the keyboard.
- A card shows the CLI name, the task name, and the status. The worktree path is only in the terminal header.
- Right side: the selected agent's terminal, or empty if a project header is selected or there are no sessions. At most one terminal on screen.
- Header controls: pinned CLI tab, optional shell tabs with individual close controls, new shell `+`, Close task. Worktree path and focus hint sit immediately below.
- Top of the column: the title bar, then the projects. No headline and no status chips; each card shows its own status.
- No kanban, no flat session list, no project tabs, no divider drag, no second terminal, no editor.

## Build order

Do not skip ahead of a failed proof. Each step is done only when its check passes.

The GPUI port must pass the same checks. The migration order above says when.

### 0. Toolchain and scaffold

Install Rust with rustup and full Xcode for GPUI. Create the Cargo workspace and the GPUI app in `crates/shika`. Bundle identifier `com.hieule.shika`. Window opens on Mac.

Check: `cargo run -p shika` shows an empty split window.

### 1. Projects

Folder picker, git toplevel check, persist `projects.json`, render the left column. A project with no agents still shows. Restart keeps the list. `a` and a button both add.

Check: add two repos, quit, relaunch, both are there.

### 2. PATH and presets

Login-shell PATH. Claude Code, Codex, Cursor CLI, and Pi, with absolute paths. A missing binary stays in the picker and cannot be launched.

Check: from a built app opened with `open` (not `cargo run`), the installed ones resolve. This is the Dock-launch trap. Do it here, before believing any later CLI test.

### 3. Worktree, PTY, and terminal view

This is the risk. Prove it before cards get fancy.

Create the draft worktree, write `.worktrees/` into info/exclude, spawn the preset with the argv above and the login PATH, bind a terminal view to that PTY. Keep the terminal view alive when switching cards. Typing and the mouse scroll work. The CLI's own UI is usable, including a question the user can answer.

Launch Claude with `--dangerously-skip-permissions`, Codex with `--dangerously-bypass-approvals-and-sandbox`, Cursor with `--yolo --trust --sandbox disabled`, and Pi with `--approve`. Do not pass `--worktree`. Confirm none of them asks Shika to approve a shell command. If Claude still shows a workspace trust prompt, check `claude --help` again for an existing flag before adding anything.

Check, in the built app opened from Finder:

- New on a real repo opens Claude in the new worktree. A second card can open Cursor the same way.
- A prompt runs, tools run, and a question can be answered in the embedded terminal.
- Switch to the other card and back. The draft or the running session is intact.
- `.worktrees/` is not showing up in `git status` on the main checkout.
- Cursor did not create a worktree under `~/.cursor/worktrees`. Codex did not create one either. Shika's worktree is the one in use.

If the embedded terminal cannot drive Claude's or Cursor's UI, fix that before continuing. The rest of the MVP depends on it.

### 4. Card title, status, collapse

First Enter renames the branch and the card. Counts line. Status colors. More than three cards collapse, keyboard still reaches them. Working view does not grow a custom transcript.

Check: two agents under one project show the right titles and statuses. A third and fourth collapse. An empty project remains visible.

### 5. Keyboard

The map in the PRD, plus `a` and the picker keys. Keys do nothing while typing in the terminal.

Check: add, new, move, focus terminal, ctrl+q back to the cards, close, all without the mouse.

### 6. Shell and close

A shell tab is the user's shell with cwd on the worktree. `git status` and `git diff` show that task. `git push` leaves the card in place.

Close with nothing to lose removes the card and the worktree. Close while the agent is working, the tree is dirty, or commits are unpushed asks: discard, or push when the tree is clean. Discard removes the worktree and the local branch. Push, on success, removes the card and the worktree and keeps the branch. A dirty tree sends the user back to the shell to commit. Close does not commit.

Check: a push leaves the card; a later close of that clean pushed task removes the card and the directory and leaves the branch; discard of a dirty task removes the directory and the branch; the other agent's process is still running; `git worktree list` no longer has the removed path.

### 7. Notifications and leftovers

Notification when a session becomes Ready to check (and Asking you, if that flip exists). Click focuses Shika and selects the card. Next launch lists journaled worktrees whose sessions are gone, and can remove them on request.

Check: start an agent, hide Shika, get the notification, click it, land on that card. Kill the app mid-task, relaunch, see the leftover, remove it.

### 8. Done when

On this Mac, with Claude Code and Cursor CLI already logged in:

1. Add two real repos.
2. Start Claude in one and Cursor in the other, on separate tasks, without typing a git command. Cursor must be using Shika's worktree.
3. Leave both running, use the browser, and get a notification when one is ready or asking.
4. Answer in the agent's terminal.
5. In Shika's shell, run `git status`, `git diff`, commit, and `git push`.
6. The card is still there after the push.
7. Close that card. The work is already pushed and the tree is clean, so the card and the worktree are removed, and the local branch stays. The other agent is still running.
8. Opening the `.app` from Finder still finds both CLIs.

Then stop. Distribution (signed dmg, notarization, Homebrew) is out of scope until this passes.

## Do not build

Phone, web, Windows, Linux, Homebrew, accounts, sync, telemetry, a planner, PR merge/review management, CI, review, worktree reuse, conversation history, more than one visible terminal, a split between terminals, a code editor, installing the CLIs, a custom chat transcript, a per-CLI question parser, Kiro, Codex's or Cursor's own worktree flag, auto-removing a card after `git push`, committing without the explicit Create PR confirmation.
