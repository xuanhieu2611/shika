# Worktree preparation

Shika keeps creating a fresh, isolated worktree for each card. Optional project preparation makes that worktree ready before its agent starts: copy selected local files, then run the commands the project needs.

This is preparation, not a warm-worktree pool or a compiler-cache feature. It does not promise faster launches or end-to-end performance parity with another app. Projects without configuration keep the existing workflow. Shika does not infer a package manager, run a build automatically, share mutable dependency folders, or execute Cursor/Codex configuration.

Start here when contributing to this feature. [PLAN.md](../PLAN.md#optional-worktree-preparation) records the product decision; this document explains usage and implementation. [MANUAL_CHECKS.md](../MANUAL_CHECKS.md#optional-worktree-preparation) separates verified behavior from remaining GUI checks.

## Two-minute overview

- **Before:** New created a draft branch/worktree and started the CLI. Ignored local files and dependency installation were left to the user or agent.
- **Now:** An optional, approved project configuration inserts copying and setup between worktree creation and CLI startup. Nothing runs when a project is added.
- **Ownership:** Shika still owns worktrees and branches. Setup uses ordinary child processes, not an agent PTY. There is no new model API, session history, or second visible terminal.
- **Safety:** Register the worktree before setup, exclude active setup from leftovers, and remove failed work only when it is provably untouched. Quit preserves worktrees.
- **Concurrency:** Two setups run at once outside the core operation lock. Card completion uses shared-state identity, not a captured row index.
- **Start reading code:** `Core::create_session_with_preparation`, `preparation::prepare`, then `Shika::begin_launch`. See the [contributor guide](#contributor-guide) for the full map and debugging recipe.

## Why this design

The goal was "configure once, then prompt several isolated agents", while preserving an already-working fresh-worktree lifecycle. A new checkout does not contain ignored local configuration or installed dependencies. Making it ready is a separate problem from allocating it quickly.

| Decision | Reason and trade-off |
| --- | --- |
| Keep a fresh branch/worktree per card | Preserves isolation and existing close protections. A pool would introduce retained state, ownership, reset, and recovery rules; it was not needed to address checkout allocation. Warm build output can still be valuable, but this change does not preserve it across unrelated tasks. |
| Opt-in `.shika/worktrees.json` in the main checkout | Project-specific preparation is reviewable and shareable. Reading the main checkout lets configuration be tried before committing it; scripts and ignore rules still need to exist on the task's base. |
| Own configuration, not automatic provider-config execution | The repository-configuration approach fits the setup workflows investigated in Cursor and Codex desktop. It is not format compatibility. Silently importing another tool's scripts would obscure which code Shika has permission to run. |
| Local approval of the parsed configuration | A repository cannot grant itself execution consent. Parsed equality avoids reapproval for JSON formatting or omitted defaults, but changes to command strings, array order, paths, or timeout ask again. Referenced script contents are not pinned. |
| Literal, independent copies of selected ignored files | Supplies local configuration without copying tracked edits or sharing mutable task files. Globs, directory copying, symlinks, hardlinks to the source, and overwrite behavior were deliberately excluded. |
| Explicit commands, no inferred installs or builds | A project's correct setup depends on its workflow. Automatically building every card could delay prompting and consume significant CPU/disk. Setup improves readiness, not necessarily speed. |
| Two setup slots and fresh-worktree retry | Bounds simultaneous heavyweight work without serializing all app actions. A fresh retry avoids trusting partially installed or modified state; valuable failed work stays separately journaled. The limit is policy, not a measured optimum for every machine. |

### Evidence behind keeping fresh worktrees

The initial exploratory benchmark on 2026-10-05 used Shika's repository at `42ca34e0e477eeb738151a5bb95617b883a3fc87`, an Apple M4 Pro with 24 GiB RAM, and disposable checkouts:

- Fresh worktree creation: median **38.4 ms**, 10 samples. Four concurrent creations: median **79.9 ms total**, five batches.
- Full workspace debug builds into separate empty targets: **71.498 s** and **71.504 s**, using `cargo build --workspace --locked --offline -j 2`.
- Reusing populated build output: unchanged builds took **0.235-0.567 s**. Each populated target occupied about **2.2 GiB** of per-directory output.

Checkout allocation was inexpensive here; compilation dominated the measured build time. These were Git/build-operation measurements, not application comparisons. Dependency sources and filesystem caches were warm; there were no network-cold installs, concurrent full builds, GUI timings, or model requests. Warm reuse modeled native Git operations, not the Treehouse binary. Do not generalize these numbers into performance guarantees.

Compiler-cache investigation was explicitly paused, and pooling was not adopted. Neither is an implied follow-up task for this feature. Any future proposal needs a separate decision and evidence.

Background references, not dependencies or a promise of current CLI parity: [Treehouse](https://github.com/kunchenguid/treehouse), [Cursor worktrees](https://cursor.com/docs/configuration/worktrees), [Codex desktop worktrees](https://developers.openai.com/codex/app/worktrees), and [Codex local environments](https://developers.openai.com/codex/app/local-environments). The investigated Codex desktop setup behavior was not verified for its CLI. Never add a provider's worktree flag: Shika already created the checkout.

## Configure once

Create `.shika/worktrees.json` in the project's main checkout:

```json
{
  "copy-files": [".env.local"],
  "setup-worktree": ["npm ci"],
  "timeout-seconds": 600
}
```

Choose commands appropriate for your repository. For a longer procedure, use a tracked script:

```json
{
  "setup-worktree": ["./scripts/setup-worktree.sh"]
}
```

The script must exist on the base the new task starts from. The configuration is read from the main checkout, including uncommitted configuration changes; its commands run inside the task worktree. Shika does not copy the main checkout's tracked edits into the task.

All fields are optional. `setup-worktree` is an ordered array of nonempty shell-command strings. `copy-files` is an array of literal relative file paths. `timeout-seconds` defaults to 600 and must be between 1 and 3600. The timeout covers copying and all commands together, after a setup slot is available; it excludes the base fetch, checkout creation, and queue wait. Unknown fields are errors. Configuration is limited to 64 KiB, 64 commands, and 128 copied files.

The configuration file and its `.shika` directory must not be symlinks. Invalid or unreadable configuration blocks New instead of silently skipping setup.

Commit the configuration if everyone on the project should use it. Never put secret values in commands or configuration. Keep secrets in ignored local files instead. Removing the configuration disables preparation for subsequent launches, without changing running cards.

## Approval

Adding a project never runs its setup. The first New with configuration shows the selected files, exact commands, timeout, and a warning that commands run with your permissions. Enter approves and starts; Escape cancels without allocating a worktree.

Approval is saved locally with that project in `projects.json`, not in the repository. A change to the parsed configuration requires approval again. Formatting-only changes do not. Shika rechecks configuration before allocation and before starting the CLI, and does not launch on an unexpected change.

Approval trusts the repository and its scripts. It does not freeze script contents, lockfiles, dependencies, or executables named by a command. Commands are not sandboxed and can access the rest of your machine. Review changes to project code as you would before running it yourself. The CLI's own first-run/authentication or workspace-trust prompts remain its responsibility.

## Local files

Listed files must be ignored by Git in **both** the main checkout and the new task. For example, `.env.local` must already be ignored on the task's base. Missing files, directories, symlinks, and files that are not ignored fail preparation rather than being skipped.

Paths are literal, not globs. Absolute paths, traversal, leading `./`, control characters, `.git`, and `.worktrees` components are rejected. Parent directories are created as needed. Rooted file operations do not follow symlinks and never overwrite an existing destination. Each task receives independent file contents, not a symlink or hardlink to the source; permission bits are preserved.

Do not copy `node_modules`, virtual environments, or build directories. Install what is needed with an explicit command in the new worktree. Separate task directories prevent accidental shared-file writes; they do not isolate ports, databases, services, global caches, or credentials. Scripts must account for those resources themselves.

## Commands

Commands run sequentially with `/bin/sh -c`, in the task worktree, using Shika's captured login-shell PATH. Each command is a separate shell, so an `export` or `cd` does not carry into the next command. Use one command or a script when state needs to carry across steps.

Available environment variables:

| Variable | Value |
| --- | --- |
| `SHIKA_PROJECT_ROOT` | Main checkout path |
| `ROOT_WORKTREE_PATH` | Same main checkout path |
| `SHIKA_WORKTREE_PATH` | New task worktree path |
| `PWD` | New task worktree path |

Quote these variables in shell commands. Git checkout-redirection variables are removed, as they are for the agent PTY. `TERM` is `dumb`, stdin is closed, and stdout/stderr stream into the existing terminal pane. Setup cannot answer interactive questions. Use unattended commands; do not start long-lived servers, daemonize, or escape the process group with `setsid`.

Shika runs at most two preparations at once. Additional cards show "Waiting for setup slot...". Setup does not hold the core operation lock, so other sessions can launch, stream output, and close. Setup input is suppressed rather than saved as a future agent prompt. Normal CLI terminal-query replies are enabled when the agent starts.

## Failure, cancellation, and retry

A nonzero command stops the sequence. Copy errors, timeout, configuration changes, and cancellation also stop preparation. The agent is not launched into an incomplete environment. The card shows setup stages and output, with Cancel setup while running and Retry setup after failure.

- On the cards, `c` cancels setup or closes a failed card. `r` retries a failed setup. These are not terminal shortcuts; Ctrl+Q returns from the terminal to the cards.
- Retry creates a **fresh** worktree, with approval again if configuration changed. It never layers another installation on a failed tree.
- Ordinary descendant processes are stopped on completion, timeout, or cancellation, before the agent starts or cleanup runs. Processes that deliberately leave the process group are unsupported.
- A provably untouched failed task is removed, including its ignored setup artifacts. Tracked edits, nonignored untracked files, commits, pushed work, a changed branch, or an unverifiable state keep the tree journaled in Leftover worktrees for inspection and explicit cleanup. Do not use ignored files as the only copy of valuable work.
- Active preparation worktrees are journaled but are not offered as disposable leftovers.
- Quit and project removal stop setup but preserve its worktree. A later launch offers explicit leftovers cleanup. A crash can leave setup descendants alive; inspect and stop them before removing their worktree.

Once the CLI starts, existing branch naming, shell behavior, Ready notifications, close protections, and cleanup rules apply unchanged. Preparation itself does not send a model prompt or post a Ready notification.

## Contributor guide

### Implementation map

Use symbols rather than line numbers, which drift. The core stays UI-free; the existing terminal crate is reused without adding a process launcher there.

| Location | Symbols | Responsibility |
| --- | --- | --- |
| `crates/shika-core/src/preparation.rs` | `PreparationConfig`, `load`, `validate_path` | Strict JSON schema, defaults/limits, rooted config loading, and literal-path validation |
| Same module | `directory`, `relative_file`, `copy_file` | Descriptor-relative copying, regular-file checks, independent destination creation, and permissions |
| Same module | `PreparationControl`, `PreparationLimiter`, `PreparationSlot` | Cancellation/preservation flags, registered process group, and two-slot RAII limit |
| Same module | `prepare`, `run_command`, `drain`, `child_exited`, `check_deadline` | Copy-then-command order, shared deadline, bounded pipe reads, exit observation, group stop/reap |
| `crates/shika-core/src/projects.rs` | `Project::approved_preparation`, `ProjectDb::approve_preparation` | Locally persisted consent; missing field is backward-compatible and unapproved |
| `crates/shika-core/src/lib.rs` | `project_preparation`, `preparation_approved`, `approve_preparation` | Read-only preflight, parsed-config comparison, recheck before saving approval |
| Same file | `create_session`, `create_session_with_preparation` | Existing entry point delegates to the preparation-aware lifecycle; core enforces consent even without the UI |
| Same file | `Core::preparing`, `cancel_preparations`, `remove_project`, `leftovers_list`, `leftover_remove`, `cancel_session_start` | Pending ownership, quit/removal cancellation, protected leftovers, and abandoned-launch preservation |
| `crates/shika-core/src/error.rs` | `Preparation`, `PreparationNeedsApproval`, `PreparationCancelled` | Failure detail, approval fence, and explicit cancellation outcomes |
| `crates/shika/src/main.rs` | `request_launch`, `Overlay::Preparation`, `approve_preparation` | Async preflight, approval UI, and local consent save |
| Same file | `begin_launch`, `retry_preparation`, `Card::launch_control`, `launch_error`, `stage` | Pane/card creation, background launch, fresh retry, progress and failure UI |
| Same file | `HostState::preparing`, `agent_starting`, `Host::write`, `bind_host` | Setup input gating, early CLI query replies, PTY binding/resizing |
| Same file | `tick`, `close`, `Card::drop`, `stop_cancelled_launch`, `main`'s `on_app_quit` hook | Cancel requests, deferred card removal, dropped UI, and synchronous quit cancellation |

The only added persisted project field is `Project::approved_preparation`, serialized as `approvedPreparation` in `projects.json`. Its value is the parsed config with the same `setup-worktree`, `copy-files`, and `timeout-seconds` keys, not a boolean, hash, script snapshot, or copied-file contents. A missing field means no consent. Progress, controls, process-group IDs, and failure-card state are memory-only; `settings.json` is unchanged.

`libc` was added directly to `shika-core` for rooted file operations and process-group control; it was already in the workspace lockfile transitively. No provider launch flags or model integration were added.

### Launch lifecycle and lock boundaries

```text
New / Retry
  -> read main-checkout configuration and local approval
  -> if needed: display exact config -> approve or cancel
  -> create pane/card with session = None
  -> background Core launch:
       fetch base, best effort
       [operation lock] recheck config; allocate draft; journal; register pending
       [no operation lock] wait for slot; copy files; run commands; release slot
       [operation lock] recheck cancel/project/config/branch; start PTY; insert session
       unregister pending; on error evaluate guarded cleanup
  -> bind host; find card by shared-state identity
  -> install session, or leave failure output / handle an abandoned launch
```

The journal entry is added immediately after allocation, before any setup command. If adding the entry fails, allocation is rolled back. IDs account for both existing sessions and journaled draft branches, including preparing and leftover tasks. Journal schema is unchanged: `{ projectId, branch, path, baseRef? }`.

`Core::preparing` maps worktree paths to project IDs and controls. It is transient, not a second journal. Both live-session paths and pending paths are excluded from `leftovers_list`, so explicit leftover removal cannot delete an active setup. A relaunch has neither in-memory map and lists journaled worktrees instead.

Only short allocation and finalization sections hold `Core::operations`. Fetching, slot wait, file copying, and commands do not. Do not move a package install, child wait, or slot wait under that lock. The shared setup deadline starts inside `prepare`, after acquiring the slot. It is checked between operations/copy chunks and during command polling; it is not a hard real-time interrupt for blocking filesystem I/O or Git subprocesses.

Configuration is a launch snapshot. Approval is rechecked against the displayed value before saving, and the core rereads configuration before allocation and before the PTY opens. This is not a file watcher: editing config during setup does not immediately stop the already-approved snapshot, but a mismatch at finalization prevents agent launch. Restoring the exact previously approved parsed value can reuse consent. Deleting config disables preparation rather than clearing stored approval.

### File and process safety details

Copy safety is implemented using opened directory descriptors and `openat`, with `O_NOFOLLOW` at each component, `O_CLOEXEC`, and `O_EXCL` on destination creation. Leaf opens use `O_NONBLOCK` so a FIFO cannot block before its regular-file check. New parent directories use mode `0700`; destination files start at `0600`, then receive the source's ordinary `0777` permission bits, not setuid/setgid bits. Do not replace this with a path precheck followed by `fs::copy`: that reintroduces symlink/overwrite races. These guards constrain Shika's copier, not the subsequently approved shell script. On macOS, `mode_t` is `u16`; the variadic `openat` argument must be integer-promoted, as `relative_file` does with `c_uint`. Keep that conversion when refactoring the FFI.

Each command gets its own process group. Spawn and group registration are fenced by the control's group mutex against synchronous cancellation. The polling loop drains limited amounts of each nonblocking pipe before checking again, so a flood cannot indefinitely starve cancellation. Stdout and stderr are shown in polling order, not guaranteed cross-stream chronological order. Output remains in the pane's in-memory terminal scrollback; there is no persistent setup log or session history.

`child_exited` uses `waitid(..., WNOWAIT)` to observe the leader without reaping it. The group is stopped with `SIGKILL` before `Child::wait` reaps the leader. Keeping the PID reserved until signaling, and taking the group registration once under the mutex, avoids a later signal reaching a reused PID. This sequence also stops ordinary background descendants after a successful command. Preserve it when changing the runner; `try_wait` reaps too early for this design.

Cancellation is intentionally forceful, not a graceful shutdown protocol. Shika cannot roll back external side effects or terminate a daemon that escaped the group. A slot/control belongs to one launch; do not share one control between concurrent commands or retry a cancelled control.

### UI state, input, and concurrency traps

Preparing/failed cards have `session = None`. Their coarse status is still Waiting, with a separate setup `stage` or `launch_error`. They must not enter agent quiet/Ready notification logic just because an installer printed output. Shell creation is unavailable until a session exists.

Configured launches release the app's global `busy` gate so another card can start or an existing terminal can be used. Short unconfigured launches retain the previous busy behavior. Deferred cancelled-card removal in `tick` waits until no conflicting busy operation or overlay is active. This avoids invalidating the stable indices still used by other short UI operations.

Long-launch completion and retry locate their card through `Arc::ptr_eq` on `agent.state`. Never carry a card index across a long setup await: another cancellation/removal can shift the array. Completion only focuses the terminal if its card is still selected and no overlay is open. See [keyboard flow](keyboard-flow.md) for overlay restoration and navigation rules.

A configured pane can be hidden by a second launch before its first layout. `begin_launch` supplies an initial measured grid from the current pane, or 32 rows by 100 columns, so startup cannot wait forever for a visible layout. Actual layout and `bind_host` resize the PTY afterward. Do not remove this fallback without replacing the hidden-pane startup path.

Input has three phases:

| Phase | `preparing` | `agent_starting` | `Host::write` behavior |
| --- | --- | --- | --- |
| Copy/setup | true | false | Drop typing, paste, and terminal query replies; never queue an installer answer |
| `PreparationEvent::StartingAgent`, before UI completion | true | true | Permit only `InputSource::Reply`, queued until PTY binding if needed |
| Session installed | false | irrelevant | Existing agent input and typeahead behavior |

The typed `StartingAgent` event is emitted immediately before opening the agent PTY. A CLI may issue a cursor-position query before the background launch returns. Suppressing those replies can stall or misrender startup; allowing all input too early can send an installer answer as a model prompt. Keep these cases distinct. A configured error turns `agent_starting` off again. Stage strings are presentation, not the input-phase protocol.

### Failure and ownership matrix

| Situation | PTY/card outcome | Worktree outcome |
| --- | --- | --- |
| Approval cancelled or invalid config at preflight | No launch/card allocation | No tree allocated |
| Copy, command, timeout, config fence, or branch check fails | No lasting agent session; configured card keeps failure output | Remove only if cleanup proves it untouched; otherwise keep journaled |
| User cancels queued/running setup | Stop setup; remove cancelled card after completion | Same guarded cleanup |
| Quit/project removal during setup | Stop setup using the preservation flag | Keep journal and tree, even if clean |
| Launch returns a session after its UI disappeared or cancellation won the UI race | `cancel_session_start` hangs up/removes the session | Always preserve its journal/tree; this is not discard |
| Normal running task closes | Existing close/discard/push rules | Existing safety behavior |

Automatic failed-launch cleanup requires all of: preservation flag unset, HEAD still on the draft branch, readable Git state, no dirty files, no own commits, and no pushed branch. Removal must succeed before its journal entry is removed. Ignored files do not make a tree dirty and may be deleted, including partially installed dependencies. Failure to prove safety preserves rather than guesses. Retry creates a separate new draft; it does not repair the preserved one.

### Debugging by symptom

Start with the setup terminal output, the main checkout's config, and the **isolated** data directory's `projects.json`/`worktrees.json`. `--diagnostics-file` covers CLI discovery and notification metadata, not setup output. Redact secret values and private paths before sharing logs or screenshots.

| Symptom | Inspect first |
| --- | --- |
| No setup, or unexpected approval | Confirm `.shika/worktrees.json` is in the registered project's checkout, not only the task. Check schema and `approvedPreparation` parsed equality. Missing config means no setup; `{}` is still an opt-in config. |
| Approval keeps returning | Check command whitespace/order, explicit versus default timeout, concurrent config edits, and `approve_preparation`'s reread. JSON formatting alone should not invalidate it. |
| Copy fails although the source is ignored | Check the task's actual base and its ignore rules; source-only ignore changes are insufficient. Run the two read-only Git checks below. Check regular-file status, symlink parents, duplicate paths, and destination existence. |
| Script missing, or setup runs an older version | Commands run from the task's base, not main-checkout tracked edits. Commit the script to the intended base; do not fix this by copying arbitrary tracked files. |
| Binary missing from a Finder launch | Inspect `path_env`, CLI discovery, and the captured login-shell PATH. `/bin/sh` is not an interactive login shell; exports in one command do not persist to another. |
| Card stuck waiting for a slot | Inspect active runners and slot drops; the two-slot limit is not a worktree/card limit. Waiting does not consume the setup deadline. |
| Hidden card never begins | Inspect fallback grid, `HostState::measured`, and the measurement loop in `begin_launch`. |
| Agent startup hangs or shows a broken first frame | Inspect `StartingAgent`, reply gating, `pending_input`, and `bind_host` before changing terminal encoding. |
| Completion changes the wrong card or steals focus | Check shared-state identity, selection/overlay guards, and deferred cancelled-card removal. |
| Failed tree remains, or disappears unexpectedly | Inspect preservation flag, current branch, status, own commits, and pushed state. Ignored output alone is disposable; unverifiable state is preserved. |
| A process writes after cancel/quit | Inspect group registration, stop-before-reap, and whether the script daemonized. After a crash, stop surviving writers before explicit leftover cleanup. |
| Parallel tests intermittently fail with `AlreadyExists` in `Fixture::new` | The current integration fixture names its root using PID plus a timestamp, then uses `create_dir_all`. Concurrent equal timestamps can share a root and collide when creating `repo with spaces`. This is a test-harness limitation observed during the documentation check, not a setup command failure. Rerun serially as below; a proper fix needs atomic unique-directory allocation, not ignoring the error or sharing a repository. |

Read-only checks, with paths filled in for the disposable fixture:

```sh
GIT_OPTIONAL_LOCKS=0 git -C "$repo" check-ignore -v -- .env.local
GIT_OPTIONAL_LOCKS=0 git -C "$task" check-ignore -v -- .env.local
GIT_OPTIONAL_LOCKS=0 git -C "$task" status --porcelain
GIT_OPTIONAL_LOCKS=0 git -C "$task" symbolic-ref --short HEAD
GIT_OPTIONAL_LOCKS=0 git -C "$repo" worktree list --porcelain
```

### Reproducible native smoke fixture

This recipe needs no package download and contains no secret. It creates a disposable repo and separate app data, not configuration for Shika's own source checkout:

```sh
source "$HOME/.cargo/env"
./scripts/bundle-app.sh --debug

fixture=$(mktemp -d "${TMPDIR:-/tmp}/shika-preparation-docs-XXXXXX")
fixture=$(cd "$fixture" && pwd -P)
repo="$fixture/repo"
mkdir -p "$repo" "$fixture/data"
git -C "$repo" init -b main
git -C "$repo" config user.name Test
git -C "$repo" config user.email test@invalid.example
git -C "$repo" config commit.gpgsign false
git -C "$repo" config core.hooksPath /dev/null
printf '%s\n' '.env.local' '*.generated' > "$repo/.gitignore"
printf '%s\n' original > "$repo/tracked.txt"
git -C "$repo" add .
git -C "$repo" commit -m 'Disposable preparation fixture'

mkdir "$repo/.shika"
printf '%s\n' fixture-only > "$repo/.env.local"
printf '%s\n' '{
  "copy-files": [".env.local"],
  "setup-worktree": ["test -f .env.local && printf prepared > ready.generated"],
  "timeout-seconds": 45
}' > "$repo/.shika/worktrees.json"

open -n target/debug/Shika.app --args \
  --data-dir "$fixture/data" --diagnostics-file "$fixture/diagnostics"
printf 'Add this disposable project: %s\n' "$repo"
```

In that test instance, add the printed repo using Add project. New should ask for approval; Escape should allocate nothing. New again, approve, and inspect the new path in `worktrees.json`. The copied file and `ready.generated` must exist before the real CLI starts. Its own trust/authentication prompt can still appear. No model prompt is required for this check. Do not open New on a personal project by mistake.

Replace the fixture's command(s) for these cases. JSON must escape backslashes inside command strings:

| Case | Configuration change | Expected result |
| --- | --- | --- |
| Visible failure/order | `setup-worktree`: `["printf 'visible error\\n' >&2; exit 7", "touch should-not-run.generated"]` | First command/output shown, second not run, no agent; clean tree removed; Retry available |
| Valuable failed work | `setup-worktree`: `["printf changed > tracked.txt; exit 7"]` | Tree retained in leftovers with its edit; retry uses another tree |
| Queue/cancel | `setup-worktree`: `["printf 'setup-running\\n'; sleep 30"]` | First two cards run, third waits; cancel the queued/running card without touching the others |
| Timeout | `setup-worktree`: `["sleep 30"]`, `timeout-seconds`: `1` | Timeout, no agent, guarded cleanup |
| Changed config | Edit config while slow setup is running | Old approved snapshot may finish; final config mismatch blocks agent; next New/Retry asks again |
| Quit | Quit the test instance while slow setup is running | Setup group stops; journaled tree remains; reopen with the same isolated data to inspect leftovers |

Confirm the specific test Shika window is foreground before synthetic input, and never drive an ordinary instance with automation. Quit the test instance and confirm its setup processes stopped before deleting the disposable fixture. Temporary evidence paths in `MANUAL_CHECKS.md` are machine-local and can disappear; this versioned recipe and tests are the reproducible starting point.

### Tests, acceptance, and safe extension

```sh
source "$HOME/.cargo/env"
cargo test -p shika-core preparation
cargo test -p shika setup_input_is_not_queued_as_a_future_agent_prompt
cargo test -p shika prepared_agent_startup_preserves_query_replies_but_not_setup_input
cargo fmt --all --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

If the parallel run hits the fixture naming collision described above, use this diagnostic workaround:

```sh
cargo test -p shika-core preparation -- --test-threads=1
```

Keep the standard parallel workspace run in PR validation; a serial pass is not evidence that the collision or other concurrency issues are fixed.

`crates/shika-core/src/preparation_tests.rs` uses disposable repositories, isolated data, and a fake CLI; it does not call a model. Runner/copier/limiter unit tests also live in `preparation.rs`. The app host tests distinguish setup input suppression from startup reply handling. Useful regression anchors:

| Concern | Existing tests to extend |
| --- | --- |
| Consent and rechecks | `consent_persists_but_changed_configuration_requires_new_consent`, `changed_configuration_during_setup_never_launches_an_agent` |
| Copy safety | `local_files_are_independent_and_commands_finish_before_agent_launch`, `destination_files_and_symlinks_are_never_overwritten_or_followed`, `a_file_ignored_only_in_the_source_is_not_copied_into_the_task` |
| Config bounds/type | `malformed_unknown_and_unbounded_configuration_is_rejected`, `oversized_and_nonregular_configuration_is_rejected_without_blocking` |
| Rollback versus preservation | `failed_command_stops_sequence_never_launches_agent_and_cleans_untouched_tree`, `failure_preserves_tracked_edits_untracked_files_and_commits_in_leftovers`, `a_completed_launch_abandoned_by_its_ui_retains_the_journaled_worktree` |
| Cancellation/process ownership | `cancellation_kills_setup_descendants_and_removes_clean_pending_worktree`, `background_descendants_are_stopped_before_the_agent_starts`, `shutdown_preserves_pending_worktree_and_stops_its_setup` |
| Concurrency | `two_setup_slots_bound_concurrency_and_waiting_is_cancellable`, `slow_setup_does_not_hold_operation_lock_or_block_another_session` |

The implementation validation passed 191 workspace tests, formatting, strict Clippy, a debug bundle, signature verification, and isolated native smoke checks. That is historical evidence, not a substitute for validating your change. Native smoke did not perform a real dependency install or model task; all-provider setup, focus races, mouse controls, hidden-card interaction, and minimum-size/long-config checks remain explicitly tracked in [MANUAL_CHECKS.md](../MANUAL_CHECKS.md#optional-worktree-preparation).

Before extending the feature:

1. Preserve default/no-config behavior, local consent, fresh task branches, and unchanged provider argv. No automatic setup on project add or implicit config import.
2. Keep journaling ahead of setup and retain work whenever safety cannot be proved. Do not equate ignored output with valuable-work preservation.
3. Keep long work outside core/UI global locks. Preserve hidden-pane draining, startup sizing, identity-based completion, and input-phase separation.
4. Add failure/race tests with temporary repositories and a fake CLI before adding native acceptance claims. Use the fixture for UI checks, never normal app data.
5. Treat schema changes as consent changes: add backward-compatible defaults only deliberately, keep unknown-field rejection, and update parsing/approval tests. The approval record contains the parsed config, not a versioned script digest. Consider whether deserializing a new field's default into old approval records would silently authorize new behavior; specify migration/reconsent rather than assuming equality is sufficient.
6. Globs/directories, daemon support, persistent setup logs, pooling, and compiler caches need separate design decisions. Do not quietly broaden trust or ownership boundaries as an optimization.
7. Update this guide, the short [AGENTS.md](../AGENTS.md) handoff, [PLAN.md](../PLAN.md), and relevant acceptance checks together. For UI changes, read [design/DESIGN.md](../design/DESIGN.md) and [keyboard flow](keyboard-flow.md). Keep intended behavior separate from verified GUI results.
