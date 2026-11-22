//! The ACP client with the Cursor flavor over the Cursor CLI 2026.11.19
//! recordings. Each replay drives the protocol through its own API where the
//! recording's client wrote (send, interrupt, answer) and checks that every
//! frame Shika writes matches the recorded one. Cases the recordings could
//! not reach (a question, the rarer stop reasons, a turn's JSON-RPC error)
//! use frames of the shapes read from the installed bundle.
use std::collections::VecDeque;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use serde_json::{Value, json};

use super::*;

const SESSION: &str = "00000000-0000-4000-8000-000000000001";
const INSTRUCTIONS: &str = "FIXTURE LEAD INSTRUCTIONS";

/// A folder for the Lead worktree, reserved with `create_dir`, removed on
/// drop. The rule is written here while `session/new` runs.
struct Worktree(PathBuf);
impl Worktree {
    fn new() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        loop {
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "shika-acp-test-{}-{}-{n}",
                std::process::id(),
                crate::control::new_token()
            ));
            match fs::create_dir(&path) {
                Ok(()) => return Self(path),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => panic!("could not reserve a worktree: {e}"),
            }
        }
    }
    fn rule(&self) -> PathBuf {
        self.0.join(super::super::cursor::CURSOR_RULE)
    }
}
impl Drop for Worktree {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn launch(cwd: &Worktree, conversation: &str, instructed: bool, notes: &[&str]) -> ChatLaunch {
    ChatLaunch {
        cli: "cursor",
        usage: None,
        usage_threads: None,
        program: PathBuf::from("agent"),
        cwd: cwd.0.clone(),
        path: String::new(),
        env: Vec::new(),
        instructions: INSTRUCTIONS.into(),
        model: None,
        notes: notes.iter().map(|n| n.to_string()).collect(),
        conversation: conversation.into(),
        resumed: !conversation.is_empty(),
        instructed,
        history: None,
        data: PathBuf::new(),
        project: "acp-test".into(),
    }
}

fn frames(bytes: &[u8]) -> Vec<Value> {
    assert!(bytes.ends_with(b"\n"), "{}", String::from_utf8_lossy(bytes));
    bytes
        .split(|b| *b == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice(line).unwrap())
        .collect()
}

/// A client frame comparable across Shika and the recording's probe: the
/// probe's own name and version, its fixture paths, and the disposable MCP
/// server two recordings added are not Shika's.
fn normalized(mut v: Value) -> Value {
    if v["method"] == "initialize" {
        v["params"].as_object_mut().unwrap().remove("clientInfo");
    }
    if v["params"]["cwd"].is_string() {
        v["params"]["cwd"] = "/fixture/repo".into();
    }
    if v["params"]["mcpServers"].is_array() {
        v["params"]["mcpServers"] = json!([]);
    }
    v
}

/// One recording replayed through the protocol: every parsed result in
/// order (queued results included), and Shika's frames.
struct Replay {
    acp: AcpClient,
    parsed: Vec<Parsed>,
    written: VecDeque<Value>,
    question: Option<ChatQuestion>,
    /// The rule's text while `session/new` was in flight.
    rule_during_new: Option<String>,
}

impl Replay {
    fn new() -> Self {
        Self {
            acp: super::super::cursor::client(),
            parsed: Vec::new(),
            written: VecDeque::new(),
            question: None,
            rule_during_new: None,
        }
    }
    fn write(&mut self, bytes: &[u8]) {
        if !bytes.is_empty() {
            self.written.extend(frames(bytes));
        }
    }
    fn record(&mut self, parsed: Parsed) {
        match &parsed {
            Parsed::Reply(bytes) => self.write(&bytes.clone()),
            Parsed::Question(q) => self.question = Some(q.clone()),
            _ => {}
        }
        self.parsed.push(parsed);
    }
    fn receive(&mut self, frame: &Value) {
        let parsed = self.acp.parse(&serde_json::to_vec(frame).unwrap());
        self.record(parsed);
        self.drain();
    }
    fn drain(&mut self) {
        loop {
            match self.acp.poll(Instant::now()) {
                Parsed::Ignore => break,
                parsed => self.record(parsed),
            }
        }
    }
    fn handshake(&mut self, launch: &ChatLaunch) {
        self.written.clear();
        let bytes = self.acp.handshake(launch);
        self.write(&bytes);
    }
    /// Produces Shika's frame for one the recording's client wrote and
    /// checks the two match. Shika never answers a permission request, and
    /// never lists sessions.
    fn send(&mut self, frame: &Value, cwd: &Worktree) {
        match frame["method"].as_str() {
            Some("session/prompt") => {
                let text = frame["params"]["prompt"][0]["text"].as_str().unwrap();
                let bytes = self.acp.send(text);
                self.write(&bytes);
            }
            Some("session/cancel") => match self.acp.interrupt() {
                Interrupt::Frame(bytes) => self.write(&bytes),
                _ => panic!("a running turn is interrupted with a frame"),
            },
            Some("session/new") => {
                self.rule_during_new = fs::read_to_string(cwd.rule()).ok();
            }
            Some("session/list") => return,
            None => {
                assert!(
                    self.written.is_empty(),
                    "Shika answered: {:?}",
                    self.written
                );
                return;
            }
            _ => {}
        }
        let ours = self
            .written
            .pop_front()
            .unwrap_or_else(|| panic!("Shika wrote nothing for {frame}"));
        assert_eq!(normalized(ours), normalized(frame.clone()));
    }
    /// Replays `fixture` until a line containing `stop`, starting each
    /// process (each recorded `initialize`) with `launches` in order.
    fn run(fixture: &str, stop: Option<&str>, cwd: &Worktree, launches: &[ChatLaunch]) -> Self {
        let mut replay = Self::new();
        let mut launches = launches.iter();
        for line in fixture.lines() {
            if stop.is_some_and(|stop| line.contains(stop)) {
                break;
            }
            let record: Value = serde_json::from_str(line).unwrap();
            let frame = &record["frame"];
            match record["from"].as_str().unwrap() {
                "shika" => {
                    if frame["method"] == "initialize" {
                        replay.handshake(launches.next().expect("a launch per process"));
                    }
                    replay.send(frame, cwd);
                }
                "agent" => replay.receive(frame),
                _ => {}
            }
        }
        assert!(replay.written.is_empty(), "unsent: {:?}", replay.written);
        replay
    }
    fn finals(&self) -> Vec<Option<String>> {
        self.parsed
            .iter()
            .filter_map(|p| match p {
                Parsed::Final(text) => Some(text.clone()),
                _ => None,
            })
            .collect()
    }
    fn count(&self, matches: impl Fn(&Parsed) -> bool) -> usize {
        self.parsed.iter().filter(|p| matches(p)).count()
    }
}

fn fresh(cwd: &Worktree) -> ChatLaunch {
    launch(cwd, "", false, &[])
}

fn fixture(name: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(format!("cursor-2026.11.19-{name}-local.jsonl"));
    fs::read_to_string(path).unwrap()
}

#[test]
fn normal_turns_publish_only_the_text_after_the_last_tool_with_the_rule_in_place_for_new() {
    let cwd = Worktree::new();
    let replay = Replay::run(&fixture("normal"), None, &cwd, &[fresh(&cwd)]);
    assert_eq!(
        replay.finals(),
        [
            Some("**Folder contents** (`ls -la`):\n\n- `.cursor/` (directory)\n- `.git/` (directory)\n- `calc.py`\n- `README.md`\n\n**One sentence:** `calc.py` defines two functions, `add` and `sub`, that return the sum and difference of two numbers.\n\nRULE-A-PINEAPPLE".into()),
            Some("The latest commit subject is **init** (`95f6704 init`).\n\nRULE-A-PINEAPPLE".into()),
        ]
    );
    assert!(!format!("{:?}", replay.finals()).contains("Listing the folder"));
    // The session reports its id, holds the instructions from its creation,
    // and is confirmed (saved) at each turn's first output.
    assert_eq!(replay.count(|p| *p == Parsed::Identity(SESSION.into())), 1);
    assert_eq!(replay.count(|p| *p == Parsed::Instructed), 1);
    assert_eq!(replay.count(|p| *p == Parsed::Confirmed), 2);
    assert_eq!(replay.count(|p| matches!(p, Parsed::Blocked)), 0);
    let rule = replay
        .rule_during_new
        .expect("the rule while session/new ran");
    assert!(
        rule.starts_with("---\ndescription: Shika Lead instructions."),
        "{rule}"
    );
    assert!(rule.contains("alwaysApply: true"));
    assert!(rule.contains(INSTRUCTIONS));
    assert!(!cwd.rule().exists());
    assert!(
        !cwd.0.join(".cursor").exists(),
        "the folders Shika made go too"
    );
}

#[test]
fn tools_and_a_background_command_end_with_the_final_after_them() {
    let cwd = Worktree::new();
    let replay = Replay::run(&fixture("tools"), None, &cwd, &[fresh(&cwd)]);
    assert_eq!(
        replay.finals(),
        [Some("All three steps finished: shell echoed `probe-shell`, `sub` was added to `calc.py`, and `probe_echo` returned hello.".into())]
    );
    let replay = Replay::run(&fixture("background"), None, &cwd, &[fresh(&cwd)]);
    assert_eq!(
        replay.finals(),
        [Some(
            "Background command started; it will print `bg-done` in about 8 seconds.".into()
        )]
    );
    assert_eq!(replay.count(|p| matches!(p, Parsed::Started)), 0);
}

#[test]
fn a_permission_request_blocks_and_is_never_answered() {
    let cwd = Worktree::new();
    // `Replay::send` fails if Shika writes anything for the two answers the
    // recording's probe gave.
    let replay = Replay::run(&fixture("permission"), None, &cwd, &[fresh(&cwd)]);
    assert_eq!(replay.count(|p| matches!(p, Parsed::Blocked)), 2);
    assert_eq!(replay.count(|p| matches!(p, Parsed::Question(_))), 0);
    assert_eq!(
        replay.finals(),
        [Some("I added `sub` to `calc.py`, but the shell echo and `probe_echo` calls were rejected and did not run.".into())]
    );
}

#[test]
fn a_cancelled_turn_settles_with_no_bubble_and_the_session_continues() {
    let cwd = Worktree::new();
    let replay = Replay::run(&fixture("interrupt"), None, &cwd, &[fresh(&cwd)]);
    assert_eq!(
        replay.finals(),
        [
            None,
            Some("No - it was interrupted before it completed, so `slept` never printed.".into())
        ]
    );
}

#[test]
fn a_load_swallows_the_replay_and_a_missing_session_is_reported_missing() {
    let cwd = Worktree::new();
    let replay = Replay::run(
        &fixture("resume"),
        Some(r#""sessionId": "00000000-0000-4000-8000-000000000000""#),
        &cwd,
        &[
            launch(&cwd, SESSION, true, &[]),
            launch(&cwd, "00000000-0000-4000-8000-000000000002", true, &[]),
        ],
    );
    // The replayed turns produced nothing: one final, for the new prompt.
    assert_eq!(replay.finals(), [Some("Hello!\n\nRULE-A-PINEAPPLE".into())]);
    assert_eq!(replay.count(|p| *p == Parsed::Confirmed), 1);
    assert_eq!(replay.count(|p| matches!(p, Parsed::Identity(_))), 0);
    assert!(!format!("{:?}", replay.parsed).contains("Listing the folder"));
    assert!(!cwd.0.join(".cursor").exists(), "no rule around a load");
    assert_eq!(
        replay.parsed.last(),
        Some(&Parsed::Missing(
            "Cursor CLI could not resume this conversation"
        ))
    );
}

#[test]
fn a_session_the_backend_cannot_open_fails_the_start_with_cursors_reason() {
    let cwd = Worktree::new();
    let replay = Replay::run(
        &fixture("failure"),
        Some(r#""method": "session/prompt""#),
        &cwd,
        &[fresh(&cwd)],
    );
    assert_eq!(
        replay.parsed.last(),
        Some(&Parsed::Explained {
            text: "Could not start Cursor CLI (Failed to initialize session services)".into(),
            fatal: true,
        })
    );
    assert!(!cwd.rule().exists(), "the rule goes when session/new fails");
}

/// A client with an open session, its handshake frames read.
fn opened(cwd: &Worktree) -> Replay {
    let mut replay = Replay::new();
    replay.handshake(&fresh(cwd));
    replay.receive(&json!({"jsonrpc": "2.0", "id": 1, "result": {"protocolVersion": 1, "agentCapabilities": {"loadSession": true}}}));
    replay.receive(&json!({"jsonrpc": "2.0", "id": 2, "result": {"sessionId": SESSION}}));
    replay.written.clear();
    replay.parsed.clear();
    replay
}

fn update(kind: &str, extra: Value) -> Value {
    let mut update = json!({"sessionUpdate": kind});
    update
        .as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    json!({"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": SESSION, "update": update}})
}

fn chunk(text: &str) -> Value {
    update(
        "agent_message_chunk",
        json!({"content": {"type": "text", "text": text}}),
    )
}

#[test]
fn every_stop_reason_and_a_turn_error_settle_as_shown() {
    let cwd = Worktree::new();
    let mut replay = opened(&cwd);
    let mut id = 3;
    let mut turn = |replay: &mut Replay, answer: Value| -> Parsed {
        replay.acp.send("hi");
        replay.receive(&chunk("Partial"));
        let mut frame = json!({"jsonrpc": "2.0", "id": id});
        frame
            .as_object_mut()
            .unwrap()
            .extend(answer.as_object().unwrap().clone());
        id += 1;
        let parsed = replay.acp.parse(&serde_json::to_vec(&frame).unwrap());
        replay.drain();
        parsed
    };
    let result = |reason: &str| json!({"result": {"stopReason": reason}});
    assert_eq!(
        turn(&mut replay, result("end_turn")),
        Parsed::Final(Some("Partial".into()))
    );
    assert_eq!(turn(&mut replay, result("cancelled")), Parsed::Final(None));
    assert_eq!(
        turn(&mut replay, result("max_tokens")),
        Parsed::Failed("Cursor CLI stopped the reply at the model's output limit.")
    );
    assert_eq!(
        turn(&mut replay, result("max_turn_requests")),
        Parsed::Failed("Cursor CLI stopped the reply after too many steps.")
    );
    assert_eq!(
        turn(&mut replay, result("refusal")),
        Parsed::Failed("Cursor CLI's model refused to continue the reply.")
    );
    assert_eq!(
        turn(&mut replay, result("something_new")),
        Parsed::Failed("This Cursor CLI version could not be read")
    );
    // The shape of the recorded prompt failure, with a newline to bound.
    assert_eq!(
        turn(
            &mut replay,
            json!({"error": {"code": -32603, "message": "Internal error", "data": {"details": "Usage limit\nreached"}}})
        ),
        Parsed::Explained {
            text: "Cursor CLI could not complete the reply. Check its sign-in and connection, then retry. (Usage limit reached)".into(),
            fatal: false,
        }
    );
    // Thoughts, plans, titles and commands never reach the bubble; a tool
    // drops the text before it, and a blank reply publishes nothing.
    replay.acp.send("again");
    for frame in [
        chunk("narration"),
        update(
            "tool_call",
            json!({"toolCallId": "call-1\nfc_1_0", "status": "pending"}),
        ),
        update(
            "agent_thought_chunk",
            json!({"content": {"type": "text", "text": "private thought"}}),
        ),
        update("plan", json!({"entries": []})),
        update("session_info_update", json!({"title": "private title"})),
        chunk("  \n"),
    ] {
        replay.receive(&frame);
    }
    let parsed = replay
        .acp
        .parse(br#"{"jsonrpc":"2.0","id":10,"result":{"stopReason":"end_turn"}}"#);
    assert_eq!(parsed, Parsed::Final(None));
    assert_eq!(
        replay.acp.parse(b"not json"),
        Parsed::Failed("This Cursor CLI version could not be read")
    );
}

/// Sends `text` and returns the prompt request's id.
fn prompt(replay: &mut Replay, text: &str) -> u64 {
    let bytes = replay.acp.send(text);
    frames(&bytes)[0]["id"].as_u64().unwrap()
}

fn end_turn(replay: &mut Replay, id: u64) -> Parsed {
    let frame = json!({"jsonrpc": "2.0", "id": id, "result": {"stopReason": "end_turn"}});
    let parsed = replay.acp.parse(&serde_json::to_vec(&frame).unwrap());
    replay.drain();
    parsed
}

/// The shape `ask-question-handler.ts` sends: no session id in params.
fn ask(id: u64) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": "cursor/ask_question", "params": {
        "toolCallId": "call-7\nfc_7_0",
        "title": "Base",
        "questions": [
            {"id": "q1", "prompt": "Which base?", "allowMultiple": false,
             "options": [{"id": "o1", "label": "main"}, {"id": "o2", "label": "dev"}]},
            {"id": "q2", "prompt": "Which checks?", "allowMultiple": true,
             "options": [{"id": "c1", "label": "lint"}, {"id": "c2", "label": "test"}]}
        ]
    }})
}

