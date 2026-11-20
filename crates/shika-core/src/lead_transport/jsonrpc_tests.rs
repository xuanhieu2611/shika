use super::*;

fn line(bytes: &[u8]) -> Value {
    assert!(bytes.ends_with(b"\n") && !bytes[..bytes.len() - 1].contains(&b'\n'));
    serde_json::from_slice(bytes).unwrap()
}

#[test]
fn requests_get_increasing_ids_and_the_header_only_when_asked() {
    let now = Instant::now();
    let mut codex = Rpc::new(false);
    let first = line(&codex.request(
        "initialize",
        json!({}),
        "init",
        Duration::from_secs(10),
        now,
    ));
    let second = line(&codex.request(
        "thread/start",
        json!({"cwd": "/w"}),
        "thread",
        Duration::from_secs(10),
        now,
    ));
    assert_eq!(
        first,
        json!({"id": 1, "method": "initialize", "params": {}})
    );
    assert_eq!(second["id"], 2);
    assert_eq!(second["params"]["cwd"], "/w");
    let acp = Rpc::<()>::new(true);
    assert_eq!(
        line(&acp.notification("session/cancel", json!({"sessionId": "s"}))),
        json!({"jsonrpc": "2.0", "method": "session/cancel", "params": {"sessionId": "s"}})
    );
    assert_eq!(
        line(&acp.respond(&json!("r1"), json!({}))),
        json!({"jsonrpc": "2.0", "id": "r1", "result": {}})
    );
    assert_eq!(
        line(&acp.respond_error(&json!(7), -32601, "Method not found")),
        json!({"jsonrpc": "2.0", "id": 7, "error": {"code": -32601, "message": "Method not found"}})
    );
}

#[test]
fn responses_correlate_once_and_unknown_ids_are_stray() {
    let now = Instant::now();
    let mut rpc = Rpc::new(true);
    rpc.request("a", json!({}), "a", Duration::from_secs(1), now);
    rpc.request("b", json!({}), "b", Duration::from_secs(1), now);
    assert_eq!(
        rpc.decode(br#"{"jsonrpc":"2.0","id":2,"result":{"ok":true}}"#),
        Message::Response {
            kind: "b",
            result: Ok(json!({"ok": true}))
        }
    );
    assert_eq!(
        rpc.decode(br#"{"jsonrpc":"2.0","id":2,"result":{}}"#),
        Message::Stray
    );
    assert_eq!(rpc.decode(br#"{"id":99,"result":null}"#), Message::Stray);
    assert_eq!(
        rpc.decode(br#"{"id":null,"error":{"code":-32700,"message":"Parse error"}}"#),
        Message::Stray
    );
    assert!(rpc.is_pending());
    assert_eq!(
        rpc.decode(br#"{"id":1,"error":{"code":-32000,"message":"thread not found"}}"#),
        Message::Response {
            kind: "a",
            result: Err(RpcError {
                code: -32000,
                message: "thread not found".into()
            })
        }
    );
    assert!(!rpc.is_pending());
}

#[test]
fn server_requests_and_notifications_are_told_apart() {
    let mut rpc = Rpc::<()>::new(false);
    assert_eq!(
        rpc.decode(br#"{"id":0,"method":"item/tool/requestUserInput","params":{"q":1}}"#),
        Message::Request {
            id: json!(0),
            method: "item/tool/requestUserInput".into(),
            params: json!({"q": 1})
        }
    );
    assert_eq!(
        rpc.decode(br#"{"jsonrpc":"2.0","id":"perm-1","method":"session/request_permission"}"#),
        Message::Request {
            id: json!("perm-1"),
            method: "session/request_permission".into(),
            params: Value::Null
        }
    );
    assert_eq!(
        rpc.decode(br#"{"method":"turn/completed","params":{"turn":{"status":"completed"}}}"#),
        Message::Notification {
            method: "turn/completed".into(),
            params: json!({"turn": {"status": "completed"}})
        }
    );
}

#[test]
fn malformed_lines_are_invalid() {
    let mut rpc = Rpc::<()>::new(true);
    for bad in [
        &b"garbage"[..],
        b"[1,2]",
        br#""text""#,
        br#"{"jsonrpc":"1.0","method":"x"}"#,
        br#"{"id":1}"#,
        br#"{"id":1,"result":{},"error":{}}"#,
        br#"{"id":{},"result":{}}"#,
        br#"{"id":null,"method":"x"}"#,
        br#"{"method":5}"#,
    ] {
        assert_eq!(
            rpc.decode(bad),
            Message::Invalid,
            "{}",
            String::from_utf8_lossy(bad)
        );
    }
}

#[test]
fn deadlines_expire_oldest_first_and_clear_forgets_everything() {
    let now = Instant::now();
    let mut rpc = Rpc::new(false);
    rpc.request("slow", json!({}), "slow", Duration::from_secs(10), now);
    rpc.request("fast", json!({}), "fast", Duration::from_secs(1), now);
    rpc.request("also", json!({}), "also", Duration::from_secs(1), now);
    assert!(rpc.expired(now).is_empty());
    assert_eq!(rpc.expired(now + Duration::from_secs(1)), ["fast", "also"]);
    assert_eq!(
        rpc.decode(br#"{"id":2,"result":{}}"#),
        Message::Stray,
        "a late answer to an expired request is never correlated"
    );
    assert!(rpc.is_pending());
    rpc.clear();
    assert!(!rpc.is_pending());
    assert!(rpc.expired(now + Duration::from_secs(60)).is_empty());
}

#[test]
fn error_text_is_one_bounded_line() {
    let mut rpc = Rpc::new(false);
    rpc.request("a", json!({}), (), Duration::from_secs(1), Instant::now());
    let long = format!(
        "{{\"id\":1,\"error\":{{\"code\":1,\"message\":\"line one\\nline\\u001b two {}\"}}}}",
        "x".repeat(500)
    );
    let Message::Response {
        result: Err(error), ..
    } = rpc.decode(long.as_bytes())
    else {
        panic!("expected an error response");
    };
    assert!(error.message.starts_with("line one line two xxx"));
    assert!(error.message.ends_with("..."));
    assert_eq!(error.message.chars().count(), MAX_ERROR_CHARS);
    assert_eq!(bounded("  short\ttext  "), "short text");
    assert_eq!(
        bounded(&"é".repeat(MAX_ERROR_CHARS)).chars().count(),
        MAX_ERROR_CHARS
    );
}
