//! The Pi decoder over the Pi 1.4.0 RPC recordings. Each replay drives the
//! protocol through its own API where the recording's client wrote (send,
//! interrupt, answer) and checks every frame Shika writes against the
//! recorded one.
use std::collections::{HashSet, VecDeque};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::*;

const SESSION: &str = "22222222-2222-4333-8444-555555555555";
const MISSING: &str = "99999999-2222-4333-8444-555555555555";

fn launch(conversation: &str) -> ChatLaunch {
    ChatLaunch {
        cli: "pi",
        usage: None,
        usage_threads: None,
        program: PathBuf::from("pi"),
        cwd: PathBuf::from("/fixture/repo/.worktrees/shika-lead"),
        path: String::new(),
        env: Vec::new(),
        instructions: "Fixture instructions.".into(),
        notes: Vec::new(),
        conversation: conversation.into(),
        resumed: false,
        instructed: false,
        history: None,
        data: PathBuf::new(),
        project: "pi-test".into(),
    }
}

fn frame(bytes: &[u8]) -> Vec<Value> {
    assert!(bytes.ends_with(b"\n"), "{}", String::from_utf8_lossy(bytes));
    bytes
        .split(|b| *b == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice(line).unwrap())
        .collect()
}

fn line(v: Value) -> Vec<u8> {
    serde_json::to_vec(&v).unwrap()
}

fn state(session: &str) -> Vec<u8> {
    line(
        json!({"id": STATE, "type": "response", "command": "get_state", "success": true,
        "data": {"sessionId": session, "messageCount": 0}}),
    )
}

/// The number at the end of a recorded command id, such as `shika-prompt-3`.
fn number(id: &Value) -> u64 {
    id.as_str()
        .unwrap()
        .rsplit('-')
        .next()
        .unwrap()
        .parse()
        .unwrap()
}

/// A response with its `value` read as JSON when it is a JSON object.
fn json_value(response: &Value) -> Value {
    let mut response = response.clone();
    if let Some(all) = response["value"]
        .as_str()
        .and_then(|v| serde_json::from_str::<Value>(v).ok())
    {
        response["value"] = all;
    }
    response
}

/// One recording replayed through the protocol: every parsed result with
/// the record type it came from, in order, and Shika's frames.
struct Replay {
    pi: PiRpc,
    parsed: Vec<(String, Parsed)>,
    written: VecDeque<Value>,
    questions: Vec<ChatQuestion>,
    /// Dialogs Shika cancelled before its `abort`, which the recording's
    /// client cancelled after it.
    cancelled: HashSet<String>,
}

