<div align="center">

<img src="design/assets/shika-app-icon.png" alt="Shika app icon" width="128" height="128">

# Shika

**Run your coding agents side by side.**

Shika handles the worktrees, branches, and terminals. Every task isolated. Every project organized.

[![Download for Mac](https://img.shields.io/badge/Download_for_Mac-Shika.dmg-4d6745?style=for-the-badge&logo=apple&logoColor=white)](https://github.com/xuanhieu2611/shika/releases/latest/download/Shika.dmg)

[Website and interactive demo](https://useshika.com) · [Releases](https://github.com/xuanhieu2611/shika/releases) · [Contributing](CONTRIBUTING.md)

![macOS 13+](https://img.shields.io/badge/macOS-13%2B-555?logo=apple&logoColor=white) ![Apple silicon](https://img.shields.io/badge/Apple_silicon-arm64-555) ![Rust on GPUI](https://img.shields.io/badge/Rust-GPUI-555?logo=rust&logoColor=white) [![MIT license](https://img.shields.io/badge/license-MIT-555)](LICENSE)

</div>

<br>

![Shika running parallel tasks across two sample projects, with an agent terminal and task-owned shell tabs](docs/images/shika-native.png)

<p align="center"><sub>Native app screenshot with disposable sample projects and simulated CLI output. The website demo is an interactive prototype.</sub></p>

## What it is

A keyboard-first Mac app for running Claude Code, Codex, Cursor CLI, and Pi at the same time. Press New, and Shika creates a Git worktree and opens the CLI you already use, with your existing login and plan. It does not wrap a model API.

I built Shika to stop juggling worktrees and terminals when working with several coding agents. I use it every day, including to build Shika itself.

- **A worktree per task.** Each agent gets its own branch and worktree under `.worktrees/`, started from the project's base branch.
- **Branches that name themselves.** The first prompt names the branch, and the CLI's own session title can refine it a second later.
- **Knows who needs you.** Each card shows whether its agent is working, asking you something, or ready to check, and a notification names the task.
- **Shells next to the agent.** Add terminal tabs in the same worktree to review, test, commit, and push.
- **Closing is safe.** Close asks before throwing away uncommitted work or unpushed commits, and never commits for you.
- **Create PR in one step.** Confirm the files and title, and Shika commits, pushes, and opens a GitHub PR against the task's original base.

## Install

> [!NOTE]
> Shika is early-stage, macOS-only software. Bugs and incomplete workflows are tracked in [MANUAL_CHECKS.md](MANUAL_CHECKS.md); a passing test suite does not establish every GUI check.

[Download Shika.dmg](https://github.com/xuanhieu2611/shika/releases/latest/download/Shika.dmg), open it, and drag Shika to Applications. The download is signed and notarized.

You need:

- An Apple silicon Mac with macOS 13 or later.
- At least one supported CLI, installed and signed in.

Shika does not update itself yet. Download each new version from [Releases](https://github.com/xuanhieu2611/shika/releases).

## Supported agents

Shika finds each CLI through your login shell's PATH, including when launched from Finder. Your CLI subscription or provider charges still apply.

| Agent | Command | Launch arguments |
| --- | --- | --- |
| Claude Code | `claude` | `--dangerously-skip-permissions` |
| Codex | `codex` | `--dangerously-bypass-approvals-and-sandbox` |
| Cursor CLI | `agent` | `--yolo --trust --sandbox disabled` |
| Pi | `pi` | `--approve` |

> [!WARNING]
> These presets launch each CLI in an automatic approval mode. Claude Code, Codex, and Cursor CLI bypass permission prompts or sandboxing, so agents can run commands with your user account's access. A worktree separates Git edits; it does not restrict access to files, credentials, or the network. Use Shika in projects you trust and review the agent's changes.

Shika has no telemetry or model API integration. Your CLIs connect to their providers, Git fetch and push connect to your remotes, and approved setup commands can use the network.

## How it works

1. **Add a project.** Press `a` and pick a Git repository.
2. **Start an agent.** Press Cmd+N and pick a CLI. Shika creates a fresh branch and worktree under that repository's `.worktrees/`.
3. **Write your prompt** in the terminal. The first prompt names the task and branch.
4. **Switch between tasks** with `j` / `k`. Enter focuses the terminal; Ctrl+Q returns to the cards. Ready means the agent finished its turn, so check the terminal for its result.
5. **Review and ship.** Press Cmd+T for a shell in that task's worktree. Test there, then commit and push yourself, or press Cmd+Shift+P to create a PR.
6. **Close the task** with Cmd+Shift+W.

One terminal is visible at a time; hidden task terminals keep running. Projects persist, live sessions do not restore, and quitting leaves worktrees for explicit cleanup on the next launch. App data lives in `~/Library/Application Support/com.hieule.shika/`.

### Keyboard shortcuts

| Keys | Action |
| --- | --- |
| `a` | Add a project |
| `b` | Change the selected project's base branch |
| Cmd+N | New agent in the selected project |
| `j` / `k` | Move between cards |
| Enter / Ctrl+Q | Focus the terminal / return to the cards |
| Cmd+] / Cmd+[ | Next / previous agent, from anywhere |
| Cmd+T / Cmd+W | Add a shell tab / close the selected shell |
| Ctrl+Tab, Cmd+1 to Cmd+9 | Cycle tabs, or jump to one (Cmd+1 is the agent) |
| Cmd+Shift+P | Create PR |
| Cmd+Shift+W | Close the task |
| Cmd+B | Hide or show the column |
| Cmd+, | Settings |

### Creating a PR

Create PR (Cmd+Shift+P) previews every non-ignored change and an editable title. Confirm it, and Shika stages, commits if needed, pushes, and creates a GitHub PR targeting the task's original base. It requires `gh auth login` and never merges. See [confirmed PR publishing](docs/publishing.md) for behavior, rationale, and troubleshooting.

### Closing a task

Close asks before discarding uncommitted work or unpushed commits, and offers push when the tree is clean. A `git push` keeps the card open, and Close never commits. A clean idle task on its own branch closes immediately. After a branch switch, Close can verify both branches and remove the worktree while keeping both local branches; unsafe or unverifiable work blocks that. See [branch-switch close](docs/branch-switch-close.md) for the safety rules.

## Worktree preparation

To prepare fresh worktrees before agents start, add `.shika/worktrees.json` to a project's main checkout:

```json
{
  "copy-files": [".env.local"],
  "setup-worktree": ["npm ci"],
  "timeout-seconds": 600
}
```

Use your project's own commands. Copied files must already be ignored by Git on both the main checkout and the task's base. Shika asks for local approval, and asks again when the configuration changes. Setup commands are trusted code, run with your account's access. See [worktree preparation](docs/worktree-preparation.md) for progress, cancel, retry, and cleanup rules.

## Build from source

You need macOS 13 or later, Rust, full Xcode (not only Command Line Tools), and at least one supported CLI, installed and signed in. GPUI is pinned in [Cargo.toml](Cargo.toml). Runtime Metal shaders mean you don't need Xcode's separate Metal Toolchain component.

```sh
git clone https://github.com/xuanhieu2611/shika.git
cd shika
source "$HOME/.cargo/env"
./scripts/bundle-app.sh
open target/release/Shika.app
```

Local bundles are ad hoc signed.

## Contributing

Shika is pure Rust on GPUI, in three crates:

| Crate | Owns |
| --- | --- |
| `shika-core` | Git, persistence, and processes |
| `shika-terminal` | Terminal rendering |
| `shika` | The app UI |

Issues and pull requests are welcome. Start with [CONTRIBUTING.md](CONTRIBUTING.md) for onboarding, product principles, and feature guides.

```sh
cargo fmt --all --check
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
./scripts/bundle-app.sh --debug
```

> [!IMPORTANT]
> Never run automated UI checks against your normal app data. Use disposable repositories and a fresh data directory:
>
> ```sh
> open -n target/debug/Shika.app --args --data-dir /absolute/path/to/test-data
> ```

Confirm Shika's own window is in front before sending keystrokes. A `cargo run` launch does not prove Finder PATH discovery. See [terminal checks](crates/shika-terminal/MANUAL_CHECKS.md) and the [acceptance record](MANUAL_CHECKS.md).

## License

Shika is [MIT licensed](LICENSE). Fonts and copied icons keep their upstream licenses; see [third-party notices](THIRD_PARTY_NOTICES.md). Native notices are included in the app bundle.
