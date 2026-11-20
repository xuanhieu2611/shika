//! Pi's RPC mode (`pi --mode rpc`): LF-framed JSON commands, responses and
//! events, one process per Lead that keeps running across turns and Stop
//! generating. Only `agent_settled` ends a turn: retries, threshold
//! compaction and overflow recovery all come after an `agent_end`, and
//! `agent_end.willRetry` can be false before a recovery.
use std::borrow::Cow;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{Value, json};

use super::protocol::{Interrupt, LeadProtocol, Shutdown, Strings};
use super::{ChatLaunch, ChatQuestion, ChatQuestionItem, ChatQuestionOption, Parsed};

pub(super) const BAD_STREAM: &str = "This Pi version could not be read";
pub(super) const REPLY_FAILED: &str =
    "Pi could not complete the reply. Check its model, sign-in and connection, then retry.";
pub(super) const BUSY: &str = "Pi was busy and did not take the message. Send it again.";
pub(super) const CUT_OFF: &str = "Pi stopped the reply at the model's output limit.";
static STRINGS: Strings = Strings {
    unreadable: BAD_STREAM,
    other_conversation: "Pi returned a different conversation",
    start_failed: "Could not start Pi",
    input_failed: "Could not open Pi input",
    handshake_failed: "Could not initialize Pi",
    restart_failed: "Could not start Pi. Try sending again.",
};
/// Shika's Lead-only question tool (`pi-lead.ts`) and the title of the one
/// `input` dialog it opens, followed by its tool call id.
const QUESTION_TOOL: &str = "shika_ask_author";
const QUESTION_TITLE: &str = "shika-question:";
const EXTENSION: &str = include_str!("pi-lead.ts");
const STATE: &str = "shika-state";

/// Shika's turn, as far as Pi knows it.
#[derive(Debug, PartialEq, Eq)]
enum Turn {
    Idle,
    /// Sent before `get_state` answered; the prompt goes out once it has.
    Deferred(String),
    /// `prompt` with this id is written and Pi has not answered it.
    Prompted(String),
    /// Pi has the turn, one of ours or one it started, until `agent_settled`.
    Open,
}

/// How the latest assistant message of the run ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ending {
    /// `stop`, `toolUse` or `aborted`, or no assistant message yet.
    Clean,
    /// `error`: the message may hold partial text from a cut stream.
    Failed,
    /// `length`: the model's output limit.
    CutOff,
    Unknown,
}

/// What a dialog answers, which decides the `extension_ui_response` shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    /// Shika's question tool: the answers as one JSON `value`.
    Ask,
    /// `select`, `input` or an empty `editor`: the one answer as `value`.
    Text,
    /// `confirm`: `confirmed`.
    Confirm,
}

#[derive(Debug)]
struct Dialog {
    kind: Kind,
    /// Pi resolves a dialog with a `timeout` by itself at this point.
    expires: Option<Instant>,
    question: ChatQuestion,
}

/// One per Lead; `handshake` resets it for every process. It owns the
/// extension file, so the file lives exactly as long as the Lead's transport.
pub(super) struct PiRpc {
    extension: Option<Extension>,
    expected: String,
    ready: bool,
    prompts: u64,
    aborts: u64,
    turn: Turn,
    /// From Pi's first `agent_start` of a run to its `agent_settled`. One
    /// prompt can hold several `agent_start`/`agent_end` pairs.
    run: bool,
    interrupting: bool,
    /// This run's user message was seen, so Pi has written the session.
    confirmed: bool,
    candidate: Option<String>,
    ending: Ending,
    /// Arguments of running question tools by tool call id.
    asks: HashMap<String, Value>,
    /// Open dialogs; only the first is shown, the rest wait their turn.
    dialogs: VecDeque<Dialog>,
    /// Open dialogs the question dialog cannot show.
    blocked: Vec<String>,
    /// Further results of the last line, returned by `poll`.
    queued: VecDeque<Parsed>,
}

