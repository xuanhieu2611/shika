# Agent activity and elapsed turns

This guide explains the timer-reset bug, the replacement state model, and how to debug or extend it. Read it before changing agent status, timers, notifications, input capture, or activity-based Close checks.

This guide records the product decision. [design/DESIGN.md](../design/DESIGN.md) defines the visible states. [MANUAL_CHECKS.md](../MANUAL_CHECKS.md#agent-activity-and-stable-turn-timer-2026-10-07) separates verified behavior from remaining native checks.

## What was wrong

The original implementation inferred Working and Ready almost entirely from PTY output timing. It suppressed output for one second after user input or terminal interactions, then treated about two seconds of quiet as Ready.

That conflated three different things:

1. **Bytes arriving:** terminal echoes, redraws, spinners, and responses all produce bytes.
2. **Agent activity:** the agent can be working silently, waiting for permission, or finished.
3. **A turn's lifetime:** when a prompt started work and when that work settled.

The displayed timer used `card.since`, the status timestamp, rather than a dedicated turn-start timestamp. A false Working → Ready → Working transition therefore restarted the visible timer. The submission handler also reset it on another Enter while already Working.

Clicking did not directly assign a new timer timestamp. Focus/mouse reports and resize could provoke CLI redraws and alter the output exclusion window. Real output inside the exclusion window was ignored; delayed redraws outside it could be counted as work. Typing a draft had the same classification problem. These interactions could disturb status and indirectly reset the timer.

An illustrative failure sequence, not a recording of a particular user session:

```text
prompt accepted             Working, timer starts
focus or draft interaction  real output temporarily excluded
quiet heuristic fires       Ready, status timestamp changes
delayed output/redraw        Working again, timer starts over
```

This was a Shika tracking-design bug built on an inherently ambiguous input source. CLI terminals do impose limitations, but they do not require timers to reset on clicks.

## Why this approach

Shika is a terminal workspace, not a conversation frontend. Contributors must preserve the user's real CLI, its tools and UI, and the subscription they already use.

Research compared two approaches:

- **Herdr:** agent-specific live-screen rules plus direct lifecycle integrations where available. This is a close architectural match to Shika.
- **T3 Code:** structured provider adapters, such as Codex app-server events and the Claude Agent SDK. These expose clearer turn/approval boundaries but would require a substantially different integration and user interface.

We chose a **hybrid detector with a separate turn clock**. Reliable lifecycle reports take precedence; live-screen chrome supplies states otherwise. Quiet is a bounded fallback, not the definition of completion. This improves behavior without replacing the terminal or installing global hooks.

| Alternative | Why not use it alone? |
| --- | --- |
| Remove the timer | Hides a symptom, but leaves false statuses, notifications, and Close decisions. |
| Increase the quiet timeout | Delays notifications without distinguishing silent work from completion. |
| Parse all output for questions | Prose, examples, and old history are ambiguous; unnecessary for the product requirement. |
| Use hooks universally | Providers and versions expose different events; incomplete hooks can leave stale Working or miss permission resolution. |
| Replace the CLI with structured conversation adapters | Changes Shika's product and duplicates the provider's interaction surface. |

Perfect semantic understanding is not required. An ordinary final response asking “Which approach do you prefer?” may be Ready to check. A recognizable blocking permission or structured-question dialog should be Asking you. Ready means a turn is ready to inspect, not that the task succeeded.

The screen rules are original code. Herdr's Apache-2.0 implementation informed the research only; it was not copied into this MIT project. Research snapshots: [Herdr](https://github.com/herdrdev/herdr/tree/3d9d2b18dab139ba226ebc5a1c9a9f2c9c3ee4df) and [T3 Code](https://github.com/pingdotgg/t3code/tree/bfec2387b8102975c84690f99be0f5f834fd0cbe). Their current implementations may differ.

## States and evidence

| Visible state | Meaning |
| --- | --- |
| Waiting | Initial idle, before a turn starts. |
| Working | A submitted candidate turn or recognizable live working activity. |
| Asking you | A recognizable permission/question dialog is blocking progress. |
| Ready to check | A finished turn, bounded quiet fallback, or process exit, including failure. |

`Signal::Unknown` means no useful evidence internally. It is not a fifth card state and is not synonymous with idle.

### Provider coverage

| Provider | Current activity source | Important limit |
| --- | --- | --- |
| Claude Code | Live-screen chrome and explicit terminal title signals | Private UI formats can change. No state-reporting hook is installed. |
| Codex | Live-screen chrome and explicit terminal title signals | No app-server or lifecycle hook integration is installed. |
| Cursor CLI | Live status row and the `→` composer prompt | Cursor CLI 2026.10 paints a one- or two-cell braille spinner plus a short status word (`Thinking`, `Summarizing`, `Working`, or a tool verb such as `Reading`) immediately above a `→` prompt. `▄` and `▀` bars frame that prompt, and an empty prompt shows `ctrl+c to stop` while processing. A `>` quote is transcript text, not the prompt. Terminal-title status is off unless the user enables it. Do not assume editor hooks have identical CLI coverage. |
| Pi | Session-local lifecycle extension, with screen fallback and visible blockers | The bridge does not universally observe dialogs from other extensions. |

Source precedence in the app:

1. A recognizable visible blocker can supplement an integration that does not report dialogs.
2. An available, accepted lifecycle report controls the state.
3. Otherwise use agent-specific live-screen rules.
4. If those return Unknown, the state machine may apply quiet/startup fallback within an existing turn.

Never promote raw PTY bytes into a new-turn signal. A draft and a redraw are not prompts.

### Transition rules

- A candidate submission from Waiting/Ready starts a turn. The first-ever strong Working/Blocked observation can also start one without captured input.
- Known Working survives silence and continually confirms activity. It must not expire just because a tool or model emits no text.
- Idle needs at least two observations and 500ms before completion. New meaningful content interrupts that confirmation.
- A tentative submission without confirmed work has eight seconds of startup grace. A fresh authoritative Idle report can bypass that grace, but still uses idle debounce.
- Unknown may finish a started turn after two seconds without meaningful transcript changes when output evidence exists. Its quiet clock also accounts for the last strong activity observation.
- Asking survives Unknown until visible resume, confirmed idle, or process exit. An unreadable overlay is not evidence that the user answered it.
- Process exit settles immediately and is terminal for that `Activity` instance.

The debounce, quiet, and startup-grace constants are in `activity.rs` and `model.rs`. Change them only alongside timeline tests and an explanation of which failure mode the change addresses.

## How the implementation fits together

```text
Typed input                         PTY output
  |                                   |
Host + PromptCapture                Terminal engine
  |                                   |
candidate submission                read-only live text + title
  |                                   |
  |                          detect + OutputEvidence
  |                                   |
  +--------------+--------------------+
                 |
Pi events -> private metadata -> background poll -> Lifecycle cache
                 |
        Activity::advance / advance_authoritative
                 |
       status + retained turn epoch + Transition
                 |
     card rendering / notification / diff stat / safe Close
```

### Input provenance

`Host::write` distinguishes `InputSource::Typed`, `Report`, and `Reply`.

Only typed input feeds naming/submission capture. Protocol replies and mouse/focus reports cannot name a task or submit a prompt, even if their bytes contain CR. Bracketed paste newlines remain draft text. History recall followed by Enter is a candidate because actual recalled contents belong to the CLI editor. Common cursor/editing controls, including native clearing, are handled by `PromptCapture`.

The initial naming capture and subsequent submission capture are separate. Naming a card or discovering a CLI title must not change the activity clock. Shell panes have capture disabled and never drive agent activity. Preparation input suppression and startup-query reply handling remain unchanged; see [worktree preparation](worktree-preparation.md) and [terminal tabs](terminal-tabs.md).

Capture is not a full replica of every provider editor. Local commands, history selection, or rejected submissions may create a tentative turn until live evidence or startup grace resolves it. Enter during Working/Asking does not create a new timer epoch.

### Live screen, not scrollback

`Terminal::live_text_lines` reads the current bottom/live viewport directly from the engine, independent of the user's display offset. Sampling does not scroll the terminal, move the cursor, alter selection, clear paint dirty state, or produce extra wakeups. Hidden agent terminals remain alive and drain output.

The detector looks for anchored working indicators, framed editors, dialog controls, and explicit title markers. It avoids treating quoted examples, stale spinners above a later response, or draft text as live controls. An agent-owned transcript viewer is not the current prompt state.

The app samples terminal text before locking `HostState`. Preserve that lock ordering: terminal query replies may call back into the host. Do not use the painted viewport snapshot as a convenient replacement for live sampling.

### Meaningful output evidence

`HostState::note_output` records arrival time only. `OutputEvidence::observe` compares live transcript signatures, excluding the editor and known footer/status animation. It retains real fenced response code. Whitespace is ignored to reduce resize/wrap noise, so whitespace-only changes intentionally do not count.

The first sample establishes a baseline. New evidence needs changed transcript content and a newer, nonfuture byte timestamp. Identical redraws cannot extend quiet forever. Geometry/report interactions have a one-second exclusion window; suppressed samples still update their baseline and byte watermark so a delayed identical redraw cannot qualify later.

Draft typing is not included in that geometry exclusion window. Excluding the editor structurally prevents draft echoes from looking like work without hiding genuine output while the user types.

This comparison is bounded to the live screen and remains heuristic. Unknown layouts, unusually formatted overlays, and genuine output coinciding with a geometry redraw can still be ambiguous.

### Launch prompts and turns

A CLI can be started with its prompt as an argument (`shika new` for a Lead's worker, and the Lead's own first prompt; see [lead-agent.md](lead-agent.md)). Nobody types that prompt, and "bytes alone cannot start one", so without a submission the card would stay Waiting and never notify. Two pieces fix it, both on the existing machinery:

1. **The prompt is the first submission.** When the session exists, `HostState::seed_launch_prompt` sets what a typed first line sets: `submission` and `last_submission` are bumped, the card name is the prompt's first nonblank line (the Lead keeps its title), and the naming capture is finished so a line typed later neither renames the card nor the branch. The tick then consumes it exactly as it consumes a typed one: `Lifecycle::submitted` fences the previous (startup) report, the candidate turn and `turn_started` begin, the notification budget resets, the CLI title watch starts (not for a Lead), and `Card::running` protects Close before the tick runs. `Card::control_status` also reports Working in that gap so a `wait` right after `new` is not told there is nothing to wait for.
2. **The turn is marked as booting.** The tick calls `Activity::hold_for_launch` right before the `advance` that consumes the seeded submission. For that turn only, until a strong Working or Blocked observation, the CLI's banner is not "output" for the idle rule and the quiet rule: an idle editor or an unreadable screen ends the turn only through the eight-second startup grace (measured from the last output), or an authoritative report. Without this, a CLI that draws its banner and an idle editor for more than half a second before it starts on the prompt would turn Ready, notify, and then resume Working on the same turn with no second notification.

Provider paths: Pi's startup Idle report (sequence 1) is ignored while its first turn is pending, so Pi's `agent_start` and `agent_settled` reports drive the launch turn like a typed one; Claude Code, Codex, and Cursor use live-screen chrome, so they depend on their Working chrome appearing within the grace. The tests are `a_launch_prompt_*` in `activity::tests` and `host_tests` (`a_launch_prompt_counts_as_the_first_submission_and_names_the_task_once`, `a_launch_prompt_turn_runs_the_same_steps_as_a_typed_one`, which walks the tick's steps including Pi's reports). Real-CLI behavior is a manual check in `MANUAL_CHECKS.md`.

### Lead typing: the doorbell, `send`, and `key`

A Lead can type into terminals (see [lead-agent.md](lead-agent.md)). Those writes follow the input rules above instead of adding a second path:

- **Not the author.** They go through `HostState::inject`, which writes to the PTY without setting `last_typed`. The guards that ask "did the author type?" (the doorbell's 3 second quiet, `send`'s draft check, the dropped Enter) therefore see only the author.
- **A submission when it ends in Enter.** A pasted line and its Enter pass through `HostState::capture_typed`, the same function `Host::write` uses for typed bytes, so `submission` and `last_submission` change exactly as for a typed line: the tick starts a candidate turn, the timer and notification budget apply, `Lifecycle::submitted` fences the old report. This is how the Lead card shows Working after a doorbell, and a worker after `shika send`. A `--no-enter` paste and `shika key` presses are not captured: a key is not a prompt, and a dialog answer is seen through the live screen as before.
- **Paste, then Enter.** The text is a bracketed paste and the Enter is a separate write 150 ms later, because some TUIs (Codex's paste-burst handling) read an Enter inside a paste burst as a newline and never submit.
- **A draft is typed text not yet submitted.** `HostState::has_draft` is true while the submission capture holds text on its line, or after a history recall (Up) until Enter or clear. A CLI's grey suggestion is not typed, so it does not count. The doorbell and `send`/`key` refuse while the draft exists.

Tests: `a_pasted_line_and_its_separate_enter_are_one_submission`, `injected_steps_are_refused_after_the_author_types_or_the_cli_exits`, and the doorbell tests in `shika/src/control.rs`.

### Turn clock, attention, and Close

Keep these identities separate:

| Value | Responsibility |
| --- | --- |
| `Activity::turn_started` | Retained start of the current/most recent turn; source of elapsed time. |
| `Activity::since` / `Card::since` | Status/result epoch used by attention and seen tracking, not the timer. |
| `HostState::submission` / `Card::submitted` | Captured candidate-input generation and what the UI has consumed. |
| `AgentActivity::seq` | Ordering of lifecycle reports, not user submissions. |

The timer measures wall time, including blocked time, and is shown only while Working. It is not model compute time. Drafts, clicks, focus, scroll, resize, active Enter, and temporary Ready/Working flicker cannot replace `turn_started`.

The epoch and notification budget remain available after Ready. Strong work without a new submission resumes the same turn, so a false idle observation or automatic continuation cannot restart the timer or notify twice. A new submission from Waiting/Ready creates a new epoch. This deliberately does not claim that every autonomous provider action is a distinct new user turn.

`Transition` separates three effects: `changed` requests repaint; `notify` spends the once-per-turn notification budget at the first blocker or completion; `ready` refreshes the diff stat and creates a completed result independently of whether a blocker already notified. Completion can therefore become unseen after its blocker was seen. Existing terminal-focus/one-second-dwell seen behavior is preserved.

`Card::running` and `activity_requires_confirmation` protect Working, Asking, and an unprocessed submission before both initial Close inspection and final removal. Raw redraw bytes and idle drafts are not work. This augments, rather than replaces, the dirty/unpushed/branch-switch Git safety checks.

## Pi lifecycle bridge and ownership

Core adds an explicit `--extension` path only to Pi's launch. The extension lives in a unique mode-0700 temporary directory with a child-only `SHIKA_ACTIVITY_FILE`. It does not edit user/global Pi configuration, repository files, or launch arguments for other providers. Installation failure leaves the original launch unchanged.

The extension writes tiny atomic JSON snapshots `{seq,state}` containing no prompts, output, credentials, or conversation history. It registers no long-lived resources in its factory. Reports follow this lifecycle:

```text
session_start -> idle
agent_start   -> working
agent_settled -> idle
shutdown      -> idle, best effort
```

`agent_end` is not final: retries and automatic continuations can follow. The extension guards unknown host versions and versions before the documented `agent_settled` introduction, 0.80.4. Unsupported hosts emit no reports and use screen fallback. Reload reads the previous metadata sequence rather than restarting at one. `Blocked` is reserved by the Rust protocol but not emitted by this Pi bridge.

The app polls off the UI thread every 500ms, at most one in-flight read per pane, using stable pane identity rather than card position. Core caps reads at 128 bytes, rejects invalid/regressed reports and symlinks/non-files, and takes no core operation lock. Missing or malformed reports release lifecycle authority back to screen detection.

### Why sequence and submission fencing both exist

Consider an old Idle read that finishes after the user submits another prompt. Applying it as authoritative would finish the new turn prematurely, possibly notify and allow Close while work is running.

Two protections address different races:

1. `HostState::note_lifecycle` rejects background reads scheduled for a different submission generation.
2. `Lifecycle::submitted` fences old reports already buffered in host state, including reports not yet consumed by the UI. `Lifecycle::observe` ignores those sequences and ignores delayed first-startup Idle for a pending first turn.

Valid same-sequence reports can restore authority after a transient read failure; a strict “sequence must always increase” rule would prevent recovery. Latest-only snapshots may miss Working during a very fast turn. A fresh completed Idle report is still sufficient to settle that turn without observing its intermediate Working.

Bridge ownership lives in `SessionStore`, separate from cloned public `Session` snapshots. Explicit, idempotent cleanup covers launch failure, PTY exit, session/project removal, and Core drop. The sink cleans up before forwarding exit. Poll/snapshot clones cannot keep the files alive after removal. A killed/crashed app may leave small temporary directories; orderly RAII cleanup is not crash recovery. Reports are not persisted in normal app data.

## Code map

Search by symbol rather than historical line numbers:

| File | Symbols and responsibility |
| --- | --- |
| [`shika/src/activity.rs`](../crates/shika/src/activity.rs) | `detect`, `working_row`, `transcript_signature`, `OutputEvidence`, `Activity`, `Transition`: original screen rules, evidence, clock, debounce, notification budget |
| [`shika/src/lifecycle.rs`](../crates/shika/src/lifecycle.rs) | `Lifecycle::submitted`, `observe`: report fencing, precedence, and recovery |
| [`shika/src/main.rs`](../crates/shika/src/main.rs) | `Host::write`, `HostState::note_lifecycle`, `Card::running`, `tick`, `card_view`: input, sampling, background polling, attention, rendering, and Close wiring |
| [`shika/src/control.rs`](../crates/shika/src/control.rs) | `HostState::seed_launch_prompt`, `Card::control_status`: launch prompts as the first submission, and the status the Lead sees; `HostState::inject`, `Doorbell`, `Shika::type_into`: the Lead's typing |
| [`shika/src/model.rs`](../crates/shika/src/model.rs) | `PromptCapture`, `Status`, `activity_requires_confirmation`, `ECHO`, `QUIET`: input parser, state vocabulary, safe-close predicate |
| [`shika-core/src/activity.rs`](../crates/shika-core/src/activity.rs) | `ActivityBridge`, `read_report`, `activity_sink`: optional launch wiring, validation, and cleanup |
| [`shika-core/src/pi-activity.ts`](../crates/shika-core/src/pi-activity.ts) | Pi event/version handling and atomic metadata writer |
| [`shika-core/src/lib.rs`](../crates/shika-core/src/lib.rs), [`session.rs`](../crates/shika-core/src/session.rs) | `Core::session_activity`, Pi launch wiring, and session-store bridge ownership |
| [`shika-terminal/src/engine.rs`](../crates/shika-terminal/src/engine.rs), [`terminal.rs`](../crates/shika-terminal/src/terminal.rs) | `live_text_lines`: read-only text independent of display offset |
| [`shika/src/appearance.rs`](../crates/shika/src/appearance.rs) | Asking light/dark/glass tokens restored from the archived design/demo |

## Debugging playbook

| Symptom | Inspect first | Regression to add |
| --- | --- | --- |
| Timer resets on click/draft/resize | Candidate count and `turn_started`; ensure rendering is not using `since` | Active turn with real Host input/reports plus false idle/resume |
| History prompt reuses the old timer or does not notify | Recall/Enter capture and new submission generation | Completed turn, Up/Enter, then new Working/completion |
| Card stays Working after completion | Live footer/title match and lifecycle availability; watch for prose matching chrome | Completed response containing the exact working-like phrase |
| Card turns Ready while silently working | Strong-signal recognition and authority selection; do not just enlarge quiet | Quiet tool/model interval with persistent working chrome |
| Cursor notifies while its status row is still up | The row above the `→` prompt, not a `>` quote or two seconds of quiet | Quote plus a live braille status stays Working; Ready only after that row and the processing placeholder are gone |
| Permission overlay becomes Ready | Blocker controls and Unknown retention | Dialog, ambiguous redraw, cancellation or resume |
| New Pi turn finishes immediately | Background read generation, buffered-report floor, startup sequence | Delayed old Idle crossing a new submission |
| Authority never returns after a transient failure | Same-sequence recovery and report validation | Valid report, failed read, same valid report |
| Notifications duplicate | Retained epoch and notification budget, not last typed key | Blocked then finished; Ready then unsubmitted continuation |
| Scrollback changes status | Call sites of `live_text_lines` versus painted snapshots | View old spinner/dialog while live screen is idle |
| Close warns on drafts or misses blocked work | Shared `running()` predicate and pending submission | Idle draft, pending prompt, Asking, exit, dirty Git state |

For local debugging, inspect only metadata by default: provider id, chosen evidence source, signal, status, submission generation, report sequence/floor, turn age, and transition flags. Do not add prompt/output logging to routine diagnostics or ask contributors to upload private transcripts. Do not use absolute timestamps from two clock domains interchangeably; turn/input/output timings use Rust `Instant`, while report ordering uses sequence numbers.

## Reproduce and validate

Fast deterministic checks from the repository root:

```sh
source "$HOME/.cargo/env"
cargo test -p shika activity::tests
cargo test -p shika lifecycle::tests
cargo test -p shika host_tests
cargo test -p shika-core activity::tests
cargo test -p shika-terminal live_
cargo fmt --all --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

The Rust core tests remain usable without Node. When Node 24+ is available, `extension_lifecycle_continuation_reload_and_unsupported_version` executes the bundled TypeScript factory with mocked Pi events. That validates lifecycle/version/reload behavior, not an authenticated provider run. Rust-only runs can skip that subprocess portion.

Native reproduction must use disposable Git repositories, local bare remotes, and an isolated app data directory:

```sh
./scripts/bundle-app.sh --debug
codesign --verify --deep --strict target/debug/Shika.app
open -n target/debug/Shika.app --args --data-dir /absolute/path/to/disposable/data
```

Add only the disposable repository. Verify the isolated process PID and its own window are frontmost before any synthetic input. Never test against normal app data or a personal worktree. Use [terminal-tab guardrails](terminal-tabs.md#validation-and-contributor-guardrails) for process/window identity and hidden-pane checks.

Reproduction sequence:

1. Open each installed CLI and confirm initial Waiting without a spurious completion notification.
2. Submit a prompt that keeps it busy. While Working, type a draft, change focus, click cards, scroll history, resize, and switch to a shell tab. Record whether the timer retains its start.
3. Exercise a real permission/structured-question dialog when possible. Answer or cancel it; the current epoch must survive. A final prose question may simply become Ready.
4. Finish a turn, recall a prompt with Up/Enter, and confirm a new timer and notification budget. Check fast replies and long silent work separately.
5. Check Close for active/blocked clean tasks and for idle drafts, then the existing dirty/unpushed/branch-switch cases.
6. Repeat visual checks in light/dark, opaque/glass, and narrow/hidden-column modes.

Some launch presets bypass tool approval, so not every provider run will show a permission dialog. Detector fixtures cover dialog layouts, but fixture success is not live permission-flow acceptance. Do not modify the user's global permission configuration merely to create a test.

The implementation was validated with workspace tests, strict Clippy, formatting, bundle/signature checks, and a real Pi startup smoke test using disposable Pi configuration and no submitted prompt. This does not establish every real-provider turn, permission flow, or native interaction. Keep actual results and remaining checks in `MANUAL_CHECKS.md`, not inferred passes in this guide.

## Contributor guardrails and future improvements

- Preserve the real terminal and task-local PTY ownership. Global tabs, conversation history, or SDK-based replacement are separate product decisions.
- Keep terminal-engine types inside `shika-terminal` and process creation in core. Activity detection must not clear terminal paint state or block output draining.
- Prefer complete lifecycle boundaries over private screen formats, but verify the installed provider/version, documented launch flags, hook trust requirements, continuations, interruptions, and permission cancellation/resume. A partial hook must not mask stronger evidence or leave Working forever.
- Use session-local integrations. Do not silently install user/global hooks or overwrite repository configuration. Keep filesystem work off the UI thread and cleanup tied to session ownership.
- Add sanitized live-screen fixtures for exact provider layouts, with negative cases for prose, drafts, quoted examples, stale chrome, transcript viewers, wrap, and startup dialogs. Never solve a failing positive case by matching an unanchored word everywhere.
- Test notification and safe-close consequences whenever changing status inference. Timer continuity alone can hide a remaining false Ready transition.
- Update this guide, the short [AGENTS.md](../AGENTS.md) handoff, product/design decisions, and acceptance checks when behavior changes. Preserve the distinction between intended behavior, automated evidence, and manual acceptance.

Useful future work includes additional verified session-local lifecycle adapters, broader sanitized fixtures for provider versions, and explicit waiting events from Pi dialog extensions. Crash cleanup of temporary bridge files and better observation of provider-owned editor submissions are also separate improvements. Do not trade away timer/notification invariants to make one provider fixture appear more accurate.
