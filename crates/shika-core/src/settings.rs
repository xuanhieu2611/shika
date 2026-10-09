use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

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

/// The agent column beside the terminal. A drag on its edge sets the width,
/// and it can be hidden so the terminal fills the window.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct Column {
    /// Points, from [`Column::MIN_WIDTH`] to [`Column::MAX_WIDTH`]. A narrow
    /// window can show less; the stored width comes back when it grows.
    #[serde(deserialize_with = "saturating_u16")]
    pub width: u16,
    pub hidden: bool,
}

impl Column {
    pub const MIN_WIDTH: u16 = 320;
    pub const MAX_WIDTH: u16 = 800;
    pub const DEFAULT_WIDTH: u16 = 540;

    /// A dragged width, rounded to a whole point and pulled into range.
    pub fn with_width(self, width: f32) -> Self {
        let width = if width.is_finite() {
            width
                .round()
                .clamp(f32::from(Self::MIN_WIDTH), f32::from(Self::MAX_WIDTH)) as u16
        } else {
            Self::DEFAULT_WIDTH
        };
        Self { width, ..self }
    }

    /// Values from a hand-edited file, pulled into range.
    pub fn clamped(self) -> Self {
        self.with_width(f32::from(self.width))
    }
}

impl Default for Column {
    fn default() -> Self {
        Self {
            width: Self::DEFAULT_WIDTH,
            hidden: false,
        }
    }
}

fn saturating_u16<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u16, D::Error> {
    Ok(u64::deserialize(deserializer)?.min(u64::from(u16::MAX)) as u16)
}

/// The read-only Changes panel right of the terminal. Only its width is
/// saved: the panel is closed on every launch.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Changes {
    /// Points, from [`Changes::MIN_WIDTH`] to [`Changes::MAX_WIDTH`]. A narrow
    /// window can show less; the stored width comes back when it grows.
    pub width: u16,
}

impl Changes {
    pub const MIN_WIDTH: u16 = 320;
    pub const MAX_WIDTH: u16 = 900;
    pub const DEFAULT_WIDTH: u16 = 480;

    /// A dragged width, rounded to a whole point and pulled into range.
    pub fn with_width(self, width: f32) -> Self {
        let width = if width.is_finite() {
            width
                .round()
                .clamp(f32::from(Self::MIN_WIDTH), f32::from(Self::MAX_WIDTH)) as u16
        } else {
            Self::DEFAULT_WIDTH
        };
        Self { width }
    }
}

impl Default for Changes {
    fn default() -> Self {
        Self {
            width: Self::DEFAULT_WIDTH,
        }
    }
}

/// `changes` as saved, pulled into range. A missing field, or one that is not
/// an object with a numeric `width`, is the default rather than a broken file.
fn changes_or_default<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Changes, D::Error> {
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(
        match value.get("width").and_then(serde_json::Value::as_f64) {
            Some(width) => Changes::default().with_width(width as f32),
            None => Changes::default(),
        },
    )
}

/// Terminal text size, in half-points so 12.5 can be stored exactly.
/// The range is 8 to 32. A missing value is 14.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FontSize(u8);

impl FontSize {
    const MIN_HALF: u8 = 16;
    const MAX_HALF: u8 = 64;
    const DEFAULT_HALF: u8 = 28;

    /// Size in points, for the terminal view.
    pub fn points(self) -> f32 {
        f32::from(self.0) / 2.0
    }

    /// `12.5` or `13`, for the settings field.
    pub fn text(self) -> String {
        let whole = self.0 / 2;
        if self.0.is_multiple_of(2) {
            whole.to_string()
        } else {
            format!("{whole}.5")
        }
    }

    /// One point toward the next whole size. 12.5 becomes 13 or 12.
    pub fn step(self, delta: i64) -> Self {
        if delta == 0 {
            return self;
        }
        let points = f64::from(self.0) / 2.0;
        let next = if delta > 0 {
            points.floor() + delta as f64
        } else if self.0.is_multiple_of(2) {
            points + delta as f64
        } else {
            points.ceil() + delta as f64
        };
        Self::from_points(next)
    }

    /// A typed size, rounded to the nearest half point and pulled into range.
    /// Blank or nonsense keeps the current size.
    pub fn from_text(text: &str) -> Option<Self> {
        let points = text.parse::<f64>().ok()?;
        points.is_finite().then(|| Self::from_points(points))
    }