impl PiRpc {
    /// A decoder with no extension file, for tests.
    pub(super) fn new() -> Self {
        Self {
            extension: None,
            expected: String::new(),
            ready: false,
            prompts: 0,
            aborts: 0,
            turn: Turn::Idle,
            run: false,
            interrupting: false,
            confirmed: false,
            candidate: None,
            ending: Ending::Clean,
            asks: HashMap::new(),
            dialogs: VecDeque::new(),
            blocked: Vec::new(),
            queued: VecDeque::new(),
        }
    }
    /// The protocol a chat Lead runs, with its extension written. None when
    /// the file cannot be written.
    pub(super) fn installed() -> Option<Self> {
        Some(Self {
            extension: Some(Extension::install().ok()?),
            ..Self::new()
        })
    }
    /// A new run on Pi's side: per-run decoder state starts over.
    fn begin(&mut self) {
        self.run = true;
        self.confirmed = false;
        self.candidate = None;
        self.ending = Ending::Clean;
        self.asks.clear();
    }
    fn prompt(&mut self, text: &str) -> Vec<u8> {
        self.prompts += 1;
        let id = format!("shika-prompt-{}", self.prompts);
        let bytes = frame(json!({"id": id, "type": "prompt", "message": text}));
        self.turn = Turn::Prompted(id);
        bytes
    }
    fn response(&mut self, v: &Value) -> Parsed {
        let id = v["id"].as_str();
        match v["command"].as_str() {
            Some("get_state") if id == Some(STATE) => self.state(v),
            Some("prompt") if matches!(&self.turn, Turn::Prompted(p) if Some(p.as_str()) == id) => {
                self.prompted(v)
            }
            // Pi could not parse a frame Shika wrote.
            Some("parse") => Parsed::Failed(BAD_STREAM),
            // `abort` answers after the settlement that ends the stop.
            _ => Parsed::Ignore,
        }
    }
    /// The handshake: Pi must hold the session Shika passed with
    /// `--session-id`. It creates a missing one silently, so this proves
    /// identity, not a resume.
    fn state(&mut self, v: &Value) -> Parsed {
        if self.ready {
            return Parsed::Ignore;
        }
        if v["success"] != true {
            return Parsed::Failed(BAD_STREAM);
        }
        match v["data"]["sessionId"].as_str() {
            Some(id) if id == self.expected => {
                self.ready = true;
                if let Turn::Deferred(text) = &self.turn {
                    let text = text.clone();
                    let bytes = self.prompt(&text);
                    self.queued.push_back(Parsed::Reply(bytes));
                }
                Parsed::Ignore
            }
            Some(_) => Parsed::Failed(STRINGS.other_conversation),
            None => Parsed::Failed(BAD_STREAM),
        }
    }
    /// Pi's answer to our `prompt`. Shika never sends `streamingBehavior`,
    /// so a busy Pi refuses instead of queueing.
    fn prompted(&mut self, v: &Value) -> Parsed {
        if v["success"] != true {
            self.turn = Turn::Idle;
            let busy = v["error"]
                .as_str()
                .is_some_and(|e| e.contains("already processing"));
            if self.run {
                // A run Pi started itself took the slot: it becomes the turn.
                self.turn = Turn::Open;
                self.queued.push_back(Parsed::Started);
            }
            return Parsed::Failed(if busy { BUSY } else { REPLY_FAILED });
        }
        match v["data"]["disposition"].as_str() {
            // `queued` needs `streamingBehavior`; read it as accepted anyway.
            Some("started" | "queued") => {
                self.turn = Turn::Open;
                Parsed::Accepted
            }
            // An extension command took it: no run and no settlement follow.
            Some("handled") => {
                self.turn = Turn::Idle;
                Parsed::Handled
            }
            _ => {
                self.turn = Turn::Idle;
                Parsed::Failed(BAD_STREAM)
            }
        }
    }
    /// The first `agent_start` of a run. One while Shika has no turn is a run
    /// Pi started itself (an extension's `sendUserMessage`); later ones in
    /// the same run are retries or recovery.
    fn agent_start(&mut self) -> Parsed {
        if self.run {
            return Parsed::Ignore;
        }
        self.begin();
        if self.turn == Turn::Idle {
            self.turn = Turn::Open;
            return Parsed::Started;
        }
        Parsed::Ignore
    }
    /// The final-response filter: the candidate is the text of the latest
    /// assistant message that stopped normally, and any tool after it drops
    /// it. `system`, `user` and `toolResult` messages are never candidates.
    fn message_end(&mut self, v: &Value) -> Parsed {
        if !self.run {
            return Parsed::Ignore;
        }
        let message = &v["message"];
        match message["role"].as_str() {
            Some("assistant") => {
                let Some(blocks) = message["content"].as_array() else {
                    return Parsed::Failed(BAD_STREAM);
                };
                self.candidate = None;
                self.ending = match message["stopReason"].as_str() {
                    Some("stop") => {
                        if !blocks.iter().any(|b| b["type"] == "toolCall") {
                            let text = blocks
                                .iter()
                                .filter(|b| b["type"] == "text")
                                .filter_map(|b| b["text"].as_str())
                                .filter(|t| !t.trim().is_empty())
                                .collect::<Vec<_>>()
                                .join("\n\n");
                            self.candidate = (!text.is_empty()).then_some(text);
                        }
                        Ending::Clean
                    }
                    Some("toolUse" | "aborted") => Ending::Clean,
                    Some("error") => Ending::Failed,
                    Some("length") => Ending::CutOff,
                    _ => Ending::Unknown,
                };
                Parsed::Ignore
            }
            Some("toolResult") => {
                self.candidate = None;
                Parsed::Ignore
            }
            // The session file exists from the first user message on, so a
            // resume of this id finds it.
            Some("user") if self.turn == Turn::Open && !self.confirmed => {
                self.confirmed = true;
                Parsed::Confirmed
            }
            _ => Parsed::Ignore,
        }
    }
    fn tool_started(&mut self, v: &Value) -> Parsed {
        if self.run {
            self.candidate = None;
            if v["toolName"] == QUESTION_TOOL
                && let Some(id) = v["toolCallId"].as_str()
            {
                self.asks.insert(id.into(), v["args"].clone());
            }
        }
        Parsed::Ignore
    }
    fn tool_ended(&mut self, v: &Value) -> Parsed {
        if let Some(id) = v["toolCallId"].as_str() {
            self.asks.remove(id);
        }
        Parsed::Ignore
    }
    fn settled(&mut self, v: &Value) -> Parsed {
        self.run = false;
        self.asks.clear();
        // Pi has nothing left to wait for; the app closes every dialog of the
        // Lead at Ready, so leftovers are dismissed rather than stranded.
        for id in self.take_dialogs() {
            self.queued.push_back(Parsed::Reply(cancel(&id)));
        }
        if self.turn != Turn::Open {
            return Parsed::Ignore;
        }
        self.turn = Turn::Idle;
        let interrupting = std::mem::take(&mut self.interrupting);
        let candidate = self.candidate.take();
        let Some(aborted) = v["aborted"].as_bool() else {
            return Parsed::Failed(BAD_STREAM);
        };
        if interrupting || aborted {
            return Parsed::Final(None);
        }
        // A failed run settles with `aborted: false`; only its last
        // assistant message says so.
        match self.ending {
            Ending::Clean => Parsed::Final(candidate),
            Ending::Failed => Parsed::Failed(REPLY_FAILED),
            Ending::CutOff => Parsed::Failed(CUT_OFF),
            Ending::Unknown => Parsed::Failed(BAD_STREAM),
        }
    }
    /// Every open dialog's id, shown, waiting or blocked, now forgotten.
    fn take_dialogs(&mut self) -> Vec<String> {
        let mut ids = self
            .dialogs
            .drain(..)
            .map(|d| d.question.request_id)
            .collect::<Vec<_>>();
        ids.append(&mut self.blocked);
        ids
    }
    /// `extension_ui_request`: dialogs block Pi until answered; the others
    /// are fire-and-forget status for a terminal UI.
    fn ui_request(&mut self, v: &Value) -> Parsed {
        let (Some(id), Some(method)) = (v["id"].as_str(), v["method"].as_str()) else {
            return Parsed::Failed(BAD_STREAM);
        };
        if matches!(
            method,
            "notify" | "setStatus" | "setWidget" | "setTitle" | "set_editor_text"
        ) {
            return Parsed::Ignore;
        }
        let Some(dialog) = self.dialog(id, method, v) else {
            self.blocked.push(id.into());
            return Parsed::Blocked;
        };
        self.dialogs.push_back(dialog);
        if self.dialogs.len() == 1 {
            Parsed::Question(self.dialogs[0].question.clone())
        } else {
            Parsed::Ignore
        }
    }
    /// The question dialog for a Pi dialog it can show: Shika's own question
    /// tool, a `select` of 1 to 4 options, a `confirm` as Yes or No, and an
    /// `input` or empty `editor` as a free-form answer. A prefilled editor
    /// would need its text edited, which the dialog cannot do.
    fn dialog(&self, id: &str, method: &str, v: &Value) -> Option<Dialog> {
        let title = v["title"].as_str().unwrap_or_default();
        let one = |question: String, options: Vec<ChatQuestionOption>| {
            vec![ChatQuestionItem {
                question,
                header: String::new(),
                options,
                multi_select: false,
            }]
        };
        let (kind, questions) = match method {
            "input" if title.starts_with(QUESTION_TITLE) => (
                Kind::Ask,
                asked(self.asks.get(&title[QUESTION_TITLE.len()..])?)?,
            ),
            _ if title.trim().is_empty() => return None,
            "select" => {
                let options = serde_json::from_value::<Vec<String>>(v["options"].clone()).ok()?;
                let distinct = options.iter().collect::<HashSet<_>>().len() == options.len();
                if options.is_empty() || options.len() > 4 || !distinct {
                    return None;
                }
                let options = options.into_iter().map(label).collect();
                (Kind::Text, one(title.into(), options))
            }
            "confirm" => {
                let message = v["message"].as_str().unwrap_or_default().trim();
                let question = if message.is_empty() {
                    title.into()
                } else {
                    format!("{title}\n\n{message}")
                };
                let options = vec![label("Yes".into()), label("No".into())];
                (Kind::Confirm, one(question, options))
            }
            "input" => (Kind::Text, one(title.into(), Vec::new())),
            "editor" if v["prefill"].as_str().unwrap_or_default().is_empty() => {
                (Kind::Text, one(title.into(), Vec::new()))
            }
            _ => return None,
        };
        let expires = v["timeout"]
            .as_u64()
            .filter(|ms| *ms > 0)
            .map(|ms| Instant::now() + Duration::from_millis(ms));
        Some(Dialog {
            kind,
            expires,
            question: ChatQuestion {
                request_id: id.into(),
                questions,
                input: json!({"method": method}),
            },
        })
    }
    /// Forgets an open dialog and shows the next one waiting, if any.
    fn close(&mut self, id: &str) -> Option<Kind> {
        self.blocked.retain(|blocked| blocked != id);
        let at = self
            .dialogs
            .iter()
            .position(|d| d.question.request_id == id)?;
        let dialog = self.dialogs.remove(at)?;
        if at == 0
            && let Some(next) = self.dialogs.front()
        {
            self.queued
                .push_back(Parsed::Question(next.question.clone()));
        }
        Some(dialog.kind)
    }
}

