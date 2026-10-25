//! Shika's own copies of background images, in the app data's
//! `backgrounds/` folder. Settings names a copy by its file name only, so the
//! original can move or disappear, and a hand-edited file cannot point Shika
//! at a path outside that folder. The app decodes the bytes before storing
//! them; this module only writes, resolves, and prunes files.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::error::{Error, Result};

const DIR: &str = "backgrounds";
const PREFIX: &str = "background-";

/// True for a name `store` could have written: `background-` followed by
/// letters, digits, `-`, and one `.` before the extension. Anything else,
/// including a path, never resolves.
pub(crate) fn is_managed_name(name: &str) -> bool {
    let Some(rest) = name.strip_prefix(PREFIX) else {
        return false;
    };
    let Some((stem, extension)) = rest.split_once('.') else {
        return false;
    };
    !stem.is_empty()
        && !extension.is_empty()
        && stem.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        && extension.chars().all(|c| c.is_ascii_alphanumeric())
}

fn dir(data_dir: &Path) -> PathBuf {
    data_dir.join(DIR)
}

/// The copy named `file`, when the name is one Shika writes. The file may
/// still be missing; the caller reads it and reports that.
pub(crate) fn path(data_dir: &Path, file: &str) -> Option<PathBuf> {
    is_managed_name(file).then(|| dir(data_dir).join(file))
}

/// The original's extension, lowercased, when it is a short plain word.
/// Decoding looks at the bytes, so this only keeps the copy recognizable.
fn extension(original: &str) -> String {
    Path::new(original)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .filter(|e| !e.is_empty() && e.len() <= 5 && e.chars().all(|c| c.is_ascii_alphanumeric()))
        .unwrap_or_else(|| "img".into())
}

/// Write `bytes` as a new copy and return its file name. Every call gets a
/// fresh name, so a replaced image never reuses a cached one. The bytes land
/// in a temporary file first and are renamed into place whole.
pub(crate) fn store(data_dir: &Path, bytes: &[u8], original: &str) -> Result<String> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let dir = dir(data_dir);
    fs::create_dir_all(&dir).map_err(|_| Error::SaveBackground)?;
    let extension = extension(original);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    loop {
        let serial = COUNTER.fetch_add(1, Ordering::Relaxed);
        let name = format!(
            "{PREFIX}{nanos}-{}-{serial}.{extension}",
            std::process::id()
        );
        let destination = dir.join(&name);
        let temporary = dir.join(format!(".{name}.tmp"));
        let mut file = match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
        {
            Ok(file) => file,
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(_) => return Err(Error::SaveBackground),
        };
        let written = file.write_all(bytes).and_then(|_| file.sync_all());
        drop(file);
        if destination.exists() {
            let _ = fs::remove_file(&temporary);
            continue;
        }
        if written
            .and_then(|_| fs::rename(&temporary, &destination))
            .is_err()
        {
            let _ = fs::remove_file(&temporary);
            return Err(Error::SaveBackground);
        }
        return Ok(name);
    }
}

/// Remove every copy except `keep`, along with temporary files a crash left
/// behind. Failures are ignored: a leftover file costs only disk space.
pub(crate) fn prune(data_dir: &Path, keep: Option<&str>) {
    let Ok(entries) = fs::read_dir(dir(data_dir)) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let ours = is_managed_name(name)
            || (name.starts_with(&format!(".{PREFIX}")) && name.ends_with(".tmp"));
        if ours && Some(name) != keep {
            let _ = fs::remove_file(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "shika-background-{name}-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&dir).unwrap();
        dir
    }

    #[test]
    fn only_names_shika_writes_resolve() {
        assert!(is_managed_name("background-1-2-3.png"));
        assert!(is_managed_name("background-abc.jpeg"));
        for name in [
            "",
            "background-.png",
            "background-1",
            "background-1.",
            "background-1.p/g",
            "background-../x.png",
            "../background-1.png",
            "/tmp/background-1.png",
            "background-1.tar.gz",
            "photo.png",
        ] {
            assert!(!is_managed_name(name), "{name}");
            assert_eq!(path(Path::new("/data"), name), None, "{name}");
        }
        assert_eq!(
            path(Path::new("/data"), "background-1.png"),
            Some(PathBuf::from("/data/backgrounds/background-1.png"))
        );
    }

    #[test]
    fn store_writes_a_fresh_copy_and_prune_keeps_one() {
        let data = temp_dir("store");
        let first = store(&data, b"one", "Sky.PNG").unwrap();
        let second = store(&data, b"two", "no extension").unwrap();
        assert_ne!(first, second);
        assert!(first.ends_with(".png"));
        assert!(second.ends_with(".img"));
        assert_eq!(fs::read(path(&data, &first).unwrap()).unwrap(), b"one");
        assert_eq!(fs::read(path(&data, &second).unwrap()).unwrap(), b"two");
        // A crash between write and rename leaves a temporary file.
        fs::write(data.join("backgrounds/.background-9.png.tmp"), b"x").unwrap();
        // Files that are not Shika's stay.
        fs::write(data.join("backgrounds/notes.txt"), b"x").unwrap();
        prune(&data, Some(&second));
        let mut left: Vec<_> = fs::read_dir(data.join("backgrounds"))
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        left.sort();
        assert_eq!(left, vec![second.clone(), "notes.txt".to_string()]);
        prune(&data, None);
        assert!(!path(&data, &second).unwrap().exists());
        fs::remove_dir_all(&data).unwrap();
    }

    #[test]
    fn prune_without_a_folder_does_nothing() {
        let data = temp_dir("empty");
        prune(&data, None);
        fs::remove_dir_all(&data).unwrap();
    }
}
