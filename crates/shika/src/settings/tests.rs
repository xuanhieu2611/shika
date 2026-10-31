//! Tests for Settings section navigation.
use super::*;

#[test]
fn settings_projects_is_a_separate_section_and_navigation_wraps() {
    assert!(SettingsSection::ALL.len() == 5);
    assert!(SettingsSection::Agents.step(1) == SettingsSection::Projects);
    assert!(SettingsSection::Projects.step(1) == SettingsSection::Usage);
    assert!(SettingsSection::Usage.step(1) == SettingsSection::Appearance);
    assert!(SettingsSection::Appearance.step(-1) == SettingsSection::Usage);
    assert!(SettingsSection::Projects.label() == "Projects");
}
