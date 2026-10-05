use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Deserializer, Serialize};

use crate::error::{Error, Result};

/// Which surfaces let the desktop show through.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Translucency {
    #[default]
    Sidebar,
    SidebarAndTerminal,
}

/// The window background. At 100% opacity the window is opaque and blur
/// does nothing.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct Appearance {
    /// Percent, from 0 to [`Appearance::MAX_OPACITY`].
    #[serde(deserialize_with = "saturating_u8")]
    pub opacity: u8,
    /// Blur radius in points, from 0 to [`Appearance::MAX_BLUR`].
    #[serde(deserialize_with = "saturating_u8")]
    pub blur: u8,
    pub translucency: Translucency,
}

impl Appearance {
    pub const MAX_OPACITY: u8 = 100;
    /// The same ceiling as Ghostty's `background-blur`.
    pub const MAX_BLUR: u8 = u8::MAX;

    pub fn is_opaque(&self) -> bool {
        self.opacity >= Self::MAX_OPACITY
    }

    /// A typed or stepped opacity, pulled into range.
    pub fn with_opacity(self, percent: i64) -> Self {
        Self {
            opacity: percent.clamp(0, i64::from(Self::MAX_OPACITY)) as u8,
            ..self
        }
    }

    /// A typed or stepped blur radius, pulled into range.
    pub fn with_blur(self, radius: i64) -> Self {
        Self {
            blur: radius.clamp(0, i64::from(Self::MAX_BLUR)) as u8,
            ..self
        }
    }

    /// Values from a hand-edited file, pulled into range.
    pub fn clamped(self) -> Self {
        self.with_opacity(self.opacity.into())
    }
}

/// A number too large for the field becomes the field's maximum instead of
/// failing the whole file. `clamped` then applies the real range.
fn saturating_u8<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u8, D::Error> {
    Ok(u64::deserialize(deserializer)?.min(u64::from(u8::MAX)) as u8)
}

impl Default for Appearance {
    fn default() -> Self {
        Self {
            opacity: 100,
            blur: 20,
            translucency: Translucency::Sidebar,
        }
    }
}

/// `settings.json`. A missing file, or a missing field, takes the default.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub appearance: Appearance,
    /// Put in front of every branch name Shika picks, like `hieu/`. Stored
    /// as typed; [`crate::normalize_branch_prefix`] makes it safe for git.
    pub branch_prefix: String,
}

pub struct SettingsFile {
    path: PathBuf,
    lock: Mutex<()>,
}

impl SettingsFile {
    pub fn open(path: PathBuf) -> Self {
        Self {
            path,
            lock: Mutex::new(()),
        }
    }

    pub fn load(&self) -> Result<Settings> {
        let _guard = self.lock.lock().unwrap_or_else(|err| err.into_inner());
        load(&self.path)
    }

    pub fn save(&self, settings: &Settings) -> Result<()> {
        let _guard = self.lock.lock().unwrap_or_else(|err| err.into_inner());
        save(&self.path, settings)
    }
}

fn load(path: &Path) -> Result<Settings> {
    if !path.exists() {
        return Ok(Settings::default());
    }
    let text = fs::read_to_string(path).map_err(|_| Error::ReadSettings)?;
    if text.trim().is_empty() {
        return Ok(Settings::default());
    }
    let mut settings: Settings = serde_json::from_str(&text).map_err(|_| Error::ReadSettings)?;
    settings.appearance = settings.appearance.clamped();
    Ok(settings)
}

fn save(path: &Path, settings: &Settings) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|_| Error::SaveSettings)?;
    }
    let mut json = serde_json::to_string_pretty(settings).map_err(|_| Error::SaveSettings)?;
    json.push('\n');
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, json).map_err(|_| Error::SaveSettings)?;
    fs::rename(&tmp, path).map_err(|_| Error::SaveSettings)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_file(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "shika-settings-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        dir.join("settings.json")
    }

    #[test]
    fn a_missing_file_is_the_opaque_default() {
        let file = SettingsFile::open(temp_file("missing"));
        let settings = file.load().unwrap();
        assert_eq!(settings, Settings::default());
        assert!(settings.appearance.is_opaque());
    }

    #[test]
    fn appearance_round_trips() {
        let path = temp_file("round-trip");
        let file = SettingsFile::open(path.clone());
        let settings = Settings {
            appearance: Appearance {
                opacity: 75,
                blur: 30,
                translucency: Translucency::SidebarAndTerminal,
            },
            branch_prefix: "hieu/".into(),
        };
        file.save(&settings).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"translucency\": \"sidebarAndTerminal\""));
        assert!(text.contains("\"branchPrefix\": \"hieu/\""));
        assert_eq!(file.load().unwrap(), settings);
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn missing_fields_take_defaults_and_values_are_clamped() {
        let path = temp_file("partial");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            r#"{ "appearance": { "opacity": 250, "blur": 1000 } }"#,
        )
        .unwrap();
        let appearance = SettingsFile::open(path.clone()).load().unwrap().appearance;
        assert_eq!(appearance.opacity, Appearance::MAX_OPACITY);
        assert_eq!(appearance.blur, Appearance::MAX_BLUR);
        assert_eq!(appearance.translucency, Translucency::Sidebar);
        assert_eq!(
            SettingsFile::open(path.clone())
                .load()
                .unwrap()
                .branch_prefix,
            ""
        );
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn typed_values_are_pulled_into_range() {
        let appearance = Appearance::default();
        assert_eq!(appearance.with_opacity(0).opacity, 0);
        assert_eq!(appearance.with_opacity(-5).opacity, 0);
        assert_eq!(appearance.with_opacity(140).opacity, 100);
        assert_eq!(appearance.with_blur(120).blur, 120);
        assert_eq!(appearance.with_blur(999).blur, 255);
        assert_eq!(appearance.with_blur(-1).blur, 0);
    }

    #[test]
    fn a_broken_file_is_an_error() {
        let path = temp_file("broken");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "{ nope").unwrap();
        assert_eq!(
            SettingsFile::open(path.clone()).load(),
            Err(Error::ReadSettings)
        );
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
}