/// Opens a turn, receives `ask(id)`, answers the dialog with `answers` (or
/// cancels it), ends the turn, and returns the result Shika wrote.
fn answered_with(replay: &mut Replay, id: u64, answers: Option<Value>) -> Value {
    let turn = prompt(replay, "ask me");
    replay.question = None;
    replay.receive(&ask(id));
    let question = replay.question.take().expect("the dialog");
    assert_eq!(question.request_id, id.to_string());
    assert_eq!(question.questions.len(), 2);
    assert_eq!(question.questions[0].question, "Which base?");
    assert_eq!(question.questions[1].options[1].label, "test");
    assert!(!question.questions[0].multi_select);
    assert!(question.questions[1].multi_select);
    let input = answers.map(|answers| {
        let mut input = question.input.clone();
        input["answers"] = answers;
        input
    });
    let bytes = replay.acp.answer(&question.request_id, input);
    let frame = frames(&bytes).remove(0);
    assert_eq!(frame["id"], id);
    assert_eq!(frame["jsonrpc"], "2.0");
    end_turn(replay, turn);
    frame["result"].clone()
}

#[test]
fn a_question_maps_to_the_dialog_and_answers_with_option_ids_words_or_cancel() {
    let cwd = Worktree::new();
    let mut replay = opened(&cwd);
    // Outside a turn nothing could take an answer: blocked.
    replay.receive(&ask(40));
    assert_eq!(replay.parsed.last(), Some(&Parsed::Blocked));
    assert_eq!(
        answered_with(
            &mut replay,
            41,
            Some(json!({"Which base?": "dev", "Which checks?": ["lint", "test"]}))
        ),
        json!({"outcome": {"outcome": "answered", "answers": [
            {"questionId": "q1", "selectedOptionIds": ["o2"]},
            {"questionId": "q2", "selectedOptionIds": ["c1", "c2"]}
        ]}})
    );
    assert_eq!(
        answered_with(
            &mut replay,
            42,
            Some(json!({"Which base?": "release-2", "Which checks?": "lint"}))
        ),
        json!({"outcome": {"outcome": "skipped",
            "reason": "The author answered in their own words. Which base?: release-2"}})
    );
    assert_eq!(
        answered_with(&mut replay, 43, None),
        json!({"outcome": {"outcome": "cancelled"}})
    );
    // More than 4 options, or labels it could not map back: blocked.
    prompt(&mut replay, "ask badly");
    let mut bad = ask(44);
    bad["params"]["questions"][0]["options"] = json!([
        {"id": "a", "label": "same"}, {"id": "b", "label": "same"}
    ]);
    replay.receive(&bad);
    assert_eq!(replay.parsed.last(), Some(&Parsed::Blocked));
}

