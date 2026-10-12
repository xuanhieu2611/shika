use super::*;
use serde_json::json;
use std::io::Write;

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        loop {
            let path = std::env::temp_dir().join(format!(
                "shika-usage-{}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match fs::create_dir(&path) {
                Ok(()) => return Self(path.canonicalize().unwrap()),
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(err) => panic!("reserve fixture: {err}"),
            }
        }
    }
    fn data(&self) -> PathBuf {
        let p = self.0.join("data");
        fs::create_dir_all(&p).unwrap();
        p
    }
    fn home(&self) -> CliHome {
        CliHome::at(self.0.join("home"))
    }
    fn cwd(&self) -> PathBuf {
        self.0.join("repo/.worktrees/task")
    }
    fn target(&self) -> Target {
        Target::new(&self.home(), "claude", &self.cwd()).unwrap()
    }
    fn claude(&self, name: &str, text: &str) -> PathBuf {
        let dir = self.home().claude_project_dir(&self.cwd());
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        fs::write(&path, text).unwrap();
        path
    }
    fn shown(&self) -> UsageReport {
        report(&self.data(), Some(day()))
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn day() -> Date {
    Date::parse("2026-10-14").unwrap()
}
fn stopped() -> Arc<AtomicBool> {
    Arc::new(AtomicBool::new(false))
}
fn assistant(id: &str, input: i64, output: i64) -> String {
    format!(
        "{}\n",
        json!({"type":"assistant","message":{"id":id,"content":[{"type":"text","text":"private text never stored"}],"usage":{"input_tokens":input,"output_tokens":output,"output_tokens_details":{"thinking_tokens":output}}}})
    )
}
fn codex(id: &str, thread: &str, total: i64) -> String {
    format!(
        "{}\n",
        json!({"type":"token_usage_record","payload":{"response_id":id,"thread_id":thread,"turn_id":"t","session_id":"s","usage":{"total_tokens":total,"reasoning_output_tokens":total}}})
    )
}
fn drain(conn: &mut Connection, target: &Target) {
    for _ in 0..100 {
        if !collect(conn, target, day(), &stopped()).unwrap().more {
            return;
        }
    }
    panic!("collector did not drain bounded batches");
}
fn append(path: &Path, text: &str) {
    fs::OpenOptions::new()
        .append(true)
        .open(path)
        .unwrap()
        .write_all(text.as_bytes())
        .unwrap();
}

fn queue() -> Arc<Queue> {
    Arc::new(Queue {
        requests: Mutex::new(BTreeMap::new()),
        wake: Condvar::new(),
        stopped: stopped(),
        failed: AtomicBool::new(false),
        active: AtomicBool::new(false),
        failures: Mutex::new(BTreeSet::new()),
    })
}

#[test]
fn same_length_rewrites_and_empty_truncations_do_not_hide_new_receipts() {
    let scratch = Scratch::new();
    let first = assistant("aa", 10, 10);
    let second = assistant("bb", 20, 20);
    let third = assistant("cc", 30, 30);
    assert_eq!(first.len(), second.len());
    assert_eq!(first.len(), third.len());
    let path = scratch.claude("session.jsonl", &first);
    let mut conn = open_store(&scratch.data()).unwrap();
    drain(&mut conn, &scratch.target());
    std::thread::sleep(Duration::from_millis(2));
    fs::write(&path, second).unwrap();
    drain(&mut conn, &scratch.target());
    assert_eq!(scratch.shown().claude, 60);
    fs::write(&path, "").unwrap();
    drain(&mut conn, &scratch.target());
    assert_eq!(
        conn.query_row("SELECT offset FROM files", [], |r| r.get::<_, u64>(0))
            .unwrap(),
        0
    );
    fs::write(&path, third).unwrap();
    drain(&mut conn, &scratch.target());
    assert_eq!(scratch.shown().claude, 120);
}

#[test]
fn a_reader_uses_its_opened_file_snapshot_not_an_unbounded_live_append() {
    let scratch = Scratch::new();
    let path = scratch.claude("session.jsonl", "base");
    let mut reader = PacedFile {
        file: File::open(&path).unwrap(),
        bytes: 0,
        remaining: 4,
        end: 4,
        stopped: stopped(),
    };
    append(&path, "extra");
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, b"base");
    reader.seek(SeekFrom::Start(1)).unwrap();
    bytes.clear();
    reader.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, b"ase");
}

