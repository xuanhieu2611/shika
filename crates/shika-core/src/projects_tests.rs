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
fn legacy_projects_offer_setup_unless_already_approved() {
    let mut project: Project =
        serde_json::from_str(r#"{"id":"old","name":"demo","path":"/tmp/demo"}"#).unwrap();
    assert!(!project.worktree_setup_reviewed);
    assert!(project.preparation_onboarding_pending());
    assert!(
        serde_json::to_value(&project)
            .unwrap()
            .get("worktreeSetupReviewed")
            .is_none()
    );
    project.approved_preparation = Some(PreparationConfig {
        commands: vec![],
        copy_files: vec![],
        timeout_seconds: 600,
    });
    assert!(!project.preparation_onboarding_pending());
    let roundtrip: Project =
        serde_json::from_value(serde_json::to_value(project).unwrap()).unwrap();
    assert!(!roundtrip.preparation_onboarding_pending());
}

#[test]
fn reviewed_setup_survives_reopen_without_granting_execution_consent() {
    let scratch = Scratch::new();
    let repo = scratch.repo("demo");
    let file = scratch.path.join("projects.json");
    let db = ProjectDb::open(file.clone());
    let project = add(&db, &repo).unwrap().project;
    db.review_preparation(&project.id).unwrap();
    let read = ProjectDb::open(file).get(&project.id).unwrap();
    assert!(read.worktree_setup_reviewed);
    assert!(!read.preparation_onboarding_pending());
    assert!(read.approved_preparation.is_none());
    assert_eq!(db.review_preparation("missing"), Err(Error::UnknownProject));
}

#[test]
fn a_saved_lead_round_trips_and_files_without_one_still_load() {
    let scratch = Scratch::new();
    let file = scratch.path.join("projects.json");
    fs::write(
        &file,
        r#"[{"id":"a","name":"demo","path":"/x/demo","futureField":1}]"#,
    )
    .unwrap();
    let db = ProjectDb::open(file.clone());
    assert_eq!(db.get("a").unwrap().lead, None);
    db.set_lead("a", None).unwrap();
    assert!(!fs::read_to_string(&file).unwrap().contains("lead"));

    let saved = SavedLead {
        cli: "claude".into(),
        session_id: Some("0198f2a4-7a3e".into()),
        reported_workers: Vec::new(),
        chat_history: None,
    };
    db.set_lead("a", Some(saved.clone())).unwrap();
    let text = fs::read_to_string(&file).unwrap();
    assert!(text.contains("\"lead\": {"), "{text}");
    assert!(text.contains("\"cli\": \"claude\""), "{text}");
    assert!(text.contains("\"sessionId\": \"0198f2a4-7a3e\""), "{text}");
    assert_eq!(
        ProjectDb::open(file.clone()).get("a").unwrap().lead,
        Some(saved)
    );

    // A record without an id (Codex before its first message) is valid.
    let hand_written = r#"[{"id":"a","name":"demo","path":"/x/demo","lead":{"cli":"codex"}}]"#;
    fs::write(&file, hand_written).unwrap();
    assert_eq!(
        db.get("a").unwrap().lead,
        Some(SavedLead {
            cli: "codex".into(),
            session_id: None,
            reported_workers: Vec::new(),
            chat_history: None,
        })
    );
    db.set_lead_session("a", Some("thread-1".into())).unwrap();
    assert_eq!(
        db.get("a").unwrap().lead.unwrap().session_id.as_deref(),
        Some("thread-1")
    );
    db.set_lead_session("a", None).unwrap();
    assert!(!fs::read_to_string(&file).unwrap().contains("sessionId"));
    assert_eq!(
        db.set_lead_session("missing", None),
        Err(Error::UnknownProject)
    );
    db.set_lead("a", None).unwrap();
    assert_eq!(db.set_lead_session("a", None), Err(Error::NoSavedLead));
    assert_eq!(db.set_lead("missing", None), Err(Error::UnknownProject));
}

#[test]
fn reported_workers_accumulate_dedupe_and_drop_vanished_folders() {
    let scratch = Scratch::new();
    let file = scratch.path.join("projects.json");
    let db = ProjectDb::open(file.clone());
    fs::write(&file, r#"[{"id":"a","name":"demo","path":"/x/demo"}]"#).unwrap();
    let (one, two) = (scratch.path.join("one"), scratch.path.join("two"));
    fs::create_dir(&one).unwrap();
    fs::create_dir(&two).unwrap();
    assert_eq!(
        db.mark_workers_reported("a", std::slice::from_ref(&one)),
        Err(Error::NoSavedLead)
    );
    db.set_lead(
        "a",
        Some(SavedLead {
            cli: "claude".into(),
            session_id: None,
            reported_workers: Vec::new(),
            chat_history: None,
        }),
    )
    .unwrap();
    db.mark_workers_reported("a", std::slice::from_ref(&one))
        .unwrap();
    db.mark_workers_reported("a", &[one.clone(), two.clone()])
        .unwrap();
    let text = fs::read_to_string(&file).unwrap();
    assert!(text.contains("reportedWorkers"), "{text}");
    assert_eq!(
        db.get("a").unwrap().lead.unwrap().reported_workers,
        [one.clone(), two.clone()]
    );
    fs::remove_dir(&one).unwrap();
    db.mark_workers_reported("a", &[]).unwrap();
    assert_eq!(
        db.get("a").unwrap().lead.unwrap().reported_workers,
        std::slice::from_ref(&two)
    );
    fs::remove_dir(&two).unwrap();
    db.mark_workers_reported("a", &[]).unwrap();
    assert!(
        !fs::read_to_string(&file)
            .unwrap()
            .contains("reportedWorkers")
    );
}

#[test]
fn removing_a_project_drops_its_saved_lead() {
    let scratch = Scratch::new();
    let repo = scratch.repo("demo");
    let file = scratch.path.join("projects.json");
    let db = ProjectDb::open(file.clone());
    let project = add(&db, &repo).unwrap().project;
    db.set_lead(
        &project.id,
        Some(SavedLead {
            cli: "pi".into(),
            session_id: None,
            reported_workers: Vec::new(),
            chat_history: None,
        }),
    )
    .unwrap();
    db.remove(&project.id).unwrap();
    assert!(!fs::read_to_string(&file).unwrap().contains("lead"));
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
