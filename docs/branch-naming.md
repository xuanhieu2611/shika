# Branch naming

How Shika names the git branch and the card for a new task, why it works this way, and where to look when it does not.

Added 2026-10-05. This file holds the naming rules and explains them.

## What the user sees

1. **New** creates `<repo>/.worktrees/shika-draft-<id>` on branch `shika-draft-<id>` and starts the CLI there.
2. **First Enter.** The first line typed in the agent terminal names the card and renames the branch to a slug of that line, for example `okay-we-need-a-way-to-blur`.
3. **About a second later.** The CLI's own session title replaces both, once: the card shows "Shika background opacity and blur" and the branch becomes `background-opacity-and-blur`.
4. **The author** commits and pushes in the card's shell as usual. `git push -u origin HEAD` pushes the clean name without typing it.

The worktree **folder** keeps its `shika-draft-<id>` name. Only the branch is renamed. The folder is local and never reaches git history or the remote.

An optional **branch prefix** in Settings (Cmd-,), for example `dev/`, goes in front of every name Shika picks.

## Why

### The problem

Branch names used to come only from the first prompt line. People write prompts as conversation, so the names were long, meaningless, and public once pushed: a PR merged as `Merge pull request #3 from user/i-m-not-sure-if-this-is-expected-but-i-tried-to`. Squash merging or editing merge messages hides this for one solo repo, but it does not fix it for anyone else using Shika.

A good branch name is a short summary of the task. A slug of the first few words cannot produce one, because in a conversational prompt the meaning is rarely in the first words.

### The decision

Claude Code and Cursor CLI already summarize every session. Each makes its own model call after the first prompt and saves a short title on disk. Shika reads that title. That gives a good name with no extra request, no tokens, and no model API inside Shika (Shika still does not wrap a model API). The name also matches what the user sees in the CLI's own session list.

### Alternatives considered and rejected

| Alternative | Why not |
| --- | --- |
| Run the agent's first prompt in the main checkout, worktrees only for later agents | Discard would have to reset the user's own checkout; the agent would work on `main`; Close, push, and cleanup would need two code paths. Every card gets a worktree. |
| Squash merging, or editing merge commit messages | Hides the name in one repo's history; the PR page and branch list still show it, and it is not a fix for other users. |
| A separate one-shot model call (`claude -p ...`) to name the branch | Works (tried: about 6 seconds), but it is an extra request on the user's plan when the CLI has already produced a title. |
| Ask for a task name when pressing New | An extra step. The point of Shika is to open the CLI and start typing. |
| Smarter slug rules (drop filler words) | Still guesses from the first few words. |
| Rename from the first commit message | The commit usually comes right before the push, so `git commit && git push` races the rename, and the branch would change under the user's shell. |
| Tell the agent to rename its own branch | Cursor CLI has no flag to add instructions, agents can ignore them, and it puts housekeeping into the user's conversation. |
| Rename the worktree folder too | The CLI is already running inside it; moving a process's working directory is not reliable. |

### Why renaming is safe

A local branch is only a name that points at a commit. `git branch -m` changes that name. Commit hashes, messages, and history are untouched, and nothing is sent to a remote. A remote learns a branch name only when it is pushed. That is why Shika renames only before the first push, and never after.

## How it works

### Flow

```
New card
  core.create_session            worktree + branch shika-draft-<id>, CLI starts
first Enter in the agent terminal
  PromptCapture.feed             reconstructs the first submitted line from keystrokes
  core.session_rename_from_prompt   branch -> prefix + slug(prompt), card title = prompt
  TitleWatch.start               polling begins (started by the first Enter)
every 2 s, for up to 2 min, one check at a time
  core.session_apply_cli_title
    CliHome.read                 read the CLI's own title; None means "not yet"
    if HEAD is still the task branch and the branch is not on a remote:
      branch -> prefix + slug(title)
    card title = CLI title; applied once (Session.cli_titled)
```

Both renames go through `rename_task_branch`, which adds the prefix, picks a free name, runs `git branch -m`, and updates `worktrees.json` (rolling the git rename back if the journal cannot be saved).

### Where the code is

