# Contributing to Shika

Shika is a macOS app written in Rust with GPUI. It launches existing coding-agent CLIs in isolated git worktrees. Small, focused issues and pull requests are welcome.

## Orientation

Start with [AGENTS.md](AGENTS.md) for the codebase map, guardrails, and toolchain traps. Read the [product principles](#product-principles) and [non-goals](#not-planned) below before proposing a feature. Before UI work, read [design/DESIGN.md](design/DESIGN.md).

Feature guides explain what changed, why, implementation symbols, debugging, and safe extension:

| Working on | Read first |
| --- | --- |
| Worktree creation, base branch, dirty and unpushed checks, Close, or leftovers | [Tasks and worktrees](docs/tasks-and-worktrees.md) |
| Agent state, elapsed timers, notifications, or activity-based Close checks | [Agent activity and elapsed turns](docs/agent-activity.md), including its [debugging playbook](docs/agent-activity.md#debugging-playbook) |
| Worktree setup, local files, approval, cancellation, or retry | [Worktree preparation](docs/worktree-preparation.md), especially its [contributor guide](docs/worktree-preparation.md#contributor-guide) |
| Command shortcuts, focus restoration, or selected-row scrolling | [Keyboard flow](docs/keyboard-flow.md) |
| Task titles and branch names from CLI metadata | [Branch naming](docs/branch-naming.md) |
| Close after the user switched branches | [Branch-switch close](docs/branch-switch-close.md) |
| Agent and shell tabs, PTY ownership, or hidden terminals | [Terminal tabs](docs/terminal-tabs.md) |
| Create PR, recorded-base targeting, commit/push, or publishing retries | [Confirmed PR publishing](docs/publishing.md), especially its [debugging playbook](docs/publishing.md#debugging-playbook) and [contributor guardrails](docs/publishing.md#contributor-guardrails-and-extension-points) |

[MANUAL_CHECKS.md](MANUAL_CHECKS.md) records integrated acceptance evidence and remaining checks. [Terminal checks](crates/shika-terminal/MANUAL_CHECKS.md) cover the terminal-specific matrix. Passing unit tests do not establish every native GUI behavior. Machine-local temporary evidence is not a prerequisite for contributing; use the versioned tests and each guide's isolated reproduction steps.

## Product principles

These hold for every feature. A change to one is a decision for the maintainer, so open an issue first.

- **A Mac app for coding-agent tasks.** It is not a terminal multiplexer, an editor, or a chat client. Pure Rust on GPUI, with no web view.
- **Bring your own CLI.** Shika launches Claude Code, Codex, Cursor CLI, or Pi with the user's own login and plan. It does not call a model API.
- **The real terminal is the interface.** The user reads and answers the agent in the CLI's own UI. Shika does not draw a transcript or replace a CLI's question widgets.
- **No approval prompts from Shika.** Each CLI starts in its automatic approval mode, using flags checked against its `--help` (see [AGENTS.md](AGENTS.md#clis-and-launch-arguments)).
- **One fresh worktree per task.** Tasks in a repository are independent. Shika does not reuse worktrees or merge between agents.
- **The user owns git.** Shika never commits or pushes on its own. A push does not close a task, and Close asks before anything is lost. Create PR commits only after an explicit confirmation.
- **Grouped by project, one terminal on screen.** Projects and their task cards in one column, and the selected task's terminal beside it.
- **Keyboard first.** Every workflow works without the mouse, and typing in a terminal never triggers app keys.
- **Nothing hidden.** No telemetry, no accounts, and no global hooks or edits to another tool's configuration.

## Not planned

Open an issue before working on any of these:

- Windows, Linux, web, or phone builds.
- Accounts, sync, or telemetry.
- A planner agent that splits work between agents.
- Merging PRs, managing reviews, or review comments.
- Reusing or archiving worktrees, or a CLI's own worktree flag.
- Conversation history or a browser of closed tasks.
- More than one visible terminal, or splits between terminals.
- A code editor.
- Installing the CLIs.
- A custom chat transcript or a per-CLI question parser.
- Closing a card automatically after `git push`.

## Development

Install Rust and full Xcode, then follow the README build instructions. Keep the pinned GPUI revision unless the change explicitly upgrades it. Do not copy GPL code from Zed's terminal crates.

Before submitting a change, run:

```sh
cargo fmt --all --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Use disposable repositories and an isolated app data directory for integration tests. Never test worktree deletion against personal projects. Verify the app window is in front before automated keyboard input.

Describe what changed, why, and how it was verified. Include a screenshot for visible UI changes. Check the product principles and non-goals above before adding features. Update the matching feature guide and the short AGENTS.md handoff when behavior, ownership, or a toolchain trap changes. Record actual acceptance results separately from intended behavior; do not turn an unchecked GUI item into a pass based only on implementation or unit tests.

## License

Contributions are made under the project's MIT license. Keep third-party license notices with bundled assets.
