//! Settings > Keyboard: shortcut rows, recording and clearing chords, and
//! the key hints shown in the Settings footer.
use super::*;

impl Shika {
    pub(crate) fn recording_shortcut(&self) -> bool {
        self.key_recording
            && matches!(
                self.overlay,
                Some(Overlay::Settings {
                    section: SettingsSection::Keyboard,
                    ..
                })
            )
    }

    fn begin_key_recording(&mut self, row: usize, cx: &mut Context<Self>) {
        if !matches!(
            self.overlay,
            Some(Overlay::Settings {
                section: SettingsSection::Keyboard,
                ..
            })
        ) {
            return;
        }
        if let Some(Overlay::Settings { row: at, .. }) = &mut self.overlay {
            *at = row;
            self.settings_scroll.scroll_to_item(row);
        }
        self.key_recording = true;
        self.key_note = None;
        cx.notify();
    }

    fn clear_shortcut(&mut self, row: usize, cx: &mut Context<Self>) {
        let Some(command) = shortcuts::COMMANDS.get(row) else {
            return;
        };
        match shortcuts::assign(&self.keys, command.id, None) {
            Ok(next) => {
                self.keys = next;
                self.key_note = None;
                self.save_settings();
                install_keys(cx, &self.keys);
                cx.notify();
            }
            Err(text) => {
                self.key_note = Some(text);
                cx.notify();
            }
        }
    }

    pub(crate) fn capture_shortcut(&mut self, stroke: &gpui::Keystroke, cx: &mut Context<Self>) {
        let plain = !stroke.modifiers.platform
            && !stroke.modifiers.control
            && !stroke.modifiers.alt
            && !stroke.modifiers.shift;
        if plain && stroke.key == "escape" {
            self.key_recording = false;
            self.key_note = None;
            return;
        }
        let Some(command) = self
            .keyboard_row()
            .and_then(|row| shortcuts::COMMANDS.get(row))
        else {
            self.key_recording = false;
            return;
        };
        let id = command.id;
        match shortcuts::chord_from_keystroke(stroke) {
            Ok(chord) => match shortcuts::assign(&self.keys, id, Some(chord)) {
                Ok(next) => {
                    self.keys = next;
                    self.key_recording = false;
                    self.key_note = None;
                    self.save_settings();
                    install_keys(cx, &self.keys);
                }
                Err(text) => self.key_note = Some(text),
            },
            Err(text) => self.key_note = Some(text),
        }
    }

    fn keyboard_row(&self) -> Option<usize> {
        match self.overlay {
            Some(Overlay::Settings {
                section: SettingsSection::Keyboard,
                row,
                ..
            }) => Some(row),
            _ => None,
        }
    }

    pub(crate) fn on_keyboard_key(
        &mut self,
        row: usize,
        key: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let count = shortcuts::COMMANDS.len();
        match key {
            "j" | "down" | "tab" if count > 0 => {
                self.select_setting((row + 1) % count, window, cx);
            }
            "k" | "up" if count > 0 => {
                self.select_setting((row + count - 1) % count, window, cx);
            }
            "enter" => self.begin_key_recording(row, cx),
            "backspace" | "delete" => self.clear_shortcut(row, cx),
            "escape" => self.cancel_overlay(window, cx),
            _ => {}
        }
    }