impl Replay {
    fn new() -> Self {
        Self {
            pi: PiRpc::new(),
            parsed: Vec::new(),
            written: VecDeque::new(),
            questions: Vec::new(),
            cancelled: HashSet::new(),
        }
    }
    fn write(&mut self, bytes: &[u8]) {
        if !bytes.is_empty() {
            self.written.extend(frame(bytes));
        }
    }
    fn record(&mut self, source: &str, parsed: Parsed) {
        match &parsed {
            Parsed::Reply(bytes) => self.write(&bytes.clone()),
            Parsed::Question(q) => self.questions.push(q.clone()),
            _ => {}
        }
        self.parsed.push((source.into(), parsed));
    }
    fn receive(&mut self, raw: &[u8], source: &str) {
        let parsed = self.pi.parse(raw);
        self.record(source, parsed);
        loop {
            match self.pi.poll(Instant::now()) {
                Parsed::Ignore => break,
                parsed => self.record(source, parsed),
            }
        }
    }
    /// Shika's frame for a command the recording's client wrote, checked
    /// against it. Commands Shika never sends are skipped: a second
    /// `get_state`, a prompt while Pi is busy, and `streamingBehavior`.
    fn send(&mut self, cmd: &Value) {
        match cmd["type"].as_str().unwrap() {
            "get_state" => return,
            "prompt" if cmd.get("streamingBehavior").is_some() || self.pi.turn != Turn::Idle => {
                return;
            }
            "prompt" => {
                self.pi.prompts = number(&cmd["id"]) - 1;
                let bytes = self.pi.send(cmd["message"].as_str().unwrap());
                self.write(&bytes);
            }
            "abort" => {
                self.pi.aborts = number(&cmd["id"]) - 1;
                let Interrupt::Frame(bytes) = self.pi.interrupt() else {
                    panic!("an open turn is aborted with a frame");
                };
                self.write(&bytes);
                while self.written.front().unwrap()["type"] == "extension_ui_response" {
                    let cancel = self.written.pop_front().unwrap();
                    assert_eq!(cancel["cancelled"], true);
                    self.cancelled.insert(cancel["id"].as_str().unwrap().into());
                }
            }
            "extension_ui_response" => {
                let id = cmd["id"].as_str().unwrap();
                if self.cancelled.remove(id) {
                    return;
                }
                let question = self
                    .questions
                    .iter()
                    .find(|q| q.request_id == id)
                    .expect("a shown dialog")
                    .clone();
                // The dialog's answers, keyed by question text. Shika's own
                // question tool is answered with all of them as JSON.
                let given = if let Some(value) = cmd.get("value") {
                    let all = value
                        .as_str()
                        .and_then(|v| serde_json::from_str::<Value>(v).ok())
                        .filter(Value::is_object);
                    if let Some(all) = all {
                        let mut input = question.input.clone();
                        input["answers"] = all;
                        let bytes = self.pi.answer(id, Some(input));
                        let ours = frame(&bytes).remove(0);
                        assert_eq!(json_value(&ours), json_value(cmd));
                        return;
                    }
                    Some(value.clone())
                } else {
                    cmd["confirmed"]
                        .as_bool()
                        .map(|yes| if yes { "Yes" } else { "No" }.into())
                };
                let answer = given.map(|given| {
                    let mut input = question.input.clone();
                    input["answers"] = json!({ question.questions[0].question.clone(): given });
                    input
                });
                let bytes = self.pi.answer(id, answer);
                self.write(&bytes);
            }
            other => panic!("unexpected command {other}"),
        }
        let ours = self
            .written
            .pop_front()
            .unwrap_or_else(|| panic!("Shika wrote nothing for {cmd}"));
        assert_eq!(ours, *cmd);
    }
    /// Replays `fixture` for a process on `conversation`. A recording that
    /// starts without `get_state` gets the matching response first.
    fn run(fixture: &str, conversation: &str) -> Self {
        let mut replay = Self::new();
        let bytes = replay.pi.handshake(&launch(conversation));
        replay.write(&bytes);
        if !fixture.starts_with(r#"{"fixture_stdin":{"id":"shika-state""#) {
            replay.written.clear();
            replay.receive(&state(conversation), "response");
        }
        for raw in fixture.lines() {
            let record: Value = serde_json::from_str(raw).unwrap();
            if let Some(cmd) = record.get("fixture_stdin") {
                if cmd["type"] == "get_state" && cmd["id"] == STATE {
                    let ours = replay.written.pop_front().expect("the handshake");
                    assert_eq!(ours, *cmd);
                    continue;
                }
                replay.send(cmd);
            } else {
                let source = record["type"].as_str().unwrap_or_default().to_string();
                replay.receive(raw.as_bytes(), &source);
            }
        }
        assert!(replay.written.is_empty(), "unsent: {:?}", replay.written);
        replay
    }
    fn finals(&self) -> Vec<Option<String>> {
        self.parsed
            .iter()
            .filter_map(|(_, p)| match p {
                Parsed::Final(text) => Some(text.clone()),
                _ => None,
            })
            .collect()
    }
    /// Every turn's end: finals, failures and handled sends, in order.
    fn outcomes(&self) -> Vec<&Parsed> {
        self.parsed
            .iter()
            .map(|(_, p)| p)
            .filter(|p| matches!(p, Parsed::Final(_) | Parsed::Failed(_) | Parsed::Handled))
            .collect()
    }
    fn count(&self, matches: impl Fn(&Parsed) -> bool) -> usize {
        self.parsed.iter().filter(|(_, p)| matches(p)).count()
    }
    /// Turns end only at Pi's settlement, or a handled prompt's response.
    fn settled_only_at_settlement(&self) {
        for (source, parsed) in &self.parsed {
            if matches!(parsed, Parsed::Final(_) | Parsed::Failed(_)) {
                assert!(
                    source == "agent_settled" || source == "response",
                    "{parsed:?} from {source}"
                );
            }
        }
    }
    fn shown(&self) -> String {
        format!("{:?}", self.parsed)
    }
}

const NORMAL: &str = include_str!("../../tests/fixtures/pi-1.4.0-normal-local.jsonl");

