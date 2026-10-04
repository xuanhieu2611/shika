use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use std::os::unix::fs::PermissionsExt;

const LOGIN_TIMEOUT: Duration = Duration::from_secs(20);
const GUI_PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin";

/// Why the login shell gave no PATH. The CLI picker shows this sentence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum LoginShellError {
    #[error("Could not run the login shell.")]
    Spawn,
    #[error("The login shell did not print a PATH.")]
    NoPath,
    #[error("The login shell failed.")]
    Failed,
    #[error("The login shell took too long.")]
    Timeout,
}

/// Login-shell PATH captured once at startup, plus the absolute CLI paths
/// resolved from it. Child processes must use this PATH, not the app's own.
#[derive(Debug, Clone)]
pub struct PathEnv {
    path: String,
    error: Option<LoginShellError>,
    resolved: HashMap<String, Option<PathBuf>>,
}

impl PathEnv {
    /// Runs the user's login shell once. Blocking: this can take seconds on a
    /// heavy shell config and gives up after 20 seconds.
    pub fn capture(binaries: &[&str]) -> Self {
        let shell = shell_path();
        Self::capture_with(&shell, GUI_PATH, LOGIN_TIMEOUT, binaries)
    }

    pub(crate) fn capture_with(
        shell: &Path,
        path: &str,
        timeout: Duration,
        binaries: &[&str],
    ) -> Self {
        match login_path(shell, path, timeout) {
            Ok(path) => {
                let resolved = binaries
                    .iter()
                    .map(|name| (name.to_string(), resolve_binary(name, &path)))
                    .collect();
                Self {
                    path,
                    error: None,
                    resolved,
                }
            }
            Err(error) => Self {
                path: String::new(),
                error: Some(error),
                resolved: binaries
                    .iter()
                    .map(|name| (name.to_string(), None))
                    .collect(),
            },
        }
    }

    #[cfg(test)]
    pub(crate) fn from_lookup(
        path: String,
        error: Option<LoginShellError>,
        binaries: &[(&str, Option<PathBuf>)],
    ) -> Self {
        Self {
            path,
            error,
            resolved: binaries
                .iter()
                .map(|(name, bin)| (name.to_string(), bin.clone()))
                .collect(),
        }
    }

    /// The login-shell PATH, or empty when the shell failed.
    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn error(&self) -> Option<LoginShellError> {
        self.error
    }

    /// Absolute path of a binary resolved at capture time.
    pub fn get(&self, binary: &str) -> Option<&Path> {
        self.resolved.get(binary).and_then(|path| path.as_deref())
    }

    /// A captured binary, or a fresh lookup on the login-shell PATH.
    pub fn resolve(&self, name: &str) -> Option<PathBuf> {
        if let Some(path) = self.get(name) {
            return Some(path.to_path_buf());
        }
        resolve_binary(name, &self.path)
    }
}

pub(crate) fn user_shell() -> PathBuf {
    shell_path()
}

fn shell_path() -> PathBuf {
    std::env::var_os("SHELL")
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| PathBuf::from("/bin/zsh"))
}

fn login_path(shell: &Path, path: &str, timeout: Duration) -> Result<String, LoginShellError> {
    if shell.as_os_str().is_empty() {
        return Err(LoginShellError::Spawn);
    }
    // Start from the same short PATH a Dock launch gets. Nix sets
    // `__NIX_DARWIN_SET_ENVIRONMENT_DONE` in a terminal, and a login shell that
    // inherits that flag will not rebuild PATH, so `~/.local/bin` disappears.
    let mut command = Command::new(shell);
    command
        .arg("-ilc")
        .arg(r#"printf %s "$PATH""#)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .env_clear()
        .env("PATH", path)
        .env("SHELL", shell);
    if let Some(home) = std::env::var_os("HOME") {
        command.env("HOME", home);
    }
    if let Some(user) = std::env::var_os("USER").or_else(|| std::env::var_os("LOGNAME")) {
        command.env("USER", &user);
        command.env("LOGNAME", user);
    }
    if let Some(tmpdir) = std::env::var_os("TMPDIR") {
        command.env("TMPDIR", tmpdir);
    }
    let mut child = command.spawn().map_err(|_| LoginShellError::Spawn)?;
    let pipe = child.stdout.take();
    let reader = thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_end(&mut buf);
        }
        buf
    });
    let status = match wait_for(&mut child, timeout) {
        Ok(status) => status,
        Err(err) => {
            let _ = reader.join();
            return Err(err);
        }
    };
    let bytes = reader.join().unwrap_or_default();
    let text = String::from_utf8_lossy(&bytes);
    if let Some(path) = parse_path_output(&text) {
        return Ok(path);
    }
    if status.success() {
        Err(LoginShellError::NoPath)
    } else {
        Err(LoginShellError::Failed)
    }
}

