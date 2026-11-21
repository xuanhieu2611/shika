//! A generic Agent Client Protocol (ACP 1) client: JSON-RPC 2.0 over stdio,
//! one process per Lead that keeps running across turns and Stop
//! generating. Each turn is one `session/prompt` request that stays open
//! until the turn ends. What one agent adds (its launch, how a new session
//! gets the Lead's instructions, its own extension requests) is a
//! [`Flavor`]; Cursor CLI's is `cursor.rs`.
//!
//! Shika advertises no filesystem or terminal capability, so the agent uses
//! its own tools. A `session/load` replays the whole conversation as updates
//! before its response; all of it is swallowed, since the transcript is
//! Shika's own.
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::jsonrpc::{Message, Rpc, RpcError};
use super::protocol::{Interrupt, LeadProtocol, Shutdown, Strings};
use super::{ChatLaunch, ChatQuestion, ChatQuestionItem, Parsed};

/// The ACP version Shika speaks; an agent that answers another is unreadable.
const PROTOCOL_VERSION: u64 = 1;
/// How long `initialize` and `session/new` may take to answer.
const DEADLINE: Duration = Duration::from_secs(10);
/// `session/load` replays the whole conversation before it answers, which
/// grows with the Lead's lifetime.
const LOAD_DEADLINE: Duration = Duration::from_secs(60);
/// The row a turn carrying Shika's memory line shows (`Parsed::ShikaInput`).
pub(super) const MEMORY_ROW: &str = "Shika sent the Lead its current memory notes.";
/// The JSON-RPC error Shika answers a blocked unknown request with when the
/// author stops the turn it belongs to.
const CANCELLED: i64 = -32800;

/// The messages one ACP agent shows, beyond the shared [`Strings`].
pub(super) struct Texts {
    pub strings: Strings,
    /// A turn failed; the agent's own bounded reason follows in parentheses.
    pub reply_failed: &'static str,
    /// `session/load` failed, or the agent cannot load sessions.
    pub resume_refused: &'static str,
    /// `stopReason: max_tokens`.
    pub cut_off: &'static str,
    /// `stopReason: max_turn_requests`.
    pub too_many_steps: &'static str,
    /// `stopReason: refusal`.
    pub refused: &'static str,
    /// [`Flavor::place_instructions`] failed.
    pub instructions_failed: &'static str,
}

/// What an extension request (a method outside ACP's own) is to Shika.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Extension {
    /// A question the native dialog shows.
    Question(Vec<ChatQuestionItem>),
    /// A report the agent does not wait on, answered at once with an empty
    /// success. Never a permission or a question.
    Report,
    /// Anything else: never answered for the author.
    Blocked,
}

/// One ACP agent's own parts. ACP takes no system prompt, so how a new
/// session receives the Lead's instructions is the flavor's.
pub(super) trait Flavor: Send {
    /// The preset id the history is bound to.
    fn cli(&self) -> &'static str;
    fn texts(&self) -> &'static Texts;
    fn args(&self, launch: &ChatLaunch) -> Vec<String>;
    /// Makes `instructions` part of the session that `session/new` is about
    /// to create in `cwd`. False when it could not, which fails the start.
    fn place_instructions(&mut self, cwd: &Path, instructions: &str) -> bool;
    /// `session/new` answered or failed: undoes `place_instructions`.
    fn remove_instructions(&mut self);
    fn extension(&self, method: &str, params: &Value) -> Extension;
    /// The result answering an [`Extension::Question`] from the dialog's
    /// input (the request's params plus `answers`), or cancelling it.
    fn answer(&self, answer: Option<&Value>) -> Value;
    /// The result that cancels a blocked extension request when the author
    /// stops its turn, or None to answer with a JSON-RPC error.
    fn cancel(&self, method: &str) -> Option<Value>;
}

/// What one of our requests was for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Request {
    Initialize,
    New,
    Load,
    Prompt,
}

/// Shika's turn, as far as the agent knows it.
#[derive(Debug, PartialEq, Eq)]
enum Turn {
    Idle,
    /// Sent before the session opened; `session/prompt` goes out once it has.
    Deferred(String),
    /// `session/prompt` is written; its response ends the turn.
    Prompting,
}

