# Plan: smoother worktree start

Seven changes, in the order to build them: the bug first, then the small ones, then the larger features. Each item is one PR-sized step. Tick the boxes as you go.

Background: a comparison with T3 Code (`pingdotgg/t3code`) and Synara (`Emanuele-web04/synara`, a T3 Code fork) on 2026-11-19.

Before any item, read what `AGENTS.md` names for that feature. When a change alters a product rule, update `AGENTS.md` and its guide in the same change. GUI acceptance goes in `MANUAL_CHECKS.md`; passing unit tests are not enough.

## Order

| # | Item | Size | Main files |
| --- | --- | --- | --- |
| 1 | Close deletes untracked files (done) | Small | `worktree/close.rs`, `worktree/lifecycle.rs` |
| 2 | Refresh the index after creating a worktree (done) | Small | `worktree/lifecycle.rs` |
| 3 | Fix the Claude trust note (done) | Docs only | `docs/tasks-and-worktrees.md`, `AGENTS.md` |
| 4 | Skip the first-New dialog when nothing is suggested (done) | Small | `launch/mod.rs` |
| 5 | More setup suggestions | Medium | `preparation.rs` |
| 6 | Opt-in `node_modules` cloning | Large | `preparation.rs`, new clone module, setup editor |
| 7 | Prompt field in the New picker | Large | `launch/picker.rs`, launch path |

---

## 1. Close deletes untracked files

**Problem.** With `status.showUntrackedFiles=no` in the user's Git config, `is_dirty` (`crates/shika-core/src/worktree/close.rs:11`) runs `git status --porcelain`, sees nothing, and calls the tree clean. `git worktree remove` without `--force` follows the same setting, so it removes the tree and the untracked files with it. Reproduced with git 2.50.1.

**Steps**
- [x] `is_dirty`: pass `--untracked-files=normal` to `git status`.
- [x] `remove_worktree` (`crates/shika-core/src/worktree/lifecycle.rs:49`): add `-c status.showUntrackedFiles=normal` before `worktree remove`, so Git's own refusal also sees untracked files.
- [x] Check `publish.rs:328` (`status --porcelain=v2`): does the Create PR preview miss untracked files under this setting? If so, pass `--untracked-files=all` there too. No: that status call only reads the submodule `S..U` flag, which Git reports whatever the setting, and the preview stages with `git add -A`, which ignores it. Left unchanged.
- [x] Look for any other `git status` call that decides whether a tree is clean (Lead refresh/forget, branch-switch Close) and fix it the same way. All of them (Lead refresh/forget, branch-switch Close, push-and-close, leftovers) go through `is_dirty` or `remove_worktree`, so the two fixes cover them.
- [x] Diff stat and Changes need no change: they use `git ls-files --others`, which ignores this setting.

**Tests**
- [x] A fixture repo with `status.showUntrackedFiles=no` and an untracked file: `is_dirty` is true, and Close asks instead of removing.
- [x] `remove_worktree` without force refuses that tree.

**Done when** no Close path can remove an untracked file without asking, whatever the user's Git config says.

---

## 2. Refresh the index after creating a worktree

**Problem.** Right after checkout, most files carry the same timestamp second as the index, so Git treats them as "racily clean" and re-reads them on every `git status` and `git diff`. On a large repo the first diff stat and Changes read are slow until something rewrites the index. T3 Code refreshes once after creating a worktree.

**Steps**
- [x] In `create_draft`, after `worktree add` succeeds, run `git -C <worktree> update-index -q --refresh`. Done as `worktree::refresh_index`, called by `Core` right after the operations lock is released rather than inside `create_draft`, which runs under that lock: the wait below would hold up every other operation. It adds `--force-write-index`, as T3 does, for older git that skips the write when racy entries check out clean.
- [x] If the index's modification time is still in the checkout's second, wait out that second and refresh once more. T3 does this; see `refreshNewWorktreeIndex` in their `GitVcsDriverCore.ts`. Only for an index of 10,000 entries or more: on a small repo the first refresh nearly always lands in the checkout's second, so the wait would add up to a second to every New to save a few milliseconds of background reads.
- [x] Best effort: a failed refresh never fails task creation.
- [x] Do it before setup runs, so it does not add to the time before the CLI starts more than it must. Measure it. On a synthetic 50,000-file repo: unrefreshed diff reads 0.13 to 0.34s each, refreshed 0.09s; the refresh with its wait about 0.85s, mostly the wait. Recorded in `MANUAL_CHECKS.md`.