#[test]
fn dropping_an_idle_collector_wakes_its_worker_under_the_predicate_mutex() {
    let queue = queue();
    let guard = queue.requests.lock().unwrap();
    let collector = Collector {
        data_dir: PathBuf::new(),
        queue: queue.clone(),
    };
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let handle = std::thread::spawn(move || {
        ready_tx.send(()).unwrap();
        drop(collector);
    });
    ready_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    std::thread::sleep(Duration::from_millis(10));
    assert!(!queue.stopped.load(Ordering::Relaxed));
    drop(guard);
    handle.join().unwrap();
    assert!(queue.next().is_none());
    for _ in 0..30 {
        let queue = self::queue();
        let waiting = queue.clone();
        let handle = std::thread::spawn(move || waiting.next().is_none());
        std::thread::sleep(Duration::from_millis(1));
        drop(Collector {
            data_dir: PathBuf::new(),
            queue,
        });
        assert!(handle.join().unwrap());
    }
}

#[test]
fn repaired_registration_metadata_clears_its_scoped_failure() {
    let scratch = Scratch::new();
    let target = scratch.target();
    persist_target(&scratch.data(), &target).unwrap();
    let path = scratch
        .data()
        .join("usage-sources")
        .join(format!("{}.json", target.id()));
    fs::write(&path, "bad").unwrap();
    let queue = queue();
    let collector = Collector {
        data_dir: scratch.data(),
        queue: queue.clone(),
    };
    recover(&scratch.data(), &queue);
    assert!(collector.pending());
    persist_target(&scratch.data(), &target).unwrap();
    recover(&scratch.data(), &queue);
    assert!(queue.failures.lock().unwrap().is_empty());
    queue.requests.lock().unwrap().clear();
    assert!(!collector.pending());
}

#[test]
fn checkpoint_schema_migrates_and_future_store_versions_are_refused() {
    let scratch = Scratch::new();
    let path = scratch.claude("session.jsonl", &assistant("first", 10, 10));
    let mut conn = open_store(&scratch.data()).unwrap();
    drain(&mut conn, &scratch.target());
    conn.execute_batch("ALTER TABLE files DROP COLUMN modified; PRAGMA user_version=1")
        .unwrap();
    drop(conn);
    append(&path, &assistant("second", 2, 3));
    let mut upgraded = open_store(&scratch.data()).unwrap();
    drain(&mut upgraded, &scratch.target());
    assert_eq!(scratch.shown().claude, 25);
    assert_eq!(
        upgraded
            .pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        2
    );
    upgraded.pragma_update(None, "user_version", 999).unwrap();
    assert!(open_store(&scratch.data()).is_err());
    assert!(scratch.shown().unreadable);
}

