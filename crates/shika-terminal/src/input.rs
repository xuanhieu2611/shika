//! Bytes a terminal sends for keys, paste, mouse, and focus. Pure functions
//! over Shika's own types, so they can be tested without a window.

use crate::types::{Modes, MouseEncoding, MouseTracking, ViewportPoint};

/// A key the encoder knows. `Char` carries the character the key types with
/// Shift already applied (`A`, `!`), before Control or Alt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Enter,
    Tab,
    Backspace,
    Escape,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    Delete,
    /// F1 to F20.
    F(u8),
}

/// Modifiers that reach the program. Command never does; it belongs to the
/// app. Alt is only set here when Option is acting as Meta.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct KeyMods {
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
}

impl KeyMods {
    pub const NONE: KeyMods = KeyMods {
        shift: false,
        alt: false,
        ctrl: false,
    };

    /// The xterm modifier parameter: 1 plus shift 1, alt 2, ctrl 4.
    fn param(self) -> u8 {
        1 + self.shift as u8 + 2 * self.alt as u8 + 4 * self.ctrl as u8
    }

    fn any(self) -> bool {
        self.shift || self.alt || self.ctrl
    }
}

const ESC: u8 = 0x1b;

/// Encode one key press. `None` means the key has no terminal meaning (for
/// example Ctrl with a character that has no control code), and the caller
/// should let it go elsewhere.
pub fn encode_key(key: Key, mods: KeyMods, modes: &Modes) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(8);
    match key {
        Key::Char(ch) => return encode_char(ch, mods),
        Key::Enter => {
            // Shift-Enter sends ESC CR, the same as Alt-Enter. Claude Code
            // and other agent CLIs read that as "new line, do not submit",
            // and a shell's line editor treats it as Meta-Return.
            if mods.alt || mods.shift {
                out.push(ESC);
            }
            out.push(b'\r');
        }
        Key::Tab => {
            if mods.shift {
                out.extend_from_slice(b"\x1b[Z");
            } else {
                if mods.alt {
                    out.push(ESC);
                }
                out.push(b'\t');
            }
        }
        Key::Backspace => {
            if mods.alt {
                out.push(ESC);
            }
            out.push(if mods.ctrl { 0x08 } else { 0x7f });
        }
        Key::Escape => {
            if mods.alt {
                out.push(ESC);
            }
            out.push(ESC);
        }
        Key::Up => cursor_key(&mut out, b'A', mods, modes),
        Key::Down => cursor_key(&mut out, b'B', mods, modes),
        Key::Right => cursor_key(&mut out, b'C', mods, modes),
        Key::Left => cursor_key(&mut out, b'D', mods, modes),
        Key::Home => cursor_key(&mut out, b'H', mods, modes),
        Key::End => cursor_key(&mut out, b'F', mods, modes),
        Key::Insert => tilde_key(&mut out, 2, mods),
        Key::Delete => tilde_key(&mut out, 3, mods),
        Key::PageUp => tilde_key(&mut out, 5, mods),
        Key::PageDown => tilde_key(&mut out, 6, mods),
        Key::F(n @ 1..=4) => {
            let last = b"PQRS"[(n - 1) as usize];
            if mods.any() {
                out.extend_from_slice(format!("\x1b[1;{}", mods.param()).as_bytes());
                out.push(last);
            } else {
                out.extend_from_slice(&[ESC, b'O', last]);
            }
        }
        Key::F(n @ 5..=20) => {
            let code = [
                15, 17, 18, 19, 20, 21, 23, 24, 25, 26, 28, 29, 31, 32, 33, 34,
            ][(n - 5) as usize];
            tilde_key(&mut out, code, mods);
        }
        Key::F(_) => return None,
    }
    Some(out)
}

fn encode_char(ch: char, mods: KeyMods) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(8);
    if mods.alt {
        out.push(ESC);
    }
    if mods.ctrl {
        out.push(control_code(ch)?);
        return Some(out);
    }
    let mut buf = [0u8; 4];
    out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
    Some(out)
}

/// The C0 code Ctrl plus this key sends, following xterm and the US layout
/// aliases (Ctrl-2 is NUL, Ctrl-6 is RS, Ctrl-/ is US, and so on).
fn control_code(ch: char) -> Option<u8> {
    match ch {
        'a'..='z' => Some(ch as u8 - b'a' + 1),
        'A'..='Z' => Some(ch as u8 - b'A' + 1),
        ' ' | '@' | '2' => Some(0x00),
        '[' | '3' => Some(0x1b),
        '\\' | '4' => Some(0x1c),
        ']' | '5' => Some(0x1d),
        '^' | '6' => Some(0x1e),
        '_' | '-' | '/' | '7' => Some(0x1f),
        '?' | '8' => Some(0x7f),
        _ => None,
    }
}

fn cursor_key(out: &mut Vec<u8>, last: u8, mods: KeyMods, modes: &Modes) {
    if mods.any() {
        out.extend_from_slice(format!("\x1b[1;{}", mods.param()).as_bytes());
        out.push(last);
    } else if modes.app_cursor {
        out.extend_from_slice(&[ESC, b'O', last]);
    } else {
        out.extend_from_slice(&[ESC, b'[', last]);
    }
}

