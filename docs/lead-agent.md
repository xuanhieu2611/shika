# Lead agent

Status: implemented: `help`, `tasks`, `new`, `status`, `wait`, `read`, `diff`, `send`, `key`, `pr`, and `close`, the doorbell that wakes an idle Lead, the worker report channel (`shika report`), and the Lead card and picker. GUI acceptance of `pr` and `close` is pending (see [MANUAL_CHECKS.md](../MANUAL_CHECKS.md#lead-agent-spike)). This guide records the decision and the contract the implementation keeps. Update it with the code.

Reference material lives beside it: [shika-cli.md](shika-cli.md) is the command reference (syntax, exact output, exit codes, `wait` rules, protocol, security model, debugging, and how to add a command), and [crates/shika-core/README.md](../crates/shika-core/README.md) maps the crate that holds the shared protocol and the Lead session code. This guide keeps the why; those hold the details.

A project can have one Lead: an agent CLI the author talks to, which starts and drives ordinary task cards for them through a small `shika` command. The author states a goal once; the Lead splits it into tasks, starts a worker card per task, waits for them, reads their results, and brings back what needs a decision.

Read [AGENTS.md](../AGENTS.md) first, then [tasks-and-worktrees.md](tasks-and-worktrees.md), [agent-activity.md](agent-activity.md), [terminal-tabs.md](terminal-tabs.md), and [publishing.md](publishing.md): the Lead reuses all of them and replaces none. Visuals follow [design/DESIGN.md](../design/DESIGN.md).

## The Lead is a manager

Treat coding agents as real software engineers. The author is the CEO: they give the vision ("I want X, Y, and Z"). The Lead is the manager: it talks with the author until it understands the goal, splits it into tasks, explains each task fully to an engineer (a worker), gets notified when an engineer is done, does a quick check that it works, and reports to the author. Workers are the engineers: they build, test their own work, commit, and report back.

A manager does not read every line their engineers write and does not micromanage. The trust rests on QA, which for Shika is the repository's CI plus the author's own testing or QA team. Shika users do not merge Lead work straight to production: PRs target the task's base branch (a personal or dev branch, by the author's choice), and making code safe for production is the author's responsibility (their QA, their own testing, or CI on the production merge). Firstmate gets the same trust from its no-mistakes pipeline; Shika relies on CI and the author instead.

How this shapes the design:

- **Workers report.** Each worker the Lead starts can run `shika report <text>` to say what it changed, the tests it ran and their result, and its commit. Shika appends an instruction to do so to the worker's prompt. The Lead learns results from these reports, in `wait`, `status`, and `tasks`.
- **A quick scope check, not a review.** The Lead's default check on a ready worker is the report plus `shika diff <task> --stat`, to confirm the scope matches the task. If the report does not say the tests pass, it asks the worker to run them and report; it does not run them itself.
- **`read` and full `diff` are on demand**: when the author asks, when a worker stopped without reporting, when a worker is asking (a dialog), or when the report or stat looks wrong.
- **The Lead briefs fully and asks first.** Before starting workers it makes sure it understands the goal, asks the author short clarifying questions when the goal is ambiguous, and gives each worker a self-contained task (outcome, scope, acceptance criteria, how to test).
- **The summary is honest about QA.** The Lead tells the author what each worker says it tested and what nobody verified; it does not call work correct, and it is not the QA team.

## Why it exists

The current loop for three changes is: Cmd+N, pick a CLI, type a prompt, repeat twice, then watch three cards, answer each one, read each diff, and create each PR. The author is the scheduler. Cursor Projects and Claude Code Projects (both September 2026 betas) and [firstmate](https://github.com/kunchenguid/firstmate) move that job to one coordinator agent the author talks to.

Shika's version differs from those on purpose:

| Decision | Reason |
| --- | --- |
| The Lead is one of the author's CLIs in a real terminal | Bring your own CLI: Shika still calls no model API and draws no transcript. The Lead is a card like any other. |
| Workers are ordinary cards | Nothing is hidden in a cloud thread. The author can open any worker, type into it, read its Changes panel, or close it. Auditability is the main complaint about hosted coordinators. |
| A `shika` command over a local socket, not MCP | Every supported CLI can run a shell command, so one path serves Claude Code, Codex, Cursor CLI, and Pi with no per-CLI configuration injection (Cursor has no per-session MCP flag). It loads no tool schemas into context until the Lead runs `shika help`. The author can run the same commands in a shell. This is how Herdr exposes itself to agents and how firstmate drives it. |
| Shika owns the mechanics, the Lead owns the judgment | Worktrees, PTYs, activity state, diffs, and publishing already exist in Rust. The Lead never re-derives them from files or screen parsing. firstmate needs about 114,000 lines of shell for this because it has no app; Shika does not. |
| The Lead is a manager and workers are engineers who report | Matches how the author wants to work: a CEO with a manager, not a reviewer reading every diff. Results come from the workers' own reports; CI and the author are QA; PRs go to the task's base, never straight to production. |
| Workers get a report-only token | The report is the one thing a worker must be able to say to the Lead. A token limited to `report` and `help` adds that without letting a worker start, read, or steer anything. No files are written into the repository and no CLI configuration is edited. |
| One Lead per project | Matches Cursor Projects and the project-grouped column. A Lead plans inside one repository. |
| Mixed vendors | A Claude Code Lead can start Codex and Cursor workers. Hosted coordinators cannot. |

## User-facing contract

- **Start.** The author starts a Lead for a project with any found CLI. A project has at most one live Lead. Like every session, a Lead is memory only and does not come back after a relaunch.
- **Card.** The Lead card sits first in its project group and is marked as the Lead. It has the normal status signal and terminal. It has no diff stat, no Create PR, and no shell tabs.
- **Worktree.** The Lead runs in its own detached worktree, `<repo>/.worktrees/shika-lead-<id>`, created from the project's base with `git worktree add --detach`. It reads code there. It has no branch, so it cannot be pushed or published, and an accidental edit never touches the main checkout or a worker. Preparation (`.shika/worktrees.json`) does not run for it. Closing the Lead removes its worktree after the normal dirty check.
- **Never codes.** The Lead's guide tells it that every change, even a one-line fix, goes to a worker. Its worktree is for reading.
- **Workers.** `shika new` does exactly what New does: a fresh `shika-draft-<id>` worktree from the project's base (or `--base`), preparation and its approval, the preset's launch flags, branch naming from the prompt and then the CLI title. The worker card appears in the project like any other, marked as started by the Lead. Notifications, status, timers, the Changes panel, Create PR, and Close behave as they do today.
- **Authority stays with the author.**
  - The Lead cannot merge, push, or commit through Shika. Workers are CLIs and may commit in their own worktree, as they can today; the guide asks them not to push.
  - `shika pr` selects the worker's card and opens the existing Create PR confirmation for the author, exactly as Cmd+Shift+P does, and blocks until they confirm or cancel. Nothing is published without that confirmation.
  - `shika close` closes a task directly only when Close would not need to ask (clean, nothing unpublished, not working or blocked). Otherwise it opens the normal close dialog for the author and prints what they chose. Shika never chooses for the author.
  - Both are refused while another dialog is open or another Lead dialog is pending, and work only on workers this Lead started. When the dialog ends, selection and focus return to the Lead.
  - The author can still do everything by hand on any card.
- **Questions.** When a worker shows Asking, the author sees the usual notification and can answer in that terminal. The Lead sees the same state through `shika wait` or the doorbell, reads the dialog with `shika read`, and may answer: a folder trust or first-run dialog for the worker's own Shika worktree with `shika key`, a real question its instructions answer with `shika send`. Anything else, and anything destructive or outside the worker's worktree, goes to the author, naming the task.
- **Finishing.** The Lead is woken when workers settle, reads each worker's report, checks its scope with `shika diff <task> --stat`, and does not edit files itself. Full `diff` and `read` are for when the author asks, a worker did not report, or something looks wrong.
- **Reporting.** A worker started by a Lead reports with `shika report <text>`. The latest report from the worker's current turn shows under its task line (`TaskInfo::report`). A new turn starts with none. A report changes no status and ends no turn; settlement still comes from activity.

## The `shika` command

The app binary doubles as the client. When its first argument is a command word below (or `--json` followed by one), `main` connects to the socket in `SHIKA_SOCKET`, sends `SHIKA_TOKEN`, and prints the reply, without starting GPUI. Output is plain text for the model by default; `--json` gives one JSON object for scripts. Exit status is 0 on success, 1 on a refusal with a one-line reason, 2 on an `error` reply or a usage, environment, or connection error. Replies from the app (including refusals) print to stdout; client-side failures print to stderr. Exact formats: [shika-cli.md](shika-cli.md).

| Command | Effect |
| --- | --- |
| `shika help` | Prints the Lead guide: role, rules, commands, and a worked example. Versioned with the app. |
| `shika tasks` | Lists the project's tasks: id, title, CLI, status, elapsed time, branch, diff stat, PR, and whether this Lead started it. |
| `shika new --cli <claude\|codex\|cursor\|pi> [--base <branch>] <prompt>` | Starts a worker and prints its task id once the CLI is running. Refuses when preparation needs the author's approval, the CLI is missing, or the worker limit is reached. |
| `shika status <task>` | One task's line from `tasks`. |
| `shika wait [<task>...] [--timeout <seconds>]` | Blocks until one of the tasks (default: every live task this Lead started) is Ready, Asking, or exited, then prints which and why. Prints the still-working tasks on timeout. Default timeout 100 seconds, under the default command timeout of the CLIs. Waiting costs no tokens. The server caps it at 600 seconds. |
| `shika read <task> [--lines <n>]` | The worker's agent terminal as plain text: the visible screen plus up to `n` lines of scrollback (default 0, max 2000). |
| `shika diff <task> [--stat]` | The task's changes, computed exactly as the Changes panel and diff stat are, as unified diff text. |
| `shika send <task> [--no-enter] <text...>` | Types `text` into the worker's agent terminal as a bracketed paste, then Enter as a separate delayed write. With Enter it starts a turn like a typed line. |
| `shika key <task> <key>...` | Presses named keys in order (`enter`, `escape`, arrows, `tab`, `space`, `backspace`, `a`-`z`, `0`-`9`), for answering dialogs. |
| `shika pr <task>` | Selects the card and opens Create PR. Blocks until the author confirms or cancels, then prints the PR URL, `refused: The author cancelled.` (exit 1), or the error (exit 2). Refused while the task is Working, Asking, or Exited. |
| `shika report <text...>` | Worker only. Stores the text as the worker's latest report (trimmed, control characters removed, at most 4 KB), tagged with its current turn, and prints `reported`. A Lead running it is refused. |
| `shika close <task>` | Closes the task when Close would not ask (prints `closed`); otherwise opens the close dialog, blocks, and prints what the author chose or that they cancelled. |

Each task line ends with `path=`, the worker's worktree, which the Lead may read and must not edit.

The prompt is passed to the worker CLI as its positional prompt. All four CLIs accept one (`claude [prompt]`, `codex [PROMPT]`, `agent [prompt...]`, `pi [messages...]`, checked from `--help` on 2026-10-09). Because nobody types it, Shika treats it as the first submission (see Positional prompts below): the card is named from its first line, the branch is renamed once through the existing first-prompt path, and the CLI's own title renames it later as usual.

### Scope and limits

- **Token.** Each Lead PTY gets `SHIKA_SOCKET` and a random `SHIKA_TOKEN`. The app finds the Lead by the token on its card. Shell tabs and the author's cards get neither.
- **Worker tokens.** Each worker a Lead starts gets, on its agent PTY only, `SHIKA_SOCKET`, its own random `SHIKA_TOKEN`, and the `shika` directory first on `PATH`. The app finds the worker by the token on its card. That token may run `report` and `help` only; everything else is refused with `Workers can only run shika report.` So a worker still cannot start workers, read other tasks, or steer anything: the tree is one level deep. The only new thing a worker can do is write one bounded string that its Lead reads. It dies with the card.
- **The footer.** `shika new` appends a fixed instruction to the Lead's prompt after a blank line (`worker_prompt`): `When you finish or get blocked, run: shika report "<what you changed, the tests you ran and their result, your commit>". Do not push.` `send` does not add it. The card and branch are still named from the prompt's first line.
- **Project.** A Lead sees only its project's tasks and starts workers only there.
- **Ownership.** `tasks`, `status`, `read`, and `diff` work on every task in the project. `send`, `key`, `wait`, `pr`, and `close` work only on tasks this Lead started. A card the author created stays the author's.
- **Limit.** At most 4 live workers per Lead. `new` refuses the fifth with the reason. A setting can come later. How a card counts: [shika-cli.md](shika-cli.md#scope-and-security).
- **`send` and `key` guard.** Refused while the worker is Working (only `key <task> escape` may interrupt it), when its PTY is gone, and when the author typed into that agent terminal after its last submission, so a Lead never appends to the author's unsent draft. Allowed when Waiting, Ready, or Asking. Reasons and code: [shika-cli.md](shika-cli.md#shika-send-task---no-enter-text).
- **Doorbell.** See below.
- **After the Lead closes,** its workers stay as ordinary cards owned by the author.

## Implementation map

| Piece | Where |
| --- | --- |
| Request and reply types, JSON lines, argument parsing, text rendering, `ControlDir`, `new_token` | `shika-core/src/control.rs` (module docs summarize the protocol; [shika-cli.md](shika-cli.md#protocol) is the reference) |
| Lead session, detached worktree, env, `LaunchOptions` (prompt, owner, per-launch base) | `Core::create_lead`, `Core::create_session_with_preparation`, `Core::lead_for_project`, `Core::workers_of`; `Error::LeadUnsupported` guards the per-session methods a Lead cannot use |
| Client: environment, argument parse, round trip, exit status | `shika/src/control_client.rs` (`run`), dispatched at the top of `main` before GPUI |
| Server: accept loop, one thread per connection, handoff to the UI thread | `shika/src/control.rs` (`Server`, `serve`, `Incoming`) |
| Command handling and `wait` | `shika/src/control.rs`: `Shika::drain_control`, `handle_control`, `control_new`, `resolve_waiters`; pure parts `Ledger`, `Observed`, `resolve` |
| Worker token, report, footer | `Card::worker_token`, `Card::report` (`WorkerReport`), `Shika::handle_worker`, `worker_command`, `worker_prompt`, `WORKER_FOOTER` in `shika/src/control.rs`; `WorkerEnv`, `LaunchOptions::control` in core; `clean_report`, `WORKER_HELP` in `shika-core/src/control.rs` |
| Lead card state | `Card::lead` (`LeadState`: token, `Ledger`, waiters, `Doorbell`), `Card::started_by`, `Card::launch` (`Launch::{Task, Worker, Lead}`) |
| `pr` and `close`: refusals, pending dialog, outcome to reply, focus return | `shika/src/control.rs`: `Shika::control_dialog`, `dialog_refusal`, `settle_lead_dialog`, `LeadDialog`, `dialog_reply`; hooks in `create_pr`, `publish_pr`, `close`, `finish_close`, `cancel_overlay` (`main.rs`); `Shika::publish_blocker` is shared with Cmd+Shift+P |
| Doorbell, send and key typing, guards | `shika/src/control.rs`: `Doorbell`, `Gate`, `Shika::ring_doorbells`, `Shika::type_into`, `input_refusal`; `HostState::capture_typed`, `has_draft` in `main.rs` |
| Terminal text for `read`; diff text for `diff` | `Terminal::text_with_history` (`shika-terminal`); `shika_core::render_unified` (`shika-core/src/diff.rs`) |
| Launch prompt as the first submission | `HostState::seed_launch_prompt`, `Activity::hold_for_launch`; see [agent-activity.md](agent-activity.md#launch-prompts-and-turns) |
| Lead guide text | `shika/src/lead_guide.md`, embedded with `include_str!` |
| Card, picker, menu | `Shika::new_lead`, `start_lead`, `show_lead`, `card_view`, `Overlay::Picker { lead }`, `Target::Lead` in `changes.rs`; DESIGN.md "Lead card" |

**Socket and command path.** At startup Shika creates a `0700` directory under the temp dir, `shika-control-<pid>-<nonce>/`, holding the socket `sock` and `bin/shika`, a symlink to the running executable. The Lead PTY gets `bin/` prepended to its `PATH`, so `shika` always runs the same build as the app, whether from `cargo run` or a bundle. Keep the path short: macOS limits a socket path to 104 bytes. Do not put the bundle's `Contents/MacOS` on `PATH` instead: its `Shika` binary and a `shika` name collide on a case-insensitive volume. Remove the directory on quit.

**Requests run on the app thread for state and off it for work.** A connection thread reads one request line, checks the protocol version, and sends the request to the UI thread over an `mpsc` channel, then waits on a reply channel holding no lock. `Shika::tick` drains that channel (as it drains notification clicks) and answers. Blocking work (the preparation approval check, git, worktree creation) runs in the background like the existing launch paths, and its completion answers. While waiting, the connection thread checks every 500ms whether the client left; if so it drops its reply channel, and a `wait` then keeps its events unreported.

**Token.** The token is a field of the Lead's card (`Card::lead`), not a table: the app finds the Lead by comparing the request's token with its cards. It exists before the Lead's session does, so the first `shika` call cannot race the launch, and it dies with the card (Close, project removal), so there is no revocation step. An unknown token is a refusal.

**`new`.** Same launch path as New (`begin_launch`), with `Launch::Worker(LaunchOptions { prompt, started_by, base, control })`. The prompt is the Lead's text plus the report footer, and `control` carries the socket, a fresh worker token, and the command directory for the worker's agent PTY. Differences: the card is not selected, focus and dialogs are untouched, `busy` is not set, and the pane gets a fallback terminal size because nobody has laid it out. The reply (`Started`) is sent once the session exists, after preparation. The client sets no read timeout for `new`, because preparation can take as long as `timeout-seconds`; the server always ends the wait with a launch result, a refusal or error, or (when the app quits) a closed socket. The reply carries the title the card gets from the launch prompt, not the `New <CLI>` placeholder; the branch is the draft name until the rename lands. A worker whose launch fails without setup is marked `Card::discard` and removed by the tick, never directly, because an open dialog (`Overlay::Close { index }`) holds card positions. `--base` is `LaunchOptions::base`: it applies to that task only and never changes the project's saved base; it must exist locally or on origin (fetched when new there).

**`wait`, as built.** Per Lead, `Ledger` remembers the last settlement reported for each task as (status, turn). A task has settled when it is Ready, Asking, or Exited. A `wait` returns, with every task that has settled and was not reported, as soon as one exists. The turn is `Activity::turn_started`, so a task that works again and settles again is reported again, while asking and then becoming ready in the same turn are two reports. With nothing unreported and nothing Starting or Working it returns at once with no events and `timed_out` false. Otherwise it waits; at the deadline (capped at 600 seconds) it returns `timed_out` with the tasks still working. The tick resolves waiters (`resolve_waiters`), so there is no polling thread. A settlement is marked reported only after the connection thread confirms it wrote the reply, so a client that leaves at any point loses no event; the price is a possible duplicate (see [shika-cli.md](shika-cli.md#wait-in-detail)). A task whose prompt has been handed to the CLI but not yet consumed by the tick counts as Working (`Card::control_status`), so `wait` right after `new` does not return "nothing to wait for". `Starting` exists in the protocol, but `task_info` hides a card with no session, so clients never see it. A task in `waiting` is neither busy nor settled, so a `wait` over only such tasks returns `nothing to wait for`. Named tasks must be workers this Lead started.

**Doorbell, as built.** A Lead that ends its turn cannot see workers finish (the 2026-10-09 test: the Lead's `wait` had returned two Asking events, it ended its turn, and nothing told it when the workers finished). Each tick, `Shika::ring_doorbells` asks `Doorbell::due` for settlements (Ready, Asking, Exited) of the Lead's workers that no `wait` has reported and that were not rung before, keyed like `wait`'s ledger by (task, status, turn). It rings after a 1.5 second batch window, and only while `Gate` is open: no pending or in-delivery `wait`, the Lead card Ready or Waiting with no unprocessed submission, no unsent typed text in the Lead's terminal, no author typing for 3 seconds, and a live PTY. If any fails it rings later, when they clear. The line is `[shika] Workers changed: <id> "<title>" is ready; ... Run shika wait.`, submitted by `Shika::type_into`: a bracketed paste, then Enter as a separate write 150 ms later, because Codex's paste-burst handling reads an Enter inside a paste as a newline. It goes through the same typed-input capture as a keystroke (`HostState::capture_typed`), so the Lead's turn machinery treats it as a submission and the card shows Working. The writes do not stamp `last_typed`, so Shika never counts its own bell as the author typing. A bell does not mark a settlement reported; the Lead's `wait` does. The doorbell is on for every Lead and has no setting. Details and delivery failure rules: [shika-cli.md](shika-cli.md#the-doorbell).

**`send` and `key`, as built.** `Shika::control_input` checks ownership and `input_refusal`, then `type_into` writes the steps (`paste_steps`, or `key_steps` through `shika_terminal::input::encode_key` for the terminal's mode). Their writes bypass `last_typed` but a `send` with Enter goes through the typed capture, so it is a submission; `--no-enter` and keys do not. If the author types between a paste and its Enter, the Enter is dropped.

**`pr` and `close`, as built.** Both reuse the author's flows and add only a waiting reply. `Shika::control_dialog` runs the refusal checks (`dialog_refusal`: pending Lead dialog, any overlay or busy flag, ownership, setup, and for `pr` the status and `Shika::publish_blocker`, the same function `create_pr` uses), stores one `LeadDialog` in `Shika::lead_dialog`, selects the card, and calls `create_pr` or `close`. The flows report into it (`lead_dialog_succeeded`, `lead_dialog_failed`), and `Shika::settle_lead_dialog` answers once the dialog is gone and nothing is running. It runs after each flow step and on every tick, so a card, project, or Lead that disappears still answers; an app quit drops the channel and the connection thread answers. A failure leaves the dialog open for a retry and is sent only if the author then gives up; success wins. Cancelling a Close dialog does not send the author to the worker's shell when a Lead asked. Focus: selection goes to the Lead card and its terminal is focused (`focus_terminal`), because the author was talking to the Lead. Reply mapping and exit codes: [shika-cli.md](shika-cli.md#shika-pr-task).

**Limit.** `MAX_WORKERS` (4) counts cards started by the Lead that are not exited, discarded, or failed, including finished ones the author has not closed.

**Lead bootstrap.** The Lead CLI starts with the preset's normal flags and one positional prompt that names the project and tells it to run `shika help` and then ask the author what to do. No file is written into the repository and no CLI configuration is edited. The Lead's first prompt, like a worker's, starts a turn at launch: see below.

**Positional prompts and the activity model.** Shika starts a turn only from a typed submission, and a worker started with a positional prompt has none, so its card would have stayed Waiting and `wait` would never have fired. `HostState::seed_launch_prompt` records the launch prompt through the same fields a typed first line sets (submission count, submission time, task name, finished naming capture), and the tick consumes it like any submission. A launch turn is also marked (`Activity::hold_for_launch`) because the CLI's banner and idle editor while it boots would otherwise look like output followed by idle and end the turn before the CLI began. Details and tests: [agent-activity.md](agent-activity.md#launch-prompts-and-turns).

**Traps.**

- A Lead has no branch. Anything that calls a per-session Core method refuses it with `LeadUnsupported`, which would show an error toast: CLI title polling, the diff stat on Ready, the Changes panel (`Target::Lead`), Create PR, shell tabs, and the Close dialog's branch wording are all guarded by `Card::lead`. Add the same guard to any new per-session feature.
- Cards are addressed by position in several places (`Overlay::Close { index }`, selection). The control path must only append cards; removals go through the tick.
- A report is stored on the card with the turn it was made in (`Activity::turn_started`) and shown only while that turn is current. Anything that starts a turn without a report (a typed line, `send`) therefore clears it from the Lead's view.
- Lead and worker prompts reach the CLI as one argument, so a prompt that starts with `-` or is empty is refused by Core before anything is created.
- The `bin/shika` symlink points at the running executable, so a Lead started from `cargo run` and one from a bundle each get their own build. The client path must stay free of GPUI, settings, and app data.
- A crashed app leaves its `shika-control-*` directory in the temp dir; an orderly quit removes it.
- Inside a Lead terminal (`SHIKA_SOCKET` or `SHIKA_TOKEN` set) the binary is always the client, so a bare or mistyped `shika` is a usage error (exit 2) and never starts a second app. Outside one, only a command word makes it the client. A new command must be added to `is_command` and `parse_args`, or outside a Lead terminal the binary will not run as the client.
- The client environment check runs before argument parsing, so outside a Lead terminal every invocation prints the same sentence.


## Not in the first version

- A setting to turn the doorbell off, or to change its batch window.
- Durable Lead memory or a project notes file shared between workers.
- Schedules, triggers from CI or issues, and cloud execution.
- A Lead across projects, or a Lead starting a Lead.
- Granting the Lead permission to publish or close without the author's confirmation. `pr` and `close` always ask.

## Rollout

1. **Spike.** Socket, token, `help`, `tasks`, `new`, `wait`, `status`, and a minimal Lead card. Acceptance: a Claude Code Lead starts two Codex workers on a disposable repository and reports when both are Ready, with no keystrokes from the author after the goal.
2. **Read and steer.** `read`, `diff`, `send`, `key`, and the doorbell. Built.
3. **Finish.** `pr` and `close` through the existing dialogs, and the Lead card and picker UI per DESIGN.md. Built; GUI acceptance pending.
4. **Manager and reports.** `shika report`, worker tokens, the prompt footer, `TaskInfo::report`, the reported mark on the doorbell, protocol 4, and the rewritten Lead guide (manager, quick scope check, read and diff on demand). Built; GUI acceptance pending.
5. Record GUI acceptance in [MANUAL_CHECKS.md](../MANUAL_CHECKS.md), then update [AGENTS.md](../AGENTS.md) and the docs index.

Test against disposable repositories and `--data-dir`, never normal app data. Launch from a built `.app` with `open` to test CLI discovery and session titles.
