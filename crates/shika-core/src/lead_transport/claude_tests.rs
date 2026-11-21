use super::*;
use serde_json::json;
#[test]
fn final_only_from_documented_hand_written_fixture() {
    let mut parser = ClaudeStream::new("session".into());
    parser.begin();
    let fixture = include_str!("../../tests/fixtures/claude-chat.jsonl");
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
    for line in include_str!("../../tests/fixtures/claude-2.1.302-local.jsonl").lines() {
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
    for line in include_str!("../../tests/fixtures/claude-2.1.302-background-local.jsonl").lines() {
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
    p.parse(
        br#"{"type":"assistant","message":{"id":"a","content":[{"type":"text","text":"late"}]}}"#,
    );
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
    for line in include_str!("../../tests/fixtures/claude-2.1.302-question-local.jsonl").lines() {
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
    p.parse(
        br#"{"type":"assistant","message":{"id":"a","content":[{"type":"text","text":"hidden"}]}}"#,
    );
    assert!(matches!(p.parse(br#"{"type":"result","subtype":"error_during_execution","is_error":true,"session_id":"s"}"#),Parsed::Failed(_)));
    assert_eq!(p.candidate, None);
}
