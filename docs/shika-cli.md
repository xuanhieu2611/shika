# The `shika` command

Reference for the command a Lead agent runs (and the one command, `report`, its workers run): what each subcommand does, the wire protocol under it, the security model, how to debug it, and how to add a command. The product decision and design rationale are in [lead-agent.md](lead-agent.md); read that first for why it exists. The shared protocol code is documented in [crates/shika-core/README.md](../crates/shika-core/README.md).

Contents: [What it is](#what-it-is) - [Commands](#commands) - [Output and exit codes](#output-and-exit-codes) - [Scope and security](#scope-and-security) - [wait in detail](#wait-in-detail) - [The doorbell](#the-doorbell) - [Protocol](#protocol) - [Debugging](#debugging) - [Adding a command](#adding-a-command) - [Code map](#code-map)

Status: `help`, `tasks`, `new`, `status`, `wait`, `read`, `diff`, `send`, `key`, `pr`, `close`, and the memory commands `memory`, `remember`, and `forget` are implemented, plus the doorbell that wakes an idle Lead and `report`, the one command a Lead-started worker can run. Since protocol 6, `pr` publishes directly without the author's Create PR dialog (the author's decision, see [`shika pr`](#shika-pr-task---title-text---body-text)); `close` still goes through the author's Close flow. GUI acceptance of `pr` and `close` is pending (see [MANUAL_CHECKS.md](../MANUAL_CHECKS.md#lead-agent-spike)).

## What it is

`shika` is a small client for a local Unix socket that the running Shika app serves. A Lead (an ordinary agent CLI in its own terminal) runs it as a shell command to list tasks, start worker cards, wait for them, read their terminals and diffs, and answer their questions. Every supported CLI can run a shell command, so no per-CLI configuration is injected and no MCP server exists.

**There is no separate binary.** The app binary is the client. At the top of `main`, before GPUI, settings, or app data are touched, it checks `shika_core::control::is_client_invocation` on its arguments and on whether `SHIKA_SOCKET` or `SHIKA_TOKEN` is set. Inside a Lead terminal (either variable set) it is always the client, whatever the arguments, so a bare or mistyped `shika` prints the usage and exits 2 and never starts a second app on the real data. Elsewhere it is the client only for a command word (`help`, `tasks`, `new`, `status`, `wait`, `read`, `diff`, `send`, `key`, `pr`, `close`, `report`), optionally after a leading `--json`; `--data-dir`, `--diagnostics-file`, and Finder or `open` launches start the app. When it is the client, `control_client::run` talks to the socket and the process exits with its status.

**Where it works.** Only in the agent terminal of a Lead, or of a worker that Lead started. Two environment variables make it work:

- `SHIKA_SOCKET`: absolute path of the socket.
- `SHIKA_TOKEN`: 32 hex characters, the credential. A Lead's token runs the Lead commands; a worker's own token runs only `report` and `help` (see [`shika report`](#shika-report-text)).

Run anywhere else, it prints `shika commands run inside a Shika Lead or Lead-started worker terminal.` to stderr and exits 2. Cards the author creates, and every shell tab (a Lead-started worker's too), have both variables removed, so they get the same message.

**How `shika` gets on the Lead's `PATH`.** At startup the app installs the command and creates a per-run socket directory (`shika_core::control::install_command` and `ControlDir`):

```text
<data dir>/bin/shika                 symlink to the running executable, replaced atomically at every start
<tmp>/shika-control-<pid>-<nonce>/   mode 0700
  sock                               the listening socket
```

`<data dir>` is the app data directory, `~/Library/Application Support/com.hieule.shika` or the `--data-dir` given, so the command path is the same on every launch (a Lead's recorded conversation names it, and a resumed Lead must find it again). `<tmp>` is the canonicalized temp directory (on macOS, under `/private/var/folders/...`); the socket path only reaches environment variables, which are fresh for every process, so it may change. The Lead's `PATH`, and the agent PTY's of each worker it starts, is `<data dir>/bin:` followed by the login-shell `PATH`. Consequences:

- The Lead guide, help preamble, and worker report footer provide the shell-quoted absolute `<data dir>/bin/shika` path (the `Application Support` space and any quote survive the quoting). Use it instead of an unqualified `shika`: a provider's login shell (reproduced with Codex 0.162.1) can reset PATH. It invokes the build of the app that started last. No shell startup files or provider configuration are changed.
- The bundle's `Contents/MacOS` is deliberately not put on `PATH`. The bundle binary is `Shika`; a `shika` name in the same directory would collide with it on a case-insensitive volume (macOS defaults to one). The separate `bin/` directory holds only the symlink.
- The socket path must fit `sockaddr_un.sun_path` (104 bytes on macOS, 103 usable). `ControlDir::create` fails with `Could not set up the control directory` when it would not. Keep the directory name short.

## Commands

Global rules: `--json` is accepted before the command word and, for every command but `new` and `send`, anywhere after it (for those two, only before the prompt or text). With `--json` the reply prints as one JSON line instead of text. Unknown options are usage errors. Task ids are the ids `tasks` prints.

Output samples below use two workers: `18f3a9c2b7d4e601` (working) and `18f3a9c2b7e0aa12` (ready, PR 42), in a repository at `/repo`. Task lines are shown without the trailing ` path=/repo/.worktrees/<branch>` for width; the real output has it. Ids are the full session ids; there are no short forms.

### `shika help`

Prints the Lead guide: `crates/shika/src/lead_guide.md`, embedded in the app with `include_str!`. Takes no arguments. It is versioned with the app, so the Lead reads instructions that match the commands the running app supports.

```text
$ shika help
You are the Lead for this project in Shika. The author talks only to you. ...
```

JSON: `{"type":"help","text":"..."}`.

### `shika tasks`

Lists every task in the Lead's project, one line each, in the order the app holds its cards. That includes tasks the author started. It never lists the Lead itself, and a card with no session yet (still being created) is absent.

```text
$ shika tasks
18f3a9c2b7d4e601 working "Fix login redirect" cli=codex branch=fix-login-redirect diff=2 files +64 -3 elapsed=3m12s by-lead
18f3a9c2b7e0aa12 ready "Fix login redirect" cli=codex branch=fix-login-redirect diff=2 files +64 -3 pr=#42 by-lead
```

Line format (`task_line`): `<id> <status> "<title>" cli=<cli>`, then, when present: ` branch=<branch>`, ` diff=<n> files +<added> -<removed>`, ` elapsed=<d>`, ` pr=#<n>`, ` by-lead`, ` path=<worktree>`. `path` is the worker's absolute worktree, which the Lead may read (never edit); it is quoted when it contains whitespace. The title is printed with Rust's `{:?}` quoting. `elapsed` appears only while the task is `working` and is formatted `59s`, `3m12s`, or `1h05m`. `diff` appears once the card has fetched its diff stat (when it turned Ready, and only if files changed). `by-lead` means the calling Lead started it. With no tasks it prints `No tasks.`

JSON:

```json
{"type":"tasks","tasks":[{"id":"18f3a9c2b7d4e601","title":"Fix login redirect","cli":"codex","status":"working","elapsed_secs":192,"branch":"fix-login-redirect","diff_stat":{"files":2,"added":64,"removed":3},"pr":null,"started_by_lead":true,"path":"/repo/.worktrees/fix-login-redirect"}]}
```

`report` is the worker's latest report from its current turn (`null` when it has not reported). In text, it prints on indented lines under the task line:

```text
18f3a9c2b7e0aa12 ready "Fix login redirect" cli=codex branch=fix-login-redirect diff=2 files +64 -3 pr=#42 by-lead
  report:
    Fixed the redirect loop; added a regression test. cargo test: 31 passed. Commit 3f2a9c1.
```

Absent values are `null`. An empty project is `{"type":"tasks","tasks":[]}`.

Statuses, from `Card::control_status`: `starting`, `working`, `waiting`, `asking`, `ready`, `exited`. They map from the card's status as follows: Working, Waiting, Asking, Ready are the card's own; `exited` means the CLI process ended; a prompt just handed to the CLI that the app tick has not consumed yet counts as `working`. `starting` and the launch-failure `exited` exist in the code for a card with no session, but `task_info` returns nothing for such a card, so a client cannot currently observe `starting`.

### `shika status <task>`

One task, in the `tasks` line format. Works on any task in the project, whoever started it.

```text
$ shika status 18f3a9c2b7e0aa12
18f3a9c2b7e0aa12 ready "Fix login redirect" cli=codex branch=fix-login-redirect diff=2 files +64 -3 pr=#42 by-lead
```

Exactly one id. Refuses `No task <id> in this project.` for an unknown id, an id from another project, and the Lead's own id. JSON: `{"type":"status","task":{...}}`.

### `shika new --cli <claude|codex|cursor|pi> [--base <branch>] <prompt...>`

Starts a worker card, exactly as the author's New does: a fresh `shika-draft-<id>` worktree from the project's base (or `--base`), the preset's launch flags, the prompt as the CLI's positional argument, preparation if configured and approved, branch naming from the prompt and then the CLI's title. See [tasks-and-worktrees.md](tasks-and-worktrees.md).

```text
$ shika new --cli codex Fix the login redirect loop. Add a test. Commit on your branch, do not push.
started 18f3a9c2b7d4e601 working "New Codex" cli=codex branch=shika-draft-18f3a9c2b7d4e601 elapsed=0s by-lead
```

The reply is built the moment the session exists. Its title is the one the card shows once the launch prompt is seeded (the first nonblank prompt line, whitespace collapsed, capped at 80 characters), not the `New <CLI name>` placeholder. The branch is whatever it is at that moment, usually still the draft name: the first-prompt and CLI-title renames follow within a second or so, so run `shika tasks` to see the settled branch. The id is the stable handle. JSON: `{"type":"started","task":{...}}`.

Parsing (`parse_new`): flags come first. The first plain word starts the prompt, and everything from there on is the prompt, even words that look like flags (`new --cli claude explain --json output` has the prompt `explain --json output`). `--` also starts the prompt. The prompt words are joined with single spaces, so quote one argument to keep newlines. `--cli` is required and must be one of `claude`, `codex`, `cursor`, `pi` (`CLI_IDS`). An empty prompt is a usage error. A prompt that starts with `-` passes the parser after `--` but is refused by core (below).

The reply arrives after the CLI is running. When the project has an approved setup configuration, that includes the whole setup, and the card shows its progress.

The card does not take selection or focus, no dialog opens, and the author's typing is not interrupted. The Lead sees the card only through `tasks`, `status`, and `wait`.

Refusals (exit 1), all with a one-line reason:

| Reason | Cause |
| --- | --- |
| `Shika is still looking for installed CLIs. Try again.` | The CLI catalog has not loaded yet. |
| `<Name> is not installed (no \`<binary>\` on PATH). Pick another CLI.` | The preset binary was not found on the login-shell PATH. |
| `This Lead already has 4 live workers. Wait for them, and ask the author to close finished tasks before starting more.` | The worker limit (below). |
| `This project's setup needs the author's approval in Shika first: start one task with Cmd+N and approve it.` | `.shika/worktrees.json` exists and is not approved. A Lead can never approve setup. |
| `This Lead was closed.` | The Lead closed while the approval check ran. |
| core `InvalidPrompt` text | The prompt is empty, starts with `-`, or contains a NUL byte. |
| core `NoSuchBranch` / `BaseBranchMissing` text | `--base` exists neither locally nor on origin. |
| core `CliNotFound` / `PreparationNeedsApproval` text | Raced with the checks above. |

Any other launch failure (git failure, setup failure, cancelled preparation) is an `error` reply, exit 2 (`failure_reply`). `--base` applies to that task only and never changes the project's saved base.

The client has no read timeout on `new`: it waits as long as worktree preparation takes (`timeout-seconds`, default 600, configurable). The server always ends the wait: a launch that fails gets a `refused` or `error` reply, a closed Lead or cancelled preparation is answered, and a quitting app closes the socket, which the client reports as `Shika closed the connection without a reply.` A client that is killed is noticed by the connection thread within 500 ms.

### `shika wait [<task>...] [--timeout <seconds>]`

Blocks until one of the tasks is `ready`, `asking`, or `exited`, then prints which. See [wait in detail](#wait-in-detail) for the precise rules. Default timeout 100 seconds (`DEFAULT_WAIT_SECS`), capped at 600 by the server (`MAX_WAIT_SECS`).

```text
$ shika wait
ready: 18f3a9c2b7e0aa12 ready "Fix login redirect" cli=codex branch=fix-login-redirect diff=2 files +64 -3 by-lead
still working:
  18f3a9c2b7d4e601 working "Fix login redirect" cli=codex branch=fix-login-redirect diff=2 files +64 -3 elapsed=3m12s by-lead
```

```text
$ shika wait --timeout 5
timed out: no task needs attention yet
still working:
  18f3a9c2b7d4e601 working "Fix login redirect" cli=codex branch=fix-login-redirect diff=2 files +64 -3 elapsed=3m12s by-lead
```

```text
$ shika wait
nothing to wait for
```

Each event line is `<status>: <task line>`. JSON:

```json
{"type":"waited","events":[{"task":{...}}],"still_working":[{...}],"timed_out":false}
```

Refusals: `No task <id> in this project.` and `<id> was not started by this Lead, so it cannot wait on it.` An unparsable `--timeout` is a usage error.

### `shika read <task> [--lines <n>]`

Prints the text of the task's agent terminal: the visible screen, plus up to `n` lines of scrollback above it (default 0, at most 2000; more is a usage error). Works on any task in the project. Trailing spaces on each line and trailing blank lines are removed; an empty terminal prints `(the terminal is empty)`. It never includes the author's shell tabs. It reads the terminal engine directly (`Terminal::text_with_history`), so it does not scroll the view, move the selection, or disturb the card. A full-screen CLI (the alternate screen) has no scrollback, so `--lines` adds nothing there.

```text
$ shika read 18f3a9c2b7d4e601
Do you trust the contents of this directory?
> 1. Yes, continue
  2. No, quit
```

JSON: `{"type":"text","text":"..."}`. Refusal: `No task <id> in this project.`

### `shika diff <task> [--stat]`

The task's changes, computed exactly as the Changes panel and the card's diff stat compute them (`Core::session_diff`: commits since the merge base with the ref the card started from, uncommitted edits, and untracked files that are not ignored), rendered as text by `shika_core::render_unified`. Works on any task in the project. Git runs off the app thread.

```text
$ shika diff 18f3a9c2b7e0aa12
diff M src/lib.rs (+4 -1)
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,3 +1,6 @@
...
diff U tests/slugify.rs (+12 -0) untracked
--- /dev/null
+++ b/tests/slugify.rs
@@ -0,0 +1,12 @@
...
2 files +16 -1
```

Each file starts with `diff <letter> <name> (+<added> -<removed>)`, where the letter is `A`, `M`, `D`, `R`, or `U` as in the panel. A rename reads `old -> new`. An untracked file is shown as added and marked `untracked`. A binary file reads `(binary)` with `Binary file, not shown.` and no body. A mode change adds `mode 100644 -> 100755`. A file the panel shows collapsed (over 2,000 changed lines, over 1 MiB of patch, or past the task's budget) is not expanded; a line says how many changed lines are not shown and to read the file in the worktree. The last line is the total. `--stat` prints one line per file (`M src/lib.rs +4 -1`, `R a -> b +0 -0`, `M logo.png binary`, `U notes.md +2 -0 untracked`) and the same total. No changes prints `No changes.`

The output is capped at 200 KB (`RENDER_CAP_BYTES`) and always cut at a whole line. When it is cut, a line before the total reads `[truncated at 200 KB after N of M files. Read the rest in the worktree: <path>]`, so the Lead can read the files themselves. A Lead has no diff: `diff` on the Lead's own id is `No task`. A git failure is an `error` reply (`Could not read changes` and git's first line). JSON: `{"type":"text","text":"..."}`.

### `shika send <task> [--no-enter] <text...>`

Types `text` into a worker's agent terminal as a bracketed paste, then presses Enter as a separate write about 150 ms later. With `--no-enter` it types the text and stops, for a dialog's text field. Only on a worker this Lead started.

```text
$ shika send 18f3a9c2b7d4e601 Also add a test for the empty string.
sent 38 characters and Enter to 18f3a9c2b7d4e601.
```

Parsing (`parse_send`): `--json` and `--no-enter` come before the text; the first plain word starts the text and everything after it is the text, flags included; `--` also starts it. The words are joined with single spaces. Control characters are dropped. If the program has not turned on bracketed paste (a shell prompt, say), line breaks become spaces, so nothing submits early.

Text sent with Enter is a submission exactly like a typed one: it goes through the typed-input capture (`HostState::capture_typed`), so the card turns Working, its timer starts, a Ready notification follows, and `wait` sees the turn. `--no-enter` is not a submission and is not recorded as a draft; a `key enter` that follows is a plain key press, not a submission. Use plain `send` to give a worker instructions.

Refusals (exit 1), each with a one-line reason (`input_refusal`):

| Reason | Cause |
| --- | --- |
| `No task <id> in this project.` | Unknown id, another project, or the Lead itself. |
| `<id> was not started by this Lead, so it cannot type into it.` | The author's own card. |
| `<id> is working. Wait for it, or interrupt it with: shika key <id> escape` | The task is Working (including a just-submitted prompt). |
| `<id> has exited, so there is nothing to type into.` | The CLI process ended. |
| `<id> is still starting. Try again.` | The task has no running CLI yet. |
| `The author has typed into <id>'s terminal and not sent it. Do not add to it; tell the author.` | Typed text on the line, or a recalled history entry, not yet submitted (`HostState::has_draft`). A CLI's own grey suggestion is not typed, so it does not count. |
| `<id>: the author typed into it meanwhile, so nothing more was sent.` | The author typed between the paste and its Enter. The text stays in the editor as a draft. |

JSON: `{"type":"done","message":"..."}`. The reply comes after the Enter is written, not before.

### `shika key <task> <key>...`

Presses the keys in order, for answering dialogs. Keys: `enter`, `escape`, `up`, `down`, `left`, `right`, `tab`, `space`, `backspace`, and single characters `a`-`z`, `0`-`9` (case-insensitive; at most 32 keys). Bytes come from `shika_terminal::input::encode_key` for the terminal's current mode, so arrow keys are `ESC [ B` normally and `ESC O B` when the program turned on application cursor keys. Keys go out about 40 ms apart. Same ownership and refusals as `send`, with one difference: a `key` made only of `escape` is allowed while the task is Working, to interrupt it.

```text
$ shika key 18f3a9c2b7d4e601 enter
pressed enter in 18f3a9c2b7d4e601.
```

A key is not a submission. A key that resolves a dialog is seen by the usual activity detection, which moves the card on (Asking to Working, then Ready). An unknown key is a usage error; `ctrl-c` and function keys do not exist.

### `shika pr <task> [--title <text>] [--body <text>]`

Publishes a worker this Lead started, directly: no dialog opens and the author presses nothing. It runs the pipeline the Create PR dialog runs when the author confirms it without editing a field: stage every non-ignored change, commit if anything is uncommitted, push the task branch (never forced), then create the PR with the author's authenticated `gh`, or reuse the open PR from this branch to the same target. The target is the task's recorded starting base, exactly as the dialog prefills it. The command blocks until publishing ends and prints the PR URL.

```text
$ shika pr 18f3a9c2b7e0aa12
https://github.com/acme/app/pull/42
$ shika pr 18f3a9c2b7e0aa12 --title "Fix login redirect" --body "Sends users to /home after login.

Tests: npm test passes."
https://github.com/acme/app/pull/42
```

**Why it does not ask.** Until protocol 6, `pr` opened the author's Create PR dialog and waited for them to read the title and description and press Enter, then the Lead relayed the result. The author found that a needless round trip and decided (2026-10-17): "I want my leader to open the PR for me, and just give me the response." Asking the Lead to open the PR is the author's confirmation. The guide still tells the Lead to run `pr` only when the author asks to publish. `close` keeps its dialog, and the author's own Create PR (Cmd+Shift+P, the card button, the Agent menu) is unchanged.

Options:

- `--title <text>`: the commit message (when a commit is needed) and the PR title. One line, not blank. Default: the task title, the text Create PR prefills, which is the card's name, including a name set with Rename task.
- `--body <text>`: the PR description. It may span lines; not blank; at most 65,536 characters (GitHub's limit, `MAX_PR_BODY_CHARS`). It replaces `gh --fill` for the description. Default: `gh --fill`, the description built from the commits, as Create PR does.

Each value is one argument, so the Lead quotes it. A flag given twice, a blank value, or a multi-line title is a usage error (exit 2) before anything is sent. Neither option edits an existing PR: when the branch already has an open PR to the target, Shika pushes and prints that PR's URL, and its title and body stay as they were.

Outcomes, and how they map to exit codes (`pr_reply`):

| Outcome | Output | Exit |
| --- | --- | --- |
| The PR is created, or an open one for the branch is reused | the PR URL (`done`) | 0 |
| No PR target: the task's starting base is not recorded, is not a branch of origin, or no longer exists on GitHub (`Error::PublishNoTarget`) | `refused: The task's starting base dev is not a branch on GitHub, so there is no PR target. The author can choose one in Create PR.` (or `The task did not start from a branch of origin, ...`) | 1 |
| The worktree is checked out on another branch (`Error::TaskBranchChanged`) | `refused: The worktree is on <current>, not task branch <expected>. ...` | 1 |
| A step failed: no `gh`, signed out, unsupported origin, nothing to publish, files changed while publishing, a commit hook, a rejected push, `gh pr create` | `error: Could not publish PR: <git's or gh's first line> Completed commits and pushes were kept.` | 2 |
| The task went away while publishing | `error: The task is no longer in Shika.` | 2 |
| The app quit | `error: Shika stopped handling this command. ...` | 2 |

A missing target is never a dialog and never another branch: the dialog is the only place a target is chosen, and only the author chooses it. Partial success is kept exactly as in the dialog: a failure after the commit leaves the commit, and one after the push leaves the pushed branch. Running `pr` again after the cause is fixed skips the commit when the tree already matches HEAD and reuses an open PR.

Refusals (exit 1) before anything runs, all with a one-line reason (`Shika::pr_refusal`: `pr_gate_refusal`, `worker_refusal`, `pr_status_refusal`, then `Shika::publish_blocker`):

| Reason | Cause |
| --- | --- |
| `No task <id> in this project.` | Unknown id, another project, or the Lead itself. |
| `<id> was not started by this Lead, so it cannot publish it.` | The author's own card. |
| `<id> is still working. Run shika wait, then try again.` | Working or starting. |
| `<id> is waiting on a question or permission. Answer it first.` | Asking. |
| `<id> has exited, so it cannot be published.` | The CLI ended. |
| `<id> is still being set up.` | Setup has not finished. |
| `Wait for the agent to finish before publishing.` | The card is running by Create PR's own test (`Shika::publish_blocker`, shared with Cmd+Shift+P, so the two cannot disagree). |
| `The Lead is publishing this task. Try again when it finishes.` | A `pr` on this task is still running (`Card::lead_publishing`). |
| `The author has <dialog> open for <id>. Try again after they finish it.` | The author's own Create PR, Close, or Switch agent dialog for this same task is on screen. Any other overlay (Settings, the picker, another task's dialog) does not block `pr`, because `pr` opens none. |
| `Shika is busy with another operation. Try again in a moment.` | A git or close operation of the author's is running (`Shika::busy`). |

**What the author sees.** Nothing moves: no dialog, no selection or focus change, no toast, and no browser tab (the author's own Create PR opens the URL; `pr` leaves that to the Lead's reply). The global `busy` flag is not set, so the author keeps navigating, typing, and starting tasks while it runs. Instead the card is marked `Card::lead_publishing` until the reply: the author's Create PR, Close, and Switch agent on that card, and the Lead's own `pr`, `close`, `send`, and `key` on it, are refused with `The Lead is publishing this task. Try again when it finishes.` (a toast for the author). Afterwards the card shows the PR's checks as `#N` as after any publish, and `tasks` and `status` show `pr=#N`. The client has no read timeout: every git and gh command is bounded at 120 seconds but the chain is not, and the server always answers.

### `shika close <task>`

Closes a worker this Lead started, through the author's own Close flow (`Shika::close`).

```text
$ shika close 18f3a9c2b7e0aa12
closed
```

- When Close would not need to ask (clean, nothing unpublished, not working or blocked), the worker is closed directly and the command prints `closed`. No dialog appears.
- Otherwise the normal close dialog opens on that card: discard (Cmd+D), push when the tree is clean (Cmd+Enter), safe-close for an active or blocked turn, or branch-switch recovery (Cmd+Enter). Shika never chooses. The command blocks until the author decides, then prints what happened.
- The dialog does not open while the author is typing. It opens once nothing was typed in any terminal (agent or shell) or Lead chat input for 3 seconds, with no overlay open and Shika not busy; until then nothing is selected. After 120 seconds the command is refused: `refused: The author kept typing or had another dialog open. Try again later.` The command is also refused, while waiting, if the worker or the Lead goes away.

| Outcome | Output | Exit |
| --- | --- | --- |
| Closed with no question | `closed` | 0 |
| The author discarded | `closed (the author discarded the worktree and branch)` | 0 |
| The author pushed first | `closed (the author pushed the branch first)` | 0 |
| Branch-switch recovery | `closed (the author used branch-switch recovery; both local branches were kept)` | 0 |
| The author cancelled | `refused: The author cancelled.` | 1 |
| The close failed and the author cancelled, or the task vanished | `error: <text>` | 2 |

Refusals (`Shika::control_close`: `dialog_gate_refusal`, then `worker_refusal`): unknown task, not started by this Lead (`... so it cannot close it.`), still being set up, being published by a `pr` (`The Lead is publishing this task. ...`), any overlay open (`Shika has <overlay> open. ...`), busy, or another Lead dialog pending. The Lead itself, the author's cards, and other projects' cards are never closable this way. Closing a worker frees one of the 4 worker slots. Selection and focus return to the Lead afterwards. The worker's shown tab stays, and the client has no read timeout.

### `shika memory`, `shika remember <text...>`, `shika forget <n>`

Lead only. The project's Lead memory: short notes that start every new Lead conversation. Why and how they are delivered: [lead-agent.md](lead-agent.md). They live in `<data dir>/leads/<project-id>/memory.json`, one file per project, outside the repository. The file is plain JSON (`{"notes":[{"text":"...","created":<unix seconds>}]}`) and may be read or edited by hand.

```text
$ shika memory
No notes.
$ shika remember Use pnpm, not npm. CI runs `pnpm test`.
remembered 1
$ shika remember Workers must not touch db/migrations.
remembered 2
$ shika memory
1. Use pnpm, not npm. CI runs `pnpm test`.
2. Workers must not touch db/migrations.
$ shika forget 1
forgot 1
$ shika memory
1. Workers must not touch db/migrations.
$ shika forget 5
refused: No note 5. There is 1 note.
```

Rules (`MemoryStore`, `clean_note`):

- A note is cleaned to one line: whitespace runs, including line breaks and tabs, become one space, other control characters are dropped, the ends are trimmed. Empty after that: `refused: The note is empty.` More than 1,000 characters (not bytes): `refused: The note is N characters; the most is 1,000. Shorten it.`
- All notes together are at most 16,000 bytes of text. Over that: `refused: Memory is full (16,000 bytes). Forget a note first.`
- Numbers are 1-based and close up after a `forget`. `forget` of a number with no note: `refused: No note N. There are M notes.` (`There is 1 note.`, `There are no notes.`).
- A file that does not parse (`Error::MemoryUnreadable`) fails all three with exit 2: `error: The Lead memory file <path> cannot be read. Shika left it as it is; fix or delete it.` Shika never overwrites it. A failed write is `error: Could not save the Lead memory file <path>.`
- `--json`: `memory` replies `{"type":"memory","notes":[{"text":"...","created":1760000000}]}` (an empty list for none); `remember` and `forget` reply with `done` and the same message.
- The commands read and write the file on the app thread; it is at most 16 KB. A worker's token is refused (`Workers can only run shika report.`), as for every Lead command.

Parsing: `parse_text` (shared with `report`) for `remember`: flags first, the first plain word starts the text, `--` also starts it. `parse_forget`: exactly one whole number, 1 or more.

### `shika report <text...>`

Worker only. A worker started by a Lead says what it did: what changed, the tests it ran and their result, its commit. The text is stored as the worker's latest report and shows to the Lead under the worker's task line in `tasks`, `status`, and `wait`.

```text
$ shika report "Added slugify to textkit with 4 tests; cargo test: 31 passed. Commit 3f2a9c1."
reported
```

Rules (`worker_command`, `clean_report`):

- The text is trimmed; tabs become spaces; control characters other than line breaks are dropped. Empty after that: `refused: The report is empty.` More than 4096 bytes (`MAX_REPORT_BYTES`): `refused: The report is N bytes; the most is 4096. Shorten it.`
- The report is tagged with the worker's current turn (`Card::control_turn`, including a submission not yet consumed by the tick) and replaces the earlier one. `TaskInfo::report` holds it only while that turn is current: a new turn (a typed line, or a `send` with Enter) starts with none.
- It changes no status and ends no turn. Settlement still comes from activity. A late or changed report on a settled task is a fresh event for `wait` and the doorbell, even if that status/turn was delivered already. Identical text retried in the same turn keeps its receipt and does not create another event. The doorbell line only marks the task `(reported)`; it never carries the text.
- Memory only, and it dies with the card.

Parsing (`parse_text`): flags first (`--json`); the first plain word starts the text; `--` also starts it, for text beginning with a dash. The words are joined with single spaces.

A worker's token may run only `report` and `help`. `help` prints three lines (`WORKER_HELP`). Every other command gets `refused: Workers can only run shika report.` A Lead token running `report` gets `refused: Only workers can run shika report.`

**The footer.** `shika new` appends this to the Lead's prompt, after a blank line (`worker_prompt`, `WORKER_FOOTER`); `send` never does:

```text
When you finish or get blocked, run: '<data dir>/bin/shika' report "<what you changed, the tests you ran and their result, your commit>". Wait for it to print reported. If reporting fails, retry after correcting the error, or include the exact failure in your final response. Do not push.
```

The actual executable path is substituted and shell-quoted, including spaces and single quotes. Credentials stay in the environment, never the prompt. Branch and card naming read the first nonblank prompt line, which is still the Lead's. The combined prompt does not start with `-`, so it passes the positional-prompt rules unchanged.

### How `pr` resolves

`Shika::control_pr` runs `pr_refusal` on the app thread, sets `Card::lead_publishing`, and spawns `Core::session_publish_now` on the background executor. That takes the core operation lock once and runs `publish::publish_now`: `publish::preview` (the same snapshot Create PR shows), a refusal when the preview has no target, then `publish::publish` with the preview's target and the given or default title and body, so every recheck of the dialog path runs. Back on the app thread it clears `lead_publishing`, starts the card's checks watch (`PrWatch::new`) on success, and sends `pr_reply`. Nothing else in the app waits on it: there is no `LeadDialog`, and `settle_lead_dialog` is not involved. If the app is gone the reply channel drops and the client gets `Shika stopped handling this command.`

### How `close` resolves

`Shika::control_close` runs the refusal checks and stores a `LeadDialog` (Lead id, task id, the reply channel, the outcome and last error, and `deferred`) in `Shika::lead_dialog` at once, so the one-dialog gate holds. If `author_quiet` is false it selects nothing and waits: `resume_deferred_close` runs on every tick, refuses after `DEFER_LIMIT` (120 seconds) or when the worker or Lead is gone, and otherwise, once quiet with no overlay and not busy, clears `deferred` and calls `start_lead_close`, which selects the card and calls `close`. The flow records what happens: `lead_dialog_succeeded` (`finish_close` success with `close_outcome`) and `lead_dialog_failed` (close failure). `Shika::settle_lead_dialog` answers once the dialog is no longer on screen and no flow is still running. It runs after every place a dialog can end (the flows, `cancel_overlay`) and on every tick, so a dialog that disappears another way (the card or project removed) still answers: `dialog_reply` gives the outcome if there is one, else the last error, else `The Lead was closed before the author answered.` or `The task is no longer in Shika.`, else `The author cancelled.` If the app quits, the reply channel drops and the client gets `Shika stopped handling this command.` If the Lead card goes away first, the client is answered at once and any dialog stays open for the author.

## Output and exit codes

| Exit | Meaning |
| --- | --- |
| 0 | The app answered with `help`, `tasks`, `started`, `status`, `waited`, `text`, `memory`, or `done`. A `wait` that timed out is still 0. `pr` prints the URL and `close` prints `closed ...` as `done`. |
| 1 | The app answered `refused`: understood and declined, with a reason. That includes the author cancelling a `close` (`The author cancelled.`) and a `pr` with no PR target. |
| 2 | The app answered `error`, or the client failed before or while talking: usage error, missing `SHIKA_SOCKET` or `SHIKA_TOKEN`, no connection, I/O failure, or an unreadable reply. |

Where text goes (`control_client::run`):

- Every reply the app sent, including `refused` and `error`, is printed to **stdout**: `refused: <reason>` or `error: <message>` in text mode, or the JSON object with `--json`. Check the exit code to tell them apart.
- Client-side failures are printed to **stderr**, exit 2: the missing-environment sentence, a usage error followed by the usage block, `Could not reach Shika at <socket> (<err>). Is the app running, and was this Lead started by it?`, `Control socket failed: ...`, and `Bad control message: ...`.
- The environment check runs before argument parsing, so outside a Lead even a malformed command prints the environment sentence.

Usage block (`USAGE`):

```text
usage: shika [--json] <command>
  help
  tasks
  new --cli <claude|codex|cursor|pi> [--base <branch>] <prompt...>
  status <task>
  wait [<task>...] [--timeout <seconds>]
  read <task> [--lines <n>]
  diff <task> [--stat]
  send <task> [--no-enter] <text...>
  key <task> <key>...   (enter escape up down left right tab space backspace a-z 0-9)
  pr <task> [--title <text>] [--body <text>]
  close <task>
  memory
  remember <text...>
  forget <n>
  report <text...>   (workers only)
```

## Scope and security

The design goal is a local helper that can drive one project's workers and cannot do anything the author could not see and undo.

- **The token is the credential.** `new_token` reads 128 bits from `/dev/urandom` and panics rather than returning something guessable. Each Lead gets its own token, which lives only in the Lead card (`Card::lead`, `LeadState::token`) and in the Lead's environment. There is no token table: the app finds the Lead by comparing the request's token with its cards, so the token dies when the card does (Close, project removal, quit). An unknown token is refused: `This Lead is not running in Shika (unknown or expired token). Start a Lead from Shika.`
- **The token exists before the session does**, so a very early `shika` call can arrive while the Lead card has no session yet. That is refused with `The Lead is still starting.`; retry.
- **The socket directory is the access control.** The control directory is created `0700`, so only the same macOS user can reach the socket. The socket file itself has the default mode. Anyone who is the same user could read the token from the Lead's environment anyway; the token separates Leads and workers from one another, not users from themselves.
- **Workers cannot start workers.** `pty::CONTROL_VARS` (`SHIKA_SOCKET`, `SHIKA_TOKEN`) are removed from every PTY's environment and set again only from the explicit request of `start_lead`, or of a Lead-started worker's agent (`LaunchOptions::control`). Shell tabs and the author's cards therefore have neither.
- **Worker tokens are report-only.** Each worker a Lead starts gets its own token in `SHIKA_TOKEN` on its agent PTY (`Card::worker_token`; not a table, so it dies with the card). The token names the worker, so a worker can report only for itself. `handle_control` routes it to `handle_worker`, which allows `report` and `help` and refuses everything else. A worker still cannot start, read, steer, publish, or close anything, and cannot see another task. What it gains is one bounded write: a 4 KB string, shown to its Lead as the worker's own words and never typed into any terminal.
- **Project scope.** A Lead sees and affects only cards with its own project id. `new` starts workers only in that project.
- **Ownership.** `tasks` and `status` (with the worker's report) work on every task in the project, whoever started it. `read` and `diff` are reads and work on every task in the project. `wait`, `send`, `key`, `pr`, and `close` work only on tasks this Lead started. The author's own cards stay the author's. `memory`, `remember`, and `forget` act on the Lead's own project. `send` and `key` write only to a task's agent terminal, never to a shell tab, and never to a draft the author left.
- **The 4-worker limit** (`MAX_WORKERS`). A card counts when it was started by this Lead and is not the Lead itself, not marked for discard, has no launch error, and its CLI has not exited (`Card::is_live_worker_of`). It counts while it is still being created (no session yet), while working, and after it finishes until the author closes it or its CLI exits. The check runs on the app thread twice: before the approval check and again immediately before the card is added, so parallel `new` calls cannot overshoot. Ask the author to close finished tasks to free slots.
- **Publishing is the one write the Lead does itself.** `pr` publishes a worker this Lead started without a dialog, because the author decided that asking the Lead is their confirmation (see [`shika pr`](#shika-pr-task---title-text---body-text)). It can stage, commit, push the task branch without force, and create or reuse a PR, to the task's recorded base only, with every recheck of Create PR. It cannot choose another target, merge, force-push, push anything but the task branch, edit an existing PR, or publish the author's cards. The guide limits it to when the author asks. `close` only opens the existing Close flow and waits for the author, who alone confirms; nothing in this protocol bypasses that, and close without a question happens only where the author's own Close would not ask.
- **Resource limits.** One request line at most 1 MiB (`MAX_REQUEST_BYTES`). At most 32 simultaneous connections (`MAX_CONNECTIONS`); the next gets an `error` reply `Shika is serving too many commands at once.` The server gives a client 5 seconds to send its request and 10 seconds to accept the reply.
- **Lifetime and leftovers.** The control directory is removed when the server is dropped: at an orderly quit (`cx.on_app_quit` clears `Shika::control`), and when the Server value drops. A crash or `kill -9` leaves `shika-control-<pid>-<nonce>/` in the temp directory, with a dead socket. The `<data dir>/bin/shika` link stays after any exit and is replaced at the next start. It is harmless and can be deleted by hand once that pid is gone; macOS also clears the temp directory eventually. Leads do not survive a relaunch, so a stale socket is never reused. Each launch makes a new directory. The Lead's own `shika-lead-<id>` worktree is journaled and appears under leftover worktrees after a crash.

## wait in detail

`wait` is built from the pure function `crates/shika/src/control.rs::resolve`, called by `Shika::resolve_waiters` from the app tick (every 100 ms) and once more immediately when a `wait` arrives. There is no polling thread and no cost while waiting.

**Which tasks.** With no ids: every card in the Lead's project that this Lead started and that is not marked for discard (`Shika::wait_set`), including finished ones the author has not closed. With ids: those cards only. Each named id must exist in the project and have been started by this Lead, or the whole command is refused before it waits. A card with no session yet is not in the set.

**Which statuses.**

- Settled: `ready`, `asking`, `exited`. These are reported.
- Busy: `starting`, `working`. These block.
- `waiting` (the CLI is idle and no turn is in progress) is neither. It does not wake a `wait` and does not make one block.

**The decision, in order** (`resolve`):

1. If any task in the set has settled and was not already reported, reply now with all of those tasks as `events`. `still_working` lists the busy ones. `timed_out` is false.
2. Else, if no task is busy, reply now with no events: `nothing to wait for`, `timed_out` false.
3. Else, if the deadline has passed, reply with no events, `timed_out` true, and the busy tasks in `still_working`.
4. Else keep waiting.

Because of step 2 a `wait` returns immediately when everything is `waiting` or already reported. The Lead should treat `nothing to wait for` as "nothing more will finish on its own", and then report to the author.

**Report once.** Each Lead has a ledger (`Ledger`) of the last settlement reported per task, keyed by `(status, turn, report receipt)`, where turn is the card's current/pending turn epoch. A late or changed report is a new event even for the same status and turn; an identical retry is not. A settlement is reported at most once. A task that works again and settles again has a new turn, so it is reported again. A task that goes `asking` and then `ready` within one turn is reported twice, because the status differs. Two concurrent `wait` commands share the ledger: the first resolved reports the event; the second keeps waiting. The ledger lives and dies with the Lead's card.

**Delivery before marking.** The ledger is updated only after the connection thread confirms it wrote the reply. The app thread hands the reply over and keeps its settlements as `Unconfirmed`; the connection thread sends `true` on the request's `delivered` channel after a successful `write_reply`, or `false` after a failed one, and the tick applies the marks (`apply_confirmations`). A failed write, or a connection thread that is gone, drops the marks, so the next `wait` returns the events again. The trade-off is a possible duplicate in place of a loss: a second `wait` that resolves before the confirmation lands (a tick or so), or a client that received the reply and died before acting on it, sees the same event again. A repeated event is harmless; a lost one is not.

**Client disconnect.** While it waits, the connection thread wakes every 500 ms and checks whether the client closed the socket (`peer_closed`, an EOF read). If so it drops its reply and delivery channels and exits without writing. The next tick prunes the waiter using that disconnected delivery channel, so it cannot block the doorbell until its original deadline. Nothing is marked. A client that leaves after the reply was handed over fails the write, the connection thread reports `false`, and the events stay unreported (see Delivery before marking).

**Timeout.** The client sends `timeout_secs` as given. The server waits `min(timeout_secs, 600)` seconds. The client's own read timeout is `timeout_secs + 30` seconds (60 for every other command, none for `new`, `pr`, and `close`). `--timeout 0` answers on the first evaluation: events if any, else `nothing to wait for`, else `timed out`. The default 100 seconds is chosen to stay under the default command timeout of the supported CLIs; a Lead that raises it above its CLI's tool timeout is killed by that CLI, not by Shika.

**The just-started race.** `new` replies when the card has its session. The app has by then recorded the launch prompt as a submitted line, but the tick has not necessarily consumed it. `Card::control_status` reports `working` in that gap (`host.submission != self.submitted`), so a `wait` right after `new` blocks instead of answering `nothing to wait for`. The tick consumes the submission within 100 ms and the card's real status takes over. While the CLI boots, the turn is marked so that its banner and idle editor do not look like a finished turn: [agent-activity.md](agent-activity.md#launch-prompts-and-turns).

## The doorbell

Claude Code Leads receive the same private line through their chat transport. A successful pipe write marks the batch rung; visible keyed Shika event rows replace the terminal paste, with no author bubble. Draft/typing/wait/ownership gates and delivery accounting are unchanged. Other Leads keep the terminal path described below. No command or protocol change is required; see [Lead chat](lead-chat.md).

A Lead that ends its turn cannot notice workers finishing. Shika rings for it: it submits one line into the Lead's terminal, as if the author typed it.

```text
[shika] Workers changed: 18f3a9c2b7e0aa12 "Add slugify to textkit..." is ready; 18f3a9c2b7d4e601 "Add word_count" is asking. Run shika wait.
```

The Lead's guide says a `[shika]` line comes from Shika, not the author, and to run `shika wait` on it. The doorbell is on for every Lead; there is no setting.

**What rings.** A worker this Lead started has settled (`ready`, `asking`, or `exited`), the settlement has not been reported by a `wait` (the `Ledger`), and it has not been rung before. The key is the same as `wait`'s: (task, status, turn, report receipt). A task that asks and then becomes ready in one turn rings twice; a task that works again rings again. A late or changed report on a settled task rings again; identical report retries do not. Titles are cut to 28 characters; the line names each changed task by its full id.

**When it rings (`Gate`, all must hold).**

| Condition | Why |
| --- | --- |
| No `wait` is pending, and none has a reply still being delivered | A wait resolves the settlements itself. |
| The Lead card is Ready or Waiting, with no unprocessed submission | Never into a running turn or a dialog. |
| The author has no unsent typed text in the Lead's terminal (`HostState::has_draft`) | Never appended to the author's draft. A CLI's grey suggestion is not typed. |
| The author has not typed into the Lead's terminal for 3 seconds | Do not race a person who is about to type. |
| The Lead's PTY is alive | Nothing to type into. |

The first unrung settlement starts a 1.5 second batch window (`BELL_BATCH`), so workers that finish together become one line. If a condition fails, the bell waits and rings when it clears. The check is `Shika::ring_doorbells`, run from the app tick with the pure `Doorbell::due`; there is no thread. A settlement is marked rung when the line is sent, and un-marked (`Doorbell::forget`) if the write fails, so it can ring again.

**Delivery.** `Shika::type_into` writes the line as a bracketed paste, then `\r` as a separate write 150 ms later (`ENTER_DELAY`): some TUIs, notably Codex's paste-burst handling, read an Enter that arrives inside a paste burst as a newline and never submit. The writes do not stamp `last_typed`, so the Lead's own bell never reads as the author typing. Both go through `HostState::capture_typed`, the capture typed input uses, so the line is a submission: the Lead card shows Working and its timer starts. If the author types between the two writes, the Enter is dropped and the bell forgets the settlement.

A bell does not mark the settlement reported. The Lead's next `shika wait` returns it, and that is what marks it.

## Protocol

**Transport.** A Unix stream socket at `$SHIKA_SOCKET`. One connection carries exactly one request line and one reply line, both UTF-8 JSON terminated by `\n`. The server closes after replying. There is no streaming, no pipelining, and no persistent session. A trailing newline is optional on the request (EOF ends the line).

**Request.**

```json
{"version":4,"token":"0123456789abcdef0123456789abcdef","command":{"type":"new","cli":"codex","base":"dev","prompt":"Fix it"}}
```

`command` is internally tagged by `type` (snake_case): `{"type":"help"}`, `{"type":"tasks"}`, `{"type":"new","cli":"...","base":null|"...","prompt":"..."}`, `{"type":"status","task":"..."}`, `{"type":"wait","tasks":[...],"timeout_secs":30}`, `{"type":"read","task":"...","lines":0}`, `{"type":"diff","task":"...","stat":false}`, `{"type":"send","task":"...","text":"...","enter":true}`, `{"type":"key","task":"...","keys":["down","enter"]}`, `{"type":"pr","task":"...","title":null|"...","body":null|"..."}` (`title` and `body` may also be left out), `{"type":"close","task":"..."}`, `{"type":"memory"}`, `{"type":"remember","text":"..."}`, `{"type":"forget","note":2}`, `{"type":"report","text":"..."}`.

**Reply.** One of the objects below, tagged by `type`: `help` (`text`), `tasks` (`tasks`), `started` (`task`), `status` (`task`), `waited` (`events`, `still_working`, `timed_out`), `text` (`text`; `read` and `diff`), `done` (`message`; `send`, `key`, `pr`, `close`, `remember`, and `forget`), `memory` (`notes`: `text`, `created`), `refused` (`reason`), `error` (`message`). Examples are under [Commands](#commands). `TaskInfo` fields: `id`, `title`, `cli`, `status`, `elapsed_secs`, `branch`, `diff_stat`, `pr`, `started_by_lead`, `path`, `report`.

**Versioning.** `PROTOCOL_VERSION` is currently `6` (2 added `read`, `diff`, `send`, `key`, the `text` and `done` replies, and `TaskInfo::path`; 3 added `pr` and `close`; 4 added `report`, worker tokens, and `TaskInfo::report`; 5 added `memory`, `remember`, `forget`, and the `memory` reply; 6 made `pr` publish without a dialog and gave it optional `title` and `body`). The server compares it for equality before looking at the command, and a mismatch is a refusal rather than a parse failure:

```json
{"type":"refused","reason":"This shika speaks protocol 9 but the running Shika app speaks 5. Restart the Lead from the app."}
```

Both sides are normally the same binary (the symlink), so a mismatch happens when the app is replaced while a Lead is running.

**Errors.** A line that is not valid JSON for `Request`, or is empty, gets `{"type":"error","message":"Bad control message: ..."}`. A line over `MAX_REQUEST_BYTES` gets `The request is larger than 1048576 bytes.` Both are exit 2 from the client. Because deserialization fails before the version check, a request with an unknown command `type` also arrives as `Bad control message`, not as a version refusal. This is why a new command should bump the version (see below).

**Timeouts.** Client: connect is immediate, writes time out at 10 seconds, reads at 60 seconds (`timeout_secs + 30` for `wait`, no timeout for `new`, `pr`, and `close`). Server: 5 seconds to receive the request, 10 seconds to write the reply.

## Debugging

### See your environment

From inside a Lead terminal:

```sh
echo "$SHIKA_SOCKET"        # /private/var/folders/.../T/shika-control-<pid>-<nonce>/sock
echo "${SHIKA_TOKEN:0:6}..."   # do not paste the token anywhere public
command -v shika            # <data dir>/bin/shika
readlink "$(command -v shika)"
ls -ld "$(dirname "$SHIKA_SOCKET")"   # drwx------
```

`command -v shika` printing anything other than the control `bin/` path means another `shika` is shadowing it. No `SHIKA_SOCKET` means you are not in a Lead terminal (or in a worker or shell tab, which have none).

### Send a raw request

The token is the only secret, so use the shell variable; do not type it into a transcript. Two ways that exist on stock macOS. I checked both against a throwaway server built on the same `read_request` and `write_reply` functions as the app, not against the live app.

With `nc` (BSD netcat, `-U` for Unix sockets):

```sh
printf '{"version":6,"token":"%s","command":{"type":"tasks"}}\n' "$SHIKA_TOKEN" | nc -U "$SHIKA_SOCKET"
```

With `python3`:

```sh
python3 -c 'import json,os,socket
s=socket.socket(socket.AF_UNIX); s.connect(os.environ["SHIKA_SOCKET"])
s.sendall(json.dumps({"version":6,"token":os.environ["SHIKA_TOKEN"],"command":{"type":"tasks"}}).encode()+b"\n")
print(s.makefile().readline().strip())'
```

A `wait` request over `nc` blocks until the reply, up to the server timeout, like the client. To test the version refusal, send `"version":9`. To test the framing, send `garbage` and expect `Bad control message`.

### Common failures

| What you see | Meaning and fix |
| --- | --- |
| `shika commands run inside a Shika Lead or Lead-started worker terminal.` (stderr, exit 2) | `SHIKA_SOCKET` or `SHIKA_TOKEN` is missing. You are not in a Lead or Lead-started worker agent terminal: you are in a shell tab, or a card the author created. |
| `command not found: shika` in the provider's shell tool | The login shell reset PATH. Use the absolute command from the bootstrap/help preamble or worker footer, not a different installation. |
| `refused: Workers can only run shika report.` | A worker ran a Lead command. |
| `refused: Only workers can run shika report.` | A Lead ran `report`. |
| `error: The Lead memory file <path> cannot be read. ...` | `memory.json` does not parse. Fix or delete the file; Shika left it as it was. |
| `refused: The report is empty.` / `The report is N bytes; the most is 4096. ...` | The text was empty after cleaning, or too long. Shorten it. |
| `Could not reach Shika at <path> (No such file or directory / Connection refused). Is the app running, and was this Lead started by it?` | The app quit or crashed, so the socket is gone or dead, or the environment is stale (a shell kept from a previous run). Restart the Lead from the app. |
| `refused: This Lead is not running in Shika (unknown or expired token). ...` | The Lead card was closed or its project removed, or the app restarted. Start a new Lead. |
| `refused: The Lead is still starting.` | The Lead's first command ran before the app registered its session. Retry. |
| `refused: This shika speaks protocol N but the running Shika app speaks M. ...` | App and client differ. Restart the Lead from the app. |
| `error: Bad control message: ...` | The line was not a valid request. Check JSON and field names against [Protocol](#protocol). |
| `error: Shika is serving too many commands at once.` | More than 32 simultaneous connections. Something is looping. |
| `error: Shika stopped handling this command. The Lead may have been closed.` | The reply channel dropped, typically because the Lead card was closed during a `wait` or `new`. |
| `Control socket failed: ...` (stderr) | Read or write timed out or broke. |
| `The Lead needs Shika's control socket: Could not set up the control directory: ...` (toast in the app when starting a Lead) | The app could not create the control directory, install the command under the data directory, or bind the socket at startup (socket path over the 103-byte limit, unwritable temp or data directory). The app runs without a control server and no Lead can start until it is relaunched. |
| `refused: The task's starting base <base> is not a branch on GitHub, so there is no PR target. ...` | `shika pr` found no target: the task started from a branch that is not on origin's GitHub repository, or it was deleted there. Only the author can choose another target, in Create PR. Check `Session::base_ref` (`worktrees.json`, `baseRef`) and `gh api repos/OWNER/REPO/branches`. |
| `refused: The Lead is publishing this task. ...` | A `shika pr` on that task has not replied yet. Wait for it; the author sees the same message on Create PR, Close, and Switch agent for that card. |
| `error: Could not publish PR: ... Completed commits and pushes were kept.` | A git or gh step of `shika pr` failed. The text is the first stderr line. See [publishing.md](publishing.md#debugging-playbook); never reset or force-push to retry. |
| `Unknown command <word>.` plus the usage (stderr, exit 2) | The first word is not a command. Typically a typo. Not a protocol error. |
| `Missing command.` plus the usage (stderr, exit 2) | A bare `shika` in a Lead terminal. With `SHIKA_SOCKET` or `SHIKA_TOKEN` set the binary never starts the app, so this and any mistyped or app-flag invocation are usage errors. Outside a Lead terminal a bare `shika` is the app itself, as for a Finder launch. |

### A finished worker did not reach the Lead

Ask the Lead to run `status` or `tasks` using the absolute command from its help preamble. A present `report` means Shika received it; a missing report is not proof that the worker tried to send one. Use `read <task> --lines 60` only for the affected worker: a reporting command error identifies a send failure, while no reporting attempt suggests the worker ignored its footer. Ask that finished worker to report with the absolute command and check for `reported`.

If the report is present but the Lead did not act, check whether it already consumed the event in `wait`, is still Working/Asking, has an author draft, or has a live pending wait. Those gates intentionally prevent terminal injection. A disconnected wait now releases its gate on the next tick; late reports create fresh events. Screen-based activity can still misclassify unfamiliar provider chrome. A stuck status requires a sanitized live-screen fixture, not a longer quiet timeout or typing into a busy Lead.

### Isolated Codex report check

Run `python3 scripts/check-lead-reports.py /absolute/path/to/shika [codex]` to reproduce the login-shell PATH failure and verify three concurrent absolute-path reports through the real client. It uses a local synthetic Responses server and disposable HOME/CODEX_HOME/repository, with no paid model request or normal Shika app data. It does not exercise the GUI or a real Lead's reasoning.

### See what the server did

The server writes no log. To see its state:

- `shika tasks --json` (or `status`) from the Lead, and the cards in the app itself.
- `ls -d "$TMPDIR"/shika-control-*` shows live and abandoned control directories; the pid in the name tells which app owns one. `lsof -U` can show which process listens on a `sock`; I have not tested that.
- Run the app with `cargo run -p shika -- --data-dir /absolute/test/data` and a disposable repository (see [AGENTS.md](../AGENTS.md#toolchain)). Launching with `open` is needed to test CLI discovery and session titles, because `cargo run` from inside Claude Code passes `CLAUDE_CODE_CHILD_SESSION` to PTYs.
- Unit tests: `cargo test -p shika-core control` for parsing (including `pr --title/--body`), rendering, framing, the control directory, and a socket round trip. `cargo test -p shika-core publish::tests::publish_now` for the direct publish against a disposable repository, a local bare remote, and a fake `gh`. `cargo test -p shika control::` for `resolve`/`Ledger`, the doorbell (`Doorbell`, `Gate`, `doorbell_line`), the send and key guard (`input_refusal`), the `pr` gate and reply mapping (`pr_gate_refusal`, `pr_reply`), key and paste encoding, a real socket with a stand-in UI thread, a client that left, and launch-failure classification. `cargo test -p shika-core diff::` covers `render_unified`. The core-side Lead tests are `cargo test -p shika-core lead`.

## Adding a command

Follow this order. The compiler enforces some of it (the `match` on `Command` in `handle_control` is exhaustive); the rest is by hand.

1. **Decide the contract first** in [lead-agent.md](lead-agent.md): what it does, what it refuses, and which tasks it may touch (own workers only, or the whole project). A command that writes needs an ownership check. A command that closes must open the existing author-confirmed flow and never skip it. `pr` is the one command the author chose to let publish without a dialog; it reuses the publish pipeline and every recheck. Do not extend that exception to another command, another target, or the author's cards without the author's decision.
2. **`crates/shika-core/src/control.rs`**:
   - Add the `Command` variant, with `///` docs on its fields.
   - Add its word to `is_command` (the client is chosen by this list, and the list is not derived from the enum).
   - Add the parsing arm to `parse_args` (or a helper like `parse_new`), and a line to `USAGE`.
   - If the reply needs a new shape, add a `Reply` variant and its `render_text` arm. Keep text compact, one line per task, since a model reads it. `exit_code` needs a change only if the new reply is not a success.
   - Tests: extend `requests_and_replies_round_trip_as_json`, `arguments_parse_into_commands`, `bad_arguments_return_a_short_usage`, `text_is_one_compact_line_per_task`, and `command_words_decide_client_mode`.
3. **`crates/shika/src/control.rs`**, on the app thread in `Shika::handle_control`:
   - Add the match arm. Scope by project (`project_tasks`) and check ownership (`started_by`) the way `wait_refusal` does. Answer expected declines with `refused(..)` and real failures with `Reply::Error`.
   - Anything that blocks (git, files, approval) runs in `cx.spawn_in` on the background executor and answers when done, as `control_new` does. Never block the tick.
   - Only append cards from this path. Removal goes through the tick (`Card::discard`), because open dialogs hold card positions (`Overlay::Close { index }`).
   - If the command takes selection, focus, or opens a dialog, follow the author-only rules in `begin_launch`: a worker launch deliberately touches none of them.
   - Per-session core methods refuse a Lead; the command operates on workers, so check that the core method you call accepts them.
4. **`crates/shika/src/lead_guide.md`**: add the command to the list the Lead reads, with the refusal cases that matter. This is what the model sees via `shika help`; keep it short and keep it true.
5. **Tests**: pure logic (anything like `resolve`) as a unit test; the socket path through the existing test harness (`answer_with` in `control_tests.rs`).
6. **Docs**: this file (command section, refusals, and any error table row), the implementation map and rollout in [lead-agent.md](lead-agent.md), [crates/shika-core/README.md](../crates/shika-core/README.md) if the module map changes, a line in [AGENTS.md](../AGENTS.md) if behavior changes, and a manual check in [MANUAL_CHECKS.md](../MANUAL_CHECKS.md#lead-agent-spike).
7. **`PROTOCOL_VERSION`**. The code requires equality and says to bump when a request or reply changes shape. Recommended policy: bump whenever an existing variant or field changes, and also when a variant is added. An old app would otherwise answer the new client with `Bad control message: unknown variant`, which does not tell the Lead what to do, whereas a mismatch says `Restart the Lead from the app.` The cost of a bump is only that Leads started before an app update must be restarted.

Guardrails (from [lead-agent.md](lead-agent.md)):

- Do not let the Lead close dirty work, merge, force anything, or push outside `shika pr` without the author's confirmation in the app. Keep `shika pr` on the task's recorded base, on workers this Lead started, through `session_publish_now` with every recheck; never add a target, merge, or skip-recheck option.
- Do not edit any CLI's global or project configuration to teach it `shika`. The command reaches the Lead through `PATH` plus its absolute executable in the first prompt and help preamble.
- Do not add a model API call. Shika launches the author's CLI and nothing else.
- Keep the client free of GPUI, settings, and app data (`control_client::run` runs before any of them).
- Keep the socket directory `0700` and the token out of logs, notifications, window titles, and files.
- Never give workers a Lead token. Their own report-only token and socket belong only to the agent PTY, never shell tabs.

## Code map

| Piece | Where |
| --- | --- |
| Request, reply, task types; `parse_args`; `render_text`; `send`, `read_request`, `write_reply`; `ControlDir`; `new_token`; constants | `crates/shika-core/src/control.rs` |
| Client entry: environment, parse, round trip, print, exit status | `crates/shika/src/control_client.rs` (`run`), called at the top of `main` in `crates/shika/src/main.rs` |
| Server: `Server`, `accept_loop`, `serve`, `peer_closed`, `Incoming` | `crates/shika/src/control.rs` |
| Command handling: `Shika::drain_control`, `handle_control`, `control_new`, `control_diff`, `control_input`, `new_refusal`, `wait_refusal`, `wait_set`, `resolve_waiters`; `resolve`, `Ledger`, `Observed`; `Card::control_status`, `task_info`, `is_live_worker_of`; `failure_reply` | `crates/shika/src/control.rs` |
| `pr`: `Shika::control_pr`, `pr_refusal`, `worker_refusal`; `pr_gate_refusal`, `pr_status_refusal`, `pr_reply`, `publish_failure`, `LEAD_PUBLISHING`; `Card::lead_publishing` and its checks in `publish_blocker`, `switch_blocker`, `close`, `input_refusal`; core `Core::session_publish_now`, `publish::publish_now`, `Error::PublishNoTarget`, `MAX_PR_BODY_CHARS`; parsing `parse_pr` | `crates/shika/src/control.rs`; `crates/shika/src/main.rs`, `publish.rs`, `switch_agent.rs`, `close.rs`; `crates/shika-core/src/lib.rs`, `publish.rs`, `error.rs`, `control.rs` |
| `close`: `Shika::control_close`, `worker_refusal`, `settle_lead_dialog`, `lead_dialog_succeeded`, `lead_dialog_failed`; `LeadDialog`, `dialog_gate_refusal`, `dialog_reply`, `close_outcome`; `Overlay::name` | `crates/shika/src/control.rs`; hooks in `close`, `finish_close` in `crates/shika/src/close.rs` and `cancel_overlay` in `overlay.rs` |
| Doorbell and typing: `Doorbell`, `Gate`, `doorbell_line`, `Shika::ring_doorbells`, `Shika::type_into`, `Step`, `paste_steps`, `key_steps`, `key_bytes`, `input_refusal`, `HostState::inject` | `crates/shika/src/control.rs`; `HostState::capture_typed`, `has_draft` in `crates/shika/src/main.rs` |
| Terminal text with scrollback | `Terminal::text_with_history` in `crates/shika-terminal/src/terminal.rs` |
| Diff text | `shika_core::render_unified`, `RENDER_CAP_BYTES` in `crates/shika-core/src/diff.rs` |
| Worker tokens and reports: `worker_command`, `Shika::handle_worker`, `WorkerReport`, `worker_prompt`, `WORKER_FOOTER`; `clean_report`, `WORKER_HELP`, `MAX_REPORT_BYTES` (core) | `crates/shika/src/control.rs`; `Card::worker_token`, `Card::report` in `crates/shika/src/main.rs`; `WorkerEnv` and `LaunchOptions::control` in `crates/shika-core/src/session.rs` |
| Lead card state | `Card::lead` (`LeadState`: token, ledger, waiters), `Card::started_by`, `Card::launch` (`Launch::{Task, Worker, Lead}`) in `crates/shika/src/main.rs` |
| Lead session, fixed worktree, PTY environment, `LeadLaunch`, `LaunchOptions` | `Core::start_lead`, `Core::create_session_with_preparation`, `pty::CONTROL_VARS`, `crates/shika-core/src/session.rs` |
| Guide text | `crates/shika/src/lead_guide.md` |
| Lead memory commands: `MemoryNote`, `parse_text`, `parse_forget` (core `control.rs`); the three arms in `Shika::handle_control`; storage in `crates/shika-core/src/memory.rs` | `crates/shika-core/src/control.rs`, `crates/shika/src/control.rs`, `crates/shika-core/src/memory.rs` |