fn wait_for(child: &mut Child, timeout: Duration) -> Result<ExitStatus, LoginShellError> {
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) if start.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(LoginShellError::Timeout);
            }
            Ok(None) => thread::sleep(Duration::from_millis(20)),
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(LoginShellError::Spawn);
            }
        }
    }
}

fn parse_path_output(stdout: &str) -> Option<String> {
    stdout
        .split(['\n', '\r'])
        .map(str::trim)
        .rfind(|line| line.contains('/'))
        .map(str::to_string)
}

pub(crate) fn resolve_binary(name: &str, path_env: &str) -> Option<PathBuf> {
    if name.is_empty() || name.contains('/') || name.contains('\0') {
        return None;
    }
    for dir in path_env.split(':') {
        if dir.is_empty() {
            continue;
        }
        let candidate = Path::new(dir).join(name);
        if let Some(absolute) = executable_file(&candidate) {
            return Some(absolute);
        }
    }
    None
}

fn executable_file(path: &Path) -> Option<PathBuf> {
    let meta = fs::metadata(path).ok()?;
    if !meta.is_file() || meta.permissions().mode() & 0o111 == 0 {
        return None;
    }
    if path.is_absolute() {
        Some(path.to_path_buf())
    } else {
        Some(std::env::current_dir().ok()?.join(path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

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
            let path = std::env::temp_dir().join(format!("shika-path-{nanos}-{n}"));
            fs::create_dir_all(&path).unwrap();
            Self { path }
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    struct CwdGuard(PathBuf);

    impl CwdGuard {
        fn enter(dir: &Path) -> Self {
            let previous = std::env::current_dir().unwrap();
            std::env::set_current_dir(dir).unwrap();
            Self(previous)
        }
    }

    impl Drop for CwdGuard {
        fn drop(&mut self) {
            let _ = std::env::set_current_dir(&self.0);
        }
    }

    fn write_executable(path: &Path, body: &str) {
        fs::write(path, body).unwrap();
        let mut perms = fs::metadata(path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(path, perms).unwrap();
    }

    #[test]
    fn parse_keeps_the_path_after_startup_noise() {
        let stdout = "welcome aboard\nsee /tmp/old\n/usr/local/bin:/usr/bin";
        assert_eq!(
            parse_path_output(stdout).as_deref(),
            Some("/usr/local/bin:/usr/bin")
        );
        assert_eq!(parse_path_output("   \nhello\n").as_deref(), None);
        assert_eq!(parse_path_output("").as_deref(), None);
    }

    #[test]
    fn resolve_skips_missing_unexecutable_and_directories() {
        let scratch = Scratch::new();
        let first = scratch.path.join("first");
        let second = scratch.path.join("second");
        fs::create_dir(&first).unwrap();
        fs::create_dir(&second).unwrap();
        let blocked = first.join("claude");
        fs::write(&blocked, "nope").unwrap();
        fs::create_dir(first.join("agent")).unwrap();
        let claude = second.join("claude");
        write_executable(&claude, "#!/bin/sh\n");
        let path_env = format!("{}:{}:", first.display(), second.display());

        assert_eq!(
            resolve_binary("claude", &path_env).as_deref(),
            Some(claude.as_path())
        );
        assert_eq!(resolve_binary("agent", &path_env), None);
        assert_eq!(resolve_binary("missing", &path_env), None);
        assert_eq!(resolve_binary("", &path_env), None);
        assert_eq!(resolve_binary("a/b", &path_env), None);
    }

    #[test]
    fn relative_path_entry_is_stored_absolute() {
        let scratch = Scratch::new();
        let _cwd = CwdGuard::enter(&scratch.path);
        fs::create_dir("bin").unwrap();
        write_executable(Path::new("bin/claude"), "#!/bin/sh\n");

        let found = resolve_binary("claude", "bin").unwrap();

        assert!(found.is_absolute());
        assert!(found.ends_with("bin/claude"));
    }

    #[test]
    fn resolution_uses_the_shell_path_not_the_parent() {
        let scratch = Scratch::new();
        let bin = scratch.path.join("only-here");
        fs::create_dir(&bin).unwrap();
        let claude = bin.join("claude");
        write_executable(&claude, "#!/bin/sh\n");
        let shell = scratch.path.join("shell");
        write_executable(&shell, "#!/bin/sh\nprintf '%s' \"$PATH\"\n");

        let env = PathEnv::capture_with(
            &shell,
            &bin.to_string_lossy(),
            Duration::from_secs(2),
            &["claude", "agent"],
        );

        assert_eq!(env.error, None);
        assert_eq!(env.get("claude"), Some(claude.as_path()));
        assert_eq!(env.get("agent"), None);
    }

    #[test]
    fn banner_before_path_is_ignored() {
        let scratch = Scratch::new();
        let shell = scratch.path.join("shell");
        write_executable(
            &shell,
            "#!/bin/sh\necho welcome\nprintf '%s' \"/opt/bin:/usr/bin\"\n",
        );
        let env = PathEnv::capture_with(&shell, GUI_PATH, Duration::from_secs(2), &["claude"]);
        assert_eq!(env.path, "/opt/bin:/usr/bin");
        assert_eq!(env.error, None);
    }

    #[test]
    fn failed_shell_with_a_path_still_counts() {
        let scratch = Scratch::new();
        let shell = scratch.path.join("shell");
        write_executable(&shell, "#!/bin/sh\nprintf '%s' \"/usr/bin:/bin\"\nexit 1\n");
        let env = PathEnv::capture_with(&shell, GUI_PATH, Duration::from_secs(2), &["claude"]);
        assert_eq!(env.path, "/usr/bin:/bin");
        assert_eq!(env.error, None);
    }

    #[test]
    fn missing_shell_and_empty_output_are_errors() {
        let missing = PathEnv::capture_with(
            Path::new("/no/such/shika-shell"),
            GUI_PATH,
            Duration::from_secs(1),
            &["claude"],
        );
        assert_eq!(missing.error, Some(LoginShellError::Spawn));
        assert_eq!(
            LoginShellError::Spawn.to_string(),
            "Could not run the login shell."
        );
        assert_eq!(missing.get("claude"), None);

        let scratch = Scratch::new();
        let shell = scratch.path.join("shell");
        write_executable(&shell, "#!/bin/sh\nexit 1\n");
        let failed = PathEnv::capture_with(&shell, GUI_PATH, Duration::from_secs(2), &["claude"]);
        assert_eq!(failed.error, Some(LoginShellError::Failed));

        write_executable(&shell, "#!/bin/sh\necho hello\n");
        let noisy = PathEnv::capture_with(&shell, GUI_PATH, Duration::from_secs(2), &["claude"]);
        assert_eq!(noisy.error, Some(LoginShellError::NoPath));
    }

    #[test]
    fn a_stuck_shell_is_stopped() {
        let scratch = Scratch::new();
        let shell = scratch.path.join("shell");
        write_executable(&shell, "#!/bin/sh\nexec sleep 30\n");
        let started = Instant::now();
        let env = PathEnv::capture_with(&shell, GUI_PATH, Duration::from_millis(400), &["claude"]);
        assert_eq!(env.error, Some(LoginShellError::Timeout));
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn finder_launch_path_resolves_installed_clis() {
        let shell = shell_path();
        let env = PathEnv::capture_with(
            &shell,
            GUI_PATH,
            Duration::from_secs(20),
            &["claude", "agent"],
        );
        assert!(env.error.is_none(), "login shell failed: {:?}", env.error);
        let claude = env
            .get("claude")
            .unwrap_or_else(|| panic!("claude missing on {}", env.path));
        let agent = env
            .get("agent")
            .unwrap_or_else(|| panic!("agent missing on {}", env.path));
        assert!(claude.is_absolute(), "{claude:?}");
        assert!(agent.is_absolute(), "{agent:?}");
        assert!(
            claude.ends_with(".local/bin/claude"),
            "claude resolved to {claude:?}"
        );
        assert!(
            agent.ends_with(".local/bin/agent"),
            "agent resolved to {agent:?}"
        );
        assert!(env.path.split(':').any(|dir| dir.ends_with("/.local/bin")));
        let git = env.resolve("git").expect("git");
        assert!(git.is_absolute(), "{git:?}");
    }
}