| Piece | Location |
| --- | --- |
| Reading the CLIs' titles | `crates/shika-core/src/cli_title.rs`: `CliHome::detect`, `CliHome::read`, `claude`, `codex`, `cursor`, `pi`, `cwd_forms` |
| Prompt rename (immediate fallback) | `Core::session_rename_from_prompt` in `crates/shika-core/src/lib.rs` |
| CLI title rename (once) | `Core::session_apply_cli_title` in `crates/shika-core/src/lib.rs` |
| Prefix, free name, journal update | `Core::rename_task_branch` in `crates/shika-core/src/lib.rs` |
| Slug, prefix cleanup, git checks | `crates/shika-core/src/worktree.rs`: `branch_slug`, `normalize_prefix`, `is_valid_branch`, `head_branch`, `is_published`, `rename_branch` |
| "Already applied" flag | `Session::cli_titled` in `crates/shika-core/src/session.rs` |
| Prefix setting | `Settings::branch_prefix` in `crates/shika-core/src/settings.rs`, stored as `branchPrefix` in `settings.json` |
| Polling schedule | `TitleWatch` in `crates/shika/src/model.rs` (checks every `EVERY` = 2 s for `FOR` = 120 s) |
| Polling and UI updates | `Shika::tick` in `crates/shika/src/main.rs` (look for `title_checks`) |
| First-line capture | `PromptCapture` in `crates/shika/src/model.rs` |
| Settings row | `Shika::prefix_row` and `PREFIX_ROW` in `crates/shika/src/main.rs` |

### Threads

`session_apply_cli_title` reads files and runs git, so the app calls it on the background executor, like every other blocking `Core` method. `TitleWatch` makes sure only one check per card is in flight. Both renames take `Core`'s operations lock, so they never interleave with each other or with Close.

## Naming rules

- **Slug** (`branch_slug`): split on every character that is not an ASCII letter or digit, lowercase, join with `-`. At most 48 characters (`SLUG_MAX`), cut at a word boundary. A single word longer than 48 is cut at 48.
- **Project name**: whole-word copies of the project's name are removed, as a word sequence ("Fix Sample Project tracker" in `sample-project` becomes `fix-tracker`). If that would leave nothing, the name stays. Partial words are kept (`Shikari` stays in `shika`). The card title keeps the CLI's wording; only the branch drops the project name.
- **Empty slug**: the prompt fallback uses `task-<id>`. A CLI title with no usable characters leaves the branch alone and only sets the card title.
- **Prefix** (`normalize_prefix`): keeps ASCII letters, digits, `.`, `_`, `-`, and `/`; drops empty path parts, leading `.` or `-`, trailing `.` and `.lock`, and collapses `..`. A non-empty prefix that does not end in `-` or `_` gets a `/`, so `dev` and `dev/` both give `dev/`. If git still refuses `prefix + slug` (`git check-ref-format --branch`), the prefix is dropped for that name. Unreadable settings also mean no prefix.
- **Collisions** (`rename_branch`): if `refs/heads/<name>` or any `refs/remotes/*/<name>` exists, try `<name>-2`, `<name>-3`, and so on. Asking for the name the branch already has is a no-op.
- **Never renamed**: a branch that is on a remote (`is_published`: it has an upstream, or a remote-tracking branch has its name, as after `git push origin HEAD` without `-u`); a worktree whose HEAD is not the task branch (the user switched branches). In both cases the card still takes the CLI title.
- **Once**: after a CLI title is applied, later titles and prompts change nothing.

## Where the CLIs keep their titles

These are **private files, not a public API**. They were found by inspecting real data on 2026-10-04 and 2026-10-05 with Claude Code 2.1.289, Cursor CLI 2026.10.01-e373342, Codex CLI 0.160.0, and Pi 1.0.0. A CLI update can move or change them without notice. When that happens, Shika's readers return None and naming falls back to the prompt slug. Nothing breaks, but names get worse. Fixing it means updating the reader in `cli_title.rs`.

### Claude Code

- Directory: `<config>/projects/<encoded cwd>/`, where `<config>` is `$CLAUDE_CONFIG_DIR` if set, else `~/.claude`.
- `<encoded cwd>`: the absolute working directory with every character that is not an ASCII letter or digit replaced by `-`. `/Users/x/code/shika/.worktrees/shika-draft-18db` becomes `-Users-x-code-shika--worktrees-shika-draft-18db`.
- One `<session-id>.jsonl` per session. The title is a line like `{"type":"ai-title","aiTitle":"Shika background opacity and blur","sessionId":"..."}`. It is written about a second after the first prompt and repeated later in the file.
- Shika reads the newest `.jsonl` (by modified time) that has an `ai-title` line, and takes the last one.

### Cursor CLI