    pub(crate) fn settings_keys(
        &self,
        section: SettingsSection,
        row: usize,
        editing: bool,
    ) -> &'static [(&'static str, &'static str)] {
        if section == SettingsSection::Keyboard {
            return if self.key_recording {
                &[("esc", "cancel")]
            } else {
                &[
                    ("j k", "choose"),
                    ("\u{21b5}", "set"),
                    ("\u{232b}", "clear"),
                    ("[ ]", "section"),
                    ("esc", "done"),
                ]
            };
        }
        if section == SettingsSection::Projects {
            return &[
                ("j k", "project"),
                ("↵", "setup"),
                ("[ ]", "section"),
                ("esc", "done"),
            ];
        }
        if section == SettingsSection::Usage {
            return &[
                ("h l", "provider"),
                ("w m y", "range"),
                ("[ ]", "section"),
                ("esc", "done"),
            ];
        }
        if section == SettingsSection::Agents {
            return if self.catalog.is_none() {
                &[("[ ]", "section"), ("esc", "done")]
            } else {
                &[
                    ("j k", "choose"),
                    ("h l", "enable"),
                    ("[ ]", "section"),
                    ("esc", "done"),
                ]
            };
        }
        match (editing, row) {
            (true, PREFIX_ROW) => &[("", "type a prefix"), ("↵", "apply"), ("esc", "cancel")],
            (true, _) => &[("", "type a number"), ("↵", "apply"), ("esc", "cancel")],
            (false, PREFIX_ROW) => &[
                ("j k", "choose"),
                ("↵", "edit"),
                ("[ ]", "section"),
                ("esc", "done"),
            ],
            (false, SOUND_ROW) => &[
                ("j k", "choose"),
                ("h l", "sound"),
                ("[ ]", "section"),
                ("esc", "done"),
            ],
            (false, MODE_ROW) => &[
                ("j k", "choose"),
                ("h l", "mode"),
                ("[ ]", "section"),
                ("esc", "done"),
            ],
            (false, LIGHT_ROW | DARK_ROW) => &[
                ("j k", "choose"),
                ("h l", "theme"),
                ("[ ]", "section"),
                ("esc", "done"),
            ],
            (false, APPLY_ROW) => &[
                ("j k", "choose"),
                ("h l", "change"),
                ("[ ]", "section"),
                ("esc", "done"),
            ],
            (false, IMAGE_ROW) if self.background.is_some() => &[
                ("j k", "choose"),
                ("↵", "replace"),
                ("a", "adjust"),
                ("⌫", "remove"),
                ("esc", "done"),
            ],
            (false, IMAGE_ROW) => &[
                ("j k", "choose"),
                ("↵", "choose image"),
                ("[ ]", "section"),
                ("esc", "done"),
            ],
            (false, EFFECT_ROW) => &[
                ("j k", "choose"),
                ("h l", "effect"),
                ("[ ]", "section"),
                ("esc", "done"),
            ],
            (false, _) => &[
                ("j k", "choose"),
                ("h l", "change"),
                ("", "type a number"),
                ("[ ]", "section"),
                ("esc", "done"),
            ],
        }
    }

    /// One shortcut: its name, and the chord or "none". Enter or a click on
    /// the chord records the next Command shortcut.
    pub(crate) fn shortcut_row(
        &self,
        index: usize,
        command: &shortcuts::Command,
        chrome: &Chrome,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let selected = matches!(
            &self.overlay,
            Some(Overlay::Settings {
                section: SettingsSection::Keyboard,
                row,
                ..
            }) if *row == index
        );
        let recording = selected && self.key_recording;
        let label = command.label;
        let chord = shortcuts::bound_label(&self.keys, command.id);
        let value = if recording {
            div()
                .flex_none()
                .text_size(px(12.))
                .text_color(chrome.ink_2)
                .child("Press a shortcut")
                .into_any_element()
        } else if let Some(chord) = chord {
            kbd(chord, chrome.sunken, chrome.ink_2).into_any_element()
        } else {
            div()
                .flex_none()
                .text_color(chrome.ink_4)
                .child("none")
                .into_any_element()
        };
        list_row(selected, chrome)
            .id(SharedString::from(format!("shortcut-{}", command.id)))
            .gap(px(12.))
            .justify_between()
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, window, cx| {
                this.select_setting(index, window, cx);
            }))
            .child(label)
            .child(
                div()
                    .id(SharedString::from(format!("shortcut-{}-chord", command.id)))
                    .flex_none()
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.begin_key_recording(index, cx);
                    }))
                    .child(value),
            )
    }
}
