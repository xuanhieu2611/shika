//! Opt-in copy-on-write clone of the main checkout's `node_modules` into a
//! new task worktree. Any reason not to clone skips it, and the setup
//! commands install as usual; only cancellation and timeout fail preparation.

use std::ffi::CString;
use std::fs::{self, File};
use std::io::{self, Read};
use std::os::fd::AsRawFd;
use std::path::{Component, Path, PathBuf};
use std::time::Instant;

use super::{PreparationControl, check_deadline, relative_file, suggest::ignored, validate_path};
use crate::error::Result;
use crate::worktree::git_cmd;

/// The manifests and lockfiles that decide what an install puts in
/// `node_modules`. Every tracked one must match the main checkout.
const PACKAGE_FILES: [&str; 8] = [
    "package.json",
    "package-lock.json",
    "npm-shrinkwrap.json",
    "pnpm-lock.yaml",
    "pnpm-workspace.yaml",
    "yarn.lock",
    "bun.lock",
    "bun.lockb",
];
/// At most this many `node_modules` folders are cloned; the rest install.
const FOLDER_LIMIT: usize = 256;
/// Tool caches that can hold absolute paths into the main checkout.
const CACHES: [&str; 2] = [".cache", ".vite"];
/// `CLONE_NOFOLLOW` from `<sys/clonefile.h>`; the libc crate lacks it.
const CLONE_NOFOLLOW: u32 = 0x0001;

/// What was cloned and why anything was not, for the setup output.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Cloned {
    pub(crate) folders: Vec<String>,
    pub(crate) skipped: Vec<String>,
}

impl Cloned {
    fn skip(reason: impl Into<String>) -> Self {
        Self {
            folders: Vec::new(),
            skipped: vec![reason.into()],
        }
    }

    pub(crate) fn summary(&self) -> String {
        let mut text = String::new();
        if !self.folders.is_empty() {
            text.push_str(&format!(
                "\r\nCloned {} from the main checkout.\r\n",
                self.folders.join(", ")
            ));
        }
        for reason in &self.skipped {
            text.push_str(&format!("\r\nDid not clone node_modules: {reason}.\r\n"));
        }
        text
    }
}

/// Clone each package's `node_modules` when the main checkout has one, every
/// tracked package file is byte-identical, and the task's target is missing
/// and ignored. A clone whose links lead into the main checkout never lands.
#[allow(clippy::too_many_arguments)]
pub(crate) fn dependencies(
    source: &File,
    destination: &File,
    repo: &Path,
    worktree: &Path,
    git: &Path,
    path_env: &str,
    control: &PreparationControl,
    deadline: Instant,
) -> Result<Cloned> {
    let Some(files) = package_files(git, path_env, worktree) else {
        return Ok(Cloned::skip("Git could not list the package files"));
    };
    for path in &files {
        check_deadline(control, deadline)?;
        if !identical(source, destination, Path::new(path), control, deadline)? {
            return Ok(Cloned::skip(format!(
                "{path} differs from the main checkout"
            )));
        }
    }
    if !files.iter().any(|path| file_name(path) == "package.json") {
        return Ok(Cloned::skip("the task has no tracked package.json"));
    }
    let mut folders: Vec<String> = files
        .iter()
        .filter(|path| file_name(path) == "package.json")
        .map(|path| match path.rsplit_once('/') {
            Some((dir, _)) => format!("{dir}/node_modules"),
            None => "node_modules".into(),
        })
        .collect();
    // Root first, then by path, so a cap keeps the folders nearest the top.
    folders.sort_by_key(|folder| (folder.matches('/').count(), folder.clone()));
    folders.dedup();
    let mut cloned = Cloned::default();
    let candidates: Vec<String> = folders
        .into_iter()
        .filter(|folder| source_folder(source, folder).is_some())
        .take(FOLDER_LIMIT)
        .collect();
    if candidates.is_empty() {
        cloned
            .skipped
            .push("the main checkout has no node_modules".into());
        return Ok(cloned);
    }
    // Trailing slashes let Git match directory patterns to missing folders.
    let queries: Vec<String> = candidates.iter().map(|f| format!("{f}/")).collect();
    let ignored = ignored(git, path_env, worktree, &queries)?;
    let Some(staging) = Staging::new(git, path_env, worktree) else {
        cloned
            .skipped
            .push("Git could not find the task's admin folder".into());
        return Ok(cloned);
    };
    let roots = Roots::new(repo, worktree);
    for (index, (folder, query)) in candidates.iter().zip(&queries).enumerate() {
        check_deadline(control, deadline)?;
        if !ignored.contains(query) {
            cloned
                .skipped
                .push(format!("{folder} is not ignored by Git in the task"));
            continue;
        }
        let target = Target {
            folder,
            staged: index.to_string(),
            task: worktree.join(folder),
        };
        match place(
            source,
            destination,
            &staging,
            &target,
            &roots,
            control,
            deadline,
        )? {
            Placed::Cloned => cloned.folders.push(folder.clone()),
            Placed::Skipped(reason) => cloned.skipped.push(reason),
            Placed::Stop(reason) => {
                // Another volume or one without clones fails every folder.
                cloned.skipped.push(reason.into());
                break;
            }
        }
    }
    staging.remove();
    Ok(cloned)
}

