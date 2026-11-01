use super::*;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("shika-cli-title-{nanos}-{n}"));
        fs::create_dir_all(&path).unwrap();
        Self(path.canonicalize().unwrap())
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

const WORKTREE: &str = "/Users/x/code/shika/.worktrees/shika-draft-18db";

fn claude_dir(home: &Path) -> PathBuf {
    home.join(".claude/projects/-Users-x-code-shika--worktrees-shika-draft-18db")
}

fn cursor_dir(home: &Path, cwd: &str) -> PathBuf {
    let hash = Md5::digest(cwd.as_bytes());
    let hash: String = hash.iter().map(|byte| format!("{byte:02x}")).collect();
    home.join(".cursor/chats").join(hash)
}

#[test]
fn task_recovery_ids_are_scoped_to_cwd_epoch_and_metadata_not_transcripts() {
    let scratch = Scratch::new();
    let home = CliHome::at(scratch.0.clone());
    let cwd = Path::new(WORKTREE);
    let claude = home.claude_project_dir(cwd);
    let pi = home.pi_root().join(pi_session_folder(cwd));
    let cursor = cursor_dir(&scratch.0, WORKTREE);
    fs::create_dir_all(&claude).unwrap();
    fs::create_dir_all(&pi).unwrap();
    fs::create_dir_all(cursor.join("old-chat")).unwrap();
    fs::write(claude.join("old-chat.jsonl"), "not read").unwrap();
    fs::write(pi.join("timestamp_old-chat.jsonl"), "not read").unwrap();
    fs::write(
        cursor.join("old-chat/meta.json"),
        format!(r#"{{"cwd":{WORKTREE:?}}}"#),
    )
    .unwrap();
    let since = instant_after_this();
    for cli in ["claude", "pi", "cursor"] {
        assert_eq!(home.task_conversation(cli, cwd, since), None);
    }
    fs::write(claude.join("exact-chat.jsonl"), "not a transcript").unwrap();
    fs::write(pi.join("timestamp_exact-chat.jsonl"), "not a transcript").unwrap();
    fs::create_dir_all(cursor.join("exact-chat")).unwrap();
    fs::write(
        cursor.join("exact-chat/meta.json"),
        format!(r#"{{"cwd":{WORKTREE:?}}}"#),
    )
    .unwrap();
    for cli in ["claude", "pi", "cursor"] {
        assert_eq!(
            home.task_conversation(cli, cwd, since).as_deref(),
            Some("exact-chat")
        );
        assert!(home.task_session_exists(cli, "exact-chat", cwd));
        assert!(!home.task_session_exists(cli, "exact-chat", Path::new("/tmp/another-repo")));
    }
    fs::write(claude.join("--unsafe.jsonl"), "not read").unwrap();
    assert_eq!(
        home.task_conversation("claude", cwd, since).as_deref(),
        Some("exact-chat")
    );
    fs::write(
        cursor.join("exact-chat/meta.json"),
        r#"{"cwd":"/tmp/different-checkout"}"#,
    )
    .unwrap();
    assert_eq!(home.task_conversation("cursor", cwd, since), None);
    assert!(!home.task_session_exists("cursor", "exact-chat", cwd));
}

#[test]
fn claude_title_is_the_last_ai_title_line() {
    let scratch = Scratch::new();
    let dir = claude_dir(&scratch.0);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("a.jsonl"),
        concat!(
            "{\"type\":\"user\",\"message\":{\"content\":\"mentions \\\"ai-title\\\" in text\"}}\n",
            "{\"type\":\"ai-title\",\"aiTitle\":\"Shika background opacity\",\"sessionId\":\"a\"}\n",
            "not json at all \"ai-title\"\n",
            "{\"type\":\"ai-title\",\"aiTitle\":\"Shika  background opacity and blur\",\"sessionId\":\"a\"}\n",
        ),
    )
    .unwrap();
    let home = CliHome::at(scratch.0.clone());
    assert_eq!(
        home.read("claude", Path::new(WORKTREE), None).as_deref(),
        Some("Shika background opacity and blur")
    );
    // Cursor's reader does not look at Claude's files.
    assert_eq!(home.read("cursor", Path::new(WORKTREE), None), None);
}

/// After `since`, with time for the next file's creation to be later.
fn instant_after_this() -> SystemTime {
    let since = SystemTime::now();
    std::thread::sleep(std::time::Duration::from_millis(20));
    since
}

#[test]
fn with_since_only_a_conversation_begun_after_it_names_the_session() {
    let scratch = Scratch::new();
    let dir = claude_dir(&scratch.0);
    fs::create_dir_all(&dir).unwrap();
    let title = |name: &str| format!("{{\"type\":\"ai-title\",\"aiTitle\":\"{name}\"}}\n");
    fs::write(
        dir.join("authors-own.jsonl"),
        title("The author's own chat"),
    )
    .unwrap();
    let since = instant_after_this();
    let home = CliHome::at(scratch.0.clone());
    assert_eq!(
        home.read("claude", Path::new(WORKTREE), None).as_deref(),
        Some("The author's own chat")
    );
    assert_eq!(home.read("claude", Path::new(WORKTREE), Some(since)), None);
    // Touching the older conversation does not make it begin later.
    fs::write(dir.join("authors-own.jsonl"), title("Renamed meanwhile")).unwrap();
    assert_eq!(home.read("claude", Path::new(WORKTREE), Some(since)), None);
    fs::write(
        dir.join("ours.jsonl"),
        title("Where is the notification code"),
    )
    .unwrap();
    assert_eq!(
        home.read("claude", Path::new(WORKTREE), Some(since))
            .as_deref(),
        Some("Where is the notification code")
    );

    let pi = pi_folder(&scratch.0.join(".pi/agent/sessions"));
    fs::create_dir_all(&pi).unwrap();
    let info = |name: &str| format!("{{\"type\":\"session_info\",\"name\":\"{name}\"}}\n");
    fs::write(pi.join("old.jsonl"), info("Old Pi chat")).unwrap();
    let since = instant_after_this();
    assert_eq!(home.read("pi", Path::new(WORKTREE), Some(since)), None);
    fs::write(pi.join("new.jsonl"), info("New Pi chat")).unwrap();
    assert_eq!(
        home.read("pi", Path::new(WORKTREE), Some(since)).as_deref(),
        Some("New Pi chat")
    );

    let cursor = cursor_dir(&scratch.0, WORKTREE);
    let meta = |name: &str| format!(r#"{{"title":"{name}","cwd":"{WORKTREE}","updatedAtMs":1}}"#);
    fs::create_dir_all(cursor.join("old")).unwrap();
    fs::write(cursor.join("old/meta.json"), meta("Old Cursor chat")).unwrap();
    let since = instant_after_this();
    assert_eq!(home.read("cursor", Path::new(WORKTREE), Some(since)), None);
    fs::create_dir_all(cursor.join("new")).unwrap();
    fs::write(cursor.join("new/meta.json"), meta("New Cursor chat")).unwrap();
    assert_eq!(
        home.read("cursor", Path::new(WORKTREE), Some(since))
            .as_deref(),
        Some("New Cursor chat")
    );
}

#[test]
fn with_since_codex_counts_only_threads_created_from_that_second() {
    let scratch = Scratch::new();
    let path = scratch.0.join(".codex/state_1.sqlite");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "CREATE TABLE threads (id TEXT, cwd TEXT, name TEXT, created_at INTEGER,
                created_at_ms INTEGER, updated_at_ms INTEGER);",
    )
    .unwrap();
    let since = UNIX_EPOCH + std::time::Duration::from_millis(1_000_500);
    let row = |id: &str, name: &str, created_ms: i64, updated: i64| {
        conn.execute(
            "INSERT INTO threads VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            (id, WORKTREE, name, created_ms / 1000, created_ms, updated),
        )
        .unwrap();
    };
    row("a", "Before the launch", 999_999, 90);
    let home = CliHome::at(scratch.0.clone());
    assert_eq!(home.read("codex", Path::new(WORKTREE), Some(since)), None);
    row("b", "After the launch", 1_000_700, 10);
    assert_eq!(
        home.read("codex", Path::new(WORKTREE), Some(since))
            .as_deref(),
        Some("After the launch")
    );
    assert_eq!(
        home.read("codex", Path::new(WORKTREE), None).as_deref(),
        Some("Before the launch"),
        "without since, the newest update wins as before"
    );
}

