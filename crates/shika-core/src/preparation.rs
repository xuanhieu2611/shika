//! Opt-in worktree preparation. Config is reviewed before allocation; file
//! copying is rooted and never follows symlinks. Commands are trusted project
//! code, not a sandbox. They run in isolated process groups with no stdin.

use std::ffi::CString;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Component, Path};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::worktree::{GIT_REDIRECTS, git_cmd};

pub const CONFIG_PATH: &str = ".shika/worktrees.json";
const CONFIG_LIMIT: u64 = 64 * 1024;
const POLL: Duration = Duration::from_millis(20);

/// Literal paths, not globs or directories. Approval is for this configuration,
/// not a promise that project scripts or their dependencies remain unchanged.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PreparationConfig {
    #[serde(default, rename = "setup-worktree")]
    pub commands: Vec<String>,
    #[serde(default, rename = "copy-files")]
    pub copy_files: Vec<String>,
    #[serde(default = "default_timeout", rename = "timeout-seconds")]
    pub timeout_seconds: u64,
}

/// Unsaved Settings/first-New draft. Suggestions never become launch setup
/// until the author explicitly saves this configuration.
#[derive(Debug, Clone)]
pub struct PreparationDraft {
    pub existing: Option<PreparationConfig>,
    pub config: PreparationConfig,
    pub note: String,
}

fn default_timeout() -> u64 {
    600
}

