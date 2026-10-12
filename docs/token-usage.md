# Token usage

A local count of tokens Claude Code and Codex used in Shika. It is a rough total, grouped by day and by week. It is not money, a subscription bill, or a quality score. More tokens does not mean better work.

Cursor and Pi stay out. Shika does not read their session files for this, does not store transcripts, and does not call a network.

Read [AGENTS.md](../AGENTS.md) and [design/DESIGN.md](../design/DESIGN.md) before changing it. [MANUAL_CHECKS.md](../MANUAL_CHECKS.md#token-usage) is the native check. Unit tests do not pass it.

## When a total is recorded

A card records once when it turns Ready, and once when its CLI exits. Both happen only for preset `claude` or `codex`, including a Lead that uses one of those CLIs. The read runs on the background executor. There is no timer, watcher, poller, or extra process. The UI thread never reads a CLI file for this, and the read never takes `Core`'s operations lock.

A failed or slow read is skipped. The previous totals stay. Opening the dialog does not read the CLI files again.

The same cumulative total adds nothing the second time. Relaunching Shika does not count a turn twice, because the last cumulative for that session is in the usage file. If a later read is lower, that value becomes the new baseline and nothing is added.

## Claude Code

Same project directory as session titles (`cli_title`): `$CLAUDE_CONFIG_DIR` or `~/.claude/projects/<cwd with every non-alphanumeric character replaced by ->`. The newest `.jsonl` in that directory, by mtime, is the session. Only its tail is read, at most 64KB. The last `cost-state` record wins. From `modelUsage`, Shika sums `inputTokens`, `outputTokens`, `thinkingTokens`, `cacheReadInputTokens`, and `cacheCreationInputTokens` across models.

Shika does not read `stats-cache.json`, does not sum assistant transcript lines, and does not scan subagent files. If the tail has no `cost-state`, the read is skipped. The session key is the file name.

## Codex

Same newest `state_<n>.sqlite` as session titles, opened `SQLITE_OPEN_READ_ONLY` with a 200ms busy timeout. One query for that cwd returns the newest thread's `id` and `tokens_used`. Rollout jsonl files are not opened. A locked database or a failed query is skipped. The session key is the thread id.

## The usage file

`usage.json` sits in the app data directory, beside `settings.json`. It keeps a daily total per CLI for the local calendar date of the read, and the last cumulative value per CLI session. Only the increase is added. Days with no tokens are absent. A missing day is a gap, not a zero that replaces history. An unreadable file is left as it is.

## The dialog

View > Usage opens it. There is no shortcut, so it does not collide with Cmd+Shift+A. The dialog reads only `usage.json`. It shows this calendar week, Monday through Sunday, local time, as two totals: Claude Code and Codex. Then each day in that week that has tokens, newest first. With nothing recorded, it says "No tokens recorded yet." The line under the title is "Tokens from Claude Code and Codex in Shika. Not a bill." Escape closes and restores focus. Close does the same. No chart, refresh button, network, new color, or animation.

## Code map

| Piece | Where |
| --- | --- |
| Tail read, Codex query, daily file, week report | `crates/shika-core/src/usage.rs` |
| Shared project directory, newest `.jsonl`, newest `state_<n>.sqlite`, cwd forms | `crates/shika-core/src/cli_title.rs` |
| `Core::record_token_usage`, `Core::token_usage` | `crates/shika-core/src/lib.rs` |
| Ready and exit hook, View menu, dialog | `crates/shika/src/main.rs` (`usage_reads`, `ShowUsage`, `Overlay::Usage`) |

`record_token_usage` is blocking and belongs on the background executor. `token_usage` only reads the small JSON file and is fine on the UI thread when the dialog opens.

## Debugging

The file is `~/Library/Application Support/com.hieule.shika/usage.json`, or `<data-dir>/usage.json` with `--data-dir`. `days` is the calendar totals. `sessions` is the last cumulative per Claude file name or Codex thread id. If a Ready card does not move the totals, the tail had no `cost-state`, the Codex query failed, or the cumulative did not increase. A dialog that is open does not update until it is opened again.

## Guardrails

- No dollar amounts, price table, or model-quality score.
- No Cursor, Pi, or agent switching.
- No transcript storage, no full Claude session read, no `stats-cache.json`, no subagent scan, no Codex rollout file.
- No timer, watcher, poller, or extra process.
- Do not take the operations lock. A failed read must not rewrite the file.
- Do not mark the native Usage check passed from unit tests.
