//! Lead-only visible history. Immutable records and rebuildable SQL indexes,
//! with FULL synchronous transactions; no provider transcript or tool payloads.
use std::fs;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

pub type ChatResult<T> = Result<T, String>;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChatOrigin {
    Author,
    ShikaEvent,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ChatContent {
    Author { text: String },
    Lead { text: String },
    Event { text: String, key: Option<String> },
    Outgoing { origin: ChatOrigin },
    Accepted,
    Settled,
    Context { cli: String, conversation: String },
}
impl ChatContent {
    pub fn text(&self) -> Option<&str> {
        match self {
            Self::Author { text } | Self::Lead { text } | Self::Event { text, .. } => Some(text),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChatRecord {
    pub id: i64,
    pub turn: i64,
    pub timestamp: u64,
    pub segment: i64,
    pub content: ChatContent,
}

pub(crate) struct ChatStore {
    db: Connection,
    pub lifetime: String,
    segment: i64,
}

fn failure(_: impl std::fmt::Display) -> String {
    "Could not save this conversation".into()
}
pub(crate) fn chat_dir(data: &Path, project: &str) -> ChatResult<PathBuf> {
    if project.is_empty()
        || !project
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        return Err("Could not read this conversation".into());
    }
    Ok(data.join("leads").join(project).join("chat"))
}

impl ChatStore {
    pub fn open(data: &Path, project: &str) -> ChatResult<Self> {
        let dir = chat_dir(data, project)?;
        // Refuse redirected history, including its parent project directory.
        for path in [
            data.join("leads"),
            data.join("leads").join(project),
            dir.clone(),
        ] {
            match fs::symlink_metadata(&path) {
                Ok(m) if !m.is_dir() || m.file_type().is_symlink() => {
                    return Err(failure("redirected history"));
                }
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    fs::DirBuilder::new()
                        .mode(0o700)
                        .create(&path)
                        .or_else(|e| {
                            if e.kind() == std::io::ErrorKind::AlreadyExists
                                && fs::symlink_metadata(&path)
                                    .is_ok_and(|m| m.is_dir() && !m.file_type().is_symlink())
                            {
                                Ok(())
                            } else {
                                Err(e)
                            }
                        })
                        .map_err(failure)?;
                }
                Err(e) => return Err(failure(e)),
            }
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).map_err(failure)?;
        }
        let path = dir.join("history.sqlite");
        if fs::symlink_metadata(&path).is_ok_and(|m| !m.is_file() || m.file_type().is_symlink()) {
            return Err(failure("redirected history"));
        }
        let db = Connection::open(&path).map_err(failure)?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).map_err(failure)?;
        db.busy_timeout(std::time::Duration::from_secs(3))
            .map_err(failure)?;
        let version: i64 = db
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .map_err(failure)?;
        if version > 1 {
            return Err("This conversation uses a newer Shika version".into());
        }
        let rebuild = db
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='visible_entries'",
                [],
                |r| r.get::<_, i64>(0),
            )
            .map_err(failure)?
            == 0;
        db.execute_batch(
            "PRAGMA journal_mode=DELETE; PRAGMA synchronous=FULL;
            CREATE TABLE IF NOT EXISTS metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS records (
                id INTEGER PRIMARY KEY, turn INTEGER NOT NULL, timestamp INTEGER NOT NULL,
                segment INTEGER NOT NULL, visible INTEGER NOT NULL, content TEXT NOT NULL);
            CREATE INDEX IF NOT EXISTS visible_records ON records(visible,id);
            CREATE INDEX IF NOT EXISTS turn_records ON records(turn,id);
            CREATE TABLE IF NOT EXISTS visible_entries (id INTEGER PRIMARY KEY,record_id INTEGER NOT NULL,event_key TEXT UNIQUE);
            PRAGMA user_version=1;",
        )
        .map_err(failure)?;
        let lifetime = db
            .query_row("SELECT value FROM metadata WHERE key='lifetime'", [], |r| {
                r.get(0)
            })
            .optional()
            .map_err(failure)?
            .unwrap_or_else(crate::control::new_token);
        db.execute(
            "INSERT OR IGNORE INTO metadata VALUES ('lifetime',?1)",
            [&lifetime],
        )
        .map_err(failure)?;
        let lifetime = db
            .query_row("SELECT value FROM metadata WHERE key='lifetime'", [], |r| {
                r.get(0)
            })
            .map_err(failure)?;
        let segment = db
            .query_row("SELECT COALESCE(MAX(segment),0) FROM records", [], |r| {
                r.get(0)
            })
            .map_err(failure)?;
        let mut store = Self {
            db,
            lifetime,
            segment,
        };
        if rebuild {
            store.rebuild_index()?;
        }
        Ok(store)
    }
    pub fn bind(&mut self, cli: &str, conversation: &str) -> ChatResult<Option<ChatRecord>> {
        let saved: Option<String> = self
            .db
            .query_row("SELECT value FROM metadata WHERE key='context'", [], |r| {
                r.get(0)
            })
            .optional()
            .map_err(failure)?;
        let binding = serde_json::to_string(&(cli, conversation)).map_err(failure)?;
        if saved.as_ref() == Some(&binding) {
            return Ok(None);
        }
        self.segment += 1;
        let mut records = vec![(
            0,
            ChatContent::Context {
                cli: cli.into(),
                conversation: conversation.into(),
            },
        )];
        if saved.is_some() {
            records.push((
                0,
                ChatContent::Event {
                    text: "A new Lead conversation started".into(),
                    key: None,
                },
            ));
        }
        let result = self.append_with_context(records, Some(&binding));
        if result.is_err() {
            self.segment -= 1;
        }
        Ok(result?.into_iter().find(|r| r.content.text().is_some()))
    }
    /// This projection can be recreated from immutable records without loading
    /// the lifetime transcript into memory. Ordinary opens never scan history.
    fn rebuild_index(&mut self) -> ChatResult<()> {
        let tx = self.db.transaction().map_err(failure)?;
        {
            let mut query = tx
                .prepare("SELECT id,content FROM records WHERE visible=1 ORDER BY id")
                .map_err(failure)?;
            let records = query
                .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
                .map_err(failure)?;
            for record in records {
                let (id, text) = record.map_err(failure)?;
                let content: ChatContent = serde_json::from_str(&text).map_err(failure)?;
                let key = if let ChatContent::Event { key, .. } = &content {
                    key.as_deref()
                } else {
                    None
                };
                tx.execute("INSERT INTO visible_entries(id,record_id,event_key) VALUES (?1,?1,?2) ON CONFLICT(event_key) DO UPDATE SET record_id=excluded.record_id",params![id,key]).map_err(failure)?;
            }
        }
        tx.commit().map_err(failure)
    }

    pub fn next_turn(&self) -> ChatResult<i64> {
        self.db
            .query_row("SELECT COALESCE(MAX(turn),0)+1 FROM records", [], |r| {
                r.get(0)
            })
            .map_err(failure)
    }
    #[cfg(test)]
    pub fn append(&mut self, turn: i64, content: ChatContent) -> ChatResult<ChatRecord> {
        Ok(self.append_batch(vec![(turn, content)])?.remove(0))
    }
    /// Author+outgoing and final+settled use one FULL synchronous transaction.
    pub fn append_batch(
        &mut self,
        contents: Vec<(i64, ChatContent)>,
    ) -> ChatResult<Vec<ChatRecord>> {
        self.append_with_context(contents, None)
    }
    fn append_with_context(
        &mut self,
        contents: Vec<(i64, ChatContent)>,
        context: Option<&str>,
    ) -> ChatResult<Vec<ChatRecord>> {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let tx = self.db.transaction().map_err(failure)?;
        let mut records = Vec::new();
        for (turn, content) in contents {
            let json = serde_json::to_string(&content).map_err(failure)?;
            tx.execute("INSERT INTO records(turn,timestamp,segment,visible,content) VALUES (?1,?2,?3,?4,?5)",params![turn,timestamp,self.segment,content.text().is_some(),json]).map_err(failure)?;
            let id = tx.last_insert_rowid();
            let key = if let ChatContent::Event { key, .. } = &content {
                key.as_deref()
            } else {
                None
            };
            let visible_id = if content.text().is_some() {
                tx.execute("INSERT INTO visible_entries(id,record_id,event_key) VALUES (?1,?1,?2) ON CONFLICT(event_key) DO UPDATE SET record_id=excluded.record_id",params![id,key]).map_err(failure)?;
                tx.query_row(
                    "SELECT id FROM visible_entries WHERE record_id=?1",
                    [id],
                    |r| r.get(0),
                )
                .map_err(failure)?
            } else {
                id
            };
            records.push(ChatRecord {
                id: visible_id,
                turn,
                timestamp,
                segment: self.segment,
                content,
            });
        }
        if let Some(context) = context {
            tx.execute(
                "INSERT OR REPLACE INTO metadata VALUES ('context',?1)",
                [context],
            )
            .map_err(failure)?;
        }
        tx.commit().map_err(failure)?;
        Ok(records)
    }
    pub fn page(&self, before: Option<i64>, count: usize) -> ChatResult<Vec<ChatRecord>> {
        let mut query = self
            .db
            .prepare(
                "SELECT v.id,r.turn,original.timestamp,r.segment,r.content FROM visible_entries v
            JOIN records r ON r.id=v.record_id JOIN records original ON original.id=v.id
            WHERE v.id<?1 ORDER BY v.id DESC LIMIT ?2",
            )
            .map_err(failure)?;
        let rows = query
            .query_map(
                params![before.unwrap_or(i64::MAX), count.min(100) as i64],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get::<_, String>(4)?,
                    ))
                },
            )
            .map_err(failure)?;
        let mut records = Vec::new();
        for row in rows {
            let (id, turn, timestamp, segment, content) = row.map_err(failure)?;
            records.push(ChatRecord {
                id,
                turn,
                timestamp,
                segment,
                content: serde_json::from_str(&content).map_err(failure)?,
            });
        }
        records.reverse();
        Ok(records)
    }
    pub fn recover(&mut self) -> ChatResult<()> {
        let turn: Option<i64> = self
            .db
            .query_row(
                "SELECT turn FROM records WHERE turn>0 ORDER BY id DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .optional()
            .map_err(failure)?;
        let Some(turn) = turn else { return Ok(()) };
        let mut query = self
            .db
            .prepare("SELECT content FROM records WHERE turn=?1 ORDER BY id")
            .map_err(failure)?;
        let records = query
            .query_map([turn], |r| r.get::<_, String>(0))
            .map_err(failure)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(failure)?;
        drop(query);
        let records = records
            .into_iter()
            .map(|r| serde_json::from_str::<ChatContent>(&r).map_err(failure))
            .collect::<ChatResult<Vec<_>>>()?;
        if records.iter().any(|r| matches!(r, ChatContent::Settled)) {
            return Ok(());
        }
        let text = if records.iter().any(|r| matches!(r, ChatContent::Accepted)) {
            "Reply interrupted by restart"
        } else {
            "Delivery unknown"
        };
        self.append_batch(vec![
            (
                turn,
                ChatContent::Event {
                    text: text.into(),
                    key: None,
                },
            ),
            (turn, ChatContent::Settled),
        ])?;
        Ok(())
    }
}