#[test]
fn unsafe_metadata_and_sqlite_symlinks_are_rejected_without_hanging() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let scratch = Scratch::new();
    let data = scratch.data();
    let legacy = data.join("usage.json");
    let target = scratch.0.join("other.json");
    fs::write(&target, "{}").unwrap();
    symlink(&target, &legacy).unwrap();
    assert!(report(&data, Some(day())).unreadable);
    fs::remove_file(&legacy).unwrap();
    let name = std::ffi::CString::new(legacy.as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    assert!(read_legacy(&data).is_err());
    fs::remove_file(&legacy).unwrap();
    File::create(&legacy)
        .unwrap()
        .set_len(64 * 1024 * 1024 + 1)
        .unwrap();
    assert!(read_legacy(&data).is_err());
    fs::remove_file(&legacy).unwrap();
    let oversized = data.join("source.json");
    File::create(&oversized)
        .unwrap()
        .set_len(64 * 1024 + 1)
        .unwrap();
    assert!(read_target(&oversized).is_err());
    symlink(&target, database(&data)).unwrap();
    assert!(open_store(&data).is_err());
    assert_eq!(fs::read(&target).unwrap(), b"{}");
    fs::remove_file(database(&data)).unwrap();
    drop(open_store(&data).unwrap());
    assert_eq!(
        fs::metadata(database(&data)).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[test]
fn discovery_keeps_valid_sessions_next_to_an_unsafe_entry() {
    let scratch = Scratch::new();
    let path = scratch.claude("session.jsonl", &assistant("first", 10, 10));
    let link = path.parent().unwrap().join("unsafe.jsonl");
    std::os::unix::fs::symlink(&path, &link).unwrap();
    let mut conn = open_store(&scratch.data()).unwrap();
    drain(&mut conn, &scratch.target());
    assert_eq!(scratch.shown().claude, 20);
    assert!(scratch.shown().pending);
    fs::remove_file(link).unwrap();
    drain(&mut conn, &scratch.target());
    assert!(!scratch.shown().pending);
}

#[test]
fn missing_codex_ownership_schema_keeps_roots_and_warns_without_guessing_children() {
    let scratch = Scratch::new();
    let root = scratch.home().codex_root();
    fs::create_dir_all(&root).unwrap();
    let parent = root.join("parent.jsonl");
    let child = root.join("unverified.jsonl");
    fs::write(&parent, codex("root-response", "parent", 18)).unwrap();
    fs::write(&child, codex("unverified-response", "child", 999)).unwrap();
    let db = Connection::open(root.join("state_5.sqlite")).unwrap();
    db.execute_batch(
        "CREATE TABLE threads(id TEXT PRIMARY KEY, cwd TEXT, source TEXT, rollout_path TEXT)",
    )
    .unwrap();
    db.execute(
        "INSERT INTO threads VALUES ('parent',?1,'cli',?2)",
        params![scratch.cwd().to_string_lossy(), parent.to_string_lossy()],
    )
    .unwrap();
    db.execute(
        "INSERT INTO threads VALUES ('child',?1,?2,?3)",
        params![
            scratch.cwd().to_string_lossy(),
            "{\"subagent\":\"unverified\"}",
            child.to_string_lossy()
        ],
    )
    .unwrap();
    let target = Target::new(&scratch.home(), "codex", &scratch.cwd()).unwrap();
    let mut conn = open_store(&scratch.data()).unwrap();
    drain(&mut conn, &target);
    assert_eq!(scratch.shown().codex, 18);
    assert!(scratch.shown().pending);
}

#[test]
fn relative_provider_config_roots_resolve_from_the_agent_cwd() {
    let scratch = Scratch::new();
    let cwd = scratch.cwd();
    let home = CliHome::with_homes(
        scratch.0.join("home"),
        Some("claude-relative".into()),
        Some("codex-relative".into()),
    );
    let Target::Claude { directories } = Target::new(&home, "claude", &cwd).unwrap() else {
        panic!("Claude target")
    };
    assert!(
        directories
            .iter()
            .all(|p| p.starts_with(cwd.join("claude-relative")))
    );
    let Target::Codex { root, .. } = Target::new(&home, "codex", &cwd).unwrap() else {
        panic!("Codex target")
    };
    assert_eq!(root, cwd.join("codex-relative"));
}

#[test]
fn corrupt_stored_counters_do_not_advance_the_provider_checkpoint() {
    let scratch = Scratch::new();
    let path = scratch.claude("session.jsonl", &assistant("first", 10, 10));
    let mut conn = open_store(&scratch.data()).unwrap();
    drain(&mut conn, &scratch.target());
    let offset: u64 = conn
        .query_row("SELECT offset FROM files", [], |r| r.get(0))
        .unwrap();
    append(&path, &assistant("second", 5, 10));
    conn.execute_batch("UPDATE sessions SET observed=-1")
        .unwrap();
    assert!(collect(&mut conn, &scratch.target(), day(), &stopped()).is_err());
    assert_eq!(
        conn.query_row("SELECT offset FROM files", [], |r| r.get::<_, u64>(0))
            .unwrap(),
        offset
    );
    conn.execute_batch("UPDATE sessions SET observed=20")
        .unwrap();
    drain(&mut conn, &scratch.target());
    assert_eq!(scratch.shown().claude, 35);
}

#[test]
fn non_utf8_provider_paths_do_not_panic_during_registration() {
    use std::os::unix::ffi::OsStringExt;
    let scratch = Scratch::new();
    let root = PathBuf::from(std::ffi::OsString::from_vec(b"config-\xff".to_vec()));
    let home = CliHome::with_homes(scratch.0.join("home"), Some(root), None);
    let target = Target::new(&home, "claude", &scratch.cwd()).unwrap();
    assert_eq!(target.id().len(), 32);
    assert!(persist_target(&scratch.data(), &target).is_err());
}

#[test]
fn claude_counts_without_cost_state_and_deduplicates_streaming_usage() {
    let scratch = Scratch::new();
    let path = scratch.claude("session.jsonl", &assistant("m", 10, 7));
    let mut conn = open_store(&scratch.data()).unwrap();
    drain(&mut conn, &scratch.target());
    assert_eq!(scratch.shown().claude, 17); // thinking is already inside output
    append(&path, &assistant("m", 10, 7));
    drain(&mut conn, &scratch.target());
    assert_eq!(scratch.shown().claude, 17);
    append(&path, &assistant("m", 10, 9));
    drain(&mut conn, &scratch.target());
    assert_eq!(scratch.shown().claude, 19);
    let cache = format!(
        "{}\n",
        json!({"type":"assistant","message":{"id":"cache","usage":{"input_tokens":2,"output_tokens":3,"cache_read_input_tokens":5,"cache_creation_input_tokens":7}}})
    );
    append(&path, &cache);
    drain(&mut conn, &scratch.target());
    assert_eq!(scratch.shown().claude, 36);
    let bodies: String = conn
        .query_row("SELECT group_concat(id) FROM receipts", [], |r| r.get(0))
        .unwrap();
    assert!(!bodies.contains("private text"));
}

#[test]
fn all_task_sessions_and_subagents_are_counted_once() {
    let scratch = Scratch::new();
    let root = scratch.claude("first.jsonl", &assistant("parent", 3, 4));
    scratch.claude("second.jsonl", &assistant("second", 2, 3));
    let children = root.with_extension("").join("subagents");
    fs::create_dir_all(&children).unwrap();
    fs::write(
        children.join("agent.jsonl"),
        assistant("parent", 3, 4) + &assistant("child", 8, 9),
    )
    .unwrap();
    let mut conn = open_store(&scratch.data()).unwrap();
    drain(&mut conn, &scratch.target());
    assert_eq!(scratch.shown().claude, 29);
    drain(&mut conn, &scratch.target());
    assert_eq!(scratch.shown().claude, 29);
}

#[test]
fn partial_record_is_retried_and_its_offset_does_not_lose_tokens() {
    let scratch = Scratch::new();
    let next = assistant("next", 6, 7);
    let split = next.len() - 6;
    let path = scratch.claude(
        "session.jsonl",
        &(assistant("first", 4, 5) + &next[..split]),
    );
    let mut conn = open_store(&scratch.data()).unwrap();
    let outcome = collect(&mut conn, &scratch.target(), day(), &stopped()).unwrap();
    assert!(outcome.pending);
    assert_eq!(scratch.shown().claude, 9);
    assert!(scratch.shown().pending);
    append(&path, &next[split..]);
    drain(&mut conn, &scratch.target());
    assert_eq!(scratch.shown().claude, 22);
    assert!(!scratch.shown().pending);
}

#[test]
fn malformed_and_negative_records_do_not_strand_later_good_records() {
    let scratch = Scratch::new();
    let text =
        assistant("a", 4, 5) + "{broken}\n" + &assistant("invalid", -1, 10) + &assistant("b", 6, 7);
    scratch.claude("session.jsonl", &text);
    let mut conn = open_store(&scratch.data()).unwrap();
    drain(&mut conn, &scratch.target());
    assert_eq!(scratch.shown().claude, 22);
    assert!(scratch.shown().pending);
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM receipts WHERE id='invalid'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
}

#[test]
fn database_failure_rolls_back_both_counts_and_offsets() {
    let scratch = Scratch::new();
    scratch.claude("session.jsonl", &assistant("m", 4, 5));
    let mut conn = open_store(&scratch.data()).unwrap();
    conn.execute_batch("CREATE TRIGGER fail_cursor BEFORE INSERT ON files BEGIN SELECT RAISE(ABORT,'fixture failure'); END").unwrap();
    assert!(collect(&mut conn, &scratch.target(), day(), &stopped()).is_err());
    assert_eq!(scratch.shown().claude, 0);
    assert_eq!(
        conn.query_row("SELECT count(*) FROM receipts", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    conn.execute_batch("DROP TRIGGER fail_cursor").unwrap();
    drain(&mut conn, &scratch.target());
    assert_eq!(scratch.shown().claude, 9);
}

#[test]
fn restart_and_file_replacement_do_not_count_a_receipt_twice() {
    let scratch = Scratch::new();
    let path = scratch.claude("session.jsonl", &assistant("old", 4, 5));
    {
        let mut conn = open_store(&scratch.data()).unwrap();
        drain(&mut conn, &scratch.target());
    }
    append(&path, &assistant("new", 6, 7));
    {
        let mut conn = open_store(&scratch.data()).unwrap();
        drain(&mut conn, &scratch.target());
    }
    assert_eq!(scratch.shown().claude, 22);
    let replacement = path.with_extension("tmp");
    fs::write(
        &replacement,
        assistant("old", 4, 5) + &assistant("new", 6, 7) + &assistant("later", 1, 2),
    )
    .unwrap();
    fs::rename(replacement, path).unwrap();
    let mut conn = open_store(&scratch.data()).unwrap();
    drain(&mut conn, &scratch.target());
    assert_eq!(scratch.shown().claude, 25);
}

fn codex_fixture(scratch: &Scratch) -> (Target, PathBuf, PathBuf, PathBuf) {
    let root = scratch.0.join("codex-home");
    fs::create_dir_all(&root).unwrap();
    let parent = root.join("parent.jsonl");
    let child = root.join("child.jsonl");
    let other = root.join("other.jsonl");
    fs::write(&parent, codex("p", "parent", 20)).unwrap();
    fs::write(&child, codex("p", "parent", 20) + &codex("c", "child", 7)).unwrap();
    fs::write(&other, codex("o", "other", 900)).unwrap();
    let db = Connection::open(root.join("state_5.sqlite")).unwrap();
    db.execute_batch("CREATE TABLE threads(id TEXT PRIMARY KEY,cwd TEXT,source TEXT,rollout_path TEXT,tokens_used INTEGER DEFAULT 999999);
      CREATE TABLE thread_spawn_edges(parent_thread_id TEXT,child_thread_id TEXT);").unwrap();
    for (id, cwd, source, path) in [
        ("parent", scratch.cwd(), "cli", &parent),
        (
            "child",
            PathBuf::from("/different/cwd"),
            "{\"subagent\":{\"other\":\"worker\"}}",
            &child,
        ),
        ("other", scratch.cwd(), "vscode", &other),
    ] {
        db.execute(
            "INSERT INTO threads(id,cwd,source,rollout_path) VALUES (?1,?2,?3,?4)",
            params![id, cwd.to_string_lossy(), source, path.to_string_lossy()],
        )
        .unwrap();
    }
    db.execute(
        "INSERT INTO thread_spawn_edges VALUES ('parent','child')",
        [],
    )
    .unwrap();
    (
        Target::Codex {
            root,
            cwds: vec![scratch.cwd()],
        },
        parent,
        child,
        other,
    )
}

#[test]
fn codex_counts_parent_and_descendants_not_inherited_context_or_unrelated_threads() {
    let scratch = Scratch::new();
    let (target, _, child, _) = codex_fixture(&scratch);
    let mut conn = open_store(&scratch.data()).unwrap();
    drain(&mut conn, &target);
    assert_eq!(scratch.shown().codex, 27);
    append(&child, &codex("c", "child", 7));
    drain(&mut conn, &target);
    assert_eq!(scratch.shown().codex, 27);
    append(&child, &codex("c2", "child", 3));
    drain(&mut conn, &target);
    assert_eq!(scratch.shown().codex, 30);
}

#[test]
fn missing_child_file_does_not_block_other_sources_and_recovers_later() {
    let scratch = Scratch::new();
    let (target, _, child, _) = codex_fixture(&scratch);
    fs::remove_file(&child).unwrap();
    let mut conn = open_store(&scratch.data()).unwrap();
    drain(&mut conn, &target);
    assert_eq!(scratch.shown().codex, 20);
    assert!(scratch.shown().pending);
    fs::write(child, codex("c", "child", 7)).unwrap();
    drain(&mut conn, &target);
    assert_eq!(scratch.shown().codex, 27);
    assert!(!scratch.shown().pending);
}

#[test]
fn locked_provider_database_has_a_bounded_wait_and_can_be_retried() {
    let scratch = Scratch::new();
    let (target, _, _, _) = codex_fixture(&scratch);
    let Target::Codex { root, .. } = &target else {
        unreachable!()
    };
    let held = Connection::open(root.join("state_5.sqlite")).unwrap();
    held.execute_batch("BEGIN EXCLUSIVE").unwrap();
    let mut conn = open_store(&scratch.data()).unwrap();
    let start = Instant::now();
    assert!(collect(&mut conn, &target, day(), &stopped()).is_err());
    assert!(start.elapsed() < Duration::from_secs(2));
    drop(held);
    drain(&mut conn, &target);
    assert_eq!(scratch.shown().codex, 27);
}

#[test]
fn older_codex_context_counters_are_reported_as_a_gap_not_fresh_api_usage() {
    let scratch = Scratch::new();
    let (target, parent, _, _) = codex_fixture(&scratch);
    fs::write(&parent,"{\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"info\":{\"total_token_usage\":{\"total_tokens\":99999}}}}\n").unwrap();
    let mut conn = open_store(&scratch.data()).unwrap();
    drain(&mut conn, &target);
    assert_eq!(scratch.shown().codex, 7);
    assert!(scratch.shown().pending);
    append(&parent, &codex("p", "parent", 20));
    drain(&mut conn, &target);
    assert_eq!(scratch.shown().codex, 27);
    assert!(!scratch.shown().pending);
}

#[test]
fn huge_content_is_skipped_streamingly_and_batch_offsets_continue() {
    let scratch = Scratch::new();
    let giant = "x".repeat(2 * 1024 * 1024);
    let text = format!(
        "{{\"type\":\"progress\",\"data\":{{\"text\":\"{giant}\"}}}}\n{}",
        assistant("m", 4, 5)
    );
    scratch.claude("session.jsonl", &text);
    let mut conn = open_store(&scratch.data()).unwrap();
    assert!(
        collect(&mut conn, &scratch.target(), day(), &stopped())
            .unwrap()
            .more
    );
    assert_eq!(scratch.shown().claude, 0);
    drain(&mut conn, &scratch.target());
    assert_eq!(scratch.shown().claude, 9);
}

#[test]
fn legacy_json_is_imported_once_without_recounting_its_session_baseline() {
    let scratch = Scratch::new();
    let original = "{\"days\":{\"2026-10-12\":{\"claude\":9}},\"sessions\":{\"claude\":{\"session.jsonl\":9}}}";
    fs::write(scratch.data().join("usage.json"), original).unwrap();
    let path = scratch.claude("session.jsonl", &assistant("m", 4, 5));
    let mut conn = open_store(&scratch.data()).unwrap();
    drain(&mut conn, &scratch.target());
    assert_eq!(scratch.shown().claude, 9);
    append(&path, &assistant("new", 6, 7));
    drain(&mut conn, &scratch.target());
    assert_eq!(scratch.shown().claude, 22);
    drop(conn);
    let _ = open_store(&scratch.data()).unwrap();
    assert_eq!(scratch.shown().claude, 22);
    assert_eq!(
        fs::read_to_string(scratch.data().join("usage.json")).unwrap(),
        original
    );
}

#[test]
fn unreadable_legacy_data_is_preserved_and_not_shown_as_empty_success() {
    let scratch = Scratch::new();
    fs::write(scratch.data().join("usage.json"), "{").unwrap();
    assert!(scratch.shown().unreadable);
    assert!(open_store(&scratch.data()).is_err());
    assert!(scratch.shown().unreadable);
    assert_eq!(
        fs::read_to_string(scratch.data().join("usage.json")).unwrap(),
        "{"
    );
}

#[test]
fn reports_are_read_only_and_week_totals_match_saved_days_across_year_boundary() {
    let scratch = Scratch::new();
    let conn = open_store(&scratch.data()).unwrap();
    for (date, cli, total) in [
        ("2025-12-28", "claude", 100),
        ("2025-12-29", "claude", 9),
        ("2026-01-01", "codex", 7),
        ("2026-01-04", "claude", 3),
        ("2026-01-05", "codex", 200),
    ] {
        conn.execute(
            "INSERT INTO days VALUES (?1,?2,?3)",
            params![date, cli, total],
        )
        .unwrap();
    }
    let before = fs::read(database(&scratch.data())).unwrap();
    let shown = report(&scratch.data(), Some(Date::parse("2026-01-01").unwrap()));
    assert_eq!(shown.claude, 12);
    assert_eq!(shown.codex, 7);
    assert_eq!(shown.days.len(), 3);
    assert_eq!(shown.days[0].label, "Sunday, Jan 4");
    assert_eq!(shown.days[2].label, "Monday, Dec 29");
    assert_eq!(fs::read(database(&scratch.data())).unwrap(), before);
}

#[test]
fn wakeups_are_coalesced_and_no_work_is_scheduled_when_idle() {
    let scratch = Scratch::new();
    let queue = Arc::new(Queue {
        requests: Mutex::new(BTreeMap::new()),
        wake: Condvar::new(),
        stopped: stopped(),
        failed: AtomicBool::new(false),
        active: AtomicBool::new(false),
        failures: Mutex::new(BTreeSet::new()),
    });
    let target = Arc::new(scratch.target());
    let source = Source {
        id: target.id(),
        target,
        queue: queue.clone(),
    };
    for _ in 0..20000 {
        source.notify();
    }
    assert_eq!(queue.requests.lock().unwrap().len(), 1);
    let request = queue.requests.lock().unwrap().pop_first().unwrap().1;
    assert!(request.due <= Instant::now() + COALESCE);
    assert!(queue.requests.lock().unwrap().is_empty());
    queue.stopped.store(true, Ordering::Relaxed);
    source.notify();
    assert!(queue.requests.lock().unwrap().is_empty());
}

#[test]
fn durable_source_recovers_after_restart_even_after_the_worktree_is_gone() {
    let scratch = Scratch::new();
    fs::create_dir_all(scratch.cwd()).unwrap();
    let collector = Collector::start(scratch.data());
    let source = collector
        .register(&scratch.home(), "claude", &scratch.cwd())
        .unwrap();
    let path = scratch.claude("session.jsonl", &assistant("first", 4, 5));
    source.notify();
    wait_until(|| report(&scratch.data(), local_today()).claude == 9);
    drop(source);
    drop(collector);
    fs::remove_dir(scratch.cwd()).unwrap();
    append(&path, &assistant("later", 6, 7)); // persisted after the app stopped
    let restarted = Collector::start(scratch.data());
    wait_until(|| report(&scratch.data(), local_today()).claude == 22);
    drop(restarted);
}
fn wait_until(mut condition: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(6);
    while Instant::now() < end {
        if condition() {
            return;
        }
        thread::sleep(Duration::from_millis(20));
    }
    panic!("background collector did not converge");
}
