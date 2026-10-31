use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::preparation::PreparationConfig;
use crate::worktree::git_cmd;

/// A saved repository. Optional base, setup consent, and onboarding preference
/// fields have backward-compatible defaults, so older project files still load.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    pub id: String,
    pub name: String,
    pub path: PathBuf,
    /// The branch new agents start from, as the user typed it (`dev`).
    /// None means the remote default branch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_branch: Option<String>,
    /// Local consent for the exact parsed setup configuration. Copied file
    /// contents are never stored here. Missing means setup is unapproved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approved_preparation: Option<PreparationConfig>,
    /// Local onboarding preference, not execution consent. Missing means the
    /// next author-created New may offer setup once for this project.
    #[serde(default, skip_serializing_if = "is_false")]
    pub worktree_setup_reviewed: bool,
    /// The project's saved Lead, so a relaunch can resume its conversation.
    /// Missing means no Lead has been started or it was forgotten.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lead: Option<SavedLead>,
}

/// What a project remembers about its Lead between launches: which CLI it ran
/// in and the id of its conversation. Never a transcript.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SavedLead {
    /// The CLI preset id the conversation belongs to (`claude`, `codex`, ...).
    pub cli: String,
    /// The conversation id. Missing while Codex's has not been discovered yet,
    /// or when Cursor's `create-chat` failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Worktrees of workers this Lead started before a relaunch that the
    /// restart summary already named, so a later relaunch does not repeat
    /// them. Pruned when the worktree no longer exists.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reported_workers: Vec<PathBuf>,
    /// Shika-owned Lead lifetime history, independent of provider context.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_history: Option<String>,
}

fn is_false(value: &bool) -> bool {
    !value
}

impl Project {
    pub fn preparation_onboarding_pending(&self) -> bool {
        !self.worktree_setup_reviewed && self.approved_preparation.is_none()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectAdded {
    pub project: Project,
    /// A line for the user when the pick was adjusted or already listed.
    pub note: Option<String>,
}

pub struct ProjectDb {
    path: PathBuf,
    lock: Mutex<()>,
}

impl ProjectDb {
    pub fn open(path: PathBuf) -> Self {
        Self {
            path,
            lock: Mutex::new(()),
        }
    }

    pub fn list(&self) -> Result<Vec<Project>> {
        let _guard = self.lock.lock().unwrap_or_else(|err| err.into_inner());
        load(&self.path)
    }

    pub fn get(&self, id: &str) -> Result<Project> {
        self.list()?
            .into_iter()
            .find(|project| project.id == id)
            .ok_or(Error::UnknownProject)
    }

    /// Validates a picked folder and saves its git root. Blocking: runs git.
    pub fn add(&self, git: &Path, path_env: &str, picked: &Path) -> Result<ProjectAdded> {
        let _guard = self.lock.lock().unwrap_or_else(|err| err.into_inner());
        let toplevel = git_toplevel(git, path_env, picked)?;
        let mut projects = load(&self.path)?;
        if let Some(existing) = projects
            .iter()
            .find(|project| same_dir(&project.path, &toplevel))
        {
            return Ok(ProjectAdded {
                project: existing.clone(),
                note: Some("That repository is already in the list.".to_string()),
            });
        }

        let name = folder_name(&toplevel);
        let nested = !same_dir(picked, &toplevel);
        let project = Project {
            id: new_id(&projects),
            name: name.clone(),
            path: toplevel,
            base_branch: None,
            approved_preparation: None,
            worktree_setup_reviewed: false,
            lead: None,
        };
        projects.push(project.clone());
        save(&self.path, &projects)?;
        let note = nested.then(|| format!("Using the git root \"{name}\"."));
        Ok(ProjectAdded { project, note })
    }

    /// Saves the project's base branch, or clears it with None. The caller
    /// has already checked that the branch exists.
    pub fn set_base_branch(&self, id: &str, branch: Option<String>) -> Result<Project> {
        let _guard = self.lock.lock().unwrap_or_else(|err| err.into_inner());
        let mut projects = load(&self.path)?;
        let project = projects
            .iter_mut()
            .find(|project| project.id == id)
            .ok_or(Error::UnknownProject)?;
        project.base_branch = branch;
        let project = project.clone();
        save(&self.path, &projects)?;
        Ok(project)
    }

    pub fn approve_preparation(&self, id: &str, config: PreparationConfig) -> Result<()> {
        let _guard = self.lock.lock().unwrap_or_else(|err| err.into_inner());
        let mut projects = load(&self.path)?;
        let project = projects
            .iter_mut()
            .find(|project| project.id == id)
            .ok_or(Error::UnknownProject)?;
        project.approved_preparation = Some(config);
        project.worktree_setup_reviewed = true;
        save(&self.path, &projects)
    }

    pub fn review_preparation(&self, id: &str) -> Result<()> {
        let _guard = self.lock.lock().unwrap_or_else(|err| err.into_inner());
        let mut projects = load(&self.path)?;
        let project = projects
            .iter_mut()
            .find(|project| project.id == id)
            .ok_or(Error::UnknownProject)?;
        if project.worktree_setup_reviewed {
            return Ok(());
        }
        project.worktree_setup_reviewed = true;
        save(&self.path, &projects)
    }

    /// Replaces the project's saved Lead, or removes it with None.
    pub fn set_lead(&self, id: &str, lead: Option<SavedLead>) -> Result<()> {
        let _guard = self.lock.lock().unwrap_or_else(|err| err.into_inner());
        let mut projects = load(&self.path)?;
        let project = projects
            .iter_mut()
            .find(|project| project.id == id)
            .ok_or(Error::UnknownProject)?;
        if project.lead == lead {
            return Ok(());
        }
        project.lead = lead;
        save(&self.path, &projects)
    }

    /// Records worktrees the restart summary named, and drops earlier ones
    /// whose folder is gone. [`Error::NoSavedLead`] without a saved Lead.
    pub fn mark_workers_reported(&self, id: &str, paths: &[PathBuf]) -> Result<()> {
        let _guard = self.lock.lock().unwrap_or_else(|err| err.into_inner());
        let mut projects = load(&self.path)?;
        let lead = projects
            .iter_mut()
            .find(|project| project.id == id)
            .ok_or(Error::UnknownProject)?
            .lead
            .as_mut()
            .ok_or(Error::NoSavedLead)?;
        let mut reported = lead.reported_workers.clone();
        for path in paths {
            if !reported.contains(path) {
                reported.push(path.clone());
            }
        }
        reported.retain(|path| path.exists());
        if lead.reported_workers == reported {
            return Ok(());
        }
        lead.reported_workers = reported;
        save(&self.path, &projects)
    }

    /// Sets or clears the saved Lead's conversation id.
    pub fn set_lead_session(&self, id: &str, session_id: Option<String>) -> Result<()> {
        let _guard = self.lock.lock().unwrap_or_else(|err| err.into_inner());
        let mut projects = load(&self.path)?;
        let lead = projects
            .iter_mut()
            .find(|project| project.id == id)
            .ok_or(Error::UnknownProject)?
            .lead
            .as_mut()
            .ok_or(Error::NoSavedLead)?;
        if lead.session_id == session_id {
            return Ok(());
        }
        lead.session_id = session_id;
        save(&self.path, &projects)
    }

    pub fn remove(&self, id: &str) -> Result<()> {
        let _guard = self.lock.lock().unwrap_or_else(|err| err.into_inner());
        let mut projects = load(&self.path)?;
        let before = projects.len();
        projects.retain(|project| project.id != id);
        if projects.len() == before {
            return Err(Error::UnknownProject);
        }
        save(&self.path, &projects)
    }
}

fn load(path: &Path) -> Result<Vec<Project>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = fs::read_to_string(path).map_err(|_| Error::ReadProjects)?;
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }
    serde_json::from_str(&text).map_err(|_| Error::ReadProjects)
}

