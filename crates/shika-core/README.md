# shika-core

Shika's backend as a library: saved projects, draft worktrees, the login-shell `PATH`, the agent CLI presets, PTY processes, live sessions, diffs, publishing, and the protocol the Lead's `shika` command speaks. The GPUI app (`crates/shika`) drives all of it through one type, `Core`.

Read [AGENTS.md](../../AGENTS.md) for the product rules and [docs/README.md](../../docs/README.md) for the feature guides. This file is the map of the crate itself. The crate's rustdoc (`cargo doc -p shika-core --no-deps --open`) holds the per-method contracts; this file does not repeat them.

## Contents

- [What it is and is not](#what-it-is-and-is-not)
- [Threads, blocking, and the operations lock](#threads-blocking-and-the-operations-lock)
- [Module map](#module-map)
- [Concepts and lifecycles](#concepts-and-lifecycles)
- [The control protocol](#the-control-protocol)
- [Invariants](#invariants)
- [Testing](#testing)
- [Extending the crate](#extending-the-crate)

## What it is and is not

It is:

- The only code that creates or removes worktrees, spawns PTYs, runs git, or reads and writes `projects.json`, `worktrees.json`, and `settings.json`.
- Synchronous. Methods block on git, the login shell, or file IO. There is no async runtime in the crate.
- Free of UI. It never wakes the UI thread. A PTY's output goes to a `PtySink` the caller supplies.

It is not:

- A GPUI crate. Nothing here imports `gpui`. Terminal emulation lives in `shika-terminal`, and core only moves bytes.
- A model client. Shika launches the user's own CLI. There is no API key, no transcript, and no conversation history anywhere in core.
- The owner of UI state. Which card is selected, whether a task is Working or Ready, notifications, and dialogs belong to the app. Core reports git and process facts; the app decides what they mean. The control server therefore lives in `crates/shika`, and only the shared protocol is here.

## Threads, blocking, and the operations lock

`Core` is `Send + Sync` and is shared as `Arc<Core>`. The crate docs in `src/lib.rs` list which methods block. The short version:

- **Blocking** methods run git, the user's login shell, or setup commands, and can take seconds: `add_project`, `project_preparation_draft`, `save_project_preparation`, `save_and_approve_project_preparation`, `create_session`, `create_session_with_preparation`, `create_lead`, `open_shell`, every `session_*` git method, `leftover_remove`, `remove_project`, the base-branch methods, and the first call to `path_env` or `cli_catalog` (the login shell gets up to 20 seconds). Call them from a background executor, never from the GPUI main thread.
- **Quick** methods read a small JSON file, take a short lock, or queue bytes: `open`, `projects`, `settings`, `save_settings`, `worktree_journal`, `leftovers_list`, `sessions`, `session`, `lead_for_project`, `workers_of`, `write`, `resize`.

`Core::operations` is one mutex that serializes everything that changes worktrees, branches, the journal, or the live session list: creating and removing worktrees, close, discard, push, rename, publish, and `remove_project`. Take it for any new method that mutates those. Two kinds of work deliberately stay outside it:

- **Reads** that must not hold up Close: `session_diff_stat`, `session_diff`, `session_file_diff`, `session_pr_checks`, `session_pushed_head`, `session_activity`. They use `GIT_OPTIONAL_LOCKS=0` (see [Invariants](#invariants)).
- **Slow preparation.** Setup commands run outside the lock so Close and Discard stay responsive. The worktree is journaled first, and `Core::preparing` keeps leftovers from offering a tree that has an active writer. In-app configuration saves also validate copy-source metadata/ignore rules outside the lock; only the project/config recheck and atomic write/removal hold it. Settings saves do not grant consent. First-New's explicit `save_and_approve_project_preparation` also records consent for the reread parsed value, but allocates no task or command; normal launch still rechecks it. `skip_preparation_onboarding` refuses an appeared config and stores only the local per-project `worktreeSetupReviewed` preference, never permission.

A poisoned lock is recovered with `unwrap_or_else(|e| e.into_inner())`; keep that pattern.

## Module map

All modules are private except `control`. The crate root re-exports the public surface.

| Module | Responsibility | Main public items |
| --- | --- | --- |
| `lib.rs` | `Core`: opens the data files and ties every module together. Crate-level threading docs. | `Core`, `app_data_dir`, `APP_IDENTIFIER`, `ProjectBase`, `SwitchedBranchClose` |
| `control` | Wire protocol between the `shika` command and the app: request and reply types, JSON-line framing, argument parsing, text rendering, the per-run control directory, token generator. Public module. | `Request`, `Command`, `Reply`, `TaskInfo`, `TaskStatus`, `PROTOCOL_VERSION`, `parse_args`, `render_text`, `send`, `read_request`, `write_reply`, `ControlDir`, `new_token` |
| `session` | The in-memory session list (`SessionStore`), ids, launch argument rules, git facts for the close dialog, manual task names. | `Session`, `LaunchOptions`, `LeadEnv`, `SessionGitState`, `DiffStat`, `task_title` |
| `worktree` | Everything that runs git for a task: creating drafts and Lead worktrees, base resolution, fetch, the journal, dirty and unpushed checks, branch renames, diff stat and diff, removal. | `JournalEntry`, `KnownBranches`, `normalize_prefix` (as `normalize_branch_prefix`); internally `create_draft`, `create_lead`, `resolve_base`, `git_cmd`, `read_only_git`, `GIT_REDIRECTS` |
| `pty` | `portable-pty` wrapper: one reader thread and one writer thread per PTY, environment setup, exit events. | `PtyId`, `PtySize`, `PtyEvent`, `PtyExit`, `PtySink`; internally `PtyHub`, `SpawnRequest`, `CONTROL_VARS` |
| `agents` | The four CLI presets and their launch flags. | `CliPreset`, `CliCatalog` |
| `path_env` | The login-shell `PATH`, captured once, and the absolute CLI paths resolved from it. | `PathEnv`, `LoginShellError` |
| `projects` | `projects.json`: add (git root lookup), remove, base branch, preparation approval and local onboarding decision. | `Project`, `ProjectAdded` |
| `settings` | `settings.json`: theme, appearance, font size, branch prefix, sound, column, Changes width. Missing fields take defaults. | `Settings`, `ThemeSettings`, `ThemeMode`, `Appearance`, `Translucency`, `FontSize`, `Column`, `Changes` |
| `preparation` | Opt-in `.shika/worktrees.json`: parse, Settings/first-New metadata suggestions, validated atomic save/disable, approval comparison, rooted file copy, setup commands in process groups, two-slot limiter, cancel. | `PreparationConfig`, `PreparationDraft`, `PreparationControl`, `PreparationEvent` |
| `activity` | Pi lifecycle bridge: a private directory and extension that report Idle or Working through a small file. No terminal bytes. | `AgentActivity`, `AgentActivityState` |
| `cli_title` | Read-only access to the session title each CLI keeps in its own private files. | internal `CliHome` |
| `diff` | Parses git's unified patch into UI-free types with caps. | `SessionDiff`, `FileDiff`, `Hunk`, `DiffLine`, `FileKey`, `FileStatus`, `Collapse`, `LineKind`, `ModeChange` |
| `publish` | Confirmed commit, push, and `gh pr create`, plus the PR checks read. | `PublishPreview`, `PublishedPr`, `PrChecks`, `ChecksState` |
| `error` | The one `Error` enum. `Display` is the sentence the app shows. | `Error`, `Result` |

`preparation_tests.rs` holds the integration tests for preparation.

## Concepts and lifecycles

### Data files

`Core::open(dir)` creates `dir` and reads three files lazily. The app passes `app_data_dir()`, `~/Library/Application Support/com.hieule.shika`, the directory the earlier Tauri build used. Tests pass a temporary directory and `--data-dir` does the same for the app.

- `projects.json`: `{ id, name, path, baseBranch?, approvedPreparation? }` per project. The id is time-derived; the name is the folder name; a nested folder is stored as its repository root. `approvedPreparation` is the parsed setup config the author consented to, compared by equality.
- `worktrees.json`: the journal, one `{ projectId, branch, path, baseRef? }` per worktree Shika created. It is written before a CLI starts and removed after the tree is gone. It is the only source for leftovers. `branch` is empty for a Lead.
- `settings.json`: see `settings::Settings`. A missing file or field takes its default. An unknown theme id is kept on disk until the user picks again.

Files are written through a temp file and `rename`. Each has its own small mutex.

Sessions, titles, status, and PTY ids are memory only. After a relaunch the journal still lists the worktrees, and `leftovers_list` offers them for explicit cleanup. Quit never deletes a worktree. See [tasks-and-worktrees.md](../../docs/tasks-and-worktrees.md).

### Draft worktrees and the base

`create_session` (and `create_session_with_preparation`) does, in order:

1. Validate the prompt, if any (see [Launch options](#launch-options-and-the-positional-prompt)).
2. Find the preset's binary on the login-shell `PATH`; `CliNotFound` otherwise.
3. Load `.shika/worktrees.json` without running it. If present and not approved for this project, `PreparationNeedsApproval`.
4. Resolve the base. A per-launch `LaunchOptions::base` must exist locally or on origin (fetched when new there) and replaces the project's base for this task only. The project's saved base is never changed.
5. Fetch the base from origin, best effort, 4 seconds at most, deduplicated per project for 30 seconds (`freshen_base`). Failures are ignored.
6. Under the operations lock: resolve the start ref (`worktree::resolve_base`: configured base as `origin/<B>` then local `<B>`, an error if neither exists; unset base is `origin/HEAD`, then `main`, then `master`, then the main checkout's HEAD), append `.worktrees/` to the repository's `info/exclude`, run `git worktree add --no-track -b shika-draft-<id> <repo>/.worktrees/shika-draft-<id> <start>`, and write the journal entry.
7. Run copy and setup commands outside the lock, if configured. Output reaches the sink through `PreparationEvent`.
8. Open the PTY with the preset's flags, plus the prompt as the last argument if given, and register the `Session`.

`--no-track` matters: starting from `origin/dev` would otherwise make `origin/dev` the upstream, and a plain `git push` would target the base. The ref the branch started from is recorded as `Session::base_ref` and in the journal; Close and the diff stat measure against it, so later base changes leave running cards alone.

A failed launch removes the worktree only when it is provably untouched. Changed or unverifiable trees stay journaled and show up as leftovers. Details: [worktree-preparation.md](../../docs/worktree-preparation.md).

### Sessions

A `Session` is one card: id, project, preset, title, branch, repo, worktree, `base_ref`, the agent `pty`, shell PTYs, `cli_titled`, `manual_title`, and the two Lead fields:

- `lead: bool` - this session is its project's Lead.
- `started_by: Option<String>` - the id of the Lead that started this worker. `None` for a card the author created.

Ids come from `session::new_id` (time-derived hex). `Core::taken_ids` feeds it every live id plus every id found in the journal, from both the branch name (`shika-draft-<id>`) and the worktree folder name (`shika-draft-<id>` or `shika-lead-<id>`). It uses the folder, not the branch, because a draft's branch can be renamed. This is what keeps draft and Lead ids from colliding.

The first prompt line names the branch (`session_rename_from_prompt`), and a second later the CLI's own title renames it once (`session_apply_cli_title`, which reads the CLI's private files through `cli_title`). Rules and file formats: [branch-naming.md](../../docs/branch-naming.md). Manual names: [task-names.md](../../docs/task-names.md).

### Launch options and the positional prompt

`LaunchOptions { prompt, started_by, base, control }`, with `Default` meaning the plain New flow.

- `prompt` is passed to the CLI as its single positional argument after the preset's flags. All four CLIs accept one. `session::check_prompt` refuses a prompt that is empty or whitespace, starts with `-` (the CLI would read a flag), or contains a NUL byte, with `Error::InvalidPrompt`, before anything is created. Core does not rename the branch from the prompt; the caller does that, because no typed submission exists for the app to observe.
- `started_by` records the owning Lead's session id. Core stores it and does not enforce limits; the 4-worker limit lives in the app's control server.
- `base` is the per-launch base described above.
- `control: Option<WorkerEnv>` is set only for a worker a Lead starts. `WorkerEnv { socket, token, bin_dir }` gives that worker's **agent PTY** (never its shell tabs) `SHIKA_SOCKET`, its own `SHIKA_TOKEN`, and `bin_dir` first on `PATH`, so it can run `shika report`. The app maps the token to the worker and accepts only `report` and `help` from it. `None`, as for every card the author creates, means no `SHIKA_*` variables.

### Lead sessions

`Core::create_lead(project_id, preset_id, size, sink, LeadEnv)` starts a project's Lead:

- The worktree is `<repo>/.worktrees/shika-lead-<id>`, created with `git worktree add --detach <start>` from the same start ref New uses (fetched first). **No branch is created.** The journal entry has an empty `branch`.
- Preparation never runs for a Lead, and the project's setup approval is not consulted.
- The CLI gets the preset's flags, `LeadEnv::prompt` as its last argument, `PATH` with `LeadEnv::bin_dir` first, and `SHIKA_SOCKET` and `SHIKA_TOKEN`. Every other PTY has those two variables removed (see [PTY environment](#pty-environment)).
- The returned session has `lead: true`, the title `Lead`, and an empty `branch`. A second Lead for a project is `Error::LeadExists`, checked again under the lock.
- If anything after the worktree is created fails, the untouched tree is removed and the journal entry dropped.

Methods and how they treat a Lead (`refuse_lead` returns `Error::LeadUnsupported`):

| Behavior | Methods |
| --- | --- |
| Refuse with `LeadUnsupported` | `open_shell`, `session_rename_from_prompt`, `session_apply_cli_title`, `session_diff`, `session_file_diff`, `session_publish_preview`, `session_publish`, `session_pr_checks`, `session_push_and_close`, `session_switched_close_check` (and so branch-switch close) |
| Return a defined result | `session_git_state`: `dirty` from the tree, everything else false. `session_diff_stat`: `DiffStat::default()`. `session_pushed_head`: `None`. `session_refresh_branch`: the session unchanged. |
| Work, with Lead rules | `session_close`: refuses a dirty tree (`WorktreeHasChanges`), asks when the agent is working (`CloseNeedsConfirmation`), then removes the worktree. `session_discard`: removes the worktree with force. Neither touches any branch. |
| Unchanged | `session_dirty`, `session_set_title`, `cancel_session_start`, `close_shell`, `write`, `resize`, `leftover_remove` (an empty-branch entry removes the tree only) |

Any new per-session method that assumes a branch must call `refuse_lead` or return an explicit Lead result, and the app must guard its callers (`Card::lead`). See [lead-agent.md](../../docs/lead-agent.md).

`lead_for_project` finds the live Lead. `workers_of(lead_id)` returns the live sessions whose `started_by` is that id.

### PTYs

`PtyHub::open` spawns the program on a PTY, starts one reader thread and one writer thread, and returns a `PtyId` (never reused in a run). The reader reads for as long as the process lives, whether or not the terminal is on screen, so a hidden CLI never stalls on a full buffer. It hands raw bytes, then exactly one `Exit`, to the `PtySink`. A sink must never block; the app's sink pushes into an unbounded channel. `write` queues bytes to the writer thread and never blocks. Dropping a live PTY kills a child that has not exited.

One terminal view per live PTY belongs to the app, not core. See [terminal-tabs.md](../../docs/terminal-tabs.md).

### PTY environment

`pty::configure_child` builds every child's environment:

- `PATH` is the login-shell `PATH` (for a Lead and a Lead-started worker's agent, with the control `bin/` directory first).
- `PWD` is the worktree, so a login shell reports it.
- `GIT_DIR`, `GIT_WORK_TREE`, `GIT_INDEX_FILE`, `GIT_PREFIX`, `GIT_COMMON_DIR`, and `GIT_OBJECT_DIRECTORY` (`worktree::GIT_REDIRECTS`) are removed so git cannot follow another checkout.
- `SHIKA_SOCKET` and `SHIKA_TOKEN` (`pty::CONTROL_VARS`) are removed, then set from `SpawnRequest::env` only when the request carries them. Only `create_lead` and a Lead-started worker's agent (`LaunchOptions::control`) do. An author-created card, every shell tab, and a Shika started from inside a Lead terminal end up without them. A worker's own token is report-only in the app, so a worker still cannot start workers.

`PathEnv::capture` runs the user's login shell once, from a short `PATH` with a cleared environment (Nix's `__NIX_DARWIN_SET_ENVIRONMENT_DONE` otherwise stops the shell from rebuilding `PATH`), and resolves each preset binary. A GUI app does not see Homebrew, nvm, or `~/.local/bin`, so everything that spawns a process uses this `PATH`, never the app's own.

### Activity bridge

For Pi only, `create_session` and `create_lead` install an `ActivityBridge`: a private `0700` directory, a small extension passed to Pi, and a report file the extension rewrites atomically. `Core::session_activity` reads the latest report (at most 129 bytes) without any lock. The sink wrapper `activity_sink` cleans the directory up when the PTY exits. Other CLIs use the app's live-screen detection. Core never inspects terminal bytes. Model and traps: [agent-activity.md](../../docs/agent-activity.md).

### Diff and diff stat

`session_diff_stat` and `session_diff` compare the task worktree to the merge base with the ref the card started from, falling back to the default branch, then to `HEAD`. They count commits, uncommitted edits, and untracked files that are not ignored. The Changes panel and the card therefore agree.

Both read only the task worktree. `worktree::read_only_git` sets `GIT_OPTIONAL_LOCKS=0` and `GIT_CEILING_DIRECTORIES` to the worktree's parent, and `check_worktree` requires git's top level to equal the worktree. A missing or broken `.git` is an error, never the main checkout around `.worktrees/`. Shared diff options (`TASK_DIFF`, `TASK_PATCH`) pin color, external diff, textconv, rename detection, submodule format, and prefixes so the user's git config cannot change what is counted. Caps, collapse reasons, and performance: [changes-panel.md](../../docs/changes-panel.md).

### Diff rendering for the Lead

`render_unified(&SessionDiff, stat_only, worktree, cap)` in `diff.rs` is a pure function from the Changes panel's data to the text `shika diff` prints: `diff <letter> <name> (+a -r)` per file (renames `old -> new`, untracked marked and shown as added, binary noted), `---`/`+++` and hunks, collapsed files noted with the worktree to read, a total line, and a truncation note naming `worktree` when the output passes `cap` (`RENDER_CAP_BYTES`, 200 KB). It cuts only at line boundaries. `--stat` gives one line per file. The server computes the diff with `session_diff` off the app thread and renders it there.

### Publishing

`session_publish_preview` computes the file list, target, and an editable title using a disposable index; it never touches the user's staging area. `session_publish` re-verifies the preview against the current tree and then commits if needed, pushes, and runs `gh pr create`. It never merges, force-pushes, runs tests, or closes the task. `session_pr_checks` is one bounded `gh pr view`. All of it needs an authenticated `gh` on the login-shell `PATH`. Full contract: [publishing.md](../../docs/publishing.md) and [pr-checks.md](../../docs/pr-checks.md).

### Preparation

`.shika/worktrees.json` in the main checkout may declare `setup-worktree` commands, literal ignored `copy-files`, and `timeout-seconds`. Core parses it without executing, compares it with the stored approval, copies files with rooted no-symlink writes, and runs commands in isolated process groups with no stdin. Two setups run at once at most (`PreparationLimiter`). `PreparationControl` cancels, optionally preserving the worktree (quit and project removal). Commands are trusted project code, not a sandbox. Read [worktree-preparation.md](../../docs/worktree-preparation.md) before touching this.

### Close and leftovers

`session_close` closes only when nothing can be lost, `session_discard` is the explicit confirmed throw-away, and `session_push_and_close` is the explicit push choice. None of them commit. `session_switched_close_check` and `session_close_switched` handle a task whose branch the user switched. `leftover_remove` takes a path, requires it to be in the journal, not live, and under a `.worktrees/` folder. See [tasks-and-worktrees.md](../../docs/tasks-and-worktrees.md) and [branch-switch-close.md](../../docs/branch-switch-close.md).

## The control protocol

`shika_core::control` is the one public module. It defines everything the `shika` command and the app server share, and nothing about the server's state. In short:

- One Unix-socket connection carries one JSON request line and one JSON reply line.
- A request is `{ version, token, command }`. Commands: `help`, `tasks`, `new`, `status`, `wait`, `read`, `diff`, `send`, `key`, `pr`, `close`, and the worker-only `report` (`clean_report`, `MAX_REPORT_BYTES`, `WORKER_HELP`). Replies: `help`, `tasks`, `started`, `status`, `waited`, `text`, `done` (also the PR URL and `closed ...`), `refused`, `error`. `TaskInfo` carries the worker's worktree `path` and its latest `report` from the current turn.
- `PROTOCOL_VERSION` (currently 4) must match, or the server refuses. The request line is capped at `MAX_REQUEST_BYTES` (1 MiB).
- `ControlDir::create` makes the per-run `0700` directory holding `sock` and `bin/shika`, a symlink to the running executable.
- `parse_args` and `render_text` are the client's front and back end; `--json` prints the reply as serialized. An unknown command word is a usage error (exit 2). `is_client_invocation(args, lead_env)` decides client or app: with `SHIKA_SOCKET` or `SHIKA_TOKEN` set it is always the client, otherwise only a command word is.
- `key_name` normalizes and validates `shika key` names (`KEY_NAMES`, `MAX_KEYS`); `MAX_READ_LINES` bounds `read --lines`. The app turns names into bytes with `shika_terminal::input`; core stays free of terminal types.
- `send` has no read timeout for `new` (preparation can take `timeout-seconds`; the server always answers or closes the socket) or for `pr` and `close` (they wait for the author), `timeout_secs + 30` for `wait`, 60 seconds otherwise.

The command reference, wire examples, `wait` semantics, security model, debugging, and the checklist for adding a command are in [docs/shika-cli.md](../../docs/shika-cli.md). The product decision is in [docs/lead-agent.md](../../docs/lead-agent.md). The server is `crates/shika/src/control.rs`.

## Invariants

Do not break these. Each has a reason.

- **Never pass a CLI's own worktree flag** (`agent --worktree`, `codex --worktree`). Shika creates the worktree so it can journal, base, track, and clean it. Do not invent launch flags; the table in AGENTS.md was taken from each binary's `--help`.
- **Create worktrees with `--no-track`.** Otherwise a fresh card looks pushed and a plain `git push` targets the base.
- **Never edit the user's `.gitignore`.** `.worktrees/` goes into the repository's `info/exclude` (`ensure_excluded`), which is local and invisible to the repo.
- **Journal before you start, remove after the tree is gone.** A crash between the two leaves a leftover the author can see, never an invisible tree. Failure paths remove only provably untouched trees.
- **Close, discard, and leftover cleanup act only on what the journal and session list prove is Shika's.** Never delete a path because it looks like a task folder.
- **Close never commits.** Only the confirmed publish flow creates a commit.
- **Remove the git redirect variables from every git call and every PTY** (`GIT_REDIRECTS`). Use `worktree::git_cmd` for git; do not build a bare `Command::new("git")`.
- **Background reads use `read_only_git`.** `GIT_OPTIONAL_LOCKS=0` so a refresh never takes `index.lock` from under the author's own git, and the ceiling directory so a broken worktree never reads the main checkout.
- **Reads take no operations lock.** Otherwise a slow diff on a huge change blocks Close.
- **Use the login-shell `PATH` for every child**, and set `PWD`. The app's own `PATH` is the GUI's short one.
- **Ids are unique across draft and Lead folders.** Both are generated from `taken_ids`. Two live worktrees must never share `<id>`.
- **Scrub `SHIKA_SOCKET` and `SHIKA_TOKEN` from every PTY except a Lead's.** This is what makes the worker tree one level deep.
- **A Lead has no branch.** An empty `branch` is a signal, not an oversight: do not give it one, and guard new per-session features.
- **A launch prompt reaches the CLI as one argument that cannot be a flag.** Keep `check_prompt` in front of every path that adds one.
- **Core never talks to a model API and never blocks the PTY reader.** A `PtySink::send` that blocks stalls the CLI.
- **Do not copy from Zed's `terminal` crates (GPL).** Terminal types stay in `shika-terminal`.
- **No em dashes** in code, docs, or visible text.

## Testing

```sh
source "$HOME/.cargo/env"
cargo test -p shika-core
cargo test --workspace
cargo test --workspace --doc
cargo doc -p shika-core --no-deps
```

Tests never touch real app data or the user's repositories:

- Each module's tests build a scratch directory under `std::env::temp_dir()` named with a timestamp and a counter, `git init` repositories inside it, and delete it on `Drop`. Each module has its own small `Scratch` (or `Fixture`) helper in its test module. Paths are canonicalized, because `/var` is a symlink on macOS and git reports resolved paths.
- Git is run with fixed author and committer environment variables so tests do not depend on the machine's git config.
- A bare remote is `git init --bare` in the scratch directory, added as `origin` and pushed to. The base-branch, fetch, push, publish, and close tests use it, including a remote that rejects pushes to exercise real push errors.
- The agent CLI is a shell script (`fake_cli`, or `REPORTING_CLI`, which prints the last argument and the `SHIKA_*` and `PATH` it was given). `Core::open_with` takes a `PathEnv` built by the test-only `PathEnv::from_lookup`, so no login shell runs and no real CLI is needed.
- `pty::tests` provides `channel_sink`, `collect_until`, and `collect_to_exit` for reading what a child printed.
- The Lead tests in `lib.rs` start with `a_lead_`, `workers_get_`, `only_a_lead_started_workers_agent_`, and `closing_a_lead_`; the control protocol tests are in `control.rs`.

Preparation integration fixtures (`preparation_tests.rs`) reserve exclusive roots with `create_dir`, PID/timestamp/counter, and collision retries. This replaces timestamp-only `create_dir_all`, which let parallel fixtures share/delete each other's repositories. Do not regress to adopting existing roots, and retain parallel workspace validation. See [worktree-preparation.md](../../docs/worktree-preparation.md).

Rules for manual testing: use disposable repositories and local bare remotes with `open -n target/debug/Shika.app --args --data-dir /absolute/test/data`. Never run an experiment against `~/Library/Application Support/com.hieule.shika`. Unit tests passing does not establish GUI behavior; see [MANUAL_CHECKS.md](../../MANUAL_CHECKS.md).

## Extending the crate

- A new `Core` method that changes git or the session list: take `operations`, return `Error` variants with the sentence the user should read, and add it to the blocking or quick list in the `lib.rs` crate docs.
- A new per-session method: decide its Lead behavior (refuse or a defined result) and add a test beside `a_lead_refuses_branch_diff_publish_and_shell_operations`.
- A new git call: go through `git_cmd`, or `read_only_git` for background reads of a task worktree, and keep the user's git config from changing results (see `TASK_DIFF`).
- A new persisted field: give it a serde default so old files load, and say in the relevant guide what a missing value means.
- A new control command: follow the checklist in [docs/shika-cli.md](../../docs/shika-cli.md#adding-a-command).
- Update [AGENTS.md](../../AGENTS.md) when behavior or a toolchain trap changes, and the matching guide in `docs/`.