/// One per Lead. The flavor lives as long as the Lead's transport;
/// `handshake` starts the `Run` over for every process.
pub(super) struct AcpClient {
    flavor: Box<dyn Flavor>,
    run: Run,
}

/// What one process knows.
struct Run {
    rpc: Rpc<Request>,
    /// The session to load, or None to create one.
    resume: Option<String>,
    cwd: PathBuf,
    instructions: String,
    notes: Vec<String>,
    /// The session already holds the current instructions and notes.
    instructed: bool,
    version: Option<String>,
    /// `initialize` answered and `session/load` was sent: the replay may
    /// take longer than the handshake check allows.
    loading: bool,
    session: Option<String>,
    turn: Turn,
    interrupting: bool,
    /// This turn's prompt carries the memory line.
    delivering: bool,
    /// The agent has produced output for this turn, so it keeps the session.
    produced: bool,
    /// `agent_message_chunk` text since the last tool event.
    candidate: String,
    /// Open questions by request id; only the first is shown.
    questions: VecDeque<(Value, ChatQuestion)>,
    /// Open requests never answered for the author, with their method.
    blocked: Vec<(Value, String)>,
    /// Further results of the last line, returned by `poll`.
    queued: VecDeque<Parsed>,
    /// A request expired; reported once.
    expired: bool,
}

impl Run {
    fn new() -> Self {
        Self {
            rpc: Rpc::new(true),
            resume: None,
            cwd: PathBuf::new(),
            instructions: String::new(),
            notes: Vec::new(),
            instructed: false,
            version: None,
            loading: false,
            session: None,
            turn: Turn::Idle,
            interrupting: false,
            delivering: false,
            produced: false,
            candidate: String::new(),
            questions: VecDeque::new(),
            blocked: Vec::new(),
            queued: VecDeque::new(),
            expired: false,
        }
    }
}

