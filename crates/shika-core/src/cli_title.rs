//! The title an agent CLI gave its own session, read from the CLI's private
//! files. None of this is a public format, so every reader is read-only and
//! treats anything missing, unreadable, or unexpected as "no title yet".
//!
//! A card's worktree is new, so the only sessions stored for that folder are
//! the card's own.

use std::fs;
use std::path::{Path, PathBuf};

use md5::{Digest, Md5};
use rusqlite::{Connection, OpenFlags};
use serde_json::Value;

/// Where the CLIs keep their data. The home directory in the app, a scratch
/// directory in tests.
#[derive(Debug, Clone)]
pub(crate) struct CliHome {
    home: PathBuf,
    claude_config: Option<PathBuf>,
    codex_home: Option<PathBuf>,
    pi_agent_dir: Option<PathBuf>,
    pi_session_dir: Option<PathBuf>,
}

fn env_dir(key: &str) -> Option<PathBuf> {
    std::env::var_os(key)
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
}

impl CliHome {
    /// The user's home. Each CLI inherits Shika's environment, so a set
    /// config directory is honored the same way the CLI itself honors it.
    pub(crate) fn detect() -> Option<Self> {
        Some(Self {
            home: dirs::home_dir()?,
            claude_config: env_dir("CLAUDE_CONFIG_DIR"),
            codex_home: env_dir("CODEX_HOME"),
            pi_agent_dir: env_dir("PI_CODING_AGENT_DIR"),
            pi_session_dir: env_dir("PI_CODING_AGENT_SESSION_DIR"),
        })
    }

    #[cfg(test)]
    pub(crate) fn at(home: PathBuf) -> Self {
        Self {
            home,
            claude_config: None,
            codex_home: None,
            pi_agent_dir: None,
            pi_session_dir: None,
        }
    }

    /// The session title the preset's CLI wrote for `worktree`, if any.
    pub(crate) fn read(&self, preset_id: &str, worktree: &Path) -> Option<String> {
        let title = cwd_forms(worktree)
            .into_iter()
            .find_map(|cwd| match preset_id {
                "claude" => self.claude(&cwd),
                "codex" => self.codex(&cwd),
                "cursor" => self.cursor(&cwd),
                "pi" => self.pi(&cwd),
                _ => None,
            })?;
        let title = title.split_whitespace().collect::<Vec<_>>().join(" ");
        (!title.is_empty()).then_some(title)
    }