pub(crate) fn delete(data: &Path, project: &str) -> ChatResult<()> {
    let dir = chat_dir(data, project)?;
    for path in [
        data.join("leads"),
        data.join("leads").join(project),
        dir.clone(),
    ] {
        match fs::symlink_metadata(path) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            _ => return Err("Could not delete redirected chat history".into()),
        }
    }
    fs::remove_dir_all(dir).map_err(failure)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn completion_is_atomic_and_worker_report_updates_rebuild_in_place() {
        let dir = std::env::temp_dir().join(format!("shika-chat-{}", crate::control::new_token()));
        fs::create_dir(&dir).unwrap();
        let mut store = ChatStore::open(&dir, "p").unwrap();
        store.bind("claude", "same").unwrap();
        store
            .append_batch(vec![
                (
                    1,
                    ChatContent::Author {
                        text: "hello".into(),
                    },
                ),
                (
                    1,
                    ChatContent::Outgoing {
                        origin: ChatOrigin::Author,
                    },
                ),
                (1, ChatContent::Accepted),
            ])
            .unwrap();
        store
            .append_batch(vec![
                (
                    1,
                    ChatContent::Lead {
                        text: "final only".into(),
                    },
                ),
                (1, ChatContent::Settled),
            ])
            .unwrap();
        let event = store
            .append(
                2,
                ChatContent::Event {
                    text: "Worker is ready to check".into(),
                    key: Some("worker-turn".into()),
                },
            )
            .unwrap();
        let update = store
            .append(
                3,
                ChatContent::Event {
                    text: "Worker is ready to check - Report updated".into(),
                    key: Some("worker-turn".into()),
                },
            )
            .unwrap();
        assert_eq!(event.id, update.id);
        assert_eq!(store.page(None, 60).unwrap().len(), 3);
        store
            .db
            .execute_batch("DROP TABLE visible_entries")
            .unwrap();
        drop(store);
        let mut store = ChatStore::open(&dir, "p").unwrap();
        let page = store.page(None, 60).unwrap();
        assert_eq!(page.len(), 3);
        assert_eq!(page[2].id, event.id);
        assert!(page[2].content.text().unwrap().ends_with("Report updated"));
        store.recover().unwrap();
        assert_eq!(store.page(None, 60).unwrap().len(), 4);
        // The unfinished internal event is recovered, but a completed final is not.
        assert_eq!(
            store
                .page(None, 60)
                .unwrap()
                .iter()
                .filter(|r| matches!(r.content, ChatContent::Lead { .. }))
                .count(),
            1
        );
        drop(store);
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn corrupt_history_is_preserved() {
        let dir = std::env::temp_dir().join(format!("shika-chat-{}", crate::control::new_token()));
        fs::create_dir(&dir).unwrap();
        drop(ChatStore::open(&dir, "p").unwrap());
        let path = dir.join("leads/p/chat/history.sqlite");
        fs::write(&path, b"not a database").unwrap();
        assert!(ChatStore::open(&dir, "p").is_err());
        assert_eq!(fs::read(&path).unwrap(), b"not a database");
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn history_pages_reopen_and_forget_preserves_memory() {
        let dir = std::env::temp_dir().join(format!("shika-chat-{}", crate::control::new_token()));
        fs::create_dir(&dir).unwrap();
        let mut store = ChatStore::open(&dir, "p").unwrap();
        let lifetime = store.lifetime.clone();
        store.bind("claude", "one").unwrap();
        for turn in 1..=220 {
            store
                .append(
                    turn,
                    ChatContent::Author {
                        text: format!("message {turn}"),
                    },
                )
                .unwrap();
        }
        let page = store.page(None, 60).unwrap();
        assert_eq!(page.len(), 60);
        assert_eq!(store.page(Some(page[0].id), 60).unwrap().len(), 60);
        drop(store);
        let mut store = ChatStore::open(&dir, "p").unwrap();
        assert_eq!(store.lifetime, lifetime);
        store.recover().unwrap();
        assert_eq!(
            store.page(None, 1).unwrap()[0].content.text(),
            Some("Delivery unknown")
        );
        store.recover().unwrap();
        drop(store);
        fs::write(dir.join("leads/p/memory.json"), "notes").unwrap();
        delete(&dir, "p").unwrap();
        assert_eq!(
            fs::read_to_string(dir.join("leads/p/memory.json")).unwrap(),
            "notes"
        );
        assert!(
            ChatStore::open(&dir, "p")
                .unwrap()
                .page(None, 60)
                .unwrap()
                .is_empty()
        );
        fs::remove_dir_all(dir).unwrap();
    }
}
