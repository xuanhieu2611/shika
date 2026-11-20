//! Opt-in `node_modules` cloning: when it clones, every reason it skips, and
//! how the key takes part in consent.

use super::*;
use crate::preparation_tests::{Fixture, git};
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::sync::{Arc, Mutex};

/// A pnpm project with a root and one workspace package, committed, and
/// installed in the main checkout.
fn installed() -> Fixture {
    let f = Fixture::new();
    fs::write(
        f.repo.join(".gitignore"),
        ".env.local\nlocal/\n*.generated\nnode_modules/\n",
    )
    .unwrap();
    fs::write(f.repo.join("package.json"), r#"{"name":"root"}"#).unwrap();
    fs::write(f.repo.join("pnpm-lock.yaml"), "lockfileVersion: '9.0'\n").unwrap();
    fs::create_dir_all(f.repo.join("packages/a")).unwrap();
    fs::write(f.repo.join("packages/a/package.json"), r#"{"name":"a"}"#).unwrap();
    git(&f.repo, &["add", "."]);
    git(&f.repo, &["commit", "-m", "packages"]);
    let modules = f.repo.join("node_modules");
    fs::create_dir_all(modules.join("pkg")).unwrap();
    fs::write(modules.join("pkg/index.js"), "main").unwrap();
    fs::create_dir_all(modules.join(".bin")).unwrap();
    symlink("../pkg/index.js", modules.join(".bin/pkg")).unwrap();
    symlink("../packages/a", modules.join("a")).unwrap();
    for cache in [".cache", ".vite"] {
        fs::create_dir_all(modules.join(cache).join("deep")).unwrap();
        fs::write(modules.join(cache).join("deep/entry"), "/abs/path").unwrap();
    }
    fs::create_dir_all(f.repo.join("packages/a/node_modules/dep")).unwrap();
    fs::write(f.repo.join("packages/a/node_modules/dep/index.js"), "dep").unwrap();
    f
}

fn cloning(f: &Fixture, commands: &[&str]) -> PreparationConfig {
    let config = PreparationConfig {
        commands: commands.iter().map(|s| s.to_string()).collect(),
        copy_files: Vec::new(),
        timeout_seconds: 30,
        clone_dependencies: true,
    };
    f.core
        .save_and_approve_project_preparation(&f.project.id, None, &config)
        .unwrap();
    config
}

/// Launch through preparation and return the session with its setup output.
fn launch(f: &Fixture) -> (Result<Session>, String) {
    let output = Arc::new(Mutex::new(Vec::new()));
    let sink = output.clone();
    let session = f.core.create_session_with_preparation(
        &f.project.id,
        "claude",
        PtySize::default(),
        |_, _| {},
        LaunchOptions::default(),
        PreparationControl::default(),
        move |event| {
            if let PreparationEvent::Output(bytes) = event {
                sink.lock().unwrap().extend(bytes);
            }
        },
    );
    let text = String::from_utf8_lossy(&output.lock().unwrap()).into_owned();
    (session, text)
}

#[test]
fn matching_packages_clone_node_modules_before_setup_without_caches() {
    let f = installed();
    cloning(
        &f,
        &["test -f node_modules/pkg/index.js && printf seen > cloned.generated"],
    );
    let (session, output) = launch(&f);
    let session = session.unwrap();
    let task = &session.worktree;
    assert!(
        output.contains("Cloned node_modules, packages/a/node_modules from the main checkout."),
        "{output}"
    );
    // Setup commands run after the clone and see it.
    assert_eq!(
        fs::read_to_string(task.join("cloned.generated")).unwrap(),
        "seen"
    );
    assert_eq!(
        fs::read_to_string(task.join("packages/a/node_modules/dep/index.js")).unwrap(),
        "dep"
    );
    // Relative links resolve inside the task, never back to the main checkout.
    assert_eq!(
        fs::read_link(task.join("node_modules/.bin/pkg")).unwrap(),
        Path::new("../pkg/index.js")
    );
    assert_eq!(
        fs::canonicalize(task.join("node_modules/a")).unwrap(),
        task.join("packages/a").canonicalize().unwrap()
    );
    assert!(!task.join("node_modules/.cache").exists());
    assert!(!task.join("node_modules/.vite").exists());
    assert!(f.repo.join("node_modules/.cache/deep/entry").exists());
    // Copy-on-write: a write in the task leaves the main checkout alone.
    fs::write(task.join("node_modules/pkg/index.js"), "task edit").unwrap();
    assert_eq!(
        fs::read_to_string(f.repo.join("node_modules/pkg/index.js")).unwrap(),
        "main"
    );
    // Ignored dependencies keep the task clean for Close, and staging in the
    // task's Git admin folder is gone once the clones land.
    assert_eq!(git(task, &["status", "--porcelain"]), "");
    let admin = git(task, &["rev-parse", "--absolute-git-dir"]);
    assert!(!Path::new(&admin).join("shika-clone").exists());
}

#[test]
fn changed_package_files_skip_the_clone_and_setup_still_runs() {
    for changed in ["package.json", "pnpm-lock.yaml", "packages/a/package.json"] {
        let f = installed();
        cloning(&f, &["printf ran > setup.generated"]);
        // An uncommitted edit in the main checkout: its install no longer
        // matches the task's base.
        fs::write(f.repo.join(changed), "{\"changed\":true}").unwrap();
        let (session, output) = launch(&f);
        let task = session.unwrap().worktree;
        assert!(
            output.contains(&format!(
                "Did not clone node_modules: {changed} differs from the main checkout."
            )),
            "{output}"
        );
        assert!(!task.join("node_modules").exists());
        assert!(!task.join("packages/a/node_modules").exists());
        assert!(task.join("setup.generated").exists());
    }
}

#[test]
fn missing_or_unignored_node_modules_are_never_cloned() {
    let f = installed();
    cloning(&f, &[]);
    fs::remove_dir_all(f.repo.join("node_modules")).unwrap();
    fs::remove_dir_all(f.repo.join("packages/a/node_modules")).unwrap();
    let (session, output) = launch(&f);
    assert!(!session.unwrap().worktree.join("node_modules").exists());
    assert!(
        output.contains("the main checkout has no node_modules"),
        "{output}"
    );

    let f = installed();
    cloning(&f, &[]);
    // Only the workspace package's folder is left unignored on the base.
    fs::write(
        f.repo.join(".gitignore"),
        ".env.local\nlocal/\n*.generated\n/node_modules/\n",
    )
    .unwrap();
    git(&f.repo, &["commit", "-am", "ignore only the root"]);
    let (session, output) = launch(&f);
    let task = session.unwrap().worktree;
    assert!(task.join("node_modules/pkg/index.js").exists());
    assert!(!task.join("packages/a/node_modules").exists());
    assert!(
        output.contains("packages/a/node_modules is not ignored by Git in the task"),
        "{output}"
    );
}

#[test]
fn links_into_the_main_checkout_keep_that_clone_out_of_the_task() {
    for target in [
        // Absolute, into the main checkout.
        None,
        // Relative, climbing out of the task into the main checkout.
        Some("../../../../src"),
    ] {
        let f = installed();
        cloning(&f, &["printf ran > setup.generated"]);
        let link = f.repo.join("node_modules/pkg/escape");
        match target {
            Some(relative) => symlink(relative, &link).unwrap(),
            None => symlink(f.repo.join("tracked.txt"), &link).unwrap(),
        }
        let (session, output) = launch(&f);
        let task = session.unwrap().worktree;
        assert!(!task.join("node_modules").exists(), "{target:?}");
        assert!(
            output.contains("node_modules links into the main checkout"),
            "{output}"
        );
        // Other folders and the setup commands are unaffected.
        assert!(task.join("packages/a/node_modules/dep/index.js").exists());
        assert!(task.join("setup.generated").exists());
        assert!(link.symlink_metadata().is_ok());
    }
    // A link outside the repository is the same link from either checkout.
    let f = installed();
    cloning(&f, &[]);
    symlink("/usr/bin/env", f.repo.join("node_modules/pkg/env")).unwrap();
    let (session, _) = launch(&f);
    assert!(
        session
            .unwrap()
            .worktree
            .join("node_modules/pkg/env")
            .symlink_metadata()
            .is_ok()
    );
}

#[test]
fn a_failed_clone_falls_back_to_setup_and_keeps_a_symlinked_source_out() {
    let f = installed();
    cloning(&f, &["printf ran > setup.generated"]);
    // An unreadable folder inside fails the whole atomic clone.
    let modules = f.repo.join("node_modules/pkg");
    fs::set_permissions(&modules, fs::Permissions::from_mode(0o000)).unwrap();
    let (session, output) = launch(&f);
    fs::set_permissions(&modules, fs::Permissions::from_mode(0o755)).unwrap();
    let task = session.unwrap().worktree;
    assert!(output.contains("Did not clone node_modules"), "{output}");
    assert!(!task.join("node_modules").exists());
    assert!(task.join("setup.generated").exists());

    // A main-checkout node_modules that is a link is never followed.
    let f = installed();
    cloning(&f, &[]);
    let elsewhere = f.root.join("elsewhere");
    fs::rename(f.repo.join("node_modules"), &elsewhere).unwrap();
    symlink(&elsewhere, f.repo.join("node_modules")).unwrap();
    let (session, output) = launch(&f);
    let task = session.unwrap().worktree;
    assert!(task.join("node_modules").symlink_metadata().is_err());
    assert!(
        task.join("packages/a/node_modules/dep").exists(),
        "{output}"
    );
}

#[test]
fn clone_dependencies_takes_part_in_consent_only_when_on() {
    let parsed = |text: &str| serde_json::from_str::<PreparationConfig>(text).unwrap();
    let off = parsed(r#"{"setup-worktree":["true"]}"#);
    assert_eq!(
        off,
        parsed(r#"{"setup-worktree":["true"],"clone-dependencies":false}"#)
    );
    assert!(!off.clone_dependencies);
    // Off is never written, so saved files and approvals stay as they were.
    assert!(
        !String::from_utf8(serde_json::to_vec(&off).unwrap())
            .unwrap()
            .contains("clone")
    );
    let on = parsed(r#"{"setup-worktree":["true"],"clone-dependencies":true}"#);
    assert_ne!(on, off);
    assert!(
        String::from_utf8(serde_json::to_vec(&on).unwrap())
            .unwrap()
            .contains("\"clone-dependencies\":true")
    );

    let f = Fixture::new();
    let config = f.config(&["true"], &[], 600);
    f.core.approve_preparation(&f.project.id, &config).unwrap();
    // An approval from before the key still matches a file that spells it off.
    fs::write(
        f.repo.join(preparation::CONFIG_PATH),
        r#"{"setup-worktree":["true"],"clone-dependencies":false}"#,
    )
    .unwrap();
    assert!(f.launch().is_ok());
    // Turning it on is new behavior and asks again.
    fs::write(
        f.repo.join(preparation::CONFIG_PATH),
        r#"{"setup-worktree":["true"],"clone-dependencies":true}"#,
    )
    .unwrap();
    assert_eq!(f.launch(), Err(Error::PreparationNeedsApproval));
}

#[test]
fn setup_suggestions_clone_only_for_installs_that_keep_node_modules() {
    let f = installed();
    let draft = f.core.project_preparation_draft(&f.project.id).unwrap();
    assert_eq!(draft.config.commands, ["pnpm install --frozen-lockfile"]);
    assert!(draft.config.clone_dependencies);

    // npm ci deletes node_modules before installing.
    fs::remove_file(f.repo.join("pnpm-lock.yaml")).unwrap();
    fs::write(f.repo.join("package-lock.json"), "{}").unwrap();
    let draft = f.core.project_preparation_draft(&f.project.id).unwrap();
    assert_eq!(draft.config.commands, ["npm ci"]);
    assert!(!draft.config.clone_dependencies);

    let f = installed();
    fs::write(f.repo.join(".gitignore"), "*.generated\n").unwrap();
    assert!(
        !f.core
            .project_preparation_draft(&f.project.id)
            .unwrap()
            .config
            .clone_dependencies
    );

    let f = installed();
    fs::remove_dir_all(f.repo.join("node_modules")).unwrap();
    let draft = f.core.project_preparation_draft(&f.project.id).unwrap();
    assert_eq!(draft.config.commands, ["pnpm install --frozen-lockfile"]);
    assert!(!draft.config.clone_dependencies);
}
