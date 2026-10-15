//! A project's Lead memory: short notes the Lead saved with `shika remember`
//! and every new conversation starts with.
//!
//! A CLI compacts long conversations, a lost session starts fresh, and Forget
//! or a CLI change starts a new one. The notes outlive all of that because
//! they belong to the project, not to a conversation. One plain JSON file per
//! project, outside the repository: `<data_dir>/leads/<project-id>/memory.json`,
//! `{"notes": [{"text": "...", "created": <unix seconds>}]}`. A missing file
//! is no notes. The author may read or edit it; Shika has no editor for it.
//!
//! Notes are one line each, [`MAX_NOTE_CHARS`] at most, [`MAX_TOTAL_BYTES`] of
//! text in all. A file that does not parse is left alone: reads and writes
//! fail with [`Error::MemoryUnreadable`] naming it. The decisions and the
//! delivery per CLI: `docs/lead-agent.md`.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// The longest note, in characters, after cleaning.
pub const MAX_NOTE_CHARS: usize = 1000;

/// The most note text a project keeps, in bytes.
pub const MAX_TOTAL_BYTES: usize = 16_000;

/// One saved note.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryNote {
    pub text: String,
    /// Seconds since the Unix epoch.
    pub created: u64,
}

#[derive(Default, Serialize, Deserialize)]
struct MemoryFile {
    #[serde(default)]
    notes: Vec<MemoryNote>,
}

/// The memory files under `<data_dir>/leads`. Each read-modify-write holds the
/// lock, so two commands never lose each other's note.
pub(crate) struct MemoryStore {
    root: PathBuf,
    lock: Mutex<()>,
}

impl MemoryStore {
    pub(crate) fn open(data_dir: &Path) -> Self {
        Self {
            root: data_dir.join("leads"),
            lock: Mutex::new(()),
        }
    }

    /// The project's memory file. A project id is a path component, so
    /// anything else is refused rather than joined.
    pub(crate) fn path(&self, project_id: &str) -> Result<PathBuf> {
        let plain = !project_id.is_empty()
            && project_id
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_');
        if !plain {
            return Err(Error::UnknownProject);
        }
        Ok(self.root.join(project_id).join("memory.json"))
    }

    pub(crate) fn list(&self, project_id: &str) -> Result<Vec<MemoryNote>> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        load(&self.path(project_id)?)
    }

    /// Appends one cleaned note and returns its 1-based number.
    pub(crate) fn add(&self, project_id: &str, text: &str) -> Result<usize> {
        let path = self.path(project_id)?;
        let text = clean_note(text).map_err(Error::MemoryRefused)?;
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut notes = load(&path)?;
        let used: usize = notes.iter().map(|note| note.text.len()).sum();
        if used + text.len() > MAX_TOTAL_BYTES {
            return Err(Error::MemoryRefused(
                "Memory is full (16,000 bytes). Forget a note first.".into(),
            ));
        }
        notes.push(MemoryNote {
            text,
            created: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_secs()),
        });
        save(&path, &notes)?;
        Ok(notes.len())
    }

    /// Removes note `number` (1-based); the later notes move up by one.
    pub(crate) fn remove(&self, project_id: &str, number: usize) -> Result<()> {
        let path = self.path(project_id)?;
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut notes = load(&path)?;
        if number == 0 || number > notes.len() {
            return Err(Error::MemoryRefused(match notes.len() {
                0 => format!("No note {number}. There are no notes."),
                1 => format!("No note {number}. There is 1 note."),
                count => format!("No note {number}. There are {count} notes."),
            }));
        }
        notes.remove(number - 1);
        save(&path, &notes)
    }

    /// Deletes the project's memory. Best effort: the id is never reused.
    pub(crate) fn delete(&self, project_id: &str) {
        if let Ok(path) = self.path(project_id) {
            let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(dir) = path.parent() {
                let _ = fs::remove_dir_all(dir);
            }
        }
    }
}

/// One line: whitespace runs (line breaks and tabs too) become a space, other
/// control characters go, and the ends are trimmed. Empty or over
/// [`MAX_NOTE_CHARS`] is refused with the reason.
pub fn clean_note(text: &str) -> std::result::Result<String, String> {
    let spaced: String = text
        .chars()
        .filter_map(|ch| match ch {
            ch if ch.is_whitespace() => Some(' '),
            ch if ch.is_control() => None,
            ch => Some(ch),
        })
        .collect();
    let cleaned = spaced.split_whitespace().collect::<Vec<_>>().join(" ");
    if cleaned.is_empty() {
        return Err("The note is empty.".into());
    }
    let chars = cleaned.chars().count();
    if chars > MAX_NOTE_CHARS {
        return Err(format!(
            "The note is {chars} characters; the most is 1,000. Shorten it."
        ));
    }
    Ok(cleaned)
}