fn save(path: &Path, projects: &[Project]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|_| Error::SaveProjects)?;
    }
    let mut json = serde_json::to_string_pretty(projects).map_err(|_| Error::SaveProjects)?;
    json.push('\n');
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, json).map_err(|_| Error::SaveProjects)?;
    fs::rename(&tmp, path).map_err(|_| Error::SaveProjects)?;
    Ok(())
}

fn git_toplevel(git: &Path, path_env: &str, picked: &Path) -> Result<PathBuf> {
    if !picked.exists() {
        return Err(Error::FolderMissing);
    }
    if !picked.is_dir() {
        return Err(Error::NotAFolder);
    }
    let output = git_cmd(git, path_env, picked)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .map_err(|_| Error::Git(None))?;
    if !output.status.success() {
        return Err(Error::NotARepository);
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let raw = text.trim();
    if raw.is_empty() {
        return Err(Error::NotARepository);
    }
    PathBuf::from(raw)
        .canonicalize()
        .map_err(|_| Error::NotARepository)
}

fn folder_name(path: &Path) -> String {
    path.file_name()
        .and_then(|name| {
            let name = name.to_string_lossy();
            if name.is_empty() {
                None
            } else {
                Some(name.into_owned())
            }
        })
        .unwrap_or_else(|| "Project".to_string())
}

fn same_dir(left: &Path, right: &Path) -> bool {
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(left), Ok(right)) => left == right,
        _ => left == right,
    }
}

fn new_id(existing: &[Project]) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let mut extra = 0u32;
    loop {
        let id = if extra == 0 {
            format!("{nanos:x}")
        } else {
            format!("{nanos:x}-{extra}")
        };
        if existing.iter().all(|project| project.id != id) {
            return id;
        }
        extra += 1;
    }
}

#[cfg(test)]
#[path = "projects_tests.rs"]
mod tests;