#[test]
fn claude_without_a_title_yet_is_none() {
    let scratch = Scratch::new();
    let home = CliHome::at(scratch.0.clone());
    assert_eq!(home.read("claude", Path::new(WORKTREE), None), None);
    let dir = claude_dir(&scratch.0);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("a.jsonl"), "{\"type\":\"user\"}\n").unwrap();
    fs::write(
        dir.join("notes.txt"),
        "{\"type\":\"ai-title\",\"aiTitle\":\"x\"}\n",
    )
    .unwrap();
    assert_eq!(home.read("claude", Path::new(WORKTREE), None), None);
    fs::write(
        dir.join("b.jsonl"),
        "{\"type\":\"ai-title\",\"aiTitle\":\"  \"}\n",
    )
    .unwrap();
    assert_eq!(home.read("claude", Path::new(WORKTREE), None), None);
}

#[test]
fn claude_follows_its_config_dir() {
    let scratch = Scratch::new();
    let config = scratch.0.join("elsewhere");
    let dir = config.join("projects/-Users-x-code-shika--worktrees-shika-draft-18db");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("a.jsonl"),
        "{\"type\":\"ai-title\",\"aiTitle\":\"Moved\"}\n",
    )
    .unwrap();
    let home = CliHome {
        claude_config: Some(config),
        ..CliHome::at(scratch.0.clone())
    };
    assert_eq!(
        home.read("claude", Path::new(WORKTREE), None).as_deref(),
        Some("Moved")
    );
}