    fn from_points(points: f64) -> Self {
        if !points.is_finite() {
            return Self::default();
        }
        let half = (points * 2.0).round();
        Self(half.clamp(f64::from(Self::MIN_HALF), f64::from(Self::MAX_HALF)) as u8)
    }
}

impl Default for FontSize {
    fn default() -> Self {
        Self(Self::DEFAULT_HALF)
    }
}

impl Serialize for FontSize {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if self.0.is_multiple_of(2) {
            serializer.serialize_u8(self.0 / 2)
        } else {
            serializer.serialize_f64(f64::from(self.0) / 2.0)
        }
    }
}

impl<'de> Deserialize<'de> for FontSize {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_points(f64::deserialize(deserializer)?))
    }
}

/// Which side of the theme catalog paints the window.
#[derive(Debug, Clone, Copy, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ThemeMode {
    /// Follow the macOS appearance.
    #[default]
    System,
    Light,
    Dark,
}

impl ThemeMode {
    /// The modes in Settings order, left to right.
    pub const ALL: [ThemeMode; 3] = [ThemeMode::System, ThemeMode::Light, ThemeMode::Dark];

    /// One segment left (`-1`) or right (`1`), stopping at the ends.
    pub fn step(self, delta: i64) -> Self {
        let at = Self::ALL.iter().position(|mode| *mode == self).unwrap_or(0) as i64;
        let next = (at + delta.signum()).clamp(0, Self::ALL.len() as i64 - 1);
        Self::ALL[next as usize]
    }
}

/// An unknown mode, or a value that is not a string, is the default rather
/// than a broken file.
impl<'de> Deserialize<'de> for ThemeMode {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(
            match serde_json::Value::deserialize(deserializer)?.as_str() {
                Some("light") => Self::Light,
                Some("dark") => Self::Dark,
                _ => Self::System,
            },
        )
    }
}

/// The ids of the default themes, the same as the catalog's in
/// `shika-terminal`. Core does not know the catalog.
pub const DEFAULT_LIGHT_THEME: &str = "shika-light";
pub const DEFAULT_DARK_THEME: &str = "shika-dark";

/// The theme mode and the picked theme for each side, as catalog ids. An id
/// the catalog does not know, or one from the other side, is kept as saved;
/// the app paints that side's default until the user picks again.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct ThemeSettings {
    pub mode: ThemeMode,
    #[serde(deserialize_with = "theme_id_or_light")]
    pub light: String,
    #[serde(deserialize_with = "theme_id_or_dark")]
    pub dark: String,
}

impl Default for ThemeSettings {
    fn default() -> Self {
        Self {
            mode: ThemeMode::System,
            light: DEFAULT_LIGHT_THEME.into(),
            dark: DEFAULT_DARK_THEME.into(),
        }
    }
}

fn theme_id_or_light<'de, D: Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    theme_id_or(deserializer, DEFAULT_LIGHT_THEME)
}

fn theme_id_or_dark<'de, D: Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    theme_id_or(deserializer, DEFAULT_DARK_THEME)
}

/// A theme id that is not a string takes the side's default.
fn theme_id_or<'de, D: Deserializer<'de>>(
    deserializer: D,
    default: &str,
) -> Result<String, D::Error> {
    Ok(match serde_json::Value::deserialize(deserializer)? {
        serde_json::Value::String(id) => id,
        _ => default.into(),
    })
}

fn notification_sound_on() -> bool {
    true
}

