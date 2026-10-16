# Lead chat

Status: reviewed design, revised to the author's decisions on 2026-10-16. Design phase only; no transport or UI described here is implemented. This is a UI improvement for the Lead, preserving today's behavior. The current Lead lifecycle remains specified by [lead-agent.md](lead-agent.md) and [shika-cli.md](shika-cli.md). The visual contract and light/dark plates are in [DESIGN.md](../design/DESIGN.md#lead-chat-proposal).

## Intent and boundary

Talking to the Lead should feel like messaging a coworker: your messages on the right, one final response per turn on the left, a quiet composer below. Thinking, tool calls/output and intermediate assistant text stay hidden. The final response may contain bash or code blocks, rendered as markdown. The Lead still runs the installed CLI using the author's existing login and plan. Shika starts that executable and exchanges structured records over local pipes. It does not call a model API, extract credentials, embed a provider SDK, or put a terminal behind the chat view.

Only the Lead changes. Workers keep their real PTYs, terminal tabs, activity detection, reports, diffs, notifications, and safe publishing/Close flows. Keep one Lead per project, its verified fixed detached worktree, no preparation, no editing by the Lead, the four-worker limit, project scope, ownership checks, and the existing control socket. The Lead still manages through the absolute `shika` command supplied in its instructions. Stop leaves workers as ordinary author-owned cards.

The author has decided the narrow Lead-only UI/history exception to [CONTRIBUTING.md](../CONTRIBUTING.md#product-principles). Workers and closed tasks get no transcript browser. Resume, fresh-start fallback/toasts, Seen, Stop/Forget, memory, worker management and author-confirmed dialogs keep their current behavior. No Lead guide or prompt-content change is part of this improvement. Update conflicting product clauses when implementing the reviewed design; this documentation change does not alter the running app.

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

Common arguments: `claude -p --input-format stream-json --output-format stream-json --verbose --dangerously-skip-permissions --append-system-prompt <instructions>`.

- Fresh: generate a v4 UUID and add `--session-id <uuid>`. Start in the verified Lead worktree with stdin left open. Send the first author message as one JSON user record when the author presses Send, not as a bootstrap turn. Further messages use that same pipe, one turn at a time. A user record has `type: "user"` and `message: {role: "user", content: <text>}`; confirm the full envelope against a live fixture before implementation.
- Instructions: the unchanged Lead guide plus project memory, delivered through the checked append flag as today. Keep Claude's current default prompt-snapshot behavior; chat adds no override or command-narration instruction. Verify compaction and resume without changing what the Lead is told.
- After Stop/relaunch/interrupt: use `--resume <saved-id>` instead of the fresh session flag, with all common arguments again. Subsequent turns stay in the resumed process.
- Identity: compare `session_id` from `system/init` and subsequent records with the requested UUID; save the confirmed id in `projects.json`'s `lead.sessionId`. A preallocated id is provisional until confirmed. Never adopt a different session opportunistically.
- Final response: buffer top-level assistant messages privately, rejecting messages with a parent tool identity. On the terminal `result`, publish only the final completed assistant response after the last tool call. Earlier assistant messages and any partials never enter chat or its history. Neither a finished text block nor a tool result ends the turn. Verify the final-response/result relationship with fixtures; never append an aggregate containing intermediate text.

Reason: the installed help explicitly advertises realtime streaming input, so a process can retain the conversation and avoid repeated CLI startup. Pipes must drain even while this Lead is off screen. For the first build, Stop generating interrupts the process group and reopens the same conversation only on the next send; do not invent an stdin interrupt record. A future capability-checked native interrupt can preserve the process after its protocol is separately verified.

### Codex

Fresh launch shape: `codex exec --json --dangerously-bypass-approvals-and-sandbox -c <instructions-override> -c <trust-override> -`. Resume shape: `codex exec resume --json --dangerously-bypass-approvals-and-sandbox -c <instructions-override> -c <trust-override> <saved-id> -`.

- Write exactly one prompt to stdin, then close it. Both help pages explicitly accept `-` to read the prompt from stdin. Set the process cwd to the verified Lead worktree; do not assume an exec-only cwd option is available on resume.
- Instructions: TOML-encode `developer_instructions=<guide plus memory>`, as today's Lead does. The `-c` option is help-verified; that key's behavior is the repository's 2026-10-15 synthetic-server evidence in [lead-agent.md](lead-agent.md#implementation-map), not something help promises. Repeat it on every fresh/resumed exec and reverify it specifically in exec mode. Repeat today's per-process `projects={"<worktree>"={trust_level="trusted"}}` override too. No global config writes.
- Identity: capture `thread_id` from `thread.started`, persist it under `lead.sessionId` with the live Lead identity check, and require it to match on resume. No newest-thread scan or two-second id polling for chat. Always resume the saved UUID, never whichever session ran most recently.
- Events: buffer `agent_message` items privately by item id. On `turn.completed`, publish only the final response, using a verified final phase where supplied or the last completed assistant message after the last tool call. Intermediate commentary remains hidden even though its item type is also `agent_message`. Reasoning, command execution and tools stay hidden. `turn.failed` or `error` settles with an app error, without exposing the candidate text.

Reason: exec advertises one prompt and a finite JSONL output, not a streaming input conversation. A successful child exit after a terminal turn event is normal and leaves the logical Lead Ready, not resting. The next send runs exec resume. A clean exit without a terminal event is an incomplete turn, never success. A persistent Codex server would be another possible CLI integration, but is not the transport selected by this proposal.

### Cursor CLI

Common turn arguments: `agent -p --output-format stream-json --yolo --trust --sandbox disabled --resume <saved-id> <prompt>`.

- Fresh: run the checked `agent create-chat` command in the Lead worktree, validate the returned id, then save it in `lead.sessionId`. Keep today's fallback if allocation fails: launch with the existing bootstrap and no id. Do not replace that behavior with a new startup refusal.
- Instructions: installed help has no system/developer prompt option. Keep today's unchanged bootstrap as a separate first submission for a fresh Lead, with its existing memory instruction and activity accounting; its input is not an author bubble. A resumed Lead gets no bootstrap, and subsequent author/doorbell turns get no repeated preamble. Chat changes neither prompt content nor instruction priority.
- Resume: the same common turn arguments and exact saved chat id for every send. In the fresh fallback without an id, omit the resume option until that child reports its id. No keep-alive process: help exposes streaming output but no stream-input option.
- Identity: require `session_id` from `system/init`/subsequent records to match the saved/allocated id. If today's create-chat fallback left no id, bind the structured id only to this fresh live Lead with the same generation checks. Do not use a previous/latest-chat selector. Cursor has no dependable local session-existence reader in current Shika.
- Final response: buffer complete top-level assistant messages, discarding candidates followed by a tool call or later assistant message. Publish the final candidate only after a successful terminal `result`. Cursor's `result` text can concatenate intermediate assistant messages, so it must not be used blindly as the visible final response. No partial-output option is needed. A missing result or nonzero exit is failure, not permission to expose buffered commentary.

Reason: one child per turn fits the advertised print mode. It must preserve the existing fresh-only bootstrap and saved conversation across invocations. Keep the Lead's existing authority checks in Shika regardless of what it says.

### Pi

Launch shape: `pi --mode rpc --approve --append-system-prompt <instructions> --session-id <uuid>`.

- Fresh: generate an exact UUID. Launch with the guide plus memory as appended system instructions. Before sending a prompt, issue RPC `get_state` with a unique request id and verify `data.sessionId`. RPC emits no JSON-mode session header. Save the confirmed id in `lead.sessionId`; a never-used id may still have no durable session file.
- Turns: one RPC `prompt` record containing the new message, then wait for its correlated acceptance response and session events. Do not queue prompts or use steering/follow-up queues in the first chat version. A handled prompt that starts no run must settle without waiting forever for a run-end event.
- Resume: keep today's exact-id session-existence check and automatic fresh fallback, then start the same launch shape with the selected id and unchanged instructions. Pi's help explicitly says `--session-id` creates a missing id, so this flag alone is not evidence of successful resume.
- Final response: buffer authoritative assistant `message_end` records privately. Publish only the final completed text response after the last tool call when `agent_settled` confirms completion without abort/error. `message_update`, thinking, tool-call blocks and intermediate assistant messages never enter the UI/history. `agent_end` can precede automatic recovery and is not final. RPC command acceptance is not turn completion.
- Interrupt: send RPC `abort`, wait for settlement, keep the process if healthy. No queued inputs means abort cannot unexpectedly start a queued turn. On a stuck abort, terminate/reap the process group and recover on next send.

Reason: RPC has bidirectional input, correlation, cancellation, and explicit settlement. `--mode json` is also help-verified but is a finite event stream for supplied prompts; it is not selected. The implementation should use original Rust protocol code, not embed Pi's TypeScript SDK. Installed package docs, rather than guessed CLI flags, define these RPC verbs.

## Process and message ownership

Separate a **logical live Lead** from a child process and from a turn. It remains live between Codex/Cursor children and therefore retains the token, worker ledger, worker ownership, and doorbell. Stop revokes that logical identity and ends any child; it does not merely wait for a per-turn process to exit. Quit stops all Lead children and retains saved records/trees; the next launch shows a resting row. Do not warm up a CLI with a synthetic author prompt.

Core owns process groups, pipe readers/writers, cancellation, and session persistence. The app owns messages, focus, attention, dialogs, and notifications. Use the existing login-shell PATH discovery and child environment cleanup, fresh `SHIKA_SOCKET`/`SHIKA_TOKEN`, and stable absolute command path. Never inherit worker report credentials as a Lead credential. Keep the fixed-worktree identity checks and operations-lock commit recheck from the current Lead implementation.

Each adapter emits a small common vocabulary: conversation confirmed, input accepted/rejected, final response, turn settled, structured blocker, recoverable error, and transport exit. Assistant candidates stay inside the adapter until finality is verified. Every event is fenced by project, logical Lead generation, process generation, and turn id. Late events from a stopped/replaced child cannot update a card, save an id, or append to a different conversation. Deduplicate on provider message/block/item identity, not matching text; identical sentences can be legitimate separate messages.

There is one active turn. Send becomes Stop while Working/Asking; the author may draft the next message, but Enter does not send or queue it until the current turn settles. Preserve drafts per project in memory while switching cards, not on disk in this proposal. IME composition consumes Enter/Escape before chat actions. A failed input write is a failed delivery, not permission to resend automatically. Once any bytes may have reached a CLI, retry is an explicit author action because workers may already have started.

## Visible conversation

For each turn, the conversation shows the author's submitted message and **one Lead bubble containing only that turn's final response**. Shika-generated turns, such as the existing doorbell or fresh Cursor bootstrap, do not create a fake author bubble; their final response follows the same filter. Small Shika event/error rows remain app-owned metadata. No per-message avatars or names. Render final responses with the markdown subset in [DESIGN.md](../design/DESIGN.md#lead-chat-proposal).

Buffer candidate assistant messages privately until the adapter has both the final completed message and the terminal turn event. Prefer a verified explicit final-response marker where supplied. Otherwise select the last completed top-level assistant response after the last tool call. A later tool call or assistant message invalidates the earlier candidate. Publish the selected final response once, as a single bubble; its paragraphs and text blocks belong inside it. Never stream a candidate into chat and later retract it. If the protocol cannot establish finality, surface a transport error and keep the candidate hidden. Interrupted, failed or incomplete turns show an app error/event, without partial assistant text. A successfully completed turn with no final text creates no empty assistant bubble.

Thinking, tool calls, tool output and **all intermediate assistant text between tool calls** are neither displayed nor stored in visible history. The same applies to usage events, system prompts, bootstrap inputs, session ids and CLI chrome. Unknown event types are ignored, not printed as JSON or treated as text. No expandable tool cards, raw-log drawer, or hidden terminal UI. This is a filter on message provenance and turn finality, not on the wording of the final response: bash, commands and code blocks in a final response are allowed and rendered as markdown code blocks.

Use a static **"Working..."** line at the Lead edge while a turn runs. Do not infer labels such as "Checking on workers..." by parsing shell command strings. A known outstanding Shika `wait` may say **"Waiting for workers..."**, and an author dialog says **"Waiting for your decision"**, derived from Shika's own control state. These are temporary activity labels, not extra card statuses. No pulsing dots or new animation loop. The normal small working pixels remain on the column card.

Keep the Lead's current guide and prompts unchanged. Do not add instructions to avoid narrating commands or to shape a chat response. The UI alone decides which structured message is visible.

## Worker events and the doorbell

Keep the ledger's `(worker, status, turn, report receipt)` identity, batching and ownership from [shika-cli.md](shika-cli.md#the-doorbell). Replace Lead terminal paste/delayed Enter with one internal submission through the selected adapter. The content still identifies itself as `[shika] Workers changed: ... Run shika wait.` so the existing guide understands it. It is never labelled as the author's words.

- Wait for the existing 1.5 second batch, no pending/in-delivery `wait`, no active/blocked turn, no unprocessed input, no composer draft, and 3 seconds since author typing. Replace "live PTY" with "live logical Lead with a usable transport or a per-turn child ready to start". A resting Lead is never automatically resumed.
- Show a centered, muted event row when delivery starts: **"Fix login redirect is ready to check"**, **"Fix login redirect is asking you"**, or **"Fix login redirect exited"**. Use task title, with CLI only to disambiguate duplicate titles. Ready means its turn ended, not verified success, so avoid "finished" as a claim of success. A late report updates the same worker-turn row with **"Report updated"**; it does not duplicate the whole result.
- A batch may show several short rows together. Rows are keyed internally by Shika-owned event ids; no ids or worker report contents are painted. The Lead's own reply explains outcomes and decisions. Worker rows do not steal focus or scroll a reader away from older messages.
- An accepted internal submission starts a normal Lead turn and notification budget. It is persisted as origin `shika_event`, with delivery state, never as an author bubble. Mark rung only after transport acceptance (or a successful write where no explicit ack exists). A bell does not mark reported; a delivered `wait` reply still does. Keep disconnected-wait handling and identical-report idempotency.
- Keep today's delivery-failure accounting: a failed send unmarks the bell so it can ring again; a delivered bell does not mark the event reported. Event ids remain internal bookkeeping and add no new wording to the Lead's input. Preserve existing restart accounting rather than replaying completed bells or author messages from chat history.

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

Keep today's Seen rule unchanged: the Ready result is seen when the selected Lead's content has focus (the chat composer/transcript replaces terminal focus), or when its card stays selected for one second in the active window. Scroll position and newest-result visibility are not conditions. Passing over the card with `j` does not count. Resting Leads retain no live signal.

## Decisions, interruption, and failures

`shika pr` and `shika close` remain the author's existing dialogs. A chat message, a quoted "yes", or a provider approval event can never confirm either. Keep ownership, stale preview, worker status/draft checks, and all Git safety checks. While the request blocks, the Lead is Asking and chat shows "Waiting for your decision". The worker card is selected for the dialog as today; closing/cancelling restores the Lead conversation, its scroll anchor, draft, and composer focus. The result goes back to the CLI's blocked shell tool; it is not echoed as raw tool output. If the Lead was stopped meanwhile, answer/disconnect through the existing flow and do not recreate it or reclaim focus. Direct clean Close remains direct.

Preserve today's question/approval behavior. Pi RPC dialog methods with verified schemas can map the same question/options into the existing native dialog shell and return the author's correlated response, keeping Asking until resolved; notification/widget/status requests are not blocking. Do not automatically answer or cancel an author question merely because chat cannot render it. Claude/Codex/Cursor print modes may deny or fail interactive-only tools rather than emit a usable question: required interaction parity is an implementation gate for that adapter, not an approved behavior change. An unrecognized protocol interaction surfaces **"This Lead needs an interaction chat does not support."** and blocks adapter acceptance until handled. Do not infer a blocker from assistant prose or invent a response protocol. Tool permission mode remains today's automatic approval mode, without new Shika tool-approval prompts.

| Situation | Chat behavior |
| --- | --- |
| Stop generating / Escape with chat focused | Chat equivalent of today's Escape interrupt: interrupt this turn only. Show "Stopping..." until abort settles or the owned process group is reaped, then "Reply stopped". Discard unpublished assistant candidates; no partial response is shown or saved. Workers already started keep running. Next send uses the same saved conversation and existing recovery behavior. Escape dismisses an overlay/IME first. |
| Claude, Codex, Cursor interrupt | Send SIGINT to the owned group, continue draining, then bounded termination if necessary. Reap before another child starts. CLI persistence after interruption must be tested; no immediate resubmit and no raw Escape byte into a pipe. |
| Pi interrupt | RPC abort, then the same bounded termination fallback. Do not end at its acceptance ack alone. |
| Authentication, rate limit, network/provider error | Plain app-owned error row, e.g. "Codex could not sign in. Sign in with Codex, then retry." Keep the author's bubble, with no unpublished candidate/partial reply. Retry sends only on an explicit action and warns that work may already have started. No CLI credential prompts inside chat. |
| Unexpected long-lived CLI exit | Settle the active turn as failed, then rest the Lead with transcript visible and "The Lead stopped unexpectedly" plus Resume. Keep id and worktree. An idle crash creates no fake completed turn notification. |
| Expected Codex/Cursor exit | After a valid final event, stay logically live and Ready. Missing final event is "The reply ended before it was complete" even on exit code zero. |
| Expired, missing, or refused resume | Keep today's automatic fresh-start fallback and toast, with the existing safeguards described below. No new Start new conversation button or confirmation. Visible history stays with the Lead's lifetime; a context boundary may identify an automatic fresh conversation without replaying old messages. |
| CLI missing or disabled | Keep today's resume routing: open the existing Lead picker for that project. Choosing another CLI starts fresh as today, without a new confirmation step. History stays readable and is never forwarded to the other CLI. Do not install a CLI. |
| Malformed stream / unknown required schema | Stop accepting text, settle with "This CLI version could not be read" and the version. Drain/terminate boundedly. Unknown optional events may be ignored; an unrecognizable required lifecycle event cannot become success. |
| Transcript write failure | Keep existing data, stop new sends, show "Could not save this conversation" and Retry save. Never silently turn persistence off or replay a prompt. |

Errors are facts in muted event rows with neutral actions, not fake assistant bubbles or red status dots. Extract known error categories; never dump stderr, JSON, commands, or tokens into the transcript. Routine diagnostics remain metadata-only. Do not kill descendants belonging to worker cards when interrupting a Lead; they are app-owned processes, not its shell-tool group. Stop Lead uses the existing confirmation and worker reassignment; Stop generating does neither.

**Resume recovery is today's lifecycle, not a new chat choice.** Keep `Core`'s existing saved-id/session-existence decision before launch. If a resumed CLI exits within the existing five-second grace, ask `saved_lead_session_exists` off-thread. Known missing: clear the id and start that CLI fresh once, with the existing toast **"Could not resume the Lead's conversation. Started a new one."** If the session exists or its existence is unknown (including Cursor), keep the id, rest the Lead and show today's toast **"The Lead's CLI exited right after resuming. Select it to try again, or Forget it to start a new conversation."** Preserve the existing conditional selection/focus restoration on automatic restart. Normal successful Codex/Cursor per-turn exits are not resume refusals. A mismatched structured id is an adapter error, not evidence to overwrite the saved id or erase history.

## Persistence decision

The CLI's conversation is model context. The visible transcript is the author's record. They need separate identities and lifecycles; replaying saved bubbles into a resumed CLI would duplicate context and actions.

| Option | Benefits | Costs and limitations |
| --- | --- | --- |
| Read each CLI's own files on open | Avoids a second copy of visible content; can recover pre-chat messages where available | Four private layouts, compaction/branching, tool and thinking filtering, missing/pruned/encrypted files, and no dependable Cursor reader. Slow history reads and provider upgrades can erase or reorder what the author saw. Does not reliably meet cross-provider relaunch scrollback. |
| Shika-owned visible transcript per project (chosen) | Same scrollback across all four CLIs; exact role/event provenance, interruption markers, and stable ordering; no provider-file parser for normal chat | New local content storage and crash recovery responsibilities; old terminal history is not imported; it does not restore provider context. The author approved this Lead-only UI/history exception. |

**Decided: Shika-owned visible history for the Lead's lifetime, with no age or size limit.** Proposed location: `<data dir>/leads/<project-id>/chat/`, separate from `memory.json`, with a schema version, Lead-lifetime identity, CLI/conversation-id segments, and append-only records plus a rebuildable index. Extend the saved `lead` record in `projects.json` with an optional history/transport discriminator; preserve existing `cli`, `sessionId`, and restart accounting. A provider fresh fallback or CLI change begins a new context segment without deleting lifetime history. Extend rather than replay that history. No message bodies in `projects.json` or repository files. Use private directories/files, off-thread writes, serialized appends, and atomic metadata replacement; no account, sync, or cloud storage from Shika.

Store author text once, each turn's final response only, visible Shika events/errors, local timestamps, message/turn ids, context-segment identity, delivery states, and completion/interrupted markers. Exclude thinking, intermediate assistant text, tool payloads, stderr and credentials. Candidate buffers are transient; do not checkpoint partial assistant text to visible history. Commit final responses durably after verified completion. Persist the outgoing record before dispatch and record acceptance separately. After a crash, an unconfirmed outgoing record is **"Delivery unknown"**, never automatically resent; an unfinished turn has **"Reply interrupted by restart"** with no partial response. No cross-file exactly-once claim: reconcile the transcript header and saved Lead binding before continuing.

Loading history never starts a CLI. Load the newest page first; fetch older pages on scroll, with a stable anchor. A failed or corrupt transcript is preserved and reported, not overwritten as empty. Restore reading position in memory between cards; on app restart open at the newest saved message. Reloading history does not replay old notifications or mark an old turn Working.

**Lifetime and deletion:** Stop and quit keep all visible history. Forget deletes the Lead's Shika-owned history together with its saved conversation record/link through today's existing Forget flow; the next Lead starts a fresh conversation and an empty chat. Preserve existing worktree checks, destructive confirmation and failure handling. The dialog's facts should include **"Its Shika chat history will be deleted. The CLI keeps its own history. Its memory notes stay for the next Lead."** Shika still does not delete provider-owned session files. Project removal deletes chat with the project's other app data, keeping today's worktree and memory-removal rules. Automatic fresh fallback and a CLI change do not silently discard lifetime history. There is no age expiry, size cap, automatic pruning or truncation. Bound in-memory pages and rendering work rather than retained history.

`shika memory`, `remember` and `forget` keep today's behavior. Notes remain in `memory.json`, survive Stop, app restart, Forget Lead and a CLI change, and are supplied to future Leads as today. Forgetting the Lead must not delete those notes. Project removal retains today's separate deletion behavior.

## Migration from a terminal Lead

An existing saved Lead keeps its CLI and exact saved conversation id. It comes back resting and resumes through today's entry points, in chat mode. Only Shika's visible history starts empty: no provider-history import, migration confirmation, synthetic recap, new bootstrap or replacement session just because the UI changed. The CLI still has its existing conversation context. Nothing is submitted just to populate the view, and no new migration notice is required.

Initialize the new history binding without clearing the saved id. Use the unchanged instructions and provider-specific fresh/resume behavior above; in particular, a resumed Cursor Lead gets no bootstrap. A legacy Lead without a usable id follows today's fresh-start path. Refused/missing resume follows today's automatic recovery/toasts, not a new decision dialog. Stop, Forget, CLI availability and picker routing remain the same. A running terminal Lead at an app upgrade is stopped through the normal app restart before the chat UI resumes it.

## Implementation map and build order

Names below are proposed modules, not files created by this change. Read current symbol maps in [lead-agent.md](lead-agent.md#implementation-map), [shika-cli.md](shika-cli.md#code-map), and [agent-activity.md](agent-activity.md#code-map).

| Area | Planned work |
| --- | --- |
| `shika-core/src/lead_transport/` (new) | Original Rust child/pipe supervisor, per-CLI args and structured adapters, common events, process generations, cancel/reap and bounded framing. No GPUI. |
| Core `lead.rs`, `agents.rs`, `session.rs`, `lib.rs`, `path_env.rs` | Separate logical Lead lifetime from a PTY/per-turn child; reuse worktree/env/discovery invariants. Chat-specific argument builder instead of changing worker launch flags. |
| Core `lead_chat_store.rs` (new), `projects.rs` | Visible transcript/index, binding and crash reconciliation, optional backward-compatible saved fields; generation-checked id commits. `memory.rs` stays independent. |
| App `lead_chat.rs`, `lead_chat_markdown.rs` (new) | Virtualized conversation, composer, selection/copy, light markdown, scroll anchors, unread affordance, errors and native accessibility. |
| App `main.rs`, `model.rs`, `activity.rs`, `lifecycle.rs`, `notifications.rs` | Chat routing/focus and logical Lead state reducer, turn clock/seen integration. Exclude chat from terminal screen detection and host naming capture; keep worker behavior. |
| App `control.rs`; existing `lead_guide.md` unchanged | Doorbell/internal submission path, existing ledgers/dialog return focus. Keep socket/token/worker report semantics and the exact current guide/prompt content. No new control command is required for basic chat. |
| App `appearance.rs` | Reuse existing chrome fields; any additional field needs Shika Light/Dark/glass values and a derivation, approved before code. |
| `shika-terminal` | No chat rendering or transport; worker terminals remain intact. Audit callers that currently assume every live card owns a terminal. |

1. **Use the author's reviewed decisions.** This is a Lead UI improvement with today's lifecycle, prompts and Seen behavior. Update conflicting product clauses during implementation without reopening the decided history or geometry choices.
2. **Claude Code first.** Capture structured fixtures and real fresh/two-turn/resume/compaction/interrupt/error behavior in disposable repos and isolated data. Build the common transport/store, state reducer, and chat view. Check worker launch/report/doorbell and author dialogs end to end. Do not ship the other chat choices until their adapters pass.
3. **Codex.** Validate exec-specific developer instructions and exact thread resume. Test normal child exit versus failed/incomplete turn, duplicate item snapshots, and process cancellation without affecting workers.
4. **Cursor CLI.** Validate create-chat/resume durability and today's allocation fallback/fresh-only bootstrap. Prove that only the final assistant message appears even when the terminal result aggregates intermediate text. Check missing id/login failures and context recall across print invocations on the installed release.
5. **Pi.** Validate RPC acceptance versus settlement, missing exact ids, compaction/retry, abort, and supported/cancelled extension dialogs. No quiet fallback for chat.
6. **Integrated acceptance.** All catalog themes, light/dark, solid/sidebar-only/full frost, Reduce transparency/motion, 960x600 and hidden column, long text/code/links, IME, keyboard-only, screen reader and text copy. Verify today's Seen rule even when scrolled up, exact saved-id migration with empty visible history, automatic fresh fallback/toasts, unlimited lifetime retention, and Forget deleting chat while preserving memory. Stress long paged transcripts and hidden candidate processing while a worker terminal is visible. Record actual outcomes in `MANUAL_CHECKS.md`; this design establishes none of those passes.

## Risks and verification boundaries

- Structured mode entry points are documented, but event formats and their version compatibility are not a stable public API contract Shika can assume indefinitely. Help verifies flags only. Keep per-version fixtures, tolerate unknown optional fields, and fail visibly on missing lifecycle/identity evidence. Never fall back to printing raw records or scraping a hidden Lead terminal.
- CLI session persistence can fail independently of Shika's visible history. Verify real conversation recall after Stop/relaunch for each provider, not just equal ids. Preserve today's automatic recovery/toasts. Cursor's fresh-only instruction channel and Pi's create-if-missing semantics are specific risks. Assistant-message completion alone does not establish turn finality; fixtures must include commentary, multiple tools, retries, compaction, empty finals and interrupted/failed turns. Cursor's aggregate result must never leak intermediate text.
- The selected non-interactive modes do not all support arbitrary questions/permissions. Do not claim universal Asking coverage. Prove supported interactions per CLI, fail unsupported ones plainly, and preserve author confirmation exclusively in Shika's PR/Close dialogs.
- Implement the markdown subset and interaction in Shika's own code, optionally using a compatible MIT/Apache parser after license review. Never copy Zed's GPL markdown, editor, terminal or terminal-view crates. No web view, executable HTML, remote image fetching or terminal escape interpretation.
- Lifetime history has no age or size limit. Use a virtualized, paged variable-height list with stable message ids, cached text shaping/layout and bounded off-screen state. Parse and index off-thread; process candidates privately and repaint conversation content only when a final response is published. No entity per historical paragraph or whole-history loading. Tall final responses must render visible blocks lazily without truncating stored content.
- Final-only filtering must happen before both display and persistence; hiding intermediate text after writing it to history is insufficient. Commands and code in the final response are valid content. Keep the Lead guide/prompts unchanged. Every chat color must derive from active theme chrome/palette across all catalog themes, light/dark and translucency; geometry approval is not permission for hard-coded colors.
- Interrupting a shell tool can leave external work already completed. Keep worker records authoritative; never treat a stopped reply as rollback or automatically replay a message, bell, publishing request or close request.

## Author's decisions

- This is a UI improvement for the Lead. Keep today's lifecycle, instructions, worker behavior, automatic fresh-start fallback/toasts, Seen rule and author-confirmed PR/Close dialogs.
- Per turn, show only the author's message and the Lead's final response. Hide thinking, tools/output and intermediate assistant messages. Bash/code in the final response renders as markdown; no prompt change to suppress commands.
- Shika owns visible history for the Lead's lifetime, with no age or size limit. Stop/quit keep it; Forget deletes it with the saved conversation link so a new Lead starts empty. Memory notes retain today's behavior and survive Forget.
- An existing saved Lead resumes its saved conversation id in chat. Only visible history starts empty, without import or a migration confirmation/notice.
- Every chat color derives from the active theme chrome/palette for every catalog theme, light/dark and translucency. No hard-coded chat colors.

## Approved design values

The author approved these geometry values. They no longer require a design-token question before implementation:

| Value | Purpose |
| --- | --- |
| Bubble radius **22px** | Fully rounded for a single line: 20px line height plus 12px top/bottom padding makes a 44px pill. Both roles share it. |
| Composer radius **24px** | Fully rounded at the existing 48px minimum height; keep the radius when it grows. |
| Bubble width cap **80% of the lane** | Preserve side alignment at narrow widths; also cap at the existing 480px size. |
| Composer growth **six text lines**, capped at **144px total** | Existing 20px line height plus 12px top/bottom padding, then internal scrolling. |
| Time separators **five minutes** | Centered local-time separator when the gap reaches this threshold; always separate a new local day. |

No new color, font size, shadow or animation values. Lane width 640, insets 20, gaps 6/12/20, avatar 24/radius 6, header 48, and type 14/20 and 11.5 reuse the existing design system. The design remains documentation only in this change.

## Open questions for the author

None pending. The decisions above are settled. Provider-specific final-response/lifecycle compatibility still needs implementation evidence and fixtures; it is not a request to change Lead behavior. Attachments, voice input and emoji reactions remain possible follow-ups outside this design.

## Evidence for this design change

Installed help/version inspection and primary documentation/package-reference review only. No authenticated provider conversation, transport prototype, Rust change, or native GUI acceptance was performed. Documentation validation and the commit are reported separately when this proposal is delivered.
