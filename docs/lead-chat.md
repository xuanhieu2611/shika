# Lead chat

A Claude Code Lead is a native chat instead of a terminal. The author's messages sit on the right, one final response per turn sits on the left, and a composer sits below. Everything else about the Lead is unchanged: the same installed CLI and login, the same fixed worktree, the same instructions, the same `shika` command, workers, doorbell, resume, Stop and Forget.

Status: implemented for Claude Code on 2026-10-16 (commits `c1f03a0` and `63f98f7`, designed in `0785b3c` and `a100820`). Codex, Cursor CLI and Pi Leads keep their terminals; their adapters are planned in [Extending to other CLIs](#extending-to-other-clis). Native acceptance beyond an isolated smoke run is still open in [MANUAL_CHECKS.md](../MANUAL_CHECKS.md#claude-code-lead-chat-2026-10-16).

Start here when contributing to this feature. This guide records the decisions and explains how the code works and how to debug and extend it. The Lead itself (lifecycle, worktree, resume, memory, workers) is specified in [lead-agent.md](lead-agent.md); the `shika` command and doorbell in [shika-cli.md](shika-cli.md); the visual contract in [DESIGN.md](../design/DESIGN.md#lead-chat) (its SVG light/dark plates are outdated; the text is the contract).

## Reading paths

- **New to the team:** read the [overview](#two-minute-overview), [evolution](#how-the-feature-evolved), and [decisions](#why-this-design), then [one turn end to end](#one-turn-end-to-end) and the [code map](#code-map).
- **Debugging:** start with [debugging by symptom](#debugging-by-symptom). Reproduce with [isolated data and a fake CLI](#isolated-reproduction), never normal app data or a personal repository.
- **Adding a CLI:** read [extending to other CLIs](#extending-to-other-clis) and the [guardrails](#contributor-guardrails) first. The current transport is Claude-specific in more places than the module name suggests.
- **Changing visuals:** read [DESIGN.md](../design/DESIGN.md#lead-chat) and the [approved design values](#approved-design-values). Every chat color comes from the active theme.

## Two-minute overview

- **Before:** every Lead was a CLI in a PTY, drawn by the terminal view. The author read thinking, tool calls, tool output and narration in the CLI's own UI. The doorbell was a bracketed paste plus a delayed Enter.
- **Now:** a Claude Code Lead runs as one long-lived `claude -p` child speaking bidirectional stream JSON over pipes. It has no PTY (`Session::pty` is `None`). Shika shows only the author's text and each turn's final response, rendered as light markdown.
- **Hidden:** thinking, tool calls, tool output, subagent text and every intermediate assistant message. They are dropped before anything is written to history.
- **History:** Shika keeps a visible transcript for the Lead's lifetime in `leads/<project-id>/chat/history.sqlite`. Stop and quit keep it, Forget deletes it, memory notes survive. It is never replayed into the CLI.
- **Unchanged:** the Lead guide and prompts, the worktree, the four-worker limit, ownership checks, the doorbell's gates and accounting, author-confirmed PR/Close dialogs, resume and the fresh-start fallback, Seen and notifications. Workers keep real terminals.
- **Start reading code:** `Core::start_claude_lead` (`crates/shika-core/src/lead.rs`), `ChatTransport` and `ClaudeStream::parse` (`lead_transport.rs`), `ChatStore` (`lead_chat_store.rs`), then `Shika::drain_lead_chats` (`crates/shika/src/lead_chat.rs`).

## How the feature evolved

| Stage | What changed | Why the next change was needed |
| --- | --- | --- |
| Terminal Lead | The Lead was an ordinary CLI terminal in its fixed worktree, steered by the author and by `[shika]` lines pasted into its PTY. Shika stored no transcript. | A manager conversation is mostly reading results. In a terminal the author scrolls past thinking, tool calls, command output and narration to find the answer, and the doorbell looks like author input. |
| Chat proposal (`0785b3c`) | A design for a native chat over each CLI's structured mode, with per-CLI transports, final-only rendering, and a persistence comparison. | Several product clauses (no transcript, no chat client) conflicted, and history lifetime, migration, and theming needed the author's decision. |
| Author decisions (`a100820`) | UI-only change; final response only; Shika-owned lifetime history; Forget deletes it; memory survives; existing saved Leads resume in chat with empty history; all colors from the theme; approved geometry. | Claude Code had the strongest evidence (realtime streaming input and a native question channel), so it went first. |
| Claude Code chat (`c1f03a0`, `63f98f7`) | Pipe supervisor, final-response filter, SQLite history, native question dialog, chat view, composer and markdown renderer. Fixtures recorded from the installed CLI against a local synthetic server. Product clauses in AGENTS.md and CONTRIBUTING.md narrowed to this exception. | Other CLIs need their own transport evidence before they switch; until then they keep terminals. |

## Why this design

Each decision below is the author's. Do not relitigate them in a change; a change in direction comes from the author, and this guide changes with the code.

**A UI change only; Lead behavior stays.** The Lead's value is its management loop, which already worked. Changing how the author reads it should not change what the Lead is told, when it runs, or what it may do. So the guide (`crates/shika/src/lead_guide.md`) and memory notes travel through the same `--append-system-prompt` flag, the worktree and control environment come from the same `start_lead_inner`, and resume, fresh fallback, toasts, Seen, Stop/Forget and the doorbell keep their rules. No prompt was added to make replies "chat friendly" or to stop the Lead narrating commands.

**Only the author's message and the turn's final response are shown.** The author wants the answer, not the process. One bubble per turn keeps the transcript readable and makes history small. The final response may contain bash or code blocks; that is content, rendered as code, not filtered by wording. The filter is about provenance and finality, never about what the text says.

**Thinking, tool calls, tool output and intermediate assistant text are hidden, and never stored.** Hiding after storage would still leak into history and into later features. So the decoder keeps a private candidate, invalidates it on any later tool call or tool result, and releases it only on a successful terminal `result`. There is no expandable tool card, raw log drawer or hidden terminal to fall back on. If finality cannot be established the turn fails visibly and the candidate is dropped.

**Shika owns the visible history for the Lead's lifetime.** The CLI's conversation is model context; the transcript is the author's record. They have separate identities and lifetimes. Reading each CLI's private files would mean four undocumented layouts, compaction and pruning, tool/thinking filtering, and no dependable Cursor reader, and a provider upgrade could reorder or erase what the author saw. A Shika-owned store gives the same scrollback across CLIs with exact provenance. The rejected and chosen options are compared in [the persistence decision](#the-persistence-decision). Lifetime means: Stop and quit keep it; Forget deletes it together with the saved conversation link, so the next Lead starts empty; memory notes (`memory.json`) are separate and survive Forget, as before. No age or size limit; rendering is bounded instead.

**Every color derives from the active theme.** The chat sits where the terminal sits and must work in every catalog theme, light and dark, solid and frosted. Geometry was approved; colors were not, so there are no chat hex values. `LeadChat::render` reads `Chrome` fields and applies `appearance::legible` at 4.5:1 against the real surfaces. See [DESIGN.md](../design/DESIGN.md#lead-chat).

**Claude Code first.** Its installed help advertises `--input-format stream-json` (realtime streaming input), so one process can hold the conversation across turns, and its stdio permission channel preserves native `AskUserQuestion` questions in print mode. Codex and Cursor only offer one prompt per process; Pi needs RPC work. Shipping one well-evidenced adapter beats four guessed ones. The gate is per CLI: a preset switches to chat only after its adapter has fixtures and passes the same checks.

**Shika's own markdown renderer, not Zed's.** Zed's markdown, editor, terminal and terminal-view crates are GPL-3.0 and Shika is MIT. The renderer in `lead_chat_markdown.rs` is deliberately small: bold, italic, inline code, fenced code, bullet and check lists, one indent level, headings as bold paragraphs, and validated `http`, `https` and `mailto` links. No HTML, images, remote content, terminal escapes, syntax highlighting, or executable controls. The composer and the selectable bubble text (`chat_input.rs`) follow the pinned GPUI input example's `EntityInputHandler` contract.

**Transport per CLI, chosen from its own help.** Each CLI exposes a different structured mode, so there is no shared wire format to target:

| CLI | Installed version checked | Help checked | Process model |
| --- | --- | --- | --- |
| Claude Code (implemented) | 2.1.302 | `claude --help` | One long-lived child, bidirectional stream JSON |
| Codex (planned) | codex-cli 0.162.1 | `codex exec --help`, `codex exec resume --help` | One child per turn, resume the exact thread |
| Cursor CLI (planned) | 2026.10.14-941492e | `agent --help`, `agent create-chat --help` | One child per turn, resume the exact chat |
| Pi (planned) | 1.1.0 | `pi --help` | One long-lived RPC child |

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
                                           claude -p ... (own process group)
```

- **One actor per live Lead.** `ChatTransport::start` opens the store, recovers an unfinished turn, binds the context, spawns the child, and starts the `lead-chat` thread. All store writes and all pipe IO happen on that thread. The UI only sends commands and drains events, so a Lead off screen still has its pipes drained.
- **The child.** `Process::spawn` runs the resolved `claude` binary in the Lead worktree with its own process group (`process_group(0)`), `PATH` with the control `bin` directory first, `PWD`, `SHIKA_SOCKET` and `SHIKA_TOKEN`. It removes the git redirect variables, the worker report variables (`pty::CONTROL_VARS`, so a Lead never inherits a report credential), and `CLAUDE_CODE_CHILD_SESSION` and `CLAUDECODE`, so a Shika started from inside Claude Code still gets a persisted Lead conversation. stdin is set nonblocking. The first write is the initialize control request.
- **Arguments** (`ChatLaunch::args`): `-p --input-format stream-json --output-format stream-json --verbose --dangerously-skip-permissions --append-system-prompt <guide plus memory> --permission-prompts host --permission-prompt-tool stdio`, then `--session-id <uuid>` for a fresh conversation or `--resume <uuid>`. All are built as an argument array; nothing passes through a shell.
- **Framing.** The reader reads newline-terminated frames up to 8 MiB. A longer or unterminated frame is a `BadFrame`. The frame channel holds eight frames, so a flood blocks the reader, not memory. Every frame and EOF carries the process generation; frames from an older child are ignored.
- **Initialization.** If the `shika-initialize` control response does not arrive within 10 seconds, the turn fails with "This Claude Code version could not be read" and the child is stopped.
- **Usage.** `start_claude_lead` registers the Lead worktree as a Claude usage source when usage collection runs, and the actor pokes it on every event, so Lead tokens reach the Usage dashboard the same way PTY output does for workers. See [token-usage.md](token-usage.md).
- **Quit.** `cx.on_app_quit` calls `Core::stop_lead_chats`, which stops and joins every actor (SIGINT, SIGKILL after 2 seconds). GPUI may keep window state alive until exit, so RAII alone is not enough. Stop Lead goes through `Core::hang_up`, which removes and stops the transport. `ChatTransport` and `Process` also stop and reap on drop.

### One turn end to end

1. **Typing.** The composer is a `ChatInput`. Each change sets `LeadChat::last_typed`, which feeds the doorbell's 3 second typing gate.
2. **Enter.** `ChatInput` emits `Submit` (Shift+Enter inserts a newline; IME composition consumes Enter first). `LeadChat::submit` refuses unless the Lead is live, not busy, not saving, and not in save-failed state. It trims outer blank lines, remembers the exact draft as `pending`, sets `saving`, and emits `ChatAction::Send`.
3. **App dispatch.** The subscription in `Shika::ensure_lead_chat` calls `submit_lead_chat`. It refuses while the card is in an active or blocked turn, calls `Core::lead_chat_send`, and advances the card with `Activity::advance_chat(Working, submitted: true)`, which starts the turn clock.
4. **Outgoing record.** The actor checks there is no active turn, no stop in progress, no save failure, and the same cancel epoch as when Send was queued. It allocates the next turn number and writes the author text, any Shika event rows, and an `Outgoing { origin }` marker in one FULL synchronous transaction. The `Record` events reach the view; the author bubble appears, and the composer is cleared only if its text still equals `pending`.
5. **Spawn if needed.** If the previous child was interrupted, a new one starts here with `--resume` and emits `Resuming`.
6. **Write.** The decoder's per-turn state is reset (`begin`), then `{"type":"user","message":{"role":"user","content":<text>}}` is written with a 3 second deadline, retrying on `WouldBlock` and aborting if the cancel epoch moves. Success records `Accepted` and emits `Accepted` and `Status(Working)`.
7. **Identity.** Claude emits `system/init` with the session id at each turn. It must equal the requested id. The first confirmation emits `Confirmed`; the app calls `Core::confirm_lead_chat` off thread, which saves `lead.sessionId` only for that exact live Lead, under the operations lock.
8. **Decoding.** Assistant and user records update the private candidate (see [final-response extraction](#final-response-extraction)). Questions open the native dialog (see [questions](#questions-and-unknown-interactions)).
9. **Result.** A successful `result` returns the candidate. `Actor::complete` writes the `Lead { text }` record (if there is text) and `Settled` in one transaction, then emits `Record` and `Status(Ready)`.
10. **App.** `drain_lead_chats` maps `Status(Ready)` through `advance_chat`, which finishes the turn and posts the turn's single notification (titled with the project name, "Lead", and the status). The view parses the record off thread into markdown blocks and appends it. Seen follows the normal rule, with chat focus standing in for terminal focus.

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

### Questions and unknown interactions

`--permission-prompts host --permission-prompt-tool stdio` routes Claude's permission requests to stdin/stdout as `control_request` records. With `--dangerously-skip-permissions` the only one expected is `can_use_tool` for `AskUserQuestion`. Without this channel, print mode drops `AskUserQuestion`, so the flags preserve today's question behavior rather than add approvals.

- **Supported:** `can_use_tool` for `AskUserQuestion` during an active turn, with 1 to 4 questions, each with a non-empty question and at most 4 options. The actor records the request id, emits `Status(Asking)` and `Question`. The app opens `Overlay::LeadQuestion` (`lead_chat_question.rs`), or queues it in `Shika::chat_questions` if another overlay is open; a question never replaces an open dialog.
- **Answer:** the dialog collects one answer per question (an option label, a list of labels for multi-select, or free text), then `Core::lead_chat_answer` writes `{"behavior":"allow","updatedInput":<original input with "answers">}`. Cancel writes `{"behavior":"deny","message":"The author cancelled this question."}`. A successful write returns to Working. The question and its tool reply are never stored.
- **Dismissal:** `control_cancel_request`, a settled turn, or an exit closes the dialog and drops queued questions for that Lead.
- **Unknown:** any other `control_request`, or a question outside those limits, emits `Status(Asking)` and the error "This Lead needs an interaction chat does not support." It is never answered automatically. The author can Stop generating.
- **PR and Close:** `shika pr` and `shika close` still open the author's own dialogs through the control socket. While a `LeadDialog` for this Lead is open, `drain_lead_chats` holds the card at Asking and the activity line reads "Waiting for your decision". A chat message can never confirm either.

### Stop generating

Escape in the chat (outside IME and overlays) or the Stop button sends `ChatAction::Interrupt`, which is today's Escape for a terminal Lead, applied to the turn only:

1. `ChatTransport::interrupt` bumps the cancel epoch (so a blocked write aborts at once) and queues `Interrupt`.
2. The actor drops the candidate, sends SIGINT to the child's process group, and emits `Stopping` ("Stopping...").
3. After 2 seconds it sends SIGKILL. Exit is observed after the reader reaches EOF, or after 3 seconds if a descendant still holds the pipe.
4. The turn settles with the event "Reply stopped" and `Settled`; no partial text is shown or saved. The child is dropped and the actor stays.
5. The next Send spawns a fresh child with `--resume` on the same confirmed id.

Shell tools the Lead was running, including a blocking `shika wait` client, normally inherit that process group and stop too (a tool that starts its own group escapes the signal and is reaped by nothing in Shika); the socket disconnect releases the doorbell gate as for any abandoned wait. Workers are separate PTYs owned by Shika and keep running (asserted by `claude_chat_has_no_pty_confirms_identity_and_keeps_history_until_forget`). Stop generating is not Stop Lead: `⌘⇧W` and the header button still open the existing Stop/Forget confirmation.

### Failures and exits

| Situation | What the code does |
| --- | --- |
| Write fails or times out | Event "Delivery unknown. Work may already have started. Send again only when you are ready to retry.", the child is stopped, and the author's text is offered through Retry message. Nothing is resent automatically. |
| Provider error result | Event "Claude Code could not complete the reply. Check its sign-in and connection, then retry." The child is stopped; the next Send resumes. Retry message is offered. |
| Unreadable stream, oversized frame, or no initialize response in 10 seconds | Event "This Claude Code version could not be read" (with the version when known). The transport exits and the Lead rests. |
| Different session id | Event "Claude Code returned a different conversation". The transport exits; the saved id is never overwritten. |
| Child exits during a turn without a result | Event "The reply ended before it was complete", then exit handling below. |
| Child exits while idle | Event "The Lead stopped unexpectedly". |
| Any unexpected exit | `ChatEvent::Exit`; the actor ends. The app marks the host exited. Within 5 seconds of a resumed launch it is a [resume refusal](#conversation-identity-resume-and-the-fresh-start-fallback); otherwise the Lead rests with the toast "The Lead stopped unexpectedly. Select it to resume." and the chat shows "Lead is not running" with Resume. |
| Send while busy or after a save failure | Error "The Lead is still replying or the conversation needs to be saved." |
| History write fails | "Could not save this conversation" with Retry save. Records are kept in memory and appended on retry; sends are refused until then. Quitting before a successful retry loses those unsaved rows. |

Errors are muted event rows or a line above the composer, never fake assistant bubbles. stderr is drained and discarded; no raw JSON, stderr or token is painted.

### Conversation identity, resume and the fresh-start fallback

- **Choosing.** `start_lead_inner` keeps today's `choose_conversation`: resume when the saved record names Claude, has a valid id, and `CliHome::session_exists` finds `<claude config>/projects/*/<id>.jsonl` (`CLAUDE_CONFIG_DIR` is honored); otherwise fresh with a new UUID.
- **Provisional id.** For a fresh chat Lead, `projects.json` saves `sessionId: null` at start. The id is committed only when `system/init` confirms it, via `confirm_lead_chat` (live Lead, Claude preset, transport present, saved CLI is Claude, no different id saved). A Lead stopped before its first message therefore has no id and starts fresh next time.
- **Resume.** A resumed start passes `--resume <id>` immediately. After an interrupt, a respawn on the next Send also uses `--resume` and emits `Resuming`, which resets `LeadRun::resumed_at`.
- **Refusal.** If the transport exits within `model::RESUME_GRACE` (5 seconds) of a resumed launch, `lead_upkeep` asks `saved_lead_session_exists` off thread. Known missing: clear the id, start fresh once, toast "Could not resume the Lead's conversation. Started a new one." Exists or unknown: keep the id, rest, toast "The Lead's CLI exited right after resuming. Select it to try again, or Forget it to start a new conversation." This is the terminal Lead's logic, unchanged.
- **Migration.** A saved terminal Claude Lead keeps its CLI and id and resumes in chat. Its visible history starts empty; nothing is imported, recapped, or submitted to populate the view.
- **CLI change.** If the author picks another CLI, `SavedLead::chat_history` is carried over, so a later Claude Lead reopens the same lifetime history. History is never sent to the other CLI.

### History storage

Location: `<data dir>/leads/<project-id>/chat/history.sqlite`, next to `memory.json` but separate from it. `chat_dir` accepts only `[A-Za-z0-9-]` project ids. `ChatStore::open` refuses symlinked or non-directory `leads`, project and `chat` directories and a symlinked database, creates directories `0700`, and sets the file to `0600`. SQLite uses `journal_mode=DELETE`, `synchronous=FULL`, and a 3 second busy timeout. `user_version` is 1; a newer version is refused with "This conversation uses a newer Shika version".

| Table | Contents |
| --- | --- |
| `metadata` | `lifetime`: a random id created once per store. `context`: the JSON `[cli, conversation]` of the current binding. |
| `records` | Immutable append-only rows: `id`, `turn` (0 for events outside a turn), `timestamp` (local Unix seconds, never provider timestamps), `segment` (context number), `visible`, `content` (JSON `ChatContent`). Indexed by `(visible, id)` and `(turn, id)`. |
| `visible_entries` | The projection pages read: one row per visible entry, `event_key UNIQUE`. A keyed event upserts: the entry keeps its first id and position but points at the newest record. Rebuilt from `records` when the table is missing. |

`ChatContent` kinds: `Author { text }`, `Lead { text }`, `Event { text, key }` are visible; `Outgoing { origin }`, `Accepted`, `Settled`, `Context { cli, conversation }` are bookkeeping. `ChatOrigin` is `author` or `shika_event`.

- **Binding.** `SavedLead::chat_history` in `projects.json` (`chatHistory`) holds the store's `lifetime`. A start or page read whose saved lifetime differs from the store's fails with "Could not read this conversation: history identity changed" rather than showing someone else's transcript. `bind(cli, conversation)` writes a `Context` row when the binding changes, plus the visible event "A new Lead conversation started" if there was an earlier context.
- **Transactions.** Author, events and `Outgoing` commit together before dispatch. `Accepted` commits after the write. Final text and `Settled` commit together. There is no cross-file exactly-once claim between `projects.json` and the store.
- **Crash recovery.** `recover` runs at every transport start. If the last turn has no `Settled`, it appends "Reply interrupted by restart" (if `Accepted` exists) or "Delivery unknown" (if not), then `Settled`. Nothing is resent.
- **Reading.** `Core::lead_chat_page(project, before)` returns up to 60 visible entries before an id. The view loads the newest page on first render, loads older pages when scrolled near the top, keeps at most 180 records in memory, and never prunes the store. A failed or corrupt store is left untouched and reported; sends are blocked.
- **Deletion.** Forget calls `lead_chat_store::delete` (refusing redirected paths) before clearing the saved Lead; `memory.json` stays. Project removal deletes the whole `leads/<project-id>/` folder, as before.

### Doorbell and restart summary

The ledger, batching, and gates are the ones in [shika-cli.md](shika-cli.md#the-doorbell); only delivery changes.

- **Gate.** `lead_gate` reads the chat for a chat Lead: idle also requires not saving, not busy and no save failure; draft is "the composer has text"; last typed is the composer's; alive is `LeadChat::live`.
- **Bell.** `ring_doorbells` builds the same `[shika] Workers changed: ... Run shika wait.` line and submits it with origin `ShikaEvent`, plus one visible event row per worker: "<title> is ready to check", "<title> is asking you", or "<title> exited", with the CLI in parentheses only when titles repeat. Each row is keyed `worker:<id>:<turn>`; a later report on the same worker turn replaces the row's text with "... - Report updated" in place. The pending marks are held in `Card::chat_delivery`.
- **Accounting.** `chat_delivery_finished` marks the batch rung only on `Accepted` (a successful pipe write). On delivery failure nothing is marked and the bell rings again later.
- **Restart summary.** For a chat Lead the summary is not held for quiet output. Rows "Shika restarted. Worker worktrees were kept." and, when the checkout was not moved, "The Lead checkout was kept without moving to the latest base." are shown; the detailed line goes privately to the CLI. `reportedWorkers` is updated only on `Accepted`; on failure the summary is restored for another try.
- Internal input never becomes an author bubble, and starts a normal turn with its own notification.

### Status, clock, notifications and Seen

| Status | Chat evidence |
| --- | --- |
| Waiting | Live Lead before its first turn. Spawning and initialization start nothing. |
| Working | A submission was dispatched (`advance_chat(Working, true)`), accepted, or a question was answered. Includes tools, silence and a blocking `shika wait`. The activity area shows [step lines](#activity-steps) built from the control socket, "Working..." when no command runs. |
| Asking | A supported question or unknown interaction is pending, or a `shika pr`/`shika close` dialog for this Lead is open (label "Waiting for your decision"). |
| Ready | The turn settled: final result, failure, or "Reply stopped". |

`Activity::advance_chat` applies explicit transitions only: no screen sampling, quiet fallback or idle debounce (chat cards are skipped in the terminal sampling loop). The turn clock starts at dispatch and survives Asking and back. One notification per turn at the first blocker or completion. Seen is unchanged: chat focus (composer, transcript, header, or any bubble text) counts as terminal focus, or the card stays selected for one second.

### The view

`LeadChat` is one GPUI entity per project, created by `ensure_lead_chat` for every project whose saved Lead is Claude, so a resting Lead shows its history and drafts and scroll position survive card switches in memory. `chat_side` replaces the terminal side for a selected live or resting Claude Lead, with the header (antler mark tile, "{project} Lead", the worker summary on a live Lead, Stop Lead `⌘⇧W` or Resume `↵`). The list is a GPUI `list` with bottom alignment and tail follow; rows are time separators (first row, a 5 minute gap, or a new local day, formatted with libc `strftime`), event rows, and one row per markdown block, so a very tall reply builds only visible rows. Prose blocks over 4096 bytes are split for layout without truncating text. Bubble text is a read-only `ChatInput` per visible block, cached by `(record id, block)`. "New messages" appears when not at the bottom. Ctrl+Q returns to the cards. Exact geometry, keys and focus rules are in [DESIGN.md](../design/DESIGN.md#lead-chat).

A live Lead with no visible history shows the antler mark, "What should we work on?" and the composer as one centered group with no dock; after the first send the composer docks at the bottom, without animation. The composer has no border (focus is the caret) and a 32px round action: send arrow (sunken and inert when the draft is empty, primary with text) or stop square while Working or Asking.

#### Activity steps

While a turn runs, the lane shows a short step list: the current step in 12 ink-3 as the last line and up to 3 finished steps of this turn above it in 12 ink-4 with a leading "✓ ". Steps come only from Shika's knowledge of the `shika` commands the Lead runs over the control socket (never from provider output, thinking or tool text), are memory only, per turn, never stored in history, and vanish when the turn settles. Identical consecutive commands collapse. The command-to-copy table is in [DESIGN.md](../design/DESIGN.md#lead-chat) (Events and activity). The same current-step string is the second line of a Working Lead's card in the column, for terminal Leads too; otherwise a chat Lead's card shows the first line of its latest final reply (markdown stripped), and other Leads show "{CLI} · Lead". Code: `crates/shika/src/lead_steps.rs` holds the pure copy (`step_text`), the log (`StepLog`, `StepToken`) and the card line (`card_line`). `handle_control` starts a `StepToken` for every Lead command but `help` and `report` and wraps the reply channel in a `Responder`, so the step ends with the reply wherever it is sent (including the asynchronous `wait`, `pr`, `close`, `new`, and `diff`). A refused or failed command, a `wait` that timed out empty, a cancelled `close`, and a command whose client left (the token is dropped unanswered) leave no step; a cancelled `pr` leaves "Create PR cancelled for {task}". `Shika::sync_lead_activity` runs every tick: a changed `turn_started` starts the list over, a settled Lead (Ready, Waiting, exited) clears it, late replies from before a reset are ignored, and the result goes into `LeadChat::set_steps` (chat Leads) and the card. It also counts the workers the Lead started (`WorkerSummary::count`; exited counts as waiting, starting as working) for the chat header. While a wait runs "Waiting for workers..." is that step, not a second label. Terminal Leads get steps and the card line too. The card's reply line is `lead_chat_markdown::reply_first_line` of the latest Lead record, kept in `Shika::lead_snippets`: fed by live replies and read once per project from saved history off the UI thread, dropped when the Lead is forgotten.

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
- Chat is selected by preset id (`claude`) with no setting; there is no way to keep a Claude Lead in a terminal.

## Contributor guide

### Code map

| File | Responsibility |
| --- | --- |
| `crates/shika-core/src/lead_transport.rs` (new) | `ChatTransport` (public handle), `Actor` (turn state, persistence, process lifecycle), `Process` (spawn, env, nonblocking stdin, reader threads, signals, reap), `ClaudeStream` (decoder and final-response filter), `ChatLaunch::args`, `write_frame`, `ChatEvent`, `ChatStatus`, `ChatQuestion*`. Unit tests over the fixtures. |
| `crates/shika-core/src/lead_chat.rs` (new) | The `Core` boundary: `lead_chat_send`, `lead_chat_events`, `lead_chat_interrupt`, `lead_chat_answer`, `lead_chat_retry_save`, `lead_chat_page`, `confirm_lead_chat` (operations lock, live identity), `stop_lead_chats`. |
| `crates/shika-core/src/lead_chat_store.rs` (new) | `ChatStore`: open with path and version checks, `bind`, `append_batch`, `page`, `recover`, index rebuild; `delete` for Forget; `ChatContent`, `ChatOrigin`, `ChatRecord`. |
| `crates/shika-core/src/lead.rs` | `start_claude_lead` and the shared `start_lead_inner` (same worktree, conversation choice, guide plus memory, control env); chat saves no provisional id and carries `chat_history`; Forget deletes chat. |
| `crates/shika-core/src/lib.rs` | `Core::chats` map; `hang_up` stops the transport; `Session::pty` became optional; re-exports; core integration tests with a fake `claude`. `remove_project` now deletes memory (and chat) after hanging up sessions. |
| `crates/shika-core/src/projects.rs`, `session.rs`, `error.rs` | `SavedLead::chat_history` (`chatHistory`); `Session::pty: Option<PtyId>` (`None` only for a chat Lead; Switch agent refuses it); `Error::LeadChat`. |
| `crates/shika-core/tests/fixtures/` (new) | Two installed-CLI recordings, one hand-written stream, and their provenance README. |
| `crates/shika/src/lead_chat.rs` (new) | `LeadChat` view (rows, paging, scroll anchor, selection, composer wiring, banners) and the `Shika` glue: `ensure_lead_chat`, `submit_lead_chat`, `drain_lead_chats` (events to status, notifications, questions, identity confirmation, resume and exit handling), `selected_chat`, `chat_side`. |
| `crates/shika/src/lead_chat_markdown.rs` (new) | Original markdown subset parser, block splitting, `safe_link`, theme-derived highlights. |
| `crates/shika/src/lead_chat_question.rs` (new) | Native question dialog: open or queue, step through questions, build the answer, cancel, keys and view. |
| `crates/shika/src/chat_input.rs` (new) | Multiline editor and read-only selectable text: IME, selection, visual rows from GPUI's wrapped layout, caret affinity at soft wraps, undo/redo, links, Enter/Escape/Ctrl+Q events. |
| `crates/shika/src/text_edit.rs` | The editing core shared with `name_input.rs`: text, selection, and AppKit's standard key bindings (word, line, paragraph and document moves, Shift extension, deletes, the kill buffer, emacs keys). The composer keys are in [DESIGN.md](../design/DESIGN.md#lead-chat). |
| `crates/shika/src/control.rs` | `ChatDelivery`, `chat_delivery_finished`, chat-aware `lead_gate`, chat delivery in `ring_doorbells` and the restart summary, `LeadState::waiting`. |
| `crates/shika/src/activity.rs` | `Activity::advance_chat` and its test. |
| `crates/shika/src/main.rs` | `Card::chat`, `Card::chat_delivery`, `LeadRun::unexpected_exit`, `Overlay::LeadQuestion`, `Shika::lead_chats`, `chat_questions`; launch routing to `start_claude_lead`; focus and Seen for chat; skipping chat cards in terminal sampling; tabs disabled; quit hook. |
| `crates/shika/src/model.rs`, `appearance.rs`, `Cargo.toml` | Forget dialog fact about chat history; `PartialEq` on `Chrome` for cheap theme change checks; `libc` for local time formatting. |
| Docs | This guide; pointers in AGENTS.md, CONTRIBUTING.md, lead-agent.md, shika-cli.md, agent-activity.md, `crates/shika-core/README.md`; DESIGN.md Lead chat section and plates; MANUAL_CHECKS.md. The Lead guide is unchanged. |

### Ownership rules

- Core owns processes, pipes, cancellation, history and saved ids. The app owns messages on screen, focus, status, dialogs and notifications.
- Only the actor thread touches the child and the store. Blocking Core calls (`start_claude_lead`, `confirm_lead_chat`, `lead_chat_page`, `stop_lead_chats`) run off the UI thread, except the quit hook.
- Every event that changes saved state is fenced: frames by process generation, sends by cancel epoch, id commits by live session plus the operations lock. A stopped or replaced Lead cannot save an id or append to another lifetime.

### Debugging by symptom

Inspect the **isolated** data directory: `projects.json` (`lead.cli`, `lead.sessionId`, `lead.chatHistory`) and the chat database. Read it with the app quit, or read-only:

```sh
db="$fixture/data/leads/<project-id>/chat/history.sqlite"
sqlite3 -readonly "$db" "SELECT key, value FROM metadata;"
sqlite3 -readonly "$db" "SELECT id, turn, segment, visible, content FROM records ORDER BY id DESC LIMIT 20;"
sqlite3 -readonly "$db" "SELECT * FROM visible_entries ORDER BY id DESC LIMIT 20;"
```

| Symptom | Inspect first |
| --- | --- |
| A reply never appears; card stays Working | Last turn's records: `Outgoing` without `Accepted` means the write never completed; `Accepted` without `Settled` means no `result` arrived. Is the child alive (`pgrep -fl 'claude -p'`) and still in a tool or `shika wait`? An unknown `control_request` shows "This Lead needs an interaction chat does not support." and stays Asking. Stop generating settles it. |
| Turn settled but no Lead bubble | A tool-only turn has no final text and no bubble, by design. Check whether the last assistant text came before a tool call (it is commentary). Reproduce with the decoder tests, adding the stream as a fixture. |
| Intermediate text appeared in a bubble | A filter bug. Capture the stream with a fake CLI or the synthetic server, add it to the fixtures, and assert in `lead_transport` tests. Check record `uuid` dedup and `parent_tool_use_id`. Never fix by trimming text. |
| "This Claude Code version could not be read" | A record type or block type the decoder does not know, an oversized frame, or no initialize response within 10 seconds. Compare `claude --version` with the fixtures and record a new one. |
| Resume fails or starts fresh unexpectedly | `lead.sessionId` null (stopped before the first turn, or cleared after a refusal); `<claude config>/projects/*/<id>.jsonl` missing (check `CLAUDE_CONFIG_DIR`); a Shika started from a Claude session (the transport removes `CLAUDE_CODE_CHILD_SESSION`, but check the CLI's own config). Exit within 5 seconds of resume follows the refusal path in `lead_upkeep`. |
| "history identity changed" | `lead.chatHistory` does not match `metadata.lifetime`, for example after the chat folder was deleted or copied. Forget clears both. Never repair by editing normal app data. |
| History missing after relaunch | `lead.chatHistory` present? Database present and readable? Forget or project removal deletes it. An unused Lead that was stopped before any message has nothing to show. |
| "Could not save this conversation" | Permissions on `leads/`, the project folder and `chat/` (must be real directories, `0700`), disk space, a locked database. Retry save appends the buffered rows. |
| Status sticks at Asking | A question queued under another overlay (`chat_questions`), an open `LeadDialog`, or an unsupported interaction. Status sticks at Working: see "A reply never appears". |
| Doorbell never rings a chat Lead | `lead_gate`: composer draft, typing within 3 seconds, `saving`, `busy`, `save_failed`, an open wait, or `live` false. Rows appear only when delivery starts. |
| Workers stopped after Stop generating | They should not; workers are separate PTY process groups. Check whether the Lead started a process outside Shika that a worker relied on. |
| A chat child outlives the app | The quit hook (`stop_lead_chats`) did not run, for example after a crash. Find it with `pgrep -fl 'claude -p'` in the Lead worktree and stop it. |

### Isolated reproduction

Never test against normal app data or a personal repository. Two levels, from cheapest:

**Core with a fake CLI (no GUI, no network).** `claude_chat_has_no_pty_confirms_identity_and_keeps_history_until_forget` and `chat_interrupt_cancels_a_blocked_pipe_write_and_quit_reaps_the_child` in `crates/shika-core/src/lib.rs` put a shell script named `claude` on a scratch PATH and use a scratch CLI home. Copy that script shape to reproduce a stream bug: answer `shika-initialize`, then for each `"type":"user"` line print `system/init`, assistant records and a `result`.

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
- `claude-chat.jsonl` is hand-written from the documented format: thinking, subagent text, a partial `stream_event`, a duplicate record, and an aggregate `result` containing commentary.
- Tests read fixtures with `include_str!`; they never spawn a real Claude or read provider files.

To re-record after a CLI upgrade:

1. Check `claude --version` and `claude --help` for every flag in `ChatLaunch::args`. If one changed, update the code, AGENTS.md and this guide first.
2. Make a disposable repository and an empty `CLAUDE_CONFIG_DIR` under a temp directory. Run a loopback HTTP server that answers the Messages API with scripted responses: text, then a tool call (Bash, or `AskUserQuestion` for the question fixture), then a final text. Point the CLI at it with a fake key; check the installed version's documentation for the base-URL setting. Never use a real key or normal config.
3. Run the CLI with the chat arguments and a fixed `--session-id`, write the initialize control request, then user records, answering any `control_request` with the same envelopes the actor writes. Save stdout.
4. Sanitize: remove paths, usernames, metadata and random ids; normalize the session id (`fixture-session`) and request ids; keep `message.id` values and distinct record `uuid`s, because the dedup logic depends on them.
5. Name the file `claude-<version>-<case>-local.jsonl`, keep older fixtures, add a test like `installed_cli_two_turn_fixture_hides_split_blocks_with_the_same_message_id`, and update the fixtures README.

### Tests

| Test | Covers |
| --- | --- |
| `lead_transport::tests::*` | Final-only extraction from both kinds of fixture, split blocks with one message id, multiple final text blocks and uuid dedup, question parsing, identity mismatch, garbage, unknown control requests, failed results. |
| `lead_chat_store::tests::*` | Atomic completion, keyed event replacement, index rebuild, recovery markers, 60-entry paging across reopen, corruption preserved, Forget keeping `memory.json`. |
| `tests::claude_chat_has_no_pty_...` | No PTY, provisional then confirmed id, final-only history, interrupt and continue, Stop keeping history and workers, Forget deleting chat and keeping memory. |
| `tests::chat_interrupt_cancels_a_blocked_pipe_write_...` | A write blocked on a full pipe is interruptible, and quit reaps within 4 seconds. |
| `activity::tests::chat_status_uses_explicit_results_and_retains_the_turn_clock` | Turn clock across Asking, one notification per turn. |
| `lead_chat_markdown` tests | Bash fences as code, HTML as text, unsafe links refused, long prose split without losing style ranges. |

Passing tests are not native acceptance. Record GUI results in MANUAL_CHECKS.md.

## Extending to other CLIs

### Where Claude is hard-wired today

Adding a CLI is not just a new decoder. These places assume Claude:

- `lead_transport.rs`: `ClaudeStream`, `ChatLaunch::args`, the initialize write in `Process::spawn`, `store.bind("claude", ...)`, Claude-specific error strings, and the exit rule that treats every unstopped child exit as `Exit` (wrong for one-child-per-turn CLIs).
- `lead.rs`: `start_claude_lead` passes `"claude"`; `start_lead_inner` requires a preallocated id for chat and returns `Error::UnknownCli` without one.
- `lead_chat.rs` (core): `confirm_lead_chat` requires `preset_id == "claude"` and `saved.cli == "claude"`.
- App: launch routing (`chat = lead.is_some() && preset.id == "claude"` in `main.rs`), `ensure_lead_chat` in `Render`, and `selected_chat` (`lead.cli == "claude"`).
- `LeadRun::discovery` (Codex id polling) and the terminal resume grace assume PTY exits.

### Steps for any adapter

1. **Check the CLI.** Record version and `--help` for every flag. Do not invent flags. Never pass a CLI's own `--worktree`.
2. **Record evidence first.** Fixtures from the installed binary with disposable config and no paid requests where a local server route exists; otherwise ask the author before any real request, and mark hand-written fixtures as such. Cover commentary, several tools, retries, compaction, empty finals, failures and interruptions.
3. **Introduce an adapter boundary.** Split the decoder, arguments, process model (long-lived or per turn), initialization, interrupt method and identity source behind one type, and keep `Actor`'s persistence, fencing and event vocabulary shared. Keep the store's `bind(cli, conversation)` honest.
4. **Identity.** Decide where the id comes from (preallocated, allocated by a helper command, or reported on the first turn) and commit it only through a generation-checked `confirm_lead_chat` for that preset.
5. **Status and exits.** Map only explicit lifecycle events. For per-turn CLIs, a clean exit after a terminal event is a normal Ready and must not emit `Exit`, `Resuming`, or trip the 5 second resume grace; a clean exit without one is "The reply ended before it was complete".
6. **Switch the preset.** Update launch routing, `confirm_lead_chat`, `ensure_lead_chat` and `selected_chat`, and disable Codex id polling for a chat Lead.
7. **Docs and checks.** Update AGENTS.md (launch table and Lead flags), this guide, lead-agent.md, CONTRIBUTING.md's product clause, DESIGN.md if visible, and add a MANUAL_CHECKS.md section. Leave items unchecked until performed.

### Codex (planned)

- Fresh: `codex exec --json --dangerously-bypass-approvals-and-sandbox -c <instructions> -c <trust> -`. Resume: `codex exec resume --json --dangerously-bypass-approvals-and-sandbox -c <instructions> -c <trust> <id> -`. Write one prompt to stdin and close it; set the cwd to the Lead worktree.
- Instructions: TOML-encoded `developer_instructions=<guide plus memory>` on every exec, plus today's per-process `projects={"<worktree>"={trust_level="trusted"}}`. Reverify `developer_instructions` in exec mode specifically; the existing evidence is interactive.
- Identity: `thread_id` from `thread.started`, saved with the live Lead check and required to match on resume. No newest-thread scan and no 2 second polling.
- Final response: buffer `agent_message` items by item id; on `turn.completed` publish only the last completed message after the last tool call (or a verified final phase). Intermediate commentary is also `agent_message`. `turn.failed` or `error` fails the turn.
- Trap: the current fresh path returns `Error::UnknownCli` because Codex has no preallocated id.

### Cursor CLI (planned)

- Every turn: `agent -p --output-format stream-json --yolo --trust --sandbox disabled --resume <id> <prompt>`. No keep-alive process; help offers no stream input.
- Fresh: run `agent create-chat` in the Lead worktree and save the id. If allocation fails, keep today's fallback: launch without an id and bind the reported id to this fresh live Lead only.
- Instructions: there is no system prompt flag. Keep the existing bootstrap as a separate first Shika-origin turn for a fresh Lead only, with no author bubble; never on resume or later turns.
- Final response: buffer complete top-level assistant messages; publish the last one after the last tool call on a successful `result`. Cursor's `result` text can concatenate intermediate messages and must never be shown.
- Trap: `CliHome::session_exists` returns unknown for Cursor, so a refused resume always rests with the retry-or-Forget toast.

### Pi (planned)

- Launch: `pi --mode rpc --approve --append-system-prompt <instructions> --session-id <uuid>`. Before the first prompt, RPC `get_state` with a unique request id must report the same `data.sessionId`.
- Turns: one RPC `prompt` per turn; wait for its correlated acceptance and then session events. No steering or follow-up queues. A handled prompt that starts no run must still settle.
- Final response: buffer `message_end` records; publish the last completed text after the last tool call when `agent_settled` reports completion without abort or error. `agent_end` can precede automatic recovery and is not final; command acceptance is not completion.
- Interrupt: RPC `abort`, wait for settlement, keep the process; fall back to terminating the group.
- Questions: map Pi extension UI dialogs with verified schemas to the existing question dialog; notifications and status widgets are not blocking.
- Trap: `--session-id` creates a missing id, so it is not evidence of a successful resume; keep today's existence check. Use the installed package's `docs/rpc.md` and related docs, not guessed verbs, and write the protocol in Rust, not Pi's TypeScript SDK.

### Contributor guardrails

These must not change without the author's decision:

- Filter before persistence. Only the final response after the last tool call, released by a successful terminal event, may be stored or shown. Never show an aggregate `result` text, a partial, thinking, tool output, subagent text or stderr.
- No PTY or hidden terminal behind the chat, no screen scraping, no raw JSON in the UI. Unknown optional events are ignored; unknown required ones fail visibly.
- No model API, provider SDK, credential handling, or global CLI config writes. Arguments are arrays; JSON and TOML values are built with encoders.
- The Lead guide, prompts and memory delivery stay as they are. No chat-shaping instructions.
- Never replay history into a CLI, never resend automatically, never answer a question or confirm PR/Close on the author's behalf.
- Keep generation, epoch and live-identity fencing; commit ids only under the operations lock for the exact live Lead.
- Keep pipes drained off screen, frames and queues bounded, IO off the UI thread, and explicit reaping at quit.
- Stop generating stops only the Lead's own process group, never worker PTYs.
- History stays local, private, unlimited, and separate from memory. Schema changes bump `user_version` additively; never overwrite or reset a store that fails to open.
- Every chat color comes from `Chrome`. No hex values in chat views.
- Never copy Zed's GPL markdown, editor, terminal or terminal-view code.
- Workers, closed tasks and non-chat Leads get no transcript.

## Author's decisions

- This is a UI improvement for the Lead. Keep today's lifecycle, instructions, worker behavior, automatic fresh-start fallback and toasts, Seen rule and author-confirmed PR/Close dialogs.
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
| Composer growth **six text lines**, capped at **144px total** | Existing 20px line height plus 12px top/bottom padding, then internal scrolling. |
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
- One isolated native smoke run recorded in [MANUAL_CHECKS.md](../MANUAL_CHECKS.md#claude-code-lead-chat-2026-10-16): final-only rendering after a real Bash tool, markdown, a native question, Stop and relaunch keeping history, a further send on the same conversation, Shika Light and Dark.
- Still open: authenticated recall and compaction, every theme and translucency mode, all worker and dialog paths, keyboard and screen reader, IME, long histories, crash and write-failure recovery. Do not claim these from tests.

Structured stream formats are documented but not a stable public API. Keep per-version fixtures, tolerate unknown optional fields, and fail visibly when lifecycle or identity evidence is missing. Attachments, voice input and reactions are possible follow-ups outside this design.