#[test]
fn normal_turns_publish_only_the_final_reply_after_the_last_tool() {
    let replay = Replay::run(NORMAL, SESSION);
    assert_eq!(
        replay.finals(),
        [
            Some("Final synthetic reply 1.\n\n```bash\ngit status\n```".into()),
            Some("Final synthetic reply 2.".into()),
        ]
    );
    replay.settled_only_at_settlement();
    // Each prompt acknowledged by Pi's response and confirmed by its user
    // message, which is when Pi writes the session file; none started by Pi.
    assert_eq!(replay.count(|p| *p == Parsed::Accepted), 2);
    assert_eq!(replay.count(|p| *p == Parsed::Confirmed), 2);
    assert_eq!(replay.count(|p| *p == Parsed::Started), 0);
    for private in ["must remain hidden", "tool-output", "Fixture instructions"] {
        assert!(!replay.shown().contains(private), "{private}");
    }
}

#[test]
fn failed_runs_settle_unaborted_and_fail_visibly_without_their_partial_text() {
    let replay = Replay::run(
        include_str!("../../tests/fixtures/pi-1.4.0-failure-local.jsonl"),
        SESSION,
    );
    // A 400 with no retry, then a 503 whose retries all failed. Both runs
    // settled with `aborted: false`; only the last assistant message says so.
    assert_eq!(
        replay.outcomes(),
        [&Parsed::Failed(REPLY_FAILED), &Parsed::Failed(REPLY_FAILED)]
    );
    replay.settled_only_at_settlement();
    assert!(!replay.shown().contains("Synthetic"));
}

#[test]
fn a_retry_or_compaction_after_agent_end_publishes_only_at_agent_settled() {
    let replay = Replay::run(
        include_str!("../../tests/fixtures/pi-1.4.0-recovery-local.jsonl"),
        SESSION,
    );
    assert_eq!(
        replay.finals(),
        [
            Some("Final synthetic reply before compaction.".into()),
            Some("Final synthetic reply after a retry.".into()),
        ]
    );
    replay.settled_only_at_settlement();
    // The cut stream's partial text is never a candidate.
    assert!(!replay.shown().contains("Partial draft"));
    // A retry's `agent_start` inside the run is not a new turn.
    assert_eq!(replay.count(|p| *p == Parsed::Started), 0);
}

#[test]
fn overflow_recovery_after_an_agent_end_without_retry_still_publishes_its_final() {
    let replay = Replay::run(
        include_str!("../../tests/fixtures/pi-1.4.0-overflow-local.jsonl"),
        SESSION,
    );
    assert_eq!(
        replay.outcomes(),
        [
            &Parsed::Final(Some("Final synthetic reply 1.".into())),
            &Parsed::Final(Some(
                "Final synthetic reply after overflow recovery.".into()
            )),
        ]
    );
    replay.settled_only_at_settlement();
    assert!(!replay.shown().contains("Narration before the overflow"));
    assert!(!replay.shown().contains("prompt is too long"));
}

#[test]
fn dialogs_map_to_the_question_dialog_and_answer_in_their_own_shape() {
    let replay = Replay::run(
        include_str!("../../tests/fixtures/pi-1.4.0-question-local.jsonl"),
        SESSION,
    );
    // Two `select`s (answered, then cancelled) and a `confirm`.
    let [select, again, confirm] = &replay.questions[..] else {
        panic!("three dialogs: {:?}", replay.questions);
    };
    assert_eq!(select.request_id, "fixture-ui-1");
    assert_eq!(select.questions.len(), 1);
    assert_eq!(select.questions[0].question, "Which synthetic option?");
    assert_eq!(
        select.questions[0]
            .options
            .iter()
            .map(|o| o.label.as_str())
            .collect::<Vec<_>>(),
        ["Alpha", "Beta"]
    );
    assert_eq!(again.request_id, "fixture-ui-2");
    assert_eq!(
        confirm.questions[0].question,
        "Proceed with the synthetic step?\n\nSynthetic detail line."
    );
    assert_eq!(
        confirm.questions[0]
            .options
            .iter()
            .map(|o| o.label.as_str())
            .collect::<Vec<_>>(),
        ["Yes", "No"]
    );
    assert_eq!(
        replay.finals(),
        [
            Some("Final synthetic reply after answer.".into()),
            Some("Final synthetic reply after cancel.".into()),
            Some("Final synthetic reply after confirm.".into()),
        ]
    );
    assert!(!replay.shown().contains("Let me ask"));
}

