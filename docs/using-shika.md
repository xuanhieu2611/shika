# Using Shika

The user guide: what Shika does on your machine, every shortcut, and the everyday workflows. Each section links to the feature guide that holds the full rules. The [website guide](https://useshika.com/guide/) is the illustrated walkthrough.

> [!NOTE]
> Shika is early-stage, macOS-only software. Bugs and incomplete workflows are tracked in [MANUAL_CHECKS.md](../MANUAL_CHECKS.md); a passing test suite does not establish every GUI check.

## How it works

1. **Add a project.** Press Cmd+O and pick a Git repository. A nested folder becomes the repository root.
2. **Start an agent.** Press Cmd+N and pick a CLI. Shika creates a fresh branch and worktree under that repository's `.worktrees/`, started from the project's base branch (Cmd+Shift+B changes it).
3. **Write your prompt** in the terminal. The first prompt names the task and branch.
4. **Switch between tasks** with `j` / `k`. Enter focuses the terminal; Ctrl+Q returns to the cards. Ready means the agent finished its turn, so check the terminal for its result.
5. **Review and ship.** Press Cmd+T for a shell in that task's worktree. Test there, then commit and push yourself, or press Cmd+Shift+P to create a PR.
6. **Close the task** with Cmd+Shift+W.

One terminal is visible at a time; hidden task terminals keep running. Projects persist, live sessions do not restore (a project's Lead comes back resting, ready to resume), and quitting leaves worktrees for explicit cleanup on the next launch. App data lives in `~/Library/Application Support/com.hieule.shika/`. See [tasks and worktrees](tasks-and-worktrees.md) for the full lifecycle.

## Keyboard shortcuts

On the cards (the column has focus):

| Keys | Action |
| --- | --- |
| `j` / `k` or the arrows | Move between cards |
| Enter | Focus the selected terminal, or resume a resting Lead |
| Right-click a card | Rename task or Close task |

From anywhere:

| Keys | Action |
| --- | --- |
| Ctrl+Q | Return from the terminal to the cards |
| Cmd+O | Add a project |
| Cmd+N | New agent in the selected project |
| Cmd+Shift+N | New Agent in Checkout, or focus its existing checkout card |
| Cmd+L | New Lead for the selected project, or resume its saved Lead |
| Cmd+] / Cmd+[ | Next / previous agent |
| Cmd+T / Cmd+W | Add a shell tab / close the selected shell tab |
| Ctrl+Tab / Ctrl+Shift+Tab | Next / previous tab |
| Cmd+1 to Cmd+9 | Jump to a tab (Cmd+1 is the agent) |
| Cmd+Shift+P | Create PR |
| Cmd+Shift+A | Switch agent: start another CLI in the same worktree |
| Cmd+Shift+B | Change the project's base branch |
| Cmd+Shift+R | Rename the task (not its Git branch) |
| Cmd+Shift+W | Close the task |
| Cmd+B | Hide or show the column |
| Cmd+Option+B | Open or close the Changes panel |
| Cmd+Shift+U | Open Settings on Usage, or close it |
| Cmd+, | Settings |

Settings > Keyboard can change the New Agent in Checkout, Changes panel, and Usage chords, and can point Cmd+1 to Cmd+9 at agents (the Nth card in the column) instead of tabs. Typing in a terminal or text field never runs card keys, and a stray click never moves focus out of the terminal. Dialog choices that throw work away or publish it are Command chords (Cmd+D, Cmd+Enter, Cmd+Delete), never a plain letter or bare Enter. Contributor details are in [keyboard flow](keyboard-flow.md).

## Supported agents

Shika finds each CLI through your login shell's PATH, including when launched from Finder. Your CLI subscription or provider charges still apply.

| Agent | Command | Launch arguments |
| --- | --- | --- |
| Claude Code | `claude` | `--dangerously-skip-permissions` |
| Codex | `codex` | `--dangerously-bypass-approvals-and-sandbox` |
| Cursor CLI | `agent` | `--yolo --trust --sandbox disabled` |
| Pi | `pi` | `--approve` |

> [!WARNING]
> These presets launch each CLI in an automatic approval mode. Claude Code, Codex, and Cursor CLI bypass permission prompts or sandboxing, so agents can run commands with your user account's access. A worktree separates Git edits; it does not restrict access to files, credentials, or the network. Use Shika in projects you trust and review the agent's changes.

Settings > Appearance > Background image puts a picture of your own behind the glass, in place of the desktop: choose a file, pick an effect (None, Dither, ASCII, Halftone, or Scanlines), and Adjust to drag and zoom the crop. Background opacity sets how much of it shows, and Apply to picks the sidebar alone or the sidebar and terminal. Choosing an image at 100% opacity lowers it to 70% so the image shows. Shika keeps its own copy, so moving the original changes nothing.

Settings > Agents turns each CLI on or off for New. [Switch agent](agent-switch.md) (Cmd+Shift+A) starts a different CLI in an idle task's worktree, to continue the work or review it.

## Start with a terminal

Choose **Terminal** at the bottom of Cmd+N's picker to open your login shell in a fresh worktree without starting an agent. It is available even without agents installed and uses the same optional setup and safe Close. The task gets a generated name; Cmd+Shift+R renames its display name, not its branch. Shell commands never trigger agent status, naming, or completion notifications.

Changes and Create PR still work. Cmd+Shift+A starts an agent later in the same task, keeping your original terminal as a Shell tab. After quitting, Leftover worktrees > Reopen > Terminal opens a fresh shell without rerunning setup or restoring old programs. See [Terminal tasks](terminal-tasks.md).

## An agent in your checkout

For a quick question about the repository folder you added, press **Cmd+Shift+N** or choose **Agent > New Agent in Checkout**. Pick a CLI in **In checkout**; Tab changes projects. There is one checkout session per project, so invoking it again focuses the existing card. Cmd+N still always creates a fresh worktree.

The checkout card sits first in its project and shows a checkout icon and the current branch. It runs directly in your folder, with no new branch or setup. It is not read-only: the agent can change your files. Shell tabs, Switch agent, and rename work normally, but Shika never renames your branch or offers Create PR here. Diff stat and Changes show uncommitted changes against `HEAD`, including edits already present at launch. Close only stops the agent and shells, asking **Stop the agent?** with **Cmd+Enter** if a turn is active or blocked; it never discards files or deletes branches. Quit leaves no leftover. See [checkout lifecycle](tasks-and-worktrees.md#checkout).

## The Lead

Press Cmd+L to start a Lead for the selected project: one of your CLIs that you talk to, which starts up to four ordinary worker cards, waits for them, reads their reports, and brings back what needs your decision. It opens Create PR and Close dialogs for its workers, and you confirm every one. A Claude Code Lead is a native chat that shows only your messages and its final answers; other Leads keep their terminal. Closing a Lead stops it and keeps its conversation, so it rests in the column and resumes on Enter or Cmd+L. See [Lead agent](lead-agent.md) and [Lead chat](lead-chat.md).

## Worktree preparation

The first **New** for an unconfigured project offers detected files and install commands. Choose **Save, approve and start** to prepare this and future agents automatically, **Customize setup…** to edit, or **Start without setup**. Shika remembers the choice per project; adding a project runs nothing. Existing configurations use the normal approval dialog.

For later changes, open **Settings > Projects**, then choose that project's **Worktree setup…** to configure files to copy, setup commands, and a timeout. With no saved configuration, Shika prefills an unsaved draft with existing ignored root `.env` / `.env.local` files and an npm or pnpm installer when package metadata is unambiguous. Review, edit, or remove suggestions before saving; nothing runs automatically. Save creates `.shika/worktrees.json` in that project's main checkout. Opening or cancelling setup creates nothing; saving does not run commands or grant launch approval. Disable setup removes the configuration for future tasks, without changing running tasks.

You can also create the same file yourself:

```json
{
  "copy-files": [".env.local"],
  "setup-worktree": ["npm ci"],
  "timeout-seconds": 600
}
```

Once the agent starts, **Cmd+T** opens a shell in its worktree. Run your dev command (for example, `npm run dev`), then Cmd-click the localhost URL. Shika does not start servers automatically.

Use your project's own commands. Copied files must already be ignored by Git on both the main checkout and the task's base. Shika asks for local approval, and asks again when the configuration changes. Setup commands are trusted code, run with your account's access. See [worktree preparation](worktree-preparation.md) for progress, cancel, retry, and cleanup rules.

## Renaming a task

Right-click a card and choose Rename task, or press Cmd+Shift+R. Your name takes priority over automatic card titles and becomes the next Create PR title default. Renaming does not change the Git branch, worktree, existing commits, or existing PRs. Automatic branch naming continues independently. See the [task-name FAQ](task-names.md) for examples and exact effects.

## Creating a PR

Create PR (Cmd+Shift+P) previews every non-ignored change and an editable title. Confirm it, and Shika stages, commits if needed, pushes, and creates a GitHub PR targeting the task's original base. It requires `gh auth login` and never merges. Afterwards the card shows the PR's checks. See [confirmed PR publishing](publishing.md) for behavior, rationale, and troubleshooting, and [PR checks on the card](pr-checks.md) for the checks mark.

## Closing a task

For worktree tasks, Close asks before discarding uncommitted work or unpushed commits, and offers push when the tree is clean. A `git push` keeps the card open, and Close never commits. A clean idle task on its own branch closes immediately. After a branch switch, Close can verify both branches and remove the worktree while keeping both local branches; unsafe or unverifiable work blocks that. See [branch-switch close](branch-switch-close.md) for the safety rules.

## Changes and token usage

Cmd+Option+B opens a read-only diff of the selected task beside the terminal; see [Changes panel](changes-panel.md). Cmd+Shift+U opens Settings on Usage, with local Claude Code and Codex token totals by week, month, or year; see [token usage](token-usage.md).
