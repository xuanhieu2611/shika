//! The title an agent CLI gave its own session, read from the CLI's private
//! files. None of this is a public format, so every reader is read-only and
//! treats anything missing, unreadable, or unexpected as "no title yet".
//!
//! A card's worktree is new, so the only sessions stored for that folder are
//! the card's own. A checkout session's folder is the author's repository,
//! where their own conversations live too, so its title is read only from
//! conversations begun since its CLI started (the `since` of
//! [`CliHome::read`]).
//!
//! The same readers answer two questions a Lead needs: whether a stored
//! conversation id still exists ([`CliHome::session_exists`]), and which
//! Codex conversation a fresh Lead started ([`CliHome::codex_session_since`]).

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use md5::{Digest, Md5};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
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
        Self::with_homes(home, None, None)
    }

    /// A home directory plus optional Claude and Codex config roots.
    #[cfg(test)]
    pub(crate) fn with_homes(
        home: PathBuf,
        claude_config: Option<PathBuf>,
        codex_home: Option<PathBuf>,
    ) -> Self {
        Self {
            home,
            claude_config,
            codex_home,
            pi_agent_dir: None,
            pi_session_dir: None,
        }
    }

    /// The session title the preset's CLI wrote for `worktree`, if any. With
    /// `since`, only a conversation begun at or after it counts: its file was
    /// created then (Claude Code, Pi, Cursor), or its thread row says so
    /// (Codex, to the second). A conversation whose start cannot be read is
    /// skipped then.
    pub(crate) fn read(
        &self,
        preset_id: &str,
        worktree: &Path,
        since: Option<SystemTime>,
    ) -> Option<String> {
        let title = cwd_forms(worktree)
            .into_iter()
            .find_map(|cwd| match preset_id {
                "claude" => self.claude(&cwd, since),
                "codex" => self.codex(&cwd, since),
                "cursor" => self.cursor(&cwd, since),
                "pi" => self.pi(&cwd, since),
                _ => None,
            })?;
        let title = title.split_whitespace().collect::<Vec<_>>().join(" ");
        (!title.is_empty()).then_some(title)
    }

    /// `$CLAUDE_CONFIG_DIR` or `~/.claude`.
    pub(crate) fn claude_root(&self) -> PathBuf {
        self.claude_config
            .clone()
            .unwrap_or_else(|| self.home.join(".claude"))
    }

    /// `$CODEX_HOME` or `~/.codex`.
    pub(crate) fn codex_root(&self) -> PathBuf {
        self.codex_home
            .clone()
            .unwrap_or_else(|| self.home.join(".codex"))
    }

    /// `<config>/projects/<cwd with every non-alphanumeric as ->`.
    pub(crate) fn claude_project_dir(&self, cwd: &Path) -> PathBuf {
        cwd.join(self.claude_root())
            .join("projects")
            .join(encode_claude_cwd(cwd))
    }

    /// `<config>/projects/<cwd with every non-alphanumeric as ->/<session>.jsonl`,
    /// with lines like `{"type":"ai-title","aiTitle":"Fix login flow",...}`.
    /// The newest session with a title wins, and its last title is current.
    fn claude(&self, cwd: &Path, since: Option<SystemTime>) -> Option<String> {
        jsonl_newest_first(&self.claude_project_dir(cwd))
            .into_iter()
            .filter(|path| begun_since(path, since))
            .find_map(|path| {
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
    fn cursor(&self, cwd: &Path, since: Option<SystemTime>) -> Option<String> {
        let hash = Md5::digest(cwd.to_string_lossy().as_bytes());
        let hash: String = hash.iter().map(|byte| format!("{byte:02x}")).collect();
        fs::read_dir(self.home.join(".cursor").join("chats").join(hash))
            .ok()?
            .filter_map(|entry| entry.ok())
            .filter(|entry| begun_since(&entry.path(), since))
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
    fn codex(&self, cwd: &Path, since: Option<SystemTime>) -> Option<String> {
        let db = newest_state_db(&cwd.join(self.codex_root()))?;
        let conn = Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
        let cwd = cwd.to_string_lossy();
        let Some(since) = since else {
            return conn
                .query_row(
                    "SELECT name FROM threads
                     WHERE cwd = ?1 AND name IS NOT NULL AND name != ''
                     ORDER BY updated_at_ms DESC
                     LIMIT 1",
                    [cwd.as_ref()],
                    |row| row.get(0),
                )
                .ok();
        };
        // Whole seconds: an older database records only `created_at`.
        let since_ms = since.duration_since(UNIX_EPOCH).ok()?.as_secs() as i64 * 1000;
        conn.query_row(
            "SELECT name FROM threads
             WHERE cwd = ?1 AND name IS NOT NULL AND name != ''
               AND COALESCE(created_at_ms, created_at * 1000) >= ?2
             ORDER BY updated_at_ms DESC
             LIMIT 1",
            (cwd.as_ref(), since_ms),
            |row| row.get(0),
        )
        .ok()
    }

    /// `PI_CODING_AGENT_SESSION_DIR`, or `<PI_CODING_AGENT_DIR or ~/.pi/agent>/sessions`.
    fn pi_root(&self) -> PathBuf {
        if let Some(dir) = &self.pi_session_dir {
            dir.clone()
        } else {
            self.pi_agent_dir
                .clone()
                .unwrap_or_else(|| self.home.join(".pi").join("agent"))
                .join("sessions")
        }
    }

    /// Pi writes a name only when `/name`, `--name`, or an extension sets one:
    /// `{"type":"session_info","name":"..."}`. Checked against Pi 1.0.0 on 2026-10-05.
    fn pi(&self, cwd: &Path, since: Option<SystemTime>) -> Option<String> {
        let root = self.pi_root();
        let mut sessions: Vec<_> = fs::read_dir(root.join(pi_session_folder(cwd)))
            .ok()?
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "jsonl"))
            .filter(|path| begun_since(path, since))
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

impl CliHome {
    /// Whether the preset's CLI still stores conversation `id` that was started
    /// in `cwd`. None when the CLI keeps nothing Shika can read (Cursor) or the
    /// preset is unknown; callers then assume it exists. Read-only.
    ///
    /// - Claude Code: `<config>/projects/*/<id>.jsonl`. Any project folder, so a
    ///   long path whose folder name Claude shortens still matches.
    /// - Pi: `<sessions>/<cwd folder>/<timestamp>_<id>.jsonl`.
    /// - Codex: an unarchived `threads` row, else a `rollout-*-<id>.jsonl[.zst]`
    ///   file under `sessions/`.
    pub(crate) fn session_exists(&self, preset_id: &str, id: &str, cwd: &Path) -> Option<bool> {
        match preset_id {
            "claude" => Some(self.claude_has(id)),
            "pi" => Some(cwd_forms(cwd).iter().any(|cwd| self.pi_has(cwd, id))),
            "codex" => Some(self.codex_has(id)),
            _ => None,
        }
    }

    fn claude_has(&self, id: &str) -> bool {
        let file = format!("{id}.jsonl");
        fs::read_dir(self.claude_root().join("projects"))
            .into_iter()
            .flatten()
            .filter_map(|entry| entry.ok())
            .any(|entry| entry.path().join(&file).is_file())
    }

    fn pi_has(&self, cwd: &Path, id: &str) -> bool {
        let suffix = format!("_{id}.jsonl");
        fs::read_dir(self.pi_root().join(pi_session_folder(cwd)))
            .into_iter()
            .flatten()
            .filter_map(|entry| entry.ok())
            .any(|entry| entry.file_name().to_string_lossy().ends_with(&suffix))
    }

    fn codex_has(&self, id: &str) -> bool {
        let in_db = newest_state_db(&self.codex_root())
            .and_then(|db| Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY).ok())
            .and_then(|conn| {
                conn.query_row(
                    "SELECT 1 FROM threads WHERE id = ?1 AND archived = 0",
                    [id],
                    |_| Ok(()),
                )
                .ok()
            })
            .is_some();
        in_db || self.codex_rollout_exists(id)
    }

    /// `sessions/<yyyy>/<mm>/<dd>/rollout-<time>-<id>.jsonl`, possibly
    /// compressed. Walks the three date levels; no file is opened.
    fn codex_rollout_exists(&self, id: &str) -> bool {
        fn dirs(path: &Path) -> Vec<PathBuf> {
            fs::read_dir(path)
                .into_iter()
                .flatten()
                .filter_map(|entry| entry.ok())
                .map(|entry| entry.path())
                .filter(|path| path.is_dir())
                .collect()
        }
        let ends = [format!("-{id}.jsonl"), format!("-{id}.jsonl.zst")];
        dirs(&self.codex_root().join("sessions"))
            .into_iter()
            .flat_map(|year| dirs(&year))
            .flat_map(|month| dirs(&month))
            .any(|day| {
                fs::read_dir(day)
                    .into_iter()
                    .flatten()
                    .filter_map(|entry| entry.ok())
                    .any(|entry| {
                        let name = entry.file_name().to_string_lossy().into_owned();
                        name.starts_with("rollout-") && ends.iter().any(|end| name.ends_with(end))
                    })
            })
    }

    /// The newest Codex conversation that was started in `worktree` at or after
    /// `since_ms` (milliseconds since the Unix epoch), if any. Subagent threads
    /// are skipped where the database says which they are, so a Lead's helpers
    /// are never mistaken for the Lead. Read-only; `Core` polls it after a
    /// launch because Codex chooses its own conversation id.
    pub(crate) fn codex_session_since(&self, worktree: &Path, since_ms: i64) -> Option<String> {
        let db = newest_state_db(&self.codex_root())?;
        let conn = Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
        // `agent_role` is null for a user-started thread. An older database
        // without the column still answers, with the subagent filter dropped.
        const SELECT: &str = "SELECT id FROM threads
             WHERE cwd = ?1 AND archived = 0
               AND COALESCE(created_at_ms, created_at * 1000) >= ?2";
        const NEWEST: &str = "ORDER BY COALESCE(created_at_ms, created_at * 1000) DESC LIMIT 1";
        cwd_forms(worktree).into_iter().find_map(|cwd| {
            let cwd = cwd.to_string_lossy().into_owned();
            let ask = |filter: &str| {
                conn.query_row(
                    &format!("{SELECT} {filter} {NEWEST}"),
                    (&cwd, since_ms),
                    |row| row.get::<_, String>(0),
                )
                .optional()
            };
            match ask("AND agent_role IS NULL") {
                Ok(found) => found,
                // No such column: an older database cannot tell subagents.
                Err(_) => ask("").ok().flatten(),
            }
        })
    }
}

/// Whether the conversation stored at `path` began at or after `since`, by
/// the file's creation time. Always true without `since`; false when the
/// creation time cannot be read, so an unknown conversation never names a
/// checkout session.
fn begun_since(path: &Path, since: Option<SystemTime>) -> bool {
    let Some(since) = since else {
        return true;
    };
    fs::metadata(path)
        .and_then(|meta| meta.created())
        .is_ok_and(|created| created >= since)
}

/// `<cwd with every non-alphanumeric character replaced by ->`.
pub(crate) fn encode_claude_cwd(cwd: &Path) -> String {
    cwd.to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// `.jsonl` files directly in `dir`, newest mtime first. Not recursive, so a
/// session's subagent directory is left alone. Missing directories are empty.
pub(crate) fn jsonl_newest_first(dir: &Path) -> Vec<PathBuf> {
    let mut sessions: Vec<_> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "jsonl"))
        .filter_map(|path| {
            let meta = fs::metadata(&path).ok()?;
            if !meta.is_file() {
                return None;
            }
            Some((meta.modified().ok()?, path))
        })
        .collect();
    sessions.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));
    sessions.into_iter().map(|(_, path)| path).collect()
}

/// `state_<n>.sqlite` with the highest `n`. Sidecars such as `state_5.sqlite-wal`
/// do not match.
pub(crate) fn newest_state_db(root: &Path) -> Option<PathBuf> {
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
pub(crate) fn cwd_forms(worktree: &Path) -> Vec<PathBuf> {
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
        let meta =
            |name: &str| format!(r#"{{"title":"{name}","cwd":"{WORKTREE}","updatedAtMs":1}}"#);
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
}
