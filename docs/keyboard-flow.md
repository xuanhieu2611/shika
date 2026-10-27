# Keyboard flow

How Shika lets users move between tasks without leaving their working context, why the shortcuts use Command, and where to look when focus or navigation goes wrong.

Start here for keyboard-related contributions. This document holds the keyboard rules and explains their implementation; [design/DESIGN.md](../design/DESIGN.md) holds the visual and focus specification. For task-local shell ownership, terminal-tab lifecycle, and extension traps, read [terminal-tabs.md](terminal-tabs.md). The Changes panel's own keys and refresh rules are in [changes-panel.md](changes-panel.md). [MANUAL_CHECKS.md](../MANUAL_CHECKS.md#command-keyboard-flow) tracks GUI validation.

## What changed

Previously, working in a terminal meant returning to the cards for most app actions:

- Switch to shell: `Ctrl+Q`, `g`, `Enter`.
- Switch agents: `Ctrl+Q`, `j`/`k`, `Enter`. `j`/`k` moves from card to card, Leads section first, and skips project headers.
- Create another agent: `Ctrl+Q`, `n`, choose a CLI.

The card-navigation model is still available. A small native shortcut layer now makes these transitions directly:

| Shortcut | Action | Focus afterward |
| --- | --- | --- |
| `Cmd+Shift+R` | Rename the selected live task, without renaming Git | Dialog; completion/cancel restores previous focus and selection |
| `Cmd+Shift+P` | Preview commit, push, and Create PR for the task's original base | Dialog; cancel/completion restores previous focus |
| `Cmd+Shift+A` | Switch the selected task to another agent, in the same worktree | Dialog; cancel restores previous focus; confirm focuses the new agent terminal |
| `Cmd+O` | Add project (Agent > Add project…, the folder-plus button's tooltip). Fixed; Settings > Keyboard refuses it | The added project's header is selected, which shows nothing typeable, so the cards take focus (`focus_shown`) |
| `Cmd+Shift+B` | Base branch for the clicked header's project, else the selected one, else the first (Agent > Base branch…, the branch click's tooltip). Fixed | Dialog; completion/cancel restores previous focus |
| `Cmd+T` | New independent shell tab in this task's worktree | New shell; startup input is queued |
| `Cmd+W` | Close selected shell tab. On the pinned agent it does nothing and does not close the task | Preceding tab if the closed view had focus; otherwise preserve focus |
| `Cmd+Shift+W` | Close the selected task. Same path as the Close task button: cancel setup, remove a failed card, or the safe-close flow | Unchanged until close finishes; a clean idle task closes immediately |
| `Ctrl+Tab` / `Ctrl+Shift+Tab` | Next/previous tab in this task, wrapping | Destination terminal |
| `Cmd+1` through `Cmd+9` | Jump to that tab. `Cmd+1` is the pinned agent. A missing number does nothing | That terminal |
| `Cmd+]` | Next agent in current row order | Terminal if invoked from a terminal; cards if invoked from cards |
| `Cmd+[` | Previous agent in current row order | Same preservation rule |
| `Cmd+B` | Hide or show the agent column | Unchanged; with the column hidden, `j`/`k` from cards still change the agent on screen |
| `Cmd+Option+B` | Open or close the read-only Changes panel | Opening focuses the panel; closing from the panel restores the focus saved when it opened; closing from elsewhere moves no focus |
| `Cmd+Shift+U` | Open Settings on Usage, or close it (View > Usage) | Settings; Done, Escape, or the shortcut again restores the focus that opened it |
| `Cmd+N` | Open New for the current selection's project | Picker; cancel restores previous focus; successful launch focuses the new agent |
| `Cmd+Shift+N` | New Agent in Checkout for the same target project as New | In checkout picker; Tab changes project, Escape restores previous focus, launch focuses the new agent. An existing checkout card is selected and its shown terminal focused instead |
| `Cmd+L` | Start the selected project's Lead (Agent > New Lead…), show it when the project already has one, or resume its saved one | Picker in Lead mode only when the project has no saved Lead (same keys as New; Tab changes project). A live Lead is selected and its terminal focused at once. A saved (resting) Lead starts in its saved CLI with no picker, and its new card is selected with the terminal focused; if that CLI is missing or off in Settings, the picker opens instead. See [lead-agent.md](lead-agent.md) |

These are the defaults. Settings, Keyboard, can change New agent in checkout, Hide or show changes, Show or hide usage, Go to agent 1 through 9, and Go to tab 1 through 9. A new Command chord replaces the old one. Go to agent N selects the Nth card in the column (Leads section first, then the projects), skipping project headers, and does nothing when that card is not there. Agent rows start with no chord. The other shortcuts in this table stay fixed; `shortcuts::RESERVED` holds Cmd+O and Cmd+Shift+B so Keyboard cannot take them.

Next/previous agent skips project headers, crosses projects, and wraps. Each task keeps its selected tab and all terminal contents. New creates only the pinned CLI tab; the header `+` adds shells on demand. Shell labels are monotonically numbered, and individual close controls stop their PTYs without changing git or closing the task. Close task retains the safe-close flow and stops every owned PTY. With no valid selection, next chooses the first agent and previous the last. With no agents, navigation does nothing. With one agent, it stays selected.

