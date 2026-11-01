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
            share: 2500,
            hidden: true,
        },
        changes: Changes { width: 610 },
        agents: AgentSettings::default(),
        keys: KeyOverrides::default(),
        background: Some(Background {
            file: "background-1-2-3.png".into(),
            name: "Sky.png".into(),
            effect: BackgroundEffect::Halftone,
            framing: BackgroundFraming {
                focal_x: 2500,
                focal_y: 7500,
                zoom: 200,
            },
        }),
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
    assert!(text.contains("\"share\": 2500"));
    assert!(text.contains("\"hidden\": true"));
    assert!(text.contains("\"changes\": {"));
    assert!(text.contains("\"width\": 610"));
    assert!(!text.contains("open"));
    assert!(text.contains("\"background\": {"));
    assert!(text.contains("\"effect\": \"halftone\""));
    assert!(text.contains("\"focalX\": 2500"));
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
    assert!(settings.agents.enabled("claude"));
    fs::remove_dir_all(path.parent().unwrap()).unwrap();
}

#[test]
fn agent_choices_default_on_and_only_offs_are_saved() {
    let settings = Settings::default();
    assert!(settings.agents.enabled("claude"));
    assert!(settings.agents.enabled("pi"));

    let path = temp_file("agents");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        &path,
        r#"{ "agents": { "pi": false, "claude": true, "gone": false, "codex": "no", "": false } }"#,
    )
    .unwrap();
    let loaded = SettingsFile::open(path.clone()).load().unwrap();
    assert!(loaded.agents.enabled("claude"));
    assert!(loaded.agents.enabled("codex"));
    assert!(!loaded.agents.enabled("pi"));
    assert!(!loaded.agents.enabled("gone"));
    assert!(loaded.agents.enabled(""));

    fs::write(&path, r#"{ "agents": [] }"#).unwrap();
    let loaded = SettingsFile::open(path.clone()).load().unwrap();
    assert!(loaded.agents.enabled("pi"));

    let mut settings = Settings::default();
    settings.agents.set_enabled("cursor", false);
    settings.agents.set_enabled("", false);
    let file = SettingsFile::open(path.clone());
    file.save(&settings).unwrap();
    let text = fs::read_to_string(&path).unwrap();
    assert!(text.contains("\"cursor\": false"));
    assert!(!text.contains("\"claude\""));
    assert_eq!(file.load().unwrap().agents, settings.agents);
    fs::remove_dir_all(path.parent().unwrap()).unwrap();
}

