# Changes panel

The Changes panel is a read-only diff of the selected task, on the right of the terminal. It is closed by default and on every launch. Open it after a turn is Ready, read a few lines of what the agent changed, and close it.

Start here before changing the panel. This document records the product decision, the refresh and performance rules, and where the code lives. [design/DESIGN.md](../design/DESIGN.md#visual-foundations) governs its appearance (Changes panel, and Diff colors under Color). [keyboard-flow.md](keyboard-flow.md) covers app-wide focus and shortcuts. [MANUAL_CHECKS.md](../MANUAL_CHECKS.md#changes-panel) separates validation evidence from intended behavior.

## Why it exists

The author's loop after a turn: the card turns Ready, open a shell tab, type `lazygit`, read a few hunks, quit lazygit, close the tab. It works, but it is five steps to look at something Shika already knows how to compute: the card's diff stat compares the same tree against the same base.

The panel opens from its shortcut (Cmd+Option+B until Settings changes it), the toggle button, or the View menu. It answers "what did the agent touch, and does it look right?", not "let me work on this code". So it is read only:

- No editing, staging, unstaging, reverting, or committing. Create PR stays the only git write path in Shika, and the shell stays the place for hands-on git.
- Nothing in the panel writes to the repository, the index, or the worktree, or takes `index.lock`. Git runs with `GIT_OPTIONAL_LOCKS=0`, like the diff stat.
- No file tree browser, no commit or history browser, no syntax highlighting in this version.

It is the one exception to "no splits beyond the resizable agent column". It is not a second terminal and not an editor; [CONTRIBUTING.md](../CONTRIBUTING.md#product-principles) keeps one terminal on screen and lists editing from the panel as not planned.

The goal is snappy, light, and reliable. Closed, it costs nothing. Open, it stays smooth on a huge diff.

## Behavior at a glance

```text
╭────────────╮                                     │
│ Claude Code│ +                  [Close task ⌘⇧W] │ Changes  2 files +64 −3   r refresh  esc close  [▯▯]
╯            ╰──────────────────────────────────── │ ─────────────────────────────────────────────────────
 worktree path                         focus hint  │ ╭─────────────────────────────────────────────────╮
                                                   │ │ ⌄ layout.rs                              +60 −3 │
 (terminal)                                        │ │   src                                  Modified │
                                                   │ │   12  let width = bounds.width;                 │
                                                   │ │   13  let rows = height / line;       (red tint) │
                                                   │ │   13  let rows = (height / line).floor(); (green)│
                                                   │ ╰─────────────────────────────────────────────────╯
                                                   │ ╭─────────────────────────────────────────────────╮
                                                   │ │ ⌄ resize.md                                  +4 │
                                                   │ │   docs                                    Added │
```

- It shows the selected card's task changes, compared exactly as the card's diff stat is: the worktree against the merge base with the task's recorded base ref, falling back the same way (the default branch, then HEAD). That covers the agent's commits, uncommitted edits, and untracked files that are not ignored.
- It reads only the task's worktree. A worktree whose `.git` file is missing or broken shows "Could not read changes", and the card hides its diff stat; git is never allowed to walk up to the main checkout around `.worktrees/` and show that instead (see [traps](#implementation-traps)).
- Untracked files show as all added. Renames are detected and shown as `old → new`. Deleted files show their deletions. Binary files are listed as "Binary file" with no lines. Mode-only changes show their header alone.
- The panel follows the selected card. Card navigation keeps working while it is open.
- Every file is listed in git's order in one scrolling list, framed as a quiet file card. Its two-row header separates basename/counts from directory/status; code keeps line numbers and diff colors, without `+`/`-` prefixes or hunk metadata. Hunks have a quiet spacer, not a label. There is no file tree.
- Clicking either header row folds or unfolds the file without a Git read. Folding is distinct from the safety caps below: an author-folded file keeps its already-read code in memory. Folds survive same-task refresh by path/status, but clear on card change or panel close. All files initially open, except code hidden by the existing caps.
- A big file comes collapsed (see [caps](#performance-budget-and-caps)), and so does every file after the task's total budget runs out; the collapsed row names which cap applied. A click or `o` expands it either way.
- Line text is drawn safely: tabs expand to 4-column stops counted from the start of the line, the `\r` that ends a CRLF line is dropped, other control characters show as their Unicode control pictures (`␛`, `␍`), and C1 controls and bidirectional overrides show as `�`, so text cannot reorder itself. Columns are display width (`unicode-width`, per character, as a terminal counts): CJK and most emoji take two, combining marks and other zero-width characters none and stay with the character before them. A wide character is never split: when the left edge cuts one, its visible half is a blank cell, and one that starts in the last column is kept whole for the text area to clip. An emoji sequence joined with U+200D counts each emoji, as in the terminal, so the text after one sits a little left of its columns.
- Open state is memory only. Every launch starts closed. Only the width is saved.

### Controls and focus

| Control | Result |
| --- | --- |
| The shortcut (Cmd+Option+B by default), the toggle button, View menu "Hide or show changes" | Open the panel and focus it, or close it. Does nothing while a dialog or picker is open or the app is busy. Settings, Keyboard, can replace the chord, and the previous chord stops working |
| Click a card, including its diff stat | Select that card and focus its terminal. The panel stays as it is |
| Escape in the panel | Close it and restore the focus saved when it opened |
| Click in the terminal | Focus the terminal; the panel stays open |
| Click in the panel | Focus the panel |
| Click either row of a file header | Fold or unfold that file, with no Git read |
| Ctrl+Q in the panel | Return to the cards; the panel stays open |
| `j` / `k`, Down / Up | Scroll one row |
| `d` / `u` | Scroll half a page |
| Space / Shift+Space | Scroll a page |
| `g` / `G` | Top / bottom |
| `]` / `[` | Next / previous file header |
| `h` / `l`, Left / Right | Scroll long lines horizontally |
| `o` | Expand the first collapsed file on screen; nothing when none is on screen |
| `r` | Refresh |
| Trackpad, wheel | Scroll both ways |
| Cmd+] / Cmd+[, `j` / `k` on the cards | Change the agent; the panel follows. From the panel, Cmd+] / Cmd+[ keep focus in the panel |
| Cmd+T, Cmd+1 to Cmd+9, Ctrl+Tab | Act on the task's tabs and focus the destination terminal, as from the cards |

The panel's plain keys run only while the panel has focus. None of them reach a PTY, and Escape in a terminal still goes to the CLI. There is no text cursor and no selection in this version, so there is no copy from the panel.

When the panel opens it saves `window.focused(cx)` and focuses itself. Closing it from the panel (Escape, the toggle, or the button) restores that handle once; if that view is gone, focus goes to the cards. The panel follows the selection, so a saved terminal that belongs to another card than the one now selected gives way to the selected card's shown terminal (the same surface, on the card in view). Closing it while something else has focus moves no focus. A click on a card, including its diff stat, selects that card and focuses its terminal and leaves the panel as it is.

### Layout and width

The window is `[column][terminal][panel]`. The panel is 480px by default, 320 to 900, and the terminal keeps `MIN_TERMINAL_WIDTH` (420). The widths on screen are worked out in this order:

1. Panel: its saved width, capped so the terminal keeps 420 beside the column's saved width (or no column when hidden), and never under 320.
2. Column: its saved width, capped so the terminal keeps 420 beside the panel on screen, and never under 320. This is `column_width` with the panel's width taken out of the room.
3. Terminal: the rest.

So a narrowing window shrinks the panel first, then the column. Only when both are at 320 does the terminal go under 420: the window's minimum is 960, and 320 + 420 + 320 is 1060. Cmd+B gives the column's width back. Growing the window brings the saved widths back; clamping on screen never rewrites the saved values.

The left edge drags like the column's right edge: an 8px strip, double-click restores 480, the drag stops at 320 and where the terminal would drop under 420, and it never closes the panel or moves the column. The width is saved once a drag ends, as `changes.width` in `settings.json`; a missing or invalid field means 480.

Opening or closing the panel resizes the terminal and so its PTY. The panel does not animate its width for that reason. Agent activity already treats a resize as an interaction that neither starts a turn nor resets the timer; see [agent-activity.md](agent-activity.md).

## Refresh model: fetch, never watch

The panel holds only the shown card's diff and drops it when it closes. It fetches:

- when it opens,
- when the selected card changes while it is open,
- when the shown card turns Ready (the moment `fetch_diff_stat` already runs),
- on `r`.

There is no file watcher, no polling, and no timer. Nothing runs while the panel is closed. A diff that is stale because the agent kept working is fixed by the next Ready or by `r`; that is the trade for never touching a busy worktree in the background.

Git and parsing run on the background executor. The UI thread never waits for git. Each fetch takes a new generation number; a result is applied only if its generation is still the newest and the shown card is still the one it was for, so a slow old result cannot overwrite a newer one.

On a refresh of the same card, the old rows stay on screen until the new ones arrive, so there is no blank flash, and the scroll position is kept, clamped to the new length. Expanded files stay expanded by path. On a card change the old card's rows are dropped at once (another task's diff under this task's title would mislead), scroll goes to the top, and expansions are forgotten. The list stays blank until the new diff arrives, and shows "Reading changes" if that takes more than 500ms.

## Performance budget and caps

Closed:

- No git process, no watcher, no timer, no diff in memory. The toggle button is the only cost. Keep it that way.

Open, per fetch:

- One `git rev-parse --show-toplevel` that proves the worktree is its own checkout, the base lookup, one `git diff --no-color --no-ext-diff --find-renames -U3 <merge base> --`, and one `git ls-files --others --exclude-standard -z`, all read only with `GIT_OPTIONAL_LOCKS=0` and `GIT_CEILING_DIRECTORIES`, on the background executor. Untracked files are read from disk there too, without following symlinks.
- Git's output is read once into memory and parsed into owned rows. Nothing is streamed.
- The UI flattens the result into one row index (spacer, filename, metadata, hunk gap, line, cap row, rounded bottom) once per result on the background executor. Painting then costs only visible rows: the uniform list remains virtualized, so a 50,000-line diff shapes about as much text per frame as a 50-line one. File outlines are painted as visible row slices, not per-file entities or nested lists.
- `VisibleRows` maps a small set of file ranges into that unchanged full index. Folding rebuilds those ranges in O(files), not O(lines), on the UI thread, with no code cloning, measuring, parsing, or Git. Binary search maps each visible row back to its full-index row. File navigation and cap expansion use the visible header/cap positions; hidden cap rows are not expanded by `o`. Folding preserves the top visible row, or returns to its header if that row became hidden.

Caps, so a huge diff stays cheap:

| Cap | Value | Result (`Collapse` in core) |
| --- | --- | --- |
| Changed lines in one file | more than 2,000 | Header plus "Large diff hidden - 4,812 lines" (`Lines`) |
| Bytes in one file, with at most 2,000 changed lines | more than 1 MiB of patch, or of untracked content | Header plus "Large file hidden - 3.2 MB" for an untracked file, "Large diff hidden - 6.3 MB" for a tracked one (`Size`); an untracked file's lines are counted as it streams, never held |
| Parsed lines or bytes in one diff | about 50,000 lines or 8 MiB | Every file from the first that does not fit on shows its header and "Diff hidden - N lines" (`Budget`), even a one-line file. A file over its own cap there keeps that cap's wording |
| Expanding one file | 100,000 lines or 16 MiB, in core | The file's lines up to that limit, then one row "N lines not shown" |

Each collapsed row says why it collapsed, so a small file past the budget never reads as large. Sizes are decimal with one decimal, as Finder shows them (`size_label`). All three expand the same way.

Targets: rows on screen within 100ms of opening for a diff under 2,000 lines, a 50,000-line result within 500ms with the UI responsive throughout, and scrolling at the display's frame rate with no dropped frames. Measured on 2026-10-08 in a debug build on Apple silicon, with a 38-file change of about 57,000 added lines that hits the total cap (48,733 rows, 6 collapsed files): git and parsing took 60 to 73ms and building the row index 14ms, both on the background executor. Frame times were not instrumented. See [MANUAL_CHECKS.md](../MANUAL_CHECKS.md#changes-panel).

With `--diagnostics-file /path/report`, each read appends one line to `/path/report.changes`: `read_ms`, `index_ms`, and the file, row, and collapsed counts. Never paths or content. Use it to measure before and after a change to reading or rows.

## Architecture and code map

Search by symbol rather than line number. App symbols are in `crates/shika/src/changes.rs` unless another file is named.

| Layer | Symbols | Responsibility |
| --- | --- | --- |
| Core API | `crates/shika-core/src/lib.rs`: `Core::session_diff`, `Core::session_file_diff`, `Core::session_diff_stat` | Read the shown task's diff without a core lock, with the same base resolution as the stat |
| Diff reading | `crates/shika-core/src/worktree.rs`: `diff_stat`, `session_diff`, `file_diff`, `diff_base`, `compare_base`, `read_only_git`, `check_worktree` | Base resolution and fallback, lock-free git confined to the worktree, `git diff` plus untracked files |
| Diff types | `crates/shika-core/src/diff.rs`: `SessionDiff`, `FileDiff`, `Collapse`, `FileKey`, `FileStatus`, `Hunk`, `DiffLine`, `LineKind`, `Budget` | Owned, UI-free, testable results; caps recorded as `collapsed: Option<Collapse>` (why) and `hidden_lines` |
| Expansion | `Core::session_file_diff`; `Shika::expand_changes_file`, `DiffView::with_file` | One file without the per-file cap, up to the hard limit, read off the UI thread and spliced into a new row index there |
| Settings | `crates/shika-core/src/settings.rs`: `Changes`, `changes_or_default` | `changes.width`, 320 to 900, default 480, an invalid field read as the default; no open state |
| Toggle and focus | `ToggleChanges` (action, binding, and View menu item in `main.rs`); `Shika::toggle_changes`, `open_changes`, `close_changes`, `restore_changes_focus`, `Panel::return_focus`; `move_agent` in `main.rs` keeps panel focus | Open/close, saved focus, busy/overlay guards |
| Layout | `pane_widths`, `drag_width`, `Shika::drag_changes`, `changes_handle`, `ChangesDrag`; `Shika::pane_widths`, `column_width`, `drag_column`, `MIN_TERMINAL_WIDTH` in `main.rs` | Yield order: panel, then column, then terminal; widths saved when a drag ends (`tick`) |
| Panel view | `Panel`, `Shika::changes_panel`, `changes_list`, `changes_toggle`, `changes_key`, `RowPaint`, `CHANGES_ICON`; `DiffView`, `Row` | Title row, toggle, card slices, horizontal offset, empty states, keys |
| File folding | `VisibleRows`, `Panel::set_view`, `Shika::toggle_changes_file`, `folded_top`, `card_labels`; `Chrome::diff_file_header` in `appearance.rs` | O(files) range mapping, stable fold identities, scroll anchoring, filename/directory labels, opaque header wash |
| Refresh | `Shika::sync_changes` (runs each paint, acts only on a new target), `fetch_changes`, `Panel::generation`, `Panel::shows`; the Ready hook beside `fetch_diff_stat` in `tick` | Fetch on open, card change, Ready, and `r`; drop stale results |
| Labels | `crates/shika/src/model.rs`: `diff_stat_label` | The `2 files +64 −3` text shared by the card and the title row |
| Diff colors | `crates/shika/src/appearance.rs`: `diff_colors`, `Chrome::diff_added`, `diff_removed`, `term_text` | ANSI green/red text and opaque tints per theme and glass |
| Terminal keys | `crates/shika-terminal/src/view.rs`: `key_for` | Keeps Cmd+Option+B, like every Command key, out of PTY input |
| Pure logic | `visible_text`, `columns`, `glyph`, `safe_label`, `file_label`, `collapsed_label`, `size_label`, `thousands`, `lines_label`, `panel_body`, `scroll_top`, `first_in_view` | Unit tested in `changes.rs` |

Keep the core types free of GPUI and the UI free of git. The panel must not shell out itself; it asks core.

## Implementation traps

- **Translucency.** The panel paints term-surface once. Rows paint only their tints, which are opaque, like CLI-colored cells in the terminal. A translucent tint over the translucent panel would add alpha (GPUI's Metal blending) and come out darker or more opaque than designed. Do not give the row list a second background.
- **Horizontal scroll in a virtualized list.** Rows share one horizontal offset applied to line text only; the gutter and headers do not move. The pinned GPUI's `uniform_list` can scroll sideways (`ListHorizontalSizingBehavior::Unconstrained`), but it moves whole rows, gutter included, and needs every row measured at the widest line. So the list scrolls only vertically, and the panel keeps its own offset in pixels (`Panel::shift`, at most `max_shift`, the widest line less the text area). Each line row paints only the columns on screen (`visible_text` from `shift / cell`), shifted left by the part of a cell, so a 20,000-column line costs what fits in the panel and trackpad scrolling stays smooth. `h`/`l` move 8 columns; a sideways wheel delta moves `shift` in the list's `on_scroll_wheel`, and `restrict_scroll_to_axis` keeps that delta from scrolling the list down. The widest line is measured once per result, per file, off the UI thread.
- **Static list padding.** The list sits 8 under the title row and 28 above the bottom as margins, not as `uniform_list` padding: the pinned `uniform_list` leaves rows unpainted in its bottom padding while scrolled. Rows clip at those margins, like the terminal's own pad.
- **Re-entrancy.** `sync_changes` runs in `Shika::render`, so it only compares the target and starts work; it must never update another entity or notify synchronously.
- **Uniform rows.** Every row is one terminal row high, with a 21px minimum for stable header labels. The two-row file header and rounded bottom are separate uniform entries; never turn a file into a tall entity or nest another list. A wrapped line breaks the uniform list and its cheap scrolling.
- **File cards and glass.** Body slices paint borders only; no card background is layered over the translucent list. Headers use one opaque, theme-derived wash (`Chrome::diff_file_header`), and diff tints remain opaque. Slices carry the left/right outline even when a file's header is offscreen; top and bottom corners belong only to their respective rows.
- **Fold mapping.** Keep full rows and code immutable on a fold. Map visible file ranges instead, and use visible header/cap indices for keys. Refresh and cap-expansion results rebuild that mapping using the current folds, not a stale snapshot from when their background read started.
- **Ready refresh.** Hook the existing Ready moment; do not add a second status watcher. The card's `diff` stat and the panel can briefly disagree while one fetch is in flight.
- **Focus.** Plain panel keys belong to the panel's key context. Escape closes the panel only while the panel has focus; never make Escape an app-wide key.
- **Resizes.** Opening the panel or dragging its edge resizes the PTY, as the column does. A resize must not start a turn or reset the timer; if a TUI redraw shows up as Working, look at agent activity, not the panel.
- **Base resolution.** The panel and the stat must use the same `diff_base` path. A panel that counts differently from the card is a bug in one of them, not a design choice.
- **Repository discovery.** A task worktree lives inside the main checkout, at `<repo>/.worktrees/<branch>`. If its `.git` file goes missing, plain git discovery walks up and finds the main checkout, and the panel would show the main checkout's (usually empty) changes as the task's. So `read_only_git` sets `GIT_CEILING_DIRECTORIES` to the worktree's parent (git resolves its symlinks, so `/tmp` and `/private/tmp` agree), and `diff_stat`, `session_diff`, and `file_diff` first run `check_worktree`, which requires `git rev-parse --show-toplevel` to equal the worktree, both canonicalized. The check also covers a parent path with a `:`, which the colon-separated ceiling cannot express. Every `read_only_git` caller passes a task worktree root; never pass the main checkout to it. A broken `.git` file pointing nowhere already fails in git itself.
- **Wide characters.** `columns` and `visible_text` must count with the same `glyph` widths, or `max_shift` and the slicing disagree. Keep the slicing a single forward pass that stops at the right edge; never measure the whole line per frame.

## Debugging guide

| Symptom | First places to inspect |
| --- | --- |
| The panel and the card's diff stat disagree | Both must go through `diff_base` / `compare_base` with the same recorded base ref; then untracked handling and rename detection |
| The app stutters while the panel loads | Something waits on git or parses on the UI thread; both belong on the background executor |
| Scrolling a big diff drops frames | Rows built per frame instead of once per result; non-uniform row heights; shaping text for rows that are not visible |
| A stale diff after the agent finished | The Ready hook and the generation check; a result for an older generation or another card must be dropped |
| An old card's diff flashes on another card | The card-change path must clear rows before the fetch, and results must match the shown card |
| `index.lock` errors in the user's shell while the panel is open | `GIT_OPTIONAL_LOCKS=0` and `read_only_git`; the panel must never write |
| "Could not read changes" | Run the git commands below by hand in the worktree; check the base ref still resolves. "not a git repository" or "is not a git worktree of its own" means the worktree's `.git` file is missing or broken; `git worktree repair` from the main checkout restores it |
| The panel shows the main checkout's changes, or "No changes" for a task with work | Discovery left the worktree: check `GIT_CEILING_DIRECTORIES` in `read_only_git` and `check_worktree` |
| A small file reads "Large diff hidden" | The collapse reason: past the total budget it must be `Collapse::Budget`, "Diff hidden - N lines" |
| CJK or emoji lines cut early, overlap, or jump while scrolling sideways | `glyph` widths, used by both `columns` and `visible_text`; a cut wide character must become a blank cell |
| A key in the panel reaches the CLI, or Cmd+Option+B types into the terminal | The panel's key context and focus; `key_for`'s Command exclusion |
| Escape does not return to the previous terminal | The saved focus handle at open, consumed once at close; a closed tab falls back to the cards |
| The terminal is narrower than 420 with room to spare | The width order: panel, then column; both clamp against the other's on-screen width |
| Added rows look darker or lighter than designed in glass | A second translucent fill under the rows; tints must be opaque and painted once |
| Diff colors look wrong in a derived theme | `chrome_for` / `derived_chrome` diff fields: ANSI 1 and 2 from that theme's palette, tint mix, 4.5:1 text push |

To see what the panel should show, run the same read-only commands in the task's worktree:

```sh
cd /path/to/repo/.worktrees/<task>
export GIT_CEILING_DIRECTORIES="$(dirname "$PWD")"   # as the panel does: never the main checkout
base=$(git merge-base HEAD <recorded base ref>)
GIT_OPTIONAL_LOCKS=0 git diff --no-color --no-ext-diff --find-renames -U3 "$base" --
GIT_OPTIONAL_LOCKS=0 git ls-files --others --exclude-standard -z | tr '\0' '\n'
```

The recorded base ref is `baseRef` in that worktree's entry in `worktrees.json`. Use an isolated `--data-dir`; never read or change the author's normal app data while debugging.

## Validation and contributor guardrails

Run automated validation from the repository root:

```sh
source "$HOME/.cargo/env"
cargo fmt --all --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
./scripts/bundle-app.sh --debug
```

Core parsing must be covered by tests in disposable repositories: quoted and unusual paths, renames with and without edits, binary files, mode changes, `\ No newline at end of file`, CRLF, invalid UTF-8 (shown lossily), empty files, deleted files, untracked symlinks (not followed), untracked binaries, each cap and its `Collapse` reason, and a worktree whose `.git` file is missing or broken (including under a path with `:` and through a symlink). Line slicing has unit tests for tabs, controls, wide characters, and zero-width characters. Unit tests do not establish rendering, focus, scrolling feel, or translucency; use the [manual checklist](../MANUAL_CHECKS.md#changes-panel) and keep passed evidence apart from pending checks.

For GUI testing, use a disposable repository and an isolated data directory:

```sh
open -n target/debug/Shika.app --args --data-dir /absolute/path/to/disposable/data
```

Confirm Shika's own window is frontmost before any synthetic keystroke.

When extending the panel:

- Keep it read only. Editing, staging, unstaging, reverting, discarding hunks, committing, or any other write needs an explicit author decision first; Create PR is the git write path.
- Never watch or poll. New refresh moments are explicit events, like Ready or `r`. A file watcher or timer needs an explicit author decision.
- No syntax highlighting without an explicit author decision. Grammars are most of the weight in other diff viewers, and the panel's goal is to stay light.
- No file tree, history, or commit browser, and no second terminal in the panel.
- Keep the caps and the virtualized, uniform-height list. File-card outlines must remain visible row slices, never a nested list or one element per whole file. Measure a 50,000-line diff before and after any change to rows or parsing.
- Keep git in core, behind `read_only_git` and `GIT_OPTIONAL_LOCKS=0`, with the same base as the diff stat.
- Use the design tokens in `design/DESIGN.md`. The diff green and red stay inside the panel's rows.
- Guard every entry point, including the menu and the toggle, against overlays and busy state. A card click, including its diff stat, selects the card and focuses its terminal; it is not an entry point for the panel.
- Update this document, `AGENTS.md`, `design/DESIGN.md`, `docs/keyboard-flow.md`, and the acceptance checks when behavior changes. Record validation honestly.
