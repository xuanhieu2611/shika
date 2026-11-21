use std::os::unix::fs::symlink;
use std::sync::atomic::{AtomicU64, Ordering};

use super::*;

/// A folder reserved with `create_dir`, removed on drop.
struct Dir(PathBuf);
impl Dir {
    fn new() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        loop {
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "shika-cursor-test-{}-{}-{n}",
                std::process::id(),
                crate::control::new_token()
            ));
            match fs::create_dir(&path) {
                Ok(()) => return Self(path),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => panic!("could not reserve a folder: {e}"),
            }
        }
    }
}
impl Drop for Dir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn only_a_cursor_at_least_as_new_as_the_verified_one_may_run_a_chat_lead() {
    for supported in [
        "2026.11.19-f005e9d",
        "2026.11.19-f005e9d\n",
        "2026.11.20-abc",
        "2026.12.01-abc",
        "2027.01.01",
    ] {
        assert!(cursor_version_supported(supported), "{supported}");
    }
    for older in [
        "2026.11.18-abc",
        "2026.10.21-abc",
        "2025.12.31",
        "",
        "agent",
        "v2026.11.19",
        "2026.11",
        "2026.11.19.1",
    ] {
        assert!(!cursor_version_supported(older), "{older}");
    }
}

#[test]
fn the_launch_forces_runs_and_trusts_without_the_ignored_yolo() {
    let cursor = Cursor { rule: None };
    let launch = ChatLaunch {
        cli: "cursor",
        usage: None,
        usage_threads: None,
        program: PathBuf::from("agent"),
        cwd: PathBuf::from("/w"),
        path: String::new(),
        env: Vec::new(),
        instructions: String::new(),
        notes: Vec::new(),
        conversation: String::new(),
        resumed: false,
        instructed: false,
        history: None,
        data: PathBuf::new(),
        project: "p".into(),
    };
    assert_eq!(
        cursor.args(&launch),
        ["--force", "--trust", "--sandbox", "disabled", "acp"]
    );
    assert_eq!(
        cursor.extension("cursor/generate_image", &Value::Null),
        Extension::Report
    );
    assert_eq!(
        cursor.extension("cursor/create_plan", &Value::Null),
        Extension::Blocked
    );
    assert_eq!(
        cursor.extension("cursor/ask_question", &json!({"questions": []})),
        Extension::Blocked
    );
}

#[test]
fn the_rule_is_written_for_one_session_and_leaves_nothing_behind() {
    let dir = Dir::new();
    let rule = Rule::write(&dir.0, "Line one\nLine two").unwrap();
    let text = fs::read_to_string(dir.0.join(CURSOR_RULE)).unwrap();
    assert!(text.starts_with("---\ndescription: Shika Lead instructions."));
    assert!(text.contains("\nalwaysApply: true\n---\n\nLine one\nLine two\n"));
    let leftovers = fs::read_dir(dir.0.join(".cursor/rules"))
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name())
        .collect::<Vec<_>>();
    assert_eq!(leftovers, ["shika-lead.mdc"], "no temporary file is left");
    drop(rule);
    assert!(!dir.0.join(".cursor").exists());

    // Folders the repository has stay, with whatever else is in them.
    fs::create_dir_all(dir.0.join(".cursor/rules")).unwrap();
    fs::write(dir.0.join(".cursor/rules/team.mdc"), "team rule").unwrap();
    drop(Rule::write(&dir.0, "guide").unwrap());
    assert!(!dir.0.join(CURSOR_RULE).exists());
    assert!(dir.0.join(".cursor/rules/team.mdc").exists());

    // A rule a crashed start left behind is Shika's and is replaced.
    let stale = Rule::write(&dir.0, "old guide").unwrap();
    std::mem::forget(stale);
    let rule = Rule::write(&dir.0, "new guide").unwrap();
    assert!(
        fs::read_to_string(dir.0.join(CURSOR_RULE))
            .unwrap()
            .contains("new guide")
    );
    drop(rule);
    assert!(!dir.0.join(CURSOR_RULE).exists());
}

#[test]
fn the_rule_never_replaces_a_foreign_file_or_follows_a_link() {
    let dir = Dir::new();
    fs::create_dir_all(dir.0.join(".cursor/rules")).unwrap();
    fs::write(dir.0.join(CURSOR_RULE), "---\nalwaysApply: true\n---\nmine").unwrap();
    assert!(Rule::write(&dir.0, "guide").is_err());
    assert_eq!(
        fs::read_to_string(dir.0.join(CURSOR_RULE)).unwrap(),
        "---\nalwaysApply: true\n---\nmine"
    );

    let elsewhere = Dir::new();
    let linked = Dir::new();
    symlink(&elsewhere.0, linked.0.join(".cursor")).unwrap();
    assert!(Rule::write(&linked.0, "guide").is_err());
    assert!(fs::read_dir(&elsewhere.0).unwrap().next().is_none());

    let file = Dir::new();
    fs::write(file.0.join(".cursor"), "").unwrap();
    assert!(Rule::write(&file.0, "guide").is_err());

    // The flavor reports the failure, which fails the start.
    let mut cursor = Cursor { rule: None };
    assert!(!cursor.place_instructions(&dir.0, "guide"));
    assert!(cursor.place_instructions(&elsewhere.0, "guide"));
    assert!(elsewhere.0.join(CURSOR_RULE).exists());
    cursor.remove_instructions();
    assert!(!elsewhere.0.join(".cursor").exists());
}