/// Shika's question tool's arguments, as its schema allows them and the
/// dialog shows them: 1 to 4 questions with distinct text, at most 4 options.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Ask {
    question: String,
    #[serde(default)]
    header: String,
    #[serde(default)]
    options: Vec<ChatQuestionOption>,
    #[serde(default)]
    multi_select: bool,
}

fn asked(args: &Value) -> Option<Vec<ChatQuestionItem>> {
    let asks = serde_json::from_value::<Vec<Ask>>(args["questions"].clone()).ok()?;
    let mut texts = HashSet::new();
    if asks.is_empty()
        || asks.len() > 4
        || asks.iter().any(|a| {
            a.question.trim().is_empty()
                || a.options.len() > 4
                || a.options.iter().any(|o| o.label.trim().is_empty())
                || !texts.insert(a.question.as_str())
        })
    {
        return None;
    }
    Some(
        asks.into_iter()
            .map(|a| ChatQuestionItem {
                question: a.question,
                header: a.header,
                options: a.options,
                multi_select: a.multi_select,
            })
            .collect(),
    )
}

fn label(label: String) -> ChatQuestionOption {
    ChatQuestionOption {
        label,
        description: String::new(),
    }
}

/// The one answer of a single-question dialog: a label or the author's text.
fn single(answers: &Value) -> Option<String> {
    match answers.as_object()?.values().next()? {
        Value::String(text) => Some(text.clone()),
        Value::Array(list) => Some(
            list.iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(", "),
        ),
        _ => None,
    }
}