#[test]
fn cursor_title_comes_from_meta_json_for_the_same_cwd() {
    let scratch = Scratch::new();
    let home = CliHome::at(scratch.0.clone());
    let dir = cursor_dir(&scratch.0, WORKTREE);
    fs::create_dir_all(dir.join("old")).unwrap();
    fs::create_dir_all(dir.join("new")).unwrap();
    fs::create_dir_all(dir.join("broken")).unwrap();
    fs::write(
        dir.join("old/meta.json"),
        format!(r#"{{"title":null,"cwd":"{WORKTREE}","updatedAtMs":5}}"#),
    )
    .unwrap();
    assert_eq!(home.read("cursor", Path::new(WORKTREE), None), None);
    fs::write(dir.join("broken/meta.json"), "{ nope").unwrap();
    fs::write(
        dir.join("new/meta.json"),
        format!(r#"{{"title":"Setting Placement Query","cwd":"{WORKTREE}","updatedAtMs":9}}"#),
    )
    .unwrap();
    assert_eq!(
        home.read("cursor", Path::new(WORKTREE), None).as_deref(),
        Some("Setting Placement Query")
    );
    // A chat recorded for another folder is never used.
    fs::write(
        dir.join("old/meta.json"),
        r#"{"title":"Other","cwd":"/somewhere/else","updatedAtMs":99}"#,
    )
    .unwrap();
    assert_eq!(
        home.read("cursor", Path::new(WORKTREE), None).as_deref(),
        Some("Setting Placement Query")
    );
}

#[test]
fn a_resolved_worktree_path_is_also_tried() {
    let scratch = Scratch::new();
    let real = scratch.0.join("repo/.worktrees/shika-draft-1");
    fs::create_dir_all(&real).unwrap();
    let link = scratch.0.join("link");
    std::os::unix::fs::symlink(scratch.0.join("repo"), &link).unwrap();
    let through_link = link.join(".worktrees/shika-draft-1");
    let real_text = real.to_string_lossy().into_owned();
    let dir = cursor_dir(&scratch.0, &real_text);
    fs::create_dir_all(dir.join("a")).unwrap();
    fs::write(
        dir.join("a/meta.json"),
        serde_json::json!({ "title": "Resolved", "cwd": real_text }).to_string(),
    )
    .unwrap();
    let home = CliHome::at(scratch.0.clone());
    assert_eq!(
        home.read("cursor", &through_link, None).as_deref(),
        Some("Resolved")
    );
}

#[test]
fn an_unknown_cli_has_no_title() {
    let scratch = Scratch::new();
    assert_eq!(
        CliHome::at(scratch.0.clone()).read("kiro", Path::new(WORKTREE), None),
        None
    );
}

fn codex_db(path: &Path, rows: &[(&str, &str, i64)]) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(
        "CREATE TABLE threads (
                id TEXT PRIMARY KEY,
                cwd TEXT NOT NULL,
                name TEXT,
                title TEXT NOT NULL DEFAULT '',
                updated_at_ms INTEGER NOT NULL DEFAULT 0
            );",
    )
    .unwrap();
    for (i, (cwd, name, updated)) in rows.iter().enumerate() {
        conn.execute(
            "INSERT INTO threads (id, cwd, name, title, updated_at_ms) VALUES (?1, ?2, ?3, ?4, ?5)",
            (i.to_string(), cwd, name, format!("raw prompt {i}"), updated),
        )
        .unwrap();
    }
}

