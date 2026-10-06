use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::preparation::PreparationConfig;
use crate::worktree::git_cmd;

/// A saved repository. The JSON shape is the one `projects.json` has always
/// had, plus an optional `baseBranch`, so files written by the Tauri build
/// load unchanged.
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
mod tests {
    use super::*;
    use std::process::Command;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn add(db: &ProjectDb, picked: &Path) -> Result<ProjectAdded> {
        db.add(Path::new("git"), "", picked)
    }

    struct Scratch {
        path: PathBuf,
    }

    impl Scratch {
        fn new() -> Self {
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!("shika-projects-{nanos}-{n}"));
            fs::create_dir_all(&path).unwrap();
            Self { path }
        }

        fn repo(&self, name: &str) -> PathBuf {
            let dir = self.path.join(name);
            fs::create_dir_all(&dir).unwrap();
            git_init(&dir);
            dir
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn git_init(dir: &Path) {
        let status = Command::new("git")
            .arg("init")
            .current_dir(dir)
            .status()
            .expect("git init");
        assert!(status.success(), "git init failed in {}", dir.display());
    }

    #[test]
    fn nested_folder_uses_git_root_and_says_so() {
        let scratch = Scratch::new();
        let repo = scratch.repo("demo-repo");
        let nested = repo.join("crates").join("app");
        fs::create_dir_all(&nested).unwrap();
        let db = ProjectDb::open(scratch.path.join("projects.json"));

        let added = add(&db, &nested).unwrap();

        assert_eq!(added.project.name, "demo-repo");
        assert_eq!(added.project.path, repo.canonicalize().unwrap());
        assert_eq!(
            added.note.as_deref(),
            Some("Using the git root \"demo-repo\".")
        );
        assert_eq!(db.list().unwrap().len(), 1);
    }

    #[test]
    fn adding_the_root_has_no_note() {
        let scratch = Scratch::new();
        let repo = scratch.repo("shika");
        let db = ProjectDb::open(scratch.path.join("projects.json"));

        let added = add(&db, &repo).unwrap();

        assert_eq!(added.project.name, "shika");
        assert_eq!(added.note, None);
    }

    #[test]
    fn the_same_repository_is_not_added_twice() {
        let scratch = Scratch::new();
        let repo = scratch.repo("demo-repo");
        let nested = repo.join("src");
        fs::create_dir(&nested).unwrap();
        let db = ProjectDb::open(scratch.path.join("projects.json"));
        let first = add(&db, &repo).unwrap();

        let again = add(&db, &nested).unwrap();

        assert_eq!(again.project.id, first.project.id);
        assert_eq!(
            again.note.as_deref(),
            Some("That repository is already in the list.")
        );
        assert_eq!(db.list().unwrap().len(), 1);
    }

    #[test]
    fn same_folder_name_in_different_places_both_stay() {
        let scratch = Scratch::new();
        let first_repo = scratch.repo("app");
        let other = scratch.path.join("other");
        fs::create_dir(&other).unwrap();
        let second_repo = other.join("app");
        fs::create_dir(&second_repo).unwrap();
        git_init(&second_repo);
        let db = ProjectDb::open(scratch.path.join("projects.json"));

        let first = add(&db, &first_repo).unwrap();
        let second = add(&db, &second_repo).unwrap();

        assert_ne!(first.project.id, second.project.id);
        assert_eq!(first.project.name, "app");
        assert_eq!(second.project.name, "app");
        assert_eq!(db.list().unwrap().len(), 2);
    }

    #[test]
    fn plain_folder_file_and_missing_path_are_rejected() {
        let scratch = Scratch::new();
        let plain = scratch.path.join("notes");
        fs::create_dir(&plain).unwrap();
        let file = scratch.path.join("notes.txt");
        fs::write(&file, "hi").unwrap();
        let db = ProjectDb::open(scratch.path.join("projects.json"));

        assert_eq!(add(&db, &plain).unwrap_err(), Error::NotARepository);
        assert_eq!(add(&db, &file).unwrap_err(), Error::NotAFolder);
        assert_eq!(
            add(&db, &scratch.path.join("missing")).unwrap_err(),
            Error::FolderMissing
        );
        assert!(db.list().unwrap().is_empty());
    }

    #[test]
    fn list_survives_reopen_and_remove() {
        let scratch = Scratch::new();
        let alpha = scratch.repo("alpha");
        let beta = scratch.repo("beta");
        let file = scratch.path.join("projects.json");
        let first_id = {
            let db = ProjectDb::open(file.clone());
            let added = add(&db, &alpha).unwrap();
            add(&db, &beta).unwrap();
            added.project.id
        };

        let db = ProjectDb::open(file);
        let listed = db.list().unwrap();
        assert_eq!(
            listed
                .iter()
                .map(|project| project.name.as_str())
                .collect::<Vec<_>>(),
            ["alpha", "beta"]
        );

        db.remove(&first_id).unwrap();
        assert_eq!(
            db.list()
                .unwrap()
                .iter()
                .map(|project| project.name.as_str())
                .collect::<Vec<_>>(),
            ["beta"]
        );
        assert_eq!(db.remove("missing").unwrap_err(), Error::UnknownProject);
    }

    #[test]
    fn a_base_branch_is_saved_and_cleared() {
        let scratch = Scratch::new();
        let repo = scratch.repo("demo");
        let file = scratch.path.join("projects.json");
        let db = ProjectDb::open(file.clone());
        let id = add(&db, &repo).unwrap().project.id;
        // Unset is left out of the file, as in files written before it existed.
        assert!(!fs::read_to_string(&file).unwrap().contains("baseBranch"));

        let saved = db.set_base_branch(&id, Some("dev".into())).unwrap();
        assert_eq!(saved.base_branch.as_deref(), Some("dev"));
        assert!(
            fs::read_to_string(&file)
                .unwrap()
                .contains("\"baseBranch\": \"dev\"")
        );
        assert_eq!(
            ProjectDb::open(file.clone()).get(&id).unwrap().base_branch,
            Some("dev".into())
        );

        db.set_base_branch(&id, None).unwrap();
        assert_eq!(db.get(&id).unwrap().base_branch, None);
        assert!(!fs::read_to_string(&file).unwrap().contains("baseBranch"));
        assert_eq!(
            db.set_base_branch("missing", None).unwrap_err(),
            Error::UnknownProject
        );
    }

    #[test]
    fn empty_file_is_an_empty_list_and_corrupt_file_is_an_error() {
        let scratch = Scratch::new();
        let file = scratch.path.join("projects.json");
        let db = ProjectDb::open(file.clone());

        fs::write(&file, "\n").unwrap();
        assert!(db.list().unwrap().is_empty());

        fs::write(&file, "{").unwrap();
        assert_eq!(db.list().unwrap_err(), Error::ReadProjects);
    }
}