fn tilde_key(out: &mut Vec<u8>, code: u8, mods: KeyMods) {
    if mods.any() {
        out.extend_from_slice(format!("\x1b[{code};{}~", mods.param()).as_bytes());
    } else {
        out.extend_from_slice(format!("\x1b[{code}~").as_bytes());
    }
}

/// Paste text the way xterm.js does: line breaks become CR, which is what a
/// terminal's Enter sends. With bracketed paste on, the text is wrapped in
/// the start and end markers, and ESC and ETX are removed so pasted text
/// cannot close the bracket early or interrupt the program.
pub fn encode_paste(text: &str, bracketed: bool) -> Vec<u8> {
    let normalized = text.replace("\r\n", "\r").replace('\n', "\r");
    if !bracketed {
        return normalized.into_bytes();
    }
    let mut out = Vec::with_capacity(normalized.len() + 12);
    out.extend_from_slice(b"\x1b[200~");
    out.extend(normalized.bytes().filter(|b| *b != ESC && *b != 0x03));
    out.extend_from_slice(b"\x1b[201~");
    out
}

/// Paths dropped on the terminal, as text a shell or an agent CLI reads
/// back as paths: each one backslash-escaped, joined by spaces. Claude
/// Code and Cursor turn a pasted image path into an attachment.
pub fn dropped_paths<'a>(paths: impl IntoIterator<Item = &'a str>) -> String {
    paths
        .into_iter()
        .map(shell_escape)
        .collect::<Vec<_>>()
        .join(" ")
}

/// Backslash-escape the characters a POSIX shell would split on or expand,
/// the same set Ghostty escapes for a dropped file.
fn shell_escape(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for ch in path.chars() {
        if matches!(
            ch,
            '\\' | ' '
                | '\t'
                | '('
                | ')'
                | '['
                | ']'
                | '{'
                | '}'
                | '<'
                | '>'
                | '"'
                | '\''
                | '`'
                | '!'
                | '#'
                | '$'
                | '&'
                | ';'
                | '|'
                | '*'
                | '?'
        ) {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// Sent when the terminal gains or loses focus, if the program asked for it
/// (DECSET 1004).
pub fn encode_focus(focused: bool) -> &'static [u8] {
    if focused { b"\x1b[I" } else { b"\x1b[O" }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
    WheelUp,
    WheelDown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseAction {
    Press,
    Release,
    /// Pointer moved. `button` is the one held, if any.
    Motion,
}

/// Encode a mouse report, or `None` when the program's tracking mode does
/// not want this event.
pub fn encode_mouse(
    button: Option<MouseButton>,
    action: MouseAction,
    at: ViewportPoint,
    mods: KeyMods,
    modes: &Modes,
) -> Option<Vec<u8>> {
    let wheel = matches!(button, Some(MouseButton::WheelUp | MouseButton::WheelDown));
    match (modes.mouse, action) {
        (MouseTracking::Off, _) => return None,
        (_, MouseAction::Release) if wheel => return None,
        (MouseTracking::Click, MouseAction::Motion) => return None,
        (MouseTracking::Drag, MouseAction::Motion) if button.is_none() => return None,
        _ => {}
    }

    let mut code: u32 = match button {
        Some(MouseButton::Left) => 0,
        Some(MouseButton::Middle) => 1,
        Some(MouseButton::Right) => 2,
        Some(MouseButton::WheelUp) => 64,
        Some(MouseButton::WheelDown) => 65,
        None => 3,
    };
    if action == MouseAction::Motion {
        code += 32;
    }
    if mods.shift {
        code += 4;
    }
    if mods.alt {
        code += 8;
    }
    if mods.ctrl {
        code += 16;
    }
    let x = at.col as u32 + 1;
    let y = at.row as u32 + 1;

    match modes.mouse_encoding {
        MouseEncoding::Sgr => {
            let last = if action == MouseAction::Release {
                'm'
            } else {
                'M'
            };
            Some(format!("\x1b[<{code};{x};{y}{last}").into_bytes())
        }
        MouseEncoding::Normal | MouseEncoding::Utf8 => {
            // The old encodings cannot say which button was released.
            if action == MouseAction::Release {
                code = (code & !0b11) | 3;
            }
            let mut out = b"\x1b[M".to_vec();
            let utf8 = modes.mouse_encoding == MouseEncoding::Utf8;
            for value in [code, x, y] {
                push_mouse_value(&mut out, value + 32, utf8)?;
            }
            Some(out)
        }
    }
}

fn push_mouse_value(out: &mut Vec<u8>, value: u32, utf8: bool) -> Option<()> {
    if utf8 {
        let ch = char::from_u32(value).filter(|_| value <= 2047)?;
        let mut buf = [0u8; 4];
        out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
    } else {
        out.push(u8::try_from(value).ok()?);
    }
    Some(())
}

#[cfg(test)]
#[path = "input_tests.rs"]
mod tests;
