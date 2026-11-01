# Terminal tasks

New (Cmd+N) offers **Terminal** after the installed, enabled agents. With **Create worktree** on, it starts a task without an agent: a fresh generated branch/worktree, the normal optional preparation, then the user's interactive login shell. With the switch off, the same row starts a login shell in the project folder and no agent. It works with no agents installed. Terminal is not a CLI preset in Settings or a Lead choice.

## Behavior

- The card starts as `Terminal <id>` on `shika-draft-<id>`. No naming prompt interrupts launch. Rename task changes its display name and future PR defaults, not Git.
- The pinned first tab is Terminal, without an individual close control. Cmd+T adds independent Shell tabs; Cmd+W closes only those additional tabs. Close task ends the whole task through the ordinary dirty/unpushed/branch-identity checks.
- Typing, history recall, output, and manually launching an agent in this shell never drive automatic task naming, agent activity, elapsed turns, or completion notifications. The card has no status signal and its column hint is `↵ terminal`. Shika does not inspect shell jobs or infer whether a command is safe to stop. As with existing shell tabs, save running tools before closing or publishing.
- Changes and Create PR retain ordinary worktree behavior. Changes is refreshed explicitly with `r`; shell output adds no watcher or poller. No Ready event is manufactured to refresh the diff stat.
- Cmd+Shift+A / Switch agent starts an installed agent in the same worktree. The original Terminal becomes an additional numbered Shell tab, preserving its process, output, and draft. Existing shells also stay. Failed starts keep the original task untouched.
- An untouched generated branch can be named by the explicitly started agent's first prompt and CLI title, as with an ordinary draft task. Manual display names and externally named/published branches keep the existing protections.
- Quit preserves the tree and checkpoints its display name. Explicit Reopen offers Terminal even without installed agents and opens a fresh shell in that verified tree, without setup or restoring old programs/tabs. Its confirmation is **Open terminal**, exact Cmd+Enter. Agents remain available as alternative recovery choices.

## Checkout

With **Create worktree** off, Terminal is still the last row. It starts a [checkout session](tasks-and-worktrees.md#checkout) whose pinned pane is the login shell: the project folder, on the branch already checked out, with no agent, setup, journal, or worktree. One checkout per project, so an existing checkout card is focused instead of starting another. Shell commands do not name the card or drive activity, status, or notifications. The card shows Terminal, the checkout mark, and the current branch, and has no status signal. Close stops the shell immediately and never discards files; Shika does not inspect shell jobs, so save a running command first. Quit does not restore it. Switch agent keeps that shell as a Shell tab and starts the agent in the same folder. Changes compares against `HEAD` when opened. There is no Create PR and no leftover.

## Implementation

- `Core::create_terminal_with_preparation` and ordinary agent creation share `create_task`: identical base, consent, setup, cancellation, journal-first ownership and guarded failure cleanup. Terminal uses `session::shell_request` directly, not `open_agent`, so no provider flags, activity bridge, usage source or worker control environment is installed.
- `Session::is_terminal` identifies the reserved launch id `terminal`. A worktree Terminal stays `SessionKind::Task`; a checkout Terminal is `SessionKind::Checkout`. `terminal_choice` supplies display/launch metadata only and is never in the discovered CLI catalog.
- `Core::create_checkout_terminal` opens that same shell request in the repository root and stores a `SessionKind::Checkout` with the reserved id. It shares the checkout slot, close, and git refusal with an agent checkout.
- New uses `PickerMode::choices`; Task and Checkout append Terminal. Switch and Lead keep agent-only lists. Recovery explicitly appends Terminal.
- The pinned shell pane has input capture disabled and the activity tick skips it. `retarget_agent` transfers the old primary PTY into authoritative `shell_ptys`; the UI moves that same pane into `Card::shells`. Never close/recreate that shell or call `hang_up` when starting an agent.
- Recovery records `cli: terminal` with no provider conversation. Existing Git identity checks apply before a shell opens. Close still stops the primary and all recorded shell PTYs.

Tests cover agent-free launch, naming isolation, diff and dirty Close, metadata-only rename, fresh shell recovery without setup, shell preservation on agent start, failed switching, preparation consent/order and failure preservation, picker scope and uncaptured input, and a checkout shell's directory, shared slot, idle close, and switch. Native acceptance remains separate in [MANUAL_CHECKS.md](../MANUAL_CHECKS.md#terminal-tasks).
