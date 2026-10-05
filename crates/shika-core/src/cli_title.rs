//! The title an agent CLI gave its own session, read from the CLI's private
//! files. None of this is a public format, so every reader is read-only and
//! treats anything missing, unreadable, or unexpected as "no title yet".
//!
//! A card's worktree is new, so the only sessions stored for that folder are
//! the card's own.

use std::fs;
use std::path::{Path, PathBuf};

use md5::{Digest, Md5};
use serde_json::Value;

/// Where the CLIs keep their data. The home directory in the app, a scratch
/// directory in tests.
#[derive(Debug, Clone)]
pub(crate) struct CliHome {
    home: PathBuf,
    claude_config: Option<PathBuf>,
}

impl CliHome {
    /// The user's home. Claude Code moves its data when `CLAUDE_CONFIG_DIR`
    /// is set, and the CLI inherits Shika's environment, so honor it too.
    pub(crate) fn detect() -> Option<Self> {
        Some(Self {
            home: dirs::home_dir()?,
            claude_config: std::env::var_os("CLAUDE_CONFIG_DIR")
                .filter(|dir| !dir.is_empty())
                .map(PathBuf::from),
        })
    }

    #[cfg(test)]
    pub(crate) fn at(home: PathBuf) -> Self {
        Self {
            home,
            claude_config: None,
        }
    }

    /// The session title the preset's CLI wrote for `worktree`, if any.
    pub(crate) fn read(&self, preset_id: &str, worktree: &Path) -> Option<String> {
        let title = cwd_forms(worktree)
            .into_iter()
            .find_map(|cwd| match preset_id {
                "claude" => self.claude(&cwd),
                "cursor" => self.cursor(&cwd),
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
            home: scratch.0.clone(),
            claude_config: Some(config),
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
            CliHome::at(scratch.0.clone()).read("codex", Path::new(WORKTREE)),
            None
        );
    }
}
