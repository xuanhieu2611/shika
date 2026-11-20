//! Unsaved setup suggestions for Settings and first-New review. Reads a bounded
//! set of root metadata in the main checkout; never saves, approves, runs a
//! package manager, or reads dotenv contents.

use std::collections::HashSet;
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::Path;
use std::process::Stdio;

use super::{
    CONFIG_LIMIT, PreparationConfig, PreparationDraft, default_timeout, directory, io_failure,
    load, relative_file, validate_path,
};
use crate::error::Result;
use crate::worktree::git_cmd;

const WORKTREE_INCLUDE: &str = ".worktreeinclude";
const COPY_LIMIT: usize = 128;
const NO_LOCK: &str = "No lockfile found. Installation may create one.";

type Suggestion = (Option<&'static str>, Option<&'static str>);

/// Existing configuration wins; detection never saves, approves, or starts
/// preparation. Fixed commands, never repository scripts.
pub(crate) fn draft(repo: &Path, git: &Path, path_env: &str) -> Result<PreparationDraft> {
    if let Some(existing) = load(repo)? {
        return Ok(PreparationDraft {
            config: existing.clone(),
            existing: Some(existing),
            note: "Saved configuration. Disable setup removes this file for future agents.".into(),
        });
    }
    let root = directory(repo).map_err(io_failure)?;
    let mut notes = Vec::new();
    let mut candidates: Vec<String> = [".env", ".env.local"]
        .into_iter()
        .filter(|name| matches!(optional_regular(&root, name), Ok(Some(_))))
        .map(String::from)
        .collect();
    let dotenv = candidates.len();
    let include = worktree_include(&root, &mut candidates);
    notes.extend(include.note);
    let ignored = ignored(git, path_env, repo, &candidates)?;
    let not_ignored = candidates[dotenv..]
        .iter()
        .filter(|path| !ignored.contains(*path))
        .count();
    let copy_files: Vec<String> = candidates
        .into_iter()
        .filter(|path| ignored.contains(path))
        .collect();
    if include.left_out + not_ignored > 0 {
        notes.push(
            "Some .worktreeinclude paths are missing, not plain files, or not ignored by Git, so they were left out.",
        );
    }
    let mut commands = Vec::new();
    // JavaScript first: React Native pods read node_modules.
    for (command, note) in [suggest_installer(&root), suggest_pods(&root)] {
        commands.extend(command.map(String::from));
        notes.extend(note);
    }
    let found = !copy_files.is_empty() || !commands.is_empty();
    let mut note = if found {
        "Suggested from this checkout, not saved. Review before saving.".to_string()
    } else {
        "Not configured. No safe defaults found; add what this project needs.".to_string()
    };
    for detail in notes {
        note.push(' ');
        note.push_str(detail);
    }
    Ok(PreparationDraft {
        existing: None,
        config: PreparationConfig {
            commands,
            copy_files,
            timeout_seconds: default_timeout(),
        },
        note,
    })
}

fn optional_regular(root: &File, name: &str) -> io::Result<Option<File>> {
    let file = match relative_file(root, Path::new(name), libc::O_RDONLY, 0) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a regular file",
        ));
    }
    Ok(Some(file))
}

/// A regular root file of at most 64 KiB, or `Err` when it is unsafe to read.
fn bounded(root: &File, name: &str) -> std::result::Result<Option<Vec<u8>>, ()> {
    let Some(file) = optional_regular(root, name).map_err(|_| ())? else {
        return Ok(None);
    };
    let mut bytes = Vec::new();
    if file.take(CONFIG_LIMIT + 1).read_to_end(&mut bytes).is_err()
        || bytes.len() as u64 > CONFIG_LIMIT
    {
        return Err(());
    }
    Ok(Some(bytes))
}

