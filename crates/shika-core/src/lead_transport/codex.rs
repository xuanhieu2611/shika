//! Codex's `codex app-server`: JSON-RPC over stdio, one process per Lead that
//! keeps running across turns and Stop generating. Candidates are private
//! until `turn/completed` reports success. Only the Lead's own thread counts:
//! subagent threads stream on the same connection.
use std::collections::{HashSet, VecDeque};
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::jsonrpc::{Message, Rpc, RpcError};
use super::protocol::{Interrupt, LeadProtocol, Shutdown, Strings};
use super::{ChatLaunch, ChatQuestion, ChatQuestionItem, ChatQuestionOption, Parsed};

pub(super) const BAD_STREAM: &str = "This Codex version could not be read";
pub(super) const REPLY_FAILED: &str =
    "Codex could not complete the reply. Check its sign-in and connection, then retry.";
pub(super) const RESUME_REFUSED: &str = "Codex could not resume this conversation";
static STRINGS: Strings = Strings {
    unreadable: BAD_STREAM,
    other_conversation: "Codex returned a different conversation",
    start_failed: "Could not start Codex",
    input_failed: "Could not open Codex input",
    handshake_failed: "Could not initialize Codex",
    restart_failed: "Could not start Codex. Try sending again.",
};
/// How long each handshake request and `turn/start` may take to answer.
const DEADLINE: Duration = Duration::from_secs(10);
/// Item types that are neither the reply nor a tool. Every other type, known
/// or not, is work that makes earlier text commentary.
const NEUTRAL_ITEMS: &[&str] = &[
    "userMessage",
    "reasoning",
    "plan",
    "hookPrompt",
    "contextCompaction",
];

/// What one of our requests was for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Request {
    Initialize,
    Thread,
    Inject,
    TurnStart,
    Interrupt,
}

/// The Lead thread's turn, as far as Codex knows it.
#[derive(Debug, PartialEq, Eq)]
enum Turn {
    Idle,
    /// Sent before the handshake finished; `turn/start` goes out once it has.
    Deferred(String),
    /// `turn/start` is written and its turn id not known yet.
    Starting,
    Running(String),
}

/// One per Lead; `handshake` resets it for every process.
pub(super) struct CodexAppServer {
    rpc: Rpc<Request>,
    /// The thread to resume, or None to start one.
    resume: Option<String>,
    cwd: String,
    instructions: String,
    /// The thread already holds `instructions`; a resume sends them otherwise.
    instructed: bool,
    version: Option<String>,
    thread: Option<String>,
    injecting: bool,
    turn: Turn,
    interrupting: bool,
    /// `final_answer` messages of this turn, in order.
    finals: Vec<String>,
    /// The last other message with no tool after it.
    fallback: Option<String>,
    /// Command items started and not completed.
    running: HashSet<String>,
    /// An `error` that Codex will not retry arrived for this turn.
    errored: bool,
    /// Further results of the last line, returned by `poll`.
    queued: VecDeque<Parsed>,
    /// A request expired; reported once.
    expired: bool,
}

