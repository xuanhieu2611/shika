# Token usage

Local token usage from Claude Code and Codex launched in Shika, including their recorded subagents. Cursor and Pi are not collected. Counts are provider-reported traffic, not money, a subscription bill, or a quality score.

Read [AGENTS.md](../AGENTS.md) and [design/DESIGN.md](../design/DESIGN.md) before changing it. Native acceptance lives in [MANUAL_CHECKS.md](../MANUAL_CHECKS.md#token-usage), not inferences from unit tests.

## Decisions from PR #69 review

For dashboard accuracy, timestamped receipts belong to the provider record's local calendar day, including historical time-zone and DST rules. Recovery after relaunch does not move those tokens into today. Missing or invalid timestamps fall back to the collection day; old saved estimates stay on their original dates. Missing available usage is not acceptable merely because a card closed or a read failed. Collection must stay independent of terminal responsiveness.

The original summary-only design was replaced after review:

- Claude's `thinkingTokens` is already inside output tokens, not an extra bucket. Its `cost-state` can be saved infrequently, including at exit, so a 64KB tail read on Ready is not a reliable live collector.
- Codex's newest thread can be a child, not the main CLI. Its context counters can include inherited or reconstructed context. Collect deduplicable response receipts, not a guessed delta from one `tokens_used` row.
- A detached one-shot read cannot recover a final write after quit. Persist the source before launching, then commit receipt counters and file offsets together.

This explicitly supersedes the PR's original no-retry, no-subagent, and summary-tail-only rules. It does not add a provider API wrapper, conversation history, network request, or extra CLI process.

## Lifecycle and recovery

The app explicitly starts one `shika-usage` worker. `Core::open` alone does not start it, so ordinary core tests never inspect the user's provider data.

Before an agent starts, its launch thread atomically journals a small path-only source in `<data-dir>/usage-sources/<id>.json`. It records the exact project directories or Codex home and cwd forms, including a resolved cwd while the worktree still exists. No prompt, response, credential, or model API call is stored. Source registration failure does not block launch; Usage reports the collection problem. `CLAUDE_CONFIG_DIR` and `CODEX_HOME` are honored, including relative roots resolved from the agent's cwd. Keep provider session storage outside disposable worktrees: deleted-before-read data cannot be recovered.

The agent's existing PTY sink only marks a per-source atomic flag and unparks the worker on the first pending event. It never waits for the scheduler's mutex, clones a source ID, or wakes again while already marked. No provider read, counter write, hash, or serialization happens on the PTY reader or UI thread. Ready and exit request collection too. The sink also covers Close, Switch agent, hidden terminals, and CLI output before its card is installed. Shell tabs do not register sources. The worker never takes Core's operations lock.

Wakeups coalesce at 250ms per source. After a successful drain there are two settling reads, after 1 and 2 seconds, to catch delayed writes. Failed or partial reads use a bounded retry burst of 1, 2, 5, 15, and 30 seconds. A source then waits for another PTY event, Usage open, or app launch; no perpetual idle poll or filesystem watcher runs. A failure retains every committed count.

On restart the worker recovers only journaled Shika sources. It does not scan global Claude or Codex history, restore sessions, change leftovers, or require the worktree to survive. Sources remain registered after Close so a late provider flush can be recovered. Recovery enumerates at most 16 registrations per step and keeps at most 64 queued historical jobs. Due live jobs have priority, with one historical job after at most three live jobs for progress. Historical batch continuations drain ahead of untouched history, so compressed readers do not repeatedly replay evicted prefixes. Opening Usage only signals recovery and reads an indexed saved snapshot in the background; it never enumerates the registry or opens provider files on the UI thread. A snapshot does not live-refresh. Routine scheduling or settling alone is not a collection warning; warnings represent observed read/format failures or unfinished records.

Quit does not join the worker or delay terminal shutdown. A killed worker loses only its uncommitted transaction. Source metadata and completed-record offsets permit the next launch to replay it without counting a receipt twice.

## Provider records

### Claude Code

Look only in the registered project directories and each root session's direct `subagents` directory. Read all those session files, not whichever happens to be newest. A streaming JSON reader extracts `assistant.message.id` and `message.usage`; other fields, including content, are skipped rather than loaded into a transcript object.

Count `input_tokens`, `output_tokens`, `cache_read_input_tokens`, and `cache_creation_input_tokens`. Thinking is a subset of output and is not added again. The message ID deduplicates streaming blocks, copied history, and forwarded subagent messages across files. Repeated partial counters retain each bucket's maximum, so a later final record adds only its increase.

No `cost-state`, `stats-cache.json`, provider price table, or usage network endpoint is needed. Verified against Claude Code 2.1.296 with a disposable configuration and synthetic local Anthropic response; this verifies persisted metadata, not interactive GUI acceptance or real account billing.

### Codex

Open the newest `state_<n>.sqlite` read-only, with a 200ms busy wait and a query execution progress deadline. Find all CLI threads for the registered cwd, then descendants through `thread_spawn_edges`, including children in another cwd. Where that table is absent, readable CLI-root totals are kept with a visible ownership/discovery gap; cwd alone cannot prove a child's ownership. Editor and independent exec threads are not roots.

The database is only a rollout-path index. Stream `token_usage_record.payload`, deduplicate by `response_id`, and count `usage.total_tokens`. Do not add cached-input or reasoning subsets again. A copied receipt whose `thread_id` differs from its rollout's thread is inherited history, not newly consumed tokens.

Remember discovered paths for both providers. Missing or locked Codex indexes retain a visible discovery gap while known rollouts remain readable, including after restart. An untouched home can have no index yet; once storage has been observed, its disappearance is a gap. A symlinked config directory is supported, but an index file itself must be regular and not a symlink. A known Claude child's remaining usage can be collected even after its parent log disappears.

When Codex's optional compression replaces a plain rollout with `.jsonl.zst`, resolve its compressed sibling and decode it read-only. Persist logical decoded offsets; at most four in-progress decoders are pinned, each with an 8MiB window cap. Additional compressed sources yield until a slot finishes. Missing, replaced, or unreadable files release slots, even without revisiting their original source. EOF checkpoints avoid decoding complete cold files again. Restart replays to a logical checkpoint with pacing and deduplication; no decompressed provider file is written.

A rollout exposing only legacy `token_count` context snapshots is reported as an uncollected gap, not silently zero and not inflated into fresh API consumption. A later response receipt clears the transient missing-receipt warning, but does not reconstruct context-only history; missing older responses cannot always be detected. Exact receipt format verified from [Codex 0.161.0 source](https://github.com/openai/codex/blob/rust-v0.161.0/codex-rs/history/src/rollout_payload.rs) and an isolated `codex exec` run against a synthetic local Responses server. Interactive thread ownership and real-provider acceptance remain native checks.

## Storage and performance bounds

`usage.sqlite` is a local SQLite WAL database using the dependency already bundled in Shika. A transaction saves daily increments, deduplication counters, and completed-record file offsets together. Restart or retry cannot save one without the other. SQLite serializes writers from overlapping app instances; there is no shared `usage.json.tmp` race.

Tables retain only dates, provider IDs, message/response IDs, numeric counters, source paths, file identity/modification fingerprints/offsets, and collection issues. No conversation text is persisted. Indexed tables avoid rewriting an ever-growing JSON map of every message or loading historic receipts for the dashboard. A pre-existing PR-format `usage.json` is imported once transactionally and left untouched; its session baselines prevent recounting that already-saved history. Previously saved estimates/errors are preserved, not retroactively corrected. An unreadable import is not overwritten or presented as a successful empty report. Metadata inputs must be regular, non-symlink files: registrations are capped at 64KB and the one-time legacy import at 64MB. New SQLite files are owner-only. Checkpoint schema upgrades preserve existing counters; future schemas are refused.

The worker uses macOS Background QoS so foreground work has scheduling priority. Normal work is a 256KB batch, checked at JSON record boundaries, with a global 250ms yield between jobs, including across sources in a startup backlog. A single very large JSON value can exceed that quantum: it is streamed without allocating its content and pauses 5ms per 64KB read. This is pacing, not a hard per-record time cap. One pathological record can delay another source's collection, but cannot take a UI/Core lock or block PTY draining. Do not claim zero overhead or a strict 256KB maximum for every record. No native latency benchmark has been established from unit tests.

A partial final record keeps its previous offset and is retried. A complete malformed JSONL record is drained without buffering its content and skipped with a persistent visible gap so later valid records are not stranded. A missing or corrupt compressed child does not roll back another file's available receipts. Opened-file identity, modification fingerprints, and 4KB samples at the beginning and checkpoint boundary detect typical replacement, rewrite, and truncate/regrowth cases without hashing complete history on append. Arbitrary middle edits that preserve those samples cannot be proved absent; these are provider append logs, not an integrity monitor. A write concurrent with a read forces deduplicated replay. Each plain read stops at its opened file's size snapshot, rather than chasing an indefinitely growing append.

## Reliability limits

This recovers records that providers actually persist. It cannot reconstruct:

- tokens a provider never wrote, for example a force-kill before its usage receipt;
- provider files deleted before collection, disabled session persistence, or an unsupported format;
- missing usage counters/IDs or corrupt records;
- legacy Codex snapshots without response receipts, or unknown child ownership on older databases;
- usage from Cursor, Pi, other machines, or unrelated CLI sessions.

Detectable read/format failures are shown as pending/unreadable, not invented numbers. A receipt that never existed cannot always be distinguished from no model work; Shika cannot detect every missing-provider-record case. Transient failures retry; permanently unavailable data cannot be promised complete. There is no measured claim that such failures are frequent or rare. Dates follow recorded receipts when available, with collection-date fallback. A persisted timestamp describes the provider's recording time, not a precise billing instant.

## Dashboard and debugging

View > Usage, no shortcut, opens an 880px dashboard, capped at the window width minus 40px and its available dialog height. Close or Escape restores opening focus. The solid surface, typography, spacing, segment track and controls reuse the app's dialog tokens.

Claude Code, Codex and Combined appear side by side with compact token counts (for example `1.2M`) for this local week through today and the entire visible calendar range. All / Claude Code / Codex filters the daily heatmap and highlights the matching summary column. Click or `h` / `l` / Left / Right steps the segment, stopping at either end as the Settings Theme control does. All is selected on each open. Provider totals stay visible for comparison.

The calendar has 52 week columns, oldest first, with Monday through Sunday rows. It starts on the Monday 51 weeks before this week's Monday and ends on the snapshot's local today (358 to 364 days); future cells are blank. Every past day exists, including zero days. Month labels mark the first Monday in each month. Hovering a day shows its weekday, date and year, exact Claude Code and Codex counts, and their combined total, regardless of the filter. The small Less / More legend names the selected range's maximum tokens per day. Zero has its own bucket; four nonzero buckets divide that maximum into equal intervals with inclusive upper bounds. Filtering recalculates the maximum from the selected provider. Colors are opaque neutral mixes of the theme's overlay and primary ink, specified in DESIGN.md, without provider or status hues.

Loading says "Reading usage..."; an unreadable store says "Could not read usage." Collection gaps say "Some usage is pending or could not be read. Saved totals are retained." Saved totals and the heatmap remain visible with a collection warning. Empty data still shows zero totals and the calendar, plus "No tokens recorded yet." (or "No tokens recorded in this range." when older usage exists). Queued checks alone do not show a collection warning.

Opening signals paced background recovery and reads the saved snapshot off-thread, with the existing request-identity fence against a closed/reopened overlay. The daily range query uses the `(day, cli)` primary-key index and ends at today, as do the compatibility weekly totals; it reads daily aggregates, never receipt history or file checkpoints. Gaps, daily counts and totals share a read-only SQLite transaction. Calendar shaping and totals run on that same background executor. Dates keep the recorded local day when available, otherwise the collection day, with calendar arithmetic independent of DST day length. The collector, schema and provider parsers are unchanged. The fixed-size calendar grid has one cached GPUI entity per open snapshot: unrelated card/timer redraws reuse its subtree, while filter, theme and hover changes invalidate it. Filter/theme updates happen before rendering. Closing drops the entity; there is no global dashboard cache. No live refresh, polling, watcher, refresh control, bill, network request or animation is added.

Inspect `<data-dir>/usage-sources/` for registered path metadata and `usage.sqlite` read-only for `days`, `sessions`, `receipts`, `files`, and `issues`. Never ask contributors to upload transcripts or provider credentials. Recover on Usage open or relaunch after correcting permissions/locks. A `corrupt:` issue is intentionally retained because skipped bytes cannot be counted with certainty.

For isolated core delivery measurements, run `cargo run -p shika-core --release --example usage_performance`. It uses disposable provider/git/app data and a stand-in CLI, checking exact receipt totals, PTY throughput/echo latency, CPU, and RSS. `Core::usage_collection_idle` is a metadata-only diagnostic of queued/executing work, independent of collection gaps; the fixture waits for it before measuring idle CPU. This is not native rendering, real-account usage, notification latency, or a direct OS wakeup count. Recorded measurements and native acceptance limits live in `MANUAL_CHECKS.md`.

Code: `shika-core/src/usage.rs` (calendar/report types), `usage/collector.rs` (worker, readers, transactional storage), `usage/collector_tests.rs` (isolated failure/recovery and indexed snapshot fixtures), `Core::open_agent` (registration/sink), and `Shika::show_usage` / `Overlay::Usage` (background snapshot, filtering and focus fencing), and `shika/src/usage_dashboard.rs` (totals, buckets, heatmap and day tooltips).
