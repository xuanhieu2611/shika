# MVP acceptance checks

Use a built .app opened with open, an isolated --data-dir, disposable repositories, and local bare remotes. Inspect Shika's own foreground window before synthetic input.

## Software updates, 2026-10-08

Exercised with the local recipe in `docs/updates.md`: bundle id `com.hieule.shika.updatetest`, a disposable `--data-dir`, and a feed served from 127.0.0.1. The test state was removed afterwards.

- PASS: `scripts/release-app.sh` built build 101 with Sparkle embedded; notarization accepted it, and it was stapled and passed Gatekeeper.
- PASS: `scripts/appcast.sh` wrote and signed the feed, and `sign_update --verify` accepted it. A second run against the published build 101 refused build 100.
- PASS: A Developer ID signed build 100 with automatic updates loaded Sparkle under the hardened runtime, fetched the signed feed, and downloaded and verified the DMG. On quit it was replaced in place by build 101, which `spctl` accepted as Notarized Developer ID.
- PASS: The release bundle's Shika menu lists "Check for updates..." first. The second launch showed Sparkle's "Check for updates automatically?" prompt. The menu item fetched the feed and reported "You're up to date!"
- [ ] Install and Relaunch from the update window, on a real release: Shika quits, updates, and reopens with projects intact.
- [ ] The update window renders the Markdown release notes legibly in light and dark.
- [ ] `cargo run` and a `bundle-app.sh` bundle show no "Check for updates..." item.
- [ ] The first real release after 0.3.0 reaches an installed 0.3.0 through `https://useshika.com/appcast.xml`.

## Themes, 2026-10-07

Automated validation: workspace tests, formatting, and strict Clippy passed, and a debug bundle was built. Tests cover `theme` defaults, unknown and wrong-side ids, mode stepping, a byte-identical Shika chrome and palette snapshot across solid, glass, and Reduce transparency, and, for every catalog theme plus synthetic dark, light, and low-contrast themes, 4.5:1 secondary and status text on the column, the active tab matching the terminal, the shared column background, and solid popups. The bundle was opened with a disposable `--data-dir` and a forced mode opposite macOS's; it started and was quit. No screenshots or input were taken, so nothing below is established.

- [ ] Theme Light and Dark force the window, traffic lights, menus, and tooltips whatever macOS says; System follows a live macOS appearance change again, in both directions.
- [ ] `h` / `l` on Light theme and Dark theme step and wrap through the catalog, repaint the chrome, and recolor every live agent and shell terminal at once. The choice survives a relaunch.
- [ ] Several derived themes (Catppuccin, Rosé Pine, Tokyo Night, Dracula, Gruvbox) look like themselves: cards, selected card, dialogs, key caps, segmented tracks, toast, status dots, timers, and tints, opaque and in glass, with frost on the sidebar alone and on the terminal.
- [ ] The Settings panel scrolls in a 600px-high window and `j` / `k` keep the selected row in view.

## Confirmed PR publishing

Automated validation: 285 workspace tests, formatting, strict Clippy, debug app bundle, and strict signature verification passed. Sixteen publishing tests use real disposable Git repos/worktrees, fake gh, and a local bare push transport. They cover recorded main/dev mapping, explicit missing-target selection, origin mapping, preview preserving the real index, ignored files, stale previews, commit/push/target arguments, existing PR reuse excluding same-named fork branches, partial-failure retries without duplicate commits, hook failure preserving staged work, unfinished merge refusal, dirty-submodule detection, switched-source refusal, and bounded subprocess timeout. No real GitHub PR or normal app data was changed.

Remaining integrated checks (not established by unit/fixture tests):

