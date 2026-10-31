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
    /// Discover an exact conversation only in this worktree and launch epoch.
    /// Reads directory metadata (and Cursor's small meta.json), not transcripts.
    pub(crate) fn task_conversation(
        &self,
        cli: &str,
        worktree: &Path,
        since: SystemTime,
    ) -> Option<String> {
        if cli == "codex" {
            let seconds = i64::try_from(since.duration_since(UNIX_EPOCH).ok()?.as_secs()).ok()?;
            let ms = seconds.checked_mul(1000)?;
            return self.codex_session_since(worktree, ms);
        }
        cwd_forms(worktree).into_iter().find_map(|cwd| {
            let root = match cli {
                "claude" => self.claude_project_dir(&cwd),
                "pi" => self.pi_root().join(pi_session_folder(&cwd)),
                "cursor" => {
                    let hash = Md5::digest(cwd.to_string_lossy().as_bytes());
                    let hash: String = hash.iter().map(|byte| format!("{byte:02x}")).collect();
                    self.home.join(".cursor/chats").join(hash)
                }
                _ => return None,
            };
            let mut candidates: Vec<_> = fs::read_dir(root)
                .ok()?
                .filter_map(|entry| entry.ok())
                .filter_map(|entry| {
                    let path = entry.path();
                    if !begun_since(&path, Some(since)) {
                        return None;
                    }
                    if entry.file_type().ok()?.is_symlink() {
                        return None;
                    }
                    let id = if cli == "cursor" {
                        let meta = cursor_meta(&path)?;
                        if meta["cwd"].as_str().map(Path::new) != Some(cwd.as_path()) {
                            return None;
                        }
                        entry.file_name().to_str()?.to_string()
                    } else {
                        if !entry.file_type().ok()?.is_file()
                            || path.extension()?.to_str()? != "jsonl"
                        {
                            return None;
                        }
                        let stem = path.file_stem()?.to_str()?;
                        if cli == "pi" {
                            stem.rsplit_once('_')?.1.to_string()
                        } else {
                            stem.to_string()
                        }
                    };
                    crate::agents::valid_conversation_id(&id)
                        .then_some((fs::metadata(path).ok()?.modified().ok()?, id))
                })
                .collect();
            candidates.sort_by_key(|(time, _)| std::cmp::Reverse(*time));
            candidates.into_iter().next().map(|(_, id)| id)
        })
    }

    /// Stricter than Lead existence checks: a recovered task id must belong
    /// to this exact cwd. Missing/unreadable metadata means fresh fallback.
    pub(crate) fn task_session_exists(&self, cli: &str, id: &str, worktree: &Path) -> bool {
        cwd_forms(worktree).into_iter().any(|cwd| match cli {
            "claude" => self
                .claude_project_dir(&cwd)
                .join(format!("{id}.jsonl"))
                .is_file(),
            "pi" => self.pi_has(&cwd, id),
            "cursor" => {
                let hash = Md5::digest(cwd.to_string_lossy().as_bytes());
                let hash: String = hash.iter().map(|byte| format!("{byte:02x}")).collect();
                cursor_meta(&self.home.join(".cursor/chats").join(hash).join(id))
                    .is_some_and(|meta| meta["cwd"].as_str().map(Path::new) == Some(cwd.as_path()))
            }
            "codex" => newest_state_db(&self.codex_root())
                .and_then(|db| {
                    Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY).ok()
                })
                .and_then(|conn| {
                    conn.query_row(
                        "SELECT 1 FROM threads WHERE id = ?1 AND cwd = ?2 AND archived = 0",
                        (id, cwd.to_string_lossy().as_ref()),
                        |_| Ok(()),
                    )
                    .ok()
                })
                .is_some(),
            _ => false,
        })
    }

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

fn cursor_meta(dir: &Path) -> Option<Value> {
    use std::io::Read;
    let file = fs::File::open(dir.join("meta.json")).ok()?;
    let mut bytes = Vec::new();
    file.take(16 * 1024 + 1).read_to_end(&mut bytes).ok()?;
    if bytes.len() > 16 * 1024 {
        return None;
    }
    serde_json::from_slice(&bytes).ok()
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
#[path = "cli_title_tests.rs"]
mod tests;