impl PreparationConfig {
    fn validate(&self) -> Result<()> {
        if !(1..=3600).contains(&self.timeout_seconds) {
            return Err(failed("timeout-seconds must be between 1 and 3600"));
        }
        if self.commands.len() > 64 || self.copy_files.len() > 128 {
            return Err(failed(
                "At most 64 commands and 128 copied files are allowed",
            ));
        }
        for command in &self.commands {
            if command.trim().is_empty() || command.contains('\0') {
                return Err(failed("Setup commands must be nonempty and contain no NUL"));
            }
        }
        let mut paths = std::collections::HashSet::new();
        for path in &self.copy_files {
            validate_path(Path::new(path))?;
            if !paths.insert(Path::new(path)) {
                return Err(failed(format!("Duplicate copy-files path: {path}")));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub enum PreparationEvent {
    Stage(String),
    Output(Vec<u8>),
    /// Setup has finished; startup query replies may now be queued for the PTY.
    StartingAgent,
}

#[derive(Debug, Default)]
struct ControlState {
    cancelled: AtomicBool,
    preserve: AtomicBool,
    group: Mutex<Option<libc::pid_t>>,
}

#[derive(Debug, Clone, Default)]
pub struct PreparationControl(Arc<ControlState>);

impl PreparationControl {
    pub fn cancel(&self) {
        self.0.cancelled.store(true, Ordering::Release);
        self.stop_group();
    }

    /// Stop setup but retain its journaled tree, for quit/project removal.
    pub fn cancel_preserving_worktree(&self) {
        self.0.preserve.store(true, Ordering::Release);
        self.cancel();
    }

    pub(crate) fn preserves_worktree(&self) -> bool {
        self.0.preserve.load(Ordering::Acquire)
    }

    fn stop_group(&self) {
        if let Some(group) = self
            .0
            .group
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            // SAFETY: only the group registered by our own setup child is used;
            // taking it under the mutex prevents later signals to a reused pid.
            unsafe {
                libc::kill(-group, libc::SIGKILL);
            }
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.cancelled.load(Ordering::Acquire)
    }

    pub(crate) fn check(&self) -> Result<()> {
        if self.is_cancelled() {
            Err(Error::PreparationCancelled)
        } else {
            Ok(())
        }
    }
}

/// Limit heavyweight setup without holding Core's operation lock or blocking
/// unrelated card navigation, terminals, close, or discard.
#[derive(Default)]
pub(crate) struct PreparationLimiter {
    active: Mutex<usize>,
    changed: Condvar,
}

pub(crate) struct PreparationSlot<'a>(&'a PreparationLimiter);

impl PreparationLimiter {
    pub(crate) fn acquire(&self, control: &PreparationControl) -> Result<PreparationSlot<'_>> {
        let mut count = self.active.lock().unwrap_or_else(|e| e.into_inner());
        while *count >= 2 {
            control.check()?;
            count = self
                .changed
                .wait_timeout(count, POLL)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        control.check()?;
        *count += 1;
        Ok(PreparationSlot(self))
    }
}

impl Drop for PreparationSlot<'_> {
    fn drop(&mut self) {
        *self.0.active.lock().unwrap_or_else(|e| e.into_inner()) -= 1;
        self.0.changed.notify_one();
    }
}

pub(crate) fn load(repo: &Path) -> Result<Option<PreparationConfig>> {
    let root = directory(repo).map_err(|e| failed(format!("Could not read project: {e}")))?;
    load_at(&root, Path::new(CONFIG_PATH))
}

fn load_at(root: &File, path: &Path) -> Result<Option<PreparationConfig>> {
    let file = match relative_file(root, path, libc::O_RDONLY, 0) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(failed(format!("Could not read {CONFIG_PATH}: {e}"))),
    };
    if !file.metadata().map_err(io_failure)?.is_file() {
        return Err(failed(format!("{CONFIG_PATH} must be a regular file")));
    }
    let mut bytes = Vec::new();
    file.take(CONFIG_LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(io_failure)?;
    if bytes.len() as u64 > CONFIG_LIMIT {
        return Err(failed(format!("{CONFIG_PATH} exceeds 64 KiB")));
    }
    let config: PreparationConfig = serde_json::from_slice(&bytes)
        .map_err(|e| failed(format!("Invalid {CONFIG_PATH}: {e}")))?;
    config.validate()?;
    Ok(Some(config))
}

/// Read-only and bounded for Settings or eligible first-New review. Existing
/// configuration wins; detection never saves, approves, or starts preparation.
/// Fixed commands, never repository scripts; ignored dotenv contents are not read.
pub(crate) fn draft(repo: &Path, git: &Path, path_env: &str) -> Result<PreparationDraft> {
    if let Some(existing) = load(repo)? {
        return Ok(PreparationDraft {
            config: existing.clone(),
            existing: Some(existing),
            note: "Saved configuration. Disable setup removes this file for future agents.".into(),
        });
    }
    let root = directory(repo).map_err(io_failure)?;
    let mut config = PreparationConfig {
        commands: vec![],
        copy_files: vec![],
        timeout_seconds: default_timeout(),
    };
    for name in [".env", ".env.local"] {
        if matches!(optional_regular(&root, name), Ok(Some(_)))
            && git_cmd(git, path_env, repo)
                .env("GIT_OPTIONAL_LOCKS", "0")
                .args(["check-ignore", "--quiet", "--", name])
                .output()
                .map_err(io_failure)?
                .status
                .success()
        {
            config.copy_files.push(name.into());
        }
    }
    let (command, installer_note) = suggest_installer(&root);
    if let Some(command) = command {
        config.commands.push(command.into());
    }
    let found = !config.copy_files.is_empty() || !config.commands.is_empty();
    let mut note = if found {
        "Suggested from this checkout, not saved. Review before saving.".to_string()
    } else {
        "Not configured. No safe defaults found; add what this project needs.".to_string()
    };
    if let Some(detail) = installer_note {
        note.push(' ');
        note.push_str(detail);
    }
    Ok(PreparationDraft {
        existing: None,
        config,
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

fn suggest_installer(root: &File) -> (Option<&'static str>, Option<&'static str>) {
    let unsafe_metadata = (
        None,
        Some("Package metadata could not be read safely. Add setup commands manually."),
    );
    let Some(file) = (match optional_regular(root, "package.json") {
        Ok(file) => file,
        Err(_) => return unsafe_metadata,
    }) else {
        return (None, None);
    };
    let mut bytes = Vec::new();
    if file.take(CONFIG_LIMIT + 1).read_to_end(&mut bytes).is_err()
        || bytes.len() as u64 > CONFIG_LIMIT
    {
        return unsafe_metadata;
    }
    let package = match serde_json::from_slice::<serde_json::Value>(&bytes) {
        Ok(package) if package.is_object() => package,
        _ => return unsafe_metadata,
    };
    let mut locks = std::collections::HashSet::new();
    for (name, manager) in [
        ("package-lock.json", "npm"),
        ("npm-shrinkwrap.json", "npm"),
        ("pnpm-lock.yaml", "pnpm"),
        ("yarn.lock", "yarn"),
        ("bun.lock", "bun"),
        ("bun.lockb", "bun"),
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
    let declared = match package.get("packageManager") {
        Some(value) => match value
            .as_str()
            .map(|name| name.split('@').next().unwrap_or(""))
        {
            Some("npm") => Some("npm"),
            Some("pnpm") => Some("pnpm"),
            _ => {
                return (
                    None,
                    Some("This package manager has no suggested installer. Add commands manually."),
                );
            }
        },
        None => None,
    };
    if declared.is_some() && locked.is_some() && declared != locked {
        return ambiguous;
    }
    match (declared.or(locked), locked.is_some()) {
        (Some("npm"), true) => (Some("npm ci"), None),
        (Some("pnpm"), true) => (Some("pnpm install --frozen-lockfile"), None),
        (Some("npm"), false) => (
            Some("npm install"),
            Some("No lockfile found. Installation may create one."),
        ),
        (Some("pnpm"), false) => (
            Some("pnpm install"),
            Some("No lockfile found. Installation may create one."),
        ),
        _ => (
            None,
            Some("No npm or pnpm installer detected. Add setup commands manually."),
        ),
    }
}

/// Validate an explicit edit outside the operation lock. Checks metadata and
/// Git ignore rules, never secret contents or user setup commands.
pub(crate) fn validate_edit(
    repo: &Path,
    config: Option<&PreparationConfig>,
    git: &Path,
    path_env: &str,
) -> Result<Option<Vec<u8>>> {
    let bytes = if let Some(config) = config {
        config.validate()?;
        let bytes = serde_json::to_vec_pretty(config).map_err(|e| failed(e.to_string()))?;
        if bytes.len() as u64 + 1 > CONFIG_LIMIT {
            return Err(failed("Configuration exceeds 64 KiB"));
        }
        // Check paths and ignored status without reading secret contents.
        let root = directory(repo).map_err(io_failure)?;
        for path in &config.copy_files {
            let file = relative_file(&root, Path::new(path), libc::O_RDONLY, 0)
                .map_err(|e| failed(format!("Could not copy {path}: {e}")))?;
            if !file.metadata().map_err(io_failure)?.is_file() {
                return Err(failed(format!("{path} must be a regular file")));
            }
            if !git_cmd(git, path_env, repo)
                .args(["check-ignore", "--quiet", "--", path])
                .output()
                .map_err(io_failure)?
                .status
                .success()
            {
                return Err(failed(format!(
                    "{path} must be ignored by Git; tracked files are never copied"
                )));
            }
        }
        Some(bytes)
    } else {
        None
    };
    Ok(bytes)
}

/// Commit a validated explicit edit atomically. No execution or consent
/// changes. A stale editor must reload instead of replacing changed config.
pub(crate) fn save(
    repo: &Path,
    expected: Option<&PreparationConfig>,
    bytes: Option<Vec<u8>>,
) -> Result<()> {
    let root = directory(repo).map_err(io_failure)?;
    if load_at(&root, Path::new(CONFIG_PATH))?.as_ref() != expected {
        return Err(failed(
            "Worktree configuration changed. Cancel and reopen setup before saving.",
        ));
    }
    if bytes.is_none() && expected.is_none() {
        return Ok(());
    }
    let dir_name = CString::new(".shika").unwrap();
    if bytes.is_some() {
        // SAFETY: creates a directory under the opened project, never a link.
        let result = unsafe { libc::mkdirat(root.as_raw_fd(), dir_name.as_ptr(), 0o700) };
        if result != 0 && io::Error::last_os_error().kind() != io::ErrorKind::AlreadyExists {
            return Err(io_failure(io::Error::last_os_error()));
        }
    }
    let dir = relative_file(
        &root,
        Path::new(".shika"),
        libc::O_RDONLY | libc::O_DIRECTORY,
        0,
    )
    .map_err(io_failure)?;
    let name = CString::new("worktrees.json").unwrap();
    if load_at(&dir, Path::new("worktrees.json"))?.as_ref() != expected {
        return Err(failed(
            "Worktree configuration changed. Cancel and reopen setup before saving.",
        ));
    }
    let Some(mut bytes) = bytes else {
        // SAFETY: unlinks only the config entry in the opened directory.
        if unsafe { libc::unlinkat(dir.as_raw_fd(), name.as_ptr(), 0) } != 0 {
            return Err(io_failure(io::Error::last_os_error()));
        }
        return Ok(());
    };
    static SERIAL: AtomicU64 = AtomicU64::new(0);
    let temporary = format!(
        ".worktrees-{}-{}.tmp",
        std::process::id(),
        SERIAL.fetch_add(1, Ordering::Relaxed)
    );
    let temp_name = CString::new(temporary.as_str()).unwrap();
    let mut file = relative_file(
        &dir,
        Path::new(&temporary),
        libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
        0o600,
    )
    .map_err(io_failure)?;
    bytes.push(b'\n');
    let result = (|| {
        file.write_all(&bytes).map_err(io_failure)?;
        file.sync_all().map_err(io_failure)?;
        if load_at(&dir, Path::new("worktrees.json"))?.as_ref() != expected {
            return Err(failed(
                "Worktree configuration changed. Cancel and reopen setup before saving.",
            ));
        }
        // SAFETY: atomic replacement within the opened directory; no following
        // of a destination link and no partial JSON visible to launches.
        if unsafe {
            libc::renameat(
                dir.as_raw_fd(),
                temp_name.as_ptr(),
                dir.as_raw_fd(),
                name.as_ptr(),
            )
        } != 0
        {
            return Err(io_failure(io::Error::last_os_error()));
        }
        Ok(())
    })();
    if result.is_err() {
        // SAFETY: cleans up only the temporary entry created above.
        unsafe {
            libc::unlinkat(dir.as_raw_fd(), temp_name.as_ptr(), 0);
        }
    }
    result
}

pub(crate) fn prepare(
    config: &PreparationConfig,
    repo: &Path,
    worktree: &Path,
    git: &Path,
    path_env: &str,
    control: &PreparationControl,
    report: &mut impl FnMut(PreparationEvent),
) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(config.timeout_seconds);
    let source = directory(repo).map_err(io_failure)?;
    let destination = directory(worktree).map_err(io_failure)?;
    for path in &config.copy_files {
        check_deadline(control, deadline)?;
        report(PreparationEvent::Stage(format!("Copying {path}")));
        let ignored = git_cmd(git, path_env, repo)
            .args(["check-ignore", "--quiet", "--", path])
            .output()
            .map_err(io_failure)?;
        let ignored_in_task = git_cmd(git, path_env, worktree)
            .args(["check-ignore", "--quiet", "--", path])
            .output()
            .map_err(io_failure)?;
        if !ignored.status.success() || !ignored_in_task.status.success() {
            return Err(failed(format!(
                "{path} must be ignored by Git in both the project and task; tracked files are never copied"
            )));
        }
        copy_file(&source, &destination, Path::new(path), control, deadline)?;
    }
    for (i, command) in config.commands.iter().enumerate() {
        check_deadline(control, deadline)?;
        report(PreparationEvent::Stage(format!(
            "Running setup {}/{}",
            i + 1,
            config.commands.len()
        )));
        // The command itself is already displayed in the approval dialog.
        report(PreparationEvent::Output(
            format!("\r\n$ {command}\r\n").into_bytes(),
        ));
        run_command(command, repo, worktree, path_env, control, deadline, report)?;
    }
    check_deadline(control, deadline)
}

fn validate_path(path: &Path) -> Result<()> {
    if path.as_os_str().is_empty()
        || path.as_os_str().as_bytes().contains(&0)
        || path.components().any(|c| match c {
            Component::Normal(part) => matches!(part.to_str(), Some(".git" | ".worktrees")),
            _ => true,
        })
        || path
            .as_os_str()
            .as_bytes()
            .iter()
            .any(|b| b.is_ascii_control())
    {
        return Err(failed(format!(
            "Invalid copy-files path: {}",
            path.display()
        )));
    }
    Ok(())
}

fn directory(path: &Path) -> io::Result<File> {
    let path = CString::new(path.as_os_str().as_bytes())?;
    // SAFETY: the path is NUL terminated; a successful fd is owned below.
    owned(unsafe {
        libc::open(
            path.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    })
}

fn owned(fd: libc::c_int) -> io::Result<File> {
    if fd < 0 {
        Err(io::Error::last_os_error())
    } else {
        // SAFETY: open/openat returned a new owned descriptor.
        Ok(unsafe { File::from_raw_fd(fd) })
    }
}

fn relative_file(
    root: &File,
    path: &Path,
    flags: libc::c_int,
    mode: libc::mode_t,
) -> io::Result<File> {
    // mode_t is u16 on macOS; variadic openat requires integer promotion.
    let mode = libc::c_uint::from(mode);
    let parts: Vec<_> = path.components().collect();
    let mut parent = root.try_clone()?;
    for (i, part) in parts.iter().enumerate() {
        let Component::Normal(part) = part else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "not a relative file path",
            ));
        };
        let part = CString::new(part.as_bytes())?;
        let last = i + 1 == parts.len();
        let open_flags = libc::O_NOFOLLOW
            | libc::O_CLOEXEC
            | if last {
                flags | libc::O_NONBLOCK
            } else {
                libc::O_RDONLY | libc::O_DIRECTORY
            };
        // SAFETY: fd and NUL-terminated name are valid; result is owned.
        let mut opened =
            owned(unsafe { libc::openat(parent.as_raw_fd(), part.as_ptr(), open_flags, mode) });
        if !last
            && flags & libc::O_CREAT != 0
            && opened
                .as_ref()
                .is_err_and(|e| e.kind() == io::ErrorKind::NotFound)
        {
            // SAFETY: this only creates a directory relative to the open root.
            let created = unsafe { libc::mkdirat(parent.as_raw_fd(), part.as_ptr(), 0o700) };
            if created != 0 && io::Error::last_os_error().kind() != io::ErrorKind::AlreadyExists {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: same rooted open; no symlink following on races.
            opened =
                owned(unsafe { libc::openat(parent.as_raw_fd(), part.as_ptr(), open_flags, mode) });
        }
        parent = opened?;
    }
    Ok(parent)
}

fn copy_file(
    source: &File,
    destination: &File,
    path: &Path,
    control: &PreparationControl,
    deadline: Instant,
) -> Result<()> {
    let mut input = relative_file(source, path, libc::O_RDONLY, 0)
        .map_err(|e| failed(format!("Could not copy {}: {e}", path.display())))?;
    let meta = input.metadata().map_err(io_failure)?;
    if !meta.is_file() {
        return Err(failed(format!(
            "{} must be a regular file, not a directory or link",
            path.display()
        )));
    }
    // Never overwrite tracked content, an earlier copy, or a symlink.
    let mut output = relative_file(
        destination,
        path,
        libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
        0o600,
    )
    .map_err(|e| {
        failed(format!(
            "Could not create {} without overwriting: {e}",
            path.display()
        ))
    })?;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        check_deadline(control, deadline)?;
        let n = input.read(&mut buffer).map_err(io_failure)?;
        if n == 0 {
            break;
        }
        output.write_all(&buffer[..n]).map_err(io_failure)?;
    }
    output
        .set_permissions(fs::Permissions::from_mode(
            meta.permissions().mode() & 0o777,
        ))
        .map_err(io_failure)?;
    Ok(())
}

fn check_deadline(control: &PreparationControl, deadline: Instant) -> Result<()> {
    control.check()?;
    if Instant::now() >= deadline {
        Err(failed("Worktree setup timed out"))
    } else {
        Ok(())
    }
}

fn nonblocking(file: &impl AsRawFd) -> io::Result<()> {
    // SAFETY: fd belongs to a live pipe; fcntl does not take ownership.
    let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
    if flags < 0
        || unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0
    {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn drain(pipe: &mut impl Read, report: &mut impl FnMut(PreparationEvent)) -> Result<()> {
    let mut bytes = [0u8; 8192];
    // Bounded per poll so flooding output cannot starve cancellation/timeout.
    for _ in 0..8 {
        match pipe.read(&mut bytes) {
            Ok(0) => break,
            Ok(n) => report(PreparationEvent::Output(bytes[..n].to_vec())),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(io_failure(e)),
        }
    }
    Ok(())
}

fn run_command(
    command: &str,
    repo: &Path,
    worktree: &Path,
    path_env: &str,
    control: &PreparationControl,
    deadline: Instant,
    report: &mut impl FnMut(PreparationEvent),
) -> Result<()> {
    let mut cmd = Command::new("/bin/sh");
    cmd.args(["-c", command])
        .current_dir(worktree)
        .env("PATH", path_env)
        .env("PWD", worktree)
        .env("SHIKA_PROJECT_ROOT", repo)
        .env("ROOT_WORKTREE_PATH", repo)
        .env("SHIKA_WORKTREE_PATH", worktree)
        .env("TERM", "dumb")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    for key in GIT_REDIRECTS {
        cmd.env_remove(key);
    }
    // Fence spawn/registration against synchronous cancellation during quit.
    // A control belongs to one launch, not multiple concurrent commands.
    let mut active_group = control.0.group.lock().unwrap_or_else(|e| e.into_inner());
    control.check()?;
    if active_group.is_some() {
        return Err(failed("Preparation control is already running a command"));
    }
    let mut child = cmd.spawn().map_err(io_failure)?;
    *active_group = Some(child.id() as libc::pid_t);
    drop(active_group);
    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut stderr = child.stderr.take().expect("piped stderr");
    let result = (|| {
        nonblocking(&stdout).map_err(io_failure)?;
        nonblocking(&stderr).map_err(io_failure)?;
        loop {
            check_deadline(control, deadline)?;
            drain(&mut stdout, report)?;
            drain(&mut stderr, report)?;
            // Observe without reaping: the leader's PID cannot be reused
            // before stop_group, even when a very short command has exited.
            if child_exited(child.id())? {
                return Ok(());
            }
            std::thread::sleep(POLL);
        }
    })();
    // Setup must not leave ordinary background writers running when the CLI
    // starts or rollback removes a tree. Commands must not daemonize/setsid.
    control.stop_group();
    let status = child.wait();
    result?;
    drain(&mut stdout, report)?;
    drain(&mut stderr, report)?;
    let status = status.map_err(io_failure)?;
    if status.success() {
        Ok(())
    } else {
        Err(failed(format!(
            "Setup command exited with {}",
            status
                .code()
                .map(|n| n.to_string())
                .unwrap_or_else(|| "a signal".into())
        )))
    }
}

fn child_exited(pid: u32) -> Result<bool> {
    // SAFETY: siginfo_t is a C POD, waitid writes it, and WNOWAIT leaves
    // reaping to our Child. This is our own direct child, not an arbitrary pid.
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    let result = unsafe {
        libc::waitid(
            libc::P_PID,
            pid as libc::id_t,
            &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    if result < 0 {
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::Interrupted {
            return Ok(false);
        }
        return Err(io_failure(error));
    }
    // SAFETY: waitid populated the exited-child arm; a zero pid means no exit.
    Ok(unsafe { info.si_pid() } != 0)
}

fn failed(message: impl Into<String>) -> Error {
    Error::Preparation(message.into())
}
fn io_failure(error: io::Error) -> Error {
    failed(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    use std::sync::mpsc;

    #[test]
    fn malformed_unknown_and_unbounded_configuration_is_rejected() {
        for text in [
            "",
            "{",
            "{\"setup-worktre\":[\"true\"]}",
            "{\"setup-worktree\":[\"\"]}",
            "{\"timeout-seconds\":0}",
            "{\"timeout-seconds\":3601}",
        ] {
            let parsed = serde_json::from_str::<PreparationConfig>(text);
            assert!(
                parsed.is_err() || parsed.unwrap().validate().is_err(),
                "accepted {text}"
            );
        }
        let default: PreparationConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(default.timeout_seconds, 600);
        assert!(default.validate().is_ok());
        let full: PreparationConfig =
            serde_json::from_str("{\"setup-worktree\":[\"true\"],\"copy-files\":[\".env\"]}")
                .unwrap();
        assert!(full.validate().is_ok());
    }

    #[test]
    fn destination_files_and_symlinks_are_never_overwritten_or_followed() {
        let root = std::env::temp_dir().join(format!(
            "shika-copy-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let source = root.join("source");
        let dest = root.join("dest");
        let outside = root.join("outside");
        fs::create_dir_all(source.join("local")).unwrap();
        fs::create_dir_all(&dest).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(source.join(".env"), "source").unwrap();
        fs::write(source.join("local/secret"), "source-secret").unwrap();
        fs::write(dest.join(".env"), "existing").unwrap();
        fs::write(outside.join("secret"), "outside").unwrap();
        symlink(&outside, dest.join("local")).unwrap();
        let src = directory(&source).unwrap();
        let dst = directory(&dest).unwrap();
        let control = PreparationControl::default();
        let deadline = Instant::now() + Duration::from_secs(5);
        assert!(copy_file(&src, &dst, Path::new(".env"), &control, deadline).is_err());
        assert!(copy_file(&src, &dst, Path::new("local/secret"), &control, deadline).is_err());
        assert_eq!(fs::read_to_string(dest.join(".env")).unwrap(), "existing");
        assert_eq!(
            fs::read_to_string(outside.join("secret")).unwrap(),
            "outside"
        );
        fs::remove_file(dest.join(".env")).unwrap();
        symlink(outside.join("secret"), dest.join(".env")).unwrap();
        assert!(copy_file(&src, &dst, Path::new(".env"), &control, deadline).is_err());
        assert_eq!(
            fs::read_to_string(outside.join("secret")).unwrap(),
            "outside"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn two_setup_slots_bound_concurrency_and_waiting_is_cancellable() {
        let limiter = Arc::new(PreparationLimiter::default());
        let control = PreparationControl::default();
        let first = limiter.acquire(&control).unwrap();
        let second = limiter.acquire(&control).unwrap();
        let waiting = PreparationControl::default();
        let child_control = waiting.clone();
        let child_limiter = limiter.clone();
        let child = std::thread::spawn(move || child_limiter.acquire(&child_control).map(|_| ()));
        waiting.cancel();
        assert_eq!(child.join().unwrap(), Err(Error::PreparationCancelled));
        let (tx, rx) = mpsc::channel();
        let child_limiter = limiter.clone();
        let child = std::thread::spawn(move || {
            let _slot = child_limiter
                .acquire(&PreparationControl::default())
                .unwrap();
            tx.send(()).unwrap();
        });
        assert!(rx.recv_timeout(Duration::from_millis(100)).is_err());
        drop(first);
        rx.recv_timeout(Duration::from_secs(2)).unwrap();
        child.join().unwrap();
        drop(second);
    }
}