impl CodexAppServer {
    pub(super) fn new() -> Self {
        Self {
            rpc: Rpc::new(false),
            resume: None,
            cwd: String::new(),
            instructions: String::new(),
            instructed: false,
            version: None,
            thread: None,
            injecting: false,
            turn: Turn::Idle,
            interrupting: false,
            finals: Vec::new(),
            fallback: None,
            running: HashSet::new(),
            errored: false,
            queued: VecDeque::new(),
            expired: false,
        }
    }
    fn begin(&mut self) {
        self.interrupting = false;
        self.finals.clear();
        self.fallback = None;
        self.running.clear();
        self.errored = false;
    }
    fn request(&mut self, method: &str, params: Value, kind: Request) -> Vec<u8> {
        self.rpc
            .request(method, params, kind, DEADLINE, Instant::now())
    }
    /// `thread/start` with the instructions, or `thread/resume`, which
    /// ignores new ones (see `Parsed::Instructed`). Without `excludeTurns`
    /// the response would carry the whole history.
    fn open_thread(&mut self) -> Vec<u8> {
        let mut params = json!({
            "cwd": self.cwd,
            "approvalPolicy": "never",
            "sandbox": "danger-full-access",
        });
        let method = if let Some(id) = &self.resume {
            params["threadId"] = id.clone().into();
            params["excludeTurns"] = true.into();
            "thread/resume"
        } else {
            params["developerInstructions"] = self.instructions.clone().into();
            "thread/start"
        };
        self.request(method, params, Request::Thread)
    }
    fn turn_start(&mut self, text: &str) -> Vec<u8> {
        let thread = self.thread.clone().unwrap_or_default();
        self.turn = Turn::Starting;
        self.request(
            "turn/start",
            json!({"threadId": thread, "input": [{"type": "text", "text": text}]}),
            Request::TurnStart,
        )
    }
    /// A send that waited for the handshake goes out now.
    fn flush_deferred(&mut self) {
        if let Turn::Deferred(text) = &self.turn {
            let text = text.clone();
            let frame = self.turn_start(&text);
            self.queued.push_back(Parsed::Reply(frame));
        }
    }
    fn response(&mut self, kind: Request, result: Result<Value, RpcError>) -> Parsed {
        match (kind, result) {
            (Request::Initialize, Ok(v)) => {
                self.version = version(&v["userAgent"]);
                let mut frames = self.rpc.notification("initialized", json!({}));
                frames.extend(self.open_thread());
                Parsed::Reply(frames)
            }
            (Request::Thread, Ok(v)) => self.thread_opened(&v),
            (Request::Thread, Err(_)) if self.resume.is_some() => Parsed::Fatal(RESUME_REFUSED),
            (Request::Thread, Err(_)) => Parsed::Fatal(STRINGS.start_failed),
            (Request::Inject, Ok(_)) => {
                self.injecting = false;
                self.flush_deferred();
                Parsed::Instructed
            }
            (Request::TurnStart, Ok(v)) => {
                let Some(id) = v["turn"]["id"].as_str() else {
                    return Parsed::Failed(BAD_STREAM);
                };
                if self.turn == Turn::Starting {
                    self.turn = Turn::Running(id.into());
                }
                Parsed::Accepted
            }
            (Request::TurnStart, Err(_)) => {
                self.turn = Turn::Idle;
                Parsed::Failed(REPLY_FAILED)
            }
            // The turn's own `turn/completed` settles the stop.
            (Request::Interrupt, _) => Parsed::Ignore,
            (Request::Initialize | Request::Inject, Err(_)) => Parsed::Failed(BAD_STREAM),
        }
    }
    /// The thread is open. A fresh one holds the instructions; a resumed one
    /// is sent them first when they changed since it last got them.
    fn thread_opened(&mut self, v: &Value) -> Parsed {
        let Some(id) = v["thread"]["id"]
            .as_str()
            .filter(|id| crate::agents::valid_conversation_id(id))
        else {
            return Parsed::Failed(BAD_STREAM);
        };
        if self.resume.as_ref().is_some_and(|resume| resume != id) {
            return Parsed::Failed(STRINGS.other_conversation);
        }
        self.thread = Some(id.into());
        if self.resume.is_none() {
            self.queued.push_back(Parsed::Instructed);
        } else if !self.instructed {
            self.injecting = true;
            let item = json!({
                "type": "message",
                "role": "developer",
                "content": [{"type": "input_text", "text": self.instructions}],
            });
            let frame = self.request(
                "thread/inject_items",
                json!({"threadId": id, "items": [item]}),
                Request::Inject,
            );
            self.queued.push_back(Parsed::Reply(frame));
        }
        if !self.injecting {
            self.flush_deferred();
        }
        Parsed::Identity(id.into())
    }
    fn current(&self, turn: &Value) -> bool {
        matches!(&self.turn, Turn::Running(id) if turn.as_str() == Some(id))
    }
    fn notification(&mut self, method: &str, params: &Value) -> Parsed {
        // Subagent threads share the connection; only the Lead's counts.
        if self.thread.is_none() || params["threadId"].as_str() != self.thread.as_deref() {
            return Parsed::Ignore;
        }
        match method {
            "turn/started" => self.turn_started(params),
            "item/started" => self.item(params, false),
            "item/completed" => self.item(params, true),
            "turn/completed" => self.turn_completed(params),
            // `willRetry: true` is a reconnect notice, not a failure.
            "error" if params["willRetry"].as_bool() == Some(false) => {
                if self.current(&params["turnId"]) {
                    self.errored = true;
                }
                Parsed::Ignore
            }
            "serverRequest/resolved" => Parsed::Cancelled(params["requestId"].to_string()),
            _ => Parsed::Ignore,
        }
    }
    /// Our turn starting confirms the thread, which Codex keeps from its
    /// first turn on. One while idle is a turn Codex started itself.
    fn turn_started(&mut self, params: &Value) -> Parsed {
        let Some(id) = params["turn"]["id"].as_str() else {
            return Parsed::Failed(BAD_STREAM);
        };
        match &self.turn {
            Turn::Running(current) if current == id => Parsed::Confirmed,
            Turn::Starting => {
                self.turn = Turn::Running(id.into());
                Parsed::Confirmed
            }
            Turn::Idle => {
                self.begin();
                self.turn = Turn::Running(id.into());
                Parsed::Started
            }
            _ => Parsed::Ignore,
        }
    }
    /// The final-response filter: `final_answer` messages win; otherwise the
    /// last message after the last tool.
    fn item(&mut self, params: &Value, completed: bool) -> Parsed {
        if !self.current(&params["turnId"]) {
            return Parsed::Ignore;
        }
        let item = &params["item"];
        let (Some(kind), Some(id)) = (item["type"].as_str(), item["id"].as_str()) else {
            return Parsed::Failed(BAD_STREAM);
        };
        if kind == "agentMessage" {
            if !completed {
                return Parsed::Ignore;
            }
            let Some(text) = item["text"].as_str() else {
                return Parsed::Failed(BAD_STREAM);
            };
            if !text.trim().is_empty() {
                if item["phase"] == "final_answer" {
                    self.finals.push(text.into());
                } else {
                    self.fallback = Some(text.into());
                }
            }
        } else if !NEUTRAL_ITEMS.contains(&kind) {
            if kind == "commandExecution" {
                if completed {
                    self.running.remove(id);
                } else {
                    self.running.insert(id.into());
                }
            }
            self.fallback = None;
        }
        Parsed::Ignore
    }
    fn turn_completed(&mut self, params: &Value) -> Parsed {
        let turn = &params["turn"];
        if !self.current(&turn["id"]) && self.turn != Turn::Starting {
            return Parsed::Ignore;
        }
        self.turn = Turn::Idle;
        let interrupting = std::mem::take(&mut self.interrupting);
        let lingering = !std::mem::take(&mut self.running).is_empty();
        let errored = std::mem::take(&mut self.errored);
        let finals = std::mem::take(&mut self.finals);
        let fallback = self.fallback.take();
        match turn["status"].as_str() {
            // A stopped command keeps running; only the process's end stops it.
            _ if interrupting && lingering => Parsed::Lingering,
            _ if interrupting => Parsed::Final(None),
            Some("completed") if !errored => Parsed::Final(if finals.is_empty() {
                fallback
            } else {
                Some(finals.join("\n\n"))
            }),
            Some("completed" | "failed") => Parsed::Failed(REPLY_FAILED),
            Some("interrupted") => Parsed::Final(None),
            _ => Parsed::Failed(BAD_STREAM),
        }
    }
    /// Only a question from the Lead's running turn is shown. Everything else
    /// Codex asks, approvals included, is never answered for the author.
    fn server_request(&mut self, id: &Value, method: &str, params: &Value) -> Parsed {
        if method == "item/tool/requestUserInput"
            && params["threadId"].as_str() == self.thread.as_deref()
            && self.current(&params["turnId"])
        {
            return question(id, params);
        }
        Parsed::Blocked
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CodexQuestion {
    id: String,
    #[serde(default)]
    header: String,
    question: String,
    #[serde(default)]
    options: Option<Vec<ChatQuestionOption>>,
    #[serde(default)]
    is_secret: bool,
}

/// A `request_user_input` Shika can show: 1 to 4 questions with distinct
/// ids and text, and at most 4 options each. The dialog keys answers by text.
/// A secret answer would show as typed.
fn question(id: &Value, params: &Value) -> Parsed {
    let Ok(raw) = serde_json::from_value::<Vec<CodexQuestion>>(params["questions"].clone()) else {
        return Parsed::Blocked;
    };
    let (mut ids, mut texts) = (HashSet::new(), HashSet::new());
    if raw.is_empty()
        || raw.len() > 4
        || raw.iter().any(|q| {
            q.question.is_empty()
                || q.is_secret
                || q.options.as_ref().is_some_and(|o| o.len() > 4)
                || !ids.insert(q.id.as_str())
                || !texts.insert(q.question.as_str())
        })
    {
        return Parsed::Blocked;
    }
    Parsed::Question(ChatQuestion {
        request_id: id.to_string(),
        questions: raw
            .into_iter()
            .map(|q| ChatQuestionItem {
                question: q.question,
                header: q.header,
                options: q.options.unwrap_or_default(),
                multi_select: false,
            })
            .collect(),
        input: params.clone(),
    })
}

/// `{"<question id>": {"answers": [...]}}` from the dialog's answers, which
/// are keyed by question text. Cancel is the empty map.
fn answers(input: Option<&Value>) -> Map<String, Value> {
    let mut answers = Map::new();
    let Some(input) = input else {
        return answers;
    };
    for q in input["questions"].as_array().into_iter().flatten() {
        let (Some(id), Some(text)) = (q["id"].as_str(), q["question"].as_str()) else {
            continue;
        };
        let given = match &input["answers"][text] {
            Value::String(answer) => vec![Value::String(answer.clone())],
            Value::Array(list) => list.iter().filter(|a| a.is_string()).cloned().collect(),
            _ => continue,
        };
        answers.insert(id.into(), json!({"answers": given}));
    }
    answers
}

/// The version in `userAgent` ("shika/0.162.1 (Mac OS ...) ..."), if plain.
fn version(user_agent: &Value) -> Option<String> {
    user_agent
        .as_str()?
        .split_whitespace()
        .next()?
        .split_once('/')
        .map(|(_, version)| version)
        .filter(|s| {
            !s.is_empty()
                && s.len() <= 64
                && s.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        })
        .map(str::to_string)
}

/// `codex app-server` on stdio. Questions need the under-development
/// `default_mode_request_user_input`; goals are off, so turns come only from
/// the author and the doorbell. The trust override keeps `thread/start` from
/// writing trust into the author's `config.toml`.
impl LeadProtocol for CodexAppServer {
    fn cli(&self) -> &'static str {
        "codex"
    }
    fn strings(&self) -> &'static Strings {
        &STRINGS
    }
    fn args(&self, launch: &ChatLaunch) -> Vec<String> {
        [
            "app-server",
            "--enable",
            "default_mode_request_user_input",
            "--disable",
            "goals",
            "-c",
        ]
        .into_iter()
        .map(str::to_string)
        .chain([crate::agents::codex_trust_override(&launch.cwd)])
        .collect()
    }
    fn handshake(&mut self, launch: &ChatLaunch) -> Vec<u8> {
        *self = Self::new();
        // A conversation the transport holds is resumed, also after a stop
        // before Codex confirmed it.
        self.resume = (!launch.conversation.is_empty()).then(|| launch.conversation.clone());
        self.cwd = launch.cwd.to_string_lossy().into_owned();
        self.instructions = launch.instructions.clone();
        self.instructed = launch.instructed;
        let client = json!({"name": "shika", "version": env!("CARGO_PKG_VERSION")});
        self.request(
            "initialize",
            json!({"clientInfo": client}),
            Request::Initialize,
        )
    }
    fn ready(&self) -> bool {
        self.thread.is_some() && !self.injecting
    }
    fn version(&self) -> Option<&str> {
        self.version.as_deref()
    }
    fn send(&mut self, text: &str) -> Vec<u8> {
        self.begin();
        if self.ready() {
            self.turn_start(text)
        } else {
            self.turn = Turn::Deferred(text.into());
            Vec::new()
        }
    }
    fn write_accepts(&self) -> bool {
        false
    }
    fn answer(&mut self, request_id: &str, answer: Option<Value>) -> Vec<u8> {
        let id = serde_json::from_str(request_id).unwrap_or_else(|_| request_id.into());
        self.rpc
            .respond(&id, json!({"answers": answers(answer.as_ref())}))
    }
    fn interrupt(&mut self) -> Interrupt {
        self.finals.clear();
        self.fallback = None;
        match &self.turn {
            Turn::Idle => Interrupt::Idle,
            Turn::Deferred(_) => {
                self.turn = Turn::Idle;
                Interrupt::Idle
            }
            // Codex has the turn but Shika cannot name it yet.
            Turn::Starting => Interrupt::Signal,
            Turn::Running(turn) => {
                let params = json!({"threadId": self.thread, "turnId": turn});
                self.interrupting = true;
                Interrupt::Frame(self.request("turn/interrupt", params, Request::Interrupt))
            }
        }
    }
    fn shutdown(&self) -> Shutdown {
        Shutdown::CloseStdin
    }
    /// Closing stdin or SIGTERM ends Codex with its tools; SIGKILL would
    /// leave them running in their own process groups.
    fn stop_signal(&self) -> i32 {
        libc::SIGTERM
    }
    fn parse(&mut self, line: &[u8]) -> Parsed {
        match self.rpc.decode(line) {
            Message::Invalid => Parsed::Failed(BAD_STREAM),
            Message::Stray => Parsed::Ignore,
            Message::Response { kind, result } => self.response(kind, result),
            Message::Request { id, method, params } => self.server_request(&id, &method, &params),
            Message::Notification { method, params } => self.notification(&method, &params),
        }
    }
    fn poll(&mut self, now: Instant) -> Parsed {
        if let Some(parsed) = self.queued.pop_front() {
            return parsed;
        }
        let late = self.rpc.expired(now);
        if !self.expired && late.iter().any(|kind| *kind != Request::Interrupt) {
            self.expired = true;
            return Parsed::Failed(BAD_STREAM);
        }
        Parsed::Ignore
    }
}

#[cfg(test)]
#[path = "codex_tests.rs"]
mod tests;
