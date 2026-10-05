# Contributing to Shika

Shika is a macOS app written in Rust with GPUI. It launches existing coding-agent CLIs in isolated git worktrees. Small, focused issues and pull requests are welcome.

## Development

Install Rust and full Xcode, then follow the README build instructions. Keep the pinned GPUI revision unless the change explicitly upgrades it. Do not copy GPL code from Zed's terminal crates.

Before submitting a change, run:

```sh
cargo fmt --all --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Use disposable repositories and an isolated app data directory for integration tests. Never test worktree deletion against personal projects. Verify the app window is in front before automated keyboard input.

Describe what changed, why, and how it was verified. Include a screenshot for visible UI changes. Check PLAN.md for current scope and decisions before adding features. `docs/` explains individual features in depth; read the matching file before changing one, and update it with the change.

## License

Contributions are made under the project's MIT license. Keep third-party license notices with bundled assets.