fn frame(value: Value) -> Vec<u8> {
    let mut bytes = serde_json::to_vec(&value).expect("JSON value");
    bytes.push(b'\n');
    bytes
}

fn ui_response(id: &str, mut fields: Value) -> Vec<u8> {
    fields["type"] = "extension_ui_response".into();
    fields["id"] = id.into();
    frame(fields)
}

fn cancel(id: &str) -> Vec<u8> {
    ui_response(id, json!({"cancelled": true}))
}

/// Only the record type, so the largest records (`agent_end` repeats every
/// message of the run) are scanned without being built.
#[derive(Deserialize)]
struct Head<'a> {
    #[serde(rename = "type", borrow, default)]
    kind: Option<Cow<'a, str>>,
}

/// `agent_end` is never read, so one past `MAX_FRAME` is dropped unread.
/// Pi writes `type` first in every record.
fn oversized_skippable(prefix: &[u8]) -> bool {
    prefix.starts_with(br#"{"type":"agent_end""#)
}

/// `pi --mode rpc` in the Lead worktree with today's Lead flags and Shika's
/// question extension. `pi-activity.ts` is not loaded: status comes from the
/// RPC events, and Lead naming capture is off.
impl LeadProtocol for PiRpc {
    fn cli(&self) -> &'static str {
        "pi"
    }
    fn strings(&self) -> &'static Strings {
        &STRINGS
    }
    fn args(&self, launch: &ChatLaunch) -> Vec<String> {
        let mut args = vec!["--mode".to_string(), "rpc".into(), "--approve".into()];
        if let Some(extension) = &self.extension {
            args.push("--extension".into());
            args.push(extension.path().to_string_lossy().into_owned());
        }
        // `--session-id` creates the session when it is missing, so a fresh
        // and a resumed Lead launch the same way. The new system prompt
        // addendum replaces the old one on a resume.
        args.extend([
            "--session-id".into(),
            launch.conversation.clone(),
            "--append-system-prompt".into(),
            launch.instructions.clone(),
        ]);
        args
    }
    fn handshake(&mut self, launch: &ChatLaunch) -> Vec<u8> {
        let extension = self.extension.take();
        *self = Self {
            extension,
            expected: launch.conversation.clone(),
            ..Self::new()
        };
        frame(json!({"id": STATE, "type": "get_state"}))
    }
    fn ready(&self) -> bool {
        self.ready
    }
    fn send(&mut self, text: &str) -> Vec<u8> {
        self.interrupting = false;
        if self.ready {
            self.prompt(text)
        } else {
            self.turn = Turn::Deferred(text.into());
            Vec::new()
        }
    }
    fn write_accepts(&self) -> bool {
        false
    }
    fn answer(&mut self, request_id: &str, answer: Option<Value>) -> Vec<u8> {
        let kind = self.close(request_id);
        let answers = answer.as_ref().map(|input| &input["answers"]);
        let fields = match (kind, answers) {
            (Some(Kind::Ask), Some(given)) if given.is_object() => {
                json!({"value": given.to_string()})
            }
            (Some(Kind::Confirm), Some(given)) => {
                json!({"confirmed": single(given).as_deref() == Some("Yes")})
            }
            (Some(Kind::Text), Some(given)) => match single(given) {
                Some(text) => json!({"value": text}),
                None => json!({"cancelled": true}),
            },
            // Cancelled, or a dialog Pi already resolved at its timeout.
            _ => json!({"cancelled": true}),
        };
        ui_response(request_id, fields)
    }
    /// `abort` keeps the process, but never settles while an extension
    /// dialog waits, so every open one is cancelled first.
    fn interrupt(&mut self) -> Interrupt {
        self.candidate = None;
        match &self.turn {
            Turn::Idle => Interrupt::Idle,
            Turn::Deferred(_) => {
                self.turn = Turn::Idle;
                Interrupt::Idle
            }
            // Pi will start the run, and Shika cannot abort it before then.
            Turn::Prompted(_) => Interrupt::Signal,
            Turn::Open => {
                let mut bytes = Vec::new();
                for id in self.take_dialogs() {
                    bytes.extend(cancel(&id));
                }
                self.aborts += 1;
                bytes.extend(frame(
                    json!({"id": format!("shika-abort-{}", self.aborts), "type": "abort"}),
                ));
                self.interrupting = true;
                Interrupt::Frame(bytes)
            }
        }
    }
    fn shutdown(&self) -> Shutdown {
        Shutdown::CloseStdin
    }
    /// Pi's SIGTERM handler kills its tools' detached children before it
    /// exits; a group signal would not reach them.
    fn stop_signal(&self) -> i32 {
        libc::SIGTERM
    }
    fn oversized_skippable(&self) -> fn(&[u8]) -> bool {
        oversized_skippable
    }
    fn parse(&mut self, line: &[u8]) -> Parsed {
        let Ok(head) = serde_json::from_slice::<Head>(line) else {
            return Parsed::Failed(BAD_STREAM);
        };
        let Some(kind) = head.kind else {
            return Parsed::Failed(BAD_STREAM);
        };
        let read: fn(&mut Self, &Value) -> Parsed = match kind.as_ref() {
            "response" => Self::response,
            "agent_start" => return self.agent_start(),
            "message_end" => Self::message_end,
            "tool_execution_start" => Self::tool_started,
            "tool_execution_end" => Self::tool_ended,
            "extension_ui_request" => Self::ui_request,
            "agent_settled" => Self::settled,
            // `agent_end`, deltas, turns, queue, compaction, retries, ...
            _ => return Parsed::Ignore,
        };
        match serde_json::from_slice::<Value>(line) {
            Ok(v) => read(self, &v),
            Err(_) => Parsed::Failed(BAD_STREAM),
        }
    }
    fn poll(&mut self, now: Instant) -> Parsed {
        if let Some(parsed) = self.queued.pop_front() {
            return parsed;
        }
        // Pi resolved a dialog with a timeout by itself.
        if let Some(id) = self
            .dialogs
            .iter()
            .find(|d| d.expires.is_some_and(|at| now >= at))
            .map(|d| d.question.request_id.clone())
        {
            self.close(&id);
            return Parsed::Cancelled(id);
        }
        Parsed::Ignore
    }
}

/// Shika's Lead-only Pi extension (`pi-lead.ts`), written for one chat Lead
/// into a private temporary folder and removed with the transport. It loads
/// per process with `--extension`; Pi's own configuration is never touched.
struct Extension(PathBuf);
impl Extension {
    fn install() -> std::io::Result<Self> {
        let extension = Self(crate::activity::private_dir(
            &std::env::temp_dir(),
            "shika-pi-lead",
        )?);
        if extension.path().to_str().is_none() {
            return Err(std::io::ErrorKind::InvalidData.into());
        }
        crate::activity::write_private(&extension.path(), EXTENSION)?;
        Ok(extension)
    }
    fn path(&self) -> PathBuf {
        self.0.join("lead.ts")
    }
}
impl Drop for Extension {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
#[path = "pi_tests.rs"]
mod tests;
