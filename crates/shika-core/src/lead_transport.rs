//! Claude Code's documented bidirectional stream JSON, never terminal bytes.
//! Candidates are private until a successful result. All IO runs on the actor.
use std::collections::HashSet;
use std::hash::Hasher;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use std::sync::{Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{Value, json};

use crate::lead_chat_store::{ChatContent, ChatOrigin, ChatRecord, ChatResult, ChatStore};

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

            "system" if v["subtype"] == "init" => {
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
            "control_request"
                if self.active
                    && v["request"]["subtype"] == "can_use_tool"
                    && v["request"]["tool_name"] == "AskUserQuestion" =>
            {
                let Some(id) = v["request_id"].as_str() else {
                    return Parsed::Failed(BAD_STREAM);
                };
                let Ok(questions) = serde_json::from_value::<Vec<ChatQuestionItem>>(
                    v["request"]["input"]["questions"].clone(),
                ) else {
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
            "control_request" => Parsed::Blocked,
            "control_cancel_request" => v["request_id"]
                .as_str()
                .map(|id| Parsed::Cancelled(id.into()))
                .unwrap_or(Parsed::Ignore),
            "assistant" if self.active => {
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
            "result" if self.active => {
                self.active = false;
                if !self.initialized
                    || !self.confirmed
                    || v["session_id"].as_str() != Some(&self.expected)
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
            _ => Parsed::Ignore,
        }
    }
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

struct Process {
    child: Child,
    stdin: ChildStdin,
    decoder: ClaudeStream,
    reader_done: bool,
    question: Option<String>,
    input_epoch: u64,
    started: Instant,
}
impl Process {
    fn spawn(
        launch: &ChatLaunch,
        resumed: bool,
        generation: u64,
        input: mpsc::SyncSender<CommandIn>,
    ) -> ChatResult<Self> {
        let mut cmd = Command::new(&launch.program);
        cmd.args(launch.args(resumed))
            .current_dir(&launch.cwd)
            .env("PATH", &launch.path)
            .env("PWD", &launch.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        for key in crate::worktree::GIT_REDIRECTS
            .into_iter()
            .chain(crate::pty::CONTROL_VARS)
        {
            cmd.env_remove(key);
        }
        // A parent Claude session must not disable this Lead's persistence.
        cmd.env_remove("CLAUDE_CODE_CHILD_SESSION")
            .env_remove("CLAUDECODE");
        for (k, v) in &launch.env {
            cmd.env(k, v);
        }
        let mut child = cmd
            .spawn()
            .map_err(|_| "Could not start Claude Code".to_string())?;
        let mut stdin = child.stdin.take().expect("piped stdin");
        let flags = unsafe { libc::fcntl(stdin.as_raw_fd(), libc::F_GETFL) };
        if flags < 0
            || unsafe { libc::fcntl(stdin.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) }
                < 0
        {
            unsafe {
                libc::kill(-(child.id() as i32), libc::SIGKILL);
            }
            let _ = child.wait();
            return Err("Could not open Claude Code input".into());
        }
        if stdin.write_all(b"{\"type\":\"control_request\",\"request_id\":\"shika-initialize\",\"request\":{\"subtype\":\"initialize\"}}\n").and_then(|_|stdin.flush()).is_err() {
            let _=child.kill();let _=child.wait();return Err("Could not initialize Claude Code".into());
        }
        let stdout = child.stdout.take().expect("piped stdout");
        let mut stderr = child.stderr.take().expect("piped stderr");
        thread::spawn(move || {
            let _ = std::io::copy(&mut stderr, &mut std::io::sink());
        });
        thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let mut line = Vec::new();
                let read = (&mut reader)
                    .take(MAX_FRAME + 1)
                    .read_until(b'\n', &mut line);
                match read {
                    Ok(0) => break,
                    Ok(_) if line.len() as u64 <= MAX_FRAME && line.ends_with(b"\n") => {
                        if input.send(CommandIn::Line(generation, line)).is_err() {
                            break;
                        }
                    }
                    _ => {
                        let _ = input.send(CommandIn::BadFrame(generation));
                        let _ = std::io::copy(&mut reader, &mut std::io::sink());
                        break;
                    }
                }
            }
            let _ = input.send(CommandIn::Eof(generation));
        });
        Ok(Self {
            child,
            stdin,
            decoder: ClaudeStream::new(launch.conversation.clone()),
            reader_done: false,
            question: None,
            input_epoch: 0,
            started: Instant::now(),
        })
    }
    fn signal(&mut self, signal: i32) {
        // The child is unreaped here, so its process group cannot be reused.
        unsafe {
            libc::kill(-(self.child.id() as i32), signal);
        }
    }
}
impl Drop for Process {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            self.signal(libc::SIGKILL);
            let _ = self.child.wait();
        }
    }
}