#[test]
fn background_loads_leniently_and_none_is_not_written() {
    let path = temp_file("background");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let load = |json: &str| {
        fs::write(&path, json).unwrap();
        SettingsFile::open(path.clone()).load().unwrap().background
    };
    // A path outside the backgrounds folder never loads.
    assert_eq!(
        load(r#"{ "background": { "file": "../../etc/passwd" } }"#),
        None
    );
    assert_eq!(load(r#"{ "background": "sky.png" }"#), None);
    assert_eq!(load(r#"{ "background": { "name": "Sky.png" } }"#), None);
    let loaded = load(
        r#"{ "background": { "file": "background-1.png", "effect": "sepia",
                 "framing": { "focalX": 99999, "zoom": 9 } } }"#,
    )
    .unwrap();
    assert_eq!(loaded.name, "");
    assert_eq!(loaded.effect, BackgroundEffect::None);
    assert_eq!(loaded.framing.focal_x, BackgroundFraming::MAX_FOCAL);
    assert_eq!(loaded.framing.focal_y, 5000);
    assert_eq!(loaded.framing.zoom, BackgroundFraming::MIN_ZOOM);
    let file = SettingsFile::open(path.clone());
    file.save(&Settings::default()).unwrap();
    assert!(!fs::read_to_string(&path).unwrap().contains("background"));
    fs::remove_dir_all(path.parent().unwrap()).unwrap();
}

#[test]
fn background_framing_and_effects_stay_in_range() {
    let centered = BackgroundFraming::default();
    assert_eq!(centered.focal(), (0.5, 0.5));
    assert_eq!(centered.zoom_factor(), 1.);
    let framing = BackgroundFraming::from_parts(0.25, 1.5, 2.345);
    assert_eq!(
        (framing.focal_x, framing.focal_y, framing.zoom),
        (2500, 10000, 235)
    );
    assert_eq!(
        BackgroundFraming::from_parts(f32::NAN, -1., f32::INFINITY),
        BackgroundFraming {
            focal_x: 5000,
            focal_y: 0,
            zoom: 100
        }
    );
    assert_eq!(
        BackgroundFraming::from_parts(0.5, 0.5, 9.).zoom_factor(),
        4.
    );
    assert_eq!(BackgroundEffect::None.step(-1), BackgroundEffect::None);
    assert_eq!(BackgroundEffect::None.step(1), BackgroundEffect::Dither);
    assert_eq!(
        BackgroundEffect::Scanlines.step(1),
        BackgroundEffect::Scanlines
    );
    assert_eq!(
        BackgroundEffect::Scanlines.step(-3),
        BackgroundEffect::Halftone
    );
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
fn column_is_a_share_of_the_window_pulled_into_range() {
    let column = Column::default();
    assert_eq!(column.share, 3000);
    assert!(!column.hidden);
    // 30% of the window, in whole points, held to 320..800.
    assert_eq!(column.width(1400.), 420.);
    assert_eq!(column.width(1801.), 540.);
    assert_eq!(column.width(960.), f32::from(Column::MIN_WIDTH));
    assert_eq!(column.width(3200.), f32::from(Column::MAX_WIDTH));
    // A drag keeps its proportion when the window resizes.
    let dragged = column.with_width(500., 1400.);
    assert_eq!(dragged.share, 3571);
    assert_eq!(dragged.width(1400.), 500.);
    assert_eq!(dragged.width(1800.), 643.);
    assert_eq!(column.with_width(10., 1400.).width(1400.), 320.);
    assert_eq!(column.with_width(5000., 1400.).share, 5714);
    assert_eq!(column.with_width(5000., 400.).share, Column::MAX_SHARE);
    assert_eq!(dragged.with_width(f32::NAN, 1400.), column);
    assert_eq!(dragged.with_width(500., 0.), column);
    assert_eq!(dragged.reset(), column);
    let path = temp_file("column");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, r#"{ "column": { "share": 99999, "hidden": true } }"#).unwrap();
    let settings = SettingsFile::open(path.clone()).load().unwrap();
    assert_eq!(settings.column.share, Column::MAX_SHARE);
    assert!(settings.column.hidden);
    // An older build's fixed width gives way to the default share.
    fs::write(&path, r#"{ "column": { "width": 540, "hidden": true } }"#).unwrap();
    let settings = SettingsFile::open(path.clone()).load().unwrap();
    assert_eq!(settings.column.share, Column::DEFAULT_SHARE);
    assert!(settings.column.hidden);
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
fn key_overrides_round_trip_and_a_missing_field_is_empty() {
    let path = temp_file("keys");
    let file = SettingsFile::open(path.clone());
    let mut keys = KeyOverrides::default();
    keys.set("toggleChanges", "cmd-r");
    keys.set("selectTab1", "");
    let settings = Settings {
        keys,
        ..Settings::default()
    };
    file.save(&settings).unwrap();
    let text = fs::read_to_string(&path).unwrap();
    assert!(text.contains("\"toggleChanges\": \"cmd-r\""));
    assert!(text.contains("\"selectTab1\": \"\""));
    assert_eq!(file.load().unwrap(), settings);
    fs::remove_dir_all(path.parent().unwrap()).unwrap();

    let bare = temp_file("keys-bare");
    fs::create_dir_all(bare.parent().unwrap()).unwrap();
    fs::write(&bare, "{}\n").unwrap();
    assert!(
        SettingsFile::open(bare.clone())
            .load()
            .unwrap()
            .keys
            .is_empty()
    );
    fs::remove_dir_all(bare.parent().unwrap()).unwrap();

    let defaults_path = temp_file("keys-default");
    SettingsFile::open(defaults_path.clone())
        .save(&Settings::default())
        .unwrap();
    assert!(!fs::read_to_string(&defaults_path).unwrap().contains("keys"));
    fs::remove_dir_all(defaults_path.parent().unwrap()).unwrap();
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