    /// `<config>/projects/<cwd with every non-alphanumeric as ->/<session>.jsonl`,
    /// with lines like `{"type":"ai-title","aiTitle":"Fix login flow",...}`.
    /// The newest session with a title wins, and its last title is current.
    fn claude(&self, cwd: &Path) -> Option<String> {
        let config = self
            .claude_config
            .clone()
            .unwrap_or_else(|| self.home.join(".claude"));
        let encoded: String = cwd
            .to_string_lossy()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect();
        let mut sessions: Vec<_> = fs::read_dir(config.join("projects").join(encoded))
            .ok()?
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "jsonl"))
            .filter_map(|path| Some((fs::metadata(&path).ok()?.modified().ok()?, path)))
            .collect();
        sessions.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));
        sessions.into_iter().find_map(|(_, path)| {
            let text = fs::read_to_string(path).ok()?;
            text.lines()
                .filter(|line| line.contains("\"ai-title\""))
                .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                .filter(|record| record["type"] == "ai-title")
                .filter_map(|record| record["aiTitle"].as_str().map(str::to_string))
                .next_back()
        })
    }

    /// `~/.cursor/chats/<md5 of cwd>/<agent>/meta.json`, holding `cwd` and a
    /// `title` that stays null until the chat has been named.
    fn cursor(&self, cwd: &Path) -> Option<String> {
        let hash = Md5::digest(cwd.to_string_lossy().as_bytes());
        let hash: String = hash.iter().map(|byte| format!("{byte:02x}")).collect();
        fs::read_dir(self.home.join(".cursor").join("chats").join(hash))
            .ok()?
            .filter_map(|entry| entry.ok())
            .filter_map(|entry| {
                let text = fs::read_to_string(entry.path().join("meta.json")).ok()?;
                let meta: Value = serde_json::from_str(&text).ok()?;
                if meta["cwd"].as_str().map(Path::new) != Some(cwd) {
                    return None;
                }
                let title = meta["title"].as_str()?.to_string();
                Some((meta["updatedAtMs"].as_u64().unwrap_or(0), title))
            })
            .max_by_key(|(updated, _)| *updated)
            .map(|(_, title)| title)
    }

    /// `$CODEX_HOME` or `~/.codex`, the highest `state_<n>.sqlite`. `threads.name`
    /// is the short conversation name. `title` is often the raw first message,
    /// so it is not read. Checked against Codex CLI 0.160.0 on 2026-10-05.
    fn codex(&self, cwd: &Path) -> Option<String> {
        let root = self
            .codex_home
            .clone()
            .unwrap_or_else(|| self.home.join(".codex"));
        let db = newest_state_db(&root)?;
        let conn = Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
        let cwd = cwd.to_string_lossy();
        conn.query_row(
            "SELECT name FROM threads
             WHERE cwd = ?1 AND name IS NOT NULL AND name != ''
             ORDER BY updated_at_ms DESC
             LIMIT 1",
            [cwd.as_ref()],
            |row| row.get(0),
        )
        .ok()
    }

    /// `PI_CODING_AGENT_SESSION_DIR`, or `<PI_CODING_AGENT_DIR or ~/.pi/agent>/sessions`.
    /// Pi writes a name only when `/name`, `--name`, or an extension sets one:
    /// `{"type":"session_info","name":"..."}`. Checked against Pi 1.0.0 on 2026-10-05.
    fn pi(&self, cwd: &Path) -> Option<String> {
        let root = if let Some(dir) = &self.pi_session_dir {
            dir.clone()
        } else {
            self.pi_agent_dir
                .clone()
                .unwrap_or_else(|| self.home.join(".pi").join("agent"))
                .join("sessions")
        };
        let mut sessions: Vec<_> = fs::read_dir(root.join(pi_session_folder(cwd)))
            .ok()?
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "jsonl"))
            .filter_map(|path| Some((fs::metadata(&path).ok()?.modified().ok()?, path)))
            .collect();
        sessions.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));
        sessions.into_iter().find_map(|(_, path)| {
            let text = fs::read_to_string(path).ok()?;
            text.lines()
                .filter(|line| line.contains("\"session_info\""))
                .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                .filter(|record| record["type"] == "session_info")
                .filter_map(|record| record["name"].as_str().map(str::to_string))
                .next_back()
        })
    }
}

/// `state_<n>.sqlite` with the highest `n`. Sidecars such as `state_5.sqlite-wal`
/// do not match.
fn newest_state_db(root: &Path) -> Option<PathBuf> {
    fs::read_dir(root)
        .ok()?
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            let name = path.file_name()?.to_str()?;
            let n: u32 = name
                .strip_prefix("state_")?
                .strip_suffix(".sqlite")?
                .parse()
                .ok()?;
            Some((n, path))
        })
        .max_by_key(|(n, _)| *n)
        .map(|(_, path)| path)
}

/// Pi's session folder: `--` plus the cwd with its leading separator removed
/// and `/`, `\`, and `:` replaced by `-`, plus a trailing `--`.
fn pi_session_folder(cwd: &Path) -> String {
    let text = cwd.to_string_lossy();
    let trimmed = text
        .strip_prefix('/')
        .or_else(|| text.strip_prefix('\\'))
        .unwrap_or(&text);
    let encoded: String = trimmed
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' => '-',
            other => other,
        })
        .collect();
    format!("--{encoded}--")
}