/// `settings.json`. A missing file, or a missing field, takes the default.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    /// Theme mode and the light and dark picks. Missing means System,
    /// Shika Light, and Shika Dark.
    pub theme: ThemeSettings,
    pub appearance: Appearance,
    /// Put in front of every branch name Shika picks, like `dev/`. Stored
    /// as typed; [`crate::normalize_branch_prefix`] makes it safe for git.
    pub branch_prefix: String,
    /// Terminal text size. Missing means 14.
    pub font_size: FontSize,
    /// Play the system alert sound with the ready banner. Missing means on.
    /// A bool's own default is false, so this field names its default.
    #[serde(default = "notification_sound_on")]
    pub notification_sound: bool,
    /// The agent column's width and whether it is hidden. Missing means a
    /// 540px column on screen.
    pub column: Column,
    /// The Changes panel's width. Missing or invalid means 480. Whether the
    /// panel is open is not saved.
    #[serde(deserialize_with = "changes_or_default")]
    pub changes: Changes,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: ThemeSettings::default(),
            appearance: Appearance::default(),
            branch_prefix: String::new(),
            font_size: FontSize::default(),
            notification_sound: true,
            column: Column::default(),
            changes: Changes::default(),
        }
    }
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
    settings.column = settings.column.clamped();
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
        assert_eq!(settings.font_size.points(), 14.0);
        assert!(settings.notification_sound);
    }

    #[test]
    fn appearance_round_trips() {
        let path = temp_file("round-trip");
        let file = SettingsFile::open(path.clone());
        let settings = Settings {
            theme: ThemeSettings {
                mode: ThemeMode::Dark,
                light: "catppuccin-latte".into(),
                dark: "catppuccin-mocha".into(),
            },
            appearance: Appearance {
                opacity: 75,
                blur: 30,
                translucency: Translucency::SidebarAndTerminal,
            },
            branch_prefix: "dev/".into(),
            font_size: FontSize::from_text("14.5").unwrap(),
            notification_sound: false,
            column: Column {
                width: 400,
                hidden: true,
            },
            changes: Changes { width: 610 },
        };
        file.save(&settings).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"theme\": {"));
        assert!(text.contains("\"mode\": \"dark\""));
        assert!(text.contains("\"light\": \"catppuccin-latte\""));
        assert!(text.contains("\"dark\": \"catppuccin-mocha\""));
        assert!(text.contains("\"translucency\": \"sidebarAndTerminal\""));
        assert!(text.contains("\"branchPrefix\": \"dev/\""));
        assert!(text.contains("\"fontSize\": 14.5"));
        assert!(text.contains("\"notificationSound\": false"));
        assert!(text.contains("\"column\": {"));
        assert!(text.contains("\"width\": 400"));
        assert!(text.contains("\"hidden\": true"));
        assert!(text.contains("\"changes\": {"));
        assert!(text.contains("\"width\": 610"));
        assert!(!text.contains("open"));
        assert_eq!(file.load().unwrap(), settings);
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn missing_fields_take_defaults_and_values_are_clamped() {
        let path = temp_file("partial");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            r#"{ "appearance": { "opacity": 250, "blur": 1000 }, "fontSize": 99 }"#,
        )
        .unwrap();
        let settings = SettingsFile::open(path.clone()).load().unwrap();
        assert_eq!(settings.appearance.opacity, Appearance::MAX_OPACITY);
        assert_eq!(settings.appearance.blur, Appearance::MAX_BLUR);
        assert_eq!(settings.appearance.translucency, Translucency::Sidebar);
        assert_eq!(settings.branch_prefix, "");
        assert_eq!(settings.font_size.points(), 32.0);
        assert!(settings.notification_sound);
        assert_eq!(settings.column, Column::default());
        assert_eq!(settings.changes, Changes::default());
        assert_eq!(settings.theme, ThemeSettings::default());
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn theme_defaults_to_system_and_the_shika_themes() {
        let theme = Settings::default().theme;
        assert_eq!(theme.mode, ThemeMode::System);
        assert_eq!(theme.light, "shika-light");
        assert_eq!(theme.dark, "shika-dark");
    }

    #[test]
    fn unknown_theme_values_take_defaults_and_ids_are_kept() {
        let path = temp_file("theme");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, r#"{ "theme": { "mode": "sepia", "dark": 7 } }"#).unwrap();
        let settings = SettingsFile::open(path.clone()).load().unwrap();
        assert_eq!(settings.theme, ThemeSettings::default());
        // An id the catalog may not know, or one from the other side, stays
        // as saved. The app resolves it when it paints.
        fs::write(
            &path,
            r#"{ "theme": { "mode": "light", "light": "shika-dark", "dark": "gone" } }"#,
        )
        .unwrap();
        let settings = SettingsFile::open(path.clone()).load().unwrap();
        assert_eq!(settings.theme.mode, ThemeMode::Light);
        assert_eq!(settings.theme.light, "shika-dark");
        assert_eq!(settings.theme.dark, "gone");
        fs::write(&path, r#"{ "theme": { "mode": "dark" } }"#).unwrap();
        let settings = SettingsFile::open(path.clone()).load().unwrap();
        assert_eq!(settings.theme.mode, ThemeMode::Dark);
        assert_eq!(settings.theme.light, "shika-light");
        assert_eq!(settings.theme.dark, "shika-dark");
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn theme_mode_steps_and_stops_at_the_ends() {
        assert_eq!(ThemeMode::System.step(1), ThemeMode::Light);
        assert_eq!(ThemeMode::Light.step(1), ThemeMode::Dark);
        assert_eq!(ThemeMode::Dark.step(1), ThemeMode::Dark);
        assert_eq!(ThemeMode::Dark.step(-1), ThemeMode::Light);
        assert_eq!(ThemeMode::System.step(-1), ThemeMode::System);
    }

    #[test]
    fn column_width_is_pulled_into_range() {
        let column = Column::default();
        assert_eq!(column.width, 540);
        assert!(!column.hidden);
        assert_eq!(column.with_width(412.6).width, 413);
        assert_eq!(column.with_width(10.).width, Column::MIN_WIDTH);
        assert_eq!(column.with_width(5000.).width, Column::MAX_WIDTH);
        assert_eq!(column.with_width(f32::NAN).width, Column::DEFAULT_WIDTH);
        let path = temp_file("column");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, r#"{ "column": { "width": 99999, "hidden": true } }"#).unwrap();
        let settings = SettingsFile::open(path.clone()).load().unwrap();
        assert_eq!(settings.column.width, Column::MAX_WIDTH);
        assert!(settings.column.hidden);
        fs::write(&path, r#"{ "column": { "hidden": true } }"#).unwrap();
        let settings = SettingsFile::open(path.clone()).load().unwrap();
        assert_eq!(settings.column.width, Column::DEFAULT_WIDTH);
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn changes_width_is_pulled_into_range_and_invalid_is_the_default() {
        let changes = Changes::default();
        assert_eq!(changes.width, 480);
        assert_eq!(changes.with_width(612.4).width, 612);
        assert_eq!(changes.with_width(10.).width, Changes::MIN_WIDTH);
        assert_eq!(changes.with_width(5000.).width, Changes::MAX_WIDTH);
        assert_eq!(changes.with_width(f32::NAN).width, Changes::DEFAULT_WIDTH);
        let path = temp_file("changes");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let load = |text: &str| {
            fs::write(&path, text).unwrap();
            SettingsFile::open(path.clone()).load().unwrap().changes
        };
        assert_eq!(load(r#"{ "changes": { "width": 99999 } }"#).width, 900);
        assert_eq!(load(r#"{ "changes": { "width": 12 } }"#).width, 320);
        assert_eq!(load(r#"{ "changes": { "width": 700.6 } }"#).width, 701);
        assert_eq!(load(r#"{ "changes": { "width": -5 } }"#).width, 320);
        assert_eq!(
            load(r#"{ "changes": { "width": "wide" } }"#),
            Changes::default()
        );
        assert_eq!(load(r#"{ "changes": 3 } "#), Changes::default());
        assert_eq!(
            load(r#"{ "changes": { "open": true } }"#),
            Changes::default()
        );
        assert_eq!(
            load(r#"{ "column": { "width": 400 } }"#),
            Changes::default()
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
    fn font_size_steps_by_one_point_and_keeps_a_typed_half() {
        let size = FontSize::default();
        assert_eq!(size.points(), 14.0);
        assert_eq!(size.text(), "14");
        assert_eq!(size.step(1).points(), 15.0);
        assert_eq!(size.step(-1).points(), 13.0);
        let half = FontSize::from_text("12.5").unwrap();
        assert_eq!(half.points(), 12.5);
        assert_eq!(half.text(), "12.5");
        assert_eq!(half.step(1).points(), 13.0);
        assert_eq!(half.step(-1).points(), 12.0);
        assert_eq!(half.step(1).step(1).text(), "14");
        assert_eq!(FontSize::from_text("12.2").unwrap().points(), 12.0);
        assert_eq!(FontSize::from_text("12.3").unwrap().points(), 12.5);
        assert_eq!(FontSize::from_text("7").unwrap().points(), 8.0);
        assert!(FontSize::from_text("").is_none());
        assert_eq!(FontSize::from_text("8").unwrap().step(-1).points(), 8.0);
        assert_eq!(FontSize::from_text("32").unwrap().step(1).points(), 32.0);
    }

    #[test]
    fn a_file_without_font_size_uses_the_default() {
        let path = temp_file("no-font");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, r#"{ "branchPrefix": "dev/" }"#).unwrap();
        let settings = SettingsFile::open(path.clone()).load().unwrap();
        assert_eq!(settings.font_size, FontSize::default());
        assert_eq!(settings.branch_prefix, "dev/");
        assert!(settings.notification_sound);
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
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
