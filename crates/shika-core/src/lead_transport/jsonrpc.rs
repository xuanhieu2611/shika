//! JSON-RPC 2.0 over LF-delimited lines, for the Codex app-server and ACP
//! adapters. Only framing and correlation: no IO, no method knowledge. Codex
//! omits the `"jsonrpc":"2.0"` member on the wire; ACP requires it.
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use serde_json::{Map, Value, json};

/// Error text from a CLI is shown to the author only through this bound.
const MAX_ERROR_CHARS: usize = 200;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct RpcError {
    pub code: i64,
    pub message: String,
}

/// One decoded line. `K` is the caller's tag for what a request was for.
#[derive(Debug, PartialEq)]
pub(super) enum Message<K> {
    /// The answer to one of our pending requests, which is no longer pending.
    Response {
        kind: K,
        result: Result<Value, RpcError>,
    },
    /// A response whose id is not pending: late after expiry, or never ours.
    Stray,
    /// The CLI asks us something and waits for a response with this `id`.
    Request {
        id: Value,
        method: String,
        params: Value,
    },
    Notification {
        method: String,
        params: Value,
    },
    /// Not JSON, not an object, the wrong version, or neither shape.
    Invalid,
}

struct Pending<K> {
    kind: K,
    deadline: Instant,
}

pub(super) struct Rpc<K> {
    header: bool,
    next: u64,
    pending: BTreeMap<u64, Pending<K>>,
}
impl<K> Rpc<K> {
    /// `header` writes `"jsonrpc":"2.0"` on every frame.
    pub fn new(header: bool) -> Self {
        Self {
            header,
            next: 1,
            pending: BTreeMap::new(),
        }
    }
    fn frame(&self, mut object: Map<String, Value>) -> Vec<u8> {
        if self.header {
            object.insert("jsonrpc".into(), "2.0".into());
        }
        let mut bytes = serde_json::to_vec(&Value::Object(object)).expect("JSON value");
        bytes.push(b'\n');
        bytes
    }
    /// A request with the next id, pending until its response or `timeout`.
    pub fn request(
        &mut self,
        method: &str,
        params: Value,
        kind: K,
        timeout: Duration,
        now: Instant,
    ) -> Vec<u8> {
        let id = self.next;
        self.next += 1;
        self.pending.insert(
            id,
            Pending {
                kind,
                deadline: now + timeout,
            },
        );
        self.frame(object(
            json!({"id": id, "method": method, "params": params}),
        ))
    }
    pub fn notification(&self, method: &str, params: Value) -> Vec<u8> {
        self.frame(object(json!({"method": method, "params": params})))
    }
    /// The success response to a request the CLI sent.
    pub fn respond(&self, id: &Value, result: Value) -> Vec<u8> {
        self.frame(object(json!({"id": id, "result": result})))
    }
    pub fn respond_error(&self, id: &Value, code: i64, message: &str) -> Vec<u8> {
        self.frame(object(
            json!({"id": id, "error": {"code": code, "message": message}}),
        ))
    }
    pub fn is_pending(&self) -> bool {
        !self.pending.is_empty()
    }
    /// Removes and returns, oldest first, every request past its deadline.
    pub fn expired(&mut self, now: Instant) -> Vec<K> {
        let ids: Vec<u64> = self
            .pending
            .iter()
            .filter(|(_, p)| p.deadline <= now)
            .map(|(id, _)| *id)
            .collect();
        ids.into_iter()
            .filter_map(|id| self.pending.remove(&id))
            .map(|p| p.kind)
            .collect()
    }
    /// Forgets every pending request, for a new process.
    pub fn clear(&mut self) {
        self.pending.clear();
    }
    pub fn decode(&mut self, line: &[u8]) -> Message<K> {
        let Ok(Value::Object(v)) = serde_json::from_slice::<Value>(line) else {
            return Message::Invalid;
        };
        if v.get("jsonrpc").is_some_and(|version| version != "2.0") {
            return Message::Invalid;
        }
        let id = v.get("id").filter(|id| id.is_number() || id.is_string());
        match (v.get("method"), id) {
            (Some(Value::String(method)), Some(id)) => Message::Request {
                id: id.clone(),
                method: method.clone(),
                params: v.get("params").cloned().unwrap_or(Value::Null),
            },
            (Some(Value::String(method)), None) if !v.contains_key("id") => Message::Notification {
                method: method.clone(),
                params: v.get("params").cloned().unwrap_or(Value::Null),
            },
            (None, _) => self.response(&v),
            _ => Message::Invalid,
        }
    }
    fn response(&mut self, v: &Map<String, Value>) -> Message<K> {
        let result = match (v.get("result"), v.get("error")) {
            (Some(result), None) => Ok(result.clone()),
            (None, Some(error)) => Err(RpcError {
                code: error["code"].as_i64().unwrap_or(0),
                message: bounded(error["message"].as_str().unwrap_or("")),
            }),
            _ => return Message::Invalid,
        };
        // Our ids are positive integers. An error about a request the CLI
        // could not parse carries a null id.
        match v.get("id") {
            Some(Value::Number(id)) => match id.as_u64().and_then(|id| self.pending.remove(&id)) {
                Some(pending) => Message::Response {
                    kind: pending.kind,
                    result,
                },
                None => Message::Stray,
            },
            Some(Value::Null | Value::String(_)) => Message::Stray,
            _ => Message::Invalid,
        }
    }
}

fn object(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        _ => unreachable!("frames are objects"),
    }
}

/// One line of at most `MAX_ERROR_CHARS` characters with no control
/// characters, safe to show in an event row.
pub(super) fn bounded(text: &str) -> String {
    let flat = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>();
    let flat = flat.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= MAX_ERROR_CHARS {
        return flat;
    }
    let mut cut = flat.chars().take(MAX_ERROR_CHARS - 3).collect::<String>();
    cut.push_str("...");
    cut
}

#[cfg(test)]
#[path = "jsonrpc_tests.rs"]
mod tests;
