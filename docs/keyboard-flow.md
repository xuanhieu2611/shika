# Keyboard flow

How Shika lets users move between tasks without leaving their working context, why the shortcuts use Command, and where to look when focus or navigation goes wrong.

Start here for keyboard-related contributions. [PLAN.md](../PLAN.md#keyboard-and-focus) holds the behavior rules; [design/DESIGN.md](../design/DESIGN.md) holds the visual and focus specification. This document explains the implementation, not a separate spec. [MANUAL_CHECKS.md](../MANUAL_CHECKS.md#command-keyboard-flow) tracks GUI validation.

## What changed

Previously, working in a terminal meant returning to the cards for most app actions:

- Switch to shell: `Ctrl+Q`, `g`, `Enter`.
- Switch agents: `Ctrl+Q`, `j`/`k`, `Enter`, potentially passing project headers.
- Create another agent: `Ctrl+Q`, `n`, choose a CLI.

The card-navigation model is still available. A small native Command shortcut layer now makes these transitions directly:

| Shortcut | Action | Focus afterward |
| --- | --- | --- |
| `Cmd+Enter` | Toggle the selected task's agent/shell | Destination terminal, even when invoked from the cards |
| `Cmd+]` | Next agent in current row order | Terminal if invoked from a terminal; cards if invoked from cards |
| `Cmd+[` | Previous agent in current row order | Same preservation rule |
| `Cmd+N` | Open New for the current selection's project | Picker; cancel restores previous focus; successful launch focuses the new agent |

Next/previous agent skips project headers, includes collapsed cards, crosses projects, and wraps. Each task keeps its own agent/shell choice and terminal contents. With no valid selection, next chooses the first agent and previous the last. With no agents, navigation does nothing. With one agent, it stays selected.

These actions do nothing while the app is busy or any overlay is open. `Cmd+N` uses the first project when nothing is selected and opens Add project when there are no projects.

Other improvements:

- Picker cancellation, Settings dismissal, and Base branch completion/cancellation restore the focus that opened the overlay.
- A selection change scrolls the selected header/card into view with minimal movement. Ordinary redraws do not undo manual scrolling.
- First shell open focuses the new view immediately. Early typing is queued until the PTY is bound; startup completion does not reclaim focus if the user has returned to the cards.
- The native Agent menu lists the new actions and shortcuts. New and the terminal toggle expose Command shortcuts in tooltips.

## What did not change

- `j`/`k` and arrows navigate headers and cards while the cards are focused.
- `Enter` enters the selected task's currently shown terminal.
- `Ctrl+Q` returns from the terminal to the cards.
- `g` previews/toggles agent and shell without moving card focus. This is intentionally different from `Cmd+Enter`.
- `n`, `a`, `b`, and `c` retain their card actions.
- Escape reaches the CLI when a terminal is focused. It is not an app-wide escape-to-navigation key.
- Close cancellation still routes dirty or unpushed work to the task shell. It is a workflow transition, not generic focus restoration.
- Cards remain sorted by attention within each project. This improvement does not stabilize their order during status changes.
- One terminal is visible at a time. Hidden views and PTYs stay alive.
- There is no prefix mode, configurable keymap, or saved keyboard preference. These changes add no persistence or core session schema.

## Why Command, not a multiplexer prefix

Shika is a terminal workspace inside a native Mac app. Users should be able to keep typing in the CLI while invoking explicit app actions.

Command provides a useful ownership boundary: Shika owns its Command shortcuts; ordinary typing and Control combinations generally belong to the CLI. `Ctrl+Q` is the existing explicit exception. This avoids taking Escape or common shell/TUI keys away from the program.

A multiplexer prefix would add a keyboard mode and an extra step to frequent transitions. It also needs cancellation, conflict handling, and a way to send the prefix itself into the terminal. Being a developer does not imply wanting that trade-off.

The decision is one interaction model with a small, predictable default shortcut set. An optional prefix could be added later if actual users need it, but it should invoke the same actions and preserve the same focus rules, not create a second product mode. Full keymap customization is not part of this change.

## Implementation map

All app paths below are in `crates/shika/src/main.rs` unless stated otherwise. Use symbol names rather than line numbers, which drift.

| Piece | Symbols / location | Responsibility |
| --- | --- | --- |
| Action definitions | `gpui::actions!` | `NewAgent`, `SwitchTerminal`, `NextAgent`, `PreviousAgent` |
| Shortcut registration | `main`, `cx.bind_keys` | Command bindings scoped to the `Shika` key context |
| Action dispatch | `Shika::render`, root `.key_context("Shika")` and `.on_action` | Routes actions from cards or descendant terminal views; handlers guard busy/overlay state |
| Existing keys | `Shika::key` | Plain-key navigation, overlay editing, and terminal `Ctrl+Q` escape hatch |
| Navigation order | `rows`, `sorted_cards`, `adjacent_agent` | Project-grouped, status-sorted traversal; agent-only navigation skips headers |
| Focus-preserving navigation | `move_agent`, `focus_terminal` | Changes selection and, only when needed, focuses the destination task's shown terminal |
| Overlay restoration | `overlay_return_focus`, `restore_overlay_focus` | Stores a `FocusHandle`, restores it once, defaults to card focus when none was saved |
| Overlay entry/exit | `picker`, `picker_for`, `open_settings`, `open_base`, `cancel_overlay`, `apply_base`, `launch` | Captures focus before opening; restores on dismissal; launch intentionally clears restoration |
| Terminal switching | `toggle`, `Pane::new`, `bind_host`, `Host::write` | Reuses shell views or creates one; queues startup input; binds/resizes PTY and drains queued bytes in order |
| Sidebar reveal | `sidebar_scroll`, `last_revealed_selection`, `render`, `selection_reveal` | Tracks viewport and selection, measures the selected row, adjusts offset on the next frame |
| Scroll arithmetic | `crates/shika/src/model.rs`: `reveal_delta` | Minimal offset adjustment; oversized rows align their top |
| Terminal ownership | `crates/shika-terminal/src/view.rs`: `init`, `key_for` | Keeps Command shortcuts out of PTY key encoding; retains terminal copy/paste and history bindings |
| Discovery | `main` menu registration, `top_row`, `terminal_side`, `KeyTip` | Native Agent menu and existing themed tooltip component |

### Agent navigation

`rows()` includes project headers and every card, not just the three currently visible cards per project. `adjacent_agent()` walks that order in the requested direction until it finds a card. `move_agent()` guards overlay/busy state, records whether cards had focus, selects the result, and focuses its terminal only if the user was already in a terminal.

The selected task's `show_shell` determines which terminal receives focus. Do not force every destination to the agent view: returning to a task should preserve where the user was working. `visible_indices()` reveals collapsed cards around the selection without changing the three-card cap.

### Overlay focus

Before a picker, Settings, or Base branch overlay takes card focus, its opener captures `window.focused(cx)` in `overlay_return_focus`. Restoration consumes the saved handle with `take()` so it cannot accidentally affect a later overlay.

Base branch apply runs asynchronously with `spawn_in` / `update_in`, allowing successful completion to restore focus through the same window. Errors leave the dialog open. A new-agent launch discards the saved handle because it intentionally changes selection and focuses the new agent.

Do not apply restoration indiscriminately to Close: dirty/unpushed cancellation is deliberately routed into the shell for committing or pushing.

### Shell startup

For a new shell, `toggle(true, ...)` installs the `Pane`, then focuses its view before setting `busy`. The host may not yet have a PTY ID. `Host::write` stores incoming bytes in `pending_input`; `bind_host` registers the PTY, applies the measured size, and flushes bytes in order.

Successful asynchronous completion only binds the host. It does not focus again. If startup fails while the shell view is still focused, the app returns focus to the agent. If the user already left, failure does not pull them back into the terminal.

Preserve this sequence. Delaying focus can send early typing to the previous view; unconditionally focusing on completion can override `Ctrl+Q`.

### Selected-row visibility

The sidebar div uses `track_scroll` with `sidebar_scroll`. `render()` compares selection with `last_revealed_selection`; only a changed selection attaches the invisible measurement canvas to the selected header/card.

During prepaint, `selection_reveal()` measures that row against the viewport. `reveal_delta()` computes the smallest vertical adjustment needed, or zero if visible. The next-frame callback checks that selection still matches before changing the scroll offset and notifying the app. This avoids scrolling for a stale selection after rapid navigation.

Do not run this on every redraw: terminal output and timer updates must not prevent the user from scrolling away from the selected card. The implementation currently keys reveal requests to selection identity, not status-driven reordering or window resizing.

## Debugging

| Symptom | Start here |
| --- | --- |
| Command action does not fire in the terminal | Check binding spelling, root `Shika` context, action listeners, and busy/overlay guards. Check for a conflicting terminal binding before changing PTY encoding. |
| App shortcut sends bytes to the CLI | Inspect `key_for`'s early Command exclusion and terminal input handling. Do not implement these app actions as terminal input sequences. |
| Switching agents loses focus or lands in the wrong terminal | Inspect `move_agent`, `selected_card`, `focus_terminal`, and the destination's `show_shell`. |
| Next agent changes unexpectedly | Inspect `sorted_cards` and status updates. Attention sorting is still live; stable navigation order was not implemented here. |
| Picker/Settings dismissal lands on cards instead of the original terminal | Check that the opener saved `window.focused(cx)` before focusing the overlay, and that exit consumes the saved handle. |
| Close cancellation lands in shell | Expected for dirty/unpushed work. Check `cancel_overlay` before treating it as a restoration bug. |
| Early shell typing goes to agent, or startup steals focus | Check `toggle`'s immediate focus and completion branch, then `pending_input` / `bind_host`. |
| Selected row remains offscreen | Check sidebar tracking, selection-change detection, selected-row canvas prepaint, `reveal_delta`, and the deferred callback's selection guard. Test with enough projects to overflow the sidebar. |
| Manual scrolling snaps back | Verify reveal is triggered by selection changes only, not every render/tick. |

Reproduce with a built `.app`, disposable repositories, local bare remotes, and an isolated data directory. Never use a contributor's or author's normal app data for automated testing:

```sh
source "$HOME/.cargo/env"
./scripts/bundle-app.sh --debug
open -n target/debug/Shika.app --args --data-dir /absolute/path/to/disposable/data
```

Confirm Shika's own window is frontmost before synthetic keystrokes. A `cargo run` launch is not a substitute for the Dock/Finder PATH check. See [AGENTS.md](../AGENTS.md#toolchain) for environment and testing traps.

## Tests and acceptance

```sh
source "$HOME/.cargo/env"
cargo fmt --all --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Automated coverage added or extended:

- `agent_navigation_skips_headers_and_wraps_in_both_directions`: row-order traversal across projects and project-header entry points.
- `agent_navigation_handles_empty_single_and_missing_selection`: empty, one-agent, and invalid/missing-selection cases.
- `selection_reveal_moves_only_the_clipped_edge`: visible, top-clipped, bottom-clipped, and oversized rows.
- `command_is_left_to_the_app`: new Command combinations remain excluded from terminal key encoding.
- Existing `startup_query_replies_and_typeahead_survive_until_pty_binding`: startup input and terminal replies queue in order before binding.

These are unit tests, not proof of native shortcut dispatch, actual focus restoration, or rendered scrolling. Implementation validation passed 166 workspace tests, formatting, strict Clippy, a debug bundle build, and strict signature verification. The hands-on keyboard-flow checks remain unverified in [MANUAL_CHECKS.md](../MANUAL_CHECKS.md#command-keyboard-flow); do not report them as passed without exercising them.

For a keyboard contribution, exercise both terminal surfaces, both focus modes, first shell creation, dialog cancel/apply, busy state, collapsed cards, sidebar overflow, and light/dark appearance. Verify unsent drafts survive and ordinary CLI input, Escape, copy, and paste still work.

## Contributor guardrails

- Extend the existing GPUI actions and handlers rather than adding a parallel keyboard dispatcher or per-CLI shortcut map.
- Keep app shortcuts out of the terminal encoder. Preserve plain typing, Escape, and supported CLI bindings.
- Keep the busy/overlay guards when adding action entry points, including menus.
- Focus restoration is the default for non-workflow overlays; explicit workflow transitions may intentionally choose another destination.
- Use existing theme tokens, `KeyTip`, menu conventions, and key hints. Do not add styling values for keyboard behavior.
- Do not add animation delays to frequent keyboard transitions.
- Update `PLAN.md`, `design/DESIGN.md`, `AGENTS.md`, this document, and relevant acceptance checks when changing the documented behavior. Keep acceptance evidence separate from intended behavior.
