# Spec: an agent in the checkout

Status: agreed, not built, 2026-10-26. When it is decided, the rules move into `docs/tasks-and-worktrees.md` and this file is deleted.

## Problem

Shika's answer to every task is a fresh worktree. That is right for real work running in parallel. It is wrong for small things that change nothing:

- "What version of Node does this repo use?"
- "Do we have docs on the release process?"
- "Where is the notification code?"

For these the author does not want a new branch, a worktree, a setup run, or a card to clean up afterwards. Today they leave Shika for another terminal app, which is exactly the juggling Shika exists to remove.

## Positioning

Shika stays a worktree-first, multi-agent app. That is what the site, the README, and the default New flow say and do. This feature is a quiet utility:

- **Not advertised.** No hero copy, no feature cell, no README bullet.
- **Documented.** It has a line in `docs/using-shika.md` and its shortcut in the shortcut table. Unadvertised is fine; undocumented is a hidden feature, and hidden features become bug reports.
- **Never contradicted.** Public copy that would become false is reworded (see [Public copy](#public-copy)).
- **One step more deliberate than New.** Cmd+N always creates a worktree. The checkout session has its own chord with Shift.

## Name

**Checkout.** The repository folder the author added, on whatever branch it has checked out.

- Not "main": the author's checkout is often on `dev`, and "main checkout" on `dev` reads as a contradiction.
- Not "primary" (Orca's word): it makes the exception sound like the default.
- Menu item: **New Agent in Checkout**. Picker title: **In checkout**. Card marker tooltip: "Runs in your checkout, no worktree".

## The shape

A **checkout session**: an ordinary agent card whose CLI runs in the project's own folder instead of a new worktree. Everything that does not depend on the worktree works as usual (status, notifications, timer, shell tabs, Switch agent). Everything that owns git is turned off.

### Starting one

- **Cmd+Shift+N** (free today) or **Agent > New Agent in Checkout**. It opens the same CLI picker as Cmd+N, titled "In checkout" with the project and its current branch in the footer, for the same target project rules as Cmd+N (selected card's project, Tab to change). Enter starts it.
- Rebindable in Settings > Keyboard like the other app chords. Inert while a dialog or picker is open.
- Not on the column, not on a project header, no `+` variant.
- **One per project.** If the project already has one, the chord selects it and focuses its terminal instead of opening the picker. Two agents in one checkout is the collision worktrees exist to prevent.
- No base branch fetch, no preparation, no worktree setup onboarding, no journal entry.
- The CLI starts in the repository root with the usual environment (login-shell `PATH`, `PWD`, the `GIT_*` variables removed) and the preset's launch flags. Codex's trust override names the repository root; it is per process and writes no config.
- An empty prompt, like New: the card is Waiting until the author types.

### On the card

- First line: the display name, from the first prompt line, then the CLI's session title, exactly as task names work today. Manual rename works.
- Second line: the CLI mark, then the **checked-out branch** (for example `dev`) with a small checkout icon, read when the card turns Ready and when it is selected. A detached HEAD shows the short commit. This is how the author tells it apart from a worktree task at a glance.
- Grouped under its project like any card, ordered first in the group.
- The terminal header shows the repository path and the current branch, read-only.

### Git: hands off

- **No branch is created or renamed.** Automatic branch naming is off for this session. It never touches the author's branch.
- **No Create PR.** The button, Cmd+Shift+P, and the menu item are absent. The author's own git flow applies.
- **Diff stat and Changes panel** compare against `HEAD`: uncommitted work only. See [Diff baseline](#diff-baseline).
- Reads stay bounded to the repository root the way task reads are bounded to their worktree.

### Close

- Close stops every PTY of the session and removes the card. Nothing else: no `git status` gate, no discard, no push, no worktree removal, no branch deletion.
- It still asks when a turn is active or blocked ("Stop the agent?"), with the same exact-chord rules as every destructive dialog. Idle closes at once.
- **There is never a Discard here.** Discard in a worktree throws away a disposable copy; in the checkout it would throw away the author's own work.
- Quit stops it like any session. Nothing persists and nothing becomes a leftover.

### The Lead

- A Lead cannot start, list, read, send to, or close a checkout session. Lead workers always get worktrees.
- The doorbell ignores it.

### Interaction with worktree tasks

None. Worktree tasks never read the checkout's working tree. Git already refuses to check out, in the checkout, a branch a worktree holds, so the author cannot cross the two by accident.

## Diff baseline

The diff stat on a card (`2 files +64 −3`) and the Changes panel answer "what is different here?". A worktree task compares against the branch it started from, so it counts the agent's commits too. In the checkout there is no starting branch, so it needs its own answer:

| Baseline | Shows | Noise |
| --- | --- | --- |
| `HEAD` | Uncommitted edits | Edits that were already there before the agent started |
| Upstream (`origin/dev`) | Uncommitted edits plus commits not yet pushed | Old unpushed commits from before the session |
| Snapshot at start | Only what changed since this session began | Edits the author makes in their editor meanwhile; needs a stored tree snapshot |

**Decision: `HEAD`.** The session is for questions, so its diff stat is a warning light: it appears only if something in the working tree changed. After the author commits, it goes away, which matches "I handle git myself". Upstream would light up for every unpushed commit on `dev`, agent or not. A snapshot is the most precise but adds state for little gain.

## Reference: how Orca does it

Orca (MIT, `stablyai/orca`, read 2026-10-26) is an Electron multi-agent IDE. Its unit is the *workspace* (a folder with its own terminals, editor, and browser tabs), and agents are optional inside it ("Agent [Optional]", including a "Blank Terminal").

- The original clone is just another sidebar row, badged **primary** ("Primary worktree (original clone directory)").
- **Adding a project opens that primary row by default**, even when the author's filter hides it. So the author's guess is right: in Orca, the checkout is the starting point and worktrees are what you add.
- The primary row cannot be deleted; its context menu offers Remove project instead.
- A sidebar filter, "Default branch workspaces", hides the checkout rows for people who never use them.

What fits Shika: the checkout gets a visible marker, and it can never be deleted or discarded. What does not: Orca's unit is a folder, so the checkout comes free and is always present; Shika's unit is an agent task, so a checkout row that is always there would be an empty card and would make the checkout the default path, the opposite of the positioning above. Orca's create dialog is also mouse-heavy (two comboboxes, tabs, an Advanced drawer), which Shika avoids by reusing the keyboard picker.

## Alternatives considered

- **A plain shell card** (no agent; the author types `claude` themselves). Literally "a normal terminal", but Shika would not know which CLI runs, so status, notifications, and the timer would not work, and it drifts toward a terminal multiplexer, which the product principles rule out.
- **An always-present checkout row per project**, as in Orca. Makes the checkout the default and adds a card with nothing running in it.
- **Ask the Lead.** It already lives in the repository, but in a detached worktree at the base branch, not the author's checkout, and its instructions make it a manager of workers, not a quick-answer helper.
- **A worktree that skips setup.** Still creates a branch and a folder to clean up, and still answers about the base branch, not what the author has checked out.

## Principle change

`CONTRIBUTING.md` says "One fresh worktree per task." It becomes: "One fresh worktree per task. The one exception is a single checkout session per project, which owns no git state." `AGENTS.md` gets the dated decision. Without this, agents working on Shika will keep removing the feature as a violation.

## Public copy

- `site/index.html`, the worktree cell: "Agents never touch your main checkout or each other." becomes false. Reword to something like "Each task runs in its own worktree, away from your checkout and from every other task." True, and still says nothing about the exception.
- `README.md`, the site hero and lede: already true as written ("A worktree per task"). No change.
- `docs/using-shika.md`: one short section and the shortcut row.

## Code touch points (estimate)

- `shika-core`: a session kind (`Task`, `Checkout`, plus the Lead's existing kind) instead of an `Option` worktree. `create_session` gains a checkout path that skips `create_draft` and the journal. `session_close` for a checkout only stops PTYs. `session_diff_stat` and the Changes read take the `HEAD` comparison. Branch naming and title-to-branch rename skip the kind. Control (`shika` CLI) refuses it.
- `shika`: the `NewInCheckout` action, chord, and menu item; the picker title and footer; the one-per-project check; the card's branch line; the header without Create PR; the Close dialog variant.
- Tests: Close never runs a git write for a checkout session; a dirty checkout closes without Discard; branch naming never renames the checked-out branch; the Lead cannot target it; a second start focuses the first.

## Decisions

Agreed with the author on 2026-10-26:

- Name: Checkout. Menu item: New Agent in Checkout.
- Chord: Cmd+Shift+N, also in the Agent menu.
- The card shows the checked-out branch on its second line and sits first in its project group.
- Diff stat and Changes compare against `HEAD`.
- Not advertised; documented in the user guide; the site's worktree sentence is reworded so it stays true.

## Tasks

Three items, one coding session each. Check an item off only when everything under "Done when" holds.

- [x] **1. Core: checkout sessions in `shika-core`**

  A session kind for the checkout instead of a fake worktree, and every core path that assumes one task means one worktree taught about it.

  - `create_session` gains a checkout path: no `create_draft`, no base fetch, no preparation, no journal entry; the CLI starts in the repository root with the usual environment and launch flags (Codex trust override on the root).
  - One checkout session per project, enforced in core so the UI cannot race past it.
  - `session_close` for a checkout only stops its PTYs. Discard, push, branch-switch recovery, and leftovers refuse it.
  - Branch naming and title-to-branch rename skip it; display naming and manual rename still work.
  - `session_diff_stat` and the Changes read compare against `HEAD`, bounded to the repository root.
  - A cheap read of the checked-out branch (or short commit when detached) for the card.
  - The control socket refuses it: a Lead cannot start, list, read, send to, or close it, and the doorbell ignores it.

  Done when: `cargo test --workspace` passes with new tests for each point above, including a dirty checkout that closes with no git write, a checked-out branch that is never renamed, a second start that is refused, and a Lead command that is refused.

- [ ] **2. App: starting, showing, and closing it in `shika`**

  The author-facing feature, keyboard first, following `design/DESIGN.md`.

  - `NewInCheckout` action on Cmd+Shift+N, rebindable in Settings > Keyboard, inert under a dialog or picker, listed in the Agent menu as New Agent in Checkout.
  - The CLI picker reused with the title "In checkout" and the project and current branch in the footer; Tab changes the project; Escape restores focus. With a session already running, the chord selects it and focuses its terminal instead.
  - The card sits first in its project group; its second line shows the CLI mark, a checkout icon, and the branch, with the tooltip "Runs in your checkout, no worktree".
  - The terminal header shows the path and branch with no Create PR button; Cmd+Shift+P and the menu item do nothing for it.
  - Close: idle closes at once; Working or Asking asks "Stop the agent?" with an exact Command chord and the 300ms key guard. No Discard, ever.
  - Shell tabs, Switch agent, rename, notifications, diff stat, and Changes work as on any card.

  Done when: a built `.app` opened with `open` against a disposable repo and `--data-dir` passes the flow by keyboard alone: start, ask a question, see Ready and the notification, open a shell tab, change a file and see the diff stat, close while Working and while idle. Results recorded in `MANUAL_CHECKS.md`.

- [ ] **3. Docs, public copy, and cleanup**

  Make the exception a recorded decision so nobody removes it, and keep every public sentence true.

  - `CONTRIBUTING.md`: the "One fresh worktree per task" principle gains the checkout exception.
  - `AGENTS.md`: a short dated paragraph and the new chord in the keyboard map.
  - `docs/tasks-and-worktrees.md`: a Checkout section with the lifecycle, Close, and diff rules from this spec, plus the code map rows.
  - `docs/using-shika.md`: a short section and the shortcut table row. `docs/keyboard-flow.md` if focus rules changed.
  - `site/index.html`: reword "Agents never touch your main checkout or each other." README and hero unchanged.
  - Delete this spec once its rules live in the guides.

  Done when: every doc above matches the shipped behavior, no em dashes, and `docs/specs/main-checkout.md` is gone.
