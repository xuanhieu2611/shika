# MVP acceptance checks

Use a built .app opened with open, an isolated --data-dir, disposable repositories, and local bare remotes. Inspect Shika's own foreground window before synthetic input.

## Acceptance record, 2026-10-04

The PLAN Done when flow was exercised with two disposable git repositories and the installed, authenticated Claude Code and Cursor CLI. App data was isolated.

- PASS: The app opened with open discovers both binaries under ~/.local/bin through the login-shell PATH.
- PASS: Claude and Cursor launch in separate Shika worktrees. Terminals measure their pane before the PTY starts.
- PASS: Real Claude tool output, multiple-choice questions, keyboard answers, and Cursor task output render in their own terminal UI.
- PASS: An unsent prompt survives switching cards. Hidden PTYs drain. Terminal typing does not trigger card shortcuts. Ctrl+Q restores navigation. Escape is typed into the terminal.
- PASS: The first prompt updates title and branch. The worktree directory stays fixed.
- PASS: About two seconds of quiet changes Working to Ready to check.
- PASS: A native notification arrives while another app is active. The author saw and clicked it; the delegate recorded the expected session ID and Shika selected that card. Notification permission was enabled with approval.
- PASS: Four cards collapse to three; keyboard navigation reveals the hidden card.
- PASS: Shell status and staged diff show task changes. Shell commit and push leave the card open.
- PASS: Closing a clean pushed task removes its worktree, retains its branch, and leaves the other CLI alive.
- PASS: Dirty close hides Push. Cancel focuses the task shell with work intact. After a shell commit, close offers Push.
- PASS: A rejecting local remote causes a toast and retains the card and worktree. Retrying pushes and closes only that task.
- PASS: Quit preserves projects and worktrees. Relaunch restores projects without live sessions and lists leftovers. Explicit discard removes a leftover worktree and branch.
- PASS: Two repositories add through the native folder picker and survive relaunch. A nested folder resolves to its repository root and duplicate projects are prevented.

## Appearance settings, 2026-10-04

The author checked the first version by eye and it looked good. Typed values and the wider ranges (opacity 0 to 100, blur 0 to 255) are not yet checked by eye.

- [ ] At 100% opacity the window looks as before and blur has no effect.
- [ ] Below 100%, the sidebar shows the blurred desktop. Blur 0 is see-through without blur. Raising blur softens the desktop behind.
- [ ] Sidebar only keeps the terminal and its header opaque. Sidebar and terminal makes both translucent, with CLI-colored cells still opaque and text readable.
- [ ] Changes apply live from the dialog, keyboard and mouse, and survive a relaunch.
- [ ] Typing digits or clicking the number edits it. Enter applies, Escape cancels, 300 becomes 255, and blank keeps the old value.
- [ ] The title bar shows Shika on the left and a settings gear on the right. Hovering the gear shows Settings ⌘,, and clicking it opens the dialog. Dragging the bar moves the window.
- [ ] The title bar uses the same opacity and blur as the sidebar. At 100% it is solid. Below that, the blurred desktop shows through the whole bar, including above the terminal.

## Automated checks

Workspace tests cover PTY streaming, hidden terminal drains, terminal input, git exclusion, branch renaming, journal updates, unpushed commits, failed pushes, branch retention, safe close, branch-switch refusal, and leftovers. All use temporary app data and repositories.

Final checks passed: cargo fmt --all --check, cargo test --workspace (108 tests: 55 core, 44 terminal, 9 app), cargo clippy --workspace --all-targets -- -D warnings, and strict bundle signature verification.

IME candidate placement, sustained typing feel, and the broader terminal matrix remain human checks in crates/shika-terminal/MANUAL_CHECKS.md. They are not claimed as acceptance passes here.
