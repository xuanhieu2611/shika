# Task-scoped terminal tabs

Shika starts each task with one pinned agent terminal. Developers can add independent shell terminals in the same worktree when they need to review code, edit files, or run tools. Tabs belong to a task, not to a global terminal workspace. Only one terminal is visible at a time.

Start here before changing terminal-tab behavior. [PLAN.md](../PLAN.md#task-scoped-terminal-tabs) records the product decision and overrides the older fixed Agent/Shell specification in `PRD.md`. [design/DESIGN.md](../design/DESIGN.md) governs appearance. [keyboard-flow.md](keyboard-flow.md) explains app-wide focus and keyboard routing. [MANUAL_CHECKS.md](../MANUAL_CHECKS.md#task-scoped-terminal-tabs) separates validation evidence from intended behavior.

## Why this changed

The previous UI always showed a CLI/Shell segmented control. Each card had exactly one agent PTY and, on first use, one reusable shell PTY.

Actual usage exposed two needs:

- **Prompt-first work:** start agents, return to test their work, and ask for fixes. An unused Shell destination need not occupy the default interface.
- **Hands-on review:** keep Neovim, lazygit, or a dev server open alongside the agent. One shell forces developers to stop or switch tools unnecessarily.

Optional shells support both workflows without making Shika a general-purpose terminal app. The organizing unit remains the task: Shika already knows which repository, worktree, and branch its terminals belong to. New shell tabs reuse that context; they never create a new worktree or agent.

This is not a safety boundary. Shells and the agent share files, and the agent can modify them while a developer reviews them. Terminal tabs do not add approval gates, sandboxing, PR review, or CI protections.

## Behavior at a glance

```text
               ╭──────╮
New task:      │ Pi   │ +                                   [Close task c]
              ─╯      ╰────────────────────────────────────────────────────
                     ╭─────────╮
With tools:     Pi   │ Shell × │ Shell 2   +                [Close task c]
              ───────╯         ╰───────────────────────────────────────────
               worktree path                                     focus hint
```

- New creates only the pinned CLI tab, labeled with the agent preset name.
- `+` creates a new login shell with its starting cwd in this task's worktree. Users run their own tools there; Shika does not automatically launch an editor or git UI.
- Clicking a tab selects and focuses its terminal. Switching cards preserves each card's selected tab and terminal contents.
- Hidden views remain alive and output continues to drain. Switching tabs does not suspend or restart processes.
- Shell labels are monotonic within a card: Shell, Shell 2, Shell 3. Removing Shell 2 does not rename Shell 3 or reuse its number. Labels are not process titles, and custom renaming is not implemented.
- A shell's `×` closes only that shell. There is no confirmation dialog; the tooltip warns that closing stops its processes. Save work in terminal tools before closing their tabs.
- The agent has no individual close control, including after its process exits. Close task remains the separate task/worktree lifecycle action.
- An exited shell tab remains available for reading its output until explicitly closed. There is no automatic shell restart.
- Live tabs are memory only. Relaunch restores projects, not sessions or terminal tabs. Quit retains worktrees for the existing leftovers flow.

### Controls and focus

| Control | Result |
| --- | --- |
| `+`, Cmd+T | Add and focus a new shell in the selected task |
| Shell `×` | Close that specific shell, whether selected or not |
| Cmd+W | Close the selected shell. On the pinned agent it does nothing, and it does not start Close task |
| Ctrl+Tab / Ctrl+Shift+Tab | Next/previous task-local tab, wrapping; focus the destination |
| Cmd+1 through Cmd+9 | Jump to that tab and focus it. Cmd+1 is the pinned agent. A missing tab does nothing |
| Cmd+] / Cmd+[ | Navigate tasks, not tabs; preserve card versus terminal focus |
| Cmd+N | New agent, not new shell |
| Enter from cards / Ctrl+Q from terminal | Focus the selected tab / return to cards |
| Close task, `c` from cards | Existing safe-close flow for the whole task |

Tab actions are blocked while an overlay is open or the app is busy; adding shells is also blocked while the card is being created/prepared. Ordinary typing and Escape still belong to the terminal program. Command shortcuts, Ctrl+Tab, and Ctrl+Shift+Tab are app actions, not PTY input.

The title row stays 48px high. Tabs are connected tabs, not the old segmented control: the active tab takes the terminal's fill and the header's bottom line breaks under it. `+` follows the last tab, outside the scrolling strip, so it stays visible when tabs overflow; Close task stays at the right. The worktree path and focus hint sit in a row beneath, on the terminal background, as part of the active tab's page. Tabs, `+`, and Close occlude title dragging; empty title-row space, including above the tabs, still drags the window.

Two rendering traps. The header's bottom line is drawn by each piece of the row (a border under every tab wrapper, `+`, the filler, and Close), because a translucent active tab painted over a header-wide line would still show it. And the active tab is painted over the translucent header, so its fill is `Chrome::term_tab`, computed by `appearance::over_to_match` to give exactly the terminal's pixels; filling it with the terminal color directly would stack two translucent fills and look more opaque than the terminal below.

## Architecture and code map

All UI symbols below are in `crates/shika/src/main.rs` unless otherwise noted. Search by symbol rather than line number.

| Layer | Symbols | Responsibility |
| --- | --- | --- |
| Task UI state | `Card`, `Card::active_pane` | Own agent and shell panes; resolve the selected terminal |
| Terminal pane | `Pane`, `Pane::new` | Own one GPUI `TerminalView`, terminal engine, and shared `HostState` per PTY |
| Header rendering | `terminal_side`, `TAB_HEIGHT`, `KeyTip`, `title_drag`; `appearance.rs`: `Chrome::term_tab`, `over_to_match` | Tab strip, shell close/add controls, metadata row, task close, and drag behavior |
| Tab actions | `new_shell`, `select_tab`, `cycle_tab`, `close_tab`, `toggle`, `SelectTerminal` | Shell startup, selection, numbered jump, removal, focus, and the close-cancel shell route |
| Native dispatch | `gpui::actions!`, `Shika::render`, `main` | Root action handlers, Shika key context, key bindings, and Agent menu |
| Focus and scroll | `focus_terminal`, `Card::tab_scroll`, `move_agent` | Focus the selected view and reveal tabs without resetting manual scrolling on every render |
| Selection arithmetic | `crates/shika/src/model.rs`: `adjacent_tab`, `tab_after_close` | GPUI-independent wrap and removal rules |
| Startup bridge | `Host::write`, `Host::resize`, `bind_host` | Queue pre-bind input/replies, measure terminal size, bind/resize PTY, and flush queued bytes |
| Task shell ownership | `crates/shika-core/src/session.rs`: `Session`, `SessionStore` | Record shell PTYs and validate ownership on removal |
| Core lifecycle | `crates/shika-core/src/lib.rs`: `open_shell`, `close_shell`, `hang_up` | Spawn independent shells, close one shell, or stop every task-owned PTY |
| PTY implementation | `crates/shika-core/src/pty.rs`: `PtyHub` | Process/PTY ownership, reader threads, writes, resize, and teardown |
| Terminal keyboard | `crates/shika-terminal/src/view.rs`: `key_for` | Keep Command combinations, Ctrl+Tab, and Ctrl+Shift+Tab out of terminal key encoding |
| Appearance updates | `set_font_size`, `push_terminal_theme` | Update the agent and every shell, including hidden ones |

### State and identity

The UI changed from `shell: Option<Pane>` plus `show_shell: bool` to:

- `shells: Vec<Pane>`: ordered shell panes, separate from `agent`.
- `active_tab: usize`: zero is the pinned agent; shell index `i` is tab `i + 1`.
- `shell_serial`: monotonically increasing display-label counter.
- `Pane::shell_number`: label number assigned when the shell pane is created.
- `tab_scroll`: a per-card GPUI scroll handle for horizontal tab visibility.

Do not confuse these identities. `active_tab` is a mutable position, `shell_number` is a display label, and `PtyId` identifies a core-owned terminal process. A shell labeled Shell 5 can be tab 1 after earlier shells have closed.

Core's memory-only `Session` changed from `shell_pty: Option<PtyId>` to `shell_ptys: Vec<PtyId>`. `Core::open_shell` no longer returns/reuses a singleton shell or returns `ShellOpen`; every successful call returns a fresh `PtyId` attached to the supplied output sink. UI reuse happens by selecting an existing pane, not by calling `open_shell` again.

The authoritative ownership list is in `SessionStore`. A `Session` returned by core is a clone/snapshot; the UI's stored session is not automatically refreshed when shells are added. Do not use that snapshot's `shell_ptys` to decide what core must tear down.

## Lifecycle and implementation traps

### Add a shell

1. `new_shell` checks busy/overlay state, selected card, creation state, and session availability.
2. It constructs `Pane::new(..., capture = false, ...)` with the current font size, palette, and opacity. Shell typing must not capture agent prompts or name branches.
3. It assigns a display number, appends the pane, selects it, and requests horizontal reveal.
4. When invoked with focus, it focuses the new view **before setting `busy`**. `focus_terminal` normally refuses focus transitions while busy.
5. The async path waits for the view's measured terminal size, then calls `Core::open_shell` on a background executor.
6. Core serializes lifecycle operations, resolves the login shell/PATH, spawns the PTY in the worktree, and records ownership. Failure to record ownership closes the newly spawned PTY.
7. Output feeds this pane's terminal engine; exit sets its `HostState::exited`. Shell output does not run the agent's output/status capture path.
8. Completion clears busy and calls `bind_host`. It must **not focus again**: the user may already have pressed Ctrl+Q to leave the terminal.

Before binding, `Host::write` queues bytes in `pending_input`, including typed input and terminal query replies. `bind_host` registers the PTY, applies the latest measured size, and flushes those bytes in order. Delaying initial focus sends early typing to the old terminal; unconditionally focusing on completion steals focus back from the cards.

On startup failure, the UI removes only the new pane, repairs selection, and reports the error. It focuses the preceding tab only if the failed view still had focus. Failed starts may leave gaps in display numbering; the counter is not rolled back.

**Concurrency trap:** the current async startup callback retains card and tab indices. It relies on the existing global busy guard to prevent destructive reordering/removal during that short operation. If adding cancellable or concurrent shell startups, do not merely remove the guard: introduce stable task/tab identity and handle late results, deleted cards, orphan PTYs, and cleanup explicitly. Do not generalize this index-based path into the separately cancellable preparation lifecycle; read [worktree-preparation.md](worktree-preparation.md) before changing setup behavior.

### Select or close a tab

`select_tab` bounds-checks the position, updates `active_tab`, and focuses/reveals its view. `cycle_tab` uses `adjacent_tab`; a task with only its agent stays on tab zero in either direction.

`close_tab` rejects tab zero, invalid positions, overlays, busy state, and unbound panes. It captures the session ID and PTY before removing the pane. `tab_after_close` then repairs selection:

- Removing a tab after the selected tab leaves selection unchanged.
- Removing one before it shifts the selected position left, preserving the same pane.
- Removing the selected tab chooses the preceding tab, ultimately the agent when the last shell closes.

Focus changes only if the removed pane had focus. Closing a background tab does not pull the user out of another terminal or the cards.

PTY teardown runs on the background executor, not the UI thread. `Core::close_shell` validates ownership with `SessionStore::forget_shell` before calling `PtyHub::close`. An unknown task returns `UnknownSession`; a PTY not owned as a shell by that task returns `UnknownPty`. This prevents accidentally closing the pinned agent or another task's shell. The UI currently discards the background close result; if diagnosing teardown problems, inspect core ownership/errors rather than expecting a toast.

### Close a task, cancel Close, or quit

The existing task-close checks, discard/push choices, and worktree/branch cleanup rules are unchanged. `hang_up` now closes the agent and **all** PTYs recorded in that session's `shell_ptys`.

Canceling Close for dirty or unpushed work preserves an already-selected shell. From the agent it selects the first remaining shell, or creates one if none exists, so the user can inspect/commit work. This is deliberate workflow routing, not generic overlay focus restoration.

Quit does not delete worktrees, and terminal tabs are not persisted. Project removal and task cleanup continue through core's existing lifecycle paths. Closing a shell alone never runs git or changes the journal.

## Debugging guide

| Symptom | First places to inspect |
| --- | --- |
| New unexpectedly opens a shell | Card initialization in `launch`; keep `shells` empty and `active_tab` zero |
| Adding a tab reuses a shell or loses output | `Core::open_shell`, output sink ownership, and `SessionStore::remember_shell`; the API must create a new PTY every time |
| Shell opens in the wrong checkout | `session::shell_request`, `Session::worktree`, `path_env`, and PTY git-environment sanitization; not the displayed/truncated path |
| Shell input renames a branch or changes agent status | `capture = false`, agent versus shell output callbacks, and `tick`'s use of agent state |
| Early typing disappears or goes to the agent | Immediate pane installation/focus, `Host::pending_input`, and `bind_host` |
| Startup completion steals focus | Successful completion must bind only; failure must test whether its view is still focused |
| Switching tasks lands on the agent instead of a shell | `Card::active_tab`, `Card::active_pane`, `move_agent`, and `focus_terminal` |
| Closing a tab selects the wrong pane | One-based tab position versus zero-based `shells` index; `tab_after_close`; do not use label number as an index |
| A closed shell survives or another process stops | Captured session/PTY IDs, `forget_shell` ownership check, `close_shell`, and `PtyHub::close`; distinguish normal PTY children from deliberately detached processes |
| Cmd+W closes a task/window, or a shortcut reaches the CLI | Root Shika action context/handlers, native bindings, conflicting terminal bindings, and `key_for` |
| Many tabs hide the active tab or task-close control | `terminal_side` flex/overflow structure and `tab_scroll.scroll_to_item`; `+` and Close must remain outside the scrolling strip |
| Manual tab scrolling snaps back continuously | Reveal on explicit selection/focus changes, not every render or terminal-output event |
| Hidden shells retain old font/colors/opacity | `set_font_size` and `push_terminal_theme` must traverse agent plus every shell |
| The active tab looks lighter or darker than the terminal, or a line shows under it | `Chrome::term_tab`, `over_to_match`, and the per-piece baseline borders; nothing else may paint under the tab |
| Clicking a tab drags the window | Button/tab `.occlude()` and `title_drag`; do not disable title-row dragging globally |

## Validation and contributor guardrails

Run automated validation from the repository root:

```sh
source "$HOME/.cargo/env"
cargo fmt --all --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
./scripts/bundle-app.sh --debug
codesign --verify --deep --strict target/debug/Shika.app
```

Relevant automated coverage:

- `model::tab_tests`: wrap navigation, agent-only navigation, selection after active/inactive/last-shell removal.
- `session::tests::shells_are_independent_and_only_owned_shells_can_be_removed`: multiple shells and agent/cross-task ownership protection.
- `tests::a_session_runs_in_its_worktree_and_close_removes_it` in core: independent PTY IDs, individual close, surviving shell writes, and full task teardown.
- `startup_query_replies_and_typeahead_survive_until_pty_binding` in the app: ordered pre-bind replies/input.
- `view::tests::command_is_left_to_the_app` and `control_tab_is_left_to_the_app` in terminal: tab shortcuts, including Ctrl+Tab, remain excluded from PTY key encoding.

Unit tests do not establish native shortcut dispatch, rendered overflow, focus restoration, or actual Neovim/lazygit behavior. Use the dedicated [manual checklist](../MANUAL_CHECKS.md#task-scoped-terminal-tabs), including light/dark, translucency, narrow windows, multiple tasks/shells, exited shells, startup failure, and safe Close cancellation. Keep passed evidence distinct from pending checks.

For GUI testing, use disposable git repositories, local bare remotes, and an isolated data directory:

```sh
open -n target/debug/Shika.app --args --data-dir /absolute/path/to/disposable/data
```

Never test against the author's normal app data. Before synthetic input, verify the **isolated process's PID and window** are frontmost, not just that an app named Shika is active. Multiple Shika instances can coexist. See [AGENTS.md](../AGENTS.md#toolchain) for Dock-launch PATH and toolchain traps.

When extending the feature:

- Preserve task-local ownership, one visible terminal, and the pinned agent. Global tabs, splits, built-in editors, and session restoration require an explicit product decision.
- Keep PTY types/process creation in core and terminal-engine types inside `shika-terminal`; do not add UI dependencies to core.
- Preserve hidden-pane lifetime, output draining, startup input ordering, and focus rules.
- Keep shell typing/output separate from agent prompt capture, naming, readiness, and notifications.
- Guard every entry point, including menus and clicks, against overlay/busy conflicts. Revisit identity and cleanup before relaxing startup serialization.
- If adding custom labels or command-derived titles, keep display metadata separate from positional selection and PTY ownership. Do not silently launch or install tools.
- Reuse the existing actions, `KeyTip`, and design tokens. Extend the hidden-pane appearance traversal when adding terminal kinds.
- Update this document, `PLAN.md`, `AGENTS.md`, `design/DESIGN.md`, relevant keyboard docs, and acceptance checks when behavior changes. Record validation honestly.
