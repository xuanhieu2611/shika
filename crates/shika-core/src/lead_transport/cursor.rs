//! Cursor CLI's ACP flavor (`agent acp`, checked with Cursor CLI
//! 2026.11.19-f005e9d). ACP takes no system prompt, so a new session reads
//! the Lead's instructions from an always-applied rule that exists only
//! while `session/new` runs; Cursor keeps that copy with the session, and a
//! rule present at a later `session/load` is ignored. Cursor's own requests:
//! `cursor/ask_question` is the native question dialog, the reports it does
//! not wait on are acknowledged, and everything else is blocked.
use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::acp::{AcpClient, Extension, Flavor, Texts};
use super::protocol::Strings;
use super::{ChatLaunch, ChatQuestionItem, ChatQuestionOption};

/// The oldest Cursor CLI whose ACP mode was verified for a chat Lead.
pub(crate) const CURSOR_MINIMUM: &str = "2026.11.19";
/// The rule, relative to the Lead worktree. Kept out of `git status` by the
/// repository's `info/exclude`, so a leftover never makes the tree dirty.
pub(crate) const CURSOR_RULE: &str = ".cursor/rules/shika-lead.mdc";
/// The first line after the front matter's opening, which marks a rule as
/// Shika's: only such a file is ever replaced or removed.
const RULE_MARK: &str = "description: Shika Lead instructions.";

static TEXTS: Texts = Texts {
    strings: Strings {
        unreadable: "This Cursor CLI version could not be read",
        other_conversation: "Cursor CLI returned a different conversation",
        start_failed: "Could not start Cursor CLI",
        input_failed: "Could not open Cursor CLI input",
        handshake_failed: "Could not initialize Cursor CLI",
        restart_failed: "Could not start Cursor CLI. Try sending again.",
    },
    reply_failed: "Cursor CLI could not complete the reply. Check its sign-in and connection, then retry.",
    resume_refused: "Cursor CLI could not resume this conversation",
    cut_off: "Cursor CLI stopped the reply at the model's output limit.",
    too_many_steps: "Cursor CLI stopped the reply after too many steps.",
    refused: "Cursor CLI's model refused to continue the reply.",
    instructions_failed: "Could not give Cursor CLI the Lead's instructions",
};

/// The chat Lead protocol for Cursor CLI.
pub(super) fn client() -> AcpClient {
    AcpClient::new(Box::new(Cursor { rule: None }))
}

/// Whether `agent --version` output names a Cursor CLI at least
/// [`CURSOR_MINIMUM`]. Its versions are dates, `2026.11.19-f005e9d`.
pub(crate) fn cursor_version_supported(output: &str) -> bool {
    fn date(text: &str) -> Option<(u32, u32, u32)> {
        let date = text.split('-').next()?;
        let mut parts = date.split('.').map(|part| part.parse::<u32>().ok());
        let found = (parts.next()??, parts.next()??, parts.next()??);
        parts.next().is_none().then_some(found)
    }
    let minimum = date(CURSOR_MINIMUM).expect("a valid minimum");
    output
        .split_whitespace()
        .next()
        .and_then(date)
        .is_some_and(|found| found >= minimum)
}

struct Cursor {
    /// The rule written for the `session/new` in flight.
    rule: Option<Rule>,
}

impl Flavor for Cursor {
    fn cli(&self) -> &'static str {
        "cursor"
    }
    fn texts(&self) -> &'static Texts {
        &TEXTS
    }
    /// `--force` is what stops `session/request_permission` in ACP mode:
    /// `--yolo`, its alias elsewhere, is ignored there (Decision 7).
    fn args(&self, _: &ChatLaunch) -> Vec<String> {
        ["--force", "--trust", "--sandbox", "disabled", "acp"]
            .into_iter()
            .map(str::to_string)
            .collect()
    }
    fn place_instructions(&mut self, cwd: &Path, instructions: &str) -> bool {
        self.remove_instructions();
        match Rule::write(cwd, instructions) {
            Ok(rule) => {
                self.rule = Some(rule);
                true
            }
            Err(_) => false,
        }
    }
    fn remove_instructions(&mut self) {
        self.rule = None;
    }
    fn extension(&self, method: &str, params: &Value) -> Extension {
        match method {
            // Answering it with method-not-found would turn each question
            // into a permission request, even under `--force`.
            "cursor/ask_question" => {
                questions(params).map_or(Extension::Blocked, Extension::Question)
            }
            // Requests Cursor sends and does not wait on; it only logs an
            // error answer.
            "cursor/update_todos" | "cursor/task" | "cursor/generate_image" => Extension::Report,
            // `cursor/create_plan` waits for an accepted plan.
            _ => Extension::Blocked,
        }
    }
    fn answer(&self, answer: Option<&Value>) -> Value {
        answered(answer)
    }
    fn cancel(&self, method: &str) -> Option<Value> {
        (method == "cursor/create_plan").then(|| json!({"outcome": {"outcome": "cancelled"}}))
    }
}

