//! The durable source registry: path-only targets journaled under
//! `usage-sources/` before an agent spawns, and the bounded recovery walk that
//! feeds them back to the queue as history after a restart.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use md5::{Digest, Md5};
use serde::{Deserialize, Serialize};

use super::COALESCE;
use super::queue::{Priority, Queue};
use crate::cli_title::{self, CliHome};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "cli", rename_all = "lowercase", deny_unknown_fields)]
pub(super) enum Target {
    Claude { directories: Vec<PathBuf> },
    Codex { root: PathBuf, cwds: Vec<PathBuf> },
}

impl Target {
    pub(super) fn new(home: &CliHome, preset: &str, cwd: &Path) -> Option<Self> {
        if !matches!(preset, "claude" | "codex") {
            return None;
        }
        let forms = cli_title::cwd_forms(cwd);
        match preset {
            "claude" => Some(Self::Claude {
                directories: forms.iter().map(|p| home.claude_project_dir(p)).collect(),
            }),
            "codex" => Some(Self::Codex {
                root: cwd.join(home.codex_root()),
                cwds: forms,
            }),
            _ => None,
        }
    }

    pub(super) fn id(&self) -> String {
        fn path(digest: &mut Md5, path: &Path) {
            let bytes = path.as_os_str().as_encoded_bytes();
            digest.update((bytes.len() as u64).to_le_bytes());
            digest.update(bytes);
        }
        let mut digest = Md5::new();
        match self {
            Self::Claude { directories } => {
                digest.update(b"claude");
                for directory in directories {
                    path(&mut digest, directory);
                }
            }
            Self::Codex { root, cwds } => {
                digest.update(b"codex");
                path(&mut digest, root);
                for cwd in cwds {
                    path(&mut digest, cwd);
                }
            }
        }
        format!("{:x}", digest.finalize())
    }
}

pub(super) fn persist_target(data_dir: &Path, target: &Target) -> std::io::Result<()> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let dir = data_dir.join("usage-sources");
    fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{}.json", target.id()));
    if read_target(&path)
        .ok()
        .is_some_and(|saved| saved.id() == target.id())
    {
        return Ok(());
    }
    let temp = dir.join(format!(
        ".{}-{}.tmp",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)?;
        serde_json::to_writer(&mut file, target)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        fs::rename(&temp, &path)?;
        File::open(&dir)?.sync_all()
    })();
    let _ = fs::remove_file(temp);
    result
}

pub(super) fn read_target(path: &Path) -> std::io::Result<Target> {
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > 64 * 1024 {
        return Err(std::io::ErrorKind::InvalidData.into());
    }
    serde_json::from_reader(file.take(64 * 1024))
        .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))
}

pub(super) const RECOVERY_BATCH: usize = 16;

pub(super) const RECOVERY_WINDOW: usize = 64;

#[derive(Default)]
pub(super) struct Recovery {
    pub(super) entries: Option<fs::ReadDir>,
}

impl Recovery {
    pub(super) fn step(&mut self, data_dir: &Path, queue: &Queue) {
        // Keep one additional pass if Usage opens during enumeration: a
        // registration already visited may have become readable since then.
        let start = if self.entries.is_none() {
            let _requests = queue.requests.lock().unwrap_or_else(|e| e.into_inner());
            let start = queue.recovery.swap(false, Ordering::AcqRel);
            if start {
                // Publish enumeration before clearing its pending predicate
                // becomes visible to diagnostics that hold this same lock.
                queue.enumerating.store(true, Ordering::Release);
            }
            start
        } else {
            false
        };
        if start {
            match fs::read_dir(data_dir.join("usage-sources")) {
                Ok(entries) => {
                    queue
                        .failures
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .remove("registry");
                    self.entries = Some(entries);
                }
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                    queue.enumerating.store(false, Ordering::Release);
                }
                Err(_) => {
                    queue
                        .failures
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .insert("registry".into());
                    queue.enumerating.store(false, Ordering::Release);
                }
            }
        }
        for _ in 0..RECOVERY_BATCH {
            let historical = {
                let requests = queue.requests.lock().unwrap_or_else(|e| e.into_inner());
                requests.history.len() + requests.continuations.len()
            };
            if queue.stopped.load(Ordering::Relaxed) || historical >= RECOVERY_WINDOW {
                return;
            }
            let Some(entries) = self.entries.as_mut() else {
                return;
            };
            let Some(entry) = entries.next() else {
                self.entries = None;
                queue.enumerating.store(false, Ordering::Release);
                return;
            };
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => {
                    queue
                        .failures
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .insert("registry".into());
                    continue;
                }
            };
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "json") {
                let failure = format!("metadata:{}", path.display());
                match read_target(&path) {
                    Ok(target) => {
                        queue
                            .failures
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .remove(&failure);
                        queue.schedule(Arc::new(target), COALESCE, 0, Priority::History);
                    }
                    Err(_) => {
                        queue
                            .failures
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .insert(failure);
                    }
                }
            }
        }
    }
}