**Tests**
- [x] A unit test that creation still succeeds when the refresh fails.
- [ ] Manual: time the first diff stat on a large repo before and after. Measured with git on a synthetic repo; the in-app check on a real large repo is open in `MANUAL_CHECKS.md`.

---

## 3. Fix the Claude trust note

**Finding.** Claude Code stores folder trust under the main repo's path, even inside a linked worktree: it resolves the worktree's `.git` file back to the main checkout. Tested 2026-11-19 with Claude Code 2.1.311: a detached worktree of a repo already trusted in Claude Code, placed outside the repo, went straight to the prompt with no trust dialog. The dialog appears only for a repo never trusted in Claude Code, probably once per repo (not verified).

**Steps**
- [x] `docs/tasks-and-worktrees.md`, "First-run folder trust": update the Claude Code row and add the trusted-repo result. Keep the 2026-10-09 brand-new-repo result.
- [x] `AGENTS.md`: replace "still appears once per new worktree" with the new finding.
- [ ] Optional: confirm the once-per-repo part with a throwaway repo, without touching normal Claude config. Not done: trusting a throwaway repo writes to the normal Claude config, and an isolated `CLAUDE_CONFIG_DIR` has no login, so the docs keep "probably once per repository (not verified)".

---

## 4. Skip the first-New dialog when nothing is suggested

**Problem.** The "Set up worktrees for {project}?" dialog appears even when detection found nothing to copy or install (`crates/shika/src/launch/mod.rs:119-126` never checks whether the draft is empty).

**Steps**
- [x] Show onboarding only when the draft has at least one copy-file or setup command suggestion. `PreparationDraft::suggests_setup`, applied by `first_new_review` in `launch/mod.rs`. An installer note with no command (conflicting lockfiles) does not count; Settings still shows it.
- [x] When the draft is empty, start the task as if there were no config. Do not write `worktreeSetupReviewed`, so a later `.env` or lockfile can still trigger the dialog.
- [x] Detection then runs on each New for such a project. It is only a few file checks, but confirm it stays off the UI thread. Confirmed: draft loading runs inside `request_launch`'s `background_executor().spawn`, with the rest of launch preflight.

**Docs:** `docs/worktree-preparation.md` (First New, state table) and the preparation paragraph in `AGENTS.md`.

**Tests:** an empty draft launches with no dialog and no write. A draft with suggestions still shows the dialog.
- [x] Core: `preparation_onboarding_offers_review_only_when_something_is_suggested` (empty draft, launch, no write, still pending; a later `.env.local` suggests; a saved config never does).
- [x] App: `first_new_reviews_setup_only_when_something_is_suggested` (routing for empty, file-only, command-only, and saved drafts).
- [ ] Manual: the native flow, listed in `MANUAL_CHECKS.md` under first-New onboarding.

---

## 5. More setup suggestions

**Today** only npm and pnpm get an install suggestion, plus root `.env` / `.env.local` (`crates/shika-core/src/preparation.rs`, around lines 220 and 290). Yarn and bun lockfiles are detected but get "no suggested installer".

**Steps**
- [ ] **yarn:** Yarn 2 or later (`.yarnrc.yml`, or `packageManager` `yarn@2+`) suggests `yarn install --immutable`. Yarn 1 suggests `yarn install --frozen-lockfile`.
- [ ] **bun:** `bun install --frozen-lockfile`.
- [ ] Accept `packageManager` values `yarn@…` and `bun@…`, which currently return "no suggested installer".
- [ ] **CocoaPods:** `Podfile` at the root suggests `pod install`; `ios/Podfile` suggests `cd ios && pod install`. If a root `Gemfile` names `cocoapods`, use `bundle exec pod install`. Order it after the JS install, because React Native pods read `node_modules`.
- [ ] **`.worktreeinclude`:** read a root `.worktreeinclude` and suggest its entries as copy-files. Decide first:
  - v1 accepts literal file paths only, each through the existing copy-files validation (ignored, regular file, no symlink). Globs are reported as unsupported. This keeps the "no recursive scans" rule.
  - Globs would need `git ls-files --others --ignored --exclude-from=.worktreeinclude`, which walks the tree. Leave that for later, with a file-count cap.
- [ ] Keep every read bounded and root-only, as today. A monorepo app in `apps/mobile` is not detected; that stays manual.

**Docs:** `docs/worktree-preparation.md` (Suggested defaults). In `AGENTS.md`, replace "fixed npm/pnpm installers".