- Directory: `~/.cursor/chats/<md5 hex of the cwd>/<agent-id>/`.
- `meta.json` there holds `{"schemaVersion":1,"title":...,"cwd":"...","updatedAtMs":...}`. `title` is missing or null until the chat is named. The file holds no secrets.
- Shika reads every `meta.json` in that directory, keeps those whose `cwd` equals the worktree path and whose `title` is a string, and takes the most recently updated one.
- Do not read `store.db` next to it. It holds the same name inside SQLite along with a `blobEncryptionKey`; `meta.json` is enough.

### Codex

- Database: the highest `state_<n>.sqlite` in `$CODEX_HOME` if that is set, otherwise `~/.codex`. On 2026-10-05 that file was `state_5.sqlite`. Sidecars such as `state_5.sqlite-wal` are not databases.
- Table `threads`. The conversation name is `name`. It stays empty until Codex names the thread. `title` is often the raw first message, so Shika does not read it. `cwd` must equal the worktree.
- Shika opens the database read-only and takes the newest matching `name` (`updated_at_ms`). A missing file, a lock, or an unexpected schema means no title.

### Pi

- Pi does not generate a conversation name. A name exists only after `/name`, `--name`, or `pi.setSessionName()` from an extension. Of the sessions inspected on 2026-10-05, none had one. When none is set, the prompt name stays.
- Directory: `PI_CODING_AGENT_SESSION_DIR` if set, otherwise `<PI_CODING_AGENT_DIR or ~/.pi/agent>/sessions/--<path>--/`.
- `<path>`: the absolute working directory with the leading separator removed and `/`, `\`, and `:` replaced by `-`. `/Users/x/code/shika/.worktrees/shika-draft-18db` becomes `--Users-x-code-shika-.worktrees-shika-draft-18db--`.
- One `<timestamp>_<session-id>.jsonl` per session. The name is a line like `{"type":"session_info","name":"Opacity and blur"}`.
- Shika reads the newest `.jsonl` (by modified time) that has a `session_info` name, and takes the last one.

### Paths

The CLIs record the resolved working directory, so under `/tmp` they record `/private/tmp/...`. `cwd_forms` tries the worktree path as Shika has it and its canonical form. Codex matches `threads.cwd` against those forms. Pi encodes each form into its session folder name.

## Guarantees

- Shika only reads the CLIs' files. It never writes, locks, or deletes them.
- A missing, unreadable, or unexpected file is "no title yet", not an error.
- No title within two minutes leaves the prompt name in place.
- A git failure while applying a title (for example a corrupt repository) stops the watch for that card and shows a toast. The card keeps working.

## Debugging

Start with the card's branch (shown on the card) and `git branch --show-current` in its shell.

**Is there a title to read?** In the card's shell (cwd is the worktree):

```sh
# Claude Code
ls ~/.claude/projects/"$(pwd -P | sed 's/[^A-Za-z0-9]/-/g')"/
grep -h '"ai-title"' ~/.claude/projects/"$(pwd -P | sed 's/[^A-Za-z0-9]/-/g')"/*.jsonl | tail -1

# Cursor CLI (one meta.json per chat started in this folder)
cat ~/.cursor/chats/"$(printf %s "$(pwd -P)" | md5)"/*/meta.json

# Codex. The number in state_<n>.sqlite moves; use the highest one.
sqlite3 ~/.codex/state_5.sqlite "SELECT name, cwd FROM threads WHERE cwd = '$(pwd -P)' ORDER BY updated_at_ms DESC LIMIT 1;"

# Pi. A name is present only when one was set.
dir=$(pwd -P | sed 's#^/##; s#[/:\\]#-#g')
grep -h '"session_info"' ~/.pi/agent/sessions/"--${dir}--"/*.jsonl | tail -1
```

If these find nothing, check whether the CLI moved its files, then update `cli_title.rs`.

**A title exists but the branch did not change.** Check, in order:

- The watch ran out. It stops two minutes after the first Enter.
- The branch is on a remote. `git rev-parse --abbrev-ref @{upstream}`, and `git for-each-ref "refs/remotes/*/$(git branch --show-current)"`.
- HEAD is not the task branch (the user ran `git switch`).
- The title has no ASCII letters or digits, so it produced no slug.
- A toast reported a git error, which stops the watch.

**No title at all from Claude Code.** Claude saves no transcript, and so writes no title, when it inherits `CLAUDE_CODE_CHILD_SESSION`. That happens when Shika itself was started with `cargo run` from inside a Claude Code session. Launch the built app with `open` instead. A Dock or `open` launch does not inherit it.