#[test]
fn reports_are_acknowledged_and_stop_cancels_whatever_was_never_answered() {
    let cwd = Worktree::new();
    let mut replay = opened(&cwd);
    prompt(&mut replay, "work");
    replay.receive(&json!({"jsonrpc": "2.0", "id": 50, "method": "cursor/update_todos", "params": {"todos": []}}));
    assert_eq!(
        replay.written.pop_front(),
        Some(json!({"jsonrpc": "2.0", "id": 50, "result": {}}))
    );
    // As a notification there is nothing to answer.
    replay.receive(&json!({"jsonrpc": "2.0", "method": "cursor/task", "params": {}}));
    assert_eq!(replay.parsed.last(), Some(&Parsed::Ignore));
    assert!(replay.written.is_empty());
    for (id, method) in [
        (51, "session/request_permission"),
        (52, "cursor/create_plan"),
        (53, "cursor/something_new"),
        (54, "fs/read_text_file"),
    ] {
        replay.receive(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": {"sessionId": SESSION}}));
        assert_eq!(replay.parsed.last(), Some(&Parsed::Blocked), "{method}");
    }
    replay.receive(&ask(55));
    assert!(matches!(replay.parsed.last(), Some(Parsed::Question(_))));
    assert!(
        replay.written.is_empty(),
        "nothing is answered for the author"
    );
    let Interrupt::Frame(bytes) = replay.acp.interrupt() else {
        panic!("a running turn is interrupted with a frame");
    };
    assert_eq!(
        frames(&bytes),
        [
            json!({"jsonrpc": "2.0", "id": 55, "result": {"outcome": {"outcome": "cancelled"}}}),
            json!({"jsonrpc": "2.0", "id": 51, "result": {"outcome": {"outcome": "cancelled"}}}),
            json!({"jsonrpc": "2.0", "id": 52, "result": {"outcome": {"outcome": "cancelled"}}}),
            json!({"jsonrpc": "2.0", "id": 53, "error": {"code": -32800, "message": "Cancelled"}}),
            json!({"jsonrpc": "2.0", "id": 54, "error": {"code": -32800, "message": "Cancelled"}}),
            json!({"jsonrpc": "2.0", "method": "session/cancel", "params": {"sessionId": SESSION}}),
        ]
    );
    // Even an `end_turn` that raced the cancel settles as stopped.
    replay.receive(&chunk("late text"));
    assert_eq!(end_turn(&mut replay, 3), Parsed::Final(None));
}

#[test]
fn a_send_before_the_session_opens_is_written_and_accepted_once_it_does() {
    let cwd = Worktree::new();
    let mut replay = Replay::new();
    replay.handshake(&fresh(&cwd));
    assert!(!replay.acp.ready());
    assert!(replay.acp.send("early").is_empty());
    assert!(!replay.acp.write_accepts());
    replay.receive(&json!({"jsonrpc": "2.0", "id": 1, "result": {"protocolVersion": 1, "agentCapabilities": {}}}));
    assert!(!replay.acp.ready(), "session/new is a handshake step");
    replay.receive(&json!({"jsonrpc": "2.0", "id": 2, "result": {"sessionId": SESSION}}));
    assert!(replay.acp.ready());
    let sent = replay.written.pop_back().unwrap();
    assert_eq!(sent["method"], "session/prompt");
    assert_eq!(
        sent["params"]["prompt"],
        json!([{"type": "text", "text": "early"}])
    );
    assert_eq!(
        replay.parsed[replay.parsed.len() - 4..],
        [
            Parsed::Identity(SESSION.into()),
            Parsed::Instructed,
            Parsed::Reply(
                serde_json::to_vec(&sent)
                    .map(|mut b| {
                        b.push(b'\n');
                        b
                    })
                    .unwrap()
            ),
            Parsed::Accepted,
        ]
    );
    // An agent speaking another protocol version is unreadable.
    let mut other = Replay::new();
    other.handshake(&fresh(&cwd));
    other.receive(&json!({"jsonrpc": "2.0", "id": 1, "result": {"protocolVersion": 2}}));
    assert_eq!(
        other.parsed.last(),
        Some(&Parsed::Failed("This Cursor CLI version could not be read"))
    );
    assert!(!cwd.rule().exists());
}

#[test]
fn cursor_model_override_waits_for_acknowledgment_before_prompting() {
    let cwd = Worktree::new();
    let mut launch = fresh(&cwd);
    launch.model = Some("test-model".into());
    for accepted in [true, false] {
        let mut replay = Replay::new();
        replay.handshake(&launch);
        replay.receive(&json!({"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1}}));
        assert!(replay.acp.send("early").is_empty());
        replay.written.clear();
        replay.receive(&json!({"jsonrpc":"2.0","id":2,"result":{"sessionId":SESSION,
            "models":{"currentModelId":"default","availableModels":[{"modelId":"test-model[fast=false]"}]}}}));
        assert!(!replay.acp.ready());
        assert_eq!(
            replay.written.pop_front().unwrap(),
            json!({"jsonrpc":"2.0","id":3,
            "method":"session/set_config_option", "params":{"sessionId":SESSION,
            "configId":"model","value":"test-model[fast=false]"}})
        );
        assert!(replay.written.is_empty());
        let current = if accepted {
            "test-model[fast=false]"
        } else {
            "default"
        };
        replay.receive(&json!({"jsonrpc":"2.0","id":3,"result":{"configOptions":[{"id":"model","currentValue":current}]}}));
        if accepted {
            assert!(replay.acp.ready());
            assert_eq!(
                replay.written.pop_front().unwrap()["method"],
                "session/prompt"
            );
            assert!(replay.parsed.contains(&Parsed::Model(current.into())));
        } else {
            assert_eq!(replay.parsed.last(), Some(&model_unavailable()));
            assert!(replay.written.is_empty());
        }
    }
}

/// A resume whose notes may have changed sends them as one `[shika]` line:
/// a Shika turn of its own when nothing waits, or ahead of a send that
/// waited for the load. It counts as delivered at the turn's first output.
#[test]
fn a_resumed_session_without_the_current_notes_gets_one_memory_line() {
    let cwd = Worktree::new();
    let load = |replay: &mut Replay, early: Option<&str>| {
        replay.handshake(&launch(&cwd, SESSION, false, &["Keep this note"]));
        if let Some(text) = early {
            replay.acp.send(text);
        }
        replay.receive(&json!({"jsonrpc": "2.0", "id": 1, "result": {"protocolVersion": 1, "agentCapabilities": {"loadSession": true}}}));
        assert!(replay.acp.ready(), "a long replay is not a slow handshake");
        replay.receive(&chunk("replayed history"));
        replay.receive(&json!({"jsonrpc": "2.0", "id": 2, "result": {}}));
    };

    let mut replay = Replay::new();
    load(&mut replay, None);
    let sent = replay.written.pop_back().unwrap();
    assert_eq!(
        sent["params"]["prompt"],
        json!([{"type": "text", "text": "[shika] These are your memory notes now, in place of any earlier list:\n\n- Keep this note"}])
    );
    let tail = &replay.parsed[replay.parsed.len() - 2..];
    assert_eq!(tail[0], Parsed::ShikaInput(super::MEMORY_ROW));
    assert!(matches!(tail[1], Parsed::Reply(_)));
    assert!(!replay.parsed.contains(&Parsed::Accepted));
    replay.parsed.clear();
    replay.receive(&chunk("Noted."));
    assert_eq!(replay.parsed, [Parsed::Confirmed, Parsed::Instructed]);
    assert_eq!(
        end_turn(&mut replay, 3),
        Parsed::Final(Some("Noted.".into()))
    );

    let mut replay = Replay::new();
    load(&mut replay, Some("Workers changed"));
    let sent = replay.written.pop_back().unwrap();
    assert_eq!(sent["params"]["prompt"][1]["text"], "Workers changed");
    assert!(
        sent["params"]["prompt"][0]["text"]
            .as_str()
            .unwrap()
            .starts_with("[shika] These are")
    );
    let tail = &replay.parsed[replay.parsed.len() - 3..];
    assert_eq!(tail[0], Parsed::ShikaInput(super::MEMORY_ROW));
    assert!(matches!(tail[1], Parsed::Reply(_)));
    assert_eq!(tail[2], Parsed::Accepted);

    // Delivered, or created with them: the load sends nothing more.
    let mut replay = Replay::new();
    replay.handshake(&launch(&cwd, SESSION, true, &["Keep this note"]));
    replay.receive(&json!({"jsonrpc": "2.0", "id": 1, "result": {"protocolVersion": 1, "agentCapabilities": {"loadSession": true}}}));
    replay.receive(&chunk("replayed history"));
    replay.receive(&json!({"jsonrpc": "2.0", "id": 2, "result": {}}));
    assert!(
        replay
            .written
            .iter()
            .all(|f| f["method"] != "session/prompt")
    );
    assert!(
        !replay
            .parsed
            .iter()
            .any(|p| matches!(p, Parsed::ShikaInput(_)))
    );
    assert_eq!(
        super::memory_text(&[]),
        "[shika] You have no memory notes now; any earlier ones were removed."
    );

    // An agent that cannot load sessions has no conversation for this id.
    let mut replay = Replay::new();
    replay.handshake(&launch(&cwd, SESSION, true, &[]));
    replay.receive(&json!({"jsonrpc": "2.0", "id": 1, "result": {"protocolVersion": 1, "agentCapabilities": {}}}));
    assert_eq!(
        replay.parsed.last(),
        Some(&Parsed::Missing(
            "Cursor CLI could not resume this conversation"
        ))
    );
    // Another load failure keeps the id: fatal, but not missing.
    let mut replay = Replay::new();
    replay.handshake(&launch(&cwd, SESSION, true, &[]));
    replay.receive(&json!({"jsonrpc": "2.0", "id": 1, "result": {"protocolVersion": 1, "agentCapabilities": {"loadSession": true}}}));
    replay.receive(
        &json!({"jsonrpc": "2.0", "id": 2, "error": {"code": -32603, "message": "Internal error"}}),
    );
    assert_eq!(
        replay.parsed.last(),
        Some(&Parsed::Fatal(
            "Cursor CLI could not resume this conversation"
        ))
    );
}

/// Cursor answers a load of a session whose store it cannot read with an
/// empty session and no error (seen in the acceptance run, 2026-11-20). A
/// saved session always replays its turns, so an empty load is refused and
/// keeps the id, and nothing is sent; another session's updates do not count.
#[test]
fn a_load_that_replays_nothing_is_refused_and_sends_nothing() {
    let cwd = Worktree::new();
    let mut replay = Replay::new();
    replay.handshake(&launch(&cwd, SESSION, false, &["Keep this note"]));
    replay.acp.send("Workers changed");
    replay.receive(&json!({"jsonrpc": "2.0", "id": 1, "result": {"protocolVersion": 1, "agentCapabilities": {"loadSession": true}}}));
    let mut other = chunk("another session");
    other["params"]["sessionId"] = "00000000-0000-4000-8000-000000000009".into();
    replay.receive(&other);
    replay.receive(&json!({"jsonrpc": "2.0", "id": 2, "result": {}}));
    assert_eq!(
        replay.parsed.last(),
        Some(&Parsed::Fatal(
            "Cursor CLI could not resume this conversation"
        ))
    );
    assert!(
        replay
            .written
            .iter()
            .all(|f| f["method"] != "session/prompt")
    );
    assert!(
        !replay
            .parsed
            .iter()
            .any(|p| matches!(p, Parsed::ShikaInput(_) | Parsed::Accepted))
    );
}
