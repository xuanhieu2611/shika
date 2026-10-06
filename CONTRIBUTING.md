# Contributing to Shika

Shika is a macOS app written in Rust with GPUI. It launches existing coding-agent CLIs in isolated git worktrees. Small, focused issues and pull requests are welcome.

## Orientation

Start with [AGENTS.md](AGENTS.md) for the codebase map, guardrails, and toolchain traps. [PRD.md](PRD.md) defines product scope; [PLAN.md](PLAN.md) records later decisions and wins where they disagree. Before UI work, read [design/DESIGN.md](design/DESIGN.md).

Feature guides explain what changed, why, implementation symbols, debugging, and safe extension:

| Working on | Read first |
| --- | --- |
| Worktree setup, local files, approval, cancellation, or retry | [Worktree preparation](docs/worktree-preparation.md), especially its [contributor guide](docs/worktree-preparation.md#contributor-guide) |
| Command shortcuts, focus restoration, or selected-row scrolling | [Keyboard flow](docs/keyboard-flow.md) |
| Task titles and branch names from CLI metadata | [Branch naming](docs/branch-naming.md) |

[MANUAL_CHECKS.md](MANUAL_CHECKS.md) records integrated acceptance evidence and remaining checks. [Terminal checks](crates/shika-terminal/MANUAL_CHECKS.md) cover the terminal-specific matrix. Passing unit tests do not establish every native GUI behavior. Machine-local temporary evidence is not a prerequisite for contributing; use the versioned tests and each guide's isolated reproduction steps.

## Development

Install Rust and full Xcode, then follow the README build instructions. Keep the pinned GPUI revision unless the change explicitly upgrades it. Do not copy GPL code from Zed's terminal crates.

Before submitting a change, run:

```sh
cargo fmt --all --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Use disposable repositories and an isolated app data directory for integration tests. Never test worktree deletion against personal projects. Verify the app window is in front before automated keyboard input.

Describe what changed, why, and how it was verified. Include a screenshot for visible UI changes. Check PLAN.md for current scope and decisions before adding features. Update the matching feature guide and the short AGENTS.md handoff when behavior, ownership, or a toolchain trap changes. Record actual acceptance results separately from intended behavior; do not turn an unchecked GUI item into a pass based only on implementation or unit tests.

## License

Contributions are made under the project's MIT license. Keep third-party license notices with bundled assets.