#[test]
fn codex_name_is_the_short_thread_name_for_that_cwd() {
    let scratch = Scratch::new();
    codex_db(
        &scratch.0.join(".codex/state_1.sqlite"),
        &[
            (WORKTREE, "", 50),
            ("/somewhere/else", "Other folder", 90),
            (WORKTREE, "Fix the login flow", 10),
            (WORKTREE, "Background opacity and blur", 20),
        ],
    );
    let home = CliHome::at(scratch.0.clone());
    assert_eq!(
        home.read("codex", Path::new(WORKTREE), None).as_deref(),
        Some("Background opacity and blur")
    );
    fs::write(scratch.0.join(".codex/state_1.sqlite"), "not a database").unwrap();
    assert_eq!(home.read("codex", Path::new(WORKTREE), None), None);
}

#[test]
fn codex_reads_the_highest_state_database_and_its_home() {
    let scratch = Scratch::new();
    let older = scratch.0.join("codex-old");
    codex_db(
        &older.join("state_1.sqlite"),
        &[(WORKTREE, "From the old file", 99)],
    );
    codex_db(
        &older.join("state_2.sqlite"),
        &[(WORKTREE, "From the new file", 1)],
    );
    // A wal sidecar must not be treated as a database.
    fs::write(older.join("state_9.sqlite-wal"), "nope").unwrap();
    let home = CliHome {
        codex_home: Some(older),
        ..CliHome::at(scratch.0.clone())
    };
    assert_eq!(
        home.read("codex", Path::new(WORKTREE), None).as_deref(),
        Some("From the new file")
    );
    assert_eq!(home.read("codex", Path::new("/missing"), None), None);
}

fn pi_folder(sessions: &Path) -> PathBuf {
    sessions.join("--Users-x-code-shika-.worktrees-shika-draft-18db--")
}

#[test]
fn pi_name_is_the_last_session_info_in_the_newest_file() {
    let scratch = Scratch::new();
    let dir = pi_folder(&scratch.0.join(".pi/agent/sessions"));
    fs::create_dir_all(&dir).unwrap();
    let other = scratch.0.join(".pi/agent/sessions/--somewhere-else--");
    fs::create_dir_all(&other).unwrap();
    fs::write(
        other.join("old.jsonl"),
        "{\"type\":\"session_info\",\"name\":\"Other folder\"}\n",
    )
    .unwrap();
    fs::write(
        dir.join("plain.jsonl"),
        "{\"type\":\"message\",\"name\":\"not a session name\"}\n",
    )
    .unwrap();
    let home = CliHome::at(scratch.0.clone());
    assert_eq!(home.read("pi", Path::new(WORKTREE), None), None);
    let named = dir.join("named.jsonl");
    fs::write(
        &named,
        format!(
            "{{\"type\":\"session\",\"cwd\":\"{WORKTREE}\"}}\n{{\"type\":\"session_info\",\"name\":\"First name\"}}\nnot json \"session_info\"\n{{\"type\":\"session_info\",\"name\":\"Opacity and blur\"}}\n"
        ),
    )
    .unwrap();
    filetime(&named, 20);
    filetime(&dir.join("plain.jsonl"), 30);
    // The newest file has no session name, so an older named file is used.
    assert_eq!(
        home.read("pi", Path::new(WORKTREE), None).as_deref(),
        Some("Opacity and blur")
    );
    fs::write(
        dir.join("blank.jsonl"),
        "{\"type\":\"session_info\",\"name\":\"  \"}\n",
    )
    .unwrap();
    filetime(&dir.join("blank.jsonl"), 40);
    assert_eq!(home.read("pi", Path::new(WORKTREE), None), None);
}

