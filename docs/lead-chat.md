# Lead chat

A Claude Code, Codex or Pi Lead is a native chat instead of a terminal. The author's messages sit on the right, one final response per turn sits on the left, and a composer sits below. Everything else about the Lead is unchanged: the same installed CLI and login, the same fixed worktree, the same instructions, the same `shika` command, workers, doorbell, resume, Stop and Forget.

Status: implemented for Claude Code on 2026-10-16 (commits `c1f03a0` and `63f98f7`, designed in `0785b3c` and `a100820`) for Codex on 2026-11-20 (protocol boundary `12d79b1`, adapter `5561489`, shared routing `6bed0a3`, then its acceptance), and for Pi the same day (adapter `71cf59f`, then its acceptance). Cursor CLI Leads keep their terminals; their adapter is planned in [Extending to other CLIs](#extending-to-other-clis). Native acceptance records: [Claude Code](../MANUAL_CHECKS.md#claude-code-lead-chat-2026-10-16) (an isolated smoke run, much still open), [Codex](../MANUAL_CHECKS.md#codex-lead-chat-2026-11-20) and [Pi](../MANUAL_CHECKS.md#pi-lead-chat-2026-11-20).

Start here when contributing to this feature. This guide records the decisions and explains how the code works and how to debug and extend it. The Lead itself (lifecycle, worktree, resume, memory, workers) is specified in [lead-agent.md](lead-agent.md); the `shika` command and doorbell in [shika-cli.md](shika-cli.md); the visual contract in [DESIGN.md](../design/DESIGN.md#lead-chat) (its SVG light/dark plates are outdated; the text is the contract).

## Reading paths

- **New to the team:** read the [overview](#two-minute-overview), [evolution](#how-the-feature-evolved), and [decisions](#why-this-design), then [one turn end to end](#one-turn-end-to-end) and the [code map](#code-map).
- **Debugging:** start with [debugging by symptom](#debugging-by-symptom). A reply the CLI wrote but the chat never showed: read [turns the CLI starts itself](#turns-the-cli-starts-itself). Reproduce with [isolated data and a fake CLI](#isolated-reproduction), never normal app data or a personal repository.
- **Adding a CLI:** read [extending to other CLIs](#extending-to-other-clis) and the [guardrails](#contributor-guardrails) first, then [Codex](#codex) and [Pi](#pi) as worked examples of protocols behind `LeadProtocol`.
- **Queued messages:** read [Queued messages](#queued-messages) before touching Enter, the composer's Send/Stop action, the doorbell gate, or any path that drops a chat.
- **Changing visuals:** read [DESIGN.md](../design/DESIGN.md#lead-chat) and the [approved design values](#approved-design-values). Every chat color comes from the active theme.

## Two-minute overview

- **Before:** every Lead was a CLI in a PTY, drawn by the terminal view. The author read thinking, tool calls, tool output and narration in the CLI's own UI. The doorbell was a bracketed paste plus a delayed Enter.
- **Now:** a chat Lead runs as one long-lived structured child over pipes: `claude -p` speaking bidirectional stream JSON, `codex app-server` speaking JSON-RPC ([Codex](#codex)), or `pi --mode rpc` speaking JSONL ([Pi](#pi)). It has no PTY (`Session::pty` is `None`). Shika shows only the author's text and each turn's final response, rendered as light markdown. Cursor CLI Leads are still terminals.
- **Hidden:** thinking, tool calls, tool output, subagent text and every intermediate assistant message. They are dropped before anything is written to history.
- **History:** Shika keeps a visible transcript for the Lead's lifetime in `leads/<project-id>/chat/history.sqlite`. Stop and quit keep it, Forget deletes it, memory notes survive. It is never replayed into the CLI.
- **Unchanged:** the Lead guide and prompts, the worktree, the four-worker limit, ownership checks, the doorbell's gates and accounting, the author-confirmed Close dialog (`shika pr` publishes without a dialog since protocol 6), resume and the fresh-start fallback, Seen and notifications. Workers keep real terminals.
- **Start reading code:** `Core::start_chat_lead` (`crates/shika-core/src/lead.rs`), `ChatTransport` (`lead_transport.rs`), the `LeadProtocol` trait (`lead_transport/protocol.rs`) and its implementations, `ClaudeStream` (`claude.rs`), `CodexAppServer` (`codex.rs`) and `PiRpc` (`pi.rs`), the `Actor` that drives them (`actor.rs`), `ChatStore` (`lead_chat_store.rs`), then `Shika::drain_lead_chats` (`crates/shika/src/lead_chat/glue.rs`). One gate, `shika_core::lead_chat_supported`, decides which CLIs the app runs as chat Leads.

## How the feature evolved

| Stage | What changed | Why the next change was needed |
| --- | --- | --- |
| Terminal Lead | The Lead was an ordinary CLI terminal in its fixed worktree, steered by the author and by `[shika]` lines pasted into its PTY. Shika stored no transcript. | A manager conversation is mostly reading results. In a terminal the author scrolls past thinking, tool calls, command output and narration to find the answer, and the doorbell looks like author input. |
| Chat proposal (`0785b3c`) | A design for a native chat over each CLI's structured mode, with per-CLI transports, final-only rendering, and a persistence comparison. | Several product clauses (no transcript, no chat client) conflicted, and history lifetime, migration, and theming needed the author's decision. |
| Author decisions (`a100820`) | UI-only change; final response only; Shika-owned lifetime history; Forget deletes it; memory survives; existing saved Leads resume in chat with empty history; all colors from the theme; approved geometry. | Claude Code had the strongest evidence (realtime streaming input and a native question channel), so it went first. |
| Claude Code chat (`c1f03a0`, `63f98f7`) | Pipe supervisor, final-response filter, SQLite history, native question dialog, chat view, composer and markdown renderer. Fixtures recorded from the installed CLI against a local synthetic server. Product clauses in AGENTS.md and CONTRIBUTING.md narrowed to this exception. | Other CLIs need their own transport evidence before they switch; until then they keep terminals. The per-turn plans for Codex and Cursor CLI would also have given each a second process model. |
| Research and evidence (2026-11-20, `895e46a`) | T3 Code and Orca, two MIT multi-agent apps, both run Codex as `codex app-server`, Pi as `pi --mode rpc`, and Cursor CLI over ACP (Orca). Fixtures recorded from Codex 0.162.1, Pi 1.4.0 and Cursor CLI 2026.11.19 replaced the per-turn plans with one long-lived structured child per Lead for every CLI. The author answered nine decisions (see [Why this design](#why-this-design)). | The actor knew only Claude's wire. |
| Protocol boundary (`12d79b1`) and shared gate (`6bed0a3`) | `LeadProtocol` holds what differs per CLI (arguments, env, handshake, send, answer, interrupt, shutdown, decoding, strings); the actor, fencing, store and events stay shared. A JSON-RPC 2.0 helper. `start_chat_lead(project, cli, launch)` with three identity sources, and `lead_chat_supported` as the one app gate. Claude behavior and history byte-identical. | A second protocol had to prove the boundary. |
| Codex chat (`5561489`, acceptance 2026-11-20) | `codex app-server` adapter: final-only filter on `phase`, native `request_user_input` questions, `turn/interrupt` with a restart when a command keeps running, instructions injected on resume when they changed, usage registered by thread id. The native acceptance run found that chat CLIs inherited the app's blocked SIGINT and SIGTERM, a stray "Reply stopped" row at quit, and toasts hidden behind the chat; all three were fixed for every chat CLI before Codex joined the gate. | Pi, then Cursor CLI, each behind its own acceptance. |
| Pi chat (`71cf59f`, acceptance 2026-11-20) | `pi --mode rpc` adapter: final-only filter released at `agent_settled`, Shika's Lead-only `shika_ask_author` extension tool mapped to the native dialog, `abort` that cancels open dialogs first, the session id saved at Pi's first user message. The native acceptance run found that Pi silently starts an empty session for a saved id it cannot load, so a chat Lead would have lost its context with no toast; a resume reporting no messages is now refused before Pi joined the gate. | Cursor CLI, behind its own acceptance. |

## Why this design

Each decision below is the author's. Do not relitigate them in a change; a change in direction comes from the author, and this guide changes with the code.

**A UI change only; Lead behavior stays.** The Lead's value is its management loop, which already worked. Changing how the author reads it should not change what the Lead is told, when it runs, or what it may do. So the guide (`crates/shika/src/lead_guide.md`) and memory notes travel through the same `--append-system-prompt` flag, the worktree and control environment come from the same `start_lead_inner`, and resume, fresh fallback, toasts, Seen, Stop/Forget and the doorbell keep their rules. No prompt was added to make replies "chat friendly" or to stop the Lead narrating commands.

**Only the author's message and the turn's final response are shown.** The author wants the answer, not the process. One bubble per turn keeps the transcript readable and makes history small. The final response may contain bash or code blocks; that is content, rendered as code, not filtered by wording. The filter is about provenance and finality, never about what the text says.

**Thinking, tool calls, tool output and intermediate assistant text are hidden, and never stored.** Hiding after storage would still leak into history and into later features. So the decoder keeps a private candidate, invalidates it on any later tool call or tool result, and releases it only on a successful terminal `result`. There is no expandable tool card, raw log drawer or hidden terminal to fall back on. If finality cannot be established the turn fails visibly and the candidate is dropped.

**Shika owns the visible history for the Lead's lifetime.** The CLI's conversation is model context; the transcript is the author's record. They have separate identities and lifetimes. Reading each CLI's private files would mean four undocumented layouts, compaction and pruning, tool/thinking filtering, and no dependable Cursor reader, and a provider upgrade could reorder or erase what the author saw. A Shika-owned store gives the same scrollback across CLIs with exact provenance. The rejected and chosen options are compared in [the persistence decision](#the-persistence-decision). Lifetime means: Stop and quit keep it; Forget deletes it together with the saved conversation link, so the next Lead starts empty; memory notes (`memory.json`) are separate and survive Forget, as before. No age or size limit; rendering is bounded instead.

**Every color derives from the active theme.** The chat sits where the terminal sits and must work in every catalog theme, light and dark, solid and frosted. Geometry was approved; colors were not, so there are no chat hex values. `LeadChat::render` reads `Chrome` fields and applies `appearance::legible` at 4.5:1 against the real surfaces. See [DESIGN.md](../design/DESIGN.md#lead-chat).

**Claude Code first, then one CLI at a time.** Claude's installed help advertises `--input-format stream-json` (realtime streaming input), so one process can hold the conversation across turns, and its stdio permission channel preserves native `AskUserQuestion` questions in print mode. Shipping one well-evidenced adapter beats four guessed ones. The gate is per CLI (`lead_chat_supported`): a preset switches to chat only in the commit that records its adapter's fixtures and native acceptance; until then it keeps its terminal Lead. The author's order is Codex, then Pi, then Cursor CLI.

**One long-lived structured child per Lead, for every CLI; no SDK.** Research on 2026-11-20 into T3 Code (pingdotgg/t3code) and Orca (stablyai/orca), both MIT, found the structured modes this guide had missed: both run Codex as `codex app-server` (JSON-RPC over stdio) and Pi as `pi --mode rpc`; Orca runs Cursor CLI as `cursor-agent acp` (Agent Client Protocol), T3 Code embeds `@cursor/sdk`. Their agent SDKs are TypeScript or Python wrappers over the same pipes, so Shika, pure Rust, writes each protocol from the CLI's own schema and docs and copies no code. One child per Lead keeps the Claude actor's process model, so the actor, fencing and store are shared. Both apps also tell their coordinator to end its turn instead of polling, which is the doorbell rule; T3's MCP tool injection needed workarounds (Codex code mode yields after about 30 seconds, Pi has a 60 second MCP timeout), so Shika keeps its `shika` command.

**The author's decisions for the other CLIs (2026-11-20).** Accept Codex's `[experimental]` app-server with per-version fixtures and a visible "This Codex version could not be read". On resume Codex ignores new developer instructions, so send the current guide and memory as one `thread/inject_items` developer message, only when they changed. Stop generating sends `turn/interrupt`, and SIGTERMs app-server when a command is still running, resuming on the next send. Native questions need `--enable default_mode_request_user_input`; cancel answers `{"answers":{}}`. Goals are off (`--disable goals`), so Lead turns come only from the author and the doorbell. Shika never approves a permission request for the Lead: Cursor CLI launches `agent --force --trust --sandbox disabled acp` (ACP ignores `--yolo`), and a request that still arrives is blocked, never answered. Cursor's instructions go in a temporary always-applied `.cursor/rules` file at `session/new`; a terminal-era Cursor chat cannot be loaded over ACP, so it starts fresh with the refused-resume toast; memory changed since the session was created reaches a reopened Cursor Lead as one `[shika]` memory line. Pi gets a Lead-only question tool in Shika's own extension. Real accounts may be used to record fixtures, in disposable repositories and data, sanitized; a synthetic server stays first choice.

**Shika's own markdown renderer, not Zed's.** Zed's markdown, editor, terminal and terminal-view crates are GPL-3.0 and Shika is MIT. The renderer in `lead_chat/markdown.rs` is deliberately small: bold, italic, inline code, fenced code, bullet and check lists, one indent level, headings as bold paragraphs, and validated `http`, `https` and `mailto` links. No HTML, images, remote content, terminal escapes, syntax highlighting, or executable controls. The composer and the selectable bubble text (`chat_input.rs`) follow the pinned GPUI input example's `EntityInputHandler` contract.

**Transport per CLI, chosen from its own help.** Each CLI exposes a different structured mode, so there is no shared wire format to target:

| CLI | Installed version checked | Help checked | Process model |
| --- | --- | --- | --- |
| Claude Code (implemented) | 2.1.302 | `claude --help` | One long-lived child, bidirectional stream JSON |
| Codex (implemented) | codex-cli 0.162.1 | `codex app-server --help`, `codex features list`, `generate-json-schema` | One long-lived `codex app-server` child, JSON-RPC 2.0 |
| Pi (implemented) | 1.4.0 | `pi --help`, the package's `docs/rpc*.md` | One long-lived `pi --mode rpc` child, JSONL |
| Cursor CLI (planned) | 2026.11.19-f005e9d | `agent --help`, `agent acp --help` | One long-lived `agent acp` child, ACP 1 |

Help verifies flag spelling and advertised modes only, not record schemas or multi-turn behavior. That is why each adapter also needs recorded fixtures.

**Also rejected:** a model API or provider SDK (Shika runs the author's own CLI and plan); a PTY behind the chat with screen scraping (fragile, and it would still need final-response extraction); replaying saved bubbles into a resumed CLI (duplicates context and can repeat actions); a recap or migration notice for existing Leads (the CLI already has its context).

### The persistence decision

| Option | Benefits | Costs |
| --- | --- | --- |
| Read each CLI's own files on open | No second copy; could show pre-chat messages | Four private layouts, compaction and branching, tool/thinking filtering, missing or pruned files, no dependable Cursor reader, slow reads, and provider upgrades changing what the author saw. |
| Shika-owned visible transcript per project (chosen) | Same scrollback across CLIs, exact provenance, interruption markers, stable order, no provider parser for normal chat | New local content storage and crash recovery. Old terminal history is not imported. It does not restore provider context. |

## How it works

### Processes and threads

```text
GPUI main thread                         Core (shika-core)
  LeadChat entity (per project)            chats: id -> Arc<ChatTransport>
  Shika::drain_lead_chats (each frame) --->  ChatTransport::drain()  (events channel)
  ChatAction::Send / Interrupt ----------->  ChatTransport::send / interrupt (commands channel)
                                           "lead-chat" actor thread: owns ChatStore + child
                                             stdout reader thread -> frames (sync_channel(8))
                                             stderr drain thread -> sink
                                           claude -p, codex app-server or pi --mode rpc (own process group)
```

This section describes the shared machinery and Claude's protocol; [Codex](#codex) and [Pi](#pi) cover what differs for them.

- **One actor per live Lead.** `ChatTransport::start` opens the store, recovers an unfinished turn, binds the context, spawns the child, and starts the `lead-chat` thread. All store writes and all pipe IO happen on that thread. The UI only sends commands and drains events, so a Lead off screen still has its pipes drained.
- **The child.** `Process::spawn` runs the resolved CLI in the Lead worktree with its own process group (`process_group(0)`), `PATH` with the control `bin` directory first, `PWD`, `SHIKA_SOCKET` and `SHIKA_TOKEN`. It removes the git redirect variables, the worker report variables (`pty::CONTROL_VARS`, so a Lead never inherits a report credential), and the protocol's own (`LeadProtocol::env_remove`; for Claude `CLAUDE_CODE_CHILD_SESSION` and `CLAUDECODE`, so a Shika started from inside Claude Code still gets a persisted Lead conversation). Before exec it resets SIGINT, SIGTERM and the other job signals to their defaults and empties the signal mask (`reset_signals`, what `portable-pty` does for workers): Rust's spawn keeps the caller's mask, and the app's threads block SIGINT and SIGTERM, so until 2026-11-20 every chat CLI ignored both and only SIGKILL stopped it. stdin is set nonblocking. The first write is the protocol's handshake (`LeadProtocol::handshake`; Claude's initialize control request).
- **Arguments** (`ChatLaunch::args`): `-p --input-format stream-json --output-format stream-json --verbose --dangerously-skip-permissions --append-system-prompt <guide plus memory> --permission-prompts host --permission-prompt-tool stdio`, then `--session-id <uuid>` for a fresh conversation or `--resume <uuid>`. All are built as an argument array; nothing passes through a shell.
- **Framing.** The reader reads newline-terminated frames up to 8 MiB. A longer or unterminated frame is a `BadFrame`. The frame channel holds eight frames, so a flood blocks the reader, not memory. Every frame and EOF carries the process generation; frames from an older child are ignored.
- **Initialization.** If the handshake (Claude's `shika-initialize` control response) does not finish within 10 seconds, the turn fails with the protocol's unreadable message ("This Claude Code version could not be read", "This Codex version could not be read", "This Pi version could not be read") and the child is stopped.
- **Usage.** `start_chat_lead` registers the Lead as a usage source when usage collection runs, and the actor pokes it on every event, so Lead tokens reach the Usage dashboard the same way PTY output does for workers: Claude by its worktree, Codex by thread id once the handshake reports it (see [Codex](#codex)); Pi is not a usage source. See [token-usage.md](token-usage.md).
- **Quit.** `cx.on_app_quit` calls `Core::stop_lead_chats`, which stops and joins every actor the protocol's way (`LeadProtocol::shutdown`): Claude gets SIGINT, Codex and Pi have their stdin closed; a CLI still running 2 seconds later gets its stop signal, and SIGKILL 2 seconds after that. GPUI may keep window state alive until exit, so RAII alone is not enough. Stop Lead goes through `Core::hang_up`, which removes and stops the transport. `ChatTransport` and `Process` also stop and reap on drop.

### One turn end to end

1. **Typing.** The composer is a `ChatInput`. Each change sets `LeadChat::last_typed`, which feeds the doorbell's 3 second typing gate.
2. **Enter.** `ChatInput` emits `Submit` (Shift+Enter inserts a newline; IME composition consumes Enter first). `LeadChat::submit` asks `Lane::route`. While the Lead is busy or a send is in flight the text is queued instead (see [Queued messages](#queued-messages)); a blank draft, composition, a resting Lead and a save failure do nothing. Otherwise it trims outer blank lines, remembers the exact draft as `pending`, sets `saving`, and emits `ChatAction::Send`.
3. **App dispatch.** The subscription in `Shika::ensure_lead_chat` calls `submit_lead_chat`. It refuses while the card is in an active or blocked turn, calls `Core::lead_chat_send`, and advances the card with `Activity::advance_chat(Working, submitted: true)`, which starts the turn clock.
4. **Outgoing record.** The actor checks there is no active turn, no stop in progress, no save failure, and the same cancel epoch as when Send was queued. It allocates the next turn number and writes the author text, any Shika event rows, and an `Outgoing { origin }` marker in one FULL synchronous transaction. The `Record` events reach the view; the author bubble appears, and the composer is cleared only if its text still equals `pending`.
5. **Spawn if needed.** If the previous child was interrupted, a new one starts here with `--resume` and emits `Resuming`.
6. **Write.** The decoder's per-turn state is reset (`begin`), then `{"type":"user","message":{"role":"user","content":<text>}}` is written with a 3 second deadline, retrying on `WouldBlock` and aborting if the cancel epoch moves. Success records `Accepted` and emits `Accepted` and `Status(Working)`.
7. **Identity.** Claude emits `system/init` with the session id at each turn. It must equal the requested id. The first confirmation emits `Confirmed`; the app calls `Core::confirm_lead_chat` off thread, which saves `lead.sessionId` only for that exact live Lead, under the operations lock.
8. **Decoding.** Assistant and user records update the private candidate (see [final-response extraction](#final-response-extraction)). Questions open the native dialog (see [questions](#questions-and-unknown-interactions)).
9. **Result.** A successful `result` returns the candidate. `Actor::complete` writes the `Lead { text }` record (if there is text) and `Settled` in one transaction, then emits `Record` and `Status(Ready)`.
10. **App.** `drain_lead_chats` maps `Status(Ready)` through `advance_chat`, which finishes the turn and posts the turn's single notification (titled with the project name, "Lead", and the status). The view parses the record off thread into markdown blocks and appends it. Seen follows the normal rule, with chat focus standing in for terminal focus.

### Turns the CLI starts itself

Not every turn starts with a Shika write. When a Bash tool the Lead ran with `run_in_background` finishes (typically a long `shika wait`), Claude Code queues an internal task notification and, with no stdin input, opens a new turn and answers it. The Lead's reply in that turn is usually the one the author is waiting for ("the README worker finished, here is what it did").

**What.** Such a turn is an ordinary Lead turn: the card goes Working, then Ready with its own clock and notification; the final response is filtered exactly like any other (final only; thinking, tools, tool output and intermediate text are dropped before storage) and shown and saved as a Lead bubble; there is no author bubble.

**Why.** Before this was handled, the decoder only read `assistant` and `result` records while a Shika-submitted turn was active, so everything the CLI produced after a turn settled was dropped. On 2026-10-17 an author's Lead replied twice after its background waits returned; neither reply was recorded, the card stayed Ready with no notification, and the author had to ask "can you check on the worker?". The CLI's own transcript held both replies (each turn opened by a `<task-notification>` queue entry). The doorbell was correctly silent: the Lead's own `wait` reported those worker settlements, so nothing was due.

**How.** Claude Code 2.1.302 opens every turn, Shika's or its own, with `system/init` (recorded in `claude-2.1.302-background-local.jsonl`; startup and `--resume` print no `init` until input arrives). Before it the CLI prints `system/task_updated`, `system/task_notification` and `system/background_tasks_changed`, which are ignored, and no `user` record: the notification text never reaches stdout.

1. `ClaudeStream::parse` treats a `system/init` while no turn is active as the start of a turn: it resets the per-turn state (`begin`) and returns `Parsed::Started`. Other records while idle still start nothing, so a stray late record cannot hold the card at Working.
2. The actor allocates the next turn number (`Actor::allocate_turn`, which also counts rows buffered after a save failure), stamps the child's input epoch so a question answer in that turn is not refused as stale, writes one `Accepted` row, and emits `Confirmed`, `Started` and `Status(Working)`.
3. The app maps `Started` to `advance_chat(Working, submitted: true)`: a new turn with a fresh clock and notification. A bare `Status(Working)` after Ready would only continue the settled turn, with its old clock and no notification. The chat becomes busy, so the composer, `submit_lead_chat` and the doorbell gate wait for it like any running turn.
4. The rest is the normal path: questions, Stop generating, the successful `result`, failures and exits. Completion writes `Lead` and `Settled`. Failures add their event row, but no Retry message or `DeliveryFailed`, since nothing was sent.

**History.** The turn's rows are `Accepted`, then `Lead` (if it had text) and `Settled`, with no `Author` or `Outgoing`. `Accepted` without `Outgoing` is how a CLI-started turn is recognized. Using an existing kind keeps the store at `user_version` 1, so older builds still read it, and recovery already does the right thing: an unsettled CLI turn becomes "Reply interrupted by restart".

**Crossing.** Stream JSON carries no correlation between a user record and a turn. If the author's send reaches the actor just before the CLI's own `init`, the notification reply settles the author's turn and the author's answer arrives as the next, CLI-started turn. Both are shown, in arrival order; nothing is dropped. The window is the actor's queue latency, and the app refuses a send while the Lead is busy, so it is narrow.

### Final-response extraction

`ClaudeStream::parse` (one decoder per process, reset per turn) decides what may become the bubble:

- Records with a non-null `parent_tool_use_id` are subagent output: ignored, and they never invalidate the main candidate.
- `assistant` records are deduplicated by their record `uuid` (or, without one, message id plus a content hash). Claude 2.1.302 splits text and tool blocks of one message into separate records that share `message.id`, so identity is the record, not the message.
- An assistant record with a `tool_use` or `server_tool_use` block clears the candidate. A `user` record containing a `tool_result` clears it too: text emitted before a tool call is commentary.
- Text blocks of the same `message.id` append to one candidate, joined by a blank line. A text block from a different message replaces the candidate. `thinking` and `redacted_thinking` are skipped. Any other block type fails the turn as an unreadable stream.
- An assistant record with an `error` fails the turn.
- On `result`: if initialization, `system/init` confirmation or the session id is missing or wrong, the turn fails as unreadable. If `is_error` is not false or `subtype` is not `success`, it fails with "Claude Code could not complete the reply. Check its sign-in and connection, then retry." Otherwise the candidate (possibly none) is the final response. `result.result` is never used: some releases aggregate commentary into it.
- A turn that completes with no final text creates no bubble, only `Settled`.
- `stream_event` partials and unknown record types are ignored, never printed.
- A `system/init` while no turn is active starts a turn the CLI began itself (see [turns the CLI starts itself](#turns-the-cli-starts-itself)). `assistant`, `user` and `result` records outside a turn are ignored.

### Questions and unknown interactions

`--permission-prompts host --permission-prompt-tool stdio` routes Claude's permission requests to stdin/stdout as `control_request` records. With `--dangerously-skip-permissions` the only one expected is `can_use_tool` for `AskUserQuestion`. Without this channel, print mode drops `AskUserQuestion`, so the flags preserve today's question behavior rather than add approvals.

- **Supported:** `can_use_tool` for `AskUserQuestion` during an active turn, with 1 to 4 questions, each with a non-empty question and at most 4 options. The actor records the request id, emits `Status(Asking)` and `Question`. The app opens `Overlay::LeadQuestion` (`lead_chat/question.rs`), or queues it in `Shika::leads.questions` (`lead::Leads`) if another overlay is open or the author typed within the last 3 seconds (`author_quiet`, the doorbell's quiet); a question never replaces an open dialog, and keys other than Escape are ignored for 300ms after it appears (each question of a request restarts that window).
- **Answer:** the dialog collects one answer per question (an option label, a list of labels for multi-select, or free text), then `Core::lead_chat_answer` writes `{"behavior":"allow","updatedInput":<original input with "answers">}`. Cancel writes `{"behavior":"deny","message":"The author cancelled this question."}`. A successful write returns to Working. The question and its tool reply are never stored.
- **Dismissal:** `control_cancel_request`, a settled turn, or an exit closes the dialog and drops queued questions for that Lead.
- **Unknown:** any other `control_request`, or a question outside those limits, emits `Status(Asking)` and the error "This Lead needs an interaction chat does not support." It is never answered automatically. The author can Stop generating.
- **PR and Close:** `shika close` still opens the author's own Close dialog through the control socket. While a `LeadDialog` for this Lead is open, `drain_lead_chats` holds the card at Asking and the activity line reads "Waiting for your decision". A chat message can never confirm it. `shika pr` opens no dialog: it publishes and the Lead stays Working, with the step "Opening a PR for {task}...", until the URL comes back (see [lead-agent.md](lead-agent.md#pr-publishes-directly)).

### Stop generating

Escape in the chat (outside IME and overlays) or the Stop button sends `ChatAction::Interrupt`, which is today's Escape for a terminal Lead, applied to the turn only:

1. `ChatTransport::interrupt` bumps the cancel epoch (so a blocked write aborts at once) and queues `Interrupt`.
2. The actor drops the candidate, sends SIGINT to the child's process group, and emits `Stopping` ("Stopping...").
3. After 2 seconds it sends SIGKILL. Exit is observed after the reader reaches EOF, or after 3 seconds if a descendant still holds the pipe.
4. The turn settles with the event "Reply stopped" and `Settled`; no partial text is shown or saved. The child is dropped and the actor stays.
5. The next Send spawns a fresh child with `--resume` on the same confirmed id.

Codex stops differently: see [Codex](#codex). With Claude, shell tools the Lead was running, including a blocking `shika wait` client, normally inherit that process group and stop too (a tool that starts its own group escapes the signal and is reaped by nothing in Shika); the socket disconnect releases the doorbell gate as for any abandoned wait. Workers are separate PTYs owned by Shika and keep running (asserted by `claude_chat_has_no_pty_confirms_identity_and_keeps_history_until_forget`). Stop generating is not Stop Lead: `⌘⇧W` and the header button still open the existing Stop/Forget confirmation.

### Failures and exits

| Situation | What the code does |
| --- | --- |
| Write fails or times out | Event "Delivery unknown. Work may already have started. Send again only when you are ready to retry.", the child is stopped, and the author's text is offered through Retry message. Nothing is resent automatically. |
| Provider error result | Event "Claude Code could not complete the reply. Check its sign-in and connection, then retry." (Codex: "Codex could not complete the reply. ..."). The child is stopped; the next Send resumes. Retry message is offered. |
| Unreadable stream, oversized frame, or no handshake in 10 seconds | Event "This Claude Code version could not be read" or "This Codex version could not be read" (with the version when known). The transport exits and the Lead rests. |
| Different session id | Event "Claude Code returned a different conversation" (or "Codex returned a different conversation"). The transport exits; the saved id is never overwritten. |
| Codex refuses `thread/resume` | Event "Codex could not resume this conversation"; the transport exits, so the 5 second refused-resume rule below applies. |
| Pi resumes a saved id with no messages | Event "Pi could not resume this conversation"; the transport exits, and the same rule applies (see [Pi](#pi)). |
| Pi refuses a prompt | "Pi was busy and did not take the message. Send it again." or "Pi could not complete the reply. ...", and the text is offered again; the process is kept. |
| Child exits during a turn without a result | Event "The reply ended before it was complete", then exit handling below. |
| Child exits while idle | Event "The Lead stopped unexpectedly". |
| Any unexpected exit | `ChatEvent::Exit`; the actor ends. The app marks the host exited. Within 5 seconds of a resumed launch it is a [resume refusal](#conversation-identity-resume-and-the-fresh-start-fallback); otherwise the Lead rests with the toast "The Lead stopped unexpectedly. Select it to resume." and the chat shows "Lead is not running" with Resume. |
| Send while busy or after a save failure | The composer queues the text while the Lead is busy ([Queued messages](#queued-messages)); after a save failure Enter does nothing until Retry save. A direct send the transport refuses (retry, doorbell) gets the error "The Lead is still replying or the conversation needs to be saved." |
| History write fails | "Could not save this conversation" with Retry save. Records are kept in memory and appended on retry; sends are refused until then. Quitting before a successful retry loses those unsaved rows. |

Errors are muted event rows or a line above the composer, never fake assistant bubbles. stderr is drained and discarded; no raw JSON, stderr or token is painted.

### Conversation identity, resume and the fresh-start fallback

- **Choosing.** `start_lead_inner` keeps today's `choose_conversation`: resume when the saved record names the same CLI, has a valid id, and `CliHome::session_exists` finds it (Claude: `<claude config>/projects/*/<id>.jsonl`, `CLAUDE_CONFIG_DIR` honored; Codex: an unarchived `threads` row or a rollout file, `CODEX_HOME` honored; Pi: `<sessions>/<cwd folder>/<timestamp>_<id>.jsonl`, `PI_CODING_AGENT_DIR` honored); otherwise fresh. A fresh Claude or Pi Lead gets a new UUID from Shika; a fresh Codex Lead gets none, and its handshake reports one (`lead::Identity`).
- **Provisional id.** For a fresh chat Lead, `projects.json` saves `sessionId: null` at start. The id is committed only when the CLI confirms it at the Lead's first turn (Claude's `system/init`, Codex's first `turn/started`, Pi's first user `message_end`), via `confirm_lead_chat` (live Lead, a chat CLI, transport present, saved CLI is the same, no different id saved). A Lead stopped before its first message therefore has no id and starts fresh next time.
- **Resume.** A resumed start passes `--resume <id>` immediately. After an interrupt, a respawn on the next Send also uses `--resume` and emits `Resuming`, which resets `LeadRun::resumed_at`.
- **Refusal.** If the transport exits within `model::RESUME_GRACE` (5 seconds) of a resumed launch, `lead_upkeep` asks `saved_lead_session_exists` off thread. Known missing: clear the id, start fresh once, toast "Could not resume the Lead's conversation. Started a new one." Exists or unknown: keep the id, rest, toast "The Lead's CLI exited right after resuming. Select it to try again, or Forget it to start a new conversation." This is the terminal Lead's logic, unchanged. The toast is drawn over the chat as over a terminal (before 2026-11-20 it was drawn only on the terminal side, so a chat Lead's toasts never showed), and going live again clears the old process's "The Lead stopped unexpectedly" line (`LeadChat::went_live`).
- **Migration.** A saved terminal Lead (Claude Code, Codex or Pi) keeps its CLI and id and resumes in chat. Its visible history starts empty; nothing is imported, recapped, or submitted to populate the view. A Codex thread that never received instructions through chat gets them once as a developer message (see [Codex](#codex)); that is model context, not a visible turn. A Pi Lead gets the current guide and memory in its launch's `--append-system-prompt`, as on every start.
- **CLI change.** If the author picks another CLI, `SavedLead::chat_history` is carried over, so a later Claude Lead reopens the same lifetime history. History is never sent to the other CLI.

### History storage

Location: `<data dir>/leads/<project-id>/chat/history.sqlite`, next to `memory.json` but separate from it. `chat_dir` accepts only `[A-Za-z0-9-]` project ids. `ChatStore::open` refuses symlinked or non-directory `leads`, project and `chat` directories and a symlinked database, creates directories `0700`, and sets the file to `0600`. SQLite uses `journal_mode=DELETE`, `synchronous=FULL`, and a 3 second busy timeout. `user_version` is 1; a newer version is refused with "This conversation uses a newer Shika version".

| Table | Contents |
| --- | --- |
| `metadata` | `lifetime`: a random id created once per store. `context`: the JSON `[cli, conversation]` of the current binding. |
| `records` | Immutable append-only rows: `id`, `turn` (0 for events outside a turn), `timestamp` (local Unix seconds, never provider timestamps), `segment` (context number), `visible`, `content` (JSON `ChatContent`). Indexed by `(visible, id)` and `(turn, id)`. |
| `visible_entries` | The projection pages read: one row per visible entry, `event_key UNIQUE`. A keyed event upserts: the entry keeps its first id and position but points at the newest record. Rebuilt from `records` when the table is missing. |

`ChatContent` kinds: `Author { text }`, `Lead { text }`, `Event { text, key }` are visible; `Outgoing { origin }`, `Accepted`, `Settled`, `Context { cli, conversation }` are bookkeeping. `ChatOrigin` is `author` or `shika_event`. A turn with `Accepted` and no `Outgoing` is one the CLI started itself.

- **Binding.** `SavedLead::chat_history` in `projects.json` (`chatHistory`) holds the store's `lifetime`. A start or page read whose saved lifetime differs from the store's fails with "Could not read this conversation: history identity changed" rather than showing someone else's transcript. `bind(cli, conversation)` writes a `Context` row when the binding changes, plus the visible event "A new Lead conversation started" if there was an earlier context.
- **Transactions.** Author, events and `Outgoing` commit together before dispatch. `Accepted` commits after the write, or alone when the CLI starts a turn itself. Final text and `Settled` commit together. There is no cross-file exactly-once claim between `projects.json` and the store.
- **Crash recovery.** `recover` runs at every transport start. If the last turn has no `Settled`, it appends "Reply interrupted by restart" (if `Accepted` exists) or "Delivery unknown" (if not), then `Settled`. Nothing is resent.
- **Reading.** `Core::lead_chat_page(project, before)` returns up to 60 visible entries before an id. The view loads the newest page on first render, loads older pages when scrolled near the top, keeps at most 180 records in memory, and never prunes the store. A failed or corrupt store is left untouched and reported; sends are blocked.
- **Deletion.** Forget calls `lead_chat_store::delete` (refusing redirected paths) before clearing the saved Lead; `memory.json` stays. Project removal deletes the whole `leads/<project-id>/` folder, as before.

### Doorbell and restart summary

The ledger, batching, and gates are the ones in [shika-cli.md](shika-cli.md#the-doorbell); only delivery changes.

- **Gate.** `lead_gate` reads the chat for a chat Lead: idle also requires not saving, not busy, no save failure and no queued author messages (`chat_quiet`); draft is "the composer has text"; last typed is the composer's; alive is `LeadChat::live`.
- **Bell.** `ring_doorbells` builds the same `[shika] Workers changed: ... Run shika wait.` line and submits it with origin `ShikaEvent`, plus one visible event row per worker: "<title> is ready to check", "<title> is asking you", or "<title> exited", with the CLI in parentheses only when titles repeat. Each row is keyed `worker:<id>:<turn>`; a later report on the same worker turn replaces the row's text with "... - Report updated" in place. The pending marks are held in `Card::chat_delivery`.
- **Accounting.** `chat_delivery_finished` marks the batch rung only on `Accepted` (a successful pipe write). On delivery failure nothing is marked and the bell rings again later.
- **Restart summary.** For a chat Lead the summary is not held for quiet output. Rows "Shika restarted. Worker worktrees were kept." and, when the checkout was not moved, "The Lead checkout was kept without moving to the latest base." are shown; the detailed line goes privately to the CLI. `reportedWorkers` is updated only on `Accepted`; on failure the summary is restored for another try.
- Internal input never becomes an author bubble, and starts a normal turn with its own notification.
- A turn the CLI starts itself makes the Lead busy, so the bell waits for it. When the Lead ran `shika wait` in the background, that wait reports the settlements it returns, and they never ring.

### Status, clock, notifications and Seen

| Status | Chat evidence |
| --- | --- |
| Waiting | Live Lead before its first turn. Spawning and initialization start nothing. |
| Working | A submission was dispatched (`advance_chat(Working, true)`), accepted, the CLI started a turn itself (`ChatEvent::Started`, also a submission), or a question was answered. Includes tools, silence and a blocking `shika wait`. The activity area shows [step lines](#activity-steps) built from the control socket, "Working..." when no command runs. |
| Asking | A supported question or unknown interaction is pending, or a `shika close` dialog for this Lead is open (label "Waiting for your decision"). |
| Ready | The turn settled: final result, failure, or "Reply stopped". |

`Activity::advance_chat` applies explicit transitions only: no screen sampling, quiet fallback or idle debounce (chat cards are skipped in the terminal sampling loop). The turn clock starts at dispatch and survives Asking and back. One notification per turn at the first blocker or completion. Seen is unchanged: chat focus (composer, transcript, header, or any bubble text) counts as terminal focus, or the card stays selected for one second.

### The view

`LeadChat` is one GPUI entity per project, created by `ensure_lead_chat` for every project whose saved Lead runs on a chat CLI (`lead_chat_supported`), so a resting Lead shows its history and drafts and scroll position survive card switches in memory. `chat_side` replaces the terminal side for a selected live or resting chat Lead, and the toast is drawn over it, with the header (antler mark tile, "{project} Lead", the worker summary on a live Lead, Stop Lead `⌘⇧W` or Resume `↵`). The list is a GPUI `list` with bottom alignment and tail follow; rows are time separators (first row, a 5 minute gap, or a new local day, formatted with libc `strftime`), event rows, and one row per markdown block, so a very tall reply builds only visible rows. Prose blocks over 4096 bytes are split for layout without truncating text. Bubble text is a read-only `ChatInput` per visible block, cached by `(record id, block)`. "New messages" appears when not at the bottom, floating over the list's bottom edge so it never resizes the list. Scrolling is described in [Scrolling](#scrolling). Ctrl+Q returns to the cards. Exact geometry, keys and focus rules are in [DESIGN.md](../design/DESIGN.md#lead-chat).

A live Lead with no visible history shows the antler mark, "What should we work on?" and the composer as one centered group with no dock; after the first send the composer docks at the bottom, without animation. The composer has no border (focus is the caret) and a 32px round action: send arrow (sunken and inert when the draft is empty, primary with text) or stop square while Working or Asking. Enter in the composer queues while the Lead is Working; the stop square still stops.

#### Scrolling

History and composer each own their scroll position, and the wheel is the author's.

- **History.** The list follows the bottom only while it is at the bottom (GPUI `FollowMode::Tail`). The first wheel step up releases it, and nothing the app does afterwards moves the view: redraws, the Working indicator, status changes, a new final response or event row, and older-page loads all keep the same item and offset (`add_records` re-anchors by row key after it resets the list). Scrolling back to the bottom, "New messages", or sending resumes following. "New messages" is an overlay on the list's bottom edge, not a dock row: in the dock it resized the list on the first wheel step, clipping the last rows and moving the bottom the list was following.
- **Composer.** Past 144px it scrolls internally and shows the app's scrollbar (`scrollbar::vertical`, ink-4 thumb, ink-3 on hover or drag) in the 8px before the action, so the author can see there is more text. The wheel and the scrollbar move it smoothly and it stays where they leave it. Only an edit or a caret move scrolls to the caret, by the least amount that shows it (`reveal`): typing keeps the caret in view, while a key that changes nothing, Home/End (which scroll without moving the caret) and redraws do not. Clicking to place the caret is handled separately.
- **Two traps found here.** An `absolute()` child with no inset in a scrolling div sits at its static position, after the content, so it adds its own height to the scroll range (the caret canvas gave the composer a blank screenful below the last line) and its bounds are not the viewport. The caret canvas is pinned with `top_0().left_0()`, and visibility is measured against `ScrollHandle::bounds()`. And a GPUI `list` measures only the rows near the view, so its totals are estimates until the author scrolls; derive nothing from them beyond what the list itself does.
- **Tests.** `lead_chat::scroll_tests` (a real `LeadChat` in a GPUI test window with synthetic history and wheel events) and `chat_input::tests` (the editor's scroll range, wheel position, caret reveal, Home/End). They use GPUI's `test-support` as a dev-dependency only.

#### Activity steps

While a turn runs, the lane shows a short step list: the current step in 12 ink-3 as the last line and up to 3 finished steps of this turn above it in 12 ink-4 with a leading "✓ ". Steps come only from Shika's knowledge of the `shika` commands the Lead runs over the control socket (never from provider output, thinking or tool text), are memory only, per turn, never stored in history, and vanish when the turn settles. Identical consecutive commands collapse. The command-to-copy table is in [DESIGN.md](../design/DESIGN.md#lead-chat) (Events and activity). The same current-step string is the second line of a Working Lead's card in the column, for terminal Leads too; otherwise a chat Lead's card shows the first line of its latest final reply (markdown stripped), and other Leads show "{CLI} · Lead". Code: `crates/shika/src/lead_steps.rs` holds the pure copy (`step_text`), the log (`StepLog`, `StepToken`) and the card line (`card_line`). `handle_control` starts a `StepToken` for every Lead command but `help` and `report` and wraps the reply channel in a `Responder`, so the step ends with the reply wherever it is sent (including the asynchronous `wait`, `pr`, `close`, `new`, and `diff`). A refused or failed command, a `wait` that timed out empty, a cancelled `close`, and a command whose client left (the token is dropped unanswered) leave no step; a cancelled `pr` leaves "Create PR cancelled for {task}". `Shika::sync_lead_activity` runs every tick: a changed `turn_started` starts the list over, a settled Lead (Ready, Waiting, exited) clears it, late replies from before a reset are ignored, and the result goes into `LeadChat::set_steps` (chat Leads) and the card. It also counts the workers the Lead started (`WorkerSummary::count`; exited counts as waiting, starting as working) for the chat header. While a wait runs "Waiting for workers..." is that step, not a second label. Terminal Leads get steps and the card line too. The card's reply line is `lead_chat::markdown::reply_first_line` of the latest Lead record, kept in `Shika::leads.snippets`: fed by live replies and read once per project from saved history off the UI thread, dropped when the Lead is forgotten.

### Where the code and the earlier design text differ

The previous version of this file was the approved design. Where the implementation chose differently, the code wins and this guide describes the code:

- The design proposed append-only files plus a rebuildable index; the code uses one SQLite file with an immutable `records` table and a rebuildable `visible_entries` projection.
- The design called a preallocated id provisional; the code saves no `sessionId` until `system/init` confirms it, so an unused fresh Lead is fresh again on its next start.
- The design proposed categorized provider errors (sign-in, rate limit, network); the code has one generic message for any failed result.
- A late worker report replaces the keyed row's text with "<title> is ready to check - Report updated", not a separate "Report updated" row.
- A failed result also stops the child; the next Send resumes it. The design only required settling the turn.
- An unexpected exit both writes an event row and rests the Lead with the toast "The Lead stopped unexpectedly. Select it to resume."
- Questions are limited to 4 questions of at most 4 options; anything else is treated as an unsupported interaction.
- Unsaved history rows after a write failure are buffered in memory only; they are lost if the app quits before Retry save succeeds.
- Chat is selected by preset id (`lead_chat_supported`: `claude`, `codex`, `pi`) with no setting; there is no way to keep a Claude Code, Codex or Pi Lead in a terminal.

## Queued messages

Added 2026-11-12. The author can type and press Enter at any time. While the Lead is Working the message waits in a queue shown under the history, and the Lead gets it as soon as it is free.

### What changed

- **Before:** while a turn ran, the composer's action was Stop and Enter did nothing (`LeadChat::submit` refused when `busy`). The author could not even press Enter on the next thought; they had to wait for Ready and retype or remember it.
- **Now:** Enter (or a click on the action, when it reads Send) while the Lead is Working, Asking or already sending moves the text out of the composer into an in-memory list on the chat, painted as a bubble marked "Queued". At each Ready, the first queued message is submitted through the same path a normal Enter uses. Stop (the button, and Escape) stays available and never takes Enter.
- **Unchanged:** the actor, `ChatTransport`, history storage, turn numbering, crash recovery, the final-only filter and the control socket know nothing about the queue. A queued message is not a record until the actor accepts it as an ordinary send, so it is never in `history.sqlite` before then.

### Why this design

**The queue is view-layer only.** An earlier attempt put queueing in the actor (a second pending slot beside the active turn). That touched turn numbers, cancel epochs, the outgoing transaction and recovery, all of which are fenced for exactly-once delivery, and it stalled. A list on `LeadChat` plus the existing send needs none of that: delivery is "wait for Ready, then do what Enter does", and everything after the send is the tested path.

**Memory only.** A queued message is not yet something the Lead was told, so it is not history; persisting it would need a new row kind, a schema bump and a rule for whether a restart resends it (the guardrails forbid automatic resends). Instead the author is warned before anything that would lose it.

**In order, exactly once.** One message is taken per Ready. It leaves the queue only when `submit_lead_chat` accepts it; a refusal keeps it first. The next goes at the next Ready, because accepting makes the Lead busy and `Lane::free` is false until then.

**The author's words win over Shika's.** The doorbell stays silent while anything is queued (`chat_quiet`), so a `[shika] Workers changed` line never goes ahead of messages the author wrote. Once the queue drains and the Lead settles, the doorbell rings as usual.

**Warn before loss, no new dialog.** The author chose "warn before loss". Quit, Forget and Remove project each gain one sentence in their existing Yes/No dialog ("2 queued messages have not been sent to the Lead and will be lost."). Stop loses nothing: the queued text goes back into the composer, ahead of any draft, joined by blank lines, and the Stop dialog says so.

**No remove control.** A per-bubble remove button was optional and was left out: it needs a focusable control, tab order and a screen reader label to be correct, and Stop already returns every queued message to the composer for editing. Add one only with the keyboard and accessibility work.

### How it works

`crates/shika/src/lead_chat/queue.rs` is pure (no GPUI) and holds the rules:

- `Lane` is the chat's flags (`live`, `busy`, `saving`, `save_failed`, `composing`, `queued`). `Lane::route(text)` returns `Route::Ignore` (not live, composing, save failed, or blank), `Route::Queue` (busy, a send in flight, or older messages still queued, so order holds even in the frame between Ready and delivery) or `Route::Send`. `Lane::free()` is live, not busy, not sending, not save-failed.
- `Queue` is a `VecDeque<String>` plus `paused_until`. `next(lane, now)` returns the first message only when the lane is free and no pause is active; `delivered()` pops it; `refused(now)` pauses delivery for `REFUSAL_PAUSE` (2 seconds) so a persistent refusal cannot toast every frame; `restore(draft)` drains it into one string.

The flow:

1. **Enter.** `LeadChat::submit` asks `Lane::route`. `Queue` pushes the trimmed text (`trim_blank_lines`, like a send) and clears the composer with `clear_if`; `Send` is the old path unchanged (`pending`, `saving`, `ChatAction::Send`). Either way the list returns to following the tail.
2. **Delivery.** `Shika::drain_lead_chats` calls `deliver_queued_chat` for each chat card after draining events and before `sync_lead_chat_card`. If `LeadChat::next_queued` has a message it calls `submit_lead_chat(index, text, ChatOrigin::Author, ...)`, which is what `ChatAction::Send` calls. Accepted: `queued_delivered`. Refused (card still running, or Core refused): `queued_refused`, and it tries again after the pause. Nothing sets `pending`, so delivery never touches the composer.
3. **After acceptance** it is an ordinary send. The author bubble comes from the actor's `Record` event. The queued bubble leaves the dock in the same update, so for a frame or two the message may be on screen in neither place; nothing is lost or duplicated.
4. **Doorbell.** `lead_gate` builds `idle` from `chat_quiet(saving, busy, save_failed, queued_len)`.
5. **Losing the Lead.** Stop (`lead_departed`, not forgotten) and an unexpected `ChatEvent::Exit` call `restore_queued`: the queue is drained into the composer (queued messages first, then the existing draft). A resting Lead has no composer on screen, so the text is waiting there after Resume. A queue never delivers to a Lead that is not live.
6. **Stop generating.** Interrupt settles the turn as "Reply stopped" and the Lead becomes Ready, so the next queued message is then delivered. That is intended: the author queued it for the Lead.

### View

The queued bubbles are in the dock (`queued_lane` in `view.rs`), after the activity lane and before error banners and the composer, not rows of the history list. The list's anchors, paging and `text_cache` are keyed by saved record ids, and a message that is not a record has none. The lane scrolls on its own above 176px. A bubble is the author bubble's geometry (right aligned, radius 22, 12/20 padding, 80% cap, 480px max, 14/20 type) on the raised fill with ink-2 text made legible on it, so it reads as not yet sent, with "Queued" beneath in 11.5 ink-3. All colors come from `Chrome`. See [DESIGN.md](../design/DESIGN.md#lead-chat).

### Code map

| File | Responsibility |
| --- | --- |
| `crates/shika/src/lead_chat/queue.rs` | `Lane`, `Route`, `Queue`, `REFUSAL_PAUSE`, `warning`, `returns_note`, and their unit tests. |
| `crates/shika/src/lead_chat/mod.rs` | `LeadChat::queue`, `lane`, `submit` (routing), `queued_len`, `next_queued`, `queued_delivered`, `queued_refused`, `restore_queued`; the `Exit` arm restores. |
| `crates/shika/src/lead_chat/glue.rs` | `deliver_queued_chat` (delivery, called from `drain_lead_chats`) and `queued_lead_messages` (the count for dialogs). |
| `crates/shika/src/lead_chat/view.rs` | `queued_lane` and its place in the dock. |
| `crates/shika/src/chat_input.rs` | `ChatInput::set_value`, used by restore. |
| `crates/shika/src/control/doorbell.rs` | `chat_quiet`, used by `lead_gate`. |
| `crates/shika/src/lead.rs`, `recovery.rs`, `projects.rs` | The Stop restore in `lead_departed`; the warning sentence in the Lead close dialog, Quit and Remove project. |
| `crates/shika/src/lead_chat/queue_tests.rs` | GPUI tests over a real `LeadChat`. |

### Debugging

| Symptom | Inspect first |
| --- | --- |
| Enter does nothing while Working | `Lane::route` returns `Ignore`: not `live`, `save_failed` (the conversation could not be saved; Retry save), composing text, or blank. |
| A queued message never goes | `next_queued` needs `live`, not `busy`, not `saving`, not `save_failed` and no active pause. Is the card still Working or Asking (`submit_lead_chat` refuses while `Card::running()`)? A toast repeating every 2 seconds is a Core refusal: read its text. |
| Queued messages went in the wrong order | They should not: `Queue` is FIFO and Enter queues behind anything queued. Look for a second sender calling `submit_lead_chat` with the queue non-empty (the doorbell is gated by `chat_quiet`). |
| The doorbell never rings | A non-empty queue closes `chat_quiet`; it reopens when the queue empties and the Lead settles. |
| A message is in history and still queued | Delivery pops only on acceptance. Check that `deliver_queued_chat` is the only caller and that `queued_delivered` runs in the accepted branch. |
| Text vanished after Stop | `restore_queued` runs in `lead_departed` (Stop) and on `Exit`. Forget, Remove project and Quit drop it, after the warning. |

Reproduce with the tests below, or in the native app with disposable data per [isolated reproduction](#isolated-reproduction): send a message that makes the Lead run a long `shika wait`, press Enter twice with different text, and watch both bubbles; Escape stops the turn and the first one goes.

### Extension guardrails

- Keep the queue in the view layer. Do not add queue state to `ChatTransport`, the store or `Core`; do not persist it. If the author later wants it to survive a restart, that needs their decision on resends and a schema bump.
- Deliver only through `submit_lead_chat`, so every gate there (card running, Core refusal) applies. Never write to the transport from the queue.
- Pop only on acceptance, one per free Lead. Do not batch, merge or reorder queued messages.
- Keep the doorbell behind the queue. Any new sender that submits to a chat Lead must check `chat_quiet`.
- A new way to drop a chat (a new close path) must either call `restore_queued` or add `warning(queued_lead_messages(...))` to its dialog.
- Do not make the composer inert while Working; the author must always be able to type. Colors come from `Chrome`.
- Other CLIs: Cursor CLI Leads are terminals today and have no queue; the terminal keeps its own typing rules. A chat adapter inherits the queue unchanged because it sits above the transport, as Codex and Pi did.

### Tests

| Test | Covers |
| --- | --- |
| `lead_chat::queue::tests::*` | Routing (queue while Working, behind older messages, ignore blank, composing, save-failed), FIFO order and one per Ready, kept on refusal with a pause, nothing due while sending, restore order and draft joining, the warning and Stop sentences. |
| `lead_chat::queue_tests::*` | In a real chat view: Enter while Working queues and empties the composer without a send; Enter while free sends and the next queues; one delivery per free Lead and refusal; Stop and Exit restore the text; the queued bubble takes room under the history without breaking tail following. |
| `control::tests::the_doorbell_holds_while_a_chat_lead_has_queued_messages` | `chat_quiet` and a gate built from it: held while queued, rings after. |

The Quit, Forget and Remove project sentences come from the tested `warning` and `returns_note`; their native rendering is not covered by tests (see MANUAL_CHECKS.md).

## Contributor guide

### Code map

| File | Responsibility |
| --- | --- |
| `crates/shika-core/src/lead_transport.rs` (new), `lead_transport/protocol.rs`, `lead_transport/claude.rs`, `lead_transport/codex.rs`, `lead_transport/pi.rs` and `pi-lead.ts`, `lead_transport/jsonrpc.rs`, `lead_transport/actor.rs` | `ChatTransport` (public handle), `ChatLaunch`, `Parsed`, `ChatEvent`, `ChatStatus`, `ChatQuestion*`, `chat_cli` (CLIs with a protocol in Core) and `lead_chat_supported` (the shipped gate, `SHIPPED`); in `protocol.rs`, the `LeadProtocol` trait (arguments, env, handshake, send, answer, interrupt, shutdown, stop signal, decoding, strings) and `for_cli`; in `claude.rs`, `ClaudeStream` (decoder and final-response filter) and its arguments; in `codex.rs`, `CodexAppServer` (handshake, thread open and instruction injection, final-response filter, questions, interrupt); in `pi.rs`, `PiRpc` (handshake and resume check, final-response filter, extension dialogs, abort) and the `pi-lead.ts` question extension it installs; in `jsonrpc.rs`, `Rpc` (request ids, pending requests with deadlines, response correlation, server requests); in `actor.rs`, `Actor` (turn state, persistence, process lifecycle), `Process` (spawn with `reset_signals`, env, nonblocking stdin, reader threads, signals, reap), and `write_frame`. Unit tests over the fixtures (`claude_tests.rs`, `codex_tests.rs`, `pi_tests.rs`, `jsonrpc_tests.rs`, `actor_tests.rs`). |
| `crates/shika-core/src/lead_chat.rs` (new) | The `Core` boundary: `lead_chat_send`, `lead_chat_events`, `lead_chat_interrupt`, `lead_chat_answer`, `lead_chat_retry_save`, `lead_chat_page`, `confirm_lead_chat` (operations lock, live identity), `stop_lead_chats`. |
| `crates/shika-core/src/lead_chat_store.rs` (new) | `ChatStore`: open with path and version checks, `bind`, `append_batch`, `page`, `recover`, index rebuild; `delete` for Forget; `ChatContent`, `ChatOrigin`, `ChatRecord`. |
| `crates/shika-core/src/lead.rs` | `start_chat_lead(project, cli, launch)` and the shared `start_lead_inner` (same worktree, conversation choice, guide plus memory, control env), with `Identity` naming where a fresh id comes from (preallocated, reported by the handshake, created by the CLI); chat saves no provisional id and carries `chat_history`; Forget deletes chat. |
| `crates/shika-core/src/lib.rs` | `Core::chats` map; `hang_up` stops the transport; `Session::pty` became optional; re-exports; core integration tests with a fake `claude` (in `src/tests/lead_chat.rs`) a fake `codex` (`src/tests/lead_chat_codex.rs`) and a fake `pi` (`src/tests/lead_chat_pi.rs`). `remove_project` now deletes memory (and chat) after hanging up sessions. |
| `crates/shika-core/src/lead_chat_store.rs`, `usage/collector/` | `instructed` and `set_instructed`: the fingerprint of the instructions a Codex thread last received. Codex usage targets with `threads` roots (`usage::CodexThreads`). |
| `crates/shika-core/src/projects.rs`, `session.rs`, `error.rs` | `SavedLead::chat_history` (`chatHistory`); `Session::pty: Option<PtyId>` (`None` only for a chat Lead; Switch agent refuses it); `Error::LeadChat`. |
| `crates/shika-core/tests/fixtures/` (new) | Three installed-CLI recordings, one hand-written stream, and their provenance README. |
| `crates/shika/src/lead_chat/` (new) | `LeadChat` (`mod.rs`) and its view (`view.rs`, `rows.rs`, paging and scroll anchor in `history.rs`, `selection.rs`, composer wiring in `composer.rs`, steps and worker summary in `progress.rs`, banners) and the `Shika` glue: `ensure_lead_chat`, `submit_lead_chat`, `drain_lead_chats` (events to status, notifications, questions, identity confirmation, resume and exit handling) in `glue.rs`, `selected_chat`, `chat_side` in `side.rs`. |
| `crates/shika/src/lead_chat/queue.rs` (new) | Queued messages: routing, FIFO delivery rules, restore and the loss warnings. See [Queued messages](#queued-messages). |
| `crates/shika/src/lead_chat/markdown.rs` (new) | Original markdown subset parser, block splitting, `safe_link`, theme-derived highlights. |
| `crates/shika/src/lead_chat/question.rs` (new) | Native question dialog: open or queue, step through questions, build the answer, cancel, keys and view. |
| `crates/shika/src/chat_input.rs` (new) | Multiline editor and read-only selectable text: IME, selection, visual rows from GPUI's wrapped layout, caret affinity at soft wraps, undo/redo, links, Enter/Escape/Ctrl+Q events. |
| `crates/shika/src/text_edit.rs` | The editing core shared with `name_input.rs`: text, selection, and AppKit's standard key bindings (word, line, paragraph and document moves, Shift extension, deletes, the kill buffer, emacs keys). The composer keys are in [DESIGN.md](../design/DESIGN.md#lead-chat). |
| `crates/shika/src/control/doorbell.rs` | `ChatDelivery`, `chat_delivery_finished`, chat-aware `lead_gate`, chat delivery in `ring_doorbells` and the restart summary, `Gate::waiting`. |
| `crates/shika/src/activity/` | `Activity::advance_chat` and its test. |
| `crates/shika/src/card.rs`, `overlay.rs`, `main.rs`, `startup.rs`, `lead.rs`, `launch/mod.rs`, `focus.rs`, `tick.rs`, `tabs.rs` | `Card::chat`, `Card::chat_delivery` (`card.rs`); `Overlay::LeadQuestion` (`overlay.rs`); `Shika::leads` (`main.rs`); `Leads::chats`, `Leads::questions`, `LeadRun::unexpected_exit` (`lead.rs`); launch routing to `start_chat_lead` through `shika_core::lead_chat_supported` and `LeadChat::went_live` (`launch/start.rs`); the toast over the chat (`terminal_side/mod.rs`); focus (`focus.rs`) and Seen for chat (`tick.rs`); skipping chat cards in terminal sampling (`tick.rs`); tabs disabled (`tabs.rs`); quit hook (`startup.rs`). |
| `crates/shika/src/model.rs`, `appearance/chrome.rs`, `Cargo.toml` | Forget dialog fact about chat history; `PartialEq` on `Chrome` for cheap theme change checks; `libc` for local time formatting. |
| Docs | This guide; pointers in AGENTS.md, CONTRIBUTING.md, lead-agent.md, shika-cli.md, agent-activity.md, `crates/shika-core/README.md`; DESIGN.md Lead chat section and plates; MANUAL_CHECKS.md. The Lead guide is unchanged. |

### Ownership rules

- Core owns processes, pipes, cancellation, history and saved ids. The app owns messages on screen, focus, status, dialogs and notifications.
- Only the actor thread touches the child and the store. Blocking Core calls (`start_chat_lead`, `confirm_lead_chat`, `lead_chat_page`, `stop_lead_chats`) run off the UI thread, except the quit hook.
- Every event that changes saved state is fenced: frames by process generation, sends by cancel epoch, id commits by live session plus the operations lock. A stopped or replaced Lead cannot save an id or append to another lifetime.

### Debugging by symptom

Inspect the **isolated** data directory: `projects.json` (`lead.cli`, `lead.sessionId`, `lead.chatHistory`) and the chat database. Read it with the app quit, or read-only. Codex- and Pi-specific symptoms are in [Codex](#codex) and [Pi](#pi).

```sh
db="$fixture/data/leads/<project-id>/chat/history.sqlite"
sqlite3 -readonly "$db" "SELECT key, value FROM metadata;"
sqlite3 -readonly "$db" "SELECT id, turn, segment, visible, content FROM records ORDER BY id DESC LIMIT 20;"
sqlite3 -readonly "$db" "SELECT * FROM visible_entries ORDER BY id DESC LIMIT 20;"
```

| Symptom | Inspect first |
| --- | --- |
| A reply never appears; card stays Working | Last turn's records: `Outgoing` without `Accepted` means the write never completed; `Accepted` without `Settled` means no `result` arrived. Is the child alive (`pgrep -fl 'claude -p'`) and still in a tool or `shika wait`? An unknown `control_request` shows "This Lead needs an interaction chat does not support." and stays Asking. Stop generating settles it. |
| The CLI's transcript has a reply the chat never showed | Rows after the last `Settled`: a CLI-started turn writes `Accepted` with no `Outgoing`. None at all means its `system/init` was not read as a turn start; compare the stdout with `claude-2.1.302-background-local.jsonl` and see [turns the CLI starts itself](#turns-the-cli-starts-itself). The transcript is `<claude config>/projects/<worktree slug>/<lead.sessionId>.jsonl`; read it, never edit it. |
| Turn settled but no Lead bubble | A tool-only turn has no final text and no bubble, by design. Check whether the last assistant text came before a tool call (it is commentary). Reproduce with the decoder tests, adding the stream as a fixture. |
| Intermediate text appeared in a bubble | A filter bug. Capture the stream with a fake CLI or the synthetic server, add it to the fixtures, and assert in `lead_transport` tests. Check record `uuid` dedup and `parent_tool_use_id`. Never fix by trimming text. |
| "This Claude Code version could not be read" | A record type or block type the decoder does not know, an oversized frame, or no initialize response within 10 seconds. Compare `claude --version` with the fixtures and record a new one. |
| Resume fails or starts fresh unexpectedly | `lead.sessionId` null (stopped before the first turn, or cleared after a refusal); `<claude config>/projects/*/<id>.jsonl` missing (check `CLAUDE_CONFIG_DIR`); a Shika started from a Claude session (the transport removes `CLAUDE_CODE_CHILD_SESSION`, but check the CLI's own config). Exit within 5 seconds of resume follows the refusal path in `lead_upkeep`. |
| "history identity changed" | `lead.chatHistory` does not match `metadata.lifetime`, for example after the chat folder was deleted or copied. Forget clears both. Never repair by editing normal app data. |
| History missing after relaunch | `lead.chatHistory` present? Database present and readable? Forget or project removal deletes it. An unused Lead that was stopped before any message has nothing to show. |
| "Could not save this conversation" | Permissions on `leads/`, the project folder and `chat/` (must be real directories, `0700`), disk space, a locked database. Retry save appends the buffered rows. |
| Status sticks at Asking | A question queued under another overlay (`leads.questions`), an open `LeadDialog`, or an unsupported interaction. Status sticks at Working: see "A reply never appears". |
| Doorbell never rings a chat Lead | `lead_gate`: composer draft, typing within 3 seconds, `saving`, `busy`, `save_failed`, an open wait, or `live` false. Rows appear only when delivery starts. |
| Workers stopped after Stop generating | They should not; workers are separate PTY process groups. Check whether the Lead started a process outside Shika that a worker relied on. |
| A chat child outlives the app | The quit hook (`stop_lead_chats`) did not run, for example after a crash. Find it with `pgrep -fl 'claude -p'` or `pgrep -fl 'codex app-server'` and stop it; Pi names its process `pi`, so find it by parent or cwd (see [Pi](#pi)). |
| Stop generating or quit waits 2 seconds, then the CLI dies by SIGKILL | The child ignored its stop signal. Its signal mask must be empty: `Process::spawn` resets it, because app threads block SIGINT and SIGTERM and a spawned child keeps the caller's mask. `ps` does not show a thread's blocked signals reliably; test with a fake CLI started from a thread that blocks them, as `a_chat_cli_does_not_inherit_the_callers_blocked_signals` does. |
| "Reply stopped" appears twice, the second with no turn | Fixed 2026-11-20: a quit or interrupt after the process was already gone settled a turn that did not exist. `quitting_after_a_stop_that_ended_the_process_adds_no_row`. |
| A toast the code posts never shows over the chat | `terminal_side` draws it over `chat_side` too; check that path, not `chat_side` alone. |

### Isolated reproduction

Never test against normal app data or a personal repository. Two levels, from cheapest:

**Core with a fake CLI (no GUI, no network).** `claude_chat_has_no_pty_confirms_identity_and_keeps_history_until_forget` and `chat_interrupt_cancels_a_blocked_pipe_write_and_quit_reaps_the_child` in `crates/shika-core/src/tests/lead_chat.rs` put a shell script named `claude` on a scratch PATH and use a scratch CLI home. Copy that script shape to reproduce a stream bug: answer `shika-initialize`, then for each `"type":"user"` line print `system/init`, assistant records and a `result`.

```sh
source "$HOME/.cargo/env"
cargo test -p shika-core -- lead_transport lead_chat_store chat_
```

**Native app with disposable data.** Build a bundle, create a throwaway repository, and open a separate instance on its own data directory:

```sh
source "$HOME/.cargo/env"
./scripts/bundle-app.sh --debug
fixture=$(mktemp -d "${TMPDIR:-/tmp}/shika-lead-chat-XXXXXX")
fixture=$(cd "$fixture" && pwd -P)
repo="$fixture/repo"
mkdir -p "$repo" "$fixture/data"
git -C "$repo" init -b main
git -C "$repo" config user.name Test
git -C "$repo" config user.email test@invalid.example
git -C "$repo" commit --allow-empty -m 'Disposable Lead chat fixture'
open -n target/debug/Shika.app --args --data-dir "$fixture/data" --diagnostics-file "$fixture/diagnostics"
printf 'Add this disposable project: %s\n' "$repo"
```

Add the printed repo, then Cmd+L and choose Claude Code. That run uses the installed CLI and the author's real Claude login and plan; every message is a real request. To avoid real requests, the recorded smoke run used a launcher hard-coded to the disposable data directory and repository, with a disposable `CLAUDE_CONFIG_DIR`, a fake local-only key and a loopback synthetic Messages server (see [fixtures](#fixtures)). An instrumented or scratch build must hard-code its disposable data directory and use a unique bundle identifier, per AGENTS.md. Confirm the test window is in front before any synthetic input. Quit the instance and check that no `claude -p` child remains before deleting the fixture.

### Fixtures

`crates/shika-core/tests/fixtures/README.md` is the provenance record:

- `claude-2.1.302-local.jsonl` (two turns with a tool) and `claude-2.1.302-question-local.jsonl` (a correlated `AskUserQuestion`) were recorded on 2026-10-16 from installed Claude Code 2.1.302 with the [chat arguments](#processes-and-threads), a disposable repository and `CLAUDE_CONFIG_DIR`, a fake local-only API key, and an HTTP server on loopback returning synthetic Messages responses. No provider request or paid usage occurred. They test the real CLI envelope, not model behavior.
- `claude-2.1.302-background-local.jsonl` was recorded on 2026-10-17 the same way: one sent message whose turn starts a Bash command with `run_in_background` and settles, then the turn the CLI starts itself when that command finishes (task notification records, `system/init`, narration, a foreground tool, the final text, `result`). The loopback server answered `<task-notification>` requests like any other.
- `claude-chat.jsonl` is hand-written from the documented format: thinking, subagent text, a partial `stream_event`, a duplicate record, and an aggregate `result` containing commentary.
- `codex-0.162.1-*-local.jsonl` and the schema subset are described in [Codex](#codex), `pi-1.4.0-*-local.jsonl` in [Pi](#pi), and both in the fixtures README.
- Tests read fixtures with `include_str!`; they never spawn a real CLI or read provider files.

To re-record after a CLI upgrade:

1. Check `claude --version` and `claude --help` for every flag in `ChatLaunch::args`. If one changed, update the code, AGENTS.md and this guide first.
2. Make a disposable repository and an empty `CLAUDE_CONFIG_DIR` under a temp directory. Run a loopback HTTP server that answers the Messages API with scripted responses: text, then a tool call (Bash, or `AskUserQuestion` for the question fixture), then a final text. Point the CLI at it with a fake key; check the installed version's documentation for the base-URL setting. Never use a real key or normal config.
3. Run the CLI with the chat arguments and a fixed `--session-id`, write the initialize control request, then user records, answering any `control_request` with the same envelopes the actor writes. Save stdout.
4. Sanitize: remove paths, usernames, metadata and random ids; normalize the session id (`fixture-session`) and request ids; keep `message.id` values and distinct record `uuid`s, because the dedup logic depends on them.
5. Name the file `claude-<version>-<case>-local.jsonl`, keep older fixtures, add a test like `installed_cli_two_turn_fixture_hides_split_blocks_with_the_same_message_id`, and update the fixtures README.

### Tests

| Test | Covers |
| --- | --- |
| `lead_transport::tests::*` | Final-only extraction from both kinds of fixture, the turn the CLI starts itself, split blocks with one message id, multiple final text blocks and uuid dedup, question parsing, identity mismatch, garbage, unknown control requests, failed results. |
| `lead_chat_store::tests::*` | Atomic completion, keyed event replacement, index rebuild, recovery markers (including a CLI-started turn), 60-entry paging across reopen, corruption preserved, Forget keeping `memory.json`. |
| `tests::lead_chat::claude_chat_has_no_pty_...` | No PTY, provisional then confirmed id, final-only history, interrupt and continue, Stop keeping history and workers, Forget deleting chat and keeping memory. |
| `tests::lead_chat::a_turn_the_claude_cli_starts_itself_...` | A fake CLI's own turn after a settled one: `Started` then Working, final-only Lead bubble, rows `Accepted`, `Lead`, `Settled` with no author bubble, and the next send numbered after it. The decoder side is `installed_cli_background_fixture_...`. |
| `tests::lead_chat::chat_interrupt_cancels_a_blocked_pipe_write_...` | A write blocked on a full pipe is interruptible, and quit reaps within 4 seconds. |
| `lead_transport::codex::tests::*` | Every Codex fixture replayed through `CodexAppServer`, each written frame checked against the recorded one: final-only with and without `phase`, retried errors and failed turns, interrupts with and without a running command, questions and cancel, blocked requests, a turn Codex starts itself, subagent threads dropped, resume with unchanged and changed instructions, refused and different resumes, launch arguments, garbage and oversized records, and stops at every turn state. |
| `lead_transport::jsonrpc::tests::*`, `lead_transport::actor::tests::*` | Request ids, deadlines and correlation; the actor's soft and hard stops, `Lingering`, `Accepted` from a response, deferred sends and `Instructed`. |
| `tests::lead_chat_codex::*` | A fake `codex` on a scratch PATH: no PTY, the thread confirmed at its first turn, final-only history, interrupt with and without a running command (SIGTERM, then resume in a new process), instructions injected only when memory changed, a saved terminal-era thread resumed with empty history and one injection, Stop keeping workers, Forget keeping memory, quit with a turn running, no stray row after a stop that ended the process, and a child started from a thread that blocks SIGINT and SIGTERM still stopping at once. |
| `lead_transport::pi::tests::*` | Every Pi fixture replayed through `PiRpc`, each written frame checked against the recorded one: final-only after tools, narration and cut streams, failed runs, retries, compaction and overflow recovery publishing only at `agent_settled`, extension dialogs and Shika's question tool answered and cancelled, Stop cancelling an open dialog before `abort`, `handled` prompts, a run Pi starts itself, refused prompts, a resume that comes back empty, launch arguments, garbage, huge ignored records and the oversized `agent_end`. |
| `tests::lead_chat_pi::*` | A fake `pi` on a scratch PATH: no PTY, the preallocated id confirmed at the first user message, final-only history, a `handled` prompt, a question, interrupt, resume with current memory, a saved terminal-era session resumed with empty history, a resume Pi brings back empty refused with the id kept, Stop keeping workers, Forget keeping memory, quit by closing stdin. |
| `lead_chat::queue_tests::going_live_again_clears_the_exit_line_but_not_a_save_failure` | The old process's exit line goes when a start makes the Lead live; a save failure stays. |
| `activity::tests::chat_status_uses_explicit_results_and_retains_the_turn_clock` | Turn clock across Asking, one notification per turn. |
| `activity::tests::a_chat_turn_the_cli_starts_itself_needs_the_submission_mark` | Why `Started` is a submission: without it a new turn keeps the old clock and never notifies. |
| `lead_chat::markdown` tests | Bash fences as code, HTML as text, unsafe links refused, long prose split without losing style ranges. |

Passing tests are not native acceptance. Record GUI results in MANUAL_CHECKS.md.

## Extending to other CLIs

### Where each CLI is wired

Until 2026-11-20 the transport assumed Claude in many places (the decoder, arguments, the initialize write, `store.bind("claude", ...)`, error strings). After the protocol boundary and the Codex adapter, a new CLI touches only these:

- `lead_transport/<cli>.rs`: a `LeadProtocol` implementation (arguments, env to remove, handshake, send, answer, interrupt, shutdown and stop signal, decoding, strings), registered in `protocol::for_cli`.
- `lead_transport.rs`: `chat_cli` lists the CLIs with a protocol in Core; `lead_chat_supported` (`SHIPPED`) lists those the app runs as chat Leads. A CLI joins `SHIPPED` only in the commit that records its acceptance.
- `lead.rs`: `identity(preset, chat)` says where a fresh id comes from (`Preallocated`, `Reported` by the handshake, or `Created` by a CLI command). `start_chat_lead` and `confirm_lead_chat` accept any `chat_cli`.
- `cli_title.rs`: `CliHome::session_exists` for the refused-resume check.
- Usage: a source for the Lead's conversation if the CLI's own discovery cannot find it (Codex registers its thread id; Pi is not collected).
- The app needs no change: launch routing (`LaunchOwner::chat`), the saved Leads' chats in `Render`, `selected_chat`, the Leads card's reply line and `LeadRun::discovery` all follow `lead_chat_supported`.

### Steps for any adapter

1. **Check the CLI.** Record version and `--help` for every flag. Do not invent flags. Never pass a CLI's own `--worktree`.
2. **Record evidence first.** Fixtures from the installed binary with disposable config and no paid requests where a local server route exists; otherwise ask the author before any real request, and mark hand-written fixtures as such. Cover commentary, several tools, retries, compaction, empty finals, failures and interruptions.
3. **Introduce an adapter boundary.** Split the decoder, arguments, process model (long-lived or per turn), initialization, interrupt method and identity source behind one type, and keep `Actor`'s persistence, fencing and event vocabulary shared. Keep the store's `bind(cli, conversation)` honest.
4. **Identity.** Decide where the id comes from (preallocated, allocated by a helper command, or reported on the first turn) and commit it only through a generation-checked `confirm_lead_chat` for that preset.
5. **Status and exits.** Map only explicit lifecycle events. For per-turn CLIs, a clean exit after a terminal event is a normal Ready and must not emit `Exit`, `Resuming`, or trip the 5 second resume grace; a clean exit without one is "The reply ended before it was complete".
6. **Switch the preset.** Add it to `SHIPPED` in `lead_chat_supported` once its native acceptance passes, with the tests that list shipped CLIs.
7. **Docs and checks.** Update AGENTS.md (launch table and Lead flags), this guide, lead-agent.md, CONTRIBUTING.md's product clause, DESIGN.md if visible, and add a MANUAL_CHECKS.md section. Leave items unchecked until performed.

### Codex

Implemented 2026-11-20 against codex-cli 0.162.1 (`crates/shika-core/src/lead_transport/codex.rs`, `CodexAppServer`). Everything not named here (the store, fencing, queue, doorbell, view, status mapping) is shared with Claude.

**Process model.** One `codex app-server` child per live Lead, JSON-RPC 2.0 over stdio, one JSON object per LF-terminated line; server frames carry no `"jsonrpc"` field. It survives turns and Stop generating unless a command was still running (see Interrupt). The subcommand is labelled `[experimental]`; the author accepted that with per-version fixtures and a visible "This Codex version could not be read" (Decision 5).

**Launch** (`LeadProtocol::args`, checked with `codex app-server --help` and `codex features list`): `codex app-server --enable default_mode_request_user_input --disable goals -c projects={"<worktree>"={trust_level="trusted"}}`, in the Lead worktree with the shared Lead env. The trust value is built by `agents::codex_trust_override`, the same encoder workers use.

**Handshake.** `initialize {clientInfo: {name: "shika", version}}`; its response's `userAgent` gives the version shown in errors. Then the `initialized` notification and, in the same write, the thread call:

- Fresh: `thread/start {cwd, developerInstructions: <guide plus memory>, approvalPolicy: "never", sandbox: "danger-full-access"}`.
- Resume: `thread/resume {threadId, cwd, approvalPolicy, sandbox, excludeTurns: true}`. `excludeTurns` keeps the whole history out of the response and under `MAX_FRAME`. No `developerInstructions`: Codex ignores them on resume (Step 1 evidence; `codex resume -c developer_instructions=...` in a terminal behaves the same, so a resumed terminal Codex Lead never got later memory notes).

Each request, `turn/start` and `thread/inject_items` included, has a 10 second deadline (`jsonrpc::Rpc`); a missed one fails the turn with "This Codex version could not be read". A send that arrives before the thread is open waits in the protocol (`Turn::Deferred`) and goes out once it is.

**Instructions on resume.** After `thread/resume`, if the guide plus memory differ from what this thread last received, Shika sends one `thread/inject_items {threadId, items: [{type: "message", role: "developer", content: [{type: "input_text", text}]}]}` before any turn. It reaches the model and persists into later sessions. What was delivered is an md5 fingerprint of `[cli, conversation, instructions]` in the chat store's `metadata` table (key `instructions`, never the text), written on `Parsed::Instructed`: at `thread/start`, or when the injection is answered. A thread saved by a terminal-era build has no fingerprint, so its first chat resume injects once; it is model context, never a visible row (`a_saved_terminal_codex_lead_resumes_its_thread_in_chat_with_empty_history`).

**Identity.** `thread/start` returns the thread id (a UUIDv7) at once; that is `Parsed::Identity`, which binds the history. A thread with no turn has no rollout and `thread/resume` refuses it, so the id is only confirmed (`Parsed::Confirmed`, then `confirm_lead_chat` saves `lead.sessionId`) at the Lead thread's first `turn/started`, as Claude's is at its first `system/init`. `thread/resume` must report the same id, otherwise "Codex returned a different conversation". A refused resume (`-32600 "no rollout found for thread id <id>"`, or `"invalid session id"`) is fatal ("Codex could not resume this conversation"), so the 5 second refused-resume rule applies. No index scan and no 2 second polling: `LeadRun::discovery` is off for every chat Lead, and `discover_lead_session` returns the saved id.

**Turns.** Send is `turn/start {threadId, input: [{type: "text", text}]}`; its response (with the turn id) is `Accepted`, not the pipe write (`write_accepts` is false). Notifications whose `threadId` is not the Lead's are dropped: subagent threads stream on the same connection. A `turn/started` while no turn of ours runs is a turn Codex started itself (`Parsed::Started`); goals are its only source and they are disabled, but it is still decoded.

**Final-response filter**, released only by `turn/completed`:

- `item/completed` with `type: "agentMessage"`: `phase: "final_answer"` messages are collected in order; any other message (`commentary`, `partial_answer`, or no phase, which Codex sends when the model did not) replaces the fallback candidate.
- Any other item type except `userMessage`, `reasoning`, `plan`, `hookPrompt` and `contextCompaction` (commands, file changes, MCP and tool calls, subagent calls, unknown types) clears the fallback: text before a tool is commentary. `commandExecution` items are also tracked from `item/started` to `item/completed`.
- `status: "completed"`: publish the `final_answer` messages joined by a blank line, else the fallback, else nothing. `failed`, or `completed` after an `error` notification with `willRetry: false`, fails the turn with "Codex could not complete the reply. Check its sign-in and connection, then retry." `interrupted` settles with no bubble. Anything else is unreadable.
- Ignored: deltas, reasoning, plans, diffs, token usage, rate limits, MCP startup, `warning`, `thread/status/changed`, and `turn/completed`'s `items`, which is only a summary.
- `error` with `willRetry: true` is a reconnect notice ("Reconnecting... 2/5"), not a failure.

**Questions.** `item/tool/requestUserInput` for the Lead's thread and running turn becomes the native dialog when it has 1 to 4 questions with distinct ids and texts, at most 4 options each, and no `isSecret`; otherwise it is blocked like any unsupported interaction. Questions are single-choice with free text (`isOther`). The answer is the JSON-RPC result `{"answers": {"<question id>": {"answers": ["<label or text>"]}}}`; Cancel answers `{"answers": {}}`, which is all Codex documents (Decision 9). `serverRequest/resolved` closes the dialog. The request arrives with `isBlocking: false`, yet the turn waits for the answer. Default mode only offers the tool with `--enable default_mode_request_user_input` (an under-development feature; app-server sends a `warning` about it at every thread open); without it the model is told the tool is unavailable. Every other server request, approvals and MCP elicitation included, is blocked and never answered; with `approvalPolicy: "never"` and `danger-full-access` none was seen.

**Interrupt.** Stop generating writes `turn/interrupt {threadId, turnId}` and waits for `turn/completed` with `interrupted` (it comes at once). Codex leaves the running command alive ("Any running unified exec processes may still be running" is what it tells the model), and a Lead's blocking `shika wait` would outlive the stop. So when a `commandExecution` item is still running at that `turn/completed`, the protocol returns `Parsed::Lingering` and the actor sends SIGTERM to the group at once: Codex exits and kills its tools, and the next send resumes the thread in a new process (`Resuming`). With nothing running the process is kept. A soft stop still running after 2 seconds gets SIGTERM, then SIGKILL 2 seconds later. A send not yet written settles at once; one written but not yet named by Codex is signalled. Workers are untouched either way.

**Shutdown.** Quit and Stop Lead close stdin; Codex exits in milliseconds, also mid-tool, and kills its tools. The stop signal is SIGTERM: each tool runs in its own process group, so SIGKILL to the app-server's group orphans them.

**Usage.** App-server threads are indexed with `source = 'vscode'` (the originator is the `clientInfo` name), which the cwd-based usage discovery skips. So the actor registers the thread id as a usage root (`usage::CodexThreads`, `Target::Codex::threads`) once the handshake reports it; the thread's rollout and spawned children are then read like any Codex task. Subscription limits keep reading the same rollouts. A resumed terminal-era thread stays `cli`.

**Traps.**

- *Inherited signal mask.* Codex spawned from an app thread inherited SIGINT and SIGTERM blocked and ignored both, so the Lingering SIGTERM did nothing, SIGKILL came 2 seconds later, and the command was orphaned (found in the 2026-11-20 native acceptance; a `python3` or `cargo test` parent never shows it). `Process::spawn` now resets signals; `a_chat_cli_does_not_inherit_the_callers_blocked_signals` keeps it.
- *Trust.* Without the per-process trust override, `thread/start` with `danger-full-access` writes `[projects."<path>"] trust_level = "trusted"` into the author's `$CODEX_HOME/config.toml` (the main repository root for a linked worktree). Keep the `-c`.
- *Goals.* Codex offers `create_goal` by default and continues goals in turns it starts itself; `--disable goals` keeps Lead turns to the author and the doorbell.
- *Subagents.* Their threads share the connection; filter by `threadId` before anything else.
- *Retries.* `error` with `willRetry: true` precedes a successful retry; never fail on it.
- *Usage discovery.* `source = 'vscode'`, see Usage.
- *Code mode.* With a real account the model runs shell through code mode (`exec` calling `exec_command`); the app-server still reports a `commandExecution` item, so the filter and Lingering rule hold.

**Fixtures.** `crates/shika-core/tests/fixtures/codex-0.162.1-*-local.jsonl` (normal turns with and without `phase`, failure with retries, interrupt mid-tool, question, a background goal turn, resume of an app-server and of a TUI-created thread, subagents) and `codex-0.162.1-app-server-schema.json` (the subset of `generate-json-schema` Shika reads), recorded on 2026-11-20 against a loopback synthetic Responses server with a disposable `CODEX_HOME` and no real account. Provenance and the sanitizing rules are in the fixtures README. `codex_tests.rs` replays each recording and checks every frame Shika writes against the recorded client frame.

**Debugging by symptom.** Reproduce in a disposable data directory (see [isolated reproduction](#isolated-reproduction)) and find the process with `pgrep -fl 'codex app-server'`.

| Symptom | Inspect first |
| --- | --- |
| "This Codex version could not be read" | A request unanswered for 10 seconds or a line that is not JSON-RPC. Compare `codex --version` and `codex app-server --help` with the fixtures, run `codex app-server generate-json-schema`, and record a new fixture. |
| Stop generating leaves a command running, or takes 2 seconds | Whether the app-server got SIGTERM and exited at once. If it lingers, check the child's signal mask is empty (`reset_signals`); a SIGKILL leaves tool processes, each in its own group, reparented to launchd. |
| Every resume injects the instructions again | `metadata` key `instructions` in `history.sqlite`: it must hold `["codex", <thread id>, <md5>]` for the current thread. A guide or memory change injects once by design. |
| A memory note never reaches a resumed Lead | No `thread/inject_items` was sent: the fingerprint matched. Read the thread's rollout (`$CODEX_HOME/sessions/.../rollout-*-<id>.jsonl`, read only) for the developer message. |
| Lead tokens missing from Usage | `usage-sources/*.json` should have a Codex target with the thread id under `threads`. |
| The Lead answers a question in prose instead of the dialog | The launch lacks `--enable default_mode_request_user_input`, or the feature was renamed: check `codex features list`. |
| `config.toml` gained a trust entry for the Lead worktree | The trust override was missing from the launch. |
| A reply appears that the author did not ask for | A `turn/started` with no `turn/start` from Shika: a goal, or a new Codex feature that starts turns. Check `--disable goals` and `codex features list`. |

### Pi

Implemented 2026-11-20 against Pi 1.4.0 (`crates/shika-core/src/lead_transport/pi.rs`, `PiRpc`; adapter `71cf59f`, then its acceptance). Everything not named here (the store, fencing, queue, doorbell, view, status mapping) is shared with Claude and Codex.

**Process model.** One `pi --mode rpc` child per live Lead, one JSON object per LF-terminated line in both directions. stdout carries only protocol records (responses, events, extension UI requests); diagnostics such as the "creating a new session" warning go to stderr, which is drained and discarded. The process survives turns and Stop generating.

**Launch** (`LeadProtocol::args`, checked with `pi --help`, 1.4.0): `pi --mode rpc --approve --extension <private temp>/lead.ts --session-id <id> --append-system-prompt <guide plus memory>`, the same for a fresh and a resumed Lead, in the Lead worktree with the shared Lead env. These are today's Lead flags plus `--mode rpc` and the extension. `--session-id` creates a missing session, so fresh and resumed launch alike; `--session <missing id>` would exit 1 instead. The new `--append-system-prompt` text replaces the old one on a resume, so a memory note added while the Lead rested reaches it (seen in the acceptance run). Shika's worker extension `pi-activity.ts` is not loaded: RPC events give the status, and Lead naming capture is off. The Lead still loads the author's own global Pi extensions, packages and skills, as a terminal Pi Lead does. The stop signal is SIGTERM: Pi's handler kills its tools' detached children before exiting, which a group signal would not reach.

**Handshake and identity.** The only handshake frame is `{"id":"shika-state","type":"get_state"}`; Pi answers in about 160 ms, before any prompt. `data.sessionId` must equal the id Shika passed, otherwise "Pi returned a different conversation", which ends the transport. A send that arrives before the answer waits in the protocol (`Turn::Deferred`) and goes out once it comes. The id is Shika's preallocated UUID (`lead::Identity::Preallocated`), but, as for Codex, it is saved only on `Parsed::Confirmed`, which the decoder sends at the first user `message_end` of a run: Pi writes the session file at the first user message (`SessionManager::_persist`), so a saved id always has a file, and a Lead stopped before any message saved nothing and starts fresh next time. `get_state` carries no Pi version, so "This Pi version could not be read" never names one (`LeadProtocol::version` is `None`).

**A resume Pi did not load is refused.** Pi never refuses a `--session-id`: a missing id, a session file whose header it cannot parse, and an unreadable file all get a new empty session under the same id, with only a stderr warning and exit status 0. Before 2026-11-20 a chat Pi Lead in that state would have carried on with no context and no toast. Since a saved id always has a file with messages, a resumed launch whose `get_state` reports `messageCount: 0` is `Parsed::Fatal("Pi could not resume this conversation")` and nothing is sent; the transport ends inside the 5 second grace, so the [refused-resume rule](#conversation-identity-resume-and-the-fresh-start-fallback) applies: a file still there keeps the id and rests with "The Lead's CLI exited right after resuming...", a missing one starts fresh once with "Could not resume the Lead's conversation. Started a new one." A fresh launch expects 0, and a `get_state` with no `messageCount` is accepted (`a_resume_that_comes_back_empty_is_refused`, `a_resume_pi_brings_back_empty_is_refused_and_keeps_the_saved_id`).

**Turns.** Send is `{"id":"shika-prompt-<n>","type":"prompt","message":<text>}`; acceptance is Pi's response to it, not the pipe write (`write_accepts` is false):

- `success: true` with `data.disposition: "started"`: `Accepted`, and the run's events follow.
- `"handled"`: an extension command consumed the prompt. No run and no `agent_settled` follow, so it is `Parsed::Handled`, which settles the turn with no bubble.
- `"queued"`: only possible with `streamingBehavior`, which Shika never sends; read as accepted anyway.
- `success: false`: the turn fails visibly and the text goes back to the composer: "Pi was busy and did not take the message. Send it again." when the error says "already processing", else "Pi could not complete the reply. Check its model, sign-in and connection, then retry." Raw provider text is never shown. If a run Pi started itself took the slot, that run becomes a CLI-started turn.
- Shika never sends `steer`, `follow_up` or `streamingBehavior`; the app's [queue](#queued-messages) holds messages while the Lead is busy.
- There is no deadline on the prompt response (the 10 second deadline covers only the handshake). A Pi that never answers leaves the turn at `Outgoing` without `Accepted`, Working, until Stop generating, which signals the process for a prompt Pi has not answered.

**Final-response filter**, released only by `agent_settled`:

- Only the record `type` is decoded first (`Head`), so `agent_end` (which repeats every message of the run), deltas, `turn_*`, `queue_update`, compaction and retry records are skipped without building a JSON tree.
- An assistant `message_end` with `stopReason: "stop"` and no `toolCall` block sets the candidate to its text blocks, joined by a blank line. `toolUse` (narration before a tool), `aborted`, `error` and `length` clear it; so do a `toolResult` message and any `tool_execution_start`. `system` and `user` messages are never candidates. An `error` message may hold partial text from a cut stream; it is never shown.
- `agent_end` is not final: retries (`auto_retry_start`, a new `agent_start`), threshold compaction and overflow recovery all come after it, and `agent_end.willRetry` can be false before a recovery. An `agent_start` inside an unsettled run is not a new turn.
- `agent_settled`: `aborted: true`, or our own abort, settles with no bubble ("Reply stopped"). Otherwise the latest assistant message decides: `stop`, `toolUse` or `aborted` publish the candidate (possibly none); `error` fails with "Pi could not complete the reply. ..." (a failed run settles with `aborted: false`); `length` fails with "Pi stopped the reply at the model's output limit."; any other stop reason is unreadable.
- `get_last_assistant_text` is never used: provenance comes from the stream.

**Turns Pi starts itself.** Stock Pi starts none, but an extension can (`pi.sendUserMessage`), and the Lead loads the author's extensions. An `agent_start` while Shika has no turn is `Parsed::Started`, handled as in [turns the CLI starts itself](#turns-the-cli-starts-itself).

**Questions.** Stock Pi 1.4.0 has no question tool, so by the author's Decision 8 Shika ships one: `lead_transport/pi-lead.ts`, written per Lead into a private `0700` temporary folder that the transport removes, and loaded with `--extension`; Pi's own configuration is never touched. It registers `shika_ask_author`: 1 to 4 questions, each with an optional header, up to 4 `{label, description}` options and `multiSelect`, run sequentially. The tool opens one `input` dialog titled `shika-question:<tool call id>` and passes its abort signal; outside RPC mode it tells the model to ask in its reply. The decoder takes the questions from that tool's own `tool_execution_start` arguments, validates them, and opens the native dialog. The answer is the dialog's `extension_ui_response` with `value` set to a JSON object keyed by question text (a label, a list of labels, or the author's words); Cancel answers `cancelled: true`, and the tool tells the model "The author cancelled the question without answering." Other extension dialogs map too: `select` with 1 to 4 distinct options (bare strings, so no descriptions), `confirm` as Yes/No (`confirmed` is true only for Yes), `input` and an empty `editor` as free text. A prefilled `editor`, an empty title, more options or an unknown method are blocked like any unsupported interaction. One dialog shows at a time and the next waits; one with a `timeout` closes itself when Pi resolves it; dialogs left open at `agent_settled` are cancelled. `notify`, `setStatus`, `setWidget`, `setTitle` and `set_editor_text` are fire-and-forget and ignored.

**Interrupt.** Stop generating writes `cancelled: true` for every open dialog, shown, waiting or blocked, and then `{"type":"abort"}`, in one write: Pi's `abort` does not settle while an extension dialog waits (in the Step 1 fixture it waited until the client cancelled). The turn settles at `agent_settled {aborted: true}`, which comes before the abort's response; Pi kills the running tool and keeps the process, and the next prompt works with the aborted tool result in context. A send not yet written settles at once; one written but not yet answered by Pi is signalled. A soft stop still running after 2 seconds gets SIGTERM, then SIGKILL. Workers are untouched.

**Shutdown.** Quit and Stop Lead close stdin; Pi disposes its runtime and exits, also mid-tool, killing the tool. A Pi still running 2 seconds later gets SIGTERM.

**Usage.** Pi is not a usage source (see [token-usage.md](token-usage.md)); nothing is registered.

**Traps.**

- *Silent new sessions.* See "A resume Pi did not load is refused" above. Never rely on Pi to refuse an id.
- *Abort with a dialog open.* Cancel every dialog first, in the same write, or Stop generating hangs until the 2 second signal.
- *The oversized `agent_end`.* It repeats the whole run and is the largest record. Its line past `MAX_FRAME` is dropped by the reader instead of failing the stream (`LeadProtocol::oversized_skippable` matches the `{"type":"agent_end"` prefix; Pi writes `type` first); any other oversized line still fails it.
- *A leading `/`.* An author message that starts with `/` can be consumed by an extension command (`handled`: the turn settles with no bubble) or expanded as one of the author's skills or prompt templates before the model sees it; unknown `/text` goes to the model as text. Shika passes the message unchanged, as a terminal Pi Lead would.
- *A second question tool.* The Lead loads the author's global extensions. A package such as `rpiv-ask-user-question` adds its own `ask_user_question`, which in RPC mode walks its questions with `select` dialogs whose options are numbered labels with their descriptions plus a "Type something." row: up to 3 options fit the select mapping, more are blocked. When asked plainly the model may pick either tool; the acceptance run asked for `shika_ask_author` by name. Not verified natively with the other tool.
- *Process name.* Pi renames its process to `pi`, so `pgrep -fl 'pi --mode rpc'` never matches a Lead. Find it as a child of the app (`pgrep -P <Shika pid>`) or by its cwd in the Lead worktree.
- *`PI_CODING_AGENT_SESSION_DIR`.* With it set, Pi 1.4.0 stores sessions flat in that directory and matches them by header cwd, while `CliHome` looks in a per-cwd folder under it, so Shika reports every saved Pi Lead missing and starts fresh. This affects terminal and chat Pi Leads alike and only when the variable is set; found while isolating the acceptance run, not fixed.

**Fixtures.** `crates/shika-core/tests/fixtures/pi-1.4.0-*-local.jsonl`: normal turns, failure with retries, interrupt with a dialog open, overflow and threshold compaction recovery, extension dialogs, a background (extension-started) turn, resume of an existing and of a missing id, and `lead-question` with Shika's own `pi-lead.ts`. All recorded on 2026-11-20 from installed Pi 1.4.0 against a loopback synthetic OpenAI-compatible provider with a disposable `HOME` and `PI_CODING_AGENT_DIR` and no real account; provenance in the fixtures README. `pi_tests.rs` replays each recording and checks every frame Shika writes against the recorded client frame.

**Debugging by symptom.** Reproduce in a disposable data directory (see [isolated reproduction](#isolated-reproduction)). The Lead's session file is `~/.pi/agent/sessions/--<worktree path with / as ->--/<timestamp>_<id>.jsonl` (`PI_CODING_AGENT_DIR` honored); read it, never edit it.

| Symptom | Inspect first |
| --- | --- |
| "This Pi version could not be read" | No `get_state` answer within 10 seconds, a line that is not JSON or has no `type`, an assistant message without content, or an unknown `stopReason`. Compare `pi --version` and `pi --help` with the fixtures and record a new one. |
| "Pi could not resume this conversation" | `get_state` reported `messageCount: 0` for a saved id: Pi could not find or parse the session file. Check the file above exists, is readable and starts with a session header; a terminal-era Lead's file works unchanged. |
| A resumed Lead remembers nothing, with no error | An older build, or a Pi whose `get_state` lacks `messageCount`. Check the Pi session folder for two files with the same id. |
| The card stays Working after a prompt | `Outgoing` without `Accepted`: Pi never answered the prompt (no deadline). `Accepted` without `Settled`: no `agent_settled` yet, for example a retry backoff or compaction; Pi's stderr is discarded, so run the same command in a terminal to see it. Stop generating settles it. |
| A message got no reply and no bubble | It started with `/` and an extension command took it (`handled`). |
| The Lead asks in prose instead of a dialog | The model did not call `shika_ask_author`. Check `--extension` in the launch and that the private `lead.ts` exists while the Lead runs. |
| "This Lead needs an interaction chat does not support." | A dialog the native one cannot show: a prefilled `editor`, more than 4 options, an empty title, or an unknown method, often from another extension. Stop generating cancels it. |
| Stop generating takes 2 seconds | A dialog was open and was not cancelled before `abort`, or Pi ignored SIGTERM (check `reset_signals`). |
| A Pi process outlives the app | Find it by parent or cwd (the process is named `pi`), and check the quit hook ran. |

### Cursor CLI (planned)

Last in the author's order:

- One long-lived `agent --force --trust --sandbox disabled acp` child (ACP 1); ACP ignores `--yolo`, and a `session/request_permission` that still arrives is blocked, never answered (Decisions 1 and 7).
- `session/new` gives the id; instructions go in a temporary always-applied `.cursor/rules/shika-lead.mdc` written for `session/new` only (Decision 3), and memory changed since the session was created goes as one `[shika]` memory line on the next resume.
- `session/load` replays history as updates, which must be swallowed. A terminal-era chat id cannot be loaded, so it starts fresh with the refused-resume toast (Decision 2).
- The final is the `agent_message_chunk` text after the last `tool_call`/`tool_call_update`, released by the `session/prompt` response with `stopReason: end_turn`.

### Contributor guardrails

These must not change without the author's decision:

- Filter before persistence. Only the final response after the last tool call, released by a successful terminal event, may be stored or shown. Never show an aggregate `result` text, a partial, thinking, tool output, subagent text or stderr.
- No PTY or hidden terminal behind the chat, no screen scraping, no raw JSON in the UI. Unknown optional events are ignored; unknown required ones fail visibly.
- No model API, provider SDK, credential handling, or global CLI config writes. Arguments are arrays; JSON and TOML values are built with encoders.
- The Lead guide, prompts and memory delivery stay as they are. No chat-shaping instructions.
- Never replay history into a CLI, never resend automatically, never answer a question or confirm Close on the author's behalf. (`shika pr` publishing on the author's request is not a confirmation Shika gives; it is the Lead's command.)
- Keep generation, epoch and live-identity fencing; commit ids only under the operations lock for the exact live Lead.
- Keep pipes drained off screen, frames and queues bounded, IO off the UI thread, and explicit reaping at quit.
- Stop generating stops only the Lead's own process group, never worker PTYs.
- History stays local, private, unlimited, and separate from memory. Schema changes bump `user_version` additively; never overwrite or reset a store that fails to open.
- Every chat color comes from `Chrome`. No hex values in chat views.
- A chat CLI starts with default signal dispositions and an empty mask (`reset_signals`), as worker PTYs do. Its stop signal must reach it; never rely on the SIGKILL deadline.
- Never copy Zed's GPL markdown, editor, terminal or terminal-view code.
- Workers, closed tasks and non-chat Leads get no transcript.

## Author's decisions

- This is a UI improvement for the Lead. Keep today's lifecycle, instructions, worker behavior, automatic fresh-start fallback and toasts, Seen rule and the author-confirmed Close dialog.
- Per turn, show only the author's message and the Lead's final response. Hide thinking, tools and their output, and intermediate assistant messages. Bash and code in the final response render as markdown; no prompt change to suppress commands.
- Shika owns visible history for the Lead's lifetime, with no age or size limit. Stop and quit keep it; Forget deletes it with the saved conversation link so a new Lead starts empty. Memory notes keep today's behavior and survive Forget.
- An existing saved Lead resumes its saved conversation id in chat. Only visible history starts empty, without import or a migration confirmation or notice.
- Every chat color derives from the active theme chrome and palette for every catalog theme, light and dark and translucency. No hard-coded chat colors.

## Approved design values

The author approved these geometry values:

| Value | Purpose |
| --- | --- |
| Bubble radius **22px** | Fully rounded for a single line: 20px line height plus 12px top/bottom padding makes a 44px pill. Both roles share it. |
| Composer radius **24px** | Fully rounded at the existing 48px minimum height; keep the radius when it grows. |
| Bubble width cap **80% of the lane** | Preserve side alignment at narrow widths; also cap at the existing 480px size. |
| Composer growth **six text lines**, capped at **144px total** | Existing 20px line height plus 12px top/bottom padding, then internal scrolling with the existing scrollbar. |
| Time separators **five minutes** | Centered local-time separator when the gap reaches this threshold; always separate a new local day. |

No new color, font size, shadow or animation values. Lane width 640, insets 20, gaps 6/12/20, header 48, and type 14/20 and 11.5 reuse the existing design system.

Approved after the 2026-10-17 prototype review:

| Value | Purpose |
| --- | --- |
| Antler mark `ANTLERS_ICON`, line art in ink-2 | Replaces the `L` initial tile. Header 24px tile (radius 6, mark 16px); Leads card 30px tile (radius 8, mark 18px); empty state 52px tile (radius 14, mark 30px). Raised fill; no status color. |
| Header `{project} Lead` and worker summary | 14/20 semibold ink-1 plus " Lead" 14 medium ink-3; summary in 12 ink-3 with a 6px status dot, hidden when resting. |
| Empty state | Mark tile, "What should we work on?" (17 medium ink-2) and the composer as one centered group, gaps 14, no dock until the first send. |
| Composer | No border; 32px round action button (send arrow 16px, stop square 7px); 20px left and 8px right text padding. |
| Activity steps | Current step 12 ink-3, up to 3 finished steps 12 ink-4 with "✓ ", gap 4, from socket commands only. |
| Leads card line two | The current step while Working, else the first line of the latest reply, else "{CLI} · Lead". |

## Evidence and acceptance

- Unit and integration tests listed above, hermetic.
- Installed-CLI fixtures recorded against a local synthetic server, without authenticated requests.
- Claude Code: one isolated native smoke run recorded in [MANUAL_CHECKS.md](../MANUAL_CHECKS.md#claude-code-lead-chat-2026-10-16): final-only rendering after a real Bash tool, markdown, a native question, Stop and relaunch keeping history, a further send on the same conversation, Shika Light and Dark.
- Codex: an isolated native acceptance run with the author's real Codex login on 2026-11-20, recorded in [MANUAL_CHECKS.md](../MANUAL_CHECKS.md#codex-lead-chat-2026-11-20): final-only reply after a real tool, a native question answered and cancelled, Stop generating mid-tool with a worker kept, Stop Lead and relaunch with recall, the refused-resume fallback and toast, Forget keeping memory, the doorbell through chat, Shika Light and Dark, no child after quit.
- Pi: an isolated native acceptance run with the author's real Pi login on 2026-11-20, recorded in [MANUAL_CHECKS.md](../MANUAL_CHECKS.md#pi-lead-chat-2026-11-20): a saved terminal-era Lead resumed in chat with empty history and its context, final-only replies after real tools, `shika_ask_author` answered and cancelled, Stop generating mid-tool with a worker kept, Stop Lead and relaunch with recall and a new memory note, both refused-resume paths, Forget keeping memory, the doorbell through chat, Shika Light and Dark, no child after quit.
- Still open for all three: compaction, every theme and translucency mode, all worker and dialog paths, keyboard and screen reader, IME, long histories, crash and write-failure recovery. Do not claim these from tests.

Structured stream formats are documented but not a stable public API. Keep per-version fixtures, tolerate unknown optional fields, and fail visibly when lifecycle or identity evidence is missing. Attachments, voice input and reactions are possible follow-ups outside this design.
