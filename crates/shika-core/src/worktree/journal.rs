//! `worktrees.json`, the journal of every worktree Shika created, which is
//! the only source for leftovers.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// One Shika worktree on disk. `worktrees.json` keeps these so a quit or a
/// crash can list what was left behind.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct JournalEntry {
    pub project_id: String,
    /// Empty for a Lead's detached worktree, which has no branch.
    pub branch: String,
    pub path: PathBuf,
    /// The ref the branch started from, such as `refs/remotes/origin/dev`.
    /// Missing in entries written before it was recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_ref: Option<String>,
    /// A Lead started this worker, so a later Lead start can report it when
    /// its session did not survive. Missing in older entries means false.
    #[serde(default, skip_serializing_if = "is_false")]
    pub started_by_lead: bool,
    /// Small recovery hint, never terminal output or conversation contents.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery: Option<RecoveryMetadata>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryMetadata {
    pub cli: String,
    pub started_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

fn is_false(value: &bool) -> bool {
    !value
}

pub struct Journal {
    path: PathBuf,
    lock: Mutex<()>,
}

impl Journal {
    pub fn open(path: PathBuf) -> Self {
        Self {
            path,
            lock: Mutex::new(()),
        }
    }

    pub fn list(&self) -> Result<Vec<JournalEntry>> {
        let _guard = self.lock.lock().unwrap_or_else(|err| err.into_inner());
        load(&self.path)
    }

    pub fn add(&self, entry: &JournalEntry) -> Result<()> {
        let _guard = self.lock.lock().unwrap_or_else(|err| err.into_inner());
        let mut entries = load(&self.path)?;
        entries.push(entry.clone());
        save(&self.path, &entries)
    }

    pub fn rename_branch(&self, path: &Path, branch: &str) -> Result<()> {
        let _guard = self.lock.lock().unwrap_or_else(|err| err.into_inner());
        let mut entries = load(&self.path)?;
        let entry = entries
            .iter_mut()
            .find(|entry| entry.path == path)
            .ok_or(Error::ReadJournal)?;
        entry.branch = branch.to_string();
        save(&self.path, &entries)
    }

    /// Points the entry for `path` at another project. A path with no entry
    /// is left alone. Atomic like every journal write.
    pub fn set_project(&self, path: &Path, project_id: &str) -> Result<()> {
        let _guard = self.lock.lock().unwrap_or_else(|err| err.into_inner());
        let mut entries = load(&self.path)?;
        let mut changed = false;
        for entry in entries.iter_mut().filter(|entry| entry.path == path) {
            if entry.project_id != project_id {
                entry.project_id = project_id.to_string();
                changed = true;
            }
        }
        if changed {
            save(&self.path, &entries)?;
        }
        Ok(())
    }

    pub fn replace(&self, entry: &JournalEntry) -> Result<()> {
        let _guard = self.lock.lock().unwrap_or_else(|err| err.into_inner());
        let mut entries = load(&self.path)?;
        let target = entries
            .iter_mut()
            .find(|e| e.path == entry.path)
            .ok_or(Error::UnknownLeftover)?;
        *target = entry.clone();
        save(&self.path, &entries)
    }

    pub fn set_recovery(&self, path: &Path, recovery: RecoveryMetadata) -> Result<()> {
        let _guard = self.lock.lock().unwrap_or_else(|err| err.into_inner());
        let mut entries = load(&self.path)?;
        let entry = entries
            .iter_mut()
            .find(|entry| entry.path == path)
            .ok_or(Error::UnknownLeftover)?;
        if entry.recovery.as_ref() != Some(&recovery) {
            entry.recovery = Some(recovery);
            save(&self.path, &entries)?;
        }
        Ok(())
    }

    pub fn remove_path(&self, path: &Path) -> Result<()> {
        let _guard = self.lock.lock().unwrap_or_else(|err| err.into_inner());
        let mut entries = load(&self.path)?;
        entries.retain(|entry| entry.path != path);
        save(&self.path, &entries)
    }
}

fn load(path: &Path) -> Result<Vec<JournalEntry>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = fs::read_to_string(path).map_err(|_| Error::ReadJournal)?;
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }
    serde_json::from_str(&text).map_err(|_| Error::ReadJournal)
}

fn save(path: &Path, entries: &[JournalEntry]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|_| Error::SaveJournal)?;
    }
    let mut json = serde_json::to_string_pretty(entries).map_err(|_| Error::SaveJournal)?;
    json.push('\n');
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, json).map_err(|_| Error::SaveJournal)?;
    fs::rename(&tmp, path).map_err(|_| Error::SaveJournal)
}
