//! The per-run socket directory, the stable `shika` command link under the
//! data directory, and the token generator.

use std::fs;
use std::io::Read;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};

use super::{ControlError, Result};

/// macOS allows 104 bytes in `sockaddr_un.sun_path`, including the NUL.
pub(super) const MAX_SOCKET_PATH: usize = 103;

pub(crate) fn random_bytes<const N: usize>() -> std::io::Result<[u8; N]> {
    let mut bytes = [0u8; N];
    fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// 128 random bits as 32 hex characters, from `/dev/urandom`. The token is
/// the only credential on the socket, so a failure to read randomness panics
/// rather than returning something guessable.
pub fn new_token() -> String {
    hex(&random_bytes::<16>().expect("could not read /dev/urandom"))
}

/// Where the `shika` command lives: `<data_dir>/bin/shika`, a symlink to the
/// running executable. The path is the same on every launch, because a Lead's
/// recorded conversation names it and a resumed Lead must find it again. Each
/// call replaces the link atomically (a temporary link, then `rename`), so the
/// name never goes missing. On APFS, resolving the link at the instant it is
/// replaced can still fail with EINVAL; the app installs once per launch, so
/// that window only exists while a second Shika starts on the same data
/// directory. The directory is `0700`. Returns `<data_dir>/bin`.
///
/// Two Shika processes sharing one data directory share the link; the one
/// started last wins. Nothing removes it on quit: a dangling or stale link
/// only matters while a process that wants it is running.
pub fn install_command(data_dir: &Path) -> Result<PathBuf, ControlError> {
    let setup = |err: std::io::Error| ControlError::Setup(err.to_string());
    let bin = std::path::absolute(data_dir).map_err(setup)?.join("bin");
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&bin)
        .map_err(setup)?;
    let exe = std::env::current_exe()
        .and_then(|exe| exe.canonicalize())
        .map_err(setup)?;
    // A name nobody else is using, so concurrent installs cannot trip over
    // each other's temporary link.
    let nonce = hex(&random_bytes::<4>().map_err(setup)?);
    let temp = bin.join(format!(".shika-{}-{nonce}.tmp", std::process::id()));
    symlink(&exe, &temp).map_err(setup)?;
    if let Err(err) = fs::rename(&temp, bin.join("shika")) {
        let _ = fs::remove_file(&temp);
        return Err(setup(err));
    }
    Ok(bin)
}

/// The per-run directory holding the socket, plus the stable command
/// directory from [`install_command`]:
///
/// ```text
/// <tmp>/shika-control-<pid>-<nonce>/   0700, removed on drop
///   sock                               bound by the server
/// <data_dir>/bin/shika                 symlink to the running executable
/// ```
///
/// The socket path appears only in the environment of processes started by
/// this run, so it may change every launch. The command path is written into
/// conversations, so it may not.
#[derive(Debug)]
pub struct ControlDir {
    root: PathBuf,
    bin_dir: PathBuf,
}

impl ControlDir {
    /// Creates the socket directory and installs the command under
    /// `data_dir`. Fails when the socket path would not fit in `sockaddr_un`
    /// (103 bytes).
    pub fn create(data_dir: &Path) -> Result<Self> {
        let setup = |err: std::io::Error| ControlError::Setup(err.to_string());
        // Bind the real path: on macOS the temp dir is under /private/var.
        let base = std::env::temp_dir().canonicalize().map_err(setup)?;
        let nonce = hex(&random_bytes::<4>().map_err(setup)?);
        let root = base.join(format!("shika-control-{}-{nonce}", std::process::id()));
        let socket = root.join("sock");
        if socket.as_os_str().len() > MAX_SOCKET_PATH {
            return Err(ControlError::Setup(format!(
                "{} is too long for a Unix socket path.",
                socket.display()
            )));
        }
        let bin_dir = install_command(data_dir)?;
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .map_err(setup)?;
        // Dropping removes the directory if anything below fails.
        let dir = Self { root, bin_dir };
        fs::set_permissions(&dir.root, fs::Permissions::from_mode(0o700)).map_err(setup)?;
        Ok(dir)
    }

    /// Where the server binds its `UnixListener`.
    pub fn socket_path(&self) -> PathBuf {
        self.root.join("sock")
    }

    /// The directory holding the `shika` command, for a Lead's or worker's
    /// `PATH` and for the absolute command path in prompts.
    pub fn bin_dir(&self) -> PathBuf {
        self.bin_dir.clone()
    }
}

impl Drop for ControlDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