#[test]
fn pi_follows_its_session_dir() {
    let scratch = Scratch::new();
    let sessions = scratch.0.join("sessions");
    let dir = pi_folder(&sessions);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("a.jsonl"),
        "{\"type\":\"session_info\",\"name\":\"Moved\"}\n",
    )
    .unwrap();
    let home = CliHome {
        pi_session_dir: Some(sessions),
        ..CliHome::at(scratch.0.clone())
    };
    assert_eq!(
        home.read("pi", Path::new(WORKTREE), None).as_deref(),
        Some("Moved")
    );
}

#[test]
fn pi_follows_its_agent_dir() {
    let scratch = Scratch::new();
    let agent = scratch.0.join("agent");
    let dir = pi_folder(&agent.join("sessions"));
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("a.jsonl"),
        "{\"type\":\"session_info\",\"name\":\"Agent home\"}\n",
    )
    .unwrap();
    let home = CliHome {
        pi_agent_dir: Some(agent),
        ..CliHome::at(scratch.0.clone())
    };
    assert_eq!(
        home.read("pi", Path::new(WORKTREE), None).as_deref(),
        Some("Agent home")
    );
}

#[test]
fn claude_conversations_are_found_by_id_in_any_project_folder() {
    let scratch = Scratch::new();
    let home = CliHome::at(scratch.0.clone());
    let cwd = Path::new(WORKTREE);
    assert_eq!(home.session_exists("claude", "abc-1", cwd), Some(false));
    let dir = claude_dir(&scratch.0);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("abc-1.jsonl"), "{}\n").unwrap();
    assert_eq!(home.session_exists("claude", "abc-1", cwd), Some(true));
    assert_eq!(home.session_exists("claude", "abc-2", cwd), Some(false));
    // A shortened folder name for a long path still matches by id.
    let other = scratch.0.join(".claude/projects/-very-long-0123abcd");
    fs::create_dir_all(&other).unwrap();
    fs::write(other.join("abc-2.jsonl"), "{}\n").unwrap();
    assert_eq!(home.session_exists("claude", "abc-2", cwd), Some(true));
    // A folder that is not a file does not count.
    fs::create_dir(other.join("abc-3.jsonl")).unwrap();
    assert_eq!(home.session_exists("claude", "abc-3", cwd), Some(false));
}

#[test]
fn pi_conversations_are_found_by_the_id_suffix_in_the_cwd_folder() {
    let scratch = Scratch::new();
    let home = CliHome::at(scratch.0.clone());
    let cwd = Path::new(WORKTREE);
    assert_eq!(home.session_exists("pi", "id-9", cwd), Some(false));
    let dir = pi_folder(&scratch.0.join(".pi/agent/sessions"));
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("2026-10-15T18-05-14-237Z_id-9.jsonl"), "{}\n").unwrap();
    fs::write(dir.join("2026-10-15T18-05-14-237Z_xid-9.jsonl"), "{}\n").unwrap();
    assert_eq!(home.session_exists("pi", "id-9", cwd), Some(true));
    assert_eq!(home.session_exists("pi", "d-9", cwd), Some(false));
    assert_eq!(
        home.session_exists("pi", "id-9", Path::new("/elsewhere")),
        Some(false)
    );
}

fn codex_threads(path: &Path, rows: &[(&str, &str, i64, i64, Option<&str>)]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(
        "CREATE TABLE threads (
                id TEXT PRIMARY KEY,
                cwd TEXT NOT NULL,
                created_at INTEGER NOT NULL DEFAULT 0,
                created_at_ms INTEGER,
                archived INTEGER NOT NULL DEFAULT 0,
                agent_role TEXT
            );",
    )
    .unwrap();
    for (id, cwd, created_ms, archived, role) in rows {
        conn.execute(
            "INSERT INTO threads (id, cwd, created_at, created_at_ms, archived, agent_role)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            (id, cwd, created_ms / 1000, created_ms, archived, role),
        )
        .unwrap();
    }
}

