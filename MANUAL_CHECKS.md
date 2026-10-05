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

## Notification sound

- [ ] A ready notification plays the system alert sound while another app is active, and also when Shika is in front.
- [ ] Settings can turn the sound off. The banner still arrives, and the choice survives a relaunch.
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
- [ ] Font size changes the terminal text from 8 to 32. The default is 12.5. h / l steps by 1, a typed value such as 14 or 12.5 applies on Enter, live cards resize, a new card uses it, and it survives a relaunch.
- [ ] The title bar shows Shika on the left and a settings gear on the right. Hovering the gear shows Settings ⌘,, and clicking it opens the dialog. Dragging the bar moves the window.
- [ ] The title bar uses the same opacity and blur as the sidebar. At 100% it is solid. Below that, the blurred desktop shows through the whole bar, including above the terminal.

## Branch names from the CLI title, 2026-10-05

Checked without the GUI: the installed Claude Code (2.1.289) and Cursor CLI (2026.10.01) were started in disposable worktrees under `/private/tmp` with the launch args from `PLAN.md` and given one conversational prompt. Each wrote its session title about 1.1 seconds after Enter: Claude "Shika settings dark mode toggle", Cursor "Shika Dark Mode". Shika's readers found both, and the slugs in project `shika` were `settings-dark-mode-toggle` and `dark-mode`.

To check in the built app, with isolated data and a disposable repo:

- [ ] With Claude, the branch shown on the card starts as the prompt slug and becomes the CLI title within a few seconds. The card title becomes the CLI title. `git branch --show-current` in the shell agrees, and the folder stays `shika-draft-<id>`.
- [ ] The same with Cursor.
- [ ] A prefix such as `hieu` set in Settings gives `hieu/<title>` on the next card. Enter or a click edits it, Escape cancels, and it survives a relaunch.
- [ ] A branch pushed before the title arrives keeps its name; the card still takes the title.
- [ ] Clearing a few words with Option+Backspace before the first Enter leaves them out of the first branch name.

## Base branch, 2026-10-05

Checked with isolated app data, a disposable repo, and a local bare remote with `main` and a `dev` one commit ahead (only on origin). This session had no Accessibility permission, so no synthetic key could reach any app. A scratch-only copy of the app (not in the repo) called the same handlers the keys and clicks call (`open_base`, `apply_base`, `picker`, `launch`, `close`) and saved frames with GPUI's `render_to_image`.

- PASS: The header shows `main` after the path with no base set, and `dev` once set. With a saved base that no longer exists, the label is hidden.
- PASS: The dialog opens empty with `main` as the placeholder and "Use default branch", or prefilled with `dev` and "Set base branch". `nope` shows "No branch named nope on origin or locally." and stays open. `dev` saves `"baseBranch": "dev"` and closes.
- PASS: The picker footer reads "branches from `dev`".
- PASS: With a newer commit pushed to origin's dev from another clone, opening the picker fetched it, and New with Claude Code started at that commit, with no upstream and `baseRef` in `worktrees.json`. Close on that Waiting card removed the worktree and the draft branch at once.
- PASS: With the saved base renamed to a missing branch, New showed the toast "Base branch gone not found." and created no worktree.
- PASS: Dark, light, and dark glass render the label, dialog, error, and picker footer with existing tokens.
- [ ] `b` with a project header or a card selected opens the dialog; typing in the field runs no card keys; Enter applies; Escape cancels.
- [ ] A click on the header label opens the dialog; hovering it shows "Base branch b".
- [ ] A fresh card on a dev base shows no diff stat once Ready, and a commit in the shell shows its own stat and makes Close ask.

## Automated checks

Workspace tests cover PTY streaming, hidden terminal drains, terminal input, git exclusion, branch renaming, CLI title reading (sample, missing, and broken files), branch slugs without the project name, prefix cleanup, renames that skip taken and pushed names, the title watch timing, Option+Backspace in prompt capture, journal updates, base branch start points (origin, local, missing, unset with origin/HEAD, a changed base, a vanished base, fetch freshness, a dead remote, and waiting for the picker fetch), unpushed commits, failed pushes, branch retention, safe close, branch-switch refusal, and leftovers. All use temporary app data and repositories.

Final checks passed: cargo fmt --all --check, cargo test --workspace (158 tests: 92 core, 46 terminal, 20 app; base branch tests added 2026-10-05), cargo clippy --workspace --all-targets -- -D warnings, and strict bundle signature verification.

IME candidate placement, sustained typing feel, and the broader terminal matrix remain human checks in crates/shika-terminal/MANUAL_CHECKS.md. They are not claimed as acceptance passes here.