impl AcpClient {
    pub(super) fn new(flavor: Box<dyn Flavor>) -> Self {
        Self {
            flavor,
            run: Run::new(),
        }
    }
    fn texts(&self) -> &'static Texts {
        self.flavor.texts()
    }
    fn bad_stream(&self) -> Parsed {
        Parsed::Failed(self.texts().strings.unreadable)
    }
    fn open(&self) -> bool {
        self.run.session.is_some()
    }
    /// A new turn on the agent's side: per-turn decoder state starts over.
    fn begin(&mut self) {
        self.run.interrupting = false;
        self.run.delivering = false;
        self.run.produced = false;
        self.run.candidate.clear();
        self.run.questions.clear();
        self.run.blocked.clear();
    }
    fn prompt(&mut self, blocks: &[&str]) -> Vec<u8> {
        let prompt = blocks
            .iter()
            .map(|text| json!({"type": "text", "text": text}))
            .collect::<Vec<_>>();
        self.run.turn = Turn::Prompting;
        self.run.rpc.request_open(
            "session/prompt",
            json!({"sessionId": self.run.session, "prompt": prompt}),
            Request::Prompt,
        )
    }
    fn response(&mut self, kind: Request, result: Result<Value, RpcError>) -> Parsed {
        match (kind, result) {
            (Request::Initialize, Ok(v)) => self.initialized(&v),
            (Request::Initialize, Err(_)) => self.bad_stream(),
            (Request::New, result) => {
                self.flavor.remove_instructions();
                match result {
                    Ok(v) => self.created(&v),
                    Err(e) => Parsed::Explained {
                        text: explained(self.texts().strings.start_failed, &e),
                        fatal: true,
                    },
                }
            }
            (Request::Load, Ok(_)) => self.loaded(),
            // The agent names a session it has no record of only this way;
            // a terminal-era Cursor chat id is one (Decision 2).
            (Request::Load, Err(e)) if e.code == -32602 && e.detail.contains("not found") => {
                Parsed::Missing(self.texts().resume_refused)
            }
            (Request::Load, Err(_)) => Parsed::Fatal(self.texts().resume_refused),
            (Request::Prompt, result) => self.prompt_ended(result),
        }
    }
    /// The agent's capabilities are known: create the session with the
    /// instructions in place, or load the saved one.
    fn initialized(&mut self, v: &Value) -> Parsed {
        if v["protocolVersion"].as_u64() != Some(PROTOCOL_VERSION) {
            return self.bad_stream();
        }
        self.run.version = version(&v["agentInfo"]["version"]);
        let cwd = self.run.cwd.to_string_lossy().into_owned();
        let params = json!({"cwd": cwd, "mcpServers": []});
        let Some(id) = self.run.resume.clone() else {
            if !self
                .flavor
                .place_instructions(&self.run.cwd, &self.run.instructions)
            {
                return Parsed::Fatal(self.texts().instructions_failed);
            }
            let now = Instant::now();
            let frame = self
                .run
                .rpc
                .request("session/new", params, Request::New, DEADLINE, now);
            return Parsed::Reply(frame);
        };
        if v["agentCapabilities"]["loadSession"] != true {
            return Parsed::Missing(self.texts().resume_refused);
        }
        let mut params = params;
        params["sessionId"] = id.into();
        self.run.loading = true;
        let now = Instant::now();
        let frame = self
            .run
            .rpc
            .request("session/load", params, Request::Load, LOAD_DEADLINE, now);
        Parsed::Reply(frame)
    }
    /// `session/new` answered: the session holds the instructions placed for
    /// it. The id is saved only once the agent keeps the session, at the
    /// first output of a turn (a session with no prompt cannot be loaded).
    fn created(&mut self, v: &Value) -> Parsed {
        let Some(id) = v["sessionId"]
            .as_str()
            .filter(|id| crate::agents::valid_conversation_id(id))
        else {
            return self.bad_stream();
        };
        self.run.session = Some(id.into());
        self.run.queued.push_back(Parsed::Instructed);
        self.flush_deferred(None);
        Parsed::Identity(id.into())
    }
    /// The replay is over. A session whose instructions or notes changed
    /// since it last got them is sent the current notes first, with any
    /// send that waited, or as a Shika turn of its own.
    fn loaded(&mut self) -> Parsed {
        self.run.loading = false;
        self.run.session = self.run.resume.clone();
        if self.run.instructed {
            self.flush_deferred(None);
            return Parsed::Ignore;
        }
        let memory = memory_text(&self.run.notes);
        if self.run.turn == Turn::Idle {
            self.begin();
            self.run.delivering = true;
            let frame = self.prompt(&[&memory]);
            self.run.queued.push_back(Parsed::ShikaInput(MEMORY_ROW));
            self.run.queued.push_back(Parsed::Reply(frame));
        } else {
            self.run.queued.push_back(Parsed::ShikaInput(MEMORY_ROW));
            self.flush_deferred(Some(&memory));
        }
        Parsed::Ignore
    }
    /// A send that waited for the session goes out now, after `memory` when
    /// the session needs it, and is accepted once written.
    fn flush_deferred(&mut self, memory: Option<&str>) {
        let Turn::Deferred(text) = &self.run.turn else {
            return;
        };
        let text = text.clone();
        let frame = match memory {
            Some(memory) => {
                self.run.delivering = true;
                self.prompt(&[memory, &text])
            }
            None => self.prompt(&[&text]),
        };
        self.run.queued.push_back(Parsed::Reply(frame));
        self.run.queued.push_back(Parsed::Accepted);
    }
    /// The final-response filter: the `agent_message_chunk` text after the
    /// last tool event, released only by `end_turn`. Thoughts, plans,
    /// commands, titles and mode or config updates are ignored.
    fn update(&mut self, params: &Value) -> Parsed {
        if self.run.loading
            || self.run.turn != Turn::Prompting
            || params["sessionId"].as_str() != self.run.session.as_deref()
        {
            return Parsed::Ignore;
        }
        let update = &params["update"];
        let Some(kind) = update["sessionUpdate"].as_str() else {
            return self.bad_stream();
        };
        match kind {
            "agent_message_chunk" => {
                if update["content"]["type"] == "text"
                    && let Some(text) = update["content"]["text"].as_str()
                {
                    self.run.candidate.push_str(text);
                }
            }
            "tool_call" | "tool_call_update" => self.run.candidate.clear(),
            "agent_thought_chunk" | "plan" => {}
            _ => return Parsed::Ignore,
        }
        self.first_output()
    }
    /// The turn's first output: the agent has written the session by now
    /// (its store appears with the first output, also when the process is
    /// killed right after), so the id can be saved, and a memory line it
    /// carried counts as delivered.
    fn first_output(&mut self) -> Parsed {
        if std::mem::replace(&mut self.run.produced, true) {
            return Parsed::Ignore;
        }
        if self.run.delivering {
            self.run.queued.push_back(Parsed::Instructed);
        }
        Parsed::Confirmed
    }
    fn prompt_ended(&mut self, result: Result<Value, RpcError>) -> Parsed {
        let interrupting = self.run.interrupting;
        let candidate = std::mem::take(&mut self.run.candidate);
        self.begin();
        self.run.turn = Turn::Idle;
        if interrupting {
            return Parsed::Final(None);
        }
        let texts = self.texts();
        let v = match result {
            Ok(v) => v,
            Err(e) => {
                return Parsed::Explained {
                    text: explained(texts.reply_failed, &e),
                    fatal: false,
                };
            }
        };
        match v["stopReason"].as_str() {
            Some("end_turn") => Parsed::Final((!candidate.trim().is_empty()).then_some(candidate)),
            Some("cancelled") => Parsed::Final(None),
            Some("max_tokens") => Parsed::Failed(texts.cut_off),
            Some("max_turn_requests") => Parsed::Failed(texts.too_many_steps),
            Some("refusal") => Parsed::Failed(texts.refused),
            _ => self.bad_stream(),
        }
    }
    /// The agent asks something and waits. Only a question from the running
    /// turn is shown and a report is acknowledged; permissions and anything
    /// unknown are never answered for the author.
    fn server_request(&mut self, id: Value, method: &str, params: &Value) -> Parsed {
        let extension = if method.starts_with("session/") || !self.open() {
            Extension::Blocked
        } else {
            self.flavor.extension(method, params)
        };
        match extension {
            Extension::Report => Parsed::Reply(self.run.rpc.respond(&id, json!({}))),
            Extension::Question(questions) if self.run.turn == Turn::Prompting => {
                let question = ChatQuestion {
                    request_id: id.to_string(),
                    questions,
                    input: params.clone(),
                };
                self.run.questions.push_back((id, question.clone()));
                if self.run.questions.len() == 1 {
                    Parsed::Question(question)
                } else {
                    Parsed::Ignore
                }
            }
            _ => {
                self.run.blocked.push((id, method.into()));
                Parsed::Blocked
            }
        }
    }
    /// The answers that close every open question and blocked request of a
    /// turn the author stops: ACP asks a client that cancels a prompt to
    /// answer its pending permission requests `cancelled`.
    fn cancel_open(&mut self) -> Vec<u8> {
        let mut bytes = Vec::new();
        for (id, _) in std::mem::take(&mut self.run.questions) {
            bytes.extend(self.run.rpc.respond(&id, self.flavor.answer(None)));
        }
        for (id, method) in std::mem::take(&mut self.run.blocked) {
            let result = if method == "session/request_permission" {
                Some(json!({"outcome": {"outcome": "cancelled"}}))
            } else {
                self.flavor.cancel(&method)
            };
            bytes.extend(match result {
                Some(result) => self.run.rpc.respond(&id, result),
                None => self.run.rpc.respond_error(&id, CANCELLED, "Cancelled"),
            });
        }
        bytes
    }
}

