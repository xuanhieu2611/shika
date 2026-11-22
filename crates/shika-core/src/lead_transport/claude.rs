//! Claude Code's documented bidirectional stream JSON, never terminal bytes.
//! Candidates are private until a successful result.
use std::collections::HashSet;
use std::hash::Hasher;

use serde_json::{Value, json};

use super::protocol::{Interrupt, LeadProtocol, Shutdown, Strings};
use super::{ChatLaunch, ChatQuestion, ChatQuestionItem, Parsed};

pub(super) const BAD_STREAM: &str = "This Claude Code version could not be read";
const REPLY_FAILED: &str =
    "Claude Code could not complete the reply. Check its sign-in and connection, then retry.";
static STRINGS: Strings = Strings {
    unreadable: BAD_STREAM,
    other_conversation: "Claude Code returned a different conversation",
    start_failed: "Could not start Claude Code",
    input_failed: "Could not open Claude Code input",
    handshake_failed: "Could not initialize Claude Code",
    restart_failed: "Could not start Claude Code. Try sending again.",
};

/// One process's decoder, with fresh candidate/dedup state for every turn.
pub(super) struct ClaudeStream {
    expected: String,
    confirmed: bool,
    initialized: bool,
    version: Option<String>,
    pub(super) active: bool,
    pub(super) candidate: Option<(String, String)>,
    messages: HashSet<String>,
    model: Option<String>,
}
impl ClaudeStream {
    pub(super) fn new(expected: String) -> Self {
        Self {
            expected,
            confirmed: false,
            initialized: false,
            version: None,
            active: false,
            candidate: None,
            messages: HashSet::new(),
            model: None,
        }
    }
    pub(super) fn begin(&mut self) {
        self.active = true;
        self.candidate = None;
        self.messages.clear();
    }
    pub(super) fn parse(&mut self, line: &[u8]) -> Parsed {
        let Ok(v) = serde_json::from_slice::<Value>(line) else {
            return Parsed::Failed(BAD_STREAM);
        };
        let Some(kind) = v["type"].as_str() else {
            return Parsed::Failed(BAD_STREAM);
        };
        if let Some(id) = v["session_id"].as_str()
            && id != self.expected
        {
            return Parsed::Failed(STRINGS.other_conversation);
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
        self.model = super::reported_model(&v["model"]).map(str::to_owned);
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
            return Parsed::Failed(REPLY_FAILED);
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
            return Parsed::Failed(REPLY_FAILED);
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

fn frame(value: Value) -> Vec<u8> {
    let mut bytes = serde_json::to_vec(&value).expect("JSON value");
    bytes.push(b'\n');
    bytes
}

/// One `claude -p` process per conversation, reading stream JSON on stdin
/// until a Stop generating signal ends it.
impl LeadProtocol for ClaudeStream {
    fn cli(&self) -> &'static str {
        "claude"
    }
    fn strings(&self) -> &'static Strings {
        &STRINGS
    }
    fn args(&self, launch: &ChatLaunch) -> Vec<String> {
        let mut args: Vec<String> = [
            "-p",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--verbose",
            "--dangerously-skip-permissions",
            "--append-system-prompt",
            &launch.instructions,
            "--permission-prompts",
            "host",
            "--permission-prompt-tool",
            "stdio",
            if launch.resumed {
                "--resume"
            } else {
                "--session-id"
            },
            &launch.conversation,
        ]
        .into_iter()
        .map(str::to_string)
        .collect();
        if let Some(model) = &launch.model {
            args.extend(["--model".into(), model.clone()]);
        }
        args
    }
    /// A parent Claude session must not disable this Lead's persistence.
    fn env_remove(&self) -> &'static [&'static str] {
        &["CLAUDE_CODE_CHILD_SESSION", "CLAUDECODE"]
    }
    fn handshake(&mut self, launch: &ChatLaunch) -> Vec<u8> {
        *self = Self::new(launch.conversation.clone());
        b"{\"type\":\"control_request\",\"request_id\":\"shika-initialize\",\"request\":{\"subtype\":\"initialize\"}}\n".to_vec()
    }
    fn ready(&self) -> bool {
        self.initialized
    }
    fn version(&self) -> Option<&str> {
        self.version.as_deref()
    }
    fn send(&mut self, text: &str) -> Vec<u8> {
        self.begin();
        frame(json!({"type":"user", "message":{"role":"user", "content":text}}))
    }
    fn answer(&mut self, request_id: &str, answer: Option<Value>) -> Vec<u8> {
        let response = if let Some(input) = answer {
            json!({"behavior":"allow","updatedInput":input})
        } else {
            json!({"behavior":"deny","message":"The author cancelled this question."})
        };
        frame(
            json!({"type":"control_response","response":{"subtype":"success","request_id":request_id,"response":response}}),
        )
    }
    fn interrupt(&mut self) -> Interrupt {
        self.candidate = None;
        Interrupt::Signal
    }
    fn shutdown(&self) -> Shutdown {
        Shutdown::Signal
    }
    fn parse(&mut self, line: &[u8]) -> Parsed {
        ClaudeStream::parse(self, line)
    }
    fn poll(&mut self, _: std::time::Instant) -> Parsed {
        self.model.take().map_or(Parsed::Ignore, Parsed::Model)
    }
}

#[cfg(test)]
#[path = "claude_tests.rs"]
mod tests;