/// The Lead's instructions with the notes as a last section, or `base` alone
/// when there are none. The preamble above it stays byte for byte the same.
pub fn instructions_with_notes(base: &str, notes: &[MemoryNote]) -> String {
    if notes.is_empty() {
        return base.to_string();
    }
    let mut text = format!(
        "{}\n\n## Memory\n\nNotes you saved earlier with `shika remember`. They belong to this project and outlast any one conversation.\n",
        base.trim_end()
    );
    for note in notes {
        text.push_str("\n- ");
        text.push_str(&note.text);
    }
    text
}

fn load(path: &Path) -> Result<Vec<MemoryNote>> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(_) => return Err(Error::MemoryUnreadable(path.to_path_buf())),
    };
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }
    serde_json::from_str::<MemoryFile>(&text)
        .map(|file| file.notes)
        .map_err(|_| Error::MemoryUnreadable(path.to_path_buf()))
}

fn save(path: &Path, notes: &[MemoryNote]) -> Result<()> {
    let fail = || Error::MemoryUnwritable(path.to_path_buf());
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|_| fail())?;
    }
    let mut json = serde_json::to_string_pretty(&MemoryFile {
        notes: notes.to_vec(),
    })
    .map_err(|_| fail())?;
    json.push('\n');
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, json).map_err(|_| fail())?;
    fs::rename(&tmp, path).map_err(|_| fail())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory reserved with `create_dir`, never adopted, removed on drop.
    struct Dir(PathBuf);

    impl Dir {
        fn new() -> Self {
            loop {
                let path = std::env::temp_dir().join(format!(
                    "shika-memory-test-{}-{}",
                    std::process::id(),
                    crate::control::new_token()
                ));
                match fs::create_dir(&path) {
                    Ok(()) => return Self(path),
                    Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
                    Err(err) => panic!("could not reserve a directory: {err}"),
                }
            }
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn texts(store: &MemoryStore, project: &str) -> Vec<String> {
        store
            .list(project)
            .unwrap()
            .into_iter()
            .map(|note| note.text)
            .collect()
    }

    fn refusal(result: Result<impl std::fmt::Debug>) -> String {
        match result {
            Err(Error::MemoryRefused(reason)) => reason,
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn notes_round_trip_in_order_and_a_missing_file_is_no_notes() {
        let dir = Dir::new();
        let store = MemoryStore::open(&dir.0);
        assert!(store.list("p1").unwrap().is_empty());
        assert_eq!(store.add("p1", "Use pnpm.").unwrap(), 1);
        assert_eq!(store.add("p1", "Tests run with `just t`.").unwrap(), 2);
        assert_eq!(store.add("other", "Not shared.").unwrap(), 1);
        assert_eq!(
            texts(&store, "p1"),
            ["Use pnpm.", "Tests run with `just t`."]
        );
        let file = dir.0.join("leads/p1/memory.json");
        assert_eq!(store.path("p1").unwrap(), file);
        let saved: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(file).unwrap()).unwrap();
        assert_eq!(saved["notes"][0]["text"], "Use pnpm.");
        assert!(saved["notes"][0]["created"].as_u64().unwrap() > 1_000_000_000);
        assert!(!dir.0.join("leads/p1/memory.json.tmp").exists());
        // Another store on the same directory sees them.
        assert_eq!(texts(&MemoryStore::open(&dir.0), "p1").len(), 2);
    }

    #[test]
    fn a_note_is_one_clean_line_of_one_to_a_thousand_characters() {
        assert_eq!(
            clean_note("  a\tb\r\n\r\n c\x07\u{1b}[0m d  ").unwrap(),
            "a b c[0m d"
        );
        assert_eq!(clean_note("   \n\x07 "), Err("The note is empty.".into()));
        assert!(clean_note(&"x".repeat(MAX_NOTE_CHARS)).is_ok());
        assert_eq!(
            clean_note(&"x".repeat(MAX_NOTE_CHARS + 1)),
            Err("The note is 1001 characters; the most is 1,000. Shorten it.".into())
        );
        // Characters, not bytes.
        assert!(clean_note(&"é".repeat(MAX_NOTE_CHARS)).is_ok());

        let dir = Dir::new();
        let store = MemoryStore::open(&dir.0);
        assert_eq!(refusal(store.add("p", " ")), "The note is empty.");
        assert!(store.list("p").unwrap().is_empty());
        store.add("p", "line one\nline two").unwrap();
        assert_eq!(texts(&store, "p"), ["line one line two"]);
    }

    #[test]
    fn memory_is_capped_in_total_and_a_forget_makes_room() {
        assert_eq!(MAX_TOTAL_BYTES, 16_000);
        let dir = Dir::new();
        let store = MemoryStore::open(&dir.0);
        for _ in 0..16 {
            store.add("p", &"x".repeat(1000)).unwrap();
        }
        assert_eq!(
            refusal(store.add("p", "one more")),
            "Memory is full (16,000 bytes). Forget a note first."
        );
        assert_eq!(store.list("p").unwrap().len(), 16);
        store.remove("p", 3).unwrap();
        store.add("p", "fits now").unwrap();
    }

    #[test]
    fn numbers_close_up_after_a_forget_and_a_missing_number_is_refused() {
        let dir = Dir::new();
        let store = MemoryStore::open(&dir.0);
        assert_eq!(
            refusal(store.remove("p", 1)),
            "No note 1. There are no notes."
        );
        for text in ["a", "b", "c"] {
            store.add("p", text).unwrap();
        }
        store.remove("p", 2).unwrap();
        assert_eq!(texts(&store, "p"), ["a", "c"]);
        assert_eq!(store.add("p", "d").unwrap(), 3);
        assert_eq!(
            refusal(store.remove("p", 4)),
            "No note 4. There are 3 notes."
        );
        assert_eq!(
            refusal(store.remove("p", 0)),
            "No note 0. There are 3 notes."
        );
        store.remove("p", 3).unwrap();
        store.remove("p", 1).unwrap();
        assert_eq!(refusal(store.remove("p", 2)), "No note 2. There is 1 note.");
    }

    #[test]
    fn a_corrupt_file_is_left_alone_and_names_itself() {
        let dir = Dir::new();
        let store = MemoryStore::open(&dir.0);
        let file = store.path("p").unwrap();
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, "{ not json").unwrap();
        for result in [
            store.list("p").map(|_| ()),
            store.add("p", "x").map(|_| ()),
            store.remove("p", 1),
        ] {
            assert!(matches!(result, Err(Error::MemoryUnreadable(ref p)) if *p == file));
        }
        assert_eq!(fs::read_to_string(&file).unwrap(), "{ not json");
        let message = Error::MemoryUnreadable(file.clone()).to_string();
        assert!(message.contains(&file.display().to_string()), "{message}");
        // An empty file, or a file without the list, is no notes.
        fs::write(&file, "").unwrap();
        assert!(store.list("p").unwrap().is_empty());
        fs::write(&file, "{}").unwrap();
        assert!(store.list("p").unwrap().is_empty());
    }

    #[test]
    fn delete_removes_one_project_and_ids_cannot_escape() {
        let dir = Dir::new();
        let store = MemoryStore::open(&dir.0);
        store.add("p", "a").unwrap();
        store.add("q", "b").unwrap();
        store.delete("p");
        assert!(!dir.0.join("leads/p").exists());
        assert_eq!(texts(&store, "q"), ["b"]);
        store.delete("p");
        for bad in ["", "..", "a/b", "../x", "a.b"] {
            assert!(
                matches!(store.path(bad), Err(Error::UnknownProject)),
                "{bad}"
            );
        }
        store.delete("..");
        assert!(dir.0.exists());
    }

    #[test]
    fn instructions_end_with_the_notes_and_are_unchanged_without_them() {
        let note = |text: &str| MemoryNote {
            text: text.into(),
            created: 0,
        };
        assert_eq!(instructions_with_notes("Guide.\n", &[]), "Guide.\n");
        let text = instructions_with_notes("Guide.\n", &[note("Use pnpm."), note("No mocks.")]);
        assert!(text.starts_with("Guide.\n\n## Memory\n\nNotes you saved earlier"));
        assert!(text.ends_with("\n\n- Use pnpm.\n- No mocks."), "{text}");
        assert!(!text.contains('\u{2014}'));
        // The part before the section is the guide, byte for byte.
        assert!(text.starts_with("Guide."));
    }
}