**Odd first names (doubled or deleted words).** The fallback name comes from `PromptCapture`, which rebuilds the line from keystrokes. It understands Backspace, Ctrl+W, Option+Backspace (`ESC DEL`), Ctrl+U, and Ctrl+C. It does not follow the cursor, so editing in the middle of the line with the arrow keys can produce a wrong fallback name. The CLI title normally replaces it a second later.

**Manual end-to-end check.** Build with `./scripts/bundle-app.sh --debug`, then `open -n target/debug/Shika.app --args --data-dir /absolute/test/data` with a `projects.json` pointing at a disposable repository with a local bare remote. Never use personal projects. Steps are in `MANUAL_CHECKS.md` under "Branch names from the CLI title".

## Tests

- `cli_title.rs`: sample files for each CLI, no title yet, broken JSON or a broken database, wrong `cwd`, `CLAUDE_CONFIG_DIR`, `CODEX_HOME`, Pi's session and agent directories, a symlinked worktree path, an unknown CLI.
- `worktree.rs`: slugs and word-boundary cuts, project-name removal, prefix cleanup checked against `git check-ref-format`, renames that skip local and remote names, `is_published`.
- `lib.rs`: the full flow on real git repositories: prompt rename, then CLI title rename, once; journal kept in step; the prefix; a branch pushed without `-u` keeps its name; a switched worktree keeps its branch.
- `model.rs`: `TitleWatch` timing (one check at a time, gives up), and Option+Backspace in `PromptCapture`, using the real typing that produced a doubled branch name.

Tests use a scratch `CliHome::at(...)` and never read the real `~/.claude`, `~/.cursor`, `~/.codex`, or `~/.pi`.

## Extending

### Adding a CLI

1. Find where the CLI stores its session title, with real data from the installed version. Do not guess a format.
2. Add a reader to `CliHome` in `cli_title.rs` and match its preset id in `CliHome::read`. Keep it read-only and return None on anything unexpected.
3. Add tests with sample files: title present, not yet named, broken file, a session for another folder.
4. Record the location and the CLI version checked in this file.

### Ideas not built

- A manual "rename branch" action on the card.
- Transliterating non-ASCII titles (for example Vietnamese with diacritics) instead of dropping those letters.
- Stripping `CLAUDE_CODE_CHILD_SESSION` and related variables from the PTY environment, so a Shika started inside Claude Code still gets titles. It affects more than naming, so decide it on its own.

## External branch renames

An agent or shell can run `git branch -m` before committing, pushing, or opening a PR. Shika refreshes branch names off-thread every two seconds and also before Close, push-and-close, or discard. It adopts a new name only when the branch reflog contains an explicit rename chain from the recorded name and the original local branch no longer exists. Multiple renames with commits between them work. The card title, worktree folder, and recorded base stay unchanged; the journal saves before the session publishes the new name.

A branch switch, detached HEAD, recreated original branch, or missing rename evidence never transfers task ownership. Ordinary Close, Push, and Discard retain the identity refusal. The app now offers a separate, explicit branch-switch Close confirmation if both local branches can be verified safe; see [branch-switch-close.md](branch-switch-close.md). Detached HEAD or a missing recorded branch cannot use that recovery. The proof reads at most the latest 256 branch reflog entries; disabled, expired, or older history is conservatively refused. Matching commit hashes or a missing original ref alone never counts as proof. Dirty work and unpushed commits still require the existing confirmation. A clean pushed branch is retained; an empty task or confirmed discard deletes the current task branch. Close rechecks branch identity after stopping the PTYs.

Close reports the current and recorded task branch directly, rather than calling a branch mismatch a git-status failure. For example, an agent resolving an existing PR might rename its task branch to `conflict-resolver`, then check out the PR branch. If both branches are clean and published or integrated, the branch-switch recovery dialog can close the task while preserving both branches. Otherwise return to `conflict-resolver` in that card's shell with `git switch conflict-resolver`, then Close again. The PR branch and its commits remain separate and are not deleted by closing the task. If the recorded name was itself renamed before Shika refreshed it, return to the renamed task branch so Shika can verify the rename chain. Never force a switch or reset to bypass dirty work. A genuine failure to read HEAD still reports git's error.

Code: `worktree::was_renamed`, `Core::ensure_session_branch`, `Core::session_refresh_branch`, and the independent branch refresh in `Shika::tick`. Regression tests cover dirty, empty, unpushed, pushed and merged renamed tasks, rename chains, missing history, recreated refs, unrelated branches, and detached HEADs.
