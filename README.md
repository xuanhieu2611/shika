# Shika

Keyboard-first Mac app for running Claude Code and Cursor CLI in separate git worktrees. Pure Rust on GPUI, with one real terminal visible at a time. Shika launches the CLI tools you already use, without wrapping a model API. Early-stage, macOS-only software. See `PRD.md` for scope and `PLAN.md` for implementation decisions.

## Build and run

Requires macOS, Rust, full Xcode, and an installed, authenticated Claude Code or Cursor CLI. GPUI is pinned in the root `Cargo.toml`. The current build uses runtime Metal shaders because the separate Metal Toolchain component is not installed.

```sh
source "$HOME/.cargo/env"
cargo test --workspace
cargo run -p shika
```

Build a local app bundle:

```sh
./scripts/bundle-app.sh --debug
open target/debug/Shika.app
```

Without `--debug`, the script builds the release bundle. Local bundles are ad hoc signed; distribution signing and notarization are outside this MVP.

JetBrains Mono and its OFL license are bundled. The chrome uses the macOS system font. The app captures the login-shell PATH at startup to find `claude` and `agent` when launched from Finder.

## Isolated checks

Never run automated UI tests against your normal app data. Use a fresh directory and disposable repositories with local bare remotes:

```sh
open -n target/debug/Shika.app --args --data-dir /absolute/path/to/test-data
```

Confirm Shika's own window is in front before sending keystrokes. `cargo run` alone does not prove Finder PATH discovery. See `MANUAL_CHECKS.md` for the acceptance record and `crates/shika-terminal/MANUAL_CHECKS.md` for terminal-specific checks.

## Use

`a` adds a git repository. `n` opens the CLI picker. `j` / `k` or arrows select project headers and cards. Enter focuses the terminal; Ctrl+Q returns to cards. Escape is typed into the terminal. `g` toggles the agent and the worktree shell. `c` closes the selected task.

The first submitted prompt names the card and branch. A push keeps the card open. Close offers discard or push when work would be lost. The app never commits. Quit keeps worktrees; the next launch lists leftovers for explicit cleanup.

Projects and the worktree journal live in `~/Library/Application Support/com.hieule.shika/`. Live sessions do not restore.

## Contributing

Issues and pull requests are welcome. See [CONTRIBUTING.md](CONTRIBUTING.md) for development checks.

## License

MIT. Bundled JetBrains Mono is licensed separately under the SIL Open Font License; see `assets/fonts/OFL.txt`.