- [ ] Built .app opened via `open -n ... --args --data-dir /disposable/data` discovers gh outside system PATH; missing gh/auth failures state the cause without staging work.
- [ ] Metadata Create PR, Agent menu, and Cmd+Shift+P work from cards and terminals; active/blocked agents, busy work, and overlays prevent conflicting entry. Cancel restores focus and unsent terminal drafts.
- [ ] Light/dark, opaque/glass, hidden column, and minimum-size window: fields, file-list scrolling, branch-list selection, busy copy, and inline errors render legibly.
- [ ] Disposable GitHub repository: start from main and dev, change project base afterward, confirm PR still targets each task's original base; delete its base and verify explicit selection with no fallback.
- [ ] Review the diff and test manually, then publish edited/new/deleted files: normal hooks/identity/signing apply, title can be edited, description uses commits, GitHub URL opens, and task/PTYS remain alive. No merge happens.
- [ ] Network/auth/protected-branch rejection and PR failure retain work. Reopen and retry; no empty/duplicate commit or duplicate PR. Existing PR receives subsequent approved commits.
- [ ] Shell edits/commits/branch switches after preview refuse stale confirmation. Rebase/merge/conflicts refuse publishing. Close remains the existing non-committing safe-close flow.

## Agent activity and stable turn timer, 2026-10-07

Automated validation: 269 workspace tests after rebasing onto current main, formatting, strict Clippy, debug app bundle, and strict signature verification passed. Tests cover immutable turn epochs during typing/interactions/active Enter and Ready/Working flicker, once-per-turn notification budgets, blocked safe close, candidate/history submissions and native editor clearing, literal text questions, stale/quoted status chrome, content-based redraw/draft filtering, scrollback-independent live sampling without paint side effects, old lifecycle reads crossing submissions, same-sequence recovery, bridge bounds/order/cleanup, and optional launch failure.

The actual installed Pi loader loaded the temporary extension with no errors. A real Pi TUI in a disposable cwd and disposable Pi configuration emitted startup `{seq:1,state:"idle"}` while running, without submitting a prompt or making a model request. This verifies startup loading/reporting, not a full provider turn or GUI interaction. No normal app data or user/global CLI configuration was changed. See [agent activity](docs/agent-activity.md).

Remaining integrated checks (not established by fixture/unit tests):

- [ ] With each real CLI Working, type a draft, accidentally press a key, click other cards, focus/unfocus the window, scroll history, switch shell tabs, and resize. Timer retains the turn epoch; status does not falsely finish from interactions.
- [ ] Submit a new prompt, including Up/Enter history recall after completion. It gets a new timer and notification budget. Enter while Working or answering a blocker does not reset the current epoch.
- [ ] Long quiet tool/model work retains Working when live working chrome or a lifecycle report remains. A final ordinary text question can become Ready to check.
- [ ] Actual permission/structured-question dialogs become Asking you and resume without resetting time. Cancellation, interruption, rejected requests, and process failure settle correctly.
- [ ] First blocker or completion notifies once; later completion still produces an unseen result and fresh diff stat. Notification click selects the task.
- [ ] Close protects active and blocked clean tasks; idle drafts do not create a false Working warning. Existing dirty/unpushed/branch-switch safety remains intact.
- [ ] Pi completes fast and continued/retried turns correctly with the temporary extension. Missing/unsupported reports fall back; multiple concurrent sessions do not cross-report.
- [ ] Asking amber dot/tint, hints, ordering, and timer chrome render correctly in light/dark, opaque/glass, narrow/wide, and hidden column modes.

## Resizable and hideable agent column, 2026-10-06

Automated validation: workspace tests (including `column` settings defaults, clamping, and round trip), formatting, and Clippy passed. A debug build with disposable data was driven with synthetic mouse and key events while its own window was frontmost: dragging the edge saved the new width once the drag ended, the footer dropped its hints at 364px, double-click restored 540px, the icon buttons and Cmd+B (from a focused shell) hid and showed it, and the hidden header placed the button after the traffic lights. Card truncation at 320px was checked once. Light mode, full screen, and translucency were not checked.

