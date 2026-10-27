# Contributor guides

Start with [AGENTS.md](../AGENTS.md) for architecture, constraints, and local build/testing instructions, and [CONTRIBUTING.md](../CONTRIBUTING.md) for the product principles and non-goals. Each guide holds the rules for its feature. For UI changes, read [design/DESIGN.md](../design/DESIGN.md).

The public how-to is [Using Shika](using-shika.md), the user guide for shortcuts, supported agents, and everyday workflows, plus the website guide in [site/guide](../site/guide/index.html). Update both when user-visible behavior changes. The guides below stay the contributor contract.

These guides explain why features exist, how they are implemented, and where to debug or extend them:

| Guide | Read when working on |
| --- | --- |
| [Using Shika](using-shika.md) | The user guide: every shortcut, supported agents and launch arguments, the Lead, worktree preparation, renaming, Create PR, and Close |
| [Tasks and worktrees](tasks-and-worktrees.md) | Projects, worktree creation, checkout sessions, base branch, dirty and unpushed checks, Close, and leftovers |
| [Agent activity and elapsed turns](agent-activity.md) | Timer-reset root cause, hybrid state detection, lifecycle reports, notifications, safe Close, and debugging |
| [Token usage](token-usage.md) | Local Claude Code and Codex token totals, the daily file, and Settings > Usage |
| [Terminal tabs](terminal-tabs.md) | Task-local agent/shell tabs, PTY ownership, startup, selection, close, and hidden terminals |
| [Changes panel](changes-panel.md) | The read-only diff beside the terminal: decision, refresh without watching, caps and performance, width beside the column, debugging, and guardrails |
| [Background image](background-image.md) | The optional picture behind the glass: decisions, Shika's managed copy, effects ported from Zeron, framing and the Adjust dialog, debugging, and extending |
| [Keyboard flow](keyboard-flow.md) | Native shortcuts, terminal/card focus, overlay restoration, and selection scrolling |
| [Worktree preparation](worktree-preparation.md) | Feature evolution and decisions, per-project first-New onboarding, Settings editor, suggestions, consent/state routing, copy/setup lifecycle, debugging, and contribution guardrails |
| [Confirmed PR publishing](publishing.md) | Decision rationale, recorded-base targeting, approved-tree checks, commit/push/gh lifecycle, debugging, retries, and contribution guardrails |
| [Switch agent](agent-switch.md) | Switching an idle task to another CLI in the same worktree, to continue the work or to review it |
| [PR checks on the card](pr-checks.md) | The read-only CI mark after Create PR, its polling schedule, head matching, classification, notifications, and debugging |
| [Manual task names](task-names.md) | Rename task, display-name versus branch identity, commit/PR defaults, automatic-title priority, and the public FAQ |
| [Branch naming](branch-naming.md) | First-prompt names, CLI session-title discovery, branch renaming, and fallbacks |
| [Software updates](updates.md) | The update notice, Sparkle loading, the signed feed and its redirect, releasing, signing keys, and testing an update locally |
| [Branch-switch close](branch-switch-close.md) | Safe recovery after switching branches, publication checks, branch preservation, confirmation rechecks, and debugging |
| [Lead agent](lead-agent.md) | The decision and contract for one Lead per project that starts worker cards, waits for them, reads and steers them, and is woken by a doorbell, through the `shika` command |
| [Lead chat](lead-chat.md) | The Claude Code Lead's native chat: evolution and decisions, stream-JSON transport, final-response filter, lifetime history, doorbell and status, debugging, fixtures, and adding Codex, Cursor CLI, and Pi |
| [The `shika` command](shika-cli.md) | Reference for the Lead's control CLI: commands and exact output, exit codes, `wait` rules, wire protocol, token and scope, debugging recipes, and adding a command |

[MANUAL_CHECKS.md](../MANUAL_CHECKS.md) and [terminal checks](../crates/shika-terminal/MANUAL_CHECKS.md) track acceptance evidence and remaining GUI checks. Passing unit tests is not proof that all native UI behavior has been exercised.