#[derive(Deserialize)]
struct AskOption {
    id: String,
    label: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Ask {
    id: String,
    prompt: String,
    #[serde(default)]
    options: Vec<AskOption>,
    #[serde(default)]
    allow_multiple: bool,
}

/// A `cursor/ask_question` the dialog can show, read from the installed
/// bundle (no recording: the tool was never offered to the model):
/// `{toolCallId, title, questions: [{id, prompt, options: [{id, label}],
/// allowMultiple}]}`. 1 to 4 questions with distinct ids and prompts, at
/// most 4 options each with distinct labels and ids, since answers come
/// back as labels and are mapped to option ids.
fn questions(params: &Value) -> Option<Vec<ChatQuestionItem>> {
    let asks = serde_json::from_value::<Vec<Ask>>(params["questions"].clone()).ok()?;
    let (mut ids, mut prompts) = (HashSet::new(), HashSet::new());
    let valid = |ask: &Ask| {
        let labels = ask.options.iter().map(|o| &o.label).collect::<HashSet<_>>();
        let ids = ask.options.iter().map(|o| &o.id).collect::<HashSet<_>>();
        !ask.prompt.trim().is_empty()
            && ask.options.len() <= 4
            && labels.len() == ask.options.len()
            && ids.len() == ask.options.len()
            && ask.options.iter().all(|o| !o.label.trim().is_empty())
    };
    if asks.is_empty()
        || asks.len() > 4
        || asks
            .iter()
            .any(|a| !valid(a) || !ids.insert(a.id.as_str()) || !prompts.insert(a.prompt.as_str()))
    {
        return None;
    }
    Some(
        asks.into_iter()
            .map(|ask| ChatQuestionItem {
                question: ask.prompt,
                header: String::new(),
                options: ask
                    .options
                    .into_iter()
                    .map(|o| ChatQuestionOption {
                        label: o.label,
                        description: String::new(),
                    })
                    .collect(),
                multi_select: ask.allow_multiple,
            })
            .collect(),
    )
}

/// The `cursor/ask_question` result for the dialog's answers, keyed by
/// question text. Labels map to option ids (`answered`). Cursor has no field
/// for the author's own words, so an answer that is not a label goes as the
/// reason of a `skipped` outcome, which reaches the model as text. Cancel is
/// `cancelled`.
fn answered(input: Option<&Value>) -> Value {
    let cancelled = json!({"outcome": {"outcome": "cancelled"}});
    let Some(input) = input.filter(|input| input["answers"].is_object()) else {
        return cancelled;
    };
    let (mut answers, mut words) = (Vec::new(), Vec::new());
    for q in input["questions"].as_array().into_iter().flatten() {
        let (Some(id), Some(prompt)) = (q["id"].as_str(), q["prompt"].as_str()) else {
            continue;
        };
        let given = match &input["answers"][prompt] {
            Value::String(text) => vec![text.as_str()],
            Value::Array(list) => list.iter().filter_map(Value::as_str).collect(),
            _ => Vec::new(),
        };
        let mut selected = Vec::new();
        for text in given {
            let option = q["options"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|o| o["label"] == text)
                .and_then(|o| o["id"].as_str());
            match option {
                Some(option) => selected.push(Value::from(option)),
                None => words.push(format!("{prompt}: {text}")),
            }
        }
        let mut answer = Map::new();
        answer.insert("questionId".into(), id.into());
        answer.insert("selectedOptionIds".into(), selected.into());
        answers.push(Value::Object(answer));
    }
    if words.is_empty() {
        json!({"outcome": {"outcome": "answered", "answers": answers}})
    } else {
        let reason = format!(
            "The author answered in their own words. {}",
            words.join("; ")
        );
        json!({"outcome": {"outcome": "skipped", "reason": reason}})
    }
}

/// `.cursor/rules/shika-lead.mdc` in the Lead worktree, always applied,
/// written atomically for one `session/new` and removed with the folders it
/// created when dropped. Links are never followed, and a file Shika did not
/// write is never replaced.
struct Rule {
    path: PathBuf,
    /// The folders this rule created, outermost first.
    created: Vec<PathBuf>,
}

impl Rule {
    fn write(cwd: &Path, instructions: &str) -> io::Result<Self> {
        let mut rule = Self {
            path: cwd.join(CURSOR_RULE),
            created: Vec::new(),
        };
        let parent = rule.path.parent().expect("a rule folder").to_path_buf();
        let mut dir = cwd.to_path_buf();
        for part in parent.strip_prefix(cwd).expect("under cwd").components() {
            dir.push(part);
            match fs::symlink_metadata(&dir) {
                // `symlink_metadata`: a link to a folder is not a folder.
                Ok(meta) if meta.is_dir() => {}
                Ok(_) => return Err(io::ErrorKind::AlreadyExists.into()),
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    fs::create_dir(&dir)?;
                    rule.created.push(dir.clone());
                }
                Err(e) => return Err(e),
            }
        }
        if !rule.replaceable() {
            return Err(io::ErrorKind::AlreadyExists.into());
        }
        let text = format!(
            "---\n{RULE_MARK} Shika writes this file only while it opens a Lead session.\nalwaysApply: true\n---\n\n{instructions}\n"
        );
        let temp = parent.join(format!(".shika-lead.mdc.{}.tmp", std::process::id()));
        let written = fs::write(&temp, text).and_then(|()| fs::rename(&temp, &rule.path));
        if written.is_err() {
            let _ = fs::remove_file(&temp);
        }
        written.map(|()| rule)
    }
    /// Missing, or a plain file that is Shika's (left by a process that
    /// ended inside `session/new`).
    fn replaceable(&self) -> bool {
        match fs::symlink_metadata(&self.path) {
            Err(e) => e.kind() == io::ErrorKind::NotFound,
            Ok(meta) => meta.is_file() && self.ours(),
        }
    }
    fn ours(&self) -> bool {
        fs::read_to_string(&self.path).is_ok_and(|text| {
            text.lines()
                .nth(1)
                .is_some_and(|line| line.starts_with(RULE_MARK))
        })
    }
}

impl Drop for Rule {
    fn drop(&mut self) {
        if fs::symlink_metadata(&self.path).is_ok_and(|meta| meta.is_file()) && self.ours() {
            let _ = fs::remove_file(&self.path);
        }
        for dir in self.created.iter().rev() {
            // Only an empty folder goes; anything else put there stays.
            let _ = fs::remove_dir(dir);
        }
    }
}

#[cfg(test)]
#[path = "cursor_tests.rs"]
mod tests;
