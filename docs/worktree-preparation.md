# Worktree preparation

Shika keeps creating a fresh, isolated worktree for each card. Optional project preparation makes that worktree ready before its agent starts: copy selected local files, then run the commands the project needs.

This is preparation, not a warm-worktree pool or a compiler-cache feature. It does not promise faster launches or end-to-end performance parity with another app. Projects without configuration run no preparation. Their first author-created New offers an optional review of unsaved detected defaults; later launches remember that project's choice. Shika never silently infers executable setup, runs a build automatically, shares mutable dependency folders, or executes Cursor/Codex configuration.

Start here when contributing to this feature. This document records the product decision and explains usage and implementation. [tasks-and-worktrees.md](tasks-and-worktrees.md) covers the worktree lifecycle it extends. [MANUAL_CHECKS.md](../MANUAL_CHECKS.md#optional-worktree-preparation) separates verified behavior from remaining GUI checks.

## Reading paths

- **New to the team:** read the [overview](#two-minute-overview), [feature evolution](#how-the-feature-evolved), [decisions](#why-this-design), and [first-New flow](#first-new), then follow the [implementation map](#implementation-map).
- **Debugging:** start with the [state table](#configuration-consent-and-onboarding-state) and [symptom guide](#debugging-by-symptom). Reproduce with [isolated app data](#reproducing-first-new-onboarding), never a personal repository.
- **Contributing a change:** read the [editor lifecycle](#editor-lifecycle-and-save-boundaries), [launch lifecycle](#launch-lifecycle-and-lock-boundaries), and [extension checklist](#tests-acceptance-and-safe-extension). Check [acceptance evidence](../MANUAL_CHECKS.md#first-new-per-project-setup-onboarding) before claiming a GUI regression is fixed.

## Two-minute overview

- **Before:** New created a draft branch/worktree and started the CLI. Ignored local files and dependency installation were left to the user or agent.
- **Now:** An optional, approved project configuration inserts copying and setup between worktree creation and CLI startup. Nothing runs when a project is added.
- **Ownership:** Shika still owns worktrees and branches. Setup uses ordinary child processes, not an agent PTY. There is no new model API, session history, or second visible terminal.
- **Safety:** Register the worktree before setup, exclude active setup from leftovers, and remove failed work only when it is provably untouched. Quit preserves worktrees.
- **Concurrency:** Two setups run at once outside the core operation lock. Card completion uses shared-state identity, not a captured row index.
- **Start reading code:** `Core::create_session_with_preparation`, `preparation::prepare`, then `Shika::begin_launch`. See the [contributor guide](#contributor-guide) for the full map and debugging recipe.

## How the feature evolved

The user problem was not just creating a checkout. Developers repeatedly had to copy ignored local configuration, install dependencies, and explain those steps to each new agent. A task could start successfully but still be unusable for testing. Shika should let the author focus on prompts, feature work, and checking results, without silently executing repository code.

| Stage | What changed | Why the next change was needed |
| --- | --- | --- |
| Fresh worktrees only | Shika allocated an isolated task and started the selected CLI. | Git does not carry ignored `.env` files or installed dependencies into a fresh checkout. Manual preparation repeated per task. |
| Opt-in preparation engine | A manually created `.shika/worktrees.json` added approved copy/setup before CLI startup, progress, cancellation, and fresh retry. | The engine solved repetition, but people had to discover the feature and write JSON before it helped them. |
| In-app editing | A native editor added validated Save/Disable. An early entry-point iteration used project-header/Agent-menu controls; these were removed in favor of Settings > Projects. | Persistent setup controls should not clutter the column or turn task tracking into project administration. Settings is navigation, not a global setup template. |
| Unsaved detected defaults | Missing configs gained conservative `.env`/`.env.local` and npm/pnpm suggestions. Existing configs remained authoritative. | An empty form still asked people to know their file names and install command. Different repositories need different defaults. |
| First-New onboarding | The first eligible author New now shows a compact review, optional Customize, combined Save/approve/start, or remembered skip. | Settings-only discovery still interrupted the natural start-a-task flow. A one-time review makes setup discoverable at the point of need without repeated prompts or automatic execution. |

These are successive usability changes around the same preparation engine, not replacements for worktree isolation or the selected CLI. The former header/menu placement is historical, not a supported alternate entry point. There is no separate setup prompt on Add project.

## Why this design

The goal was "configure once, then prompt several isolated agents", while preserving an already-working fresh-worktree lifecycle. A new checkout does not contain ignored local configuration or installed dependencies. Making it ready is a separate problem from allocating it quickly.

| Decision | Reason and trade-off |
| --- | --- |
| Keep a fresh branch/worktree per card | Preserves isolation and existing close protections. A pool would introduce retained state, ownership, reset, and recovery rules; it was not needed to address checkout allocation. Warm build output can still be valuable, but this change does not preserve it across unrelated tasks. |
| Opt-in `.shika/worktrees.json` in the main checkout | Project-specific preparation is reviewable and shareable. Reading the main checkout lets configuration be tried before committing it; scripts and ignore rules still need to exist on the task's base. |
| Own configuration, not automatic provider-config execution | The repository-configuration approach fits the setup workflows investigated in Cursor and Codex desktop. It is not format compatibility. Silently importing another tool's scripts would obscure which code Shika has permission to run. |
| Local approval of the parsed configuration | A repository cannot grant itself execution consent. Parsed equality avoids reapproval for JSON formatting or omitted defaults, but changes to command strings, array order, paths, or timeout ask again. Referenced script contents are not pinned. |
| Literal, independent copies of selected ignored files | Supplies local configuration without copying tracked edits or sharing mutable task files. Globs, directory copying, symlinks, hardlinks to the source, and overwrite behavior were deliberately excluded. |
| Explicitly saved commands, no automatic installs or builds | Settings and first-New onboarding suggest fixed npm/pnpm installers in an unsaved draft when metadata is unambiguous. Settings saves without approval; onboarding's explicit Save, approve and start combines saving and consent for the displayed configuration. A project's correct setup still depends on its workflow; guesses never become launch behavior on their own. |
| Per-project configuration and decisions | One repository may copy `.env` and use `npm ci`; another may copy `.env.local` and use pnpm. Each owns its main-checkout config and local approval/preference. A shared Settings section does not imply shared setup. |
| Settings for ongoing edits, first New for discovery | Keeps project headers uncluttered while putting the initial decision in the normal task-start flow. Adding/browsing a repository is not permission to create config or run code. |
| Compact review with optional customization | Common cases show the actual paths/commands, not a large form. The same editor handles exceptions, so review and Settings do not drift into different schemas or validators. |
| Explicit combined save/approval only in first-New review | The selected CLI and launch intent are already known. A clearly named Save, approve and start action avoids a second confirmation of identical values. Settings Save still means configuration editing, never an implicit launch or permission grant. |
| Remember skip locally, not in the repository | Skip is an author's onboarding preference, not a team-wide instruction or approval. It should stop nagging that project without preventing later Settings edits or approval of a subsequently added config. |
| Conservative, on-demand metadata detection | Fixed commands and literal paths are inspectable. Root-only bounded reads avoid secret-content scanning, script evaluation, package-manager subprocesses, network work, and ongoing startup/watch costs. Unsupported projects remain configurable manually. |
| Manual dev server and browser workflow | Installation is a finite prerequisite; a server is a long-lived task process. Cmd+T and a localhost link reuse existing shell/terminal ownership rather than introducing a preview service, port manager, or second terminal. |
| Two setup slots and fresh-worktree retry | Bounds simultaneous heavyweight work without serializing all app actions. A fresh retry avoids trusting partially installed or modified state; valuable failed work stays separately journaled. The limit is policy, not a measured optimum for every machine. |

### Evidence behind keeping fresh worktrees

The initial exploratory benchmark on 2026-10-05 used Shika's repository at `42ca34e0e477eeb738151a5bb95617b883a3fc87`, an Apple M4 Pro with 24 GiB RAM, and disposable checkouts:

- Fresh worktree creation: median **38.4 ms**, 10 samples. Four concurrent creations: median **79.9 ms total**, five batches.
- Full workspace debug builds into separate empty targets: **71.498 s** and **71.504 s**, using `cargo build --workspace --locked --offline -j 2`.
- Reusing populated build output: unchanged builds took **0.235-0.567 s**. Each populated target occupied about **2.2 GiB** of per-directory output.

Checkout allocation was inexpensive here; compilation dominated the measured build time. These were Git/build-operation measurements, not application comparisons. Dependency sources and filesystem caches were warm; there were no network-cold installs, concurrent full builds, GUI timings, or model requests. Warm reuse modeled native Git operations, not the Treehouse binary. Do not generalize these numbers into performance guarantees.

Compiler-cache investigation was explicitly paused, and pooling was not adopted. Neither is an implied follow-up task for this feature. Any future proposal needs a separate decision and evidence.

Background references, not dependencies or a promise of current CLI parity: [Treehouse](https://github.com/kunchenguid/treehouse), [Cursor worktrees](https://cursor.com/docs/configuration/worktrees), [Codex desktop worktrees](https://developers.openai.com/codex/app/worktrees), and [Codex local environments](https://developers.openai.com/codex/app/local-environments). The investigated Codex desktop setup behavior was not verified for its CLI. Never add a provider's worktree flag: Shika already created the checkout.

## First New

Adding a project runs nothing and writes no repository configuration. After choosing a CLI on the first author-created **New** for an unconfigured project, Shika opens **Set up worktrees for {project}?** with a compact preview of detected local files, exact install commands, timeout, and a trusted-code warning.

- **Save, approve and start** validates and saves the displayed configuration, records local consent for exactly that parsed value, then starts the chosen agent through normal preparation. No second approval dialog is needed for that unchanged configuration. Future agents in this project repeat its copy/setup automatically.
- **Customize setup…** expands the same native editor used by Settings. Edit/remove suggestions or add your own literal paths and commands before saving and starting.
- **Start without setup** saves only a local per-project onboarding preference and continues New without writing `.shika/worktrees.json` or granting execution consent. It refuses a config that appeared since preview, and launch rechecks the current file normally. Settings remains available later.
- **Cancel / Escape** returns to the same project's CLI picker, retaining the opening focus return. It saves no decision, configuration, or approval, and allocates no card/worktree. Cancelling the picker restores the original surface.

With suggestions, Save, approve and start is the focused primary action; without suggestions, Start without setup is. Tab / Shift+Tab traverses Customize and footer buttons (or the expanded editor), and held/repeated Enter cannot submit the setup dialog. Unknown/ambiguous toolchains remain manual rather than gaining a guessed command.

The local preference is `worktreeSetupReviewed` in that project's `projects.json` entry, not global configuration or execution approval. Skipping, explicit Settings saves/disables, and successful approvals record a decision. It survives relaunch; other projects remain independent. Older approved projects do not get a redundant first-use notice. Existing configurations, including empty ones, skip onboarding and keep the ordinary approval rules. Retry, Lead launch, and Lead-created workers are not interrupted by onboarding; core still enforces configured preparation consent for workers.

Repository configuration and local consent are separate atomic files, not one transaction. If configuration saves but local approval fails, no task or command starts; the error asks for cancel and New again to review the now-saved configuration. Stale or invalid drafts never grant consent.

## Configure once

For later changes, open **Settings > Projects** and choose a project's **Worktree setup…**. Each row names the project and main checkout path. `j`/`k` or arrows choose a project; Enter opens setup; `[`/`]` switches Settings sections. The persistent setup entry lives in Settings, not in the project header or Agent menu; first-New onboarding avoids a Settings detour. Empty projects are supported; with no projects, Settings asks you to add one.

The dialog edits literal file paths, ordered commands (one per row), and a timeout. **Add file path** and **Add command** create empty editable rows; **Remove** removes a row. Tab / Shift+Tab moves through fields and buttons, Enter activates a focused button or saves from a field, and Escape cancels. Native fields support clipboard, selection, and IME. Save/cancel/disable returns to Projects with that project selected. Done/Escape in Settings then restores the original terminal/card focus. Card selection never changes.

**Save setup** creates the folder/file only on explicit save and atomically writes `.shika/worktrees.json` in the project's main checkout. The file remains the only configuration source, shareable through Git; there is no extra global setting or watcher. Opening, cancelling, and adding a project create nothing. You can remove every suggestion and save an empty setup template, with the 600-second default timeout. Save does not execute commands, allocate a task, or approve preparation. New still uses the existing consent rules. Copy paths are checked for existence, regular-file/no-symlink safety, duplicates, and source Git ignore rules on save; the task's ignore rules are checked during launch, as before. File contents are never displayed.

**Disable setup** removes only `worktrees.json`, leaving other `.shika` files alone and restoring the no-config path for future tasks. It does not stop or modify running tasks. Changes while a task is preparing retain the existing finalization fence. Disabling is also an explicit save and is refused on a stale preview.

Externally changed parsed configuration is not silently overwritten: cancel and reopen to reload it. Invalid, unreadable, oversized, or symlinked existing configuration is refused on open/save and must be repaired outside the app. Saving normalizes formatting/default fields; formatting-only changes still reuse existing approval.

### Suggested defaults

Only when no configuration exists, opening Settings setup or eligible first-New onboarding reads a bounded set of main-checkout metadata in the background and prefills an **unsaved draft**. The dialog labels suggestions and asks you to review them. Nothing is written, approved, installed, or allocated by detection; cancelling leaves the no-config launch path untouched. Existing configurations always win, including an intentionally empty one. No global setting, watcher, recursive scan, network request, package-manager invocation, or provider-config import is added.

- **Files:** root `.env` and `.env.local` only, and only existing regular files ignored by Git. Symlinks, directories, tracked/nonignored files, nested files, production-specific env files, and missing files are not suggested. Contents are never read or shown. Add other literal paths yourself. The task's base must ignore them too; that check stays at launch.
- **Installer:** a regular `package.json` object of at most 64 KiB, its optional `packageManager`, and regular lockfile presence. npm's `package-lock.json` / `npm-shrinkwrap.json` suggests `npm ci`; pnpm's `pnpm-lock.yaml` suggests `pnpm install --frozen-lockfile`. The two npm locks count as the same manager. With an explicit npm/pnpm `packageManager` but no lock, suggest `npm install` / `pnpm install` and warn that installation may create a lockfile. No declaration or lock means no installer guess.
- **Ambiguity:** conflicting manager/lock metadata, multiple managers' locks, unsupported declared managers, unsafe/unreadable/malformed/oversized metadata, and unknown toolchains get no install command. Yarn and Bun lockfiles are recognized only to avoid misidentifying them as npm/pnpm. Explanatory notes ask for manual commands. No script fields are executed or copied into commands. Lockfile contents are not read; a suggested frozen install may still fail if the lock is stale, just as a manually configured install would.
- **Limits:** detection is root-only, not a monorepo environment resolver; review workspace/custom installs, nested env paths, runtime versions, available executables, ports, databases, and services yourself. Commands use the installed npm/pnpm on the login-shell PATH; Shika does not install or version-pin a package manager. The saved snapshot is never recomputed by New; the existing approval/config-change fences still apply.

This is convenience for first-time configuration, not permission to execute repository code. Reviewed install commands can run dependency lifecycle scripts with your account's access, under the same setup trust warning as other commands.

You can also create `.shika/worktrees.json` yourself:

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

All fields are optional. `setup-worktree` is an ordered array of nonempty shell-command strings. `copy-files` is an array of literal relative file paths. `timeout-seconds` defaults to 600 and must be between 1 and 3600. The timeout covers copying and all commands together, after a setup slot is available; it excludes the base fetch, checkout creation, and queue wait. Unknown fields are errors. Configuration is limited to 64 KiB, 64 commands, and 128 unique copied files.

The configuration file and its `.shika` directory must not be symlinks. Invalid or unreadable configuration blocks New instead of silently skipping setup.

Commit the configuration if everyone on the project should use it. Never put secret values in commands or configuration. Keep secrets in ignored local files instead. Removing the configuration disables preparation for subsequent launches, without changing running cards.

## Configuration, consent, and onboarding state

Keep these three concepts separate. Treating "reviewed" as "approved", or an unsaved suggestion as configuration, is a security and lifecycle regression.

| State | Location | Meaning |
| --- | --- | --- |
| Executable configuration | `<main checkout>/.shika/worktrees.json` | Only source of copy paths, ordered commands, and timeout; can be shared through Git. Absence means no preparation; `{}` means an explicit empty configuration, not absence. |
| Execution consent | This project's `approvedPreparation` in app-data `projects.json` | Exact parsed configuration the author approved locally. Defaults are normalized; array order and command/path strings still matter. Not a script digest, package-manager pin, or sandbox. |
| Onboarding preference | This project's `worktreeSetupReviewed` in app-data `projects.json` | A setup decision was made. Missing/false is pending unless prior approval exists. True alone authorizes nothing. |
| Unsaved draft | `PreparationDraft` and the in-memory `Editor` | Proposed or edited values plus the expected existing parsed config; discardable without repository writes. |
| Allocated task | App-data `worktrees.json` journal, then in-memory preparation/session state | Ownership and cleanup record. This file is **not** the repository's `.shika/worktrees.json` and contains no setup recipe. |

Default app data is `~/Library/Application Support/com.hieule.shika/`; tests must override it with `--data-dir`. Neither approval nor the reviewed preference belongs in `settings.json` or repository configuration. Copied secret contents are not persisted in the config, approval, or preference.

### Author-New routing

`Shika::request_launch` first checks CLI availability, then loads config in the background. Onboarding requires all of: an ordinary author New (not Retry), missing config, false/missing reviewed preference, and no prior approval. `Core::preparation_onboarding_pending` checks only local history; it does not by itself check config presence or caller role. Do not use it as a permission predicate.

| Current state | Author New does |
| --- | --- |
| Missing config, no decision or approval history | Loads an unsaved draft and opens first-New review, even if no defaults were found. |
| Missing config, reviewed true or any prior approval | Starts through the normal no-preparation path; does not detect defaults again. |
| Valid config, no matching approval | Shows ordinary exact-config approval, regardless of the reviewed preference. |
| Valid config matching approval | Prepares automatically; no onboarding or redundant approval. |
| Existing empty config | Follows the same approval/preparation rules as any valid config; never supplements it with guesses. |
| Invalid/unreadable/unsafe saved config | Reports the error before allocation; never downgrades to suggestions or silently skips it. |

The draft loader rereads config. If a config appeared between the initial missing-file check and draft loading, `request_launch` adopts that existing config and uses ordinary approval instead of onboarding. Lead's own detached worktree bypasses setup by design; Lead workers/direct core launches do not show onboarding, but still cannot execute an unapproved config. See [Lead](lead-agent.md) and [the control CLI](shika-cli.md).

### Writes, migration, and repeat launches

- Preview/open/cancel creates no repository config or decision record. Adding a project persists project registration, not setup consent.
- First-New skip marks only that project's reviewed preference, after rechecking that config is still absent; the UI then reruns launch preflight.
- Settings Save/Disable records reviewed true after the repository mutation, but does not grant new execution consent or start a task. Existing consent is preserved; a changed config still mismatches it.
- Successful approval, including combined first-New save/approval, stores the parsed config and reviewed true together in the local project record.
- Existing project files load without migration: missing preference defaults false and is omitted when false. A prior approval suppresses a redundant notice even if the preference field is absent. Old records without either may get the new optional notice on their next author New.
- Deleting/disabling config does not clear approval history or reset the reviewed preference. Restoring the same parsed config can reuse approval; adding a different config needs approval, not a new detection wizard.
- The saved recipe is never recomputed because package metadata or local files changed. File contents are copied afresh per task, but recipe edits require an explicit save. Installers and referenced script contents are not frozen by consent.

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

## Developing and previewing a task

After preparation and CLI startup, Cmd+T opens an independent shell in that task's worktree. Run the project's dev command, such as `npm run dev`, then Cmd-click or Ctrl-click its localhost URL to open it. The shell stays alive when hidden. Closing the shell stops its processes, not the task; Close task stops all owned PTYs through the normal safety checks.

Shika does not infer a dev command, start a browser/server, assign ports, or manage databases. Install dependencies in finite setup commands; keep long-lived processes in task shell tabs. Two worktrees may still compete for the same port or external service. See [terminal tabs](terminal-tabs.md) for ownership and teardown.

## Failure, cancellation, and retry

A nonzero command stops the sequence. Copy errors, timeout, configuration changes, and cancellation also stop preparation. The agent is not launched into an incomplete environment. The card shows setup stages and output, with Cancel setup while running and Retry setup after failure.

- `⌘⇧W` cancels setup or closes a failed card, from the cards or the terminal. `r` retries a failed setup from the cards. Ctrl+Q returns from the terminal to the cards.
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
| Same module | `draft`, `suggest_installer`, `optional_regular`; `Core::project_preparation_draft` | Settings/first-New bounded metadata suggestions; existing config wins; no persistence/approval/execution |
| Same module | `validate_edit`, `save`, `load_at` | Schema/source validation outside the operation lock, explicit save/disable, stale-config checks, rooted temporary write plus atomic replacement; no execution/approval |
| `crates/shika-core/src/lib.rs` | `save_project_preparation`, `save_and_approve_project_preparation`, `preparation_onboarding_pending`, `skip_preparation_onboarding` | UI-free explicit edits, combined save/consent, and local per-project onboarding preference; no task/command allocation |
| `crates/shika/src/worktree_setup.rs` | `Editor`, `EntryPoint`, `open_worktree_setup`, `show_preparation_onboarding`, `customize_preparation`, `worktree_setup_key`, `worktree_setup_view`, `save_worktree_setup` | Shared native editor, compact first-New review, caller-specific cancel/save, focus traversal, async IO, retained errors |
| `crates/shika/src/main.rs` | `SettingsSection::Projects`, `settings_panel`, `Overlay::WorktreeSetup`, `cancel_overlay`; `worktree_setup.rs`: `on_projects_key`, `finish_worktree_setup` | Settings project rows, keyboard entry, nested return to Projects without consuming Settings's original focus return |
| `crates/shika-core/src/preparation.rs` | `PreparationControl`, `PreparationLimiter`, `PreparationSlot` | Cancellation/preservation flags, registered process group, and two-slot RAII limit |
| Same module | `prepare`, `run_command`, `drain`, `child_exited`, `check_deadline` | Copy-then-command order, shared deadline, bounded pipe reads, exit observation, group stop/reap |
| `crates/shika-core/src/projects.rs` | `Project::approved_preparation`, `worktree_setup_reviewed`, `preparation_onboarding_pending`; `ProjectDb::approve_preparation`, `review_preparation` | Separate local consent and onboarding preference, backward-compatible defaults; neither lives in repository config |
| `crates/shika-core/src/lib.rs` | `project_preparation`, `preparation_approved`, `approve_preparation` | Read-only preflight, parsed-config comparison, recheck before saving approval |
| Same file | `create_session`, `create_session_with_preparation` | Existing entry point delegates to the preparation-aware lifecycle; core enforces consent even without the UI |
| Same file | `Core::preparing`, `cancel_preparations`, `remove_project`, `leftovers_list`, `leftover_remove`, `cancel_session_start` | Pending ownership, quit/removal cancellation, protected leftovers, and abandoned-launch preservation |
| `crates/shika-core/src/error.rs` | `Preparation`, `PreparationNeedsApproval`, `PreparationCancelled` | Failure detail, approval fence, and explicit cancellation outcomes |
| `crates/shika/src/main.rs` | `request_launch`, `Overlay::Preparation`, `approve_preparation` | Async preflight, approval UI, and local consent save |
| Same file | `begin_launch`, `retry_preparation`, `Card::launch_control`, `launch_error`, `stage` | Pane/card creation, background launch, fresh retry, progress and failure UI |
| Same file | `HostState::preparing`, `agent_starting`, `Host::write`, `bind_host` | Setup input gating, early CLI query replies, PTY binding/resizing |
| Same file | `tick`, `close`, `Card::drop`, `stop_cancelled_launch`, `main`'s `on_app_quit` hook | Cancel requests, deferred card removal, dropped UI, and synchronous quit cancellation |

Execution consent is `Project::approved_preparation`, serialized as `approvedPreparation` in `projects.json`. Its value is the parsed config with the same `setup-worktree`, `copy-files`, and `timeout-seconds` keys, not a boolean, hash, script snapshot, or copied-file contents. A missing field means no consent. Separately, `Project::worktree_setup_reviewed` (`worktreeSetupReviewed`, missing/omitted false) remembers the local onboarding choice; it never authorizes commands. Progress, controls, process-group IDs, and failure-card state are memory-only; `settings.json` is unchanged.

`libc` was added directly to `shika-core` for rooted file operations and process-group control; it was already in the workspace lockfile transitively. No provider launch flags or model integration were added.

### Editor lifecycle and save boundaries

For UI work, read `worktree_setup.rs` before expanding `main.rs`. `Overlay::WorktreeSetup` boxes the shared `Editor`, keeping the large editor state out of every overlay variant. Native `NameInput` fields provide editing, clipboard, selection, and composition; do not replace them with terminal input or a second text-entry implementation.

`EntryPoint` carries the caller, not just a visual mode:

| Caller | Entry / initial state | Save / completion | Cancel |
| --- | --- | --- | --- |
| Settings Projects | `open_worktree_setup`; full editor and project row | `save_project_preparation`; return to the same Projects row, no launch/approval | Return to Projects, keeping Settings's original terminal/card focus return |
| First author New | `show_preparation_onboarding`; compact summary with selected preset and picker index | `save_and_approve_project_preparation`; on success, `begin_launch` for the captured project/preset | Restore that project's picker/index without recording a decision; picker cancellation restores the original surface |

Customize changes the existing editor to expanded mode; it does not reload, save, or approve anything. Every async operation captures the project ID and values, not the later card selection. Use that captured identity for saves and launch. A completion for another selected card must not retarget setup.

```text
Preview -> Core::project_preparation_draft -> in-memory Editor only
Settings Save/Disable -> Core::save_project_preparation -> return to Projects
First-New Save/start -> Core::save_and_approve_project_preparation -> begin_launch
First-New Skip -> Core::skip_preparation_onboarding -> request_launch again
Cancel -> finish_worktree_setup -> opening Settings row or picker
```

Both save APIs delegate to `save_preparation_edit`:

1. Validate schema, serialized size, source file types/paths, and source ignore rules outside `Core::operations`. No secret file contents or referenced script contents are read, and no command is executed during validation. Task-base ignore rules remain a launch check.
2. Take the short operation lock, reread the project path, and refuse a changed/removed project.
3. `preparation::save` compares current parsed config with `Editor::expected`, uses rooted directory descriptors, writes/syncs an exclusive temporary file, rechecks, and atomically renames it; Disable unlinks only the config. Expected `None` distinguishes a missing file from an existing empty config. Parsed-equal formatting changes are not stale.
4. Ordinary Settings save marks reviewed without granting consent. Combined save rereads the committed config before storing exact approval/reviewed state. Neither core API allocates or runs a task; only successful UI completion continues launch.

Atomic replacement prevents partial JSON; it is not a transaction spanning repository config and app-data consent, nor a file watcher or script-content pin. Errors retain the draft. A local consent write failure can leave config saved but unapproved; cancel and New again reloads it for ordinary approval. Never launch on that failure or silently overwrite an externally changed preview to make retry easier.

Keyboard/focus guardrails:

- `Editor::focuses` and `rows`, `focus_rows`, and `compact_focus_rows` must match the rendered direct-child order. Update arithmetic and tests when inserting a fact, row, error, or footer control.
- Keep the compact initial summary at the top for review, rather than scrolling past the warning to the focused primary action. Subsequent Tab movement reveals its control.
- Guard both mouse and keyboard save/skip against active IME composition. Escape cancels only outside composition. Held/repeated Enter is ignored so holding Enter to choose a CLI cannot also approve its setup.
- Do not consume `overlay_return_focus` when returning to the caller's picker/Settings. Only the outer dismissal restores it; successful New intentionally focuses the new task.
- Detection, validation, and persistence use background executors. The short busy gate prevents double submit; long preparation later releases it. Keep all new colors, spacing, and motion within [the design specification](../design/DESIGN.md).

### Launch lifecycle and lock boundaries

```text
New / Retry
  -> read main-checkout configuration and local approval
  -> first author New without config and without a prior decision:
       preview unsaved defaults -> save/approve/start, customize, skip, or cancel
  -> existing config if needed: display exact config -> approve or cancel
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
| First-New notice is missing or keeps returning | Check caller (`request_launch` versus Retry/Lead/core), config presence, `worktreeSetupReviewed`, and prior `approvedPreparation`. Cancel does not mark reviewed; successful skip/save/approval does. An already-approved legacy project should not be prompted. |
| Defaults are empty or surprising | Inspect only root `package.json`, `packageManager`, recognized lockfile types/presence, regular-file/symlink status, and ignored `.env`/`.env.local`. Ambiguity suppresses the installer, not necessarily valid file suggestions. A saved config always wins; no recomputation is expected. |
| Setup appears global or targets the wrong project | Compare captured `Editor::project`, the main-checkout config path, and project ID in app data. Settings is shared navigation only. Do not derive save/launch targets from the current card selection or store recipes in global settings. |
| Save succeeded but start failed before a card appeared | Check repository config separately from app-data consent and its write permissions. Combined save can leave an unapproved file; follow the retained error's cancel/New instruction. The core must not allocate or run setup on consent-write failure. |
| Skip fails with changed configuration | A file appeared after the missing-config preview. `skip_preparation_onboarding` deliberately refuses it; cancel and New again for fresh ordinary approval, not a temporary bypass. |
| Cancel restores the wrong surface or Tab jumps offscreen | Check `EntryPoint`, captured project/picker index or Settings row, `overlay_return_focus`, and direct-child focus-row arithmetic. Use long configs/minimum windows; arithmetic unit tests are not native acceptance. |
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
| Fixture roots collide or disappear during parallel tests | `Fixture::new` now reserves an exclusive temp root using `create_dir`, PID/timestamp/counter, and collision retries. The former timestamp-only `create_dir_all` allocator allowed shared roots; never reintroduce it. Check other fixture allocators if a similar failure appears. |

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

### Reproducing first-New onboarding

The smoke recipe above starts with an existing config and therefore tests ordinary approval, **not** first-New onboarding. Use a new isolated data directory with no prior decision/approval, and omit the `.shika` creation/config block, for the missing-config path:

```sh
fixture=$(mktemp -d "${TMPDIR:-/tmp}/shika-first-new-docs-XXXXXX")
fixture=$(cd "$fixture" && pwd -P)
repo="$fixture/repo"
mkdir -p "$repo" "$fixture/data"
git -C "$repo" init -b main
git -C "$repo" config user.name Test
git -C "$repo" config user.email test@invalid.example
git -C "$repo" config commit.gpgsign false
git -C "$repo" config core.hooksPath /dev/null
printf '%s\n' '.env.local' '*.generated' > "$repo/.gitignore"
printf '%s\n' fixture-only > "$repo/.env.local"
printf '%s\n' original > "$repo/tracked.txt"
git -C "$repo" add .
git -C "$repo" commit -m 'Disposable first-New fixture'
open -n target/debug/Shika.app --args \
  --data-dir "$fixture/data" --diagnostics-file "$fixture/diagnostics"
printf 'Add this disposable project: %s\n' "$repo"
```

This copy-only fixture requires no installer or model call. Build the debug bundle first as above; a CLI must be available to reach review. Its own trust/authentication prompt is independent.

1. Add the printed repo, choose a CLI with New, and confirm the unsaved `.env.local` suggestion and main-checkout config path. There must be no task worktree before approval/skip.
2. Cancel to the same picker, then cancel the picker. Confirm no `.shika/worktrees.json`, local decision, or allocated task. New again should still offer review.
3. Customize and add `test -f .env.local && printf prepared > ready.generated`, then Save, approve and start. Check independent copied contents and the marker before CLI startup; no second approval is expected for those exact saved values. Send no model prompt for this check.
4. New again should reuse the recipe/approval in a fresh tree. Edit config and check ordinary reapproval; formatting-only changes should not ask again.
5. Use another **fresh fixture/data pair** to test Start without setup, relaunch persistence, no copied file/config, and later configuration through Settings. Adding a second project must not inherit the first project's choice.
6. For installer suggestions, add root npm/pnpm metadata to a disposable repo before review. If you intend to run the installer, commit the actual package/lock files to its task base; a metadata-only fake lock proves detection, not successful installation. Test conflicts and unsupported managers as well.

Quit only the isolated test instance and stop its task processes before deleting its fixture. Do not reset normal app data to force a notice. Keep GUI results in [MANUAL_CHECKS.md](../MANUAL_CHECKS.md#first-new-per-project-setup-onboarding), not inferred from this recipe.

### Tests, acceptance, and safe extension

```sh
source "$HOME/.cargo/env"
cargo test -p shika-core preparation
cargo test -p shika-core projects::tests
cargo test -p shika worktree_setup
cargo test -p shika setup_input_is_not_queued_as_a_future_agent_prompt
cargo test -p shika prepared_agent_startup_preserves_query_replies_but_not_setup_input
cargo fmt --all --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Preparation fixtures now allocate roots atomically and exclusively; earlier timestamp-only roots could collide in parallel. Keep the standard parallel workspace run in PR validation. A serial rerun can diagnose unrelated concurrency failures, but cannot establish their resolution.

`crates/shika-core/src/preparation_tests.rs` uses disposable repositories, isolated data, and a fake CLI; it does not call a model. Runner/copier/limiter unit tests also live in `preparation.rs`. The app host tests distinguish setup input suppression from startup reply handling. Useful regression anchors:

| Concern | Existing tests to extend |
| --- | --- |
| Consent and rechecks | `consent_persists_but_changed_configuration_requires_new_consent`, `changed_configuration_during_setup_never_launches_an_agent` |
| Copy safety | `local_files_are_independent_and_commands_finish_before_agent_launch`, `destination_files_and_symlinks_are_never_overwritten_or_followed`, `a_file_ignored_only_in_the_source_is_not_copied_into_the_task` |
| Config bounds/type | `malformed_unknown_and_unbounded_configuration_is_rejected`, `oversized_and_nonregular_configuration_is_rejected_without_blocking` |
| Suggested defaults | `setup_suggestions_are_unsaved_and_never_used_by_new`, `setup_suggestions_preserve_existing_configuration_even_when_empty`, `setup_suggestions_only_include_root_regular_ignored_dotenv_files`, `setup_suggestions_use_unambiguous_npm_and_pnpm_metadata`, `setup_suggestions_skip_conflicts_unsupported_managers_and_unknown_toolchains`, `setup_suggestions_skip_invalid_unbounded_and_symlinked_package_metadata` |
| First-New onboarding | `preparation_onboarding_preview_leaves_repo_and_local_preferences_unchanged`, `preparation_onboarding_skip_is_per_project_persistent_and_not_consent`, `preparation_onboarding_skip_refuses_a_configuration_added_since_preview`, `preparation_onboarding_save_and_approve_starts_only_at_normal_launch`, `preparation_onboarding_save_and_approve_rejects_invalid_or_stale_drafts`, `preparation_onboarding_approval_write_failure_never_runs_saved_commands` |
| Setup editor persistence/safety | `setup_editor_creates_configuration_without_running_or_approving_it`, `setup_editor_empty_template_and_formatting_edits_do_not_infer_setup_or_consent`, `setup_editor_refuses_stale_invalid_and_unsafe_saves`, `setup_editor_never_follows_config_or_copy_source_links` |
| Preference migration and editor focus | `legacy_projects_offer_setup_unless_already_approved`, `reviewed_setup_survives_reopen_without_granting_execution_consent`, `compact_onboarding_focus_matches_empty_and_suggested_summary_rows`, `customized_onboarding_adds_skip_in_the_existing_footer`, `long_setups_keep_focus_targets_ordered_and_repair_after_row_removal` |
| Rollback versus preservation | `failed_command_stops_sequence_never_launches_agent_and_cleans_untouched_tree`, `failure_preserves_tracked_edits_untracked_files_and_commits_in_leftovers`, `a_completed_launch_abandoned_by_its_ui_retains_the_journaled_worktree` |
| Cancellation/process ownership | `cancellation_kills_setup_descendants_and_removes_clean_pending_worktree`, `background_descendants_are_stopped_before_the_agent_starts`, `shutdown_preserves_pending_worktree_and_stops_its_setup` |
| Concurrency | `two_setup_slots_bound_concurrency_and_waiting_is_cancellable`, `slow_setup_does_not_hold_operation_lock_or_block_another_session` |

The original engine's 191-test validation and native smoke are historical. The Settings/suggestion follow-up passed 477 tests; first-New onboarding passed 487 (172 app, 248 core, 67 terminal; one existing ignored), formatting, strict workspace/all-targets Clippy, debug bundling, and strict signature verification. Six onboarding integration tests also passed five repeated parallel runs. These are recorded baselines, not a guarantee about a later checkout.

The initial onboarding test run reproduced the old timestamp-only fixture collision. `Fixture::new` now reserves its temp root with exclusive `create_dir`, PID/timestamp/counter, and collision retries. Never revert to `create_dir_all` on a guessed name: two tests can adopt the same root and one teardown can delete the other's repository.

Native engine smoke did not exercise a real dependency install or model task. The newer editor/onboarding interaction was blocked by macOS Accessibility/Screen Recording permissions; opening an isolated bundle establishes neither rendered correctness nor focus behavior. Default/held Enter, clipboard/IME, mouse controls, scrolling, themes/minimum size, actual npm/pnpm installs, all providers, and the dev-server/browser workflow remain explicit checks in [MANUAL_CHECKS.md](../MANUAL_CHECKS.md#first-new-per-project-setup-onboarding).

Before extending the feature:

1. Preserve opt-in/no-config execution behavior, optional per-project onboarding, local consent, fresh task branches, and unchanged provider argv. No automatic setup on project add or implicit config import.
2. Keep journaling ahead of setup and retain work whenever safety cannot be proved. Do not equate ignored output with valuable-work preservation.
3. Keep long work outside core/UI global locks. Preserve hidden-pane draining, startup sizing, identity-based completion, and input-phase separation.
4. Add failure/race tests with temporary repositories and a fake CLI before adding native acceptance claims. Use the fixture for UI checks, never normal app data.
5. Treat schema changes as consent changes: add backward-compatible defaults only deliberately, keep unknown-field rejection, and update parsing/approval tests. The approval record contains the parsed config, not a versioned script digest. Consider whether deserializing a new field's default into old approval records would silently authorize new behavior; specify migration/reconsent rather than assuming equality is sufficient.
6. Globs/directories, daemon support, persistent setup logs, pooling, and compiler caches need separate design decisions. Do not quietly broaden trust or ownership boundaries as an optimization.
7. Extending suggestions requires bounded, root-scoped metadata rules, fixed inspectable commands, conflict/unsafe-metadata tests, and no implicit execution. Supporting another manager or monorepo layout must not supplement existing config, scan secret contents, invoke a manager during detection, or turn skip into a permission grant.
8. Editing/onboarding changes must preserve caller-specific focus/cancel behavior, expected parsed snapshots, distinct reviewed/approved fields, and the no-allocation-on-save/consent-failure boundary. Test existing/empty configs and legacy records as well as the happy path.
9. Update this guide, the short [AGENTS.md](../AGENTS.md) handoff, and relevant acceptance checks together. For UI changes, read [design/DESIGN.md](../design/DESIGN.md) and [keyboard flow](keyboard-flow.md). Keep intended behavior separate from verified GUI results.
