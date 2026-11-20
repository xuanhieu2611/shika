//! Unsaved setup suggestions: what detection proposes, and what it leaves out.

use super::*;
use crate::preparation_tests::{Fixture, git};
use std::fs;
use std::os::unix::fs::symlink;

#[test]
fn setup_suggestions_are_unsaved_and_never_used_by_new() {
    let f = Fixture::new();
    fs::write(f.repo.join(".env.local"), "fixture-only").unwrap();
    fs::write(
        f.repo.join("package.json"),
        r#"{"packageManager":"npm@10.8.0","scripts":{"preinstall":"touch ran.generated"}}"#,
    )
    .unwrap();
    fs::write(f.repo.join("package-lock.json"), "{}").unwrap();
    let draft = f.core.project_preparation_draft(&f.project.id).unwrap();
    assert!(draft.existing.is_none());
    assert_eq!(draft.config.copy_files, [".env.local"]);
    assert_eq!(draft.config.commands, ["npm ci"]);
    assert_eq!(draft.config.timeout_seconds, 600);
    assert!(draft.note.contains("not saved"));
    assert!(!f.repo.join(".shika").exists());
    assert!(!f.repo.join("ran.generated").exists());
    assert!(
        !f.core
            .preparation_approved(&f.project.id, &draft.config)
            .unwrap()
    );
    // Detection is a preview, not implicit core setup. Author-facing New offers
    // onboarding; programmatic launches never infer or save preparation.
    let session = f.launch().unwrap();
    assert!(!session.worktree.join(".env.local").exists());
    assert!(!f.repo.join(".shika").exists());
    f.core
        .save_project_preparation(&f.project.id, None, Some(&draft.config))
        .unwrap();
    assert_eq!(f.launch(), Err(Error::PreparationNeedsApproval));
}

#[test]
fn setup_suggestions_preserve_existing_configuration_even_when_empty() {
    let f = Fixture::new();
    fs::write(f.repo.join(".env.local"), "fixture-only").unwrap();
    fs::write(
        f.repo.join("package.json"),
        r#"{"packageManager":"pnpm@10.0.0"}"#,
    )
    .unwrap();
    let saved = f.config(&[], &[], 42);
    let draft = f.core.project_preparation_draft(&f.project.id).unwrap();
    assert_eq!(draft.existing, Some(saved.clone()));
    assert_eq!(draft.config, saved);
    // Existing config also avoids reading unsafe package metadata entirely.
    fs::remove_file(f.repo.join("package.json")).unwrap();
    symlink("missing", f.repo.join("package.json")).unwrap();
    assert!(
        f.core
            .project_preparation_draft(&f.project.id)
            .unwrap()
            .existing
            .is_some()
    );
}

#[test]
fn setup_suggestions_only_include_root_regular_ignored_dotenv_files() {
    let f = Fixture::new();
    fs::write(f.repo.join(".env"), "not ignored").unwrap();
    fs::write(f.repo.join(".env.local"), "ignored").unwrap();
    fs::write(f.repo.join(".env.production"), "not suggested").unwrap();
    let draft = f.core.project_preparation_draft(&f.project.id).unwrap();
    assert_eq!(draft.config.copy_files, [".env.local"]);
    fs::write(
        f.repo.join(".gitignore"),
        ".env\n.env.local\n.env.production\n*.generated\n",
    )
    .unwrap();
    assert_eq!(
        f.core
            .project_preparation_draft(&f.project.id)
            .unwrap()
            .config
            .copy_files,
        [".env", ".env.local"]
    );
    git(&f.repo, &["add", "-f", ".env.local"]);
    assert_eq!(
        f.core
            .project_preparation_draft(&f.project.id)
            .unwrap()
            .config
            .copy_files,
        [".env"]
    );
    fs::remove_file(f.repo.join(".env")).unwrap();
    symlink("tracked.txt", f.repo.join(".env")).unwrap();
    assert!(
        f.core
            .project_preparation_draft(&f.project.id)
            .unwrap()
            .config
            .copy_files
            .is_empty()
    );
    fs::remove_file(f.repo.join(".env")).unwrap();
    fs::create_dir(f.repo.join(".env")).unwrap();
    assert!(
        f.core
            .project_preparation_draft(&f.project.id)
            .unwrap()
            .config
            .copy_files
            .is_empty()
    );
    assert!(!f.repo.join(".shika").exists());
}

