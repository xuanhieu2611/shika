# Subscription limits

Settings > Usage shows Claude Code and Codex subscription observations above the separate [local token totals](token-usage.md). These are provider-reported account allowances, including activity outside Shika, not percentages inferred from tokens. Five-hour and weekly windows show percent used, local reset date/time and an approximate time remaining. An absent window is unavailable, never zero. A passed reset hides the old balance until another observation arrives.

Opening Usage reads a snapshot off-thread. Refresh limits (`r`, only in Usage) rereads Claude's saved observations and permits a bounded Codex metadata request. It does not run a model turn, refresh token charts, start an idle service, or poll while Settings remains open. Displayed times and observation ages belong to that snapshot; reopen or refresh to update them. There is no hosted backend, new HTTP dependency, quota estimate from token totals, or paid inference probe.

Read [AGENTS.md](../AGENTS.md), [design/DESIGN.md](../design/DESIGN.md), and this guide before changing the feature. Native evidence and unverified checks live in [MANUAL_CHECKS.md](../MANUAL_CHECKS.md#subscription-limits).

## Claude Code

The documented [status-line JSON](https://code.claude.com/docs/en/statusline) supplies `rate_limits.five_hour` and `rate_limits.seven_day`, each with `used_percentage` and Unix-second `resets_at`. Claude normally supplies these after API activity; older versions, idle sessions, API-key accounts, managed policy, or missing fields may not provide them. There is no inference fallback or direct OAuth endpoint call.

Before a Claude PTY starts, Shika composes a session-local `--settings` override. It reads the applicable user, project, and local `statusLine` settings in precedence order, retaining nested fields such as padding. The wrapper calls Shika's internal `--shika-statusline` entry point, then the original shell command with the same stdin, stdout, stderr, cwd, and environment. The entry point dispatches before Core, provider discovery, or GPUI starts. It does not edit Claude's settings files or print the input into the terminal. Explicitly disabled, unreadable, unsupported, or conflicting settings are left alone and limits stay unavailable. Managed Claude policy still takes precedence.

Only allowlisted numeric quota fields and observation timestamps enter an owner-only temporary directory. Raw status-line input, conversations and account credentials are not saved. Input/settings capture is capped at 1MiB and saved snapshots at 2KiB. Atomic replacement and a nonblocking file lock prevent overlapping callbacks from regressing the snapshot. Unchanged observations retain their timestamp instead of acquiring false freshness on every redraw. Oversized delegated input is streamed onward without capturing its tail. The user's command retains its exit status, with a ten-second helper timeout. Temporary directories are removed when their source is replaced or dropped; a hard process kill can leave a small numeric snapshot in the system temp directory.

This applies to ordinary Claude PTYs, workers, checkout tasks, Switch agent and Reopen. Claude's native Lead stream-JSON chat does not use a status line and is not a passive source. A worker observation still reflects the account's shared allowance, including Lead usage known to the provider. Do not add status-line settings to the Lead chat transport without separate protocol evidence.

## Codex

The existing paced usage reader also extracts quota metadata from timestamped `event_msg` / `token_count` records. This is independent of token receipts: legacy context counters remain an accounting gap and are never added as fresh tokens. Malformed optional quota fields do not invalidate a durable token receipt. No new rollout scan, watcher, PTY work, or database table is added for limits.

The [Codex app-server protocol](https://developers.openai.com/codex/app-server) supplies fresh metadata on demand. A short-lived `codex app-server --listen stdio://` runs in an empty private cwd with the selected `CODEX_HOME` and login-shell PATH. It performs only initialize/initialized, `account/read` with `refreshToken:false`, and `account/rateLimits/read`. A ChatGPT account is required; signed-out/API-key results clear prior observations. Codex owns authentication. Shika does not open `auth.json`, read the Keychain, implement token refresh, or call the internal WHAM endpoint.

Refreshes coalesce per configuration with a monotonic one-minute floor, including failures. Reopened Settings joins an in-flight read rather than spawning a duplicate. At most four profiles are probed concurrently per refresh; each child has an eight-second protocol deadline, 64KiB line and 1MiB total-output caps, nonblocking I/O, and owned process-group termination/reaping on success or failure. Confirmed app quit kills active probe groups and fences new spawns. There is no persistent app-server child. Provider setup before protocol I/O, OS process creation, and cleanup are not native latency benchmarks.

Map windows by duration, not position: 300 minutes is five-hour and 10080 is weekly. Ignore unknown durations and model-specific pools rather than relabeling them as general quota. Prefer the explicit `codex` bucket on newer multi-bucket replies. Weekly does not imply a Monday reset.

## Attribution, freshness, and storage

Core opts in explicitly with `start_subscription_limits`; ordinary `Core::open` and tests never enable provider requests. The app starts it before usage collection. `subscription_limits` is blocking and must stay on a background executor. No provider I/O runs while rendering or draining a PTY. `stop_subscription_limits` is part of confirmed quit.

Observations live in memory, keyed by CLI and resolved configuration root, capped at 16 roots. Claude's numeric callback snapshot is temporary, not part of `usage.sqlite` or app-data recovery. No quota history, email/account label, access token, or refresh token is persisted. Multiple roots appear as numbered configurations rather than a summed balance. Sources register only after successful PTY spawn.

The latest launch replaces a root's observation/source and fences obsolete in-flight work. For Claude this deliberately means the most recently launched PTY in that root supplies subsequent observations; older concurrent PTYs are not combined across a possible login change. Codex passive records must be newer than that launch boundary, and older timestamps cannot replace newer observations. A successful authenticated Codex refresh replaces the complete snapshot, including missing windows, and disables passive overrides until another launch. Errors retain explicitly aged prior observations except on a detected signed-out/API-key account.

A configuration root is not proof of account identity. Shika does not inspect credentials or monitor external login changes, so it cannot guarantee instantaneous detection of every account switch. A snapshot is an observation from its source, not a continuously verified live balance. External account changes, activity on other devices, provider caching, elapsed resets, and changes while Settings stays open can make it stale. A new launch starts a fresh observation epoch; Refresh checks Codex's current login. Do not combine roots, manufacture a replenished percentage, or silently revive historical logs as current allowances.

## Code and validation

- `shika-core/src/limits.rs`: public snapshots, registration, launch fences, refresh coordination, opt-in Core API.
- `limits/claude.rs`: settings composition, helper, bounded private numeric snapshot and delegation.
- `limits/codex.rs`: metadata-only protocol, deadline, cancellation, and child cleanup.
- `limits/wire.rs`: tolerant allowlisted quota parsing and duration mapping.
- `usage/collector/{receipt,collect,mod}.rs`: quota observations alongside unchanged receipt accounting.
- `Core::open_agent`: launch integration, before the positional prompt and after successful spawn for registration.
- `shika/src/quota_dashboard.rs`, `settings/{mod,view,keyboard}.rs`: neutral quota windows, background requests, opening/refresh identities and Usage-only key.

Fixtures cover absent/malformed/unknown windows, non-regression, configuration bounds, stale launch observations, status-line precedence/padding/no writes, private numeric capture, token-receipt isolation, metadata-only RPC, concurrent refresh coalescing, timeout, quit cancellation and reaping. A built-binary helper smoke verifies exact delegated input/output. Neither fixture data nor passing tests establish real authenticated percentages, CLI format compatibility, all native focus/scroll paths, or measured idle overhead. Keep those limits explicit in manual acceptance.