/// `text` followed by the agent's bounded reason, when it gave one.
fn explained(text: &str, error: &RpcError) -> String {
    let reason = if error.detail.is_empty() {
        &error.message
    } else {
        &error.detail
    };
    if reason.is_empty() {
        text.into()
    } else {
        format!("{text} ({reason})")
    }
}

/// The one `[shika]` line a resumed session gets when its notes may have
/// changed since it was created or last sent them. The notes replace the
/// list the session holds from its creation.
pub(super) fn memory_text(notes: &[String]) -> String {
    if notes.is_empty() {
        return "[shika] You have no memory notes now; any earlier ones were removed.".into();
    }
    let mut text =
        String::from("[shika] These are your memory notes now, in place of any earlier list:\n");
    for note in notes {
        text.push_str("\n- ");
        text.push_str(note);
    }
    text
}

/// `agentInfo.version`, if the agent sends one and it is plain.
fn version(value: &Value) -> Option<String> {
    value
        .as_str()
        .filter(|s| {
            !s.is_empty()
                && s.len() <= 64
                && s.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        })
        .map(str::to_string)
}

impl LeadProtocol for AcpClient {
    fn cli(&self) -> &'static str {
        self.flavor.cli()
    }
    fn strings(&self) -> &'static Strings {
        &self.texts().strings
    }
    fn args(&self, launch: &ChatLaunch) -> Vec<String> {
        self.flavor.args(launch)
    }
    fn handshake(&mut self, launch: &ChatLaunch) -> Vec<u8> {
        // Leftovers from a process that ended inside `session/new`.
        self.flavor.remove_instructions();
        self.run = Run::new();
        // A conversation the transport holds is loaded, also after a stop
        // before the agent kept it.
        self.run.resume = (!launch.conversation.is_empty()).then(|| launch.conversation.clone());
        self.run.cwd = launch.cwd.clone();
        self.run.instructions = launch.instructions.clone();
        self.run.notes = launch.notes.clone();
        self.run.instructed = launch.instructed;
        let capabilities = json!({
            "fs": {"readTextFile": false, "writeTextFile": false},
            "terminal": false,
        });
        let client = json!({"name": "shika", "version": env!("CARGO_PKG_VERSION")});
        let params = json!({
            "protocolVersion": PROTOCOL_VERSION,
            "clientCapabilities": capabilities,
            "clientInfo": client,
        });
        let now = Instant::now();
        self.run
            .rpc
            .request("initialize", params, Request::Initialize, DEADLINE, now)
    }
    fn ready(&self) -> bool {
        self.open() || self.run.loading
    }
    fn version(&self) -> Option<&str> {
        self.run.version.as_deref()
    }
    fn send(&mut self, text: &str) -> Vec<u8> {
        self.begin();
        if self.open() {
            self.prompt(&[text])
        } else {
            self.run.turn = Turn::Deferred(text.into());
            Vec::new()
        }
    }
    /// The prompt request stays open for the whole turn, so the write is the
    /// acceptance; one held for the session is accepted once written.
    fn write_accepts(&self) -> bool {
        !matches!(self.run.turn, Turn::Deferred(_))
    }
    fn answer(&mut self, request_id: &str, answer: Option<Value>) -> Vec<u8> {
        let Some(at) = self
            .run
            .questions
            .iter()
            .position(|(_, question)| question.request_id == request_id)
        else {
            return Vec::new();
        };
        let (id, _) = self.run.questions.remove(at).expect("position is in range");
        if at == 0
            && let Some((_, next)) = self.run.questions.front()
        {
            self.run.queued.push_back(Parsed::Question(next.clone()));
        }
        self.run
            .rpc
            .respond(&id, self.flavor.answer(answer.as_ref()))
    }
    fn interrupt(&mut self) -> Interrupt {
        self.run.candidate.clear();
        match &self.run.turn {
            Turn::Idle => Interrupt::Idle,
            Turn::Deferred(_) => {
                self.run.turn = Turn::Idle;
                Interrupt::Idle
            }
            Turn::Prompting => {
                let mut bytes = self.cancel_open();
                bytes.extend(
                    self.run
                        .rpc
                        .notification("session/cancel", json!({"sessionId": self.run.session})),
                );
                self.run.interrupting = true;
                Interrupt::Frame(bytes)
            }
        }
    }
    fn shutdown(&self) -> Shutdown {
        Shutdown::CloseStdin
    }
    /// Cursor did not exit on a closed stdin while an MCP server or a
    /// background shell of the session lived; SIGTERM ends it with them.
    fn stop_signal(&self) -> i32 {
        libc::SIGTERM
    }
    fn parse(&mut self, line: &[u8]) -> Parsed {
        match self.run.rpc.decode(line) {
            Message::Invalid => self.bad_stream(),
            Message::Stray => Parsed::Ignore,
            Message::Response { kind, result } => self.response(kind, result),
            Message::Request { id, method, params } => self.server_request(id, &method, &params),
            Message::Notification { method, params } if method == "session/update" => {
                self.update(&params)
            }
            Message::Notification { .. } => Parsed::Ignore,
        }
    }
    fn poll(&mut self, now: Instant) -> Parsed {
        if let Some(parsed) = self.run.queued.pop_front() {
            return parsed;
        }
        if !self.run.expired && !self.run.rpc.expired(now).is_empty() {
            self.run.expired = true;
            return self.bad_stream();
        }
        Parsed::Ignore
    }
}

#[cfg(test)]
#[path = "acp_tests.rs"]
mod tests;
