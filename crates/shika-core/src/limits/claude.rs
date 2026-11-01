//! Session-local statusLine composition. No provider file or global setting is
//! changed. The helper saves only numeric quota fields in a private temp dir.
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::{QuotaSnapshot, QuotaWindow, now, wire::window};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const MAX_INPUT: u64 = 1024 * 1024;
const MAX_SAVED: u64 = 2048;

pub(super) struct Source {
    directory: PathBuf,
}

impl Source {
    pub(super) fn install(
        helper: &Path,
        root: &Path,
        cwd: &Path,
        args: &mut Vec<String>,
    ) -> Option<Self> {
        // Never accidentally launch a test harness or guess at another --settings layer.
        if !helper.is_absolute()
            || args.iter().any(|arg| {
                arg == "--settings"
                    || arg.starts_with("--settings=")
                    || arg == "--setting-sources"
                    || arg.starts_with("--setting-sources=")
            })
        {
            return None;
        }
        let status = statusline(root, cwd)?;
        let mut status = match status {
            None => json!({"type":"command"}),
            Some(Value::Object(map))
                if map.get("type").and_then(Value::as_str) == Some("command") =>
            {
                Value::Object(map)
            }
            _ => return None, // Explicitly disabled/unknown statusline stays untouched.
        };
        if status.get("command").is_some_and(|v| !v.is_string()) {
            return None;
        }
        let delegate = status.get("command").and_then(Value::as_str).unwrap_or("");
        if delegate.contains("--shika-statusline") {
            return None;
        }
        let source = Self::create().ok()?;
        let command = format!(
            "{} --shika-statusline {} {}",
            quote(&helper.to_string_lossy()),
            quote(&source.directory.to_string_lossy()),
            quote(delegate)
        );
        status["command"] = Value::String(command);
        args.extend([
            "--settings".into(),
            json!({"statusLine":status}).to_string(),
        ]);
        Some(source)
    }

    pub(super) fn create() -> std::io::Result<Self> {
        let root = fs::canonicalize(std::env::temp_dir())?;
        for _ in 0..8 {
            let directory = root.join(format!(
                "shika-quota-{}-{}",
                std::process::id(),
                crate::session::new_id(&[])
            ));
            match fs::DirBuilder::new().mode(0o700).create(&directory) {
                Ok(()) => return Ok(Self { directory }),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            }
        }
        Err(std::io::ErrorKind::AlreadyExists.into())
    }

    pub(super) fn path(&self) -> &Path {
        &self.directory
    }

    pub(super) fn read(&self) -> Option<QuotaSnapshot> {
        read_saved(&self.directory).map(|saved| saved.snapshot())
    }
}

impl Drop for Source {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

// Claude loads the user, project and local statusLine settings in that order.
// Refuse unreadable/unsupported inputs rather than replacing a custom command.
#[derive(Default, Deserialize)]
struct StatusSetting {
    #[serde(default, rename = "statusLine", deserialize_with = "present")]
    value: Option<Value>,
}
fn present<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Value>, D::Error> {
    Value::deserialize(d).map(Some)
}

fn statusline(root: &Path, cwd: &Path) -> Option<Option<Value>> {
    let mut status = None;
    for path in [
        root.join("settings.json"),
        cwd.join(".claude/settings.json"),
        cwd.join(".claude/settings.local.json"),
    ] {
        let file = match open_regular(&path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return None,
        };
        if file.metadata().ok()?.len() > MAX_INPUT {
            return None;
        }
        // Serde streams past every other setting, including env/auth helpers.
        let settings: StatusSetting = serde_json::from_reader(file.take(MAX_INPUT)).ok()?;
        if let Some(next) = settings.value.as_ref() {
            // Settings merge nested objects, not just whole statusLine values.
            if let (Some(old), Some(new)) = (
                status.as_mut().and_then(Value::as_object_mut),
                next.as_object(),
            ) {
                old.extend(new.clone());
            } else {
                status = Some(next.clone());
            }
        }
    }
    Some(status)
}

fn open_regular(path: &Path) -> std::io::Result<fs::File> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::ErrorKind::InvalidData.into());
    }
    Ok(file)
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SavedWindow {
    used: f64,
    reset: i64,
    observed: i64,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Saved {
    five_hour: Option<SavedWindow>,
    weekly: Option<SavedWindow>,
}

impl Saved {
    fn snapshot(&self) -> QuotaSnapshot {
        let convert = |saved: Option<SavedWindow>| {
            saved.and_then(|s| {
                (s.observed <= now() + 5)
                    .then(|| window(Some(s.used), Some(s.reset as f64), s.observed))
                    .flatten()
            })
        };
        QuotaSnapshot {
            five_hour: convert(self.five_hour),
            weekly: convert(self.weekly),
        }
    }
}

fn read_saved(directory: &Path) -> Option<Saved> {
    let file = open_regular(&directory.join("latest.json")).ok()?;
    if file.metadata().ok()?.len() > MAX_SAVED {
        return None;
    }
    serde_json::from_reader(file.take(MAX_SAVED)).ok()
}

fn private_directory(directory: &Path) -> bool {
    fs::symlink_metadata(directory).is_ok_and(|m| {
        m.is_dir() && m.uid() == unsafe { libc::geteuid() } && m.permissions().mode() & 0o077 == 0
    })
}

fn save(directory: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if !private_directory(directory) {
        return Err(std::io::ErrorKind::PermissionDenied.into());
    }
    // A nonblocking lock prevents overlapping statusline commands from racing
    // their read/replace or making a newer observation regress.
    let lock = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(directory.join("lock"))?;
    use std::os::fd::AsRawFd;
    if !lock.metadata()?.is_file()
        || unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0
    {
        return Ok(());
    }
    let input: Value = serde_json::from_slice(bytes)?;
    let previous = read_saved(directory).unwrap_or_default();
    let parse = |name: &str, previous: Option<SavedWindow>| {
        let raw = input.get("rate_limits")?.get(name)?;
        let QuotaWindow {
            used_percent,
            resets_at,
            observed_at,
        } = window(
            raw.get("used_percentage").and_then(Value::as_f64),
            raw.get("resets_at").and_then(Value::as_f64),
            now(),
        )?;
        // Statusline ticks are not new provider observations. Repeated values
        // retain their timestamp; unchanged windows never gain false freshness.
        if let Some(old) = previous
            && (old.reset > resets_at || (old.reset == resets_at && old.used >= used_percent))
        {
            return Some(old);
        }
        Some(SavedWindow {
            used: used_percent,
            reset: resets_at,
            observed: observed_at,
        })
    };
    let next = Saved {
        five_hour: parse("five_hour", previous.five_hour)
            .or(previous.five_hour.filter(|w| w.reset > now())),
        weekly: parse("seven_day", previous.weekly).or(previous.weekly.filter(|w| w.reset > now())),
    };
    if next.five_hour == previous.five_hour && next.weekly == previous.weekly {
        return Ok(());
    }
    let temp = directory.join(format!(".latest-{}", std::process::id()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)?;
        serde_json::to_writer(&mut file, &next)?;
        file.write_all(b"\n")?;
        fs::rename(&temp, directory.join("latest.json"))
    })();
    let _ = fs::remove_file(temp);
    result
}