**Tests:** one fixture per case (Yarn 1, Yarn 2+, bun, conflicting lockfiles, root and `ios/` Podfile, Gemfile with cocoapods, `.worktreeinclude` with a literal path, a glob, and a tracked file).

---

## 6. Opt-in `node_modules` cloning

**Idea, from Synara** (`apps/server/src/git/worktreeDependencyClone.ts`). On APFS, clone the main checkout's `node_modules` into the new worktree with copy-on-write. Nothing is copied until a file changes, so it takes almost no time or disk. T3 Code does not do this.

**Config.** A new key in `.shika/worktrees.json`, for example `"clone-dependencies": true`, off by default. Consent compares the parsed config, so the key takes part in approval automatically. A missing key must parse as `false`, so existing approvals stay valid.

**When it clones** (all must hold, else silently skip and let the install run as usual):
- [ ] The main checkout has `node_modules` at the same place.
- [ ] Every tracked `package.json` and lockfile is byte-identical between the main checkout and the new worktree (list them with `git ls-files` in the worktree).
- [ ] The target does not exist and is Git-ignored in the worktree.
- [ ] Same volume (a clone across volumes fails with `EXDEV`; fall back).
- [ ] No absolute symlink inside points into the main checkout. pnpm's links are relative, but check, or an agent could write into the main checkout through a link.

**Steps**
- [ ] Clone with `libc::clonefile` (`libc` is already a dependency). First check that `clonefile` clones a whole directory tree on current macOS; Synara uses a JXA call instead, so confirm before relying on the syscall.
- [ ] Clone the root `node_modules` and workspace package `node_modules` (paths from tracked `package.json` files), with a cap like Synara's 256.
- [ ] Drop tool caches that may hold absolute paths: `node_modules/.cache`, `node_modules/.vite`.
- [ ] Run it after copy-files and before setup commands. Setup still runs the install, which is near-instant when everything matches.
- [ ] `npm ci` deletes `node_modules` first. When cloning is on, suggest `npm install` instead, or warn in the editor.
- [ ] Cancellation and failure follow the existing preparation rules: a clone failure falls back, it never fails the task.
- [ ] Setup editor: a toggle for the new key, per `design/DESIGN.md`. The approval dialog lists it.
- [ ] Onboarding may suggest it when a lockfile and `node_modules` are both present.

**Docs:** `docs/worktree-preparation.md` (new section, config/consent table) and `AGENTS.md`.

**Tests:** each skip condition, the symlink check, cache removal, fallback on failure, and consent equality with and without the key. Manual: time a React Native repo before and after.

---

## 7. Prompt field in the New picker

**Problem.** Today you can type only after copy and setup finish (typing during setup is dropped). In T3 Code you write the prompt first and walk away.

**Plan.** The picker gets an optional prompt field. A non-empty prompt goes to the CLI as one positional argument after its launch flags, the same path Switch agent and Lead workers already use, so it counts as the first submission (the card shows Working) and names the branch right away. Empty keeps today's behavior.

**Decide first**
- [ ] **Keys.** The picker uses plain `j`/`k` and number keys, which a text field would take. Proposal: the field has focus when the picker opens, Up/Down choose the agent, Enter launches, Escape cancels, and number keys stop selecting while the field has text. Check against `docs/keyboard-flow.md`.
- [ ] **Terminal choice:** hide or disable the field, since a shell takes no prompt.
- [ ] **Checkout mode:** allow a prompt there too, or not.
- [ ] **Retry after a failed setup:** keep the prompt for Retry.
- [ ] **Multi-line:** one line, or Shift+Enter for new lines.

**Steps**
- [ ] Field and keys in `crates/shika/src/launch/picker.rs`, following `design/DESIGN.md`.
- [ ] Carry the prompt through `request_launch`, onboarding, approval, and setup to spawn.
- [ ] Reuse the launch-prompt submission logic from `docs/agent-activity.md` and `docs/agent-switch.md`.
- [ ] The rule "typing during setup is dropped" stays: the prompt is passed at spawn, not typed.

**Docs:** `docs/keyboard-flow.md`, `docs/agent-activity.md`, `docs/tasks-and-worktrees.md`, and `AGENTS.md` ("Using the app").

**Tests:** prompt passed as the last argument for each CLI, an empty prompt starts as today, branch naming from the prompt, and Terminal ignores the field. Manual: type a prompt, start a task with setup, and walk away.

---

## Not in this plan

- Setting up git submodules in new worktrees: confirm the gap with a submodule repo first.
- Moving worktrees out of the repo: trust no longer depends on location; decide separately.
- T3's copy-on-write checkout cache, and locking a worktree during creation.