#[test]
fn codex_conversations_exist_in_the_database_or_as_a_rollout_file() {
    let scratch = Scratch::new();
    let home = CliHome::at(scratch.0.clone());
    let cwd = Path::new(WORKTREE);
    assert_eq!(home.session_exists("codex", "t-1", cwd), Some(false));
    codex_threads(
        &scratch.0.join(".codex/state_5.sqlite"),
        &[
            ("t-1", WORKTREE, 1_000, 0, None),
            ("t-archived", WORKTREE, 1_000, 1, None),
        ],
    );
    assert_eq!(home.session_exists("codex", "t-1", cwd), Some(true));
    assert_eq!(home.session_exists("codex", "t-archived", cwd), Some(false));
    let day = scratch.0.join(".codex/sessions/2026/10/15");
    fs::create_dir_all(&day).unwrap();
    fs::write(day.join("rollout-2026-10-15T18-05-14-t-file.jsonl"), "{}\n").unwrap();
    fs::write(day.join("rollout-2026-10-15T18-05-14-t-zst.jsonl.zst"), "x").unwrap();
    assert_eq!(home.session_exists("codex", "t-file", cwd), Some(true));
    assert_eq!(home.session_exists("codex", "t-zst", cwd), Some(true));
    assert_eq!(home.session_exists("codex", "ile", cwd), Some(false));
}

#[test]
fn cursor_and_unknown_clis_have_nothing_to_check() {
    let scratch = Scratch::new();
    let home = CliHome::at(scratch.0.clone());
    assert_eq!(
        home.session_exists("cursor", "c", Path::new(WORKTREE)),
        None
    );
    assert_eq!(home.session_exists("other", "c", Path::new(WORKTREE)), None);
}

#[test]
fn the_newest_codex_thread_since_launch_in_the_worktree_wins_and_helpers_do_not() {
    let scratch = Scratch::new();
    let home = CliHome::at(scratch.0.clone());
    let cwd = Path::new(WORKTREE);
    assert_eq!(home.codex_session_since(cwd, 0), None);
    codex_threads(
        &scratch.0.join(".codex/state_5.sqlite"),
        &[
            ("before", WORKTREE, 9_000, 0, None),
            ("first", WORKTREE, 10_000, 0, None),
            ("newest", WORKTREE, 12_000, 0, None),
            ("helper", WORKTREE, 13_000, 0, Some("explorer")),
            ("archived", WORKTREE, 14_000, 1, None),
            ("elsewhere", "/Users/x/other", 15_000, 0, None),
        ],
    );
    assert_eq!(
        home.codex_session_since(cwd, 10_000).as_deref(),
        Some("newest")
    );
    assert_eq!(
        home.codex_session_since(cwd, 9_000).as_deref(),
        Some("newest")
    );
    assert_eq!(
        home.task_conversation(
            "codex",
            cwd,
            UNIX_EPOCH + std::time::Duration::from_secs(10)
        )
        .as_deref(),
        Some("newest")
    );
    assert!(home.task_session_exists("codex", "newest", cwd));
    assert!(!home.task_session_exists("codex", "elsewhere", cwd));
    assert!(!home.task_session_exists("codex", "archived", cwd));
    assert_eq!(home.codex_session_since(cwd, 12_001), None);
    assert_eq!(home.codex_session_since(Path::new("/nowhere"), 0), None);
}

#[test]
fn a_resolved_worktree_path_finds_the_codex_thread_too_and_an_old_database_still_answers() {
    let scratch = Scratch::new();
    let real = scratch.0.join("real");
    fs::create_dir_all(&real).unwrap();
    let link = scratch.0.join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let home = CliHome::at(scratch.0.clone());
    let resolved = real.canonicalize().unwrap();
    // Codex records the resolved path, with no `agent_role` column.
    let db = scratch.0.join(".codex/state_5.sqlite");
    fs::create_dir_all(db.parent().unwrap()).unwrap();
    let conn = Connection::open(&db).unwrap();
    conn.execute_batch(
        "CREATE TABLE threads (id TEXT, cwd TEXT, created_at INTEGER, created_at_ms INTEGER, archived INTEGER);",
    )
    .unwrap();
    conn.execute(
        "INSERT INTO threads VALUES ('t', ?1, 5, 5000, 0)",
        [resolved.to_string_lossy().as_ref()],
    )
    .unwrap();
    drop(conn);
    assert_eq!(home.codex_session_since(&link, 0).as_deref(), Some("t"));
}

fn filetime(path: &Path, secs: u64) {
    let file = fs::File::options().write(true).open(path).unwrap();
    file.set_modified(UNIX_EPOCH + std::time::Duration::from_secs(secs))
        .unwrap();
}
