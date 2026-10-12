use super::*;

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        loop {
            let root = std::env::temp_dir().join(format!(
                "shika-receipt-date-{}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match fs::create_dir(&root) {
                Ok(()) => return Self(root.canonicalize().unwrap()),
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(err) => panic!("reserve receipt fixture: {err}"),
            }
        }
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn recovered_receipts_keep_their_recorded_day_and_deduplicate_across_days() {
    let scratch = Scratch::new();
    let mut conn = open_store(&scratch.0).unwrap();
    let collection_day = Date::parse("2026-10-19").unwrap();
    let timestamp = "2026-10-14T12:00:00Z";
    let recorded_day = recorded_date(timestamp).unwrap().iso();
    let tx = conn.transaction().unwrap();
    for (cli, json, expected) in [
        ("claude", format!(r#"{{"type":"assistant","timestamp":"{timestamp}","message":{{"id":"message","usage":{{"input_tokens":11,"output_tokens":7}}}}}}"#), 18),
        ("codex", format!(r#"{{"type":"token_usage_record","timestamp":"{timestamp}","payload":{{"response_id":"response","thread_id":"thread","usage":{{"total_tokens":23}}}}}}"#), 23),
    ] {
        let file = InputFile { path: scratch.0.join("session.jsonl"), cli, session: "thread".into() };
        receipt(&tx, &file, serde_json::from_str(&json).unwrap(), collection_day).unwrap();
        receipt(&tx, &file, serde_json::from_str(&json).unwrap(), collection_day).unwrap();
        let (day, tokens): (String, i64) = tx.query_row("SELECT day,tokens FROM days WHERE cli=?1", [cli], |row| Ok((row.get(0)?, row.get(1)?))).unwrap();
        assert_eq!(day, recorded_day);
        assert_eq!(tokens, expected);
    }
    tx.commit().unwrap();
    let shown = report(&scratch.0, Some(collection_day));
    assert_eq!((shown.claude, shown.codex), (0, 0));
    assert!(!shown.empty); // Previous-week usage is saved, not assigned to today.
}

#[test]
fn missing_or_invalid_timestamps_preserve_tokens_on_the_collection_day() {
    let scratch = Scratch::new();
    let mut conn = open_store(&scratch.0).unwrap();
    let collection_day = Date::parse("2026-10-19").unwrap();
    let file = InputFile { path: scratch.0.join("session.jsonl"), cli: "claude", session: "session".into() };
    let tx = conn.transaction().unwrap();
    for json in [
        r#"{"type":"assistant","message":{"id":"absent","usage":{"input_tokens":2,"output_tokens":3}}}"#,
        r#"{"type":"assistant","timestamp":"invalid","message":{"id":"invalid","usage":{"input_tokens":5,"output_tokens":7}}}"#,
    ] {
        receipt(&tx, &file, serde_json::from_str(json).unwrap(), collection_day).unwrap();
    }
    tx.commit().unwrap();
    assert_eq!(report(&scratch.0, Some(collection_day)).claude, 17);
}
