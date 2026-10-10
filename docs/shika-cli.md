# The `shika` command

Reference for the command a Lead agent runs: what each subcommand does, the wire protocol under it, the security model, how to debug it, and how to add a command. The product decision and design rationale are in [lead-agent.md](lead-agent.md); read that first for why it exists. The shared protocol code is documented in [crates/shika-core/README.md](../crates/shika-core/README.md).

Contents: [What it is](#what-it-is) - [Commands](#commands) - [Output and exit codes](#output-and-exit-codes) - [Scope and security](#scope-and-security) - [wait in detail](#wait-in-detail) - [The doorbell](#the-doorbell) - [Protocol](#protocol) - [Debugging](#debugging) - [Adding a command](#adding-a-command) - [Code map](#code-map)

Status: `help`, `tasks`, `new`, `status`, `wait`, `read`, `diff`, `send`, and `key` are implemented, plus the doorbell that wakes an idle Lead. `pr` and `close` are described in lead-agent.md as the contract for a later phase and do not exist. Typing one in a Lead terminal is a usage error: `Unknown command pr.` plus the usage text on stderr, exit 2.

## What it is

`shika` is a small client for a local Unix socket that the running Shika app serves. A Lead (an ordinary agent CLI in its own terminal) runs it as a shell command to list tasks, start worker cards, wait for them, read their terminals and diffs, and answer their questions. Every supported CLI can run a shell command, so no per-CLI configuration is injected and no MCP server exists.

**There is no separate binary.** The app binary is the client. At the top of `main`, before GPUI, settings, or app data are touched, it checks `shika_core::control::is_client_invocation` on its arguments and on whether `SHIKA_SOCKET` or `SHIKA_TOKEN` is set. Inside a Lead terminal (either variable set) it is always the client, whatever the arguments, so a bare or mistyped `shika` prints the usage and exits 2 and never starts a second app on the real data. Elsewhere it is the client only for a command word (`help`, `tasks`, `new`, `status`, `wait`, `read`, `diff`, `send`, `key`), optionally after a leading `--json`; `--data-dir`, `--diagnostics-file`, and Finder or `open` launches start the app. When it is the client, `control_client::run` talks to the socket and the process exits with its status.

**Where it works.** Only in a Lead's terminal tree. Two environment variables make it work, and only a Lead PTY has them:

- `SHIKA_SOCKET`: absolute path of the socket.
- `SHIKA_TOKEN`: 32 hex characters, the Lead's credential.

Run anywhere else, it prints `shika commands run inside a Shika Lead terminal.` to stderr and exits 2. Workers and shell tabs have both variables removed, so they get the same message.

**How `shika` gets on the Lead's `PATH`.** At startup the app creates a per-run control directory (`shika_core::control::ControlDir`):

```text
<tmp>/shika-control-<pid>-<nonce>/   mode 0700
  sock                               the listening socket
  bin/shika                          symlink to the running executable
```

`<tmp>` is the canonicalized temp directory (on macOS, under `/private/var/folders/...`). The Lead's `PATH` is `<that>/bin:` followed by the login-shell `PATH`. Consequences:

- `shika` is always the same build as the app that started the Lead, whether it came from `cargo run` or a `.app` bundle.
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

## Output and exit codes

| Exit | Meaning |
| --- | --- |
| 0 | The app answered with `help`, `tasks`, `started`, `status`, `waited`, `text`, or `done`. A `wait` that timed out is still 0. |
| 1 | The app answered `refused`: understood and declined, with a reason. |
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
```

## Scope and security

The design goal is a local helper that can drive one project's workers and cannot do anything the author could not see and undo.

- **The token is the credential.** `new_token` reads 128 bits from `/dev/urandom` and panics rather than returning something guessable. Each Lead gets its own token, which lives only in the Lead card (`Card::lead`, `LeadState::token`) and in the Lead's environment. There is no token table: the app finds the Lead by comparing the request's token with its cards, so the token dies when the card does (Close, project removal, quit). An unknown token is refused: `This Lead is not running in Shika (unknown or expired token). Start a Lead from Shika.`
- **The token exists before the session does**, so a very early `shika` call can arrive while the Lead card has no session yet. That is refused with `The Lead is still starting.`; retry.
- **The socket directory is the access control.** The control directory is created `0700`, so only the same macOS user can reach the socket. The socket file itself has the default mode. Anyone who is the same user could read the token from the Lead's environment anyway; the token separates Leads and workers from one another, not users from themselves.
- **Workers cannot start workers.** `pty::CONTROL_VARS` (`SHIKA_SOCKET`, `SHIKA_TOKEN`) are removed from every PTY's environment and set again only from the explicit request of `create_lead`. Workers and shell tabs therefore have neither, and the tree is one level deep.
- **Project scope.** A Lead sees and affects only cards with its own project id. `new` starts workers only in that project.
- **Ownership.** `tasks` and `status` work on every task in the project, whoever started it. `read` and `diff` are reads and work on every task in the project. `wait`, `send`, and `key` work only on tasks this Lead started. The author's own cards stay the author's. Future `pr` and `close` are specified the same way. `send` and `key` write only to a task's agent terminal, never to a shell tab, and never to a draft the author left.
- **The 4-worker limit** (`MAX_WORKERS`). A card counts when it was started by this Lead and is not the Lead itself, not marked for discard, has no launch error, and its CLI has not exited (`Card::is_live_worker_of`). It counts while it is still being created (no session yet), while working, and after it finishes until the author closes it or its CLI exits. The check runs on the app thread twice: before the approval check and again immediately before the card is added, so parallel `new` calls cannot overshoot. Ask the author to close finished tasks to free slots.
- **No authority beyond starting and watching.** The Lead cannot merge, push, commit, or publish through Shika. Nothing in this protocol can bypass the author's confirmation for Create PR or Close; if `pr` and `close` are ever added, they must open the existing dialogs and wait for the author.
- **Resource limits.** One request line at most 1 MiB (`MAX_REQUEST_BYTES`). At most 32 simultaneous connections (`MAX_CONNECTIONS`); the next gets an `error` reply `Shika is serving too many commands at once.` The server gives a client 5 seconds to send its request and 10 seconds to accept the reply.
- **Lifetime and leftovers.** The control directory is removed when the server is dropped: at an orderly quit (`cx.on_app_quit` clears `Shika::control`), and when the Server value drops. A crash or `kill -9` leaves `shika-control-<pid>-<nonce>/` in the temp directory, with a dead socket and the symlink. It is harmless and can be deleted by hand once that pid is gone; macOS also clears the temp directory eventually. Leads do not survive a relaunch, so a stale socket is never reused. Each launch makes a new directory. The Lead's own `shika-lead-<id>` worktree is journaled and appears under leftover worktrees after a crash.

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

**Report once.** Each Lead has a ledger (`Ledger`) of the last settlement reported per task, keyed by `(status, turn)`, where turn is the card's `Activity::turn_started`. A settlement is reported at most once. A task that works again and settles again has a new turn, so it is reported again. A task that goes `asking` and then `ready` within one turn is reported twice, because the status differs. Two concurrent `wait` commands share the ledger: the first resolved reports the event; the second keeps waiting. The ledger lives and dies with the Lead's card.

**Delivery before marking.** The ledger is updated only after the connection thread confirms it wrote the reply. The app thread hands the reply over and keeps its settlements as `Unconfirmed`; the connection thread sends `true` on the request's `delivered` channel after a successful `write_reply`, or `false` after a failed one, and the tick applies the marks (`apply_confirmations`). A failed write, or a connection thread that is gone, drops the marks, so the next `wait` returns the events again. The trade-off is a possible duplicate in place of a loss: a second `wait` that resolves before the confirmation lands (a tick or so), or a client that received the reply and died before acting on it, sees the same event again. A repeated event is harmless; a lost one is not.

**Client disconnect.** While it waits, the connection thread wakes every 500 ms and checks whether the client closed the socket (`peer_closed`, an EOF read). If so it drops its reply channel and exits without writing; the waiter's later send fails and nothing is marked. A client that leaves after the reply was handed over fails the write, the connection thread reports `false`, and the events stay unreported (see Delivery before marking).

**Timeout.** The client sends `timeout_secs` as given. The server waits `min(timeout_secs, 600)` seconds. The client's own read timeout is `timeout_secs + 30` seconds (60 for every other command, none for `new`). `--timeout 0` answers on the first evaluation: events if any, else `nothing to wait for`, else `timed out`. The default 100 seconds is chosen to stay under the default command timeout of the supported CLIs; a Lead that raises it above its CLI's tool timeout is killed by that CLI, not by Shika.

**The just-started race.** `new` replies when the card has its session. The app has by then recorded the launch prompt as a submitted line, but the tick has not necessarily consumed it. `Card::control_status` reports `working` in that gap (`host.submission != self.submitted`), so a `wait` right after `new` blocks instead of answering `nothing to wait for`. The tick consumes the submission within 100 ms and the card's real status takes over. While the CLI boots, the turn is marked so that its banner and idle editor do not look like a finished turn: [agent-activity.md](agent-activity.md#launch-prompts-and-turns).

## The doorbell

A Lead that ends its turn cannot notice workers finishing. Shika rings for it: it submits one line into the Lead's terminal, as if the author typed it.

```text
[shika] Workers changed: 18f3a9c2b7e0aa12 "Add slugify to textkit..." is ready; 18f3a9c2b7d4e601 "Add word_count" is asking. Run shika wait.
```

The Lead's guide says a `[shika]` line comes from Shika, not the author, and to run `shika wait` on it. The doorbell is on for every Lead; there is no setting.

**What rings.** A worker this Lead started has settled (`ready`, `asking`, or `exited`), the settlement has not been reported by a `wait` (the `Ledger`), and it has not been rung before. The key is the same as `wait`'s: (task, status, turn). A task that asks and then becomes ready in one turn rings twice; a task that works again rings again. Titles are cut to 28 characters; the line names each changed task by its full id.

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
{"version":2,"token":"0123456789abcdef0123456789abcdef","command":{"type":"new","cli":"codex","base":"dev","prompt":"Fix it"}}
```

`command` is internally tagged by `type` (snake_case): `{"type":"help"}`, `{"type":"tasks"}`, `{"type":"new","cli":"...","base":null|"...","prompt":"..."}`, `{"type":"status","task":"..."}`, `{"type":"wait","tasks":[...],"timeout_secs":30}`, `{"type":"read","task":"...","lines":0}`, `{"type":"diff","task":"...","stat":false}`, `{"type":"send","task":"...","text":"...","enter":true}`, `{"type":"key","task":"...","keys":["down","enter"]}`.

**Reply.** One of the objects below, tagged by `type`: `help` (`text`), `tasks` (`tasks`), `started` (`task`), `status` (`task`), `waited` (`events`, `still_working`, `timed_out`), `text` (`text`; `read` and `diff`), `done` (`message`; `send` and `key`), `refused` (`reason`), `error` (`message`). Examples are under [Commands](#commands). `TaskInfo` fields: `id`, `title`, `cli`, `status`, `elapsed_secs`, `branch`, `diff_stat`, `pr`, `started_by_lead`, `path`.

**Versioning.** `PROTOCOL_VERSION` is currently `2` (2 added `read`, `diff`, `send`, `key`, the `text` and `done` replies, and `TaskInfo::path`). The server compares it for equality before looking at the command, and a mismatch is a refusal rather than a parse failure:

```json
{"type":"refused","reason":"This shika speaks protocol 9 but the running Shika app speaks 2. Restart the Lead from the app."}
```

Both sides are normally the same binary (the symlink), so a mismatch happens when the app is replaced while a Lead is running.

**Errors.** A line that is not valid JSON for `Request`, or is empty, gets `{"type":"error","message":"Bad control message: ..."}`. A line over `MAX_REQUEST_BYTES` gets `The request is larger than 1048576 bytes.` Both are exit 2 from the client. Because deserialization fails before the version check, a request with an unknown command `type` also arrives as `Bad control message`, not as a version refusal. This is why a new command should bump the version (see below).

**Timeouts.** Client: connect is immediate, writes time out at 10 seconds, reads at 60 seconds (`timeout_secs + 30` for `wait`, no timeout for `new`). Server: 5 seconds to receive the request, 10 seconds to write the reply.

## Debugging

### See your environment

From inside a Lead terminal:

```sh
echo "$SHIKA_SOCKET"        # /private/var/folders/.../T/shika-control-<pid>-<nonce>/sock
echo "${SHIKA_TOKEN:0:6}..."   # do not paste the token anywhere public
command -v shika            # <control dir>/bin/shika
readlink "$(command -v shika)"
ls -ld "$(dirname "$SHIKA_SOCKET")"   # drwx------
```

`command -v shika` printing anything other than the control `bin/` path means another `shika` is shadowing it. No `SHIKA_SOCKET` means you are not in a Lead terminal (or in a worker or shell tab, which have none).

### Send a raw request

The token is the only secret, so use the shell variable; do not type it into a transcript. Two ways that exist on stock macOS. I checked both against a throwaway server built on the same `read_request` and `write_reply` functions as the app, not against the live app.

With `nc` (BSD netcat, `-U` for Unix sockets):

```sh
printf '{"version":2,"token":"%s","command":{"type":"tasks"}}\n' "$SHIKA_TOKEN" | nc -U "$SHIKA_SOCKET"
```

With `python3`:

```sh
python3 -c 'import json,os,socket
s=socket.socket(socket.AF_UNIX); s.connect(os.environ["SHIKA_SOCKET"])
s.sendall(json.dumps({"version":2,"token":os.environ["SHIKA_TOKEN"],"command":{"type":"tasks"}}).encode()+b"\n")
print(s.makefile().readline().strip())'
```

A `wait` request over `nc` blocks until the reply, up to the server timeout, like the client. To test the version refusal, send `"version":9`. To test the framing, send `garbage` and expect `Bad control message`.

### Common failures

| What you see | Meaning and fix |
| --- | --- |
| `shika commands run inside a Shika Lead terminal.` (stderr, exit 2) | `SHIKA_SOCKET` or `SHIKA_TOKEN` is missing. You are not in a Lead terminal, or you are in a worker or a shell tab. |
| `Could not reach Shika at <path> (No such file or directory / Connection refused). Is the app running, and was this Lead started by it?` | The app quit or crashed, so the socket is gone or dead, or the environment is stale (a shell kept from a previous run). Restart the Lead from the app. |
| `refused: This Lead is not running in Shika (unknown or expired token). ...` | The Lead card was closed or its project removed, or the app restarted. Start a new Lead. |
| `refused: The Lead is still starting.` | The Lead's first command ran before the app registered its session. Retry. |
| `refused: This shika speaks protocol N but the running Shika app speaks M. ...` | App and client differ. Restart the Lead from the app. |
| `error: Bad control message: ...` | The line was not a valid request. Check JSON and field names against [Protocol](#protocol). |
| `error: Shika is serving too many commands at once.` | More than 32 simultaneous connections. Something is looping. |
| `error: Shika stopped handling this command. The Lead may have been closed.` | The reply channel dropped, typically because the Lead card was closed during a `wait` or `new`. |
| `Control socket failed: ...` (stderr) | Read or write timed out or broke. |
| `The Lead needs Shika's control socket: Could not set up the control directory: ...` (toast in the app when starting a Lead) | The app could not create the control directory or bind the socket at startup (path over the 103-byte limit, unwritable temp directory). The app runs without a control server and no Lead can start until it is relaunched. |
| `Unknown command <word>.` plus the usage (stderr, exit 2) | The first word is not a command. Typical for a planned command (`pr`, `close`) or a typo. Not a protocol error. |
| `Missing command.` plus the usage (stderr, exit 2) | A bare `shika` in a Lead terminal. With `SHIKA_SOCKET` or `SHIKA_TOKEN` set the binary never starts the app, so this and any mistyped or app-flag invocation are usage errors. Outside a Lead terminal a bare `shika` is the app itself, as for a Finder launch. |

### See what the server did

The server writes no log. To see its state:

- `shika tasks --json` (or `status`) from the Lead, and the cards in the app itself.
- `ls -d "$TMPDIR"/shika-control-*` shows live and abandoned control directories; the pid in the name tells which app owns one. `lsof -U` can show which process listens on a `sock`; I have not tested that.
- Run the app with `cargo run -p shika -- --data-dir /absolute/test/data` and a disposable repository (see [AGENTS.md](../AGENTS.md#toolchain)). Launching with `open` is needed to test CLI discovery and session titles, because `cargo run` from inside Claude Code passes `CLAUDE_CODE_CHILD_SESSION` to PTYs.
- Unit tests: `cargo test -p shika-core control` for parsing, rendering, framing, the control directory, and a socket round trip. `cargo test -p shika control::` for `resolve`/`Ledger`, the doorbell (`Doorbell`, `Gate`, `doorbell_line`), the send and key guard (`input_refusal`), key and paste encoding, a real socket with a stand-in UI thread, a client that left, and launch-failure classification. `cargo test -p shika-core diff::` covers `render_unified`. The core-side Lead tests are `cargo test -p shika-core lead`.

## Adding a command

Follow this order. The compiler enforces some of it (the `match` on `Command` in `handle_control` is exhaustive); the rest is by hand.

1. **Decide the contract first** in [lead-agent.md](lead-agent.md): what it does, what it refuses, and which tasks it may touch (own workers only, or the whole project). A command that writes needs an ownership check. A command that publishes or closes must open the existing author-confirmed flow and never skip it.
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
5. **Tests**: pure logic (anything like `resolve`) as a unit test; the socket path through the existing test harness (`answer_with` in `control.rs` tests).
6. **Docs**: this file (command section, refusals, and any error table row), the implementation map and rollout in [lead-agent.md](lead-agent.md), [crates/shika-core/README.md](../crates/shika-core/README.md) if the module map changes, a line in [AGENTS.md](../AGENTS.md) if behavior changes, and a manual check in [MANUAL_CHECKS.md](../MANUAL_CHECKS.md#lead-agent-spike).
7. **`PROTOCOL_VERSION`**. The code requires equality and says to bump when a request or reply changes shape. Recommended policy: bump whenever an existing variant or field changes, and also when a variant is added. An old app would otherwise answer the new client with `Bad control message: unknown variant`, which does not tell the Lead what to do, whereas a mismatch says `Restart the Lead from the app.` The cost of a bump is only that Leads started before an app update must be restarted.

Guardrails (from [lead-agent.md](lead-agent.md)):

- Do not let the Lead publish, close dirty work, merge, push, or force anything without the author's confirmation in the app.
- Do not edit any CLI's global or project configuration to teach it `shika`. The command reaches the Lead through `PATH` and the first prompt only.
- Do not add a model API call. Shika launches the author's CLI and nothing else.
- Keep the client free of GPUI, settings, and app data (`control_client::run` runs before any of them).
- Keep the socket directory `0700` and the token out of logs, notifications, window titles, and files.
- Never give workers the token or socket.

## Code map

| Piece | Where |
| --- | --- |
| Request, reply, task types; `parse_args`; `render_text`; `send`, `read_request`, `write_reply`; `ControlDir`; `new_token`; constants | `crates/shika-core/src/control.rs` |
| Client entry: environment, parse, round trip, print, exit status | `crates/shika/src/control_client.rs` (`run`), called at the top of `main` in `crates/shika/src/main.rs` |
| Server: `Server`, `accept_loop`, `serve`, `peer_closed`, `Incoming` | `crates/shika/src/control.rs` |
| Command handling: `Shika::drain_control`, `handle_control`, `control_new`, `control_diff`, `control_input`, `new_refusal`, `wait_refusal`, `wait_set`, `resolve_waiters`; `resolve`, `Ledger`, `Observed`; `Card::control_status`, `task_info`, `is_live_worker_of`; `failure_reply` | `crates/shika/src/control.rs` |
| Doorbell and typing: `Doorbell`, `Gate`, `doorbell_line`, `Shika::ring_doorbells`, `Shika::type_into`, `Step`, `paste_steps`, `key_steps`, `key_bytes`, `input_refusal`, `HostState::inject` | `crates/shika/src/control.rs`; `HostState::capture_typed`, `has_draft` in `crates/shika/src/main.rs` |
| Terminal text with scrollback | `Terminal::text_with_history` in `crates/shika-terminal/src/terminal.rs` |
| Diff text | `shika_core::render_unified`, `RENDER_CAP_BYTES` in `crates/shika-core/src/diff.rs` |
| Lead card state | `Card::lead` (`LeadState`: token, ledger, waiters), `Card::started_by`, `Card::launch` (`Launch::{Task, Worker, Lead}`) in `crates/shika/src/main.rs` |
| Lead session, detached worktree, PTY environment, `LeadEnv`, `LaunchOptions` | `Core::create_lead`, `Core::create_session_with_preparation`, `pty::CONTROL_VARS`, `crates/shika-core/src/session.rs` |
| Guide text | `crates/shika/src/lead_guide.md` |