/// Nonblocking pipe IO lets Stop interrupt a child that stopped reading stdin.
/// Any partial/failed delivery is never replayed automatically.
fn write_frame(
    stdin: &mut ChildStdin,
    bytes: &[u8],
    cancel: &AtomicU64,
    epoch: u64,
) -> std::io::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut offset = 0;
    while offset < bytes.len() {
        if cancel.load(Ordering::Acquire) != epoch {
            return Err(std::io::ErrorKind::Interrupted.into());
        }
        match stdin.write(&bytes[offset..]) {
            Ok(0) => return Err(std::io::ErrorKind::WriteZero.into()),
            Ok(count) => offset += count,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    return Err(std::io::ErrorKind::TimedOut.into());
                }
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    stdin.flush()
}
struct Actor {
    stop: Arc<AtomicBool>,
    cancel_seen: u64,
    cancel: Arc<AtomicU64>,
    launch: ChatLaunch,
    store: ChatStore,
    output: mpsc::Sender<ChatEvent>,
    frames: mpsc::SyncSender<CommandIn>,
    child: Option<Process>,
    generation: u64,
    next_turn: i64,
    turn: Option<i64>,
    stopping: Option<Instant>,
    exiting: bool,
    save_failed: bool,
    pending: Vec<(i64, ChatContent)>,
    active_input: Option<(ChatOrigin, String)>,
    failed: bool,
}
impl Actor {
    fn emit(&self, event: ChatEvent) {
        if let Some(source) = &self.launch.usage {
            source.notify();
        }
        let _ = self.output.send(event);
    }
    /// Counted here too, so rows buffered after a save failure keep their
    /// numbers when a turn the CLI starts itself needs the next one.
    fn allocate_turn(&mut self) -> i64 {
        let turn = self.store.next_turn().unwrap_or(0).max(self.next_turn);
        self.next_turn = turn + 1;
        turn
    }
    fn persist(&mut self, turn: i64, content: ChatContent) -> bool {
        self.persist_batch(vec![(turn, content)])
    }
    fn persist_batch(&mut self, records: Vec<(i64, ChatContent)>) -> bool {
        if self.save_failed {
            self.pending.extend(records);
            return false;
        }
        match self.store.append_batch(records.clone()) {
            Ok(records) => {
                for record in records {
                    if record.content.text().is_some() {
                        self.emit(ChatEvent::Record(record));
                    }
                }
                true
            }
            Err(_) => {
                self.pending.extend(records);
                self.save_failed = true;
                self.emit(ChatEvent::SaveFailed);
                false
            }
        }
    }
    fn settle(&mut self, error: Option<&str>) {
        if self.turn.is_none()
            && let Some(text) = error
        {
            self.persist(
                0,
                ChatContent::Event {
                    text: text.into(),
                    key: None,
                },
            );
        }
        self.complete(None, error);
    }
    fn complete(&mut self, text: Option<String>, error: Option<&str>) {
        let input = self.active_input.take();
        if let Some(turn) = self.turn.take() {
            let mut records = Vec::new();
            if let Some(text) = text {
                records.push((turn, ChatContent::Lead { text }));
            }
            if let Some(text) = error {
                // A turn the CLI started has no input to offer again.
                if let Some((origin, input)) = input {
                    if text != "Reply stopped" && origin == ChatOrigin::Author {
                        self.emit(ChatEvent::Rejected(input));
                    }
                    self.emit(ChatEvent::DeliveryFailed);
                }
                records.push((
                    turn,
                    ChatContent::Event {
                        text: text.into(),
                        key: None,
                    },
                ));
            }
            records.push((turn, ChatContent::Settled));
            self.persist_batch(records);
            self.emit(ChatEvent::Status(ChatStatus::Ready));
        }
    }
    fn interrupt(&mut self) {
        if self.stopping.is_none() {
            if let Some(child) = &mut self.child {
                child.decoder.candidate = None;
                child.signal(libc::SIGINT);
                self.stopping = Some(Instant::now());
                self.emit(ChatEvent::Stopping);
            } else {
                self.settle(Some("Reply stopped"));
            }
        }
    }
    fn run(mut self, rx: mpsc::Receiver<CommandIn>, frames: mpsc::Receiver<CommandIn>) {
        loop {
            let cancel = self.cancel.load(Ordering::Acquire);
            if cancel != self.cancel_seen {
                self.cancel_seen = cancel;
                if self.turn.is_some() {
                    self.interrupt();
                }
            }
            if self.stop.load(Ordering::Acquire) {
                self.exiting = true;
                self.interrupt();
                if self.child.is_none() {
                    break;
                }
            }
            if self.stopping.is_none()
                && self.child.as_ref().is_some_and(|child| {
                    !child.decoder.initialized && child.started.elapsed() >= Duration::from_secs(10)
                })
            {
                self.failed = true;
                self.settle(Some(BAD_STREAM));
                self.emit(ChatEvent::Error(BAD_STREAM.into()));
                self.interrupt();
            }
            let input = match rx.try_recv() {
                Ok(command) => Ok(command),
                Err(mpsc::TryRecvError::Empty) => frames.recv_timeout(Duration::from_millis(25)),
                Err(mpsc::TryRecvError::Disconnected) => break,
            };
            match input {
                Ok(CommandIn::Answer { request_id, answer }) if self.stopping.is_none() => {
                    if let Some(child) = &mut self.child
                        && child.question.as_ref() == Some(&request_id)
                    {
                        let response = if let Some(input) = answer {
                            json!({"behavior":"allow","updatedInput":input})
                        } else {
                            json!({"behavior":"deny","message":"The author cancelled this question."})
                        };
                        let mut bytes=serde_json::to_vec(&json!({"type":"control_response","response":{"subtype":"success","request_id":request_id,"response":response}})).expect("JSON value");
                        bytes.push(b'\n');
                        if write_frame(&mut child.stdin, &bytes, &self.cancel, child.input_epoch)
                            .is_err()
                        {
                            self.settle(Some(
                                "The answer could not be delivered. Work may already have started.",
                            ));
                            self.interrupt();
                        } else {
                            child.question = None;
                            self.emit(ChatEvent::Status(ChatStatus::Working));
                        }
                    }
                }
                Ok(CommandIn::Stop) => {
                    self.exiting = true;
                    self.interrupt();
                    if self.child.is_none() {
                        break;
                    }
                }
                Ok(CommandIn::Interrupt) if self.turn.is_some() => self.interrupt(),
                Ok(CommandIn::RetrySave) => {
                    self.save_failed = false;
                    let pending = std::mem::take(&mut self.pending);
                    self.persist_batch(pending);
                    if !self.save_failed {
                        self.emit(ChatEvent::Saved);
                    }
                }
                Ok(CommandIn::Send {
                    text,
                    origin,
                    events,
                    epoch,
                }) if self.turn.is_none()
                    && self.stopping.is_none()
                    && !self.save_failed
                    && epoch == self.cancel.load(Ordering::Acquire) =>
                {
                    let turn = self.allocate_turn();
                    self.turn = Some(turn);
                    self.active_input = Some((origin.clone(), text.clone()));
                    let mut outgoing = Vec::new();
                    if origin == ChatOrigin::Author {
                        outgoing.push((turn, ChatContent::Author { text: text.clone() }));
                    }
                    for (text, key) in events {
                        outgoing.push((turn, ChatContent::Event { text, key }));
                    }
                    outgoing.push((turn, ChatContent::Outgoing { origin }));
                    if !self.persist_batch(outgoing) {
                        self.settle(Some("Message not sent"));
                        continue;
                    }
                    if self.child.is_none() {
                        self.generation += 1;
                        if self.launch.resumed {
                            self.emit(ChatEvent::Resuming);
                        }
                        match Process::spawn(
                            &self.launch,
                            self.launch.resumed,
                            self.generation,
                            self.frames.clone(),
                        ) {
                            Ok(child) => self.child = Some(child),
                            Err(_) => {
                                self.settle(Some(
                                    "Could not start Claude Code. Try sending again.",
                                ));
                                continue;
                            }
                        }
                    }
                    let child = self.child.as_mut().expect("started child");
                    child.decoder.begin();
                    child.input_epoch = epoch;
                    let mut bytes = serde_json::to_vec(
                        &json!({"type":"user", "message":{"role":"user", "content":text}}),
                    )
                    .expect("JSON value");
                    bytes.push(b'\n');
                    if write_frame(&mut child.stdin, &bytes, &self.cancel, epoch).is_err() {
                        if self.cancel.load(Ordering::Acquire) != epoch {
                            self.interrupt();
                            continue;
                        }
                        self.settle(Some("Delivery unknown. Work may already have started. Send again only when you are ready to retry."));
                        self.interrupt();
                    } else {
                        self.persist(turn, ChatContent::Accepted);
                        self.emit(ChatEvent::Accepted);
                        self.emit(ChatEvent::Status(ChatStatus::Working));
                    }
                }
                Ok(CommandIn::Send { .. }) => {
                    self.emit(ChatEvent::Error(
                        "The Lead is still replying or the conversation needs to be saved.".into(),
                    ));
                    self.emit(ChatEvent::DeliveryFailed);
                }
                Ok(CommandIn::Line(generation, line))
                    if generation == self.generation && self.stopping.is_none() =>
                {
                    let parsed = self
                        .child
                        .as_mut()
                        .map(|c| c.decoder.parse(&line))
                        .unwrap_or(Parsed::Ignore);
                    match parsed {
                        Parsed::Started if self.turn.is_none() => {
                            self.launch.resumed = true;
                            self.emit(ChatEvent::Confirmed(self.launch.conversation.clone()));
                            // No Outgoing row: Accepted alone marks a turn the
                            // CLI took without Shika input, so recovery and
                            // numbering treat it like any other accepted turn.
                            let turn = self.allocate_turn();
                            self.turn = Some(turn);
                            if let Some(child) = &mut self.child {
                                child.input_epoch = self.cancel.load(Ordering::Acquire);
                            }
                            self.persist(turn, ChatContent::Accepted);
                            self.emit(ChatEvent::Started);
                            self.emit(ChatEvent::Status(ChatStatus::Working));
                        }
                        Parsed::Confirmed | Parsed::Started => {
                            self.launch.resumed = true;
                            self.emit(ChatEvent::Confirmed(self.launch.conversation.clone()))
                        }
                        Parsed::Final(text) => self.complete(text, None),
                        Parsed::Blocked => {
                            self.emit(ChatEvent::Status(ChatStatus::Asking));
                            self.emit(ChatEvent::Error(UNSUPPORTED.into()));
                        }
                        Parsed::Question(question) => {
                            if let Some(child) = &mut self.child {
                                child.question = Some(question.request_id.clone());
                            }
                            self.emit(ChatEvent::Status(ChatStatus::Asking));
                            self.emit(ChatEvent::Question(question));
                        }
                        Parsed::Cancelled(id) => {
                            self.emit(ChatEvent::QuestionCancelled(id));
                        }
                        Parsed::Failed(error) => {
                            self.failed = error == BAD_STREAM
                                || error == "Claude Code returned a different conversation";
                            let display = if error == BAD_STREAM {
                                self.child
                                    .as_ref()
                                    .and_then(|c| c.decoder.version.as_ref())
                                    .map(|version| format!("{error} ({version})"))
                                    .unwrap_or_else(|| error.into())
                            } else {
                                error.into()
                            };
                            self.settle(Some(&display));
                            self.emit(ChatEvent::Error(display));
                            self.interrupt();
                        }
                        Parsed::Ignore => {}
                    }
                }
                Ok(CommandIn::BadFrame(generation)) if generation == self.generation => {
                    self.failed = true;
                    self.settle(Some(BAD_STREAM));
                    self.emit(ChatEvent::Error(BAD_STREAM.into()));
                    self.interrupt();
                }
                Ok(CommandIn::Eof(generation)) if generation == self.generation => {
                    if let Some(child) = &mut self.child {
                        child.reader_done = true;
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                _ => {}
            }
            if self
                .stopping
                .is_some_and(|at| at.elapsed() >= Duration::from_secs(2))
                && let Some(child) = &mut self.child
            {
                child.signal(libc::SIGKILL);
            }
            // Drain all queued output before observing exit, including the final result.
            let exited = self.child.as_mut().is_some_and(|c| {
                (c.reader_done
                    || self
                        .stopping
                        .is_some_and(|at| at.elapsed() >= Duration::from_secs(3)))
                    && c.child.try_wait().ok().flatten().is_some()
            });
            if exited {
                // EOF is ordered after every frame by the pipe reader. Interrupted
                // children have a bounded fallback when descendants hold a pipe.
                let stopped = self.stopping.take().is_some();
                self.child = None;
                if !stopped && self.turn.is_none() {
                    self.persist(
                        0,
                        ChatContent::Event {
                            text: "The Lead stopped unexpectedly".into(),
                            key: None,
                        },
                    );
                }
                if self.turn.is_some() {
                    self.settle(Some(if stopped { "Reply stopped" } else { INCOMPLETE }));
                }
                if self.exiting {
                    break;
                }
                if stopped {
                    self.emit(ChatEvent::Status(ChatStatus::Ready));
                }
                if !stopped || self.failed {
                    self.emit(ChatEvent::Exit);
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