- [ ] Drag the edge with a real mouse all the way to the window's left edge: the column stops at 320px and stays on screen. The resize cursor stays through the drag, the line shows on hover and while dragging, and a TUI (Claude Code, Codex) redraws cleanly at the new width without the card turning Working.
- [ ] Hidden column: Cmd+B, View menu, and both buttons toggle it from cards and from a terminal; Cmd+]/Cmd+[ and `j`/`k` change the agent on screen; dialogs and the picker still open centered.
- [ ] A small window clamps the column so the terminal keeps 420px; growing the window brings the saved width back. Quit and relaunch keeps width and hidden state.
- [ ] Light and dark, opaque and translucent, and full screen (button at the 18px inset with no traffic lights).

## Close after a branch switch, 2026-10-05

Automated validation: all 204 workspace tests passed, including rename-then-PR-checkout recovery, protection across Close/Discard/Push, and distinguishing detached HEAD from a genuine git failure. Formatting and diff checks passed. The reported live worktree was inspected read-only; its checkout was not changed.

- [ ] In an isolated app, rename a task branch, let the card refresh, and switch to an existing PR branch. Close names the current and task branches, without the misleading git-status prefix, in light and dark mode.
- [ ] Return to the renamed task branch in that card's shell and Close again. The task closes through the normal checks, and the separate PR branch remains.

## Task-scoped terminal tabs

Automated validation: 193 workspace tests, formatting, strict Clippy, debug app bundle, and strict signature verification passed. Coverage includes independent shell PTYs, refusing to close the agent or another task's shell, closing all remaining PTYs with the task, tab wrap navigation, selection after removal, and Command keys staying out of PTY input.

The debug bundle was opened with disposable repository/data under `/tmp/shika-tabs-gui.*`. Screen capture was unavailable, and the foreground PID guard blocked synthetic input when the isolated app was no longer frontmost. No native tab interaction or light/dark visual check is claimed as passed. The isolated process was stopped; the normal Shika process/data was not modified.

- [ ] New shows only the CLI tab; `+` and Cmd+T add shells in the same worktree, not new worktrees.
- [ ] Run Neovim in one shell and lazygit in another. Switch among all three tabs; contents, processes, unsaved agent drafts, and task status remain independent.
- [ ] Switch cards and return: each task restores its selected tab. Hidden shells continue draining output.
- [ ] Ctrl+Tab / Ctrl+Shift+Tab wrap tabs. Cmd+1 is the agent and Cmd+2 onward select shells; a missing number does nothing. Cmd+] / Cmd+[ still switch tasks; Cmd+N still opens New; Cmd+Shift+W closes the task. Plain `n` and `c` do nothing. Tab actions do nothing in pickers/dialogs or while busy, and Ctrl+Tab does not reach the CLI.
- [ ] Cmd+W and `×` close only a shell, including an exited shell. Agent tab has no close control and ignores Cmd+W. Closing one shell leaves the agent, other shells, worktree, and git state intact.
- [ ] Close an active shell, an inactive shell before the active one, and the last shell: selection and focus stay valid; numbered labels do not reuse closed numbers.
- [ ] First shell creation queues immediate typing; Ctrl+Q during startup stays on cards after completion. Failed startup removes only its new tab without stealing focus.
- [ ] Many tabs at 960x600: strip scrolls horizontally, keyboard-selected tab is revealed, `+` and Close task remain reachable, path truncates, metadata stays readable.
- [ ] Light/dark and glass: selected tabs, close hover, tooltips, path row, and terminal backgrounds follow existing tokens. Empty title-row space still drags/double-clicks the window.
- [ ] Connected tabs, 2026-10-05: the active tab reads as one surface with the path row and terminal below it, with no seam or line under it, in solid light/dark, sidebar-only glass, and glass covering the terminal (try 85% and 100%). Inactive tabs have no fill until hovered. `×` shows on the active shell and on hover only, without moving the label. `+` sits right after the last tab and stays visible when tabs overflow. Close task lines up with the tab labels. Empty space above the tabs drags the window. Unit test `the_active_tab_matches_the_terminal_in_every_mode` covers the color math. The author reviewed the debug bundle with disposable data and approved the look; which translucency modes were tried was not recorded, so this stays open for a full pass.
- [ ] Dirty Close task cancellation reuses the selected shell or selects/creates the first shell. Confirmed Close task stops every terminal; quit retains worktrees and does not restore live tabs.

## Acceptance record, 2026-10-04

The MVP acceptance flow was exercised with two disposable git repositories and the installed, authenticated Claude Code and Cursor CLI. App data was isolated.

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
- [ ] Font size changes the terminal text from 8 to 32. The default is 14. h / l steps by 1, a typed value such as 13 or 12.5 applies on Enter, live cards resize, a new card uses it, and it survives a relaunch.
- [ ] The title bar shows Shika on the left and a settings gear on the right. Hovering the gear shows Settings ⌘,, and clicking it opens the dialog. Dragging the bar moves the window.
- [ ] The title bar uses the same opacity and blur as the sidebar. At 100% it is solid. Below that, the blurred desktop shows through the whole bar, including above the terminal.

## Branch names from the CLI title, 2026-10-05

Checked without the GUI: the installed Claude Code (2.1.289) and Cursor CLI (2026.10.01) were started in disposable worktrees under `/private/tmp` with the launch args in `AGENTS.md` and given one conversational prompt. Each wrote its session title about 1.1 seconds after Enter: Claude "Shika settings dark mode toggle", Cursor "Shika Dark Mode". Shika's readers found both, and the slugs in project `shika` were `settings-dark-mode-toggle` and `dark-mode`.

To check in the built app, with isolated data and a disposable repo:

- [ ] With Claude, the branch shown on the card starts as the prompt slug and becomes the CLI title within a few seconds. The card title becomes the CLI title. `git branch --show-current` in the shell agrees, and the folder stays `shika-draft-<id>`.
- [ ] The same with Cursor.
- [ ] A prefix such as `dev` set in Settings gives `dev/<title>` on the next card. Enter or a click edits it, Escape cancels, and it survives a relaunch.
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
- [ ] The dialog lists local and origin branches. Typing filters by prefix. Up and Down move the highlight, Enter sets the highlighted branch, a click sets that branch, and a name that is not listed is fetched from origin. Empty with no highlight uses the default. The main checkout's branch is marked "checked out" and is not selected on its own.
- [ ] A click on the header label opens the dialog; hovering it shows "Base branch b".
- [ ] A fresh card on a dev base shows no diff stat once Ready, and a commit in the shell shows its own stat and makes Close ask.

## Command keyboard flow

Implemented with automated coverage for agent-only traversal (headers, wrapping, no agents, one agent, and missing selection), minimal selected-row scroll adjustment, and terminal Command-key isolation. Validation passed: cargo fmt --all --check, cargo test --workspace (166 tests: 97 core, 46 terminal, 23 app), strict Clippy, a debug .app build, and strict bundle signature verification. GUI checks below remain unverified:

- [ ] Cmd+T, Ctrl+Tab, and Cmd+1 through Cmd+9 work from both the cards and a terminal and focus the destination. Cmd+Enter does nothing. On first shell open, immediate typing arrives in the shell, not the agent. Ctrl+Q during startup stays on the cards after startup completes.
- [ ] Cmd+] / Cmd+[ skip headers, wrap across projects, scroll the selected card into view, and preserve terminal versus card focus. Each task keeps its agent/shell choice and draft.
- [ ] In a sidebar taller than the viewport, j/k and Command navigation reveal the selected header/card with minimal scrolling. Manual scrolling is not reset by terminal output or timer redraws.
- [ ] Cmd+N opens New for the current project from either surface. Escape returns to that exact terminal; launching focuses the new agent. Plain `n` on the cards does nothing.
- [ ] Cmd+Shift+W closes a clean idle task with no dialog, and still asks when the tree is dirty, commits are unpushed, or the agent is working. It does nothing while a dialog is open and does not reach the CLI. Cmd+W still closes only a shell. Plain `c` does nothing.
- [ ] Cmd+, then Escape returns to the terminal that opened Settings. Base branch apply/cancel restores previous focus. Dirty Close cancellation still routes to the shell.
- [ ] Command flow shortcuts do nothing while an overlay is open, including text editing, and never send input to the PTY. Escape and ordinary CLI bindings still reach the CLI.
- [ ] The Agent menu lists shortcuts. New and agent/shell tooltips show their Command keys. Check light and dark appearance.

## Optional worktree preparation

Checked the built debug `.app` opened with `open -n`, isolated `--data-dir`, a disposable repository, and native macOS keyboard input. The foreground process was checked before each input. The real Claude CLI was started only after successful setup, at its workspace-trust prompt; no model prompt was sent and no actual dependency installation was performed. Normal app data was not used. Light/dark appearance was restored after screenshots.

Evidence: `/private/tmp/shika-preparation-ui-iy5Ih7/` contains the reusable check script, JSON report, logs, screenshots, and captured approval/journal JSON. Disposable repositories and test data were removed after verification. This evidence is machine-local, not committed, and may disappear. Contributors can reproduce the flow with the versioned fixture recipe and test map in [worktree preparation](docs/worktree-preparation.md#reproducible-native-smoke-fixture).

- PASS: New shows the exact configuration for approval before a worktree is allocated. Escape cancels. Approval persists locally.
- PASS: Two cards run setup concurrently; a third shows Waiting for setup slot. Keyboard cancellation removes its untouched queued worktree without disturbing the other jobs.
- PASS: Nonzero setup stops before the agent starts. Both failed cards remain readable, with output and Retry setup. Their untouched worktrees are removed.
- PASS: `r` retries into a fresh worktree without asking again for unchanged configuration.
- PASS: Changing configuration asks again, before allocation; cancel keeps the existing task intact.
- PASS: Selected ignored local files are copied and the setup marker exists before the real CLI starts. Ctrl+Q and ordinary idle-card close remove that prepared worktree.
- PASS: Quit during setup stops the setup process group and keeps the pending worktree journaled.
- PASS: Approval and failure controls are readable in light and dark appearance. The terminal keeps its dark background.
- AUTOMATED: Consent changes/persistence, invalid/oversized/nonregular configuration, independent copy permissions, missing/unsafe paths, source/destination symlinks, source-only ignore rules, command order, preserved edits/files/commits, timeout, pre-start cancellation, descendant termination, setup-slot limits, shutdown/project removal, abandoned launch preservation, unrelated-session responsiveness, and setup-input/startup-query isolation.
- [ ] Mouse Cancel setup from a focused setup terminal restores card navigation. Mouse Retry setup behaves like `r`.
- [ ] A setup completes while another live terminal or dialog has focus, without stealing it; test the hidden fourth card and selected-row scrolling as well.
- [ ] Long configuration and copy paths remain usable at the minimum window size, including scrolling the approval dialog.
- [ ] Exercise a real project's dependency installation and all four CLIs, including existing shells, prompts, branch naming, notifications, dirty close, and pushed close after preparation. These are not claimed as new GUI acceptance passes.

Validation passed: cargo fmt --all --check, cargo test --workspace (191 tests: 119 core, 46 terminal, 26 app), strict workspace Clippy, debug `.app` build, and strict bundle signature verification. The broader existing GUI/IME checks below and above remain as recorded.

### Documentation follow-up

The contributor guide's local links/anchors, JSON examples, shell syntax, and regression-test symbols were checked. Its fixture setup and command were exercised in a disposable Git worktree, then removed; no app or model was launched for this documentation check.

- PASS: All 22 preparation tests with `--test-threads=1`, plus both app host tests for setup-input suppression and prepared-agent startup replies.
- KNOWN TEST-HARNESS ISSUE: The initial parallel preparation rerun passed 21 tests and failed one in `Fixture::new` with `AlreadyExists` while creating `repo with spaces`, before setup. Its PID-plus-timestamp directory naming permits a parallel collision. The guide records a serial diagnostic workaround and the need for atomic unique-directory allocation; this docs-only pass does not fix it. A serial pass does not establish that parallel validation is reliable.
- No additional native GUI acceptance results are claimed here. The original implementation validation above is historical evidence.

## Automated checks

Workspace tests cover PTY streaming, hidden terminal drains, terminal input, git exclusion, branch renaming, CLI title reading (sample, missing, and broken files), branch slugs without the project name, prefix cleanup, renames that skip taken and pushed names, the title watch timing, Option+Backspace in prompt capture, journal updates, base branch start points (origin, local, missing, unset with origin/HEAD, a changed base, a vanished base, fetch freshness, a dead remote, and waiting for the picker fetch), unpushed commits, failed pushes, branch retention, safe close, branch-switch refusal, and leftovers. All use temporary app data and repositories.

Final checks passed: cargo fmt --all --check, cargo test --workspace (158 tests: 92 core, 46 terminal, 20 app; base branch tests added 2026-10-05), cargo clippy --workspace --all-targets -- -D warnings, and strict bundle signature verification.

IME candidate placement, sustained typing feel, and the broader terminal matrix remain human checks in crates/shika-terminal/MANUAL_CHECKS.md. They are not claimed as acceptance passes here.

## External branch renames

- [ ] In an isolated app session, rename the task branch in its shell. Within a few seconds the card shows the new branch, while the title and worktree folder stay unchanged.
- [ ] Commit, rename, push, create/merge a PR, then press Cmd+Shift+W. The clean pushed task closes and its local branch remains.
- [ ] Rename with dirty files or unpushed commits. Close still asks; dirty work cannot be pushed, cancel keeps work, and explicit discard removes the renamed task branch.
- [ ] Switch to an unrelated branch with unpublished work, or detach HEAD. Close refuses safe recovery and preserves the worktree. A safe attached switch uses the separate confirmation below.

Automated core tests exercise these Git lifecycles in disposable repositories. GUI checks above remain pending.

## Branch-switch close

Implementation validation after merging the latest main (PR publishing and themes): parallel `cargo test --workspace` passed 309 tests (165 core, 78 app, 66 terminal); formatting and strict workspace/all-targets Clippy passed. The debug bundle built and passed strict signature verification. The upstream `block` future-compatibility warning remains. Core recovery tests use disposable repositories and local bare remotes. Native GUI acceptance below is pending, not established by unit tests.

- [ ] Create a task, `git switch -c better-name`, commit and push with a local bare remote. Cmd+Shift+W shows both branch names, preservation facts, Cancel, and Close task. Enter removes the card/worktree/journal and keeps both local branches and remote refs.
- [ ] Repeat after merging into the base and with a push without `-u`. No need to return to the old branch.
- [ ] Put unpublished commits only on the original branch; switch to a clean published branch. Close refuses and names the original branch. Repeat with unpublished work only on the current branch.
- [ ] Dirty tracked and non-ignored untracked work blocks recovery. Detached HEAD and a deleted recorded ref also block it. Card/worktree/terminals remain.
- [ ] Switch while the agent is active. The safe confirmation warns about stopping its turn. Confirm stops the agent and every shell; Cancel stops none.
- [ ] Escape/click Cancel restores the opening card or terminal focus, including a selected shell. `d` and `p` do nothing in the recovery dialog; plain `c` does nothing on cards.
- [ ] Change a branch tip or HEAD, or create dirty work through an external process while the dialog is open. Confirm refuses stale/unsafe cleanup; cancel and retry obtains fresh verification.
- [ ] Existing genuine rename adoption, dirty/unpushed normal close choices, and empty draft branch deletion still work.
- [ ] Check light/dark, translucency, long branch/task names, narrow windows, and multiple live tasks. No new colors or styling values.

Contributor overview, safety limits, implementation map, and debugging: [docs/branch-switch-close.md](docs/branch-switch-close.md).

## Open-source presentation check, 2026-10-05

- PASS: `cargo fmt --all --check`, parallel `cargo test --workspace --locked --offline` (205 tests), and strict workspace/all-targets Clippy. The upstream `block` future-compatibility warning remains.
- PASS: Debug app bundle builds, includes Shika's MIT license plus Heroicons, Octicons, and JetBrains Mono notices, and passes `codesign --verify --deep --strict`.
- Native README/poster/share screenshot captured using isolated data and disposable `storefront`/`routekit` repositories. CLI output is simulated, explicitly captioned, and image metadata is stripped. This does not validate real provider flows or close outstanding GUI checks above.
- Browser prototype checked with independent shell buffers (`git diff` and `pwd`), switching back to retained output, and the dirty-close dialog hiding Push. Component checks also cover tab ownership, pinned agent close, focus restoration, Codex/Pi picker choices, three-card cap, and simulated commit/push close behavior. Browser-reserved shortcuts can require visible controls.
