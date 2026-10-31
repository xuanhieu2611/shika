# Confirmed PR publishing

Contributor and coding-agent guide for Shika's **Create PR** feature: what changed, why, how the implementation works, and how to debug or extend it safely.

Start with [AGENTS.md](../AGENTS.md) for architecture and build constraints. This guide records the product decision: publishing is the only flow in which Shika commits for the user, and only when the author asked for it: by confirming Create PR, or by asking their Lead to publish a worker (`shika pr`, which has no dialog; see [Publishing from a Lead](#publishing-from-a-lead)). Read [design/DESIGN.md](../design/DESIGN.md) before UI changes, [keyboard-flow.md](keyboard-flow.md) for dispatch/focus, and [terminal-tabs.md](terminal-tabs.md) for task/PTY ownership. [MANUAL_CHECKS.md](../MANUAL_CHECKS.md#confirmed-pr-publishing) separates validation evidence from remaining acceptance checks.

## What changed

Previously, Shika created a task branch and worktree, then left staging, commits, pushes, and PR creation to the developer's shell and GitHub. Close could push already-committed work, but never commit it.

Create PR adds a separate, explicitly confirmed workflow:

```text
Review and test in the task shell
    → Create PR
    → preview files, source, target, and title
    → confirm
    → stage → commit if needed → push → create or reuse PR
    → open PR URL; keep task and terminals alive
```

Added:

- A metadata-row control, Agent-menu action, and Cmd+Shift+P binding.
- A confirmation dialog with an editable commit/PR title, an optional description, and a target branch.
- A core preview API and publishing API backed by Git and GitHub CLI subprocesses.
- Recorded-base targeting, approved-tree checks, bounded commands, and partial-success recovery.
- Disposable-repository tests with fake GitHub CLI responses and local bare transport.

Unchanged:

- Close never commits; its discard/push choices and cleanup rules still apply.
- Publishing does not merge, force-push, run tests, or remove a card/worktree.
- After publishing, the card shows a read-only checks mark for that PR (see [PR checks on the card](pr-checks.md)). Shika never re-runs, cancels, or lists checks.
- Shika does not install tools, authenticate on the developer's behalf, store credentials, or call a model API.
- Terminal tabs, PTY ownership, task naming, and session persistence are not expanded. Publishing does not inject commands into a running shell or agent terminal.

## Why these decisions

| Decision | Reason |
| --- | --- |
| Use GitHub CLI rather than an embedded GitHub client | Reuse the developer's existing authentication and tooling without adding accounts, token storage, or a second GitHub integration stack. |
| Target the task's original base | A feature started from `dev` normally belongs in `dev`; GitHub's default branch or a later project-setting change must not redirect it to `main`. |
| Require confirmation before staging/committing | Publishing changes local Git state and makes work remote. The developer must see the files and destination before those effects. |
| Stage the entire task worktree | The action completes the common stage/commit/push/PR loop. It does not attempt to infer which files were AI-authored or belong to a feature. |
| Use an editable task-derived title and an optional description; empty keeps the commit-derived body | Avoid extra model calls, dependencies, and generated claims about testing. Keep text predictable and under developer control. |
| Keep merge separate | PR review, branch protections, CI, and release policy belong to the repository's workflow. Creating a PR is not approval to merge. |
| Preserve completed steps on failure | Git and GitHub are not one transaction. Automatic reset or deletion could destroy valid work or undo a successful remote operation. |
| Use subprocesses, not shell input injection | A user's terminal might be running an editor, server, or agent. Publishing must not depend on its foreground program or input state. |

Review and testing are prerequisites performed by the developer, not facts Shika verifies. The confirmation is not a code review, secret scan, or security sandbox.

## User-facing contract

GitHub CLI must be installed and authenticated, normally through `gh auth login` in a shell. Git identity and any signing/SSH configuration must also work without interactive input during publishing.

Create PR is available in the terminal metadata row, the Agent menu, and Cmd+Shift+P. Entry is blocked while the app is busy, an overlay is open, setup is running, or the task has an active/blocked agent turn. The active-turn check runs again at confirmation.

The dialog displays:

- The GitHub repository and the source branch. The target is the field below, not repeated on this line.
- Changed file names relative to the task's current HEAD. None reads "Nothing new to commit".
- A title initially copied from the task title, including a manual [Rename task](task-names.md) name, used for both a new commit and a new PR. Renaming alone creates no commit and rewrites no existing commit or PR; the editable title here still requires confirmation.
- An optional description. Empty leaves the PR body to `gh --fill` (the commit messages). Text written here is the body. Enter inserts a line; Cmd+Enter confirms.
- An editable target field with matching existing GitHub branches.

A Lead publishes a worker it started with `shika pr <task>` without this dialog (see [Publishing from a Lead](#publishing-from-a-lead)). While it runs, Create PR on that card is refused with "The Lead is publishing this task. Try again when it finishes."

Tab switches the title, description, and target; clicking selects a field or branch. Enter confirms from the title or target, and inserts a line in the description. Cmd+Enter confirms from the description. The **Commit, push, create PR** button confirms from any field. Escape or Cancel restores the previous focus before publishing begins. While publishing, the action reads **Publishing...** and dismissal is blocked.

**The file list is the proposed new commit, not the entire PR diff.** Already-committed changes are not listed there. A clean worktree reads "Nothing new to commit" and can still publish existing commits without creating another commit. Selective staging is not preserved as a publishing boundary: confirmation approves `git add -A` for the task root. Use the shell for partial commits.

On success, Shika opens the returned PR URL, restores focus, and keeps the card, branch, worktree, agent, and shell PTYs intact. Reusing an existing PR pushes approved work and opens that PR; it does not rewrite the existing PR title or body.

## Publishing from a Lead

**What changed.** Until protocol 6, a Lead's `shika pr` opened this dialog for the author and waited. Since 2026-10-17 it publishes directly: no dialog, no Enter, and the Lead prints the PR URL. The author decided it: "I want my leader to open the PR for me, and just give me the response, instead of me checking the PR title, description, etc., then pressing enter, then seeing the response." Asking the Lead is the confirmation, so the second confirmation in the dialog was removed for this one caller. The author's own Create PR keeps its dialog. The command reference is [shika-cli.md](shika-cli.md#shika-pr-task---title-text---body-text); the decision, flow, and debugging live in [lead-agent.md](lead-agent.md#pr-publishes-directly).

**Same pipeline.** `Core::session_publish_now` takes the operation lock once and runs `publish::publish_now`: `preview`, then `publish` with the preview's own values, exactly as if the author opened the dialog and confirmed it untouched. Every recheck below applies. The differences are only where the dialog would have asked:

| The dialog | `shika pr` |
| --- | --- |
| Target prefilled from the recorded base; the author may choose another | The recorded base only. No target is `Error::PublishNoTarget`, a refusal that tells the author to use Create PR. Never a fallback branch. |
| Title prefilled from the task title, editable | The task title, or `--title` (one line). |
| Description empty uses `gh --fill`; text written in the dialog is `--body` | `gh --fill`, or `--body` (up to `MAX_PR_BODY_CHARS`, 65,536), passed as `gh pr create --body` instead of `--fill`. |
| Opens the PR URL in the browser, restores focus | Prints the URL for the Lead. No browser, selection, focus, or toast change. |
| Global `busy` while running | `Card::lead_publishing` for that card only; the author keeps working. Create PR, Close, and Switch agent on that card wait for it. |

A reused PR keeps its title and body; `--title` and `--body` only shape a new commit and a new PR.

## Target and repository identity

### Task base, not project base

`Session::base_ref` already records where the task started. Publishing reuses it; no new persistence field is introduced.

| Recorded ref | Initial PR target |
| --- | --- |
| `refs/remotes/origin/main` | `main` |
| `refs/heads/dev` | `dev` |
| `refs/remotes/origin/release/v2` | `release/v2` |
| Missing ref, symbolic `origin/HEAD`, or a ref under another remote | Explicit selection required |
| Recognized name absent from GitHub's branch list | Explicit selection required |

The mapping lives in `base_name`. The resulting name must exist in the explicit GitHub repository. Changing the project's base after task creation never changes this default. A developer may explicitly choose another existing target in the dialog, but cannot select the source branch itself.

Do not reuse diff-stat or Close fallback resolution here. Those features may fall back when refs disappear; silently changing a PR destination is a different and unsafe effect.

### Explicit origin

`origin` is both the push destination and the PR repository. `repository` normalizes ordinary GitHub HTTPS and Git SSH URLs to `HOST/OWNER/REPO`; GitHub Enterprise hosts use the same explicit host for API requests. This is deliberately a constrained parser, not general URL support. Ports, credential-bearing URLs, local/file remotes, and ambiguous layouts are not supported. The parser also accepts ordinary HTTP URLs; secure HTTPS or SSH remains the expected configuration.

`origin` reads the fetch URL and all push URLs. Exactly one push URL must identify the same repository as the fetch URL. Multiple destinations or a fetch/push repository mismatch are refused. Fork-to-upstream publishing is not implemented; do not guess an upstream repository.

Every relevant `gh` call specifies the repository or API hostname. `GH_REPO` is removed from subprocesses so an environment default cannot redirect the task.

Existing PR lookup filters by open state, head branch, and target, then checks `headRepositoryOwner` and `headRepository`. Branch-name matching alone is insufficient: a fork can have the same branch name. Only a PR from the intended source repository is reused.

## Implementation and lifecycle

```text
Shika::create_pr
  → background Core::session_publish_preview(session_id)
      → operation lock + task-branch ownership check + login-PATH gh resolution
      → publish::preview
  → Overlay::Publish holds preview and editable title/target

Shika::publish_pr
  → validate fields and active-turn state
  → background Core::session_publish(preview, target, title)
      → operation lock + task-branch ownership check
      → publish::publish
  → success: dismiss, restore focus, open URL
    failure: keep dialog and report retained work

Shika::control_pr (a Lead's shika pr; no dialog)
  → refusals, then Card::lead_publishing
  → background Core::session_publish_now(session_id, title, body)
      → operation lock (once) + task-branch ownership check
      → publish::publish_now: preview, recorded target or PublishNoTarget, publish
  → reply URL, refusal, or error to the Lead; start the checks watch
```

### Preview without changing the real index

`PublishPreview` stores display data plus the session ID, source branch, repository, original HEAD commit, proposed tree ID, and origin URL snapshot. The UI owns the editable title and target separately.

`candidate` constructs the proposed tree with a temporary `GIT_INDEX_FILE`:

1. `git read-tree HEAD` seeds the disposable index.
2. `git add -A -- .` stages the task root into that index.
3. `git write-tree` returns its tree ID.
4. `Index::drop` removes the temporary index and lock file.

Preview diffs that tree against the captured HEAD to list changed paths. NUL-delimited Git output avoids treating spaces or newlines in paths as record separators.

The real staging area is untouched by preview, but preview is **not completely side-effect-free**: Git can create blob/tree objects and invoke configured clean filters. It can also contact GitHub to verify the repository and list branches. No commit, push, or PR creation occurs before confirmation.

### Confirmed publishing sequence

`publish::publish`:

1. Refuses conflicts, unfinished merge/rebase/cherry-pick/revert state, and dirty content inside submodules. A committed submodule pointer change can be staged; Shika does not commit files inside submodules.
2. Validates a nonempty, single-line title, an explicit target from the preview's branch list, and a description of at most `MAX_PR_BODY_CHARS` characters. Empty or whitespace-only description keeps `gh --fill`.
3. Rechecks source branch, origin identity, HEAD, and candidate tree against the preview. A mismatch requires cancellation and a fresh preview.
4. Rechecks the target on GitHub, then fetches only that target into `refs/remotes/origin/TARGET`. This supplies the local base ref used by `gh --fill`; it never switches a checkout. The `+` in the fetch refspec updates a local remote-tracking ref, **not** a force-push.
5. Looks up matching open PRs before creating local commits.
6. If HEAD's tree differs from the approved tree, stages the real index, verifies its tree ID and source/HEAD again, and runs `git commit -m TITLE`. Normal identity, signing, and hooks apply. No author/co-author is supplied and no hook is bypassed.
7. Verifies the committed and working trees still match approval. A hook that changes the committed tree stops the flow before push, retaining the commit locally.
8. Captures an approved commit SHA, verifies there are task commits ahead of the selected target, and rechecks origin. Pushes `SHA:refs/heads/TASK` to origin without force, then establishes the task branch's upstream explicitly.
9. Reuses a matching PR or runs:

   ```text
   gh pr create --repo HOST/OWNER/REPO --head TASK --base TARGET --title TITLE --fill
   ```

   The explicit title overrides `--fill`'s generated title. An empty description comes from the commits via `--fill`. A description written in the dialog, or a Lead's `--body`, replaces `--fill` rather than joining it, so gh never has to reconcile the two. Explicit `--head` avoids gh choosing a push/fork destination.
10. Returns the existing/new PR URL. A new URL must be a single-line HTTPS value.

Pushing a **pinned SHA**, not a moving `HEAD`, prevents a concurrent later commit from hitching a ride after approval. This is not an atomic lock on the worktree or Git configuration. External shells, hooks, and other processes still require defensive rechecks and can create races.

### Threads, process environment, and ownership

All three core publishing APIs are blocking and must run on a background executor. The UI's global busy guard prevents conflicting app actions during the author's Create PR; a Lead's `shika pr` uses the per-card `Card::lead_publishing` instead, so the author's navigation is never frozen by work they did not start. The core operation lock serializes publishing with app-managed branch/lifecycle mutations; it can be held across network requests. Neither guard prevents shell edits or enforces repository-wide exclusivity against external Git processes.

`run_with_timeout` provides:

- Null stdin and disabled Git/GitHub interactive prompts.
- `ssh -oBatchMode=yes` through `GIT_SSH_COMMAND`.
- A separate process group, killed on timeout.
- Concurrent stdout/stderr draining, retaining up to 4 MiB per stream.
- A 120-second limit per subprocess, including output collection, not one limit for the whole workflow.
- The first nonempty stderr line as the command-failure detail.

The login-shell PATH is reused for Git and gh. Git-redirection environment variables are removed; the disposable-index path is the deliberate exception. The runner overrides custom `GIT_SSH_COMMAND` values, so standard SSH configuration is the supported route for identity/host configuration. Hooks, signing, and authentication that require terminal interaction can fail or time out; do not solve that by silently bypassing them.

Publishing creates no PTY and owns no terminal tab. The preview/dialog are memory-only, with no durable publishing job or automatic resume after relaunch. Quit or a lost network response can leave an uncertain remote result; inspect Git/GitHub before retrying.

## Checks on the card

After a successful publish, the card shows a read-only checks mark for the PR that was created or reused, such as `#42` with a ring, a check, or a red cross, and posts one "Checks failed" notification when it turns red. `session_publish` returns a `PublishedPr` (URL, repository, number, pushed commit) for this. The design, schedule, and debugging live in [PR checks on the card](pr-checks.md).

## Failures and recovery

There is no automatic rollback across Git and GitHub.

| Failure point | What remains | Recovery |
| --- | --- | --- |
| Missing gh, authentication, unsupported origin, invalid target | No publishing commit/push; preview may have created Git objects | Fix configuration in a shell and reopen Create PR. |
| Stale preview before real staging | Existing work and index remain | Cancel, inspect the diff, reopen to approve a fresh tree. |
| Files change during staging or commit hook fails | Real index may be staged; files remain | Inspect `git status` and staged diff; fix the cause and reopen. |
| Hook changes the committed tree or post-commit validation fails | Local commit remains; no push from this flow yet | Review the commit and worktree before retrying. |
| Push rejected, network failure, or signing/auth timeout | Local commit remains; remote success may be uncertain | Inspect local/remote refs. Never automatically reset or force-push. |
| Upstream setup or PR creation fails after push | Pushed task branch remains | Check GitHub and branch state, then reopen Create PR. |
| PR succeeds remotely but URL/response is lost | PR may already exist | Check GitHub; a fresh lookup can reuse it. |

Cancel and reopen after a publishing failure. The same snapshot may be stale because the flow already created a commit. A fresh preview skips committing when the approved tree already equals HEAD, and reuses an existing matching open PR. This is retry-friendly behavior, not an exactly-once guarantee: another client can create a PR between lookup and creation.

Never reset, delete, amend, force-push, or remove a worktree automatically to hide partial failure.

## Code map

Use symbols rather than line numbers, which change frequently.

| File / symbols | Responsibility |
| --- | --- |
| `crates/shika-core/src/publish.rs`: `PublishPreview`, `preview`, `candidate`, `Index` | Snapshot, temporary index, file list, and proposed tree. |
| Same file: `repository`, `origin`, `base_name` | Explicit repository and recorded-base mapping. |
| Same file: `ensure_not_integrating`, `dirty_submodule` | Refuse unfinished integration and uncommitted submodule content. |
| Same file: `publish`, `PullRequest` | Revalidation, stage/commit/push, source-qualified PR reuse, and creation. |
| Same file: `run`, `run_with_timeout`, `git`, `gh` | Bounded subprocesses and child environment. |
| Same file: `publish_now`, `MAX_PR_BODY_CHARS` | A Lead's one-step publish with the dialog's defaults, and the body limit. |
| `crates/shika-core/src/lib.rs`: `session_publish_preview`, `session_publish`, `session_publish_now`, `ensure_session_branch` | Public API, operation locking, CLI resolution, task ownership, and supported external renames. |
| `crates/shika-core/src/error.rs`: `Error::Publish`, `Error::TaskBranchChanged`, `Error::PublishNoTarget` | Publishing details, a distinct task-branch mismatch, and a Lead publish with no recorded target. |
| `crates/shika/src/control.rs`: `Shika::control_pr`, `pr_refusal`, `pr_reply`, `LEAD_PUBLISHING`; `Card::lead_publishing` in `main.rs` | A Lead's `shika pr`: refusals, the per-card in-flight mark, and the reply. |
| `crates/shika/src/publish.rs`: `create_pr`, `publish_pr`; `main.rs`: `CreatePr`, `Overlay::Publish` | Action dispatch, workers, validation, dialog state, and completion. |
| `terminal_side.rs`: `terminal_side`; `overlay.rs`: `overlay_view`, `cancel_overlay`; `keys.rs`: `key`; `focus.rs`: `restore_overlay_focus`; `main.rs`: `main` | Control/menu/binding registration, field editing, focus, and busy dismissal. |
| `crates/shika-core/src/session.rs`: `Session::base_ref`; `worktree.rs`: base resolution and `JournalEntry` | Existing starting-base ownership/persistence; no new schema for publishing. |

## Debugging playbook

Run these read-only commands from the **task worktree**, not the main checkout:

```sh
git status --porcelain=v2
git branch --show-current
git rev-parse HEAD
git diff
git diff --cached
git remote get-url origin
git remote get-url --push --all origin
gh auth status
```

Do not attach tokens, credential-bearing remote URLs, private file contents, or personal paths to public issues. Redact command output and use generic examples.

| Symptom | Inspect first |
| --- | --- |
| Create PR does nothing | `create_pr` busy/overlay guard, selection/session availability, setup, and `Card::running`; binding/action registration if only the shortcut fails. |
| Create PR says "The Lead is publishing this task." | A Lead's `shika pr` on that card has not finished (`Card::lead_publishing`). It clears when the Lead's command gets its reply. |
| A Lead's `shika pr` is refused for having no PR target | The task's `Session::base_ref` maps to no branch on GitHub. Publish with Create PR and choose a target; the Lead cannot. |
| `unknown flag: --repo` as soon as Create PR opens | Preview's auth check is `gh repo view HOST/OWNER/REPO --json nameWithOwner`. `repo view` takes the repository as an argument and rejects `--repo` (gh 2.102.0). `pr list`, `pr create`, and `pr view` still use `--repo`. |
| It works in a terminal but not from the Dock | `Core::path_env`, `PathEnv::resolve("gh")`, and the login-shell PATH. A shell's `which gh` alone does not prove GUI discovery. |
| Target is blank or seems wrong | The task's `Session::base_ref`, `base_name`, and GitHub branch list. Do not “fix” it by reading current project settings or `origin/HEAD`. |
| The files list is empty despite a substantial PR | Files are compared with task HEAD, not the base. Inspect commits separately; this can be expected. |
| A commit is repeatedly refused as stale | Compare HEAD, actual source branch, origin URLs, filters, and candidate tree; check shell/agent edits and automatic title/branch renames. |
| Changes appear staged after an error | Staging occurred after confirmation. Inspect the cached diff; do not roll it back automatically. |
| Permission/authentication fails | Read the inline error, test authenticated gh and normal Git access in the shell, and check noninteractive SSH/signing requirements. |
| `gh --fill` cannot resolve its base | Target fetch and `refs/remotes/origin/TARGET`; explicit targets may not have existed locally before publishing. |
| Push succeeded but no PR is visible | Check the intended repository/head/base, gh failure detail, and existing PR lookup. Do not create another commit to retry. |
| An unrelated fork PR is opened | Verify lookup requests head repository/owner metadata and source-repository matching, not only branch name. |
| Close appears to publish/commit | Publishing must remain separate from `session_push_and_close` and task teardown. |
| The app appears stuck | Distinguish slow command, operation-lock contention, and output-pipe timeout. The limit is per command; inspect `run_with_timeout`. |

## Validation and isolated reproduction

From the repository root:

```sh
source "$HOME/.cargo/env"
cargo test -p shika-core publish::tests
cargo fmt --all --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
./scripts/bundle-app.sh --debug
codesign --verify --deep --strict target/debug/Shika.app
```

`publish::tests::Fixture` uses disposable Git repositories/worktrees, a local bare remote, a Git wrapper for local fetch/push transport, and a fake gh script that records calls. Staging, trees, commits, and refs are real; GitHub responses are simulated. Start with this fixture when adding cases. A plain local origin is intentionally unsupported by production mapping, so tests substitute transport rather than weakening that rule.

Current tests cover original main/dev mapping, missing targets, index preservation, ignored additions, stale trees, switched source branches, changed origins, real commit/push behavior, PR reuse excluding forks, hook failures, retry without duplicate commits, integration-state refusal, dirty-submodule parsing, and subprocess timeout. Some are parser/unit checks, not complete native or real-provider integration scenarios.

For GUI checks, use disposable repositories and an isolated data directory:

```sh
open -n target/debug/Shika.app --args --data-dir /absolute/path/to/disposable/data
```

Verify that the isolated process's window is frontmost before synthetic input. A real GitHub end-to-end test must use a purpose-created test repository and authorized credentials, never an existing personal or production project. Network failures, protected branches, signing, light/dark and translucent rendering, focus restoration, and real gh responses need integrated checks. Keep results in [MANUAL_CHECKS.md](../MANUAL_CHECKS.md#confirmed-pr-publishing), not implied by unit-test success.

## Contributor guardrails and extension points

- Keep the original task base authoritative. Any alternative target must be an explicit selection validated in the intended repository.
- Preserve confirmation and approved-tree/HEAD/origin/source checks. Do not stage the real index while merely opening a dialog. The only publish without the dialog is a Lead's `shika pr`, which the author chose; it must go through `session_publish_now`, keep every recheck, and never choose a target. Do not add another dialog-free caller without the author's decision.
- Keep publishing independent of Close, PTY/tab ownership, activity detection, and task cleanup.
- Keep blocking commands off the UI thread. Do not remove the busy/operation guards without a stable-identity and concurrency design.
- Preserve user identity, hooks, signing, ignored-file rules, and non-force pushes. Add no agent attribution.
- Preserve partial success and source-qualified PR reuse. Do not present the workflow as an atomic transaction or exactly-once operation.
- Reuse existing design tokens and focus restoration; keep plain field typing out of card/terminal shortcuts.
- Do not install gh, edit global configuration, or collect credentials as a workaround.
- Add regression tests for the behavior you change; record GUI evidence separately.
- Update this guide, the short `AGENTS.md` handoff, and affected design/keyboard/acceptance docs with behavioral changes.

Potential improvements include a preview of the generated commit-message body, clearer completed-step progress, more complete URL/SSH configuration support, and cancellation or concurrent publishing. Each must preserve the safety contract. Fork/upstream selection needs distinct source/target repository identities; persistent jobs need reconciliation with uncertain remote results; merging or CI/review management needs a separate product decision. None is implemented by this feature. The checks mark is read-only; keep it that way, and keep its GitHub reads bounded to running or failed checks on PRs Shika published.