#[test]
fn shikas_question_tool_in_real_pi_shows_rich_questions_and_reads_their_answers() {
    // Recorded with `pi-lead.ts` itself loaded: answered, cancelled, and
    // stopped with the dialog open.
    let replay = Replay::run(
        include_str!("../../tests/fixtures/pi-1.4.0-lead-question-local.jsonl"),
        "33333333-2222-4333-8444-555555555555",
    );
    assert_eq!(replay.questions.len(), 3);
    for question in &replay.questions {
        assert_eq!(question.questions.len(), 2);
        let [base, checks] = &question.questions[..] else {
            unreachable!()
        };
        assert_eq!(base.question, "Which base?");
        assert_eq!(base.header, "Base");
        assert_eq!(base.options[0].label, "main");
        assert_eq!(base.options[0].description, "default");
        assert!(!base.multi_select);
        assert_eq!(checks.question, "Which checks?");
        assert!(checks.multi_select);
    }
    assert_eq!(
        replay.outcomes(),
        [
            &Parsed::Final(Some("Answered.".into())),
            &Parsed::Final(Some("Cancelled.".into())),
            &Parsed::Final(None),
            &Parsed::Final(Some("After abort.".into())),
        ]
    );
    assert_eq!(replay.count(|p| *p == Parsed::Blocked), 0);
    assert!(!replay.shown().contains("Answer: dev"));
}

#[test]
fn stop_cancels_an_open_dialog_before_abort_and_the_process_continues() {
    let replay = Replay::run(
        include_str!("../../tests/fixtures/pi-1.4.0-interrupt-local.jsonl"),
        SESSION,
    );
    // Mid-tool, with a dialog open (cancelled by Shika before the `abort`),
    // and while the provider streamed: each settles with no bubble.
    assert_eq!(
        replay.outcomes(),
        [
            &Parsed::Final(None),
            &Parsed::Final(Some("Final synthetic reply after interrupt.".into())),
            &Parsed::Final(None),
            &Parsed::Final(None),
            &Parsed::Final(Some("Final synthetic reply 4.".into())),
        ]
    );
    assert!(replay.cancelled.is_empty());
    assert!(!replay.shown().contains("Running a long synthetic command"));
}

#[test]
fn handled_prompts_settle_with_no_bubble_and_a_run_pi_starts_is_a_turn() {
    let replay = Replay::run(
        include_str!("../../tests/fixtures/pi-1.4.0-background-local.jsonl"),
        SESSION,
    );
    // `/fixture-handled` (a `notify` first, ignored), unknown slash text
    // that reaches the model, a run that also took a follow-up Shika never
    // sends (only the last reply shows), and `/fixture-later`, whose run
    // starts with no prompt of ours.
    assert_eq!(
        replay.outcomes(),
        [
            &Parsed::Handled,
            &Parsed::Final(Some("Reply to unknown slash text.".into())),
            &Parsed::Final(Some("Follow-up synthetic reply.".into())),
            &Parsed::Handled,
            &Parsed::Final(Some("Extension-started synthetic reply.".into())),
        ]
    );
    assert_eq!(replay.count(|p| *p == Parsed::Started), 1);
    assert_eq!(replay.count(|p| *p == Parsed::Blocked), 0);
}

#[test]
fn a_resume_and_a_missing_id_report_the_session_shika_passed() {
    let replay = Replay::run(
        include_str!("../../tests/fixtures/pi-1.4.0-resume-local.jsonl"),
        SESSION,
    );
    assert!(replay.pi.ready());
    assert_eq!(replay.finals(), [Some("Resumed synthetic reply.".into())]);
    // Pi creates a missing id silently: identity holds, a resume is unproven.
    let missing = include_str!("../../tests/fixtures/pi-1.4.0-resume-missing-local.jsonl");
    let replay = Replay::run(missing, MISSING);
    assert!(replay.pi.ready());
    assert!(replay.parsed.is_empty() || replay.count(|p| *p != Parsed::Ignore) == 0);
    let mut pi = PiRpc::new();
    pi.handshake(&launch(SESSION));
    let response = missing.lines().nth(1).unwrap().as_bytes();
    assert_eq!(
        pi.parse(response),
        Parsed::Failed(STRINGS.other_conversation)
    );
    assert!(!pi.ready());
}

