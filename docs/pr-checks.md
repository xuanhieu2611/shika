# PR checks on the card

Contributor and coding-agent guide for the **PR checks mark**: what was added, why it exists, how it works, and how to debug or extend it without making Shika heavier.

Start with [AGENTS.md](../AGENTS.md) for architecture and build constraints, and [CONTRIBUTING.md](../CONTRIBUTING.md) for the product principles and non-goals. This feature builds on [Confirmed PR publishing](publishing.md), so read that guide first. Read [design/DESIGN.md](../design/DESIGN.md) before changing anything visible. [MANUAL_CHECKS.md](../MANUAL_CHECKS.md#pr-checks-mark-2026-10-08) separates automated evidence from GUI checks that still need a person.

## What changed

Shika used to stop watching a task at Create PR. The card stays after a push, because a push does not close it, and the agent is still in that worktree. But the next failure happens in CI on GitHub, where the column cannot see it, usually after the developer has moved on to another task.

The card now carries a small, read-only mark for the PR that Create PR created or reused:

```text
Create PR succeeds
    → card shows #42 with a ring (checks running)
    → GitHub is read in the background while checks run
    → #42 ✓   all checks finished without a failure; GitHub reads stop
      #42 ✗   a check failed; mark turns red, one notification is posted
      #42     the repository reports no checks
    → a push from the task's shell or agent starts a new round
    → PR merged or closed: the mark disappears
```

Added:

- `PublishedPr`: `Core::session_publish` now returns the PR URL, repository, number, and the commit it pushed, instead of a bare URL.
- `Core::session_pr_checks`: one bounded `gh pr view` that returns the PR head, whether it is open, and a classified checks state.
- `Core::session_pushed_head`: one local `git rev-parse` of `refs/remotes/origin/<branch>`.
- `crates/shika/src/checks.rs`: the schedule and state machine (`PrWatch`), without GPUI or subprocesses, unit tested with injected time.
- A `failed` status color pair in `Chrome`, used only by this mark.
- A body parameter on `Notifications::post`, so one notification can say "Checks failed" while agent notifications keep their existing body.

Unchanged:

- Publishing itself: confirmation, targeting, staging, commit, push, PR reuse, and partial-failure rules are exactly as in [publishing.md](publishing.md). Only the return type changed.
- Agent status. The mark is not an agent state: it does not change the signal at the right of the card's first line, the card tint, the elapsed timer, the card order, or Close.
- Persistence. Nothing is written to `projects.json`, `worktrees.json`, or `settings.json`.
- Shika still never merges, re-runs, cancels, or lists checks, and never manages reviews.

## Why these decisions

The guiding constraint is that Shika should feel like a terminal: fast, light, and reliable. A background feature must cost nothing when it has nothing to do, and must never block the UI or nag the user.

| Decision | Reason |
| --- | --- |
| A read-only mark, not a CI panel | A red mark is the same kind of signal as Ready and Asking: come back to this worktree, where the branch, the diff, and the agent still are. Lists, logs, and re-runs are management, which CONTRIBUTING keeps out of scope. GitHub already shows details one click away. |
| Track only the PR Shika published | Discovering PRs for every card (for example, PRs an agent opened with `gh pr create`) would mean polling GitHub for every task, all the time. The PR from Create PR is already known, with an explicit repository. |
| Reuse the publishing `gh` runner | Same login-shell PATH resolution, non-interactive environment, `GH_REPO` removal, explicit `--repo`, process group, and timeout. No new dependency, token storage, or HTTP client. |
| `gh pr view --json headRefOid,state,statusCheckRollup` rather than `gh pr checks` | One call returns the head commit, open state, and checks, and exits 0. `gh pr checks` exits nonzero for pending (8) or failing checks, and errors when no checks exist, which would make normal states look like failures. |
| Read GitHub only while checks run | A watch with passed checks makes no network calls at all. A run that is still pending after six hours stops being read too. |
| Slow down while pending | Most CI finishes within minutes, so early reads are frequent and later ones rare. This bounds network use for slow or stuck pipelines. |
| Keep reading a red mark, slowly | Developers often re-run a flaky job on GitHub without pushing. Without a slow read, the card would stay red after the re-run passed. |
| Detect pushes with a local ref | Git updates `refs/remotes/origin/<branch>` on every push from the worktree, so reading it costs no network and catches the main case: the agent or the developer pushing a fix. |
| Fail fast | The first failed check is enough reason to come back; waiting for slower checks only delays the signal. |
| Notify each time the mark turns red, not on pass | A failure asks for action; a pass does not. A repeated failure after a re-run or a new push is new information. |
| Hide failure | gh missing, offline, signed out, or rate limited keeps the last mark and backs off. A background read never raises a toast. |
| Memory only | Live sessions do not survive a relaunch, so neither does their watch. No reconciliation logic is needed. |
| Red colors only the mark | "Status is the only color" in DESIGN.md. Green already means Ready, so a passing mark stays ink-3. Red is new and reserved for failed checks. The card gets no tint and does not move, so a background event never rearranges the column. |

## User-facing contract

After a successful Create PR, the card's second line ends with the PR mark, after the CLI, the branch, and (while Ready) the diff stat:

| Mark | Meaning | Color |
| --- | --- | --- |
| `#42` and a 7px ring | Checks are running, or have not registered yet | ink-3, ring ink-4 |
| `#42 ✓` | Every check finished without a failure | ink-3 |
| `#42 ✗` | At least one check failed | failed text color, medium weight |
| `#42` | GitHub reports no checks for the PR head | ink-3 |

- The mark shows regardless of the agent's status, unlike the diff stat.
- A click opens the PR in the browser and does not select the card or focus the terminal. The tooltip reads "Checks running", "Checks passed", "Checks failed", or "Open PR".
- When the mark turns red, Shika posts one native notification titled `{project} - {task}` with the body "Checks failed". The system alert sound follows the existing Settings choice. A click on it selects the card and focuses its terminal, like other notifications.
- Merging or closing the PR removes the mark. Closing the card or quitting Shika ends the watch.
- Reopening Create PR on the same card replaces the watch with the PR returned by the new publish.

## How it works

### Lifecycle

```text
Shika::publish_pr
  → background Core::session_publish → PublishedPr { url, repository, number, head }
  → Card::pr = PrWatch::new(published, now)        (None if the URL has no /pull/N)

Shika::tick (every 100ms)
  → Shika::watch_checks
      for each card with a session and a watch:
        PrWatch::due(now) → None | Due::Poll | Due::Ref   (marks the watch in flight)
        background: Core::session_pr_checks  or  Core::session_pushed_head
  → Shika::checks_read
      PrWatch::polled(result) → Polled { changed, failed, closed }
      PrWatch::ref_read(head) → changed
      closed: Card::pr = None
      failed: Notifications::post(..., "Checks failed", ...)
      changed: cx.notify()

Shika::card_view → pr_mark(card, watch)
```

`due` is a few `Instant` comparisons, so the 100ms tick adds no measurable cost. Each watch has at most one read in flight. All subprocesses run on GPUI's background executor; the UI thread only applies results.

### The schedule

| Constant (`checks.rs`) | Value | Purpose |
| --- | --- | --- |
| `FIRST_POLL` | 10 s | Checks need a moment to register after a push. |
| `interval` | 30 s for 10 min, then 60 s until 1 h, then 300 s | Pending reads, measured from when watching the current head began. |
| `MAX_INTERVAL` | 300 s | Interval while red, and the cap for error backoff. |
| Error backoff | interval × 2^failures (failures capped at 4), at most 300 s | Any failed read keeps the mark and retries later. |
| `REF_EVERY` | 10 s | Local `origin/<branch>` read, for every watch, settled or not. |
| `NO_CHECKS_GRACE` | 180 s | An empty rollup counts as pending this soon after a push. |
| `EXPECT_GRACE` | 120 s | How long a different PR head from GitHub is treated as stale. |
| `GIVE_UP` | 6 h | No more GitHub reads for a head that is still pending or red. |

After a state is classified for the current head:

| State | Next GitHub read |
| --- | --- |
| Pending | `interval(elapsed)`, until `GIVE_UP` |
| Failed | `MAX_INTERVAL`, until `GIVE_UP` |
| Passed or no checks | None |

### Head matching

A check result only means something for one commit. The watch keeps:

- `head`: the PR head GitHub last reported, which the shown state belongs to.
- `expect`: a commit Shika knows was pushed but GitHub has not reported yet, with the time it was seen.

`PrWatch::new` sets `expect` to the commit the publish pushed. `ref_read` sets it whenever the local `origin/<branch>` differs from both `head` and `expect`, and also resets the mark to pending and schedules a read after `FIRST_POLL`.

When a GitHub read returns a different head than `expect`:

- Within `EXPECT_GRACE`, the result is ignored and the mark stays pending. GitHub can briefly report the previous head after a push, and its finished checks would otherwise show green or red for the wrong commit. This matters most when Create PR reuses an existing PR whose previous head already has results.
- After the grace, GitHub's head is trusted. That covers a push from another machine.

Without `expect`, a new head from GitHub (for example, a push from elsewhere noticed during pending reads) resets the elapsed time, so the interval and grace start over for that commit.

### Classification

`publish::classify` reads `statusCheckRollup`, which mixes two GitHub types:

| Item | Pending | Failed | Finished without failure |
| --- | --- | --- | --- |
| CheckRun (`status`, `conclusion`) | `status` other than `COMPLETED`, or no conclusion | `FAILURE`, `TIMED_OUT`, `ACTION_REQUIRED`, `STARTUP_FAILURE` | `SUCCESS`, `NEUTRAL`, `SKIPPED`, `CANCELLED`, `STALE`, anything else |
| StatusContext (`state`) | `PENDING`, `EXPECTED` | `FAILURE`, `ERROR` | `SUCCESS`, anything else |

Any failed item makes the whole PR failed, even while others are pending. Otherwise any pending item makes it pending. An empty list is "no checks". Cancelled runs count as finished because they usually come from a superseded run and do not ask for a fix. All checks count, required or not.

### Command environment

`publish::checks` runs:

```sh
gh pr view NUMBER --repo HOST/OWNER/REPO --json headRefOid,state,statusCheckRollup
```

through `run_with_timeout` with a 20 second limit, null stdin, prompts disabled, `GH_REPO` and Git redirect variables removed, and the login-shell PATH. The repository is the one publishing already validated against `origin`, so a gh default or the current directory cannot redirect the read. `worktree::pushed_head` runs `git rev-parse --verify --quiet refs/remotes/origin/BRANCH^{commit}` with `GIT_OPTIONAL_LOCKS=0`. Neither takes the core operation lock, like `session_diff_stat`, so a slow network never holds up Close or publishing.

## Known limits

These are deliberate trade-offs, not bugs. Changing them needs a product decision first.

- **PRs opened outside Create PR are not tracked**, including PRs an agent creates with `gh pr create`. Running Create PR on the card adopts the existing open PR for that branch.
- **A re-run after a pass is not noticed.** Once checks pass, GitHub reads stop until the local ref moves. A job re-run on GitHub that then fails will not turn the card red.
- **A push from another machine while settled** is noticed only when the worktree's `origin/<branch>` moves, for example after a `git fetch` in the task shell.
- **Optional checks count.** A failing non-required check turns the mark red.
- **Rate limits.** Each read is one GraphQL request through gh. A handful of tracked PRs at a 30 second interval is far below GitHub's authenticated limits, but many cards with long pending runs add up; the slowing interval and the six-hour cap bound it.
- **Relaunch forgets the watch**, like every live session.
- **Fork-to-upstream PRs** are not published by Shika, so they are never watched.

## Code map

Use symbols rather than line numbers, which change frequently.

| File / symbols | Responsibility |
| --- | --- |
| `crates/shika-core/src/publish.rs`: `PublishedPr`, `pr_number` | Publish result; PR number parsed from `/pull/N` at the end of the URL, or none. |
| Same file: `checks`, `PrView`, `RollupItem`, `classify`, `PrChecks`, `ChecksState` | The one bounded gh read and its classification. |
| Same file: `gh`, `gh_cmd`, `run_with_timeout` | Shared subprocess environment with publishing. |
| `crates/shika-core/src/worktree.rs`: `pushed_head` | Local `origin/<branch>` commit. |
| `crates/shika-core/src/lib.rs`: `session_publish`, `session_pr_checks`, `session_pushed_head` | Public, blocking API. The last two are lock-free reads. |
| `crates/shika/src/checks.rs`: `PrWatch`, `Due`, `Read`, `Polled`, `Mark`, `interval` | Schedule and state machine. Pure logic, no GPUI, no processes. |
| `crates/shika/src/main.rs`: `Card::pr`, `publish_pr` | Starting a watch when publishing succeeds. |
| Same file: `tick`, `watch_checks`, `checks_read` | Running due reads in the background, applying results, notifying. |
| Same file: `pr_mark` | The mark on the card's second line, tooltip, and click. |
| `crates/shika/src/model.rs`: `CHECKS_FAILED`, `checks_tip` | Notification body and tooltip copy. |
| `crates/shika/src/notifications.rs`: `Notifications::post` | Native notification with a caller-supplied body. |
| `crates/shika/src/appearance.rs`: `Chrome::failed` | Hand-tuned Shika values; ANSI red for derived themes, with the 4.5:1 text rule. |

## Debugging playbook

Run these read-only commands from the **task worktree**, replacing the placeholders:

```sh
gh auth status
gh pr view NUMBER --repo HOST/OWNER/REPO --json headRefOid,state,statusCheckRollup
git rev-parse refs/remotes/origin/BRANCH
git rev-parse HEAD
```

Compare `headRefOid` with the local ref: if they differ for more than two minutes after a push, the watch follows GitHub's head.

Do not attach tokens, private repository names, check logs, or personal paths to public issues. Redact output and use placeholders like `OWNER/REPO`.

| Symptom | Inspect first |
| --- | --- |
| No mark after Create PR | The PR URL from `session_publish`: `pr_number` gives none unless it ends in `/pull/N`. Also check the card matched `preview.session_id` in `publish_pr`. |
| Ring never resolves | Run the `gh pr view` command above from the worktree. If it fails, the watch keeps backing off silently. If it succeeds, compare `headRefOid` with the pushed commit (`EXPECT_GRACE`) and look for checks stuck in `QUEUED` or `PENDING`. |
| Mark shows the previous commit's result | Whether the local ref moved after the push (`pushed_head`), and whether GitHub's head changed within `EXPECT_GRACE`. |
| `#N` alone, but the repository has CI | The rollup was empty for longer than `NO_CHECKS_GRACE`. Check whether workflows run for this branch or event at all. |
| Red does not clear after fixing | Whether the fix was pushed from this worktree (the local ref must move). A re-run on GitHub clears it on the next slow read, within five minutes. |
| Red but the GitHub page looks fine | Non-required checks count, and `ERROR` or `TIMED_OUT` count as failures. Look at every item in `statusCheckRollup`. |
| No notification on failure | The "Checks failed" notification posts only when the mark changes into red. macOS notification permission and the Settings sound choice apply as for other notifications. A later notification for the same card replaces it in Notification Center, because the identifier is the session id. |
| Too many gh processes | There should be at most one read in flight per watch. Check that `due` is the only place reads start, and that each read reports back through `polled` or `ref_read`, which clear `in_flight`. |
| Works from a terminal, not from the Dock | gh discovery uses `Core::path_env`, the login-shell PATH, as in publishing. Test with a built `.app` opened with `open`. |

## Validation and isolated reproduction

From the repository root:

```sh
source "$HOME/.cargo/env"
cargo test -p shika checks::tests
cargo test -p shika-core publish::tests
cargo fmt --all --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
./scripts/bundle-app.sh --debug
codesign --verify --deep --strict target/debug/Shika.app
```

Automated coverage:

- `checks::tests` drives `PrWatch` with injected `Instant`s: first read timing, one read in flight, no GitHub reads after a pass, slow reads while red, re-run clearing red, push restarting a round, notifying on each change into red, stale previous-head results, empty-rollup grace, interval and error backoff, the six-hour cap, and merged or closed PRs.
- `publish::tests` covers `pr_number`, `classify` for CheckRun and StatusContext items, `checks` against a fake gh that records its arguments, `PublishedPr` from both the create and reuse paths, and `pushed_head` after a real push to a local bare remote.

These tests use fake gh responses. They do not prove GitHub's real response shapes over time or the native rendering. For an integrated check, use a **purpose-created test repository** with a simple GitHub Actions workflow that can be made to pass or fail, authorized credentials, and an isolated data directory:

```sh
open -n target/debug/Shika.app --args --data-dir /absolute/path/to/disposable/data
```

Never test against an existing personal or production project, or against normal app data. Verify the isolated Shika window is frontmost before sending synthetic input. Record results in [MANUAL_CHECKS.md](../MANUAL_CHECKS.md#pr-checks-mark-2026-10-08), not as implied by unit tests.

## Contributor guardrails and extension points

- Keep it read-only. No re-run, cancel, approve, merge, or comment actions from Shika.
- Keep GitHub reads bounded: only for PRs Shika published, only while checks are pending or failed, with a slowing interval and a cap. A settled, passing watch must make no network calls.
- Keep every subprocess off the UI thread, bounded, non-interactive, and scoped with an explicit `--repo`. Do not add a GitHub HTTP client or store tokens; gh owns authentication.
- Keep failures silent and the last mark visible. Do not add toasts for background reads.
- Keep the mark separate from agent status: no tint, no reorder, no change to Close or the status signal, unless the author decides otherwise in DESIGN.md.
- Keep head matching. Never show a result for a commit other than the one the user last pushed, inside the grace window.
- Put timing and state changes in `checks.rs` with tests that inject time. Keep `main.rs` to scheduling and painting.
- Update this guide, the short AGENTS.md handoff, DESIGN.md for visible changes, and MANUAL_CHECKS.md with behavioral changes.

Possible improvements, each needing a product decision: counting only required checks, watching PRs created outside Create PR for a card's branch, a slow read after a pass to catch re-runs, ordering failed cards higher, or a card tint for red. Each must keep the cost model above: nothing for cards without a PR, and nothing for finished, passing PRs.
