# Lead chat

Status: proposal for the author to approve, 2026-10-16. Design phase only. No transport or UI described here is implemented. The current terminal Lead remains specified by [lead-agent.md](lead-agent.md) and [shika-cli.md](shika-cli.md). The proposed visual contract and light/dark plates are in [DESIGN.md](../design/DESIGN.md#lead-chat-proposal).

## Intent and boundary

Talking to the Lead should feel like messaging a coworker: your messages on the right, its replies on the left, a quiet composer below. The Lead still runs the installed CLI using the author's existing login and plan. Shika starts that executable and exchanges structured records over local pipes. It does not call a model API, extract credentials, embed a provider SDK, or put a terminal behind the chat view.

Only the Lead changes. Workers keep their real PTYs, terminal tabs, activity detection, reports, diffs, notifications, and safe publishing/Close flows. Keep one Lead per project, its verified fixed detached worktree, no preparation, no editing by the Lead, the four-worker limit, project scope, ownership checks, and the existing control socket. The Lead still manages through the absolute `shika` command supplied in its instructions. Stop leaves workers as ordinary author-owned cards.

This is an explicit proposed exception to [CONTRIBUTING.md](../CONTRIBUTING.md#product-principles): the real terminal as the interface, not being a chat client, no custom transcript/question parser, and no conversation history. Recommend a narrow exception for a project's current Lead conversation only. Workers and closed tasks get no transcript browser. Approval must precede implementation and updates to those product rules; this proposal does not change them.

## Verified CLI surface

The installed binaries were queried on **2026-10-16**, using each one's version command and the help commands below. This verifies option spelling and advertised modes, not authenticated multi-turn behavior. No paid prompts or normal provider session files were read for this design. Commands below are launch shapes, not shell scripts: implementations must build argument arrays and JSON/TOML values with encoders.

| CLI | Installed version | Help checked | Recommended process model |
| --- | --- | --- | --- |
| Claude Code | 2.1.302 | `claude --help` | One long-lived child, bidirectional stream JSON |
| Codex | codex-cli 0.162.1 | `codex exec --help`, `codex exec resume --help` | One child per turn, resume the exact thread |
| Cursor CLI | 2026.10.14-941492e | `agent --help`, `agent create-chat --help` | One child per turn, resume the exact chat |
| Pi | 1.1.0 | `pi --help` | One long-lived RPC child |

Help does not define JSON record schemas or prove that arbitrary configuration keys work. Event details below additionally use [Claude's programmatic guide](https://code.claude.com/docs/en/headless), [Codex non-interactive documentation](https://learn.chatgpt.com/docs/non-interactive-mode), [Cursor output formats](https://cursor.com/docs/cli/reference/output-format), and the installed Pi 1.1.0 package's `docs/rpc.md`, `rpc-commands.md`, `json.md`, and `rpc-extension-ui.md` ([upstream RPC reference](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/docs/rpc.md)). Online schemas may describe a different release. The implementation must capture sanitized fixtures from these installed versions before enabling an adapter; the flag check alone is not that acceptance.

### Claude Code

Common arguments: `claude -p --input-format stream-json --output-format stream-json --verbose --include-partial-messages --dangerously-skip-permissions --append-system-prompt <instructions> --system-prompt-snapshot off`.

- Fresh: generate a v4 UUID and add `--session-id <uuid>`. Start in the verified Lead worktree with stdin left open. Send the first author message as one JSON user record when the author presses Send, not as a bootstrap turn. Further messages use that same pipe, one turn at a time. A user record has `type: "user"` and `message: {role: "user", content: <text>}`; confirm the full envelope against a live fixture before implementation.
- Instructions: the current Lead guide plus project memory, delivered through the checked append flag. The snapshot flag's `off` value is explicitly documented in this installed help: it avoids a resumed conversation silently retaining an old guide until compaction. This is a proposed chat launch change, not a claim about today's terminal Lead. Verify compaction as well as resume.
- After Stop/relaunch/interrupt: use `--resume <saved-id>` instead of the fresh session flag, with all common arguments again. Subsequent turns stay in the resumed process.
- Identity: compare `session_id` from `system/init` and subsequent records with the requested UUID; save the confirmed id in `projects.json`'s `lead.sessionId`. A preallocated id is provisional until confirmed. Never adopt a different session opportunistically.
- Rendering: top-level assistant text blocks and their text deltas only; reject messages with a parent tool identity. Reconcile partial blocks with the completed assistant message by message/block identity. A `result` settles the request but must not duplicate its already-rendered text. Neither a finished text block nor a tool result ends the turn.

Reason: the installed help explicitly advertises realtime streaming input, so a process can retain the conversation and avoid repeated CLI startup. Pipes must drain even while this Lead is off screen. For the first build, Stop generating interrupts the process group and reopens the same conversation only on the next send; do not invent an stdin interrupt record. A future capability-checked native interrupt can preserve the process after its protocol is separately verified.

### Codex

Fresh launch shape: `codex exec --json --dangerously-bypass-approvals-and-sandbox -c <instructions-override> -c <trust-override> -`. Resume shape: `codex exec resume --json --dangerously-bypass-approvals-and-sandbox -c <instructions-override> -c <trust-override> <saved-id> -`.

- Write exactly one prompt to stdin, then close it. Both help pages explicitly accept `-` to read the prompt from stdin. Set the process cwd to the verified Lead worktree; do not assume an exec-only cwd option is available on resume.
- Instructions: TOML-encode `developer_instructions=<guide plus memory>`, as today's Lead does. The `-c` option is help-verified; that key's behavior is the repository's 2026-10-15 synthetic-server evidence in [lead-agent.md](lead-agent.md#implementation-map), not something help promises. Repeat it on every fresh/resumed exec and reverify it specifically in exec mode. Repeat today's per-process `projects={"<worktree>"={trust_level="trusted"}}` override too. No global config writes.
- Identity: capture `thread_id` from `thread.started`, persist it under `lead.sessionId` with the live Lead identity check, and require it to match on resume. No newest-thread scan or two-second id polling for chat. Always resume the saved UUID, never whichever session ran most recently.
- Events: render only `item` records whose type is `agent_message`. Upsert by item id; completion replaces an earlier snapshot. Keep reasoning, command execution, tools, and all other item types out of chat. `turn.completed` settles successfully; `turn.failed` or `error` settles with an app error. Expect message-level delivery where exec does not supply text deltas; do not fake token streaming.

Reason: exec advertises one prompt and a finite JSONL output, not a streaming input conversation. A successful child exit after a terminal turn event is normal and leaves the logical Lead Ready, not resting. The next send runs exec resume. A clean exit without a terminal event is an incomplete turn, never success. A persistent Codex server would be another possible CLI integration, but is not the transport selected by this proposal.

### Cursor CLI

Common turn arguments: `agent -p --output-format stream-json --stream-partial-output --yolo --trust --sandbox disabled --resume <saved-id> <prompt>`.

- Fresh: run the checked `agent create-chat` command in the Lead worktree, validate the returned id, then save it in `lead.sessionId`. This allocates an empty conversation without a model turn. Unlike today's fallback, failure to allocate stops startup with Retry; do not launch an unbound conversation and guess its id later.
- Instructions: installed help has no system/developer prompt option. Prepend the current guide plus memory in a clearly delimited Shika instruction preamble to **each** turn's single positional prompt, with the actual author text in a separate delimited part. It has user-message priority, not system priority. This deliberately improves on today's fresh-only bootstrap, keeping resumed chats aware of the current guide without editing provider rules files. Hide the preamble from the author's bubble. A doorbell turn uses the same preamble with Shika event content in place of author text.
- Resume: the same common turn arguments and exact saved chat id for every send. No keep-alive process: help exposes streaming output but no stream-input option.
- Identity: require `session_id` from `system/init`/subsequent records to match the allocated id before accepting assistant text. Do not use a previous/latest-chat selector. Cursor has no dependable local session-existence reader in current Shika.
- Rendering: reconstruct top-level assistant text, exclude tool-call records, and reconcile partials without appending flush copies. The current Cursor reference distinguishes new partials by `timestamp_ms` present and `model_call_id` absent; other assistant flushes duplicate text. This unusual rule needs version-specific fixtures. The terminal `result` is a completion marker, not an extra bubble. A missing result or nonzero exit is failure.

Reason: one child per turn fits the advertised print mode. Startup cost and repeated instruction tokens are real tradeoffs, but adding an undocumented input protocol is worse. Chat cannot repair Cursor's lower-priority instruction channel; keep the Lead's existing authority checks in Shika regardless of what it says.

### Pi

Launch shape: `pi --mode rpc --approve --append-system-prompt <instructions> --session-id <uuid>`.

- Fresh: generate an exact UUID. Launch with the guide plus memory as appended system instructions. Before sending a prompt, issue RPC `get_state` with a unique request id and verify `data.sessionId`. RPC emits no JSON-mode session header. Save the confirmed id in `lead.sessionId`; a never-used id may still have no durable session file.
- Turns: one RPC `prompt` record containing the new message, then wait for its correlated acceptance response and session events. Do not queue prompts or use steering/follow-up queues in the first chat version. A handled prompt that starts no run must settle without waiting forever for a run-end event.
- Resume: precheck today's exact-id session existence, then start the same launch shape with the saved id and current instructions. Pi's help explicitly says `--session-id` creates a missing id, so this flag alone is not evidence of successful resume. A known missing conversation follows the explicit recovery flow below.
- Rendering: `message_update` / `assistantMessageEvent.text_delta` for an assistant text block; authoritative `message_end` replaces its accumulated text. Thinking and tool-call blocks never enter the UI. Settle on `agent_settled`, checking abort/error outcomes; `agent_end` can precede automatic recovery and is not final. RPC command acceptance is not turn completion.
- Interrupt: send RPC `abort`, wait for settlement, keep the process if healthy. No queued inputs means abort cannot unexpectedly start a queued turn. On a stuck abort, terminate/reap the process group and recover on next send.

Reason: RPC has bidirectional input, correlation, cancellation, and explicit settlement. `--mode json` is also help-verified but is a finite event stream for supplied prompts; it is not selected. The implementation should use original Rust protocol code, not embed Pi's TypeScript SDK. Installed package docs, rather than guessed CLI flags, define these RPC verbs.

## Process and message ownership

Separate a **logical live Lead** from a child process and from a turn. It remains live between Codex/Cursor children and therefore retains the token, worker ledger, worker ownership, and doorbell. Stop revokes that logical identity and ends any child; it does not merely wait for a per-turn process to exit. Quit stops all Lead children and retains saved records/trees; the next launch shows a resting row. Do not warm up a CLI with a synthetic author prompt.

Core owns process groups, pipe readers/writers, cancellation, and session persistence. The app owns messages, focus, attention, dialogs, and notifications. Use the existing login-shell PATH discovery and child environment cleanup, fresh `SHIKA_SOCKET`/`SHIKA_TOKEN`, and stable absolute command path. Never inherit worker report credentials as a Lead credential. Keep the fixed-worktree identity checks and operations-lock commit recheck from the current Lead implementation.

Each adapter emits a small common vocabulary: conversation confirmed, input accepted/rejected, assistant text delta/snapshot, turn settled, structured blocker, recoverable error, and transport exit. Every event is fenced by project, logical Lead generation, process generation, and turn id. Late events from a stopped/replaced child cannot update a card, save an id, or append to a different conversation. Deduplicate on provider message/block/item identity, not matching text; identical sentences can be legitimate separate messages.

There is one active turn. Send becomes Stop while Working/Asking; the author may draft the next message, but Enter does not send or queue it until the current turn settles. Preserve drafts per project in memory while switching cards, not on disk in this proposal. IME composition consumes Enter/Escape before chat actions. A failed input write is a failed delivery, not permission to resend automatically. Once any bytes may have reached a CLI, retry is an explicit author action because workers may already have started.

## Visible conversation

The transcript contains author text, the Lead's top-level assistant text, and small Shika event/error rows. No per-message avatars or names. Stream into the current bubble when reliable deltas exist; separate completed assistant messages stack as separate bubbles. Do not split each paragraph or token into its own bubble. Render the agreed light markdown subset in [DESIGN.md](../design/DESIGN.md#lead-chat-proposal).

Tool names, call arguments, shell commands used for execution, tool stdout/stderr, tool results, reasoning/thinking, usage events, system prompts, bootstrap preambles, session ids, CLI banners and chrome are never rendered or stored in the visible transcript. Unknown event types are ignored, not printed as JSON or treated as text. No expandable tool cards, raw-log drawer, or hidden terminal UI.

Use a static **"Working..."** line at the Lead edge while a turn runs. Do not infer labels such as "Checking on workers..." by parsing shell command strings. A known outstanding Shika `wait` may say **"Waiting for workers..."**, and an author dialog says **"Waiting for your decision"**, derived from Shika's own control state. These are temporary activity labels, not extra card statuses. No pulsing dots or new animation loop. The normal small working pixels remain on the column card.

The Lead's guide should continue to ask for short, conversational outcomes, one decision per message, honest test attribution, and full PR URLs. Add a chat-specific instruction to omit commands, logs, and routine retries. Allowing assistant text does not technically guarantee that a model never repeats a command in prose: event filtering guarantees the machine-data boundary, not semantic censorship of generated text. Do not claim otherwise or build an unreliable regex censor. Code blocks are supported for deliberately explained code, not an execution log. Confirm this interpretation with the author (Q6).

## Worker events and the doorbell

Keep the ledger's `(worker, status, turn, report receipt)` identity, batching and ownership from [shika-cli.md](shika-cli.md#the-doorbell). Replace Lead terminal paste/delayed Enter with one internal submission through the selected adapter. The content still identifies itself as `[shika] Workers changed: ... Run shika wait.` so the existing guide understands it. It is never labelled as the author's words.

- Wait for the existing 1.5 second batch, no pending/in-delivery `wait`, no active/blocked turn, no unprocessed input, no composer draft, and 3 seconds since author typing. Replace "live PTY" with "live logical Lead with a usable transport or a per-turn child ready to start". A resting Lead is never automatically resumed.
- Show a centered, muted event row when delivery starts: **"Fix login redirect is ready to check"**, **"Fix login redirect is asking you"**, or **"Fix login redirect exited"**. Use task title, with CLI only to disambiguate duplicate titles. Ready means its turn ended, not verified success, so avoid "finished" as a claim of success. A late report updates the same worker-turn row with **"Report updated"**; it does not duplicate the whole result.
- A batch may show several short rows together. Rows are keyed internally by Shika-owned event ids; no ids or worker report contents are painted. The Lead's own reply explains outcomes and decisions. Worker rows do not steal focus or scroll a reader away from older messages.
- An accepted internal submission starts a normal Lead turn and notification budget. It is persisted as origin `shika_event`, with delivery state, never as an author bubble. Mark rung only after transport acceptance (or a successful write where no explicit ack exists). A bell does not mark reported; a delivered `wait` reply still does. Keep disconnected-wait handling and identical-report idempotency.
- A failed/uncertain send retains the event and shows a concise delivery error. Do not automatically retry an uncertain partial write or replay it after restart. Explicit retry may repeat a notification to the model, so the event id accompanies it for deduplication. Definitely unsent events may be batched again.

The restart summary uses the same internal-input path and gates, once: show **"Shika restarted. Worker worktrees were kept."** and any checkout warning as event rows; submit the detailed existing summary privately to the Lead. Keep today's `reportedWorkers` accounting after delivery. Workers do not resume and the Lead must ask the author how to continue. A `wait` already running inside a turn receives worker events normally; do not start a second turn for them.

## Lead status and attention

Use the existing four states and signals on the Lead card, without screen scraping, title hints, idle debounce, or the two-second quiet heuristic. Those worker mechanisms remain unchanged.

| State | Chat evidence and transition |
| --- | --- |
| Waiting | Live logical Lead before its first submitted turn. Process initialization alone starts no turn. |
| Working | An author/event submission is being dispatched, accepted, or executing. Includes tools, silent intervals, retries, compaction, and a blocking `shika wait`. |
| Asking you | A Shika PR/Close dialog or a supported structured input request actually blocks the active turn. Prose questions in a completed assistant reply remain Ready. |
| Ready to check | A verified terminal turn result, handled no-run input, interruption, or surfaced failure. Check success separately. Routine per-turn child exit leaves this state intact. |

Start the elapsed clock once at dispatch; rejection settles it as a failure, not a successful result. Retain its epoch across blockers and retries; drafts, resize, scrolling, focus and partial messages cannot reset it. It includes blocked time and paints only while Working. One notification per turn at the first blocker or completion, using today's project/Lead title and routing. Keep active/blocked safe-Stop confirmation and pending-dispatch protection.

In chat, focusing an empty composer while scrolled up does not prove a result was read. Proposed Lead-only seen rule: clear Ready when the newest completed reply/error is visible in the active window and chat has focus, or after the selected newest result remains visible for the existing one-second dwell. Selecting an old part of the transcript keeps the dot. Resting Leads retain no live signal. This refines the current terminal-focus seen rule and requires approval (Q4).

## Decisions, interruption, and failures

`shika pr` and `shika close` remain the author's existing dialogs. A chat message, a quoted "yes", or a provider approval event can never confirm either. Keep ownership, stale preview, worker status/draft checks, and all Git safety checks. While the request blocks, the Lead is Asking and chat shows "Waiting for your decision". The worker card is selected for the dialog as today; closing/cancelling restores the Lead conversation, its scroll anchor, draft, and composer focus. The result goes back to the CLI's blocked shell tool; it is not echoed as raw tool output. If the Lead was stopped meanwhile, answer/disconnect through the existing flow and do not recreate it or reclaim focus. Direct clean Close remains direct.

No general replacement for arbitrary provider widgets is promised. Pi RPC dialog methods with known schemas can map to the existing native dialog shell and correlated responses, keeping Asking until resolved; notification/widget/status requests are not blocking. Unknown extension dialogs are cancelled through their documented response where possible and surfaced as **"This Lead needs an interaction chat does not support."** Claude/Codex/Cursor print modes may deny or fail interactive-only tools rather than emit a usable question. Do not infer a blocker from assistant prose or invent a response protocol. Unsupported blockers end with an actionable error and Retry/Stop; this limitation is an implementation gate per provider, not an invisible terminal fallback. Tool permission mode remains today's automatic approval mode, without new Shika tool-approval prompts.

| Situation | Chat behavior |
| --- | --- |
| Stop generating / Escape with chat focused | Interrupt this turn only. Show "Stopping..." until abort settles or the owned process group is reaped, then "Reply stopped". Keep partial text with that qualifier. Workers already started keep running. Next send resumes the same conversation. Escape dismisses an overlay/IME first. |
| Claude, Codex, Cursor interrupt | Send SIGINT to the owned group, continue draining, then bounded termination if necessary. Reap before another child starts. CLI persistence after interruption must be tested; no immediate resubmit and no raw Escape byte into a pipe. |
| Pi interrupt | RPC abort, then the same bounded termination fallback. Do not end at its acceptance ack alone. |
| Authentication, rate limit, network/provider error | Plain app-owned error row, e.g. "Codex could not sign in. Sign in with Codex, then retry." Keep the author's bubble and any partial reply. Retry sends only on an explicit action and warns that work may already have started. No CLI credential prompts inside chat. |
| Unexpected long-lived CLI exit | Settle the active turn as failed, then rest the Lead with transcript visible and "The Lead stopped unexpectedly" plus Resume. Keep id and worktree. An idle crash creates no fake completed turn notification. |
| Expected Codex/Cursor exit | After a valid final event, stay logically live and Ready. Missing final event is "The reply ended before it was complete" even on exit code zero. |
| Expired, missing, mismatched, or refused resume | Keep transcript and id until the author decides. Show "Could not resume this conversation" with Retry and Start new conversation. Start new is explicit, with the transcript consequence explained. No automatic fresh start based on a five-second exit. Authentication failures must not erase history. |
| CLI missing or disabled | Transcript stays readable. Composer is disabled with "Claude Code is unavailable" and Open Settings. Offer Choose another CLI through the Lead picker; it explicitly starts a new conversation, never sends the old transcript to a different provider. Do not install a CLI. |
| Malformed stream / unknown required schema | Stop accepting text, settle with "This CLI version could not be read" and the version. Drain/terminate boundedly. Unknown optional events may be ignored; an unrecognizable required lifecycle event cannot become success. |
| Transcript write failure | Keep existing data, stop new sends, show "Could not save this conversation" and Retry save. Never silently turn persistence off or replay a prompt. |

Errors are facts in muted event rows with neutral actions, not fake assistant bubbles or red status dots. Extract known error categories; never dump stderr, JSON, commands, or tokens into the transcript. Routine diagnostics remain metadata-only. Do not kill descendants belonging to worker cards when interrupting a Lead; they are app-owned processes, not its shell-tool group. Stop Lead uses the existing confirmation and worker reassignment; Stop generating does neither.

## Persistence decision

The CLI's conversation is model context. The visible transcript is the author's record. They need separate identities and lifecycles; replaying saved bubbles into a resumed CLI would duplicate context and actions.

| Option | Benefits | Costs and limitations |
| --- | --- | --- |
| Read each CLI's own files on open | Avoids a second copy of visible content; can recover pre-chat messages where available | Four private layouts, compaction/branching, tool and thinking filtering, missing/pruned/encrypted files, and no dependable Cursor reader. Slow history reads and provider upgrades can erase or reorder what the author saw. Does not reliably meet cross-provider relaunch scrollback. |
| Shika-owned visible transcript per project | Same scrollback across all four CLIs; exact role/event provenance, interruption markers, and stable ordering; no provider-file parser for normal chat | New sensitive local content storage, retention/deletion and crash recovery responsibilities; old terminal history is not magically imported; it does not restore provider context. Requires a product exception. |

**Recommend Shika-owned storage**, limited to the current Lead conversation. Proposed location: `<data dir>/leads/<project-id>/chat/`, separate from `memory.json`, with a schema version, conversation generation, CLI/id binding, and append-only records plus a rebuildable index. Extend the saved `lead` record in `projects.json` with an optional chat generation/transport discriminator; preserve existing `cli`, `sessionId`, and restart accounting. No message bodies in `projects.json` or repository files. Use private directories/files, off-thread writes, serialized appends, and atomic metadata replacement; no account, sync, or cloud storage from Shika.

Store author text once, normalized assistant text only, visible Shika events/errors, local timestamps, message/turn ids, delivery states, and completion/interrupted markers. Exclude reasoning, tool payloads, stderr and credentials. Coalesce partial text checkpoints off-thread rather than one disk record per token; commit completed messages durably. Persist the outgoing record before dispatch and record acceptance separately. After a crash, an unconfirmed outgoing record is **"Delivery unknown"**, never automatically resent; an open reply is **"Reply interrupted by restart"**. No cross-file exactly-once claim: reconcile the transcript header and saved Lead binding before continuing.

Loading history never starts a CLI. Load the newest page first; fetch older pages on scroll, with a stable anchor. A failed or corrupt transcript is preserved and reported, not overwritten as empty. Restore reading position in memory between cards; on app restart open at the newest saved message. Reloading history does not replay old notifications or mark an old turn Working.

Proposed retention: keep the current conversation until the author explicitly starts a replacement, Forgets the Lead, or removes the project. Stop and quit retain it. Forget's confirmation must explicitly say **"Deletes Shika's chat history and its link to the CLI conversation. The CLI keeps its own history. Memory notes stay."** A confirmed new conversation/CLI change clears the current visible transcript after the replacement binding is safely stored; do not offer an archive browser. Project removal deletes chat with other project app data, retaining existing worktree rules. Provider history is never deleted by Shika. Memory notes retain today's independent semantics. No automatic age expiry or silent truncation in the proposed first version; storage limits need the author's decision (Q2).

## Migration from a terminal Lead

Do not change an existing record just by opening the project. A legacy saved Lead appears resting as today. The proposal recommends a one-time **"Continue this Lead in chat?"** explanation on its first resume after the feature is enabled: same CLI and conversation; earlier terminal messages are unavailable in Shika chat; the CLI can still remember them; new messages will be saved locally. Continue in chat or Cancel. No automatic import, synthetic recap, or new provider session. This is an approval of the migration behavior, not an implementation in this task.

After confirmation, use its exact saved id with the selected adapter and bind a fresh visible transcript generation. Its first row says **"Earlier messages are in the CLI's history. New messages appear here."** Claude/Codex/Pi get the current instructions; Cursor gets the per-turn preamble on the first real send. Nothing is sent just to show history. An unused legacy Lead with no id starts fresh on its first send, with an explicit boundary row. Refused resume follows recovery above. Switching provider or starting new asks about clearing Shika's current chat first. There is no rollout-time terminal toggle in the proposed final interface; Cancel leaves the Lead resting. A still-running terminal Lead at an upgrade is stopped through the normal app restart before migration.

## Implementation map and build order

Names below are proposed modules, not files created by this change. Read current symbol maps in [lead-agent.md](lead-agent.md#implementation-map), [shika-cli.md](shika-cli.md#code-map), and [agent-activity.md](agent-activity.md#code-map).

| Area | Planned work |
| --- | --- |
| `shika-core/src/lead_transport/` (new) | Original Rust child/pipe supervisor, per-CLI args and structured adapters, common events, process generations, cancel/reap and bounded framing. No GPUI. |
| Core `lead.rs`, `agents.rs`, `session.rs`, `lib.rs`, `path_env.rs` | Separate logical Lead lifetime from a PTY/per-turn child; reuse worktree/env/discovery invariants. Chat-specific argument builder instead of changing worker launch flags. |
| Core `lead_chat_store.rs` (new), `projects.rs` | Visible transcript/index, binding and crash reconciliation, optional backward-compatible saved fields; generation-checked id commits. `memory.rs` stays independent. |
| App `lead_chat.rs`, `lead_chat_markdown.rs` (new) | Virtualized conversation, composer, selection/copy, light markdown, scroll anchors, unread affordance, errors and native accessibility. |
| App `main.rs`, `model.rs`, `activity.rs`, `lifecycle.rs`, `notifications.rs` | Chat routing/focus and logical Lead state reducer, turn clock/seen integration. Exclude chat from terminal screen detection and host naming capture; keep worker behavior. |
| App `control.rs`, `lead_guide.md` | Doorbell/internal submission path, existing ledgers/dialog return focus, chat-oriented guide wording. Keep socket/token/worker report semantics. No new control command is required for basic chat. |
| App `appearance.rs` | Reuse existing chrome fields; any additional field needs Shika Light/Dark/glass values and a derivation, approved before code. |
| `shika-terminal` | No chat rendering or transport; worker terminals remain intact. Audit callers that currently assume every live card owns a terminal. |

1. **Approve this spec first.** Resolve Q1-Q6, then update conflicting product clauses and migration/retention wording as part of implementation.
2. **Claude Code first.** Capture structured fixtures and real fresh/two-turn/resume/compaction/interrupt/error behavior in disposable repos and isolated data. Build the common transport/store, state reducer, and chat view. Check worker launch/report/doorbell and author dialogs end to end. Do not ship the other chat choices until their adapters pass.
3. **Codex.** Validate exec-specific developer instructions and exact thread resume. Test normal child exit versus failed/incomplete turn, duplicate item snapshots, and process cancellation without affecting workers.
4. **Cursor CLI.** Validate create-chat/resume durability, per-turn preamble, current partial/flush deduplication, missing id and login failures. Check that conversation context persists across print invocations on the installed release.
5. **Pi.** Validate RPC acceptance versus settlement, missing exact ids, compaction/retry, abort, and supported/cancelled extension dialogs. No quiet fallback for chat.
6. **Integrated acceptance.** Light/dark, derived light/dark catalog themes, solid/sidebar-only/full frost, Reduce transparency/motion, 960x600 and hidden column, long text/code/links, IME, keyboard-only, screen reader and text copy. Stress long paged transcripts and streaming while another worker terminal is visible. Record actual outcomes in `MANUAL_CHECKS.md`; this proposal establishes none of those passes.

## Risks and verification boundaries

- Structured mode entry points are documented, but event formats and their version compatibility are not a stable public API contract Shika can assume indefinitely. Help verifies flags only. Keep per-version fixtures, tolerate unknown optional fields, and fail visibly on missing lifecycle/identity evidence. Never fall back to printing raw records or scraping a hidden Lead terminal.
- CLI session persistence can fail independently of Shika's visible history. Verify real conversation recall after Stop/relaunch for each provider, not just equal ids. Cursor instruction priority and Pi's create-if-missing semantics are specific risks. Claude partial/completed messages and Cursor flushes can duplicate text without careful reconstruction.
- The selected non-interactive modes do not all support arbitrary questions/permissions. Do not claim universal Asking coverage. Prove supported interactions per CLI, fail unsupported ones plainly, and preserve author confirmation exclusively in Shika's PR/Close dialogs.
- Implement the markdown subset and interaction in Shika's own code, optionally using a compatible MIT/Apache parser after license review. Never copy Zed's GPL markdown, editor, terminal or terminal-view crates. No web view, executable HTML, remote image fetching or terminal escape interpretation.
- Long transcripts need a virtualized, paged variable-height list with stable message ids, cached text shaping/layout and bounded off-screen state. Parse and index off-thread; batch deltas for repaint. No entity per historical paragraph or whole-transcript relayout per token. Tall messages must render visible blocks lazily, not defeat virtualization with a single unbounded entity.
- Displaying only top-level assistant text hides machine events, but can still expose prose the model chooses to quote. The conversational guide is a behavioral instruction, not a security filter. Local transcript storage also creates a new copy of anything the author or Lead says; deletion semantics must be clear.
- Interrupting a shell tool can leave external work already completed. Keep worker records authoritative; never treat a stopped reply as rollback or automatically replay a message, bell, publishing request or close request.

## Open questions for the author

1. **Lead-only product exception and migration:** approve a chat interface and local visible history for the current Lead conversation, while workers/closed tasks keep today's rules? Recommendation: yes, with the explicit legacy migration notice and no automatic import. This also approves adapting known Pi RPC dialogs, without a general provider-widget parser.
2. **Storage and replacement:** approve Shika-owned visible-only history, kept through Stop/quit and deleted on confirmed Forget/replacement/project removal, with memory notes kept separately? Recommendation: yes. Is retaining the current conversation without age expiry acceptable, or should it have an explicit user-controlled size/retention limit before shipping? Provider files remain the authority for model context.
3. **New view and geometry values:** the existing design rules require approval before new values/views. Approve the chat/composer/markdown views and header initial avatar, plus the following proposed values? They are proposals here, not additions to the shipping token set:

   | Proposed value | Purpose and recommendation |
   | --- | --- |
   | Bubble radius **22px** | Fully rounded for a single line: the reused 20px line height plus 12px top/bottom padding makes a 44px pill. Both roles share it. |
   | Composer radius **24px** | Fully rounded at the reused 48px minimum height; keep this radius when it grows. |
   | Bubble width cap **80% of the lane** | Preserve the side alignment at narrow widths; also cap at the reused 480px size. |
   | Composer growth **six text lines**, capped at **144px total** | Reused 20px line height plus 12px top/bottom padding, then internal scrolling. |
   | Time grouping threshold **five minutes** | Centered local-time separator when the gap reaches this threshold; always separate a new local day. |

   No new color, font size, shadow, or animation values are proposed. Lane width 640, insets 20, gaps 6/12/20, avatar 24/radius 6, header 48, and type 14/20 and 11.5 already exist in this design system. Light/dark plates illustrate pending geometry; they are not implementation permission.
4. **Chat attention:** approve the newest-result-visible seen rule, preserving the existing one-second dwell, so focusing a composer under old messages does not clear Ready? Recommendation: yes.
5. **Recovery and activity:** approve explicit Start new conversation after resume failure, in place of today's automatic fresh fallback, and a static Working line with only Shika-owned wait/decision labels? Recommendation: yes. Stop generating interrupts the turn; Stop Lead retains its existing confirmation.
6. **Meaning of command-free conversation:** approve hiding all machine/tool records while instructing the Lead to avoid narrating commands, with ordinary assistant markdown/code still allowed when useful to the author? Recommendation: yes; a hard guarantee that generated prose never contains a command conflicts with unrestricted assistant text and code blocks. Attachments, voice input, and emoji reactions are possible later decisions only, outside this proposal.

## Evidence for this design change

Installed help/version inspection and primary documentation/package-reference review only. No authenticated provider conversation, transport prototype, Rust change, or native GUI acceptance was performed. Documentation validation and the commit are reported separately when this proposal is delivered.