/// Pi answers a saved id it cannot load with an empty session under that
/// id, so a resume is refused by its empty history, never by an exit.
#[test]
fn a_resume_that_comes_back_empty_is_refused() {
    let resumed = |conversation| ChatLaunch {
        resumed: true,
        ..launch(conversation)
    };
    let missing = include_str!("../../tests/fixtures/pi-1.4.0-resume-missing-local.jsonl");
    let mut pi = PiRpc::new();
    pi.handshake(&resumed(MISSING));
    pi.send("early");
    let response = missing.lines().nth(1).unwrap().as_bytes();
    assert_eq!(pi.parse(response), Parsed::Fatal(NOT_RESUMED));
    assert!(!pi.ready());
    assert_eq!(pi.poll(Instant::now()), Parsed::Ignore, "nothing is sent");

    let found = include_str!("../../tests/fixtures/pi-1.4.0-resume-local.jsonl");
    let mut pi = PiRpc::new();
    pi.handshake(&resumed(SESSION));
    assert_eq!(
        pi.parse(found.lines().nth(1).unwrap().as_bytes()),
        Parsed::Ignore
    );
    assert!(pi.ready());
    // A fresh Lead's empty session is expected, and so is a Pi that does
    // not report a count.
    let mut pi = PiRpc::new();
    pi.handshake(&launch(MISSING));
    assert_eq!(pi.parse(response), Parsed::Ignore);
    let mut pi = PiRpc::new();
    pi.handshake(&resumed(SESSION));
    let uncounted = line(
        json!({"id": STATE, "type": "response", "command": "get_state",
        "success": true, "data": {"sessionId": SESSION}}),
    );
    assert_eq!(pi.parse(&uncounted), Parsed::Ignore);
    assert!(pi.ready());
}

/// A fresh decoder past the handshake.
fn ready_pi() -> PiRpc {
    let mut pi = PiRpc::new();
    pi.handshake(&launch(SESSION));
    assert_eq!(pi.parse(&state(SESSION)), Parsed::Ignore);
    pi
}

fn prompt_response(id: &str, fields: Value) -> Vec<u8> {
    let mut v = json!({"id": id, "type": "response", "command": "prompt"});
    for (key, value) in fields.as_object().unwrap() {
        v[key] = value.clone();
    }
    line(v)
}

