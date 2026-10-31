//! Claude Code's documented bidirectional stream JSON, never terminal bytes.
//! Candidates are private until a successful result. All IO runs on the actor.
mod actor;

use std::collections::HashSet;
use std::hash::Hasher;
use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use std::sync::{Mutex, mpsc};
use std::thread;

use serde::Deserialize;
use serde_json::Value;

use crate::lead_chat_store::{ChatOrigin, ChatRecord, ChatResult, ChatStore};
use actor::{Actor, Process};

const BAD_STREAM: &str = "This Claude Code version could not be read";
const UNSUPPORTED: &str = "This Lead needs an interaction chat does not support.";
const INCOMPLETE: &str = "The reply ended before it was complete";
const MAX_FRAME: u64 = 8 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChatStatus {
    Waiting,
    Working,
    Asking,
    Ready,
}

#[derive(Clone, Debug)]
pub enum ChatEvent {
    Confirmed(String),
    Accepted,
    /// The CLI began a turn without Shika input, for example after one of the
    /// Lead's background commands finished. It counts as a submission.
    Started,
    DeliveryFailed,
    Rejected(String),
    Question(ChatQuestion),
    QuestionCancelled(String),
    Record(ChatRecord),
    Status(ChatStatus),
    Stopping,
    Resuming,
    Saved,
    SaveFailed,
    Error(String),
    Exit,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct ChatQuestionOption {
    pub label: String,
    #[serde(default)]
    pub description: String,
}
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct ChatQuestionItem {
    pub question: String,
    #[serde(default)]
    pub header: String,
    pub options: Vec<ChatQuestionOption>,
    #[serde(default, rename = "multiSelect")]
    pub multi_select: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatQuestion {
    pub request_id: String,
    pub questions: Vec<ChatQuestionItem>,
    pub input: Value,
}

#[derive(Debug, PartialEq, Eq)]
enum Parsed {
    Confirmed,
    Started,
    Final(Option<String>),
    Failed(&'static str),
    Question(ChatQuestion),
    Cancelled(String),
    Blocked,
    Ignore,
}

/// One process's decoder, with fresh candidate/dedup state for every turn.
struct ClaudeStream {
    expected: String,
    confirmed: bool,
    initialized: bool,
    version: Option<String>,
    active: bool,
    candidate: Option<(String, String)>,
    messages: HashSet<String>,
}
impl ClaudeStream {
    fn new(expected: String) -> Self {
        Self {
            expected,
            confirmed: false,
            initialized: false,
            version: None,
            active: false,
            candidate: None,
            messages: HashSet::new(),
        }
    }
    fn begin(&mut self) {
        self.active = true;
        self.candidate = None;
        self.messages.clear();
    }
    fn parse(&mut self, line: &[u8]) -> Parsed {
        let Ok(v) = serde_json::from_slice::<Value>(line) else {
            return Parsed::Failed(BAD_STREAM);
        };
        let Some(kind) = v["type"].as_str() else {
            return Parsed::Failed(BAD_STREAM);
        };
        if let Some(id) = v["session_id"].as_str()
            && id != self.expected
        {
            return Parsed::Failed("Claude Code returned a different conversation");
        }
        // Subagent messages never invalidate or replace the main assistant.
        if !v["parent_tool_use_id"].is_null() {
            return Parsed::Ignore;
        }
        match kind {
            "control_response" if v["response"]["request_id"] == "shika-initialize" => {
                self.initialize_response(&v)
            }

            "system" if v["subtype"] == "init" => self.turn_init(&v),
            "control_request"
                if self.active
                    && v["request"]["subtype"] == "can_use_tool"
                    && v["request"]["tool_name"] == "AskUserQuestion" =>
            {
                ask_user_question(&v)
            }
            "control_request" => Parsed::Blocked,
            "control_cancel_request" => v["request_id"]
                .as_str()
                .map(|id| Parsed::Cancelled(id.into()))
                .unwrap_or(Parsed::Ignore),
            "assistant" if self.active => self.assistant_message(&v),
            "user" if self.active => {
                // Tool replies invalidate text emitted before the call.
                if v["message"]["content"]
                    .as_array()
                    .is_some_and(|b| b.iter().any(|b| b["type"] == "tool_result"))
                {
                    self.candidate = None;
                }
                Parsed::Ignore
            }
            "result" if self.active => self.turn_result(&v),
            _ => Parsed::Ignore,
        }
    }
    /// The reply to Shika's initialize request, with the CLI version.
    fn initialize_response(&mut self, v: &Value) -> Parsed {
        if v["response"]["subtype"] != "success" {
            return Parsed::Failed(BAD_STREAM);
        }
        self.version = v["response"]["response"]["claude_code_version"]
            .as_str()
            .filter(|s| {
                s.len() <= 64
                    && s.chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
            })
            .map(str::to_string);
        self.initialized = true;
        Parsed::Ignore
    }
    /// `system/init` confirms the conversation and opens every turn.
    fn turn_init(&mut self, v: &Value) -> Parsed {
        if v["session_id"].as_str() != Some(&self.expected) {
            return Parsed::Failed(BAD_STREAM);
        }
        self.confirmed = true;
        // Every turn opens with init. One while idle is a turn the CLI
        // started itself; its final response is filtered the same way.
        if self.active {
            Parsed::Confirmed
        } else {
            self.begin();
            Parsed::Started
        }
    }
    /// The final-response filter: text becomes the candidate only from the
    /// latest assistant message with no tool call after it.
    fn assistant_message(&mut self, v: &Value) -> Parsed {
        if v.get("error").is_some_and(|e| !e.is_null()) {
            return Parsed::Failed(
                "Claude Code could not complete the reply. Check its sign-in and connection, then retry.",
            );
        }
        let Some(id) = v["message"]["id"].as_str() else {
            return Parsed::Failed(BAD_STREAM);
        };
        let identity = if let Some(uuid) = v["uuid"].as_str() {
            uuid.to_string()
        } else {
            let mut hash = std::collections::hash_map::DefaultHasher::new();
            hash.write(v["message"]["content"].to_string().as_bytes());
            format!("{id}:{:x}", hash.finish())
        };
        if !self.messages.insert(identity) {
            return Parsed::Ignore;
        }
        let Some(blocks) = v["message"]["content"].as_array() else {
            return Parsed::Failed(BAD_STREAM);
        };
        if self
            .candidate
            .as_ref()
            .is_some_and(|(candidate, _)| candidate != id)
        {
            self.candidate = None;
        }
        let mut texts = Vec::new();
        for block in blocks {
            match block["type"].as_str() {
                Some("tool_use" | "server_tool_use") => {
                    self.candidate = None;
                    return Parsed::Ignore;
                }
                Some("text") => {
                    let Some(text) = block["text"].as_str() else {
                        return Parsed::Failed(BAD_STREAM);
                    };
                    texts.push(text);
                }
                Some("thinking" | "redacted_thinking") => {}
                _ => {
                    self.candidate = None;
                    return Parsed::Failed(BAD_STREAM);
                }
            }
        }
        let text = texts.join("\n\n");
        if !text.trim().is_empty() {
            if let Some((_, candidate)) = &mut self.candidate {
                candidate.push_str("\n\n");
                candidate.push_str(&text);
            } else {
                self.candidate = Some((id.into(), text));
            }
        }
        Parsed::Ignore
    }
    /// A successful result publishes the candidate; anything else drops it.
    fn turn_result(&mut self, v: &Value) -> Parsed {
        self.active = false;
        if !self.initialized || !self.confirmed || v["session_id"].as_str() != Some(&self.expected)
        {
            self.candidate = None;
            return Parsed::Failed(BAD_STREAM);
        }
        if v["is_error"] != false || v["subtype"] != "success" {
            self.candidate = None;
            return Parsed::Failed(
                "Claude Code could not complete the reply. Check its sign-in and connection, then retry.",
            );
        }
        // Never use result.result: some releases aggregate commentary.
        Parsed::Final(self.candidate.take().map(|(_, text)| text))
    }
}

/// An `AskUserQuestion` permission request Shika can show as a question;
/// anything it cannot show blocks.
fn ask_user_question(v: &Value) -> Parsed {
    let Some(id) = v["request_id"].as_str() else {
        return Parsed::Failed(BAD_STREAM);
    };
    let Ok(questions) =
        serde_json::from_value::<Vec<ChatQuestionItem>>(v["request"]["input"]["questions"].clone())
    else {
        return Parsed::Blocked;
    };
    if questions.is_empty()
        || questions.len() > 4
        || questions
            .iter()
            .any(|q| q.question.is_empty() || q.options.len() > 4)
    {
        return Parsed::Blocked;
    }
    Parsed::Question(ChatQuestion {
        request_id: id.into(),
        questions,
        input: v["request"]["input"].clone(),
    })
}

pub(crate) struct ChatLaunch {
    pub usage: Option<crate::usage::Source>,
    pub program: PathBuf,
    pub cwd: PathBuf,
    pub path: String,
    pub env: Vec<(String, String)>,
    pub instructions: String,
    pub conversation: String,
    pub resumed: bool,
    pub history: Option<String>,
    pub data: PathBuf,
    pub project: String,
}
impl ChatLaunch {
    fn args(&self, resumed: bool) -> Vec<String> {
        [
            "-p",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--verbose",
            "--dangerously-skip-permissions",
            "--append-system-prompt",
            &self.instructions,
            "--permission-prompts",
            "host",
            "--permission-prompt-tool",
            "stdio",
            if resumed { "--resume" } else { "--session-id" },
            &self.conversation,
        ]
        .into_iter()
        .map(str::to_string)
        .collect()
    }
}

enum CommandIn {
    Send {
        text: String,
        origin: ChatOrigin,
        events: Vec<(String, Option<String>)>,
        epoch: u64,
    },
    Interrupt,
    RetrySave,
    Answer {
        request_id: String,
        answer: Option<Value>,
    },
    Stop,
    Line(u64, Vec<u8>),
    BadFrame(u64),
    Eof(u64),
}
pub(crate) struct ChatTransport {
    stop: Arc<AtomicBool>,
    cancel: Arc<AtomicU64>,
    pub history: String,
    input: mpsc::Sender<CommandIn>,
    events: Mutex<mpsc::Receiver<ChatEvent>>,
    join: Mutex<Option<thread::JoinHandle<()>>>,
}
impl ChatTransport {
    pub fn start(launch: ChatLaunch) -> ChatResult<Self> {
        let mut store = ChatStore::open(&launch.data, &launch.project)?;
        if launch
            .history
            .as_ref()
            .is_some_and(|id| id != &store.lifetime)
        {
            return Err("Could not read this conversation: history identity changed".into());
        }
        let history = store.lifetime.clone();
        store.recover()?;
        let boundary = store.bind("claude", &launch.conversation)?;
        let next_turn = store.next_turn()?;
        let (input, rx) = mpsc::channel();
        let (frames, frame_rx) = mpsc::sync_channel(8);
        let (events, output) = mpsc::channel();
        if let Some(boundary) = boundary {
            let _ = events.send(ChatEvent::Record(boundary));
        }
        let child = Process::spawn(&launch, launch.resumed, 1, frames.clone())?;
        let actor_frames = frames;
        let cancel = Arc::new(AtomicU64::new(0));
        let actor_cancel = cancel.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let actor_stop = stop.clone();
        let join = thread::Builder::new()
            .name("lead-chat".into())
            .spawn(move || {
                Actor {
                    cancel: actor_cancel,
                    stop: actor_stop,
                    cancel_seen: 0,
                    launch,
                    store,
                    output: events,
                    frames: actor_frames,
                    child: Some(child),
                    generation: 1,
                    next_turn,
                    turn: None,
                    stopping: None,
                    exiting: false,
                    save_failed: false,
                    pending: Vec::new(),
                    active_input: None,
                    failed: false,
                }
                .run(rx, frame_rx);
            })
            .map_err(|_| "Could not start the Lead".to_string())?;
        Ok(Self {
            stop,
            cancel,
            history,
            input,
            events: Mutex::new(output),
            join: Mutex::new(Some(join)),
        })
    }
    pub fn send(
        &self,
        text: String,
        origin: ChatOrigin,
        events: Vec<(String, Option<String>)>,
    ) -> ChatResult<()> {
        self.input
            .send(CommandIn::Send {
                text,
                origin,
                events,
                epoch: self.cancel.load(Ordering::Acquire),
            })
            .map_err(|_| "The Lead is not running".into())
    }
    pub fn interrupt(&self) {
        self.cancel.fetch_add(1, Ordering::AcqRel);
        let _ = self.input.send(CommandIn::Interrupt);
    }
    pub fn retry_save(&self) {
        let _ = self.input.send(CommandIn::RetrySave);
    }
    pub fn answer(&self, request_id: String, answer: Option<Value>) {
        let _ = self.input.send(CommandIn::Answer { request_id, answer });
    }
    pub fn drain(&self) -> Vec<ChatEvent> {
        self.events
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .try_iter()
            .collect()
    }
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Release);
        self.cancel.fetch_add(1, Ordering::AcqRel);
        let _ = self.input.send(CommandIn::Stop);
        if let Some(join) = self.join.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = join.join();
        }
    }
}
impl Drop for ChatTransport {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn final_only_from_documented_hand_written_fixture() {
        let mut parser = ClaudeStream::new("session".into());
        parser.begin();
        let fixture = include_str!("../tests/fixtures/claude-chat.jsonl");
        let events = fixture
            .lines()
            .map(|l| parser.parse(l.as_bytes()))
            .collect::<Vec<_>>();
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, Parsed::Final(_)))
                .count(),
            1
        );
        assert_eq!(
            events.last(),
            Some(&Parsed::Final(Some(
                "Done.\n\n```bash\ngit status\n```".into()
            )))
        );
    }
    #[test]
    fn installed_cli_two_turn_fixture_hides_split_blocks_with_the_same_message_id() {
        let mut parser = ClaudeStream::new("fixture-session".into());
        parser.begin();
        let mut finals = Vec::new();
        for line in include_str!("../tests/fixtures/claude-2.1.302-local.jsonl").lines() {
            if let Parsed::Final(text) = parser.parse(line.as_bytes()) {
                finals.push(text.unwrap());
                parser.begin();
            }
        }
        assert_eq!(finals.len(), 2);
        assert!(
            finals
                .iter()
                .all(|text| text.starts_with("Final synthetic reply")
                    && !text.contains("Intermediate")
                    && !text.contains("synthetic-tool-output"))
        );
    }
    #[test]
    fn installed_cli_background_fixture_reads_the_turn_the_cli_starts_itself() {
        // Shika sends one message. When its background command finishes, the
        // CLI opens a second turn with no input; the parser alone must find it.
        let mut parser = ClaudeStream::new("fixture-session".into());
        parser.begin();
        let mut started = 0;
        let mut finals = Vec::new();
        for line in include_str!("../tests/fixtures/claude-2.1.302-background-local.jsonl").lines()
        {
            match parser.parse(line.as_bytes()) {
                Parsed::Started => started += 1,
                Parsed::Final(text) => finals.push(text.unwrap()),
                Parsed::Failed(error) => panic!("{error}"),
                _ => {}
            }
        }
        assert_eq!(started, 1);
        assert_eq!(
            finals,
            [
                "Synthetic reply - waiting on the background command.",
                "Background synthetic reply - the worker finished and its diff looks right."
            ]
        );
        assert!(!parser.active);
    }
    #[test]
    fn idle_records_other_than_init_never_start_a_turn() {
        let mut p = ClaudeStream::new("s".into());
        p.parse(br#"{"type":"control_response","response":{"request_id":"shika-initialize","subtype":"success"}}"#);
        p.parse(br#"{"type":"system","subtype":"task_notification","session_id":"s"}"#);
        p.parse(br#"{"type":"assistant","message":{"id":"a","content":[{"type":"text","text":"late"}]}}"#);
        assert_eq!(
            p.parse(br#"{"type":"result","subtype":"success","is_error":false,"session_id":"s"}"#),
            Parsed::Ignore
        );
        assert_eq!(
            p.parse(br#"{"type":"system","subtype":"init","session_id":"s"}"#),
            Parsed::Started
        );
        assert_eq!(
            p.parse(br#"{"type":"system","subtype":"init","session_id":"s"}"#),
            Parsed::Confirmed
        );
    }
    #[test]
    fn real_question_fixture_blocks_and_never_becomes_a_bubble() {
        let mut parser = ClaudeStream::new("fixture-session".into());
        parser.begin();
        let mut questions = 0;
        for line in include_str!("../tests/fixtures/claude-2.1.302-question-local.jsonl").lines() {
            match parser.parse(line.as_bytes()) {
                Parsed::Question(q) => {
                    questions += 1;
                    assert_eq!(q.request_id, "fixture-question");
                    assert_eq!(q.questions[0].options[0].label, "First");
                }
                Parsed::Final(text) => {
                    assert!(!text.unwrap().contains("Which synthetic option"));
                    parser.begin();
                }
                _ => {}
            }
        }
        assert_eq!(questions, 1);
    }
    #[test]
    fn multiple_final_text_blocks_share_a_bubble_and_uuid_identity() {
        let mut p = ClaudeStream::new("s".into());
        p.begin();
        p.parse(br#"{"type":"control_response","response":{"request_id":"shika-initialize","subtype":"success"}}"#);
        p.parse(br#"{"type":"system","subtype":"init","session_id":"s"}"#);
        for uuid in ["block1", "block2", "block2"] {
            p.parse(serde_json::to_string(&json!({"type":"assistant","uuid":uuid,"message":{"id":"m","content":[{"type":"text","text":"Same words"}]}})).unwrap().as_bytes());
        }
        assert_eq!(
            p.parse(br#"{"type":"result","subtype":"success","is_error":false,"session_id":"s"}"#),
            Parsed::Final(Some("Same words\n\nSame words".into()))
        );
    }
    #[test]
    fn identity_failure_error_and_interruption_never_publish_candidates() {
        let mut p = ClaudeStream::new("s".into());
        p.begin();
        assert_eq!(
            p.parse(br#"{"type":"system","subtype":"init","session_id":"other"}"#),
            Parsed::Failed("Claude Code returned a different conversation")
        );
        assert_eq!(p.parse(b"garbage"), Parsed::Failed(BAD_STREAM));
        assert_eq!(
            p.parse(br#"{"type":"control_request","request":{}}"#),
            Parsed::Blocked
        );
        p.parse(br#"{"type":"system","subtype":"init","session_id":"s"}"#);
        p.parse(br#"{"type":"assistant","message":{"id":"a","content":[{"type":"text","text":"hidden"}]}}"#);
        assert!(matches!(p.parse(br#"{"type":"result","subtype":"error_during_execution","is_error":true,"session_id":"s"}"#),Parsed::Failed(_)));
        assert_eq!(p.candidate, None);
    }
}