/// The subset of `paths` that Git ignores in the main checkout, in one
/// process. Tracked files are never reported, whatever the patterns say.
fn ignored(git: &Path, path_env: &str, repo: &Path, paths: &[String]) -> Result<HashSet<String>> {
    if paths.is_empty() {
        return Ok(HashSet::new());
    }
    let mut child = git_cmd(git, path_env, repo)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .args(["check-ignore", "-z", "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(io_failure)?;
    let mut input = Vec::new();
    for path in paths {
        input.extend_from_slice(path.as_bytes());
        input.push(0);
    }
    // Write from another thread: Git may fill stdout before reading all input.
    let mut stdin = child.stdin.take();
    let writer = std::thread::spawn(move || match stdin.as_mut() {
        Some(stdin) => stdin.write_all(&input).is_ok(),
        None => false,
    });
    let output = child.wait_with_output().map_err(io_failure)?;
    let written = writer.join().unwrap_or(false);
    // Exit 1 means nothing is ignored; anything else but 0 is a Git failure,
    // which suggests nothing rather than guessing.
    if !written || output.status.code() != Some(0) {
        return Ok(HashSet::new());
    }
    Ok(output
        .stdout
        .split(|b| *b == 0)
        .filter(|path| !path.is_empty())
        .map(|path| String::from_utf8_lossy(path).into_owned())
        .collect())
}

#[derive(Default)]
struct Include {
    /// Literal paths that were missing or not regular files.
    left_out: usize,
    note: Option<&'static str>,
}

/// Literal root-relative paths only: a glob would need a recursive walk. A
/// pattern without a slash is read as a root path, not one at any depth.
fn worktree_include(root: &File, candidates: &mut Vec<String>) -> Include {
    let mut include = Include::default();
    let bytes = match bounded(root, WORKTREE_INCLUDE) {
        Ok(Some(bytes)) => bytes,
        Ok(None) => return include,
        Err(()) => {
            include.note = Some(".worktreeinclude could not be read safely.");
            return include;
        }
    };
    let Ok(text) = String::from_utf8(bytes) else {
        include.note = Some(".worktreeinclude could not be read safely.");
        return include;
    };
    let mut unsupported = false;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let path = line.strip_prefix('/').unwrap_or(line);
        if line.starts_with('!')
            || line.ends_with('/')
            || path.contains(['*', '?', '[', '\\'])
            || validate_path(Path::new(path)).is_err()
        {
            unsupported = true;
            continue;
        }
        // Normalize `a/./b` and `a//b` so duplicates and Git see one path.
        let path = Path::new(path)
            .components()
            .map(|part| part.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        if candidates.contains(&path) {
            continue;
        }
        if candidates.len() >= COPY_LIMIT {
            include.note = Some(".worktreeinclude lists more files than setup can copy.");
            break;
        }
        match optional_regular(root, &path) {
            Ok(Some(_)) => candidates.push(path),
            _ => include.left_out += 1,
        }
    }
    if unsupported && include.note.is_none() {
        include.note = Some(
            "Only literal file paths in .worktreeinclude are suggested. Add globs and directories manually.",
        );
    }
    include
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Manager {
    Npm,
    Pnpm,
    Yarn,
    Bun,
}

/// `packageManager` as `name` or `name@version`. `None` is unsupported.
fn declared(value: &serde_json::Value) -> Option<(Manager, Option<&str>)> {
    let value = value.as_str()?;
    let (name, version) = match value.split_once('@') {
        Some((name, version)) => (name, Some(version)),
        None => (value, None),
    };
    let manager = match name {
        "npm" => Manager::Npm,
        "pnpm" => Manager::Pnpm,
        "yarn" => Manager::Yarn,
        "bun" => Manager::Bun,
        _ => return None,
    };
    Some((manager, version))
}

fn suggest_installer(root: &File) -> Suggestion {
    let unsafe_metadata = (
        None,
        Some("Package metadata could not be read safely. Add setup commands manually."),
    );
    let bytes = match bounded(root, "package.json") {
        Ok(Some(bytes)) => bytes,
        Ok(None) => return (None, None),
        Err(()) => return unsafe_metadata,
    };
    let package = match serde_json::from_slice::<serde_json::Value>(&bytes) {
        Ok(package) if package.is_object() => package,
        _ => return unsafe_metadata,
    };
    let mut locks = HashSet::new();
    for (name, manager) in [
        ("package-lock.json", Manager::Npm),
        ("npm-shrinkwrap.json", Manager::Npm),
        ("pnpm-lock.yaml", Manager::Pnpm),
        ("yarn.lock", Manager::Yarn),
        ("bun.lock", Manager::Bun),
        ("bun.lockb", Manager::Bun),
    ] {
        match optional_regular(root, name) {
            Ok(Some(_)) => {
                locks.insert(manager);
            }
            Ok(None) => {}
            Err(_) => return unsafe_metadata,
        }
    }
    let ambiguous = (
        None,
        Some("Package-manager metadata conflicts. Add setup commands manually."),
    );
    if locks.len() > 1 {
        return ambiguous;
    }
    let locked = locks.iter().copied().next();
    let unsupported = (
        None,
        Some("This package manager has no suggested installer. Add commands manually."),
    );
    let declared = match package.get("packageManager") {
        Some(value) => match declared(value) {
            Some(declared) => Some(declared),
            None => return unsupported,
        },
        None => None,
    };
    if let (Some((manager, _)), Some(lock)) = (declared, locked)
        && manager != lock
    {
        return ambiguous;
    }
    let Some(manager) = declared.map(|(manager, _)| manager).or(locked) else {
        return (
            None,
            Some("No lockfile or packageManager found. Add setup commands manually."),
        );
    };
    let (frozen, unfrozen) = match manager {
        Manager::Npm => ("npm ci", "npm install"),
        Manager::Pnpm => ("pnpm install --frozen-lockfile", "pnpm install"),
        Manager::Bun => ("bun install --frozen-lockfile", "bun install"),
        Manager::Yarn => {
            let berry_config = match optional_regular(root, ".yarnrc.yml") {
                Ok(file) => file.is_some(),
                Err(_) => return unsafe_metadata,
            };
            let major = declared.and_then(|(_, version)| version).map(|version| {
                version
                    .split(['.', '+', '-'])
                    .next()
                    .and_then(|major| major.parse::<u64>().ok())
            });
            // `.yarnrc.yml` is Yarn 2+ configuration; Yarn 1 reads `.yarnrc`.
            let berry = match major {
                Some(None) => return unsupported,
                Some(Some(major)) if major >= 2 => true,
                Some(Some(_)) if berry_config => return ambiguous,
                Some(Some(_)) => false,
                None => berry_config,
            };
            if berry {
                ("yarn install --immutable", "yarn install")
            } else {
                ("yarn install --frozen-lockfile", "yarn install")
            }
        }
    };
    if locked.is_some() {
        (Some(frozen), None)
    } else {
        (Some(unfrozen), Some(NO_LOCK))
    }
}

/// A root or `ios/` Podfile, through Bundler when the root Gemfile names
/// CocoaPods. Bundler finds the root Gemfile from `ios/` on its own.
fn suggest_pods(root: &File) -> Suggestion {
    let unsafe_metadata = (
        None,
        Some("CocoaPods metadata could not be read safely. Add pod install manually."),
    );
    let mut found = [false; 2];
    for (found, name) in found.iter_mut().zip(["Podfile", "ios/Podfile"]) {
        match optional_regular(root, name) {
            Ok(file) => *found = file.is_some(),
            Err(_) => return unsafe_metadata,
        }
    }
    let bundled = match bounded(root, "Gemfile") {
        Ok(gemfile) => {
            gemfile.is_some_and(|bytes| names_cocoapods(&String::from_utf8_lossy(&bytes)))
        }
        Err(()) => return unsafe_metadata,
    };
    let command = match (found, bundled) {
        ([true, true], _) => {
            return (
                None,
                Some("Found a Podfile at the root and in ios/. Add pod install manually."),
            );
        }
        ([false, false], _) => return (None, None),
        ([true, false], false) => "pod install",
        ([true, false], true) => "bundle exec pod install",
        ([false, true], false) => "cd ios && pod install",
        ([false, true], true) => "cd ios && bundle exec pod install",
    };
    (Some(command), None)
}

/// A `gem 'cocoapods'` line, quoted either way, with or without parentheses.
fn names_cocoapods(gemfile: &str) -> bool {
    gemfile.lines().any(|line| {
        line.trim_start()
            .strip_prefix("gem")
            .and_then(|rest| rest.strip_prefix(|c: char| c == '(' || c.is_whitespace()))
            .is_some_and(|rest| {
                let rest = rest.trim_start();
                rest.starts_with("'cocoapods'") || rest.starts_with("\"cocoapods\"")
            })
    })
}
