# Contributor guides

Start with [AGENTS.md](../AGENTS.md) for architecture, constraints, and local build/testing instructions. [PRD.md](../PRD.md) is the product spec; later decisions in [PLAN.md](../PLAN.md) take precedence. For UI changes, read [design/DESIGN.md](../design/DESIGN.md).

These guides explain why features exist, how they are implemented, and where to debug or extend them:

| Guide | Read when working on |
| --- | --- |
| [Agent activity and elapsed turns](agent-activity.md) | Timer-reset root cause, hybrid state detection, lifecycle reports, notifications, safe Close, and debugging |
| [Terminal tabs](terminal-tabs.md) | Task-local agent/shell tabs, PTY ownership, startup, selection, close, and hidden terminals |
| [Keyboard flow](keyboard-flow.md) | Native shortcuts, terminal/card focus, overlay restoration, and selection scrolling |
| [Worktree preparation](worktree-preparation.md) | Setup approval, copy/setup commands, cancellation, retry, and failure cleanup |
| [Confirmed PR publishing](publishing.md) | Decision rationale, recorded-base targeting, approved-tree checks, commit/push/gh lifecycle, debugging, retries, and contribution guardrails |
| [Branch naming](branch-naming.md) | First-prompt names, CLI session-title discovery, branch renaming, and fallbacks |

[MANUAL_CHECKS.md](../MANUAL_CHECKS.md) and [terminal checks](../crates/shika-terminal/MANUAL_CHECKS.md) track acceptance evidence and remaining GUI checks. Passing unit tests is not proof that all native UI behavior has been exercised.