/// The CLIs record the directory they were started in. That is the worktree
/// path Shika used, and on macOS can also be its resolved form (`/tmp` is
/// `/private/tmp`).
fn cwd_forms(worktree: &Path) -> Vec<PathBuf> {
    let mut forms = vec![worktree.to_path_buf()];
    if let Ok(real) = worktree.canonicalize()
        && real != worktree
    {
        forms.push(real);
    }
    forms
}

#[cfg(test)]
mod tests {
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
            home.read("claude", Path::new(WORKTREE)).as_deref(),
            Some("Shika background opacity and blur")
        );
        // Cursor's reader does not look at Claude's files.
        assert_eq!(home.read("cursor", Path::new(WORKTREE)), None);
    }

    #[test]
    fn claude_without_a_title_yet_is_none() {
        let scratch = Scratch::new();
        let home = CliHome::at(scratch.0.clone());
        assert_eq!(home.read("claude", Path::new(WORKTREE)), None);
        let dir = claude_dir(&scratch.0);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("a.jsonl"), "{\"type\":\"user\"}\n").unwrap();
        fs::write(
            dir.join("notes.txt"),
            "{\"type\":\"ai-title\",\"aiTitle\":\"x\"}\n",
        )
        .unwrap();
        assert_eq!(home.read("claude", Path::new(WORKTREE)), None);
        fs::write(
            dir.join("b.jsonl"),
            "{\"type\":\"ai-title\",\"aiTitle\":\"  \"}\n",
        )
        .unwrap();
        assert_eq!(home.read("claude", Path::new(WORKTREE)), None);
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
            home.read("claude", Path::new(WORKTREE)).as_deref(),
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
        assert_eq!(home.read("cursor", Path::new(WORKTREE)), None);
        fs::write(dir.join("broken/meta.json"), "{ nope").unwrap();
        fs::write(
            dir.join("new/meta.json"),
            format!(r#"{{"title":"Setting Placement Query","cwd":"{WORKTREE}","updatedAtMs":9}}"#),
        )
        .unwrap();
        assert_eq!(
            home.read("cursor", Path::new(WORKTREE)).as_deref(),
            Some("Setting Placement Query")
        );
        // A chat recorded for another folder is never used.
        fs::write(
            dir.join("old/meta.json"),
            r#"{"title":"Other","cwd":"/somewhere/else","updatedAtMs":99}"#,
        )
        .unwrap();
        assert_eq!(
            home.read("cursor", Path::new(WORKTREE)).as_deref(),
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
            home.read("cursor", &through_link).as_deref(),
            Some("Resolved")
        );
    }

    #[test]
    fn an_unknown_cli_has_no_title() {
        let scratch = Scratch::new();
        assert_eq!(
            CliHome::at(scratch.0.clone()).read("kiro", Path::new(WORKTREE)),
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
            home.read("codex", Path::new(WORKTREE)).as_deref(),
            Some("Background opacity and blur")
        );
        fs::write(scratch.0.join(".codex/state_1.sqlite"), "not a database").unwrap();
        assert_eq!(home.read("codex", Path::new(WORKTREE)), None);
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
            home.read("codex", Path::new(WORKTREE)).as_deref(),
            Some("From the new file")
        );
        assert_eq!(home.read("codex", Path::new("/missing")), None);
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
        assert_eq!(home.read("pi", Path::new(WORKTREE)), None);
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
            home.read("pi", Path::new(WORKTREE)).as_deref(),
            Some("Opacity and blur")
        );
        fs::write(
            dir.join("blank.jsonl"),
            "{\"type\":\"session_info\",\"name\":\"  \"}\n",
        )
        .unwrap();
        filetime(&dir.join("blank.jsonl"), 40);
        assert_eq!(home.read("pi", Path::new(WORKTREE)), None);
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
            home.read("pi", Path::new(WORKTREE)).as_deref(),
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
            home.read("pi", Path::new(WORKTREE)).as_deref(),
            Some("Agent home")
        );
    }

    fn filetime(path: &Path, secs: u64) {
        let file = fs::File::options().write(true).open(path).unwrap();
        file.set_modified(UNIX_EPOCH + std::time::Duration::from_secs(secs))
            .unwrap();
    }
}