These actions do nothing while the app is busy or any overlay is open. A Lead has no shell tabs, Create PR, or agent switch: `Cmd+T`, `Cmd+Shift+P`, and `Cmd+Shift+A` show a short message on its card. Workers started by a Lead (`shika new`) never take selection or focus from the author. `Cmd+N` and `Cmd+Shift+N` use the first project when nothing is selected and open Add project when there are no projects. A checkout session has no Create PR; its shortcut/menu cannot publish it. See [checkout lifecycle](tasks-and-worktrees.md#checkout).

## Accidental typing

The author types prompts into agent terminals all day. Typing that was not meant for Shika must never trigger anything in it (decided 2026-10-25). That extends the Command ownership boundary (below) from shortcuts to dialogs and focus:

- **Dialog choices are exact Command chords.** Discard is `Cmd+D`, Push is `Cmd+Enter` (offered only with a clean tree), branch-switch Close and checkout **Stop the agent?** are `Cmd+Enter`, and Forget, Remove project, and Remove leftover worktree are `Cmd+Delete`. A plain letter or bare Enter never discards, publishes, forgets, or removes. "Exact" means Command and nothing else: Shift, Option, or Control with the key does nothing. The chords exist only while their dialog is open and are swallowed there, so a terminal never sees them; there are no global bindings. `Cmd+D` and `Cmd+Enter` are reserved in Settings > Keyboard (as `Cmd+Delete` already was), since GPUI runs a binding before the dialog's key handler. Enter still stops a Lead ("Stop the Lead?" keeps the conversation). Escape cancels everywhere. The Stop dialog goes straight to Forget with `Cmd+Delete`: at once when the Lead's worktree has nothing to lose, otherwise to the Forget step with the facts, where a second `Cmd+Delete` confirms. Quit and Reopen also confirm only with exact `Cmd+Enter`, with the 300ms grace; repeated Cmd+Q and bare Enter never confirm. The Leftovers list uses Enter to inspect Reopen and `d` to open Remove's confirmation. Quit cancellation restores the previous overlay and its original focus, including a native text field; it never silently drops the editor.
- **A grace period for keys already in flight.** Close, branch-switch Close, the Lead's Stop and Forget, the resting-Lead menu, Remove project, Remove leftover, Quit, Reopen, worktree setup approval, and a Lead question ignore every key except Escape for 300ms after they appear. The window starts again when the dialog's identity changes (Stop to the Forget step, the next question). Mouse clicks are unaffected, and text-entry dialogs, the picker, and Settings have no grace.
- **Dialogs that appear on their own wait.** A Close requested by the Lead (`shika close`) and a Lead chat question open only after nothing has been typed in any terminal (agent or shell) or Lead chat input for 3 seconds (`BELL_TYPING_QUIET`, the doorbell's quiet), with no overlay open and Shika not busy. Questions wait in the existing inbox, first come, first served. `shika close` registers its pending dialog at once, so the one-dialog gate holds, selects nothing until it opens, and is not the Lead's dialog until then (a Close the author opens on that worker meanwhile behaves as their own, and the Lead's request is refused once if the worker goes away), is refused if the worker or Lead goes away, and after 120 seconds is refused with "The author kept typing or had another dialog open. Try again later."
- **Focus follows what can take typing.** A stray click on empty column, the title bar, footer gaps, or any chrome never moves focus. After a successful Close, a Lead Stop or Forget, an exited Lead resting, a cancelled-setup or launch-error card going away, Add project, and a dialog dismissed with no saved focus, focus lands on the shown terminal or chat input, but only when focus pointed at something now gone or hidden: an author on the cards or the Changes panel stays there, and setup finishing never takes focus from either. The cards get it only by Ctrl+Q, or when nothing shown takes typing (no selection, a project header, a resting Lead, a failed-setup card). A Lead-requested Close still returns focus to the Lead.
- **A header click does not leave the terminal.** With a live card or Lead chat on screen, clicking a project header changes neither the screen nor the focus. The project becomes the target of `Cmd+N`, `Cmd+Shift+N`, `Cmd+L`, and Base branch until the selection changes in any way (a counter, so leaving a card and coming back does not revive it), and its header is drawn selected. Only those commands use the target; the Lead chat header and everything else describing what is shown follow the selected card. With nothing typeable shown, the header is selected and the cards take focus as before.
- **The card list takes few keys.** Only `j`, `k`, the arrows, and Enter. Plain `a`, `b`, and `r` are gone (Add project and Base branch are Command chords). Enter on a failed-setup card retries it.

## Settings sections

Settings (`Cmd-,`) is Appearance, Keyboard, Agents, and Projects. `j`/`k` move within the section. `[` and `]` change section. Appearance keeps its row keys. On Keyboard, Enter or a click on the chord records the next Command shortcut, Delete clears that row, and Escape cancels the recording. A chord already used by another Keyboard row moves to the new row. A chord used by a fixed shortcut, such as New agent, or by text editing, such as Cmd+Left or Cmd+Delete, is refused: a binding is matched before a focused text field sees the key. A saved override with such a chord no longer binds. On Agents, `h` turns the selected CLI off and `l` turns it on. Projects lists repositories by name/path; Enter or a click opens that project's Worktree setup. Escape closes Settings, except inside setup where it returns to Projects.

New lists CLIs that are on the login-shell PATH and turned on, followed by Terminal in ordinary task mode. Number keys match that list. Terminal works without an installed agent and uses the same focus restoration and setup flow. Lead and checkout pickers remain agent-only; when those lists are empty, Enter opens Settings on Agents and keeps New's focus return. See [Terminal tasks](terminal-tasks.md).

Other improvements:

- Picker cancellation, Settings dismissal, and Base branch completion/cancellation restore the focus that opened the overlay.
- A selection change scrolls the selected header/card into view with minimal movement. Ordinary redraws do not undo manual scrolling.
- First shell open focuses the new view immediately. Early typing is queued until the PTY is bound; startup completion does not reclaim focus if the user has returned to the cards.
- The native Agent menu lists the new actions and shortcuts. New agent, new shell, and shell close expose Command shortcuts in tooltips.
- The View menu lists "Hide or show changes" with `Cmd+Option+B`, and the panel's toggle shows it in its tooltip. View > Usage shows `Cmd+Shift+U` (or the chord Settings gave it). The shortcut opens Settings on Usage, switches an open Settings to Usage, and closes Settings when Usage is showing; it does nothing while another dialog or picker is open, or while Settings is typing a value or recording a shortcut.

Create PR is also in the Agent menu and terminal metadata row. Its dialog uses Tab to switch title/target fields, Enter to confirm publishing, and Escape to cancel. Plain card keys never run while editing. Busy/overlay conflicts and active/blocked turns block entry. See [publishing.md](publishing.md); publishing runs on a worker, not by writing commands into a terminal.

Switch agent is in the Agent menu and on `Cmd+Shift+A`. Its dialog lists the same CLIs as New, and the prompt starts empty. `j`/`k` or the arrows move that list, Tab edits the prompt, Enter confirms, and Escape cancels and restores focus. While the prompt is focused, `j`/`k` type and Up/Down still move the list. Confirm focuses the new agent terminal and does not restore the old one. An empty prompt leaves the card Waiting; a prompt submits a turn. Busy, an open overlay, Working, Asking, setup, and unsent text in the agent terminal block entry; the check runs again at confirmation. See [agent-switch.md](agent-switch.md).

## Worktree setup

After selecting a CLI, the first author-created New for an unconfigured project offers a compact setup preview with Customize setup…, Start without setup, and Save, approve and start. Tab / Shift+Tab traverses controls; Enter activates the focused one (save/start when suggestions exist, skip otherwise). Held/repeated Enter cannot submit the setup dialog. Customize expands the existing native editor. Escape/Cancel returns to the same project's CLI picker without saving a decision or changing card selection; cancelling the picker restores its original focus. Save/approve/start prepares the chosen agent; skip remembers a local per-project preference and continues the normal launch without granting consent. Existing configurations keep the ordinary approval dialog. Retry, Lead, and its workers do not open onboarding.

Settings > Projects > Worktree setup… opens a project's configuration dialog; there is no project-header or Agent-menu control. Projects supports `j`/`k`, arrows, and Enter, independent of card selection. No new global shortcut is taken. Tab / Shift+Tab traverses native fields and buttons and scrolls the focused row into view, Enter activates the focused control (or saves from a field), and Escape cancels. Plain typing, clipboard shortcuts, and IME stay in the native field; app actions are blocked while the dialog is open. Save/cancel/disable returns to Projects at the same row, retaining Settings's original return focus; Done then restores it without changing card selection. Missing configurations may receive unsaved, editable suggestions; existing configurations stay unchanged. Source/config validation errors retain the editor; malformed existing config refuses entry. Loading and saving run on background executors. See [worktree preparation](worktree-preparation.md#configure-once).

## Card context menu

Right-click or Control-click anywhere on a card opens a menu at the pointer: **Rename task…** `Cmd+Shift+R`, then **Close task** `Cmd+Shift+W`. Rename is disabled until a live session exists (setup-running/failed cards cannot be renamed). No close button or hover control is added to the card. The menu stays inside the window, uses the existing solid overlay and list-row tokens, and works in light, dark, derived themes, and glass.

Opening does not change selection or enter a terminal. Escape or an outside click dismisses it and restores the opening focus; the outside click is consumed. Click Close task or press Cmd+Shift+W to close the clicked task through the existing `Shika::close` path, including setup cancellation, failed-card removal, and Git/active-turn safety checks. Cancelling that Close leaves the shown tab alone and restores the opening focus. Up/Down, `j`/`k`, or Tab switch the highlighted item; Enter activates it (Rename by default on live cards, Close otherwise). Hover also changes the highlight. Cmd+Shift+R opens Rename for the clicked card, regardless of highlight. Other app actions are blocked while the menu is open, and plain typing never reaches a PTY.

Rename opens a native single-line input with the full name selected; typing, editing, clipboard, and IME belong to that input, not card keys or a PTY. Enter applies after composition finishes; Escape cancels outside composition. A mouse confirmation during composition is ignored. App actions are blocked while editing. Both exits restore opening focus without changing selection. The dialog targets the clicked card by stable identity even if notification routing changes selection. See [task-names.md](task-names.md) for the public FAQ, validation, title priority, publishing effects, and implementation guardrails.

Implementation: `Overlay::CardMenu`, `CardMenu::target_index`, `open_card_menu`, `close_from_card_menu`, and `card_menu_view` in `main.rs`. The menu binds to the agent view's stable entity ID, not the selected card or its vector index; asynchronous setup cleanup or selection changes cannot retarget it. If that card disappears, activation just dismisses the menu. The unit test `card_menu_stays_bound_to_the_clicked_card_after_reordering_or_removal` covers target lookup, not native mouse dispatch. GUI acceptance is tracked in `MANUAL_CHECKS.md`.

## What did not change

- `j`/`k` and arrows navigate cards while the cards are focused, skipping project headers. An empty project is not a stop. They do not focus the terminal, so a second `j` still moves.
- Clicking a card selects it and focuses its shown terminal. A notification click activates the window and does the same. The next keystroke is typed there. A click elsewhere in the chrome moves no focus, and a project header click keeps a shown terminal focused (see Accidental typing).
- `Enter` enters the selected task's currently shown terminal while the cards have focus.
- `Ctrl+Q` returns from the terminal to the cards. It is the deliberate way there: nothing else sends focus to the list while a terminal or chat is shown, and a late setup completion does not pull it back.
- The cards take only `j`, `k`, the arrows, and Enter. Plain `a`, `b`, `r`, `n`, `c`, and `g` do nothing; Add project and Base branch are `Cmd+O` and `Cmd+Shift+B`, and Enter on a failed-setup card retries it (so does the button).
- Escape reaches the CLI when a terminal is focused. It is not an app-wide escape-to-navigation key. It closes the Changes panel only while the panel itself has focus.
- Verified branch-switch Close has a separate confirmation: Cmd+Enter closes while keeping both local branches (bare Enter does nothing), Escape restores the opening focus. Unsafe recovery is blocked rather than offering Push/Discard; see [branch-switch-close.md](branch-switch-close.md).
- Close cancellation restores the opening focus and leaves the shown tab alone. It does not open or select a shell.
- A checkout card stays first in its project; other cards remain sorted by attention within each project. This improvement does not stabilize their order during status changes.
- One terminal is visible at a time. Hidden views and PTYs stay alive. The Changes panel is a read-only diff beside it, not a second terminal.
- Keyboard in Settings can rebind New agent in checkout, Hide or show changes, Show or hide usage, and the numbered agent and tab jumps. It does not add a prefix mode, and it does not rebind the other shortcuts. Terminal tabs add no persistence. Core's memory-only Session now records `shell_ptys`; `open_shell` creates a fresh PTY on each call and `close_shell` validates task ownership before stopping one.

## Why Command, not a multiplexer prefix

Shika is a terminal workspace inside a native Mac app. Users should be able to keep typing in the CLI while invoking explicit app actions.

Command provides a useful ownership boundary: Shika owns its Command shortcuts; ordinary typing and Control combinations generally belong to the CLI. `Ctrl+Q` leaves the terminal for the cards. `Ctrl+Tab` and `Ctrl+Shift+Tab` cycle this task's tabs, the same cycle other Mac apps use, and are kept out of the PTY. This avoids taking Escape or common shell/TUI keys away from the program.

A multiplexer prefix would add a keyboard mode and an extra step to frequent transitions. It also needs cancellation, conflict handling, and a way to send the prefix itself into the terminal. Being a developer does not imply wanting that trade-off.

The same boundary covers dialogs: an exact Command chord is deliberate, a letter or Enter might be the tail of a prompt. See [Accidental typing](#accidental-typing).

The decision is one interaction model with a small, predictable default shortcut set. Settings can replace a few of those Command chords. It does not add a second mode, and a plain key still belongs to the cards or the CLI.

## Implementation map

All app paths below are in `crates/shika/src/main.rs` unless stated otherwise. Use symbol names rather than line numbers, which drift.

| Piece | Symbols / location | Responsibility |
| --- | --- | --- |
| Action definitions | `gpui::actions!`, `SelectTerminal` | `NewAgent`, `NewInCheckout`, `CloseTask`, `SwitchAgent`, `NextAgent`, `PreviousAgent`, and the numbered tab jump |
| Shortcut registration | `main`, `cx.bind_keys` | Command bindings scoped to the `Shika` key context |
| Action dispatch | `Shika::render`, root `.key_context("Shika")` and `.on_action` | Routes actions from cards or descendant terminal views; handlers guard busy/overlay state |
| Existing keys | `Shika::key` | Plain-key navigation (`j`, `k`, arrows, Enter), overlay editing, and terminal `Ctrl+Q` escape hatch |
| Dialog chords | `model::dialog_chord`, `model::DialogChord` (`Primary` Cmd+Enter, `Discard` Cmd+D, `Delete` Cmd+Delete), `Shika::dialog_chord_key`, `forget_lead_from_dialog` | Matches exact Command-only chords while a confirmation dialog is open, acts or deliberately drops them, and stops propagation so terminals never see them. Called in `Shika::key` before the plain-key path |
| Input grace | `model::DIALOG_GRACE` (300ms), `DialogGrace`, `Shika::dialog_identity`, `observe_dialog`, `dialog_ignores_key`, `QuestionDialog::identity` | Starts a window when a confirmation dialog (new identity) appears; every key but Escape is dropped while it runs. Observed from render, the tick, and every key |
| Typing gate | `Shika::author_quiet`, `model::typing_quiet`, `control::BELL_TYPING_QUIET` | True when no terminal (agent or shell) or Lead chat input saw typing for 3 seconds. Shared with the doorbell |
| Deferred dialogs | `Shika::control_close`, `resume_deferred_close`, `start_lead_close`, `LeadDialog::deferred`, `DEFER_LIMIT`; `open_chat_question`, `drain_lead_chats`, `chat_questions` in `lead_chat.rs` / `lead_chat_question.rs` | A Lead's Close and chat questions wait for `author_quiet`, no overlay, and not busy; retried on the tick |
| Focus landing | `Shika::focus_shown`, `land_focus`, `focus_pane`, `focus_terminal`, `shown`, `model::Shown` (`takes_typing`) | Puts focus on what is shown and typeable; the cards only when `Shown` says nothing takes typing |
| Stray clicks | `Shika::render`: the cards `FocusHandle` is tracked by a zero-size child (`div().absolute().size_0().track_focus(&self.focus)`); the root keeps `key_context`, actions, and the capture handler | No mouse-down can land on a tracked element, so a click on chrome never focuses the list |
| Header target | `Shika::project_target`, `click_project`, `target_project`, `project_id`, `model::header_target` | A header click with a terminal shown records the project for New, Lead, and Base branch; valid only while the selection equals the one recorded. Cleared on card removal |
| Add project, Base branch | `AddProject`, `BaseBranch` actions, `install_keys`, Agent menu, `shortcuts::RESERVED` | `Cmd+O` and `Cmd+Shift+B`; both fixed
| Navigation order | `column_layout`, `rows`, `adjacent_agent`, `model::ColumnLayout` | Leads section first (each Lead, then its workers), then project-grouped, status-sorted traversal; agent-only navigation skips headers |
| Focus-preserving navigation | `move_agent`, `focus_terminal` | Changes selection and, only when needed, focuses the destination task's shown terminal |
| Overlay restoration | `overlay_return_focus`, `restore_overlay_focus` | Stores a `FocusHandle`, restores it once, and falls back to `land_focus` (the shown terminal or chat) when none was saved |
| Overlay entry/exit | `picker`, `picker_for`, `open_settings`, `open_base`, `cancel_overlay`, `apply_base`, `launch`, `switch_agent`, `confirm_switch` | Captures focus before opening; restores on dismissal; launch and a confirmed switch intentionally clear restoration |
| Terminal switching | `new_shell`, `select_tab`, `cycle_tab`, `close_tab`, `Card::active_pane` | Task-local tabs, pinned agent, independent shells, selected-view focus and asynchronous PTY teardown |
| Shell startup | `Pane::new`, `bind_host`, `Host::write` | Queues startup input; binds/resizes PTY and drains queued bytes in order |
| Tab navigation | `model::adjacent_tab`, `model::tab_after_close`, `Card::tab_scroll` | Wrap navigation, preserve selection on removal, reveal selected tab horizontally |
| Sidebar reveal | `sidebar_scroll`, `last_revealed_selection`, `render`, `selection_reveal` | Tracks viewport and selection, measures the selected row, adjusts offset on the next frame |
| Scroll arithmetic | `crates/shika/src/model.rs`: `reveal_delta` | Minimal offset adjustment; oversized rows align their top |
| Terminal ownership | `crates/shika-terminal/src/view.rs`: `init`, `key_for` | Keeps Command shortcuts, Ctrl+Tab, and Ctrl+Shift+Tab out of PTY key encoding; retains terminal copy/paste and history bindings |
| Discovery | `main` menu registration, `top_row`, `terminal_side`, `KeyTip` | Native Agent menu and existing themed tooltip component |
| Changes panel | `ToggleChanges`, bound from `shortcuts::bindings` (default `cmd-alt-b`) in the `Shika` context, View menu; `crates/shika/src/changes.rs`: `Shika::toggle_changes`, `close_changes`, `restore_changes_focus`, `changes_key` on the panel's `Changes` key context, `Panel::focus`, `Panel::return_focus`; `move_agent` | Toggle, saved return focus, panel-local scrolling keys (see [changes-panel.md](changes-panel.md#architecture-and-code-map)) |
| Usage (Settings section) | `ToggleUsage`, bound from `shortcuts::bindings` (default `cmd-shift-u`) in the `Shika` context, View menu "Usage"; `Shika::toggle_usage`, `show_settings`, `select_section`, `on_usage_key`, `cancel_overlay` | Opens Settings on Usage with saved return focus; switches an open Settings; inert under another overlay; a second press closes and restores focus |
| Checkout entry and Stop | `crates/shika/src/checkout.rs`: `new_in_checkout`, `show_checkout`, `start_checkout`; `Overlay::CheckoutClose`, `dialog_chord_key`, `cancel_overlay` in `main.rs` | Reuses picker focus restoration; an existing card focuses its shown terminal. Active/blocked Stop uses exact Cmd+Enter and the 300ms guard; Escape restores opening focus and leaves the shown tab alone |
| Custom shortcuts | `crates/shika/src/shortcuts.rs`: `COMMANDS`, `bindings`, `assign`; `install_keys` in `main.rs`; `KeyOverrides` in `settings.json` | The Keyboard rows, conflict handling, and the chords actually bound |

### Agent navigation

A resting Lead (a saved Lead with no process) is a row in the Leads section and a stop for `j`/`k`, Cmd+] / Cmd+[, and Go to agent N (`Selection::Resting`). It has no terminal: moving onto it from a terminal with Cmd+] lands on the cards (nothing there takes typing), and moving off it with the cards focused keeps them. **Enter** on a selected resting Lead (cards focused), a click on the row, and a click on the resting terminal area resume it (`resume_lead`); the new Lead card is selected and its terminal focused. **Cmd+Shift+W** on a selected Lead opens "Stop the Lead?" (Enter stops, `Cmd+Delete` forgets, Escape cancels); on a selected resting Lead it opens "Forget the Lead?" directly (`Cmd+Delete` forgets, bare Enter does nothing). Right-click or Control-click on a resting row opens a one-item menu, Forget Lead `⌘⌫` (`Cmd+Shift+W` still opens the Forget step; bare Enter does nothing). Rename (`⌘⇧R`) does nothing for a Lead. These keys do nothing while a dialog is open.

`rows()` follows the column from top to bottom, from `column_layout()` (pure ordering in `model::ColumnLayout`). When a project has a live or resting Lead, the Leads section comes first: for each such project in project order, the Lead card and then the workers it started, sorted by attention (a resting Lead is one row with no workers). It has no headers. The Projects section follows: groups with cards first, empty headers below, preserving saved repository order within each tier. Each header precedes its own cards (not Leads, not workers of a live Lead), checkout first, then worktree tasks sorted by attention. All navigation uses this display order, not the saved repository list. A last card's exit keeps its project in the occupied tier until the exit finishes. When a Lead closes, its workers return to their project's list at once and can promote that project. Leads retain saved project order; picker and Settings order are unchanged. `adjacent_agent()` walks that order in the requested direction until it finds a card. `move_selection()` uses it for `j`/`k` and the arrows, so those keys never land on a header. `move_agent()` uses the same walk for Cmd+] / Cmd+[, guards overlay/busy state, records whether cards had focus, selects the result, and focuses its terminal only if the user was already in a terminal.

The selected task's `active_tab` determines which terminal receives focus (zero is the pinned agent; shell indices start at one). Do not force every destination to the agent view: returning to a task should preserve where the user was working. Every card is painted. When the list is taller than the column, the sidebar scroll brings the selected card into view.

### Overlay focus

Before a picker, Settings, or Base branch overlay takes card focus, its opener captures `window.focused(cx)` in `overlay_return_focus`. Restoration consumes the saved handle with `take()` so it cannot accidentally affect a later overlay.

Base branch apply runs asynchronously with `spawn_in` / `update_in`, allowing successful completion to restore focus through the same window. Errors leave the dialog open. A new-agent launch discards the saved handle because it intentionally changes selection and focuses the new agent.

Close cancellation uses that same restoration. Escape or Cancel leaves `active_tab` alone and does not open a shell, including when the tree is dirty or commits are unpushed. A Lead-requested Close still returns selection and focus to the Lead afterwards.

When nothing was saved, or a task, Lead, or card goes away under the focus, the destination is `focus_shown`, not the cards. `land_focus` is the version for restoration: with a dialog still open it focuses the cards (dialogs take their keys through them), otherwise it calls `focus_shown`. `focus_terminal` keeps its busy guard; `focus_shown` does not, because the card being created is on screen and must hold the keys (`j` would otherwise move the selection away). New focuses the new card's agent terminal at once; typing during copy and setup is dropped by design. Completion focuses only if the cards do not have focus, so a Ctrl+Q in the meantime stands (a Lead restart that asked for focus is the exception).

### Shell startup

For a new shell, `new_shell(true, ...)` installs the `Pane`, then focuses its view before setting `busy`. The host may not yet have a PTY ID. `Host::write` stores incoming bytes in `pending_input`; `bind_host` registers the PTY, applies the measured size, and flushes bytes in order.

Successful asynchronous completion only binds the host. It does not focus again. If startup fails while the shell view is still focused, the app removes that tab and focuses the preceding tab (or the agent if it was the only shell). If the user already left, failure does not pull them back into the terminal.

Preserve this sequence. Delaying focus can send early typing to the previous view; unconditionally focusing on completion can override `Ctrl+Q`.

### Changes panel focus

The Changes panel is a third focus surface, next to the cards and the terminal. `Cmd+Option+B` toggles it through the root `Shika` context like `Cmd+B`, with the same busy/overlay guards; `key_for` already keeps it out of the PTY because it carries Command.

Opening saves `window.focused(cx)` in the panel's own return slot (`Panel::return_focus`), the same pattern as `overlay_return_focus` but separate from it, since a dialog can open over an open panel. Closing from inside the panel (Escape, the toggle, the button) consumes that handle once and falls back to the cards when its view is gone; a saved terminal of a card other than the selected one gives way to the selected card's shown terminal, because the panel followed the selection there. Closing while the terminal or cards have focus moves nothing. A click on a card, including its diff stat, selects that card and focuses its terminal and leaves the panel as it is.

The root `Shika::key` capture handler leaves plain keys alone unless the cards have focus, so they reach the panel's `on_key_down` (`changes_key`), which stops propagation for the keys it handles. `Ctrl+Q` is handled by that capture handler before the panel sees it, as for a terminal. `move_agent` treats the panel as its own surface: from the panel it changes the selection without focusing a terminal.

While the panel has focus, its plain keys (`j`/`k`, arrows, `d`/`u`, Space, `g`/`G`, `]`/`[`, `h`/`l`, `o`, `r`, Escape) are handled in its own key context and never reach a PTY. `Ctrl+Q` returns to the cards and leaves the panel open. Command shortcuts keep working: `Cmd+]` / `Cmd+[` change the agent and keep focus in the panel, which follows the new card; tab shortcuts focus their destination terminal. Clicking the terminal focuses it and leaves the panel open. The panel keys are listed in [changes-panel.md](changes-panel.md#controls-and-focus). File cards have two clickable header rows that fold/unfold the file while keeping panel focus, without a Git read. File jumps and `o` use visible row positions after folding; `o` never expands a cap row inside a folded file. Code has line numbers and diff colors, but no hunk labels or added/removed signs. No panel key changes.

### Project reorder motion

`crates/shika/src/project_motion.rs` wraps each project group with a stable project-id element. Layout commits the occupied-first order immediately; prepaint translates the entire group and its hitboxes from the previous position to the new one over 220ms using `model::ease`. Only a display-order change advances `project_order_epoch`; status updates, redraws, scrolling, and window resizing do not start motion. Positions exclude the sidebar's scroll offset. An interrupted reorder retargets from the in-flight painted position; Reduce Motion snaps to the target and schedules no animation frames. No terminal, focus, or saved project order changes.

### Selected-row visibility

The sidebar div uses `track_scroll` with `sidebar_scroll`. `render()` compares selection with `last_revealed_selection`; only a changed selection attaches the invisible measurement canvas to the selected header/card.

During prepaint, `selection_reveal()` measures that row against the viewport, subtracting any project-reorder translation so it reveals the final layout position. `reveal_delta()` computes the smallest vertical adjustment needed, or zero if visible. The next-frame callback checks that selection still matches before changing the scroll offset and notifying the app. This avoids scrolling for a stale selection after rapid navigation.

Do not run this on every redraw: terminal output and timer updates must not prevent the user from scrolling away from the selected card. The implementation currently keys reveal requests to selection identity, not status-driven reordering or window resizing.

A successful Close records its replacement selection as already revealed. When a Lead's `shika close` returns selection to the Lead, the Lead is revealed as usual, because the dialog had scrolled to the worker. The exit only collapses that card's row and its leading gap; it does not reveal another row or reset manual scrolling. The scroll container still clamps its offset when the remaining content no longer fills the viewport.

## Debugging

| Symptom | Start here |
| --- | --- |
| Command action does not fire in the terminal | Check binding spelling, root `Shika` context, action listeners, and busy/overlay guards. Check for a conflicting terminal binding before changing PTY encoding. |
| App shortcut sends bytes to the CLI | Inspect `key_for`'s early Command exclusion and terminal input handling. Do not implement these app actions as terminal input sequences. |
| Switching agents loses focus or lands in the wrong terminal | Inspect `move_agent`, `selected_card`, `focus_terminal`, and the destination's `active_tab`. |
| Next agent changes unexpectedly | Inspect `sorted_cards` and status updates. Attention sorting is still live; stable navigation order was not implemented here. |
| Picker/Settings dismissal lands on cards instead of the original terminal | Check that the opener saved `window.focused(cx)` before focusing the overlay, and that exit consumes the saved handle. |
| Close cancellation changes the shown tab or opens a shell | `cancel_overlay` restores opening focus and must leave `active_tab` unchanged. A Lead-requested Close then moves to the Lead in `settle_lead_dialog`. |
| Early shell typing goes to agent, or startup steals focus | Check `new_shell`'s immediate focus and completion branch, then `pending_input` / `bind_host`. |
| Selected row remains offscreen | Check sidebar tracking, selection-change detection, selected-row canvas prepaint, `reveal_delta`, and the deferred callback's selection guard. Test with enough projects to overflow the sidebar. |
| Manual scrolling snaps back | Verify reveal is triggered by selection changes only, not every render/tick. |
| A dialog was chosen by a stray key, or ignores real ones | `dialog_chord_key` and `model::dialog_chord` for the chord, `dialog_identity` for what counts as a new dialog, `dialog_ignores_key` for the 300ms window. A dialog that ignores everything for a moment is the grace, not a bug. |
| A Lead's close dialog or question never opens | `author_quiet` (any terminal or chat typed in the last 3 seconds), an open overlay, `busy`, then `resume_deferred_close`'s 120 second limit. Questions sit in `chat_questions`. |
| Focus lands on the cards, or a click moved it | `focus_shown` and `Shown`, the zero-size focus child in `render`, and whether the call site used `window.focus(&self.focus)` directly. |
| Cmd+O, Cmd+Shift+B, or a dialog chord does nothing in a real window | `install_keys`, the Agent menu key equivalents (a menu item with the same key can take the event first), and whether the `Shika` context is active. |
| A panel key reaches the CLI, or Escape in the panel lands in the wrong place | The panel's key context and focus at the time of the key; the panel's saved return handle, consumed once on close. |

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
- `command_is_left_to_the_app` and `control_tab_is_left_to_the_app`: Command combinations, Ctrl+Tab, and Ctrl+Shift+Tab remain excluded from terminal key encoding.
- Existing `startup_query_replies_and_typeahead_survive_until_pty_binding`: startup input and terminal replies queue in order before binding.
- `only_a_bare_command_chord_chooses_in_a_dialog`: exact Cmd+Enter, Cmd+D, and Cmd+Delete; plain keys, Shift, Control, Option, and other letters give nothing.
- `a_dialog_ignores_keys_for_a_moment_after_it_appears` and `another_dialog_starts_its_own_window`: the 300ms window, no extension on re-observation, a new identity or a returning dialog starts again.
- `typing_is_quiet_only_when_every_input_is`: the 3 second gate over several input stamps.
- `only_a_shown_card_takes_typing` and `a_header_target_lasts_until_the_selection_changes` (`focus_tests`): which selections take focus, and the header target's validity.
- `add_project_and_base_branch_are_fixed_chords` (`shortcuts.rs`): Keyboard refuses Cmd+O and Cmd+Shift+B.
- `lead_tests` Stop facts: the added Forget line.

These are unit tests, not proof of native shortcut dispatch, actual focus restoration, or rendered scrolling. Implementation validation passed 166 workspace tests, formatting, strict Clippy, a debug bundle build, and strict signature verification. The hands-on keyboard-flow checks remain unverified in [MANUAL_CHECKS.md](../MANUAL_CHECKS.md#command-keyboard-flow); do not report them as passed without exercising them.

For terminal-tab validation, see the separate Task-scoped terminal tabs checklist in `MANUAL_CHECKS.md`. Unit coverage includes wrap/empty tab navigation, selection preservation on removal, independent core shells, ownership checks, and teardown of all shells on task close.

For a keyboard contribution, exercise the agent and multiple shell tabs, both focus modes, first shell creation, dialog cancel/apply, busy state, a list tall enough to scroll the column, and light/dark appearance. Verify unsent drafts survive and ordinary CLI input, Escape, copy, and paste still work.

## Contributor guardrails

- Extend the existing GPUI actions and handlers rather than adding a parallel keyboard dispatcher or per-CLI shortcut map.
- New customizable shortcuts go in `shortcuts::COMMANDS` and `install_keys`. A custom chord must include Command, so `key_for` keeps it out of the terminal. Do not take a fixed shortcut such as Quit, New agent, or copy and paste.
- Keep app shortcuts out of the terminal encoder. Preserve plain typing, Escape, and supported CLI bindings.
- Keep the busy/overlay guards when adding action entry points, including menus.
- Focus restoration is the default for non-workflow overlays; explicit workflow transitions may intentionally choose another destination.
- A new destructive or publishing dialog choice needs an exact Command chord, matched in `dialog_chord_key`, shown on its button as a Command key cap, and swallowed so terminals never see it. Never a plain letter or bare Enter.
- A new confirmation dialog goes into `dialog_identity` so it gets the 300ms grace. A new dialog the app or the Lead opens on its own must use `author_quiet` and wait with the same retry-on-tick pattern; never open one into a moment of typing.
- Never land focus on the cards implicitly. Use `focus_shown` (or `land_focus` under a dialog); call `window.focus(&self.focus, ...)` only for Ctrl+Q, a dialog that takes its keys through the cards, or a state where `Shown` says nothing takes typing. Do not give any chrome element the cards' focus handle, or a click on it will focus the list.
- A new card-list plain key needs an author decision; the list takes only `j`, `k`, the arrows, and Enter. Use a Command chord and reserve it in `shortcuts.rs`.
- Use existing theme tokens, `KeyTip`, menu conventions, and key hints. Do not add styling values for keyboard behavior.
- Do not add animation delays to frequent keyboard transitions.
- Update `design/DESIGN.md`, `AGENTS.md`, this document, and relevant acceptance checks when changing the documented behavior. Keep acceptance evidence separate from intended behavior.
