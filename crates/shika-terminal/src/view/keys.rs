//! Keyboard and clipboard input: keys, Copy and Paste, dropped files, and
//! page scrolling, encoded and sent to the PTY.

use gpui::{ClipboardItem, Context, ExternalPaths, KeyDownEvent, Keystroke, Window};

use super::{Copy, Paste, TerminalView};
use crate::input::{self, Key, KeyMods};

impl TerminalView {
    /// Typed or pasted input. Jumps back to the live screen first, like
    /// every terminal does.
    pub(super) fn send_input(&mut self, bytes: &[u8], cx: &mut Context<Self>) {
        if bytes.is_empty() {
            return;
        }
        if self.terminal.display_offset() != 0 {
            self.terminal.scroll_to_bottom();
            cx.notify();
        }
        self.terminal.write(bytes);
    }

    pub(super) fn key_down(
        &mut self,
        event: &KeyDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.marked_text.is_some() {
            return;
        }
        let Some((key, mods)) = key_for(&event.keystroke, self.config.option_as_meta) else {
            return;
        };
        let modes = self.terminal.modes();
        if let Some(bytes) = input::encode_key(key, mods, &modes) {
            self.send_input(&bytes, cx);
            cx.stop_propagation();
        }
    }

    pub(super) fn copy(&mut self, _: &Copy, _window: &mut Window, cx: &mut Context<Self>) {
        match self.terminal.selection_text() {
            Some(text) => cx.write_to_clipboard(ClipboardItem::new_string(text)),
            None => cx.propagate(),
        }
    }

    pub(super) fn paste(&mut self, _: &Paste, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            return;
        };
        let modes = self.terminal.modes();
        let bytes = input::encode_paste(&text, modes.bracketed_paste);
        self.send_input(&bytes, cx);
    }

    /// Files dropped from Finder or a screenshot thumbnail arrive as their
    /// escaped paths, pasted like any other text, and focus the terminal so
    /// the rest of the prompt can be typed.
    pub(super) fn drop_paths(
        &mut self,
        paths: &ExternalPaths,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let text = input::dropped_paths(paths.paths().iter().filter_map(|path| path.to_str()));
        if text.is_empty() {
            return;
        }
        window.focus(&self.focus_handle, cx);
        let modes = self.terminal.modes();
        let bytes = input::encode_paste(&text, modes.bracketed_paste);
        self.send_input(&bytes, cx);
    }

    pub(super) fn scroll_page(&mut self, up: bool, cx: &mut Context<Self>) {
        let modes = self.terminal.modes();
        if modes.alt_screen {
            let key = if up { Key::PageUp } else { Key::PageDown };
            let mods = KeyMods {
                shift: true,
                ..KeyMods::NONE
            };
            if let Some(bytes) = input::encode_key(key, mods, &modes) {
                self.terminal.report(&bytes);
            }
            return;
        }
        self.terminal.scroll_page(up);
        cx.notify();
    }
}

/// Map a GPUI keystroke to the encoder's key. `None` for plain text: that
/// arrives through the input handler instead, so dead keys and IME work.
pub(super) fn key_for(keystroke: &Keystroke, option_as_meta: bool) -> Option<(Key, KeyMods)> {
    let m = &keystroke.modifiers;
    // Command belongs to the app. Fn with a letter is a system shortcut.
    // Ctrl+Tab and Ctrl+Shift+Tab cycle Shika's tabs, so they stay out of the PTY.
    if m.platform || m.function || (keystroke.key == "tab" && m.control && !m.alt) {
        return None;
    }
    let named = match keystroke.key.as_str() {
        "enter" => Some(Key::Enter),
        "tab" => Some(Key::Tab),
        "backspace" => Some(Key::Backspace),
        "escape" => Some(Key::Escape),
        "up" => Some(Key::Up),
        "down" => Some(Key::Down),
        "left" => Some(Key::Left),
        "right" => Some(Key::Right),
        "home" => Some(Key::Home),
        "end" => Some(Key::End),
        "pageup" => Some(Key::PageUp),
        "pagedown" => Some(Key::PageDown),
        "insert" => Some(Key::Insert),
        "delete" => Some(Key::Delete),
        name => name
            .strip_prefix('f')
            .and_then(|n| n.parse::<u8>().ok())
            .filter(|n| (1..=20).contains(n))
            .map(Key::F),
    };
    if let Some(key) = named {
        let mods = KeyMods {
            shift: m.shift,
            alt: m.alt,
            ctrl: m.control,
        };
        return Some((key, mods));
    }

    let ch = if keystroke.key == "space" {
        ' '
    } else {
        let mut chars = keystroke.key.chars();
        let ch = chars.next()?;
        if chars.next().is_some() {
            return None;
        }
        ch
    };
    let meta = m.alt && option_as_meta;
    if !m.control && !meta {
        return None;
    }
    // GPUI reports Shift with letters as a lowercase key plus the flag;
    // other keys already carry the shifted character.
    let ch = if m.shift { ch.to_ascii_uppercase() } else { ch };
    Some((
        Key::Char(ch),
        KeyMods {
            shift: m.shift,
            alt: meta,
            ctrl: m.control,
        },
    ))
}