pub(super) fn run(args: &[String]) -> i32 {
    if args.len() != 2 {
        return 2;
    }
    let mut bytes = Vec::new();
    if std::io::stdin()
        .take(MAX_INPUT + 1)
        .read_to_end(&mut bytes)
        .is_err()
    {
        return 1;
    }
    if bytes.len() as u64 <= MAX_INPUT {
        let _ = save(Path::new(&args[0]), &bytes);
    }
    if args[1].is_empty() {
        return 0;
    }
    // Forward exactly the same stdin/stdout, cwd and environment to the user's
    // command. Never print the JSON or turn it into terminal text.
    let Ok(mut child) = Command::new("/bin/sh")
        .args(["-c", &args[1]])
        .stdin(Stdio::piped())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
    else {
        return 1;
    };
    let mut pipe = child.stdin.take().unwrap();
    std::thread::spawn(move || {
        if pipe.write_all(&bytes).is_ok() {
            let _ = std::io::copy(&mut std::io::stdin(), &mut pipe);
        }
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.code().unwrap_or(1),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn snapshot_is_numeric_private_bounded_and_does_not_refresh_identical_values() {
        let source = Source::create().unwrap();
        let payload = json!({"rate_limits":{"five_hour":{"used_percentage":12.5,"resets_at":now()+500}},"secret":"must not persist"});
        save(&source.directory, payload.to_string().as_bytes()).unwrap();
        let before = fs::read(source.directory.join("latest.json")).unwrap();
        save(&source.directory, payload.to_string().as_bytes()).unwrap();
        assert_eq!(
            before,
            fs::read(source.directory.join("latest.json")).unwrap()
        );
        assert!(!String::from_utf8(before).unwrap().contains("secret"));
        assert_eq!(source.read().unwrap().five_hour.unwrap().used_percent, 12.5);
        assert!(source.read().unwrap().weekly.is_none());
        fs::write(
            source.directory.join("latest.json"),
            vec![b' '; MAX_SAVED as usize + 1],
        )
        .unwrap();
        assert!(source.read().is_none());
    }
    #[test]
    fn custom_statusline_and_padding_are_composed_without_config_writes() {
        let temp = Source::create().unwrap();
        let root = temp.directory.join("config");
        let cwd = temp.directory.join("repo");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(cwd.join(".claude")).unwrap();
        let raw = r#"{"statusLine":{"type":"command","command":"printf 'hello'","padding":3}}"#;
        fs::write(root.join("settings.json"), raw).unwrap();
        fs::write(
            cwd.join(".claude/settings.local.json"),
            r#"{"statusLine":{"padding":4}}"#,
        )
        .unwrap();
        let mut args = vec![];
        let source = Source::install(
            Path::new("/Applications/Shika's App"),
            &root,
            &cwd,
            &mut args,
        )
        .unwrap();
        let settings: Value = serde_json::from_str(&args[1]).unwrap();
        assert_eq!(settings["statusLine"]["padding"], 4);
        assert!(
            settings["statusLine"]["command"]
                .as_str()
                .unwrap()
                .contains("--shika-statusline")
        );
        assert!(
            settings["statusLine"]["command"]
                .as_str()
                .unwrap()
                .contains("hello")
        );
        assert_eq!(fs::read_to_string(root.join("settings.json")).unwrap(), raw);
        let path = source.directory.clone();
        drop(source);
        assert!(!path.exists());
        fs::write(root.join("settings.json"), r#"{"statusLine":null}"#).unwrap();
        assert!(Source::install(Path::new("/app"), &root, &cwd, &mut vec![]).is_none());
    }
}