#[test]
fn a_send_before_the_handshake_waits_and_a_refused_prompt_fails() {
    let mut pi = PiRpc::new();
    pi.handshake(&launch(SESSION));
    assert!(pi.send("early").is_empty());
    assert!(!pi.ready());
    assert_eq!(pi.parse(&state(SESSION)), Parsed::Ignore);
    let Parsed::Reply(bytes) = pi.poll(Instant::now()) else {
        panic!("the waiting send goes out");
    };
    assert_eq!(
        frame(&bytes),
        [json!({"id": "shika-prompt-1", "type": "prompt", "message": "early"})]
    );
    let busy = json!({"success": false, "error": "Agent is already processing. Specify streamingBehavior ('steer' or 'followUp') to queue the message."});
    assert_eq!(
        pi.parse(&prompt_response("shika-prompt-1", busy.clone())),
        Parsed::Failed(BUSY)
    );
    assert_eq!(pi.turn, Turn::Idle);
    pi.send("again");
    assert_eq!(
        pi.parse(&prompt_response(
            "shika-prompt-2",
            json!({"success": false, "error": "No API key found"})
        )),
        Parsed::Failed(REPLY_FAILED)
    );
    // A run Pi started itself took the slot: our prompt fails, and that run
    // becomes the turn.
    pi.send("third");
    assert_eq!(pi.parse(br#"{"type":"agent_start"}"#), Parsed::Ignore);
    assert_eq!(
        pi.parse(&prompt_response("shika-prompt-3", busy)),
        Parsed::Failed(BUSY)
    );
    assert_eq!(pi.poll(Instant::now()), Parsed::Started);
    assert_eq!(
        pi.parse(br#"{"type":"agent_settled","aborted":false}"#),
        Parsed::Final(None)
    );
    // A response for another id, or the `abort` answer, is not ours.
    assert_eq!(
        pi.parse(&prompt_response("shika-prompt-9", json!({"success": true}))),
        Parsed::Ignore
    );
}

fn ask_started(id: &str, questions: Value) -> Vec<u8> {
    line(
        json!({"type": "tool_execution_start", "toolCallId": id, "toolName": QUESTION_TOOL,
        "args": {"questions": questions}}),
    )
}

fn ui(id: &str, method: &str, fields: Value) -> Vec<u8> {
    let mut v = json!({"type": "extension_ui_request", "id": id, "method": method});
    for (key, value) in fields.as_object().unwrap() {
        v[key] = value.clone();
    }
    line(v)
}

/// A decoder inside an accepted, running turn.
fn running_pi() -> PiRpc {
    let mut pi = ready_pi();
    pi.send("hi");
    assert_eq!(
        pi.parse(&prompt_response(
            "shika-prompt-1",
            json!({"success": true, "data": {"disposition": "started"}})
        )),
        Parsed::Accepted
    );
    assert_eq!(pi.parse(br#"{"type":"agent_start"}"#), Parsed::Ignore);
    pi
}

#[test]
fn shikas_question_tool_shows_its_questions_and_answers_them_as_json() {
    let mut pi = running_pi();
    let questions = json!([
        {"question": "Which base?", "header": "Base", "options": [
            {"label": "main", "description": "The default branch"}, {"label": "dev"}]},
        {"question": "Which checks?", "options": [{"label": "unit"}, {"label": "lint"}], "multiSelect": true},
        {"question": "Anything else?"},
    ]);
    assert_eq!(pi.parse(&ask_started("call-1", questions)), Parsed::Ignore);
    let Parsed::Question(question) = pi.parse(&ui(
        "ui-1",
        "input",
        json!({"title": "shika-question:call-1"}),
    )) else {
        panic!("the tool's questions are shown");
    };
    assert_eq!(question.request_id, "ui-1");
    assert_eq!(question.questions.len(), 3);
    assert_eq!(question.questions[0].header, "Base");
    assert_eq!(
        question.questions[0].options[0].description,
        "The default branch"
    );
    assert!(question.questions[1].multi_select);
    assert!(question.questions[2].options.is_empty());
    let mut input = question.input.clone();
    input["answers"] =
        json!({"Which base?": "dev", "Which checks?": ["unit", "lint"], "Anything else?": "No"});
    let reply = frame(&pi.answer("ui-1", Some(input)));
    assert_eq!(reply.len(), 1);
    assert_eq!(reply[0]["type"], "extension_ui_response");
    assert_eq!(reply[0]["id"], "ui-1");
    let value: Value = serde_json::from_str(reply[0]["value"].as_str().unwrap()).unwrap();
    assert_eq!(value["Which checks?"], json!(["unit", "lint"]));
    assert_eq!(value["Which base?"], "dev");

    // Cancel resolves the input to undefined.
    pi.parse(&ask_started("call-2", json!([{"question": "Q?"}])));
    assert!(matches!(
        pi.parse(&ui(
            "ui-2",
            "input",
            json!({"title": "shika-question:call-2"})
        )),
        Parsed::Question(_)
    ));
    assert_eq!(
        frame(&pi.answer("ui-2", None)),
        [json!({"type": "extension_ui_response", "id": "ui-2", "cancelled": true})]
    );
}

#[test]
fn dialogs_the_question_dialog_cannot_show_block_and_status_requests_are_ignored() {
    let mut pi = running_pi();
    for (i, (questions, why)) in [
        (json!([]), "no questions"),
        (
            json!([{"question": "1"}, {"question": "2"}, {"question": "3"}, {"question": "4"}, {"question": "5"}]),
            "five questions",
        ),
        (json!([{"question": "Q?"}, {"question": "Q?"}]), "same text"),
        (json!([{"question": " "}]), "empty question"),
        (
            json!([{"question": "Q?", "options": [{"label": "1"}, {"label": "2"}, {"label": "3"}, {"label": "4"}, {"label": "5"}]}]),
            "five options",
        ),
        (json!([{"question": "Q?", "options": ["a"]}]), "bare options"),
    ]
    .into_iter()
    .enumerate()
    {
        let call = format!("bad-{i}");
        pi.parse(&ask_started(&call, questions));
        let title = format!("shika-question:{call}");
        assert_eq!(
            pi.parse(&ui(&format!("b-{i}"), "input", json!({"title": title}))),
            Parsed::Blocked,
            "{why}"
        );
    }
    for (method, fields, why) in [
        (
            "input",
            json!({"title": "shika-question:no-such-call"}),
            "no tool",
        ),
        (
            "select",
            json!({"title": "Pick", "options": []}),
            "no options",
        ),
        (
            "select",
            json!({"title": "Pick", "options": ["1", "2", "3", "4", "5"]}),
            "five options",
        ),
        (
            "select",
            json!({"title": "Pick", "options": ["a", "a"]}),
            "same option",
        ),
        ("select", json!({"title": "", "options": ["a"]}), "no title"),
        (
            "editor",
            json!({"title": "Edit", "prefill": "line 1"}),
            "prefilled",
        ),
        ("someFutureDialog", json!({"title": "?"}), "unknown"),
    ] {
        assert_eq!(pi.parse(&ui("x", method, fields)), Parsed::Blocked, "{why}");
    }
    for method in [
        "notify",
        "setStatus",
        "setWidget",
        "setTitle",
        "set_editor_text",
    ] {
        assert_eq!(pi.parse(&ui("n", method, json!({}))), Parsed::Ignore);
    }
    // Stop cancels every blocked dialog too, or `abort` would never settle.
    let Interrupt::Frame(bytes) = pi.interrupt() else {
        panic!("an open turn is aborted with a frame");
    };
    let frames = frame(&bytes);
    assert_eq!(frames.len(), 14);
    assert!(frames[..13].iter().all(|f| f["cancelled"] == true));
    assert_eq!(frames[13], json!({"id": "shika-abort-1", "type": "abort"}));
}

#[test]
fn free_text_and_confirm_dialogs_answer_in_their_own_shape() {
    let mut pi = running_pi();
    let answered = |pi: &mut PiRpc, id: &str, method: &str, fields: Value, given: &str| {
        let Parsed::Question(question) = pi.parse(&ui(id, method, fields)) else {
            panic!("{method} is shown");
        };
        let mut input = question.input.clone();
        input["answers"] = json!({ question.questions[0].question.clone(): given });
        frame(&pi.answer(id, Some(input)))[0].clone()
    };
    let input = answered(
        &mut pi,
        "i",
        "input",
        json!({"title": "Name?", "placeholder": "x"}),
        "Ada",
    );
    assert_eq!(input["value"], "Ada");
    let editor = answered(
        &mut pi,
        "e",
        "editor",
        json!({"title": "Notes?"}),
        "Some notes",
    );
    assert_eq!(editor["value"], "Some notes");
    // A select answered with the author's own words passes them on.
    let select = answered(
        &mut pi,
        "s",
        "select",
        json!({"title": "Pick", "options": ["a", "b"]}),
        "neither",
    );
    assert_eq!(select["value"], "neither");
    let yes = answered(
        &mut pi,
        "c1",
        "confirm",
        json!({"title": "Go?", "message": ""}),
        "Yes",
    );
    assert_eq!(yes["confirmed"], true);
    let no = answered(&mut pi, "c2", "confirm", json!({"title": "Go?"}), "No");
    assert_eq!(no["confirmed"], false);
    let other = answered(
        &mut pi,
        "c3",
        "confirm",
        json!({"title": "Go?"}),
        "maybe later",
    );
    assert_eq!(other["confirmed"], false);
}

#[test]
fn dialogs_wait_their_turn_and_one_pi_resolved_at_its_timeout_closes() {
    let mut pi = running_pi();
    let first = ui("d1", "select", json!({"title": "First", "options": ["a"]}));
    let second = ui(
        "d2",
        "select",
        json!({"title": "Second", "options": ["b"], "timeout": 60000}),
    );
    let third = ui("d3", "input", json!({"title": "Third", "timeout": 1}));
    assert!(matches!(pi.parse(&first), Parsed::Question(q) if q.request_id == "d1"));
    assert_eq!(pi.parse(&second), Parsed::Ignore);
    assert_eq!(pi.parse(&third), Parsed::Ignore);
    // The third resolved itself while waiting; the second shows after the first.
    let later = Instant::now() + Duration::from_millis(50);
    assert_eq!(pi.poll(later), Parsed::Cancelled("d3".into()));
    assert_eq!(pi.poll(later), Parsed::Ignore);
    pi.answer("d1", None);
    assert!(matches!(pi.poll(Instant::now()), Parsed::Question(q) if q.request_id == "d2"));
    // A dialog still open when the run settles is dismissed, since the app
    // closes the Lead's dialogs at Ready.
    assert_eq!(
        pi.parse(br#"{"type":"agent_settled","aborted":false}"#),
        Parsed::Final(None)
    );
    let Parsed::Reply(bytes) = pi.poll(Instant::now()) else {
        panic!("the leftover dialog is cancelled");
    };
    assert_eq!(
        frame(&bytes),
        [json!({"type": "extension_ui_response", "id": "d2", "cancelled": true})]
    );
}

#[test]
fn stop_generating_matches_what_pi_has_of_the_turn() {
    let mut pi = PiRpc::new();
    pi.handshake(&launch(SESSION));
    assert!(matches!(pi.interrupt(), Interrupt::Idle));
    // Not sent yet: nothing to stop, and it never goes out.
    pi.send("early");
    assert!(matches!(pi.interrupt(), Interrupt::Idle));
    pi.parse(&state(SESSION));
    assert_eq!(pi.poll(Instant::now()), Parsed::Ignore);
    // Written, but Pi has not taken it yet.
    assert!(!pi.send("now").is_empty());
    assert!(matches!(pi.interrupt(), Interrupt::Signal));
    let mut pi = running_pi();
    let Interrupt::Frame(bytes) = pi.interrupt() else {
        panic!("an open turn is aborted with a frame");
    };
    assert_eq!(
        frame(&bytes),
        [json!({"id": "shika-abort-1", "type": "abort"})]
    );
    // A reply that ended before the abort took effect still shows nothing.
    pi.parse(
        br#"{"type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"late"}],"stopReason":"stop"}}"#,
    );
    assert_eq!(
        pi.parse(br#"{"type":"agent_settled","aborted":false}"#),
        Parsed::Final(None)
    );
    assert_eq!(pi.stop_signal(), libc::SIGTERM);
    assert!(matches!(pi.shutdown(), Shutdown::CloseStdin));
}

#[test]
fn unusual_endings_fail_visibly() {
    let end = |stop: &str| {
        let mut pi = running_pi();
        pi.parse(&line(
            json!({"type": "message_end", "message": {"role": "assistant",
            "content": [{"type": "text", "text": "private"}], "stopReason": stop}}),
        ));
        pi.parse(br#"{"type":"agent_settled","aborted":false}"#)
    };
    assert_eq!(end("length"), Parsed::Failed(CUT_OFF));
    assert_eq!(end("error"), Parsed::Failed(REPLY_FAILED));
    assert_eq!(end("somethingNew"), Parsed::Failed(BAD_STREAM));
    assert_eq!(end("stop"), Parsed::Final(Some("private".into())));
    // A tool after the reply drops it.
    let mut pi = running_pi();
    pi.parse(&line(
        json!({"type": "message_end", "message": {"role": "assistant",
        "content": [{"type": "text", "text": "narration"}], "stopReason": "stop"}}),
    ));
    pi.parse(&line(
        json!({"type": "tool_execution_start", "toolCallId": "c", "toolName": "bash", "args": {}}),
    ));
    assert_eq!(
        pi.parse(br#"{"type":"agent_settled","aborted":false}"#),
        Parsed::Final(None)
    );
}

#[test]
fn garbage_huge_ignored_records_and_the_oversized_agent_end() {
    let mut pi = ready_pi();
    assert_eq!(pi.parse(b"garbage"), Parsed::Failed(BAD_STREAM));
    assert_eq!(pi.parse(br#"{"no":"type"}"#), Parsed::Failed(BAD_STREAM));
    assert_eq!(
        pi.parse(br#"{"type":"response","command":"parse","success":false,"error":"x"}"#),
        Parsed::Failed(BAD_STREAM)
    );
    // As Pi writes it: `type` first.
    let huge = format!(
        r#"{{"type":"agent_end","messages":[{{"role":"user","content":"{}"}}]}}"#,
        "x".repeat(4 << 20)
    )
    .into_bytes();
    assert_eq!(pi.parse(&huge), Parsed::Ignore);
    let skip = pi.oversized_skippable();
    assert!(skip(&huge[..64]));
    for other in [
        &br#"{"type":"message_end","message":{}}"#[..],
        br#"{"messages":[],"type":"agent_end"}"#,
        b"garbage",
    ] {
        assert!(!skip(other), "{}", String::from_utf8_lossy(other));
    }
}

#[test]
fn launch_arguments_add_rpc_mode_and_the_question_extension_to_the_lead_flags() {
    assert_eq!(
        PiRpc::new().args(&launch(SESSION)),
        [
            "--mode",
            "rpc",
            "--approve",
            "--session-id",
            SESSION,
            "--append-system-prompt",
            "Fixture instructions.",
        ]
    );
    let pi = PiRpc::installed().unwrap();
    let args = pi.args(&launch(SESSION));
    assert_eq!(args[3], "--extension");
    let path = PathBuf::from(&args[4]);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), EXTENSION);
    assert!(EXTENSION.contains(QUESTION_TOOL) && EXTENSION.contains(QUESTION_TITLE));
    {
        use std::os::unix::fs::PermissionsExt;
        let dir = path.parent().unwrap();
        let mode = std::fs::metadata(dir).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
    }
    // A new process keeps the file; the transport's end removes it.
    let mut pi = pi;
    pi.handshake(&launch(SESSION));
    assert!(path.is_file());
    drop(pi);
    assert!(!path.parent().unwrap().exists());
}