/// Clones wait in the task's Git admin folder until their links pass, so an
/// unchecked clone never sits in the task, even when quit or a crash stops
/// preparation mid-check. Git removes the folder with the worktree.
struct Staging {
    path: PathBuf,
    dir: File,
}

impl Staging {
    fn new(git: &Path, path_env: &str, worktree: &Path) -> Option<Self> {
        let output = git_cmd(git, path_env, worktree)
            .env("GIT_OPTIONAL_LOCKS", "0")
            .args(["rev-parse", "--absolute-git-dir"])
            .output()
            .ok()?;
        let admin = String::from_utf8(output.stdout).ok()?;
        let admin = Path::new(admin.trim());
        // A linked worktree's own folder, never the main checkout's .git.
        if !output.status.success() || admin.parent()?.file_name()? != "worktrees" {
            return None;
        }
        let path = admin.join("shika-clone");
        let _ = fs::remove_dir_all(&path);
        fs::create_dir(&path).ok()?;
        let dir = super::directory(&path).ok()?;
        Some(Self { path, dir })
    }

    fn remove(self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

struct Target<'a> {
    folder: &'a str,
    /// Its name in the staging folder.
    staged: String,
    /// Where it lands in the task, which relative links are judged against.
    task: PathBuf,
}

enum Placed {
    Cloned,
    Skipped(String),
    Stop(&'static str),
}

/// Clone one folder into staging, drop its caches, check its links, then
/// move it into the task without replacing anything there.
fn place(
    source: &File,
    destination: &File,
    staging: &Staging,
    target: &Target,
    roots: &Roots,
    control: &PreparationControl,
    deadline: Instant,
) -> Result<Placed> {
    let folder = target.folder;
    let skipped = |reason: String| Ok(Placed::Skipped(reason));
    let Some(parent) = task_parent(destination, folder) else {
        return skipped(format!("{folder} has no parent folder in the task"));
    };
    if exists(&parent, "node_modules") {
        return skipped(format!("{folder} already exists in the task"));
    }
    if let Err(e) = clone_into(source, folder, &staging.dir, &target.staged) {
        return match e.raw_os_error() {
            Some(libc::EXDEV) => Ok(Placed::Stop("the task is on another volume")),
            Some(libc::ENOTSUP) => Ok(Placed::Stop("this volume cannot clone files")),
            _ => skipped(format!("{folder} could not be cloned: {e}")),
        };
    }
    let staged = staging.path.join(&target.staged);
    let reason = match drop_caches(&staged) {
        Err(cache) => Some(format!("{folder}/{cache} could not be removed")),
        // Cancelled or out of time: the unchecked clone stays in staging,
        // outside the task, and goes with the worktree.
        Ok(()) => (!links_stay_out_of(&staged, &target.task, roots, control, deadline)?)
            .then(|| format!("{folder} links into the main checkout")),
    };
    if let Some(reason) = reason {
        let _ = fs::remove_dir_all(&staged);
        return skipped(reason);
    }
    let from = CString::new(target.staged.as_str()).unwrap();
    let to = CString::new("node_modules").unwrap();
    // SAFETY: both descriptors are open directories and both names are one
    // NUL-terminated component. RENAME_EXCL never replaces an existing entry.
    let moved = unsafe {
        libc::renameatx_np(
            staging.dir.as_raw_fd(),
            from.as_ptr(),
            parent.as_raw_fd(),
            to.as_ptr(),
            libc::RENAME_EXCL,
        )
    };
    if moved == 0 {
        return Ok(Placed::Cloned);
    }
    let e = io::Error::last_os_error();
    let _ = fs::remove_dir_all(&staged);
    match e.raw_os_error() {
        Some(libc::EXDEV) => Ok(Placed::Stop("the task is on another volume")),
        Some(libc::EEXIST) => skipped(format!("{folder} already exists in the task")),
        _ => skipped(format!("{folder} could not be moved into the task: {e}")),
    }
}

/// The task folder that will hold `folder`, opened without following links.
fn task_parent(destination: &File, folder: &str) -> Option<File> {
    match folder.rsplit_once('/') {
        Some((dir, _)) => relative_file(
            destination,
            Path::new(dir),
            libc::O_RDONLY | libc::O_DIRECTORY,
            0,
        )
        .ok(),
        None => destination.try_clone().ok(),
    }
}

/// Any entry by that name, a link included.
fn exists(dir: &File, name: &str) -> bool {
    let name = CString::new(name).unwrap();
    // SAFETY: stat is a C POD that fstatat fills; the fd and name are valid.
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    unsafe {
        libc::fstatat(
            dir.as_raw_fd(),
            name.as_ptr(),
            &mut stat,
            libc::AT_SYMLINK_NOFOLLOW,
        ) == 0
    }
}

/// Remove tool caches from a fresh clone; `Err` names the one that stayed.
fn drop_caches(clone: &Path) -> std::result::Result<(), &'static str> {
    for cache in CACHES {
        let path = clone.join(cache);
        if fs::symlink_metadata(&path).is_ok_and(|meta| meta.is_dir())
            && fs::remove_dir_all(&path).is_err()
        {
            return Err(cache);
        }
    }
    Ok(())
}

/// Tracked package files in the task, or `None` when Git fails. Paths under
/// `node_modules`, `.git`, or `.worktrees` are never considered.
fn package_files(git: &Path, path_env: &str, worktree: &Path) -> Option<Vec<String>> {
    let output = git_cmd(git, path_env, worktree)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .args(["ls-files", "-z", "--"])
        .args(PACKAGE_FILES.map(|name| format!(":(glob)**/{name}")))
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let mut files = Vec::new();
    for path in output.stdout.split(|b| *b == 0).filter(|p| !p.is_empty()) {
        let path = String::from_utf8(path.to_vec()).ok()?;
        if validate_path(Path::new(&path)).is_ok()
            && !path.split('/').any(|part| part == "node_modules")
        {
            files.push(path);
        }
    }
    Some(files)
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Both copies are regular files with the same bytes. Anything unreadable
/// counts as different, which skips the clone.
fn identical(
    source: &File,
    destination: &File,
    path: &Path,
    control: &PreparationControl,
    deadline: Instant,
) -> Result<bool> {
    let open = |root: &File| -> Option<(File, u64)> {
        let file = relative_file(root, path, libc::O_RDONLY, 0).ok()?;
        let meta = file.metadata().ok()?;
        meta.is_file().then_some((file, meta.len()))
    };
    let (Some((mut a, a_len)), Some((mut b, b_len))) = (open(source), open(destination)) else {
        return Ok(false);
    };
    if a_len != b_len {
        return Ok(false);
    }
    let (mut x, mut y) = (vec![0u8; 64 * 1024], vec![0u8; 64 * 1024]);
    loop {
        check_deadline(control, deadline)?;
        let Ok(n) = a.read(&mut x) else {
            return Ok(false);
        };
        if n == 0 {
            // Equal lengths, so b must be at its end too.
            return Ok(matches!(b.read(&mut y[..1]), Ok(0)));
        }
        if b.read_exact(&mut y[..n]).is_err() || x[..n] != y[..n] {
            return Ok(false);
        }
    }
}

/// The main checkout's folder as a directory, never through a link.
fn source_folder(source: &File, folder: &str) -> Option<File> {
    let file = relative_file(
        source,
        Path::new(folder),
        libc::O_RDONLY | libc::O_DIRECTORY,
        0,
    )
    .ok()?;
    file.metadata().ok()?.is_dir().then_some(file)
}

/// Clone the main checkout's folder as `name` in `into`. The clone is
/// atomic, so an error leaves nothing behind.
fn clone_into(source: &File, folder: &str, into: &File, name: &str) -> io::Result<()> {
    let Some(from) = source_folder(source, folder) else {
        return Err(io::Error::from(io::ErrorKind::NotFound));
    };
    let name = CString::new(name)?;
    // SAFETY: both descriptors are open directories and the name is one
    // NUL-terminated component; the source is an fd, so no link is followed.
    let result = unsafe {
        libc::fclonefileat(
            from.as_raw_fd(),
            into.as_raw_fd(),
            name.as_ptr(),
            CLONE_NOFOLLOW,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// The main checkout and the task, as given and as resolved, since a link
/// can name either spelling (`/tmp` and `/private/tmp`).
struct Roots {
    repo: Vec<PathBuf>,
    worktree: Vec<PathBuf>,
}

impl Roots {
    fn new(repo: &Path, worktree: &Path) -> Self {
        let both = |path: &Path| {
            let mut paths = vec![normalize(path)];
            paths.extend(path.canonicalize().ok());
            paths
        };
        Self {
            repo: both(repo),
            worktree: both(worktree),
        }
    }

    /// Inside the main checkout but not inside the task, which lives under it.
    fn escapes(&self, path: &Path) -> bool {
        self.repo.iter().any(|root| path.starts_with(root))
            && !self.worktree.iter().any(|root| path.starts_with(root))
    }
}

/// Every link in the staged clone, judged at its place in the task, resolves
/// outside the main checkout or inside the task: by its text, and by the real
/// path when its target already exists.
fn links_stay_out_of(
    staged: &Path,
    task: &Path,
    roots: &Roots,
    control: &PreparationControl,
    deadline: Instant,
) -> Result<bool> {
    let mut pending = vec![(staged.to_path_buf(), task.to_path_buf())];
    while let Some((dir, place)) = pending.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            return Ok(false);
        };
        for entry in entries {
            check_deadline(control, deadline)?;
            let Ok(entry) = entry else {
                return Ok(false);
            };
            let Ok(kind) = entry.file_type() else {
                return Ok(false);
            };
            if kind.is_dir() {
                pending.push((entry.path(), place.join(entry.file_name())));
            } else if kind.is_symlink() {
                let Ok(target) = fs::read_link(entry.path()) else {
                    return Ok(false);
                };
                let resolved = normalize(&place.join(target));
                if roots.escapes(&resolved)
                    || resolved
                        .canonicalize()
                        .is_ok_and(|real| roots.escapes(&real))
                {
                    return Ok(false);
                }
            }
        }
    }
    Ok(true)
}

/// Resolve `.` and `..` by text, without touching the file system.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::ffi::OsStrExt;

    #[test]
    fn links_are_judged_by_where_they_land_not_how_they_are_spelled() {
        let roots = Roots {
            repo: vec![PathBuf::from("/r")],
            worktree: vec![PathBuf::from("/r/.worktrees/t")],
        };
        let dir = Path::new("/r/.worktrees/t/node_modules/.bin");
        for (target, escapes) in [
            ("../pkg/cli.js", false),
            ("/r/.worktrees/t/packages/a", false),
            ("/usr/local/lib/node", false),
            ("/r/packages/a", true),
            ("../../../../packages/a", true),
            ("/r/.worktrees/t/../../src", true),
        ] {
            let resolved = normalize(&dir.join(target));
            assert_eq!(roots.escapes(&resolved), escapes, "{target}");
        }
        assert_eq!(
            normalize(Path::new("/a/./b/../c")).as_os_str().as_bytes(),
            b"/a/c"
        );
    }

    #[test]
    fn an_existing_target_is_never_replaced_or_merged() {
        let root = std::env::temp_dir().join(format!(
            "shika-clone-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(root.join("main/node_modules/pkg")).unwrap();
        fs::write(root.join("main/node_modules/pkg/index.js"), "main").unwrap();
        // Even an empty folder, which a plain rename would replace.
        fs::create_dir_all(root.join("task/node_modules")).unwrap();
        fs::create_dir_all(root.join("staging")).unwrap();
        let open = |name: &str| super::super::directory(&root.join(name)).unwrap();
        let (source, destination) = (open("main"), open("task"));
        let staging = Staging {
            path: root.join("staging"),
            dir: open("staging"),
        };
        let target = Target {
            folder: "node_modules",
            staged: "0".into(),
            task: root.join("task/node_modules"),
        };
        let roots = Roots::new(&root.join("main"), &root.join("task"));
        let control = PreparationControl::default();
        let deadline = Instant::now() + std::time::Duration::from_secs(10);
        let place = || {
            place(
                &source,
                &destination,
                &staging,
                &target,
                &roots,
                &control,
                deadline,
            )
        };
        assert!(matches!(place().unwrap(), Placed::Skipped(r) if r.contains("already exists")));
        assert_eq!(
            fs::read_dir(root.join("task/node_modules"))
                .unwrap()
                .count(),
            0
        );
        fs::remove_dir(root.join("task/node_modules")).unwrap();
        assert!(matches!(place().unwrap(), Placed::Cloned));
        assert_eq!(
            fs::read_to_string(root.join("task/node_modules/pkg/index.js")).unwrap(),
            "main"
        );
        // Nothing stays behind in staging once a clone lands.
        assert_eq!(fs::read_dir(root.join("staging")).unwrap().count(), 0);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn summary_names_clones_and_each_reason_to_skip() {
        assert_eq!(Cloned::default().summary(), "");
        let cloned = Cloned {
            folders: vec!["node_modules".into(), "apps/web/node_modules".into()],
            skipped: vec!["the task is on another volume".into()],
        };
        assert_eq!(
            cloned.summary(),
            "\r\nCloned node_modules, apps/web/node_modules from the main checkout.\r\n\
             \r\nDid not clone node_modules: the task is on another volume.\r\n"
        );
    }
}