#[test]
fn setup_suggestions_use_unambiguous_npm_and_pnpm_metadata() {
    let f = Fixture::new();
    for (package, locks, command) in [
        ("{}", vec!["package-lock.json"], "npm ci"),
        ("{}", vec!["npm-shrinkwrap.json"], "npm ci"),
        (
            r#"{"packageManager":"npm@10.8.0"}"#,
            vec!["npm-shrinkwrap.json", "package-lock.json"],
            "npm ci",
        ),
        (
            "{}",
            vec!["pnpm-lock.yaml"],
            "pnpm install --frozen-lockfile",
        ),
        (
            r#"{"packageManager":"pnpm@10.0.0+sha512.example"}"#,
            vec!["pnpm-lock.yaml"],
            "pnpm install --frozen-lockfile",
        ),
        (r#"{"packageManager":"npm@10.8.0"}"#, vec![], "npm install"),
        (
            r#"{"packageManager":"pnpm@10.0.0"}"#,
            vec![],
            "pnpm install",
        ),
    ] {
        fs::write(f.repo.join("package.json"), package).unwrap();
        for lock in &locks {
            fs::write(f.repo.join(lock), "fixture lock").unwrap();
        }
        let draft = f.core.project_preparation_draft(&f.project.id).unwrap();
        assert_eq!(draft.config.commands, [command], "{package}: {locks:?}");
        if locks.is_empty() {
            assert!(draft.note.contains("may create one"));
        }
        for lock in locks {
            fs::remove_file(f.repo.join(lock)).unwrap();
        }
    }
    assert!(!f.repo.join(".shika").exists());
}

#[test]
fn setup_suggestions_skip_conflicts_unsupported_managers_and_unknown_toolchains() {
    let f = Fixture::new();
    fs::write(f.repo.join("Cargo.toml"), "[package]\nname = 'fixture'\n").unwrap();
    assert!(
        f.core
            .project_preparation_draft(&f.project.id)
            .unwrap()
            .config
            .commands
            .is_empty()
    );
    for (package, locks) in [
        ("{}", vec![]),
        ("{}", vec!["package-lock.json", "pnpm-lock.yaml"]),
        ("{}", vec!["package-lock.json", "yarn.lock"]),
        (
            r#"{"packageManager":"pnpm@10.0.0"}"#,
            vec!["package-lock.json"],
        ),
        (r#"{"packageManager":"npm@10.8.0"}"#, vec!["pnpm-lock.yaml"]),
        (
            r#"{"packageManager":"yarn@4.0.0"}"#,
            vec!["package-lock.json"],
        ),
        (r#"{"packageManager":"npm; touch ran.generated"}"#, vec![]),
        (r#"{"packageManager":42}"#, vec!["package-lock.json"]),
        ("{}", vec!["yarn.lock", "bun.lock"]),
        (r#"{"packageManager":"bun@1.1.0"}"#, vec!["yarn.lock"]),
        (r#"{"packageManager":"yarn@berry"}"#, vec!["yarn.lock"]),
        (
            r#"{"packageManager":"yarn@1.22.22"}"#,
            vec!["yarn.lock", ".yarnrc.yml"],
        ),
        (r#"{"packageManager":"deno@2.0.0"}"#, vec![]),
    ] {
        fs::write(f.repo.join("package.json"), package).unwrap();
        for lock in &locks {
            fs::write(f.repo.join(lock), "fixture").unwrap();
        }
        let draft = f.core.project_preparation_draft(&f.project.id).unwrap();
        assert!(draft.config.commands.is_empty(), "{package}: {locks:?}");
        assert!(draft.note.contains("manually"));
        for lock in locks {
            fs::remove_file(f.repo.join(lock)).unwrap();
        }
    }
    assert!(!f.repo.join("ran.generated").exists());
}

#[test]
fn setup_suggestions_skip_invalid_unbounded_and_symlinked_package_metadata() {
    let f = Fixture::new();
    fs::write(f.repo.join("package-lock.json"), "{}").unwrap();
    for package in ["{", "[]", &" ".repeat(65537)] {
        fs::write(f.repo.join("package.json"), package).unwrap();
        let draft = f.core.project_preparation_draft(&f.project.id).unwrap();
        assert!(draft.config.commands.is_empty());
        assert!(draft.note.contains("safely"));
    }
    fs::remove_file(f.repo.join("package.json")).unwrap();
    let outside = f.root.join("outside.json");
    fs::write(&outside, r#"{"packageManager":"npm@10.8.0"}"#).unwrap();
    symlink(&outside, f.repo.join("package.json")).unwrap();
    assert!(
        f.core
            .project_preparation_draft(&f.project.id)
            .unwrap()
            .config
            .commands
            .is_empty()
    );
    fs::remove_file(f.repo.join("package.json")).unwrap();
    fs::create_dir(f.repo.join("package.json")).unwrap();
    assert!(
        f.core
            .project_preparation_draft(&f.project.id)
            .unwrap()
            .config
            .commands
            .is_empty()
    );
    fs::remove_dir(f.repo.join("package.json")).unwrap();
    fs::write(
        f.repo.join("package.json"),
        r#"{"packageManager":"npm@10.8.0"}"#,
    )
    .unwrap();
    fs::remove_file(f.repo.join("package-lock.json")).unwrap();
    symlink(&outside, f.repo.join("package-lock.json")).unwrap();
    // Unsafe lock metadata must not degrade into an unfrozen installer.
    assert!(
        f.core
            .project_preparation_draft(&f.project.id)
            .unwrap()
            .config
            .commands
            .is_empty()
    );
    assert!(!f.repo.join(".shika").exists());
}

#[test]
fn setup_suggestions_use_unambiguous_yarn_and_bun_metadata() {
    let f = Fixture::new();
    for (package, files, command) in [
        ("{}", vec!["yarn.lock"], "yarn install --frozen-lockfile"),
        (
            "{}",
            vec!["yarn.lock", ".yarnrc.yml"],
            "yarn install --immutable",
        ),
        (
            r#"{"packageManager":"yarn@4.5.0+sha512.example"}"#,
            vec!["yarn.lock"],
            "yarn install --immutable",
        ),
        (
            r#"{"packageManager":"yarn@1.22.22"}"#,
            vec!["yarn.lock"],
            "yarn install --frozen-lockfile",
        ),
        (
            r#"{"packageManager":"yarn"}"#,
            vec!["yarn.lock", ".yarnrc.yml"],
            "yarn install --immutable",
        ),
        (r#"{"packageManager":"yarn@3.6.0"}"#, vec![], "yarn install"),
        ("{}", vec!["bun.lock"], "bun install --frozen-lockfile"),
        ("{}", vec!["bun.lockb"], "bun install --frozen-lockfile"),
        (
            r#"{"packageManager":"bun@1.1.0"}"#,
            vec!["bun.lock", "bun.lockb"],
            "bun install --frozen-lockfile",
        ),
        (r#"{"packageManager":"bun@1.1.0"}"#, vec![], "bun install"),
    ] {
        fs::write(f.repo.join("package.json"), package).unwrap();
        for file in &files {
            fs::write(f.repo.join(file), "fixture").unwrap();
        }
        let draft = f.core.project_preparation_draft(&f.project.id).unwrap();
        assert_eq!(draft.config.commands, [command], "{package}: {files:?}");
        if !files.iter().any(|file| file.contains("lock")) {
            assert!(draft.note.contains("may create one"));
        }
        for file in files {
            fs::remove_file(f.repo.join(file)).unwrap();
        }
    }
    // Yarn's configuration only decides the version, never the manager.
    fs::write(f.repo.join("package.json"), "{}").unwrap();
    fs::write(f.repo.join("package-lock.json"), "{}").unwrap();
    fs::write(f.repo.join(".yarnrc.yml"), "nodeLinker: node-modules\n").unwrap();
    let draft = f.core.project_preparation_draft(&f.project.id).unwrap();
    assert_eq!(draft.config.commands, ["npm ci"]);
    // An unsafe Yarn configuration must not degrade into the Yarn 1 flag.
    fs::remove_file(f.repo.join("package-lock.json")).unwrap();
    fs::remove_file(f.repo.join(".yarnrc.yml")).unwrap();
    fs::write(f.repo.join("yarn.lock"), "fixture").unwrap();
    symlink(f.root.join("outside.yml"), f.repo.join(".yarnrc.yml")).unwrap();
    let draft = f.core.project_preparation_draft(&f.project.id).unwrap();
    assert!(draft.config.commands.is_empty());
    assert!(draft.note.contains("safely"));
    assert!(!f.repo.join(".shika").exists());
}

#[test]
fn setup_suggestions_add_cocoapods_after_the_javascript_install() {
    let f = Fixture::new();
    let commands = |f: &Fixture| {
        f.core
            .project_preparation_draft(&f.project.id)
            .unwrap()
            .config
            .commands
    };
    fs::write(f.repo.join("Podfile"), "platform :ios, '15.0'\n").unwrap();
    assert_eq!(commands(&f), ["pod install"]);
    fs::write(
        f.repo.join("Gemfile"),
        "source 'https://rubygems.org'\ngem 'cocoapods', '>= 1.13'\n",
    )
    .unwrap();
    assert_eq!(commands(&f), ["bundle exec pod install"]);
    fs::remove_file(f.repo.join("Podfile")).unwrap();
    fs::create_dir(f.repo.join("ios")).unwrap();
    fs::write(f.repo.join("ios/Podfile"), "platform :ios, '15.0'\n").unwrap();
    assert_eq!(commands(&f), ["cd ios && bundle exec pod install"]);
    // Other gems, comments, and look-alike names do not mean Bundler.
    for gemfile in [
        "gem 'cocoapods-binary'\n",
        "# gem 'cocoapods'\n",
        "gem 'xcpretty'\n",
    ] {
        fs::write(f.repo.join("Gemfile"), gemfile).unwrap();
        assert_eq!(commands(&f), ["cd ios && pod install"], "{gemfile}");
    }
    fs::write(f.repo.join("Gemfile"), "gem(\"cocoapods\")\n").unwrap();
    fs::write(f.repo.join("package.json"), "{}").unwrap();
    fs::write(f.repo.join("yarn.lock"), "fixture").unwrap();
    assert_eq!(
        commands(&f),
        [
            "yarn install --frozen-lockfile",
            "cd ios && bundle exec pod install"
        ]
    );
    // Two Podfiles are ambiguous; the JavaScript install still stands.
    fs::write(f.repo.join("Podfile"), "platform :ios, '15.0'\n").unwrap();
    let draft = f.core.project_preparation_draft(&f.project.id).unwrap();
    assert_eq!(draft.config.commands, ["yarn install --frozen-lockfile"]);
    assert!(draft.note.contains("root and in ios/"));
    fs::remove_file(f.repo.join("Podfile")).unwrap();
    for (link, target) in [("Podfile", "ios/Podfile"), ("Gemfile", "ios/Podfile")] {
        let saved = fs::read(f.repo.join(link)).ok();
        let _ = fs::remove_file(f.repo.join(link));
        symlink(target, f.repo.join(link)).unwrap();
        let draft = f.core.project_preparation_draft(&f.project.id).unwrap();
        assert_eq!(draft.config.commands, ["yarn install --frozen-lockfile"]);
        assert!(
            draft
                .note
                .contains("CocoaPods metadata could not be read safely")
        );
        fs::remove_file(f.repo.join(link)).unwrap();
        if let Some(saved) = saved {
            fs::write(f.repo.join(link), saved).unwrap();
        }
    }
    fs::remove_dir_all(f.repo.join("ios")).unwrap();
    symlink(f.root.join("outside"), f.repo.join("ios")).unwrap();
    assert_eq!(commands(&f), ["yarn install --frozen-lockfile"]);
    assert!(!f.repo.join(".shika").exists());
}

#[test]
fn setup_suggestions_copy_only_literal_ignored_worktreeinclude_files() {
    let f = Fixture::new();
    fs::write(f.repo.join(".env.local"), "ignored").unwrap();
    fs::create_dir(f.repo.join("local")).unwrap();
    fs::write(f.repo.join("local/secret.json"), "ignored").unwrap();
    fs::write(f.repo.join("config.generated"), "ignored").unwrap();
    fs::write(f.repo.join("untracked.txt"), "not ignored").unwrap();
    symlink("../tracked.txt", f.repo.join("local/link.json")).unwrap();
    fs::write(
        f.repo.join(".worktreeinclude"),
        "# Local files for new worktrees\n\
         \n\
         .env.local\n\
         /local/secret.json\n\
         local/./secret.json\n\
         config.generated\n\
         tracked.txt\n\
         untracked.txt\n\
         missing.generated\n\
         local/link.json\n",
    )
    .unwrap();
    let draft = f.core.project_preparation_draft(&f.project.id).unwrap();
    assert_eq!(
        draft.config.copy_files,
        [".env.local", "local/secret.json", "config.generated"]
    );
    assert!(draft.note.contains("left out"), "{}", draft.note);
    assert!(!draft.note.contains("literal"), "{}", draft.note);
    // The suggestion passes the same validation Save applies.
    f.core
        .save_project_preparation(&f.project.id, None, Some(&draft.config))
        .unwrap();
    fs::remove_dir_all(f.repo.join(".shika")).unwrap();

    for pattern in [
        "*.generated",
        "!config.generated",
        "local/",
        "../outside",
        ".git/config",
    ] {
        fs::write(f.repo.join(".worktreeinclude"), format!("{pattern}\n")).unwrap();
        let draft = f.core.project_preparation_draft(&f.project.id).unwrap();
        assert_eq!(draft.config.copy_files, [".env.local"], "{pattern}");
        assert!(draft.note.contains("literal"), "{pattern}: {}", draft.note);
    }
    fs::remove_file(f.repo.join(".worktreeinclude")).unwrap();
    symlink("local/secret.json", f.repo.join(".worktreeinclude")).unwrap();
    let draft = f.core.project_preparation_draft(&f.project.id).unwrap();
    assert_eq!(draft.config.copy_files, [".env.local"]);
    assert!(
        draft
            .note
            .contains(".worktreeinclude could not be read safely")
    );
    assert!(!f.repo.join(".shika").exists());
}
