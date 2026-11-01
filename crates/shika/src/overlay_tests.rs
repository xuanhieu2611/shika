//! Tests for the overlay scrim.
use super::*;

#[test]
fn modal_scrim_preserves_appearance_previews_and_pointer_menus() {
    for section in SettingsSection::ALL {
        assert!(!Overlay::Settings(SettingsDialog::new(section, 0)).dims_background());
    }
    assert!(
        !Overlay::RestingMenu(RestingMenu {
            project: "demo".into(),
            position: gpui::point(px(10.), px(10.)),
        })
        .dims_background()
    );
    assert!(
        Overlay::Picker(PickerDialog {
            project: "demo".into(),
            index: 0,
            mode: PickerMode::Task,
        })
        .dims_background()
    );
    assert!(Overlay::CheckoutClose(close::CheckoutCloseDialog { index: 0 }).dims_background());
    assert!(
        Overlay::Quit(recovery::QuitDialog {
            previous: None,
            focus: None,
        })
        .dims_background()
    );
}
