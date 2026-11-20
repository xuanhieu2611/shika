//! The Codex decoder over the codex-cli 0.162.1 app-server recordings. Each
//! replay drives the protocol through its own API where the recording's
//! client wrote (send, interrupt, answer) and checks that every frame Shika
//! writes matches the recorded one.
use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::*;

const THREAD: &str = "00000000-0000-7000-8000-000000000002";
const CWD: &str = "/fixture/repo/.worktrees/shika-lead";

fn launch(conversation: &str, instructions: &str, instructed: bool) -> ChatLaunch {
    ChatLaunch {
        cli: "codex",
        usage: None,
        usage_threads: None,
        program: PathBuf::from("codex"),
        cwd: PathBuf::from(CWD),
        path: String::new(),
        env: Vec::new(),
        instructions: instructions.into(),
        conversation: conversation.into(),
        resumed: !conversation.is_empty(),
        instructed,
        history: None,
        data: PathBuf::new(),
        project: "codex-test".into(),
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

/// What the recording's own client wrote, comparable with Shika's frames:
/// no `"jsonrpc"` member, no empty `params`, the fixture client version, and
/// no `developerInstructions` on a resume, which Codex ignores.
fn normalized(mut v: Value) -> Value {
    let object = v.as_object_mut().unwrap();
    object.remove("jsonrpc");
    if object.get("params") == Some(&json!({})) {
        object.remove("params");
    }
    if v["method"] == "initialize" {
        v["params"]["clientInfo"]["version"] = "fixture".into();
    }
    if v["method"] == "thread/resume" {
        v["params"]
            .as_object_mut()
            .unwrap()
            .remove("developerInstructions");
    }
    v
}

/// One recording replayed through the protocol: every parsed result in
/// order (a protocol's queued results included), and Shika's frames.
struct Replay {
    codex: CodexAppServer,
    parsed: Vec<Parsed>,
    written: VecDeque<Value>,
    question: Option<ChatQuestion>,
}

impl Replay {
    fn new() -> Self {
        Self {
            codex: CodexAppServer::new(),
            parsed: Vec::new(),
            written: VecDeque::new(),
            question: None,
        }
    }
    fn write(&mut self, bytes: &[u8]) {
        if !bytes.is_empty() {
            self.written.extend(frame(bytes));
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
    fn receive(&mut self, msg: &Value) {
        let parsed = self.codex.parse(&serde_json::to_vec(msg).unwrap());
        self.record(parsed);
        loop {
            match self.codex.poll(Instant::now()) {
                Parsed::Ignore => break,
                parsed => self.record(parsed),
            }
        }
    }
    /// Produces Shika's frame for a frame the recording's client wrote and
    /// checks the two match.
    fn send(&mut self, msg: &Value) {
        match msg["method"].as_str() {
            Some("turn/start") => {
                let text = msg["params"]["input"][0]["text"].as_str().unwrap();
                let bytes = self.codex.send(text);
                self.write(&bytes);
            }
            Some("turn/interrupt") => match self.codex.interrupt() {
                Interrupt::Frame(bytes) => self.write(&bytes),
                _ => panic!("a running turn is interrupted with a frame"),
            },
            None => {
                // An answer to a server request: the dialog's answers are
                // keyed by question text.
                let question = self.question.take().expect("an open question");
                let mut input = question.input.clone();
                let mut given = serde_json::Map::new();
                for (q, raw) in question
                    .questions
                    .iter()
                    .zip(input["questions"].as_array().unwrap())
                {
                    let id = raw["id"].as_str().unwrap();
                    given.insert(
                        q.question.clone(),
                        msg["result"]["answers"][id]["answers"][0].clone(),
                    );
                }
                input["answers"] = Value::Object(given);
                let bytes = self.codex.answer(&question.request_id, Some(input));
                self.write(&bytes);
            }
            _ => {}
        }
        let ours = self
            .written
            .pop_front()
            .unwrap_or_else(|| panic!("Shika wrote nothing for {msg}"));
        assert_eq!(normalized(ours), normalized(msg.clone()));
    }
    /// Replays `fixture` until a line containing `stop`, starting each
    /// process with `launches` in order.
    fn run(fixture: &str, stop: Option<&str>, launches: &[ChatLaunch]) -> Self {
        let mut replay = Self::new();
        let mut launches = launches.iter();
        if !fixture.starts_with(r#"{"dir": "spawn""#) {
            let bytes = replay.codex.handshake(launches.next().unwrap());
            replay.write(&bytes);
        }
        for line in fixture.lines() {
            if stop.is_some_and(|stop| line.contains(stop)) {
                break;
            }
            let record: Value = serde_json::from_str(line).unwrap();
            let msg = &record["msg"];
            match record["dir"].as_str().unwrap() {
                "spawn" => {
                    let bytes = replay.codex.handshake(launches.next().unwrap());
                    replay.written.clear();
                    replay.write(&bytes);
                }
                "send" => replay.send(msg),
                "recv" => replay.receive(msg),
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

fn fresh() -> ChatLaunch {
    launch("", "FIXTURE LEAD INSTRUCTIONS A", false)
}

#[test]
fn normal_turns_publish_only_the_final_answer_with_phase_and_the_last_message_without() {
    let replay = Replay::run(
        include_str!("../../tests/fixtures/codex-0.162.1-normal-local.jsonl"),
        None,
        &[fresh()],
    );
    assert_eq!(
        replay.finals(),
        [
            Some("Final synthetic reply 1.\n\n```bash\ngit status\n```".into()),
            Some("Final synthetic reply 2, no phase.".into()),
        ]
    );
    // Identity from `thread/start`, held by the instructions it was given;
    // each of our turns acknowledged and confirmed, none started by Codex.
    assert_eq!(replay.parsed[2], Parsed::Identity(THREAD.into()));
    assert_eq!(replay.parsed[3], Parsed::Instructed);
    assert_eq!(replay.count(|p| *p == Parsed::Accepted), 2);
    assert_eq!(replay.count(|p| *p == Parsed::Confirmed), 2);
    assert_eq!(replay.count(|p| *p == Parsed::Started), 0);
    assert_eq!(replay.codex.version(), Some("0.162.1"));
    let shown = format!("{:?}", replay.finals());
    for private in [
        "I will check",
        "One more check",
        "Partial synthetic",
        "Checking again",
        "synthetic-tool-output",
    ] {
        assert!(!shown.contains(private), "{private}");
    }
}

#[test]
fn retried_errors_are_not_failures_and_failed_turns_never_publish() {
    let replay = Replay::run(
        include_str!("../../tests/fixtures/codex-0.162.1-failure-local.jsonl"),
        None,
        &[fresh()],
    );
    let outcomes = replay
        .parsed
        .iter()
        .filter(|p| matches!(p, Parsed::Final(_) | Parsed::Failed(_)))
        .collect::<Vec<_>>();
    assert_eq!(
        outcomes,
        [
            &Parsed::Failed(REPLY_FAILED),
            &Parsed::Failed(REPLY_FAILED),
            &Parsed::Final(Some("Final synthetic reply after retries.".into())),
            &Parsed::Final(Some("Final synthetic reply after failures.".into())),
        ]
    );
}

#[test]
fn an_interrupt_during_a_command_ends_the_process_and_the_thread_continues() {
    let replay = Replay::run(
        include_str!("../../tests/fixtures/codex-0.162.1-interrupt-local.jsonl"),
        None,
        &[fresh()],
    );
    // `sleep 37` never completed: only the process's end stops it.
    assert_eq!(replay.count(|p| *p == Parsed::Lingering), 1);
    assert_eq!(
        replay.finals(),
        [Some("Final synthetic reply after the interrupt.".into())]
    );
    assert!(!format!("{:?}", replay.parsed).contains("Starting a long command"));
}

#[test]
fn an_interrupt_with_nothing_running_settles_with_no_bubble() {
    let mut codex = CodexAppServer::new();
    codex.handshake(&fresh());
    codex.thread = Some(THREAD.into());
    codex.send("hi");
    codex.parse(br#"{"id":2,"result":{"turn":{"id":"t1"}}}"#);
    codex.parse(
        br#"{"method":"item/completed","params":{"threadId":"00000000-0000-7000-8000-000000000002","turnId":"t1","item":{"type":"agentMessage","id":"m","text":"private","phase":"final_answer"}}}"#,
    );
    assert!(matches!(codex.interrupt(), Interrupt::Frame(_)));
    assert_eq!(
        codex.parse(
            br#"{"method":"turn/completed","params":{"threadId":"00000000-0000-7000-8000-000000000002","turn":{"id":"t1","status":"interrupted"}}}"#
        ),
        Parsed::Final(None)
    );
}

#[test]
fn a_question_maps_to_the_dialog_and_its_answer_is_keyed_by_question_id() {
    let replay = Replay::run(
        include_str!("../../tests/fixtures/codex-0.162.1-question-local.jsonl"),
        None,
        &[fresh()],
    );
    let question = replay
        .parsed
        .iter()
        .find_map(|p| match p {
            Parsed::Question(q) => Some(q.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(question.request_id, "0");
    assert_eq!(question.questions.len(), 1);
    assert_eq!(question.questions[0].question, "Which synthetic option?");
    assert_eq!(question.questions[0].header, "Choice");
    assert_eq!(
        question.questions[0]
            .options
            .iter()
            .map(|o| o.label.as_str())
            .collect::<Vec<_>>(),
        ["Alpha (Recommended)", "Beta"]
    );
    // `serverRequest/resolved` dismisses it; then only the final shows.
    assert!(replay.parsed.contains(&Parsed::Cancelled("0".into())));
    assert_eq!(
        replay.finals(),
        [Some("Final synthetic reply after the question.".into())]
    );
}

#[test]
fn a_cancelled_question_answers_the_empty_map() {
    let mut codex = CodexAppServer::new();
    assert_eq!(
        frame(&codex.answer("0", None)),
        [json!({"id": 0, "result": {"answers": {}}})]
    );
    assert_eq!(
        frame(&codex.answer("\"r-1\"", None)),
        [json!({"id": "r-1", "result": {"answers": {}}})]
    );
}

#[test]
fn questions_the_dialog_cannot_show_and_every_other_request_block() {
    let mut codex = CodexAppServer::new();
    codex.thread = Some(THREAD.into());
    codex.turn = Turn::Running("t1".into());
    let ask = |questions: Value, thread: &str, turn: &str| {
        serde_json::to_vec(
            &json!({"id": 5, "method": "item/tool/requestUserInput", "params": {
                "threadId": thread, "turnId": turn, "itemId": "c", "isBlocking": false,
                "questions": questions,
            }}),
        )
        .unwrap()
    };
    let one = json!([{"id": "a", "header": "H", "question": "Q?", "options": [{"label": "x", "description": ""}]}]);
    assert!(matches!(
        codex.parse(&ask(one.clone(), THREAD, "t1")),
        Parsed::Question(_)
    ));
    // A free-form question with no options still fits the dialog.
    assert!(matches!(
        codex.parse(&ask(
            json!([{"id": "a", "header": "", "question": "Name?", "isOther": true}]),
            THREAD,
            "t1"
        )),
        Parsed::Question(_)
    ));
    for (questions, thread, turn, why) in [
        (one.clone(), "subagent-thread", "t1", "another thread"),
        (one.clone(), THREAD, "old-turn", "another turn"),
        (json!([]), THREAD, "t1", "no questions"),
        (
            json!([{"id": "a", "header": "", "question": "Secret?", "isSecret": true}]),
            THREAD,
            "t1",
            "secret",
        ),
        (
            json!([{"id": "a", "question": "Q?"}, {"id": "b", "question": "Q?"}]),
            THREAD,
            "t1",
            "same text",
        ),
        (
            json!([{"id": "a", "question": "Q?"}, {"id": "a", "question": "R?"}]),
            THREAD,
            "t1",
            "same id",
        ),
        (
            json!([{"id": "a", "question": "Q?", "options": [1, 2]}]),
            THREAD,
            "t1",
            "bad options",
        ),
        (
            json!([{"id": "a", "question": "Q?", "options": [
            {"label": "1"}, {"label": "2"}, {"label": "3"}, {"label": "4"}, {"label": "5"}]}]),
            THREAD,
            "t1",
            "five options",
        ),
    ] {
        assert_eq!(
            codex.parse(&ask(questions, thread, turn)),
            Parsed::Blocked,
            "{why}"
        );
    }
    for method in [
        "item/commandExecution/requestApproval",
        "item/fileChange/requestApproval",
        "item/permissions/requestApproval",
        "applyPatchApproval",
        "execCommandApproval",
        "mcpServer/elicitation/request",
        "item/tool/call",
        "some/futureRequest",
    ] {
        let line = serde_json::to_vec(
            &json!({"id": 9, "method": method, "params": {"threadId": THREAD, "turnId": "t1"}}),
        )
        .unwrap();
        assert_eq!(codex.parse(&line), Parsed::Blocked, "{method}");
    }
}

#[test]
fn a_turn_codex_starts_itself_is_read_like_any_other() {
    let replay = Replay::run(
        include_str!("../../tests/fixtures/codex-0.162.1-background-local.jsonl"),
        None,
        &[fresh()],
    );
    assert_eq!(replay.count(|p| *p == Parsed::Started), 1);
    assert_eq!(
        replay.finals(),
        [
            Some("Goal created; I will continue.".into()),
            Some("Final synthetic reply in a turn the CLI started (2).".into()),
        ]
    );
}

#[test]
fn subagent_threads_on_the_same_connection_are_dropped() {
    let replay = Replay::run(
        include_str!("../../tests/fixtures/codex-0.162.1-subagent-local.jsonl"),
        None,
        &[fresh()],
    );
    assert_eq!(replay.count(|p| *p == Parsed::Started), 0);
    assert_eq!(
        replay.finals(),
        [Some(
            "Helper spawned; I will report when it finishes.".into()
        )]
    );
    assert!(!format!("{:?}", replay.parsed).contains("Child synthetic"));
}

#[test]
fn a_resume_reports_the_same_thread_and_sends_unchanged_instructions_nowhere() {
    let fixture = include_str!("../../tests/fixtures/codex-0.162.1-resume-local.jsonl");
    let replay = Replay::run(
        fixture,
        Some(r#""id": 4, "method": "thread/resume""#),
        &[fresh(), launch(THREAD, "FIXTURE LEAD INSTRUCTIONS B", true)],
    );
    assert_eq!(replay.count(|p| *p == Parsed::Identity(THREAD.into())), 2);
    // Only the fresh thread was created holding the instructions.
    assert_eq!(replay.count(|p| *p == Parsed::Instructed), 1);
    assert_eq!(
        replay.finals(),
        [
            Some("Final synthetic hello.".into()),
            Some("Final synthetic hello.".into())
        ]
    );
    // A thread created by the TUI resumes the same way.
    let replay = Replay::run(
        include_str!("../../tests/fixtures/codex-0.162.1-resume-tui-local.jsonl"),
        None,
        &[launch(THREAD, "FIXTURE LEAD INSTRUCTIONS B", true)],
    );
    assert_eq!(replay.count(|p| *p == Parsed::Identity(THREAD.into())), 1);
    assert_eq!(replay.count(|p| *p == Parsed::Instructed), 0);
    assert_eq!(replay.finals(), [Some("Final synthetic hello.".into())]);
}

/// The recorded `thread/resume` response, answering request `id`.
fn resumed(fixture: &str, id: u64) -> Vec<u8> {
    let line = fixture
        .lines()
        .find(|line| line.contains(r#""recv""#) && line.contains(r#""id": 2, "result": {"thread""#))
        .unwrap();
    let mut msg = serde_json::from_str::<Value>(line).unwrap()["msg"].clone();
    msg["id"] = id.into();
    serde_json::to_vec(&msg).unwrap()
}

#[test]
fn changed_instructions_reach_a_resumed_thread_before_the_first_turn() {
    let fixture = include_str!("../../tests/fixtures/codex-0.162.1-resume-tui-local.jsonl");
    let mut codex = CodexAppServer::new();
    codex.handshake(&launch(THREAD, "GUIDE AND A NEW NOTE", false));
    codex.parse(br#"{"id":1,"result":{"userAgent":"shika/0.162.1 (x)"}}"#);
    // A send before the handshake finished waits for it.
    assert!(codex.send("hello").is_empty());
    assert_eq!(
        codex.parse(&resumed(fixture, 2)),
        Parsed::Identity(THREAD.into())
    );
    assert!(!codex.ready());
    let Parsed::Reply(inject) = codex.poll(Instant::now()) else {
        panic!("the instructions go first");
    };
    assert_eq!(
        frame(&inject),
        [
            json!({"id": 3, "method": "thread/inject_items", "params": {"threadId": THREAD, "items": [
                {"type": "message", "role": "developer", "content": [{"type": "input_text", "text": "GUIDE AND A NEW NOTE"}]}
            ]}})
        ]
    );
    assert_eq!(codex.poll(Instant::now()), Parsed::Ignore);
    assert_eq!(codex.parse(br#"{"id":3,"result":{}}"#), Parsed::Instructed);
    assert!(codex.ready());
    let Parsed::Reply(turn) = codex.poll(Instant::now()) else {
        panic!("the waiting send goes out");
    };
    assert_eq!(frame(&turn)[0]["method"], "turn/start");
    assert_eq!(frame(&turn)[0]["params"]["input"][0]["text"], "hello");
    assert_eq!(
        codex.parse(br#"{"id":4,"result":{"turn":{"id":"t"}}}"#),
        Parsed::Accepted
    );
}

#[test]
fn a_refused_or_different_resume_fails_and_ends_the_transport() {
    let fixture = include_str!("../../tests/fixtures/codex-0.162.1-resume-local.jsonl");
    let refused = |error_id: u64| {
        let mut msg = fixture
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap()["msg"].clone())
            .find(|msg| msg["id"] == error_id && msg.get("error").is_some())
            .unwrap();
        msg["id"] = 2.into();
        let mut codex = CodexAppServer::new();
        codex.handshake(&launch(THREAD, "guide", true));
        codex.parse(br#"{"id":1,"result":{"userAgent":"shika/0.162.1"}}"#);
        codex.parse(&serde_json::to_vec(&msg).unwrap())
    };
    // No rollout for the id, and an id that is not a UUID.
    assert_eq!(refused(4), Parsed::Fatal(RESUME_REFUSED));
    assert_eq!(refused(5), Parsed::Fatal(RESUME_REFUSED));
    let mut codex = CodexAppServer::new();
    codex.handshake(&launch(
        "00000000-0000-7000-8000-00000000000f",
        "guide",
        true,
    ));
    codex.parse(br#"{"id":1,"result":{"userAgent":"shika/0.162.1"}}"#);
    assert_eq!(
        codex.parse(&resumed(fixture, 2)),
        Parsed::Failed(STRINGS.other_conversation)
    );
}

#[test]
fn launch_arguments_enable_questions_disable_goals_and_trust_only_the_worktree() {
    let args = CodexAppServer::new().args(&launch("", "guide", false));
    assert_eq!(
        args[..6],
        [
            "app-server",
            "--enable",
            "default_mode_request_user_input",
            "--disable",
            "goals",
            "-c"
        ]
    );
    assert_eq!(
        args[6],
        crate::agents::codex_trust_override(std::path::Path::new(CWD))
    );
    assert_eq!(args.len(), 7);
}

#[test]
fn garbage_huge_ignored_records_and_a_silent_handshake() {
    let mut codex = CodexAppServer::new();
    codex.handshake(&fresh());
    assert_eq!(codex.parse(b"garbage"), Parsed::Failed(BAD_STREAM));
    assert_eq!(codex.parse(b"[1,2]"), Parsed::Failed(BAD_STREAM));
    let delta = serde_json::to_vec(&json!({"method": "item/agentMessage/delta", "params": {
        "threadId": THREAD, "turnId": "t", "itemId": "m", "delta": "x".repeat(1 << 20)
    }}))
    .unwrap();
    assert_eq!(codex.parse(&delta), Parsed::Ignore);
    assert_eq!(codex.parse(br#"{"id":77,"result":{}}"#), Parsed::Ignore);
    assert_eq!(codex.poll(Instant::now()), Parsed::Ignore);
    // No initialize response within the deadline: unreadable, once.
    let late = Instant::now() + Duration::from_secs(11);
    assert_eq!(codex.poll(late), Parsed::Failed(BAD_STREAM));
    assert_eq!(codex.poll(late), Parsed::Ignore);
}

#[test]
fn stop_generating_matches_what_codex_has_of_the_turn() {
    let mut codex = CodexAppServer::new();
    codex.handshake(&fresh());
    codex.send("early");
    // Not sent yet: nothing to stop, and it never goes out.
    assert!(matches!(codex.interrupt(), Interrupt::Idle));
    codex.parse(br#"{"id":1,"result":{"userAgent":"shika/0.162.1"}}"#);
    codex.parse(br#"{"id":2,"result":{"thread":{"id":"00000000-0000-7000-8000-000000000002"}}}"#);
    while codex.poll(Instant::now()) != Parsed::Ignore {}
    assert_eq!(codex.turn, Turn::Idle);
    assert!(matches!(codex.interrupt(), Interrupt::Idle));
    // Written, but Codex has not named the turn yet.
    assert!(!codex.send("now").is_empty());
    assert!(matches!(codex.interrupt(), Interrupt::Signal));
    assert_eq!(codex.stop_signal(), libc::SIGTERM);
    assert!(matches!(codex.shutdown(), Shutdown::CloseStdin));
}
