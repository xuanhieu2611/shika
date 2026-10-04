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
mod tests {
    use super::*;

    fn normal() -> Modes {
        Modes::default()
    }

    fn app_cursor() -> Modes {
        Modes {
            app_cursor: true,
            ..Modes::default()
        }
    }

    fn key(key: Key, mods: KeyMods, modes: &Modes) -> Vec<u8> {
        encode_key(key, mods, modes).expect("encodable")
    }

    const SHIFT: KeyMods = KeyMods {
        shift: true,
        alt: false,
        ctrl: false,
    };
    const ALT: KeyMods = KeyMods {
        shift: false,
        alt: true,
        ctrl: false,
    };
    const CTRL: KeyMods = KeyMods {
        shift: false,
        alt: false,
        ctrl: true,
    };

    #[test]
    fn arrows_follow_cursor_key_mode() {
        assert_eq!(key(Key::Up, KeyMods::NONE, &normal()), b"\x1b[A");
        assert_eq!(key(Key::Left, KeyMods::NONE, &normal()), b"\x1b[D");
        assert_eq!(key(Key::Up, KeyMods::NONE, &app_cursor()), b"\x1bOA");
        assert_eq!(key(Key::Right, KeyMods::NONE, &app_cursor()), b"\x1bOC");
        assert_eq!(key(Key::Home, KeyMods::NONE, &normal()), b"\x1b[H");
        assert_eq!(key(Key::End, KeyMods::NONE, &app_cursor()), b"\x1bOF");
    }

    #[test]
    fn modified_arrows_use_the_csi_parameter_form_in_both_modes() {
        assert_eq!(key(Key::Left, ALT, &normal()), b"\x1b[1;3D");
        assert_eq!(key(Key::Left, ALT, &app_cursor()), b"\x1b[1;3D");
        assert_eq!(key(Key::Right, CTRL, &normal()), b"\x1b[1;5C");
        assert_eq!(key(Key::Up, SHIFT, &normal()), b"\x1b[1;2A");
        let all = KeyMods {
            shift: true,
            alt: true,
            ctrl: true,
        };
        assert_eq!(key(Key::Down, all, &normal()), b"\x1b[1;8B");
    }

    #[test]
    fn control_combos_send_c0_codes() {
        assert_eq!(key(Key::Char('c'), CTRL, &normal()), [0x03]);
        assert_eq!(key(Key::Char('C'), CTRL, &normal()), [0x03]);
        assert_eq!(key(Key::Char('a'), CTRL, &normal()), [0x01]);
        assert_eq!(key(Key::Char('z'), CTRL, &normal()), [0x1a]);
        assert_eq!(key(Key::Char(' '), CTRL, &normal()), [0x00]);
        assert_eq!(key(Key::Char('['), CTRL, &normal()), [0x1b]);
        assert_eq!(key(Key::Char('\\'), CTRL, &normal()), [0x1c]);
        assert_eq!(key(Key::Char('/'), CTRL, &normal()), [0x1f]);
        assert_eq!(encode_key(Key::Char('1'), CTRL, &normal()), None);
    }

    #[test]
    fn alt_as_meta_prefixes_escape() {
        assert_eq!(key(Key::Char('b'), ALT, &normal()), b"\x1bb");
        assert_eq!(key(Key::Char('B'), ALT, &normal()), b"\x1bB");
        let ctrl_alt = KeyMods {
            shift: false,
            alt: true,
            ctrl: true,
        };
        assert_eq!(key(Key::Char('x'), ctrl_alt, &normal()), [0x1b, 0x18]);
        assert_eq!(key(Key::Backspace, ALT, &normal()), [0x1b, 0x7f]);
    }

    #[test]
    fn editing_keys() {
        assert_eq!(key(Key::Enter, KeyMods::NONE, &normal()), b"\r");
        assert_eq!(key(Key::Enter, SHIFT, &normal()), b"\x1b\r");
        assert_eq!(key(Key::Enter, ALT, &normal()), b"\x1b\r");
        assert_eq!(key(Key::Tab, KeyMods::NONE, &normal()), b"\t");
        assert_eq!(key(Key::Tab, SHIFT, &normal()), b"\x1b[Z");
        assert_eq!(key(Key::Backspace, KeyMods::NONE, &normal()), [0x7f]);
        assert_eq!(key(Key::Backspace, CTRL, &normal()), [0x08]);
        assert_eq!(key(Key::Escape, KeyMods::NONE, &normal()), [0x1b]);
        assert_eq!(key(Key::Delete, KeyMods::NONE, &normal()), b"\x1b[3~");
        assert_eq!(key(Key::PageUp, KeyMods::NONE, &normal()), b"\x1b[5~");
        assert_eq!(key(Key::PageDown, CTRL, &normal()), b"\x1b[6;5~");
    }

    #[test]
    fn function_keys() {
        assert_eq!(key(Key::F(1), KeyMods::NONE, &normal()), b"\x1bOP");
        assert_eq!(key(Key::F(4), SHIFT, &normal()), b"\x1b[1;2S");
        assert_eq!(key(Key::F(5), KeyMods::NONE, &normal()), b"\x1b[15~");
        assert_eq!(key(Key::F(12), KeyMods::NONE, &normal()), b"\x1b[24~");
        assert_eq!(key(Key::F(12), CTRL, &normal()), b"\x1b[24;5~");
    }

    #[test]
    fn plain_and_unicode_characters_pass_through_as_utf8() {
        assert_eq!(key(Key::Char('a'), KeyMods::NONE, &normal()), b"a");
        assert_eq!(
            key(Key::Char('\u{1EC7}'), KeyMods::NONE, &normal()),
            "\u{1EC7}".as_bytes()
        );
    }

    #[test]
    fn paste_without_brackets_turns_newlines_into_cr() {
        assert_eq!(encode_paste("a\nb\r\nc", false), b"a\rb\rc");
    }

    #[test]
    fn bracketed_paste_wraps_and_strips_escape() {
        assert_eq!(
            encode_paste("ls\nrm -rf /tmp/x", true),
            b"\x1b[200~ls\rrm -rf /tmp/x\x1b[201~"
        );
        // A pasted end marker cannot close the bracket early.
        assert_eq!(
            encode_paste("a\x1b[201~b\x03", true),
            b"\x1b[200~a[201~b\x1b[201~"
        );
    }

    #[test]
    fn focus_reports() {
        assert_eq!(encode_focus(true), b"\x1b[I");
        assert_eq!(encode_focus(false), b"\x1b[O");
    }

    fn mouse_modes(mouse: MouseTracking, encoding: MouseEncoding) -> Modes {
        Modes {
            mouse,
            mouse_encoding: encoding,
            ..Modes::default()
        }
    }

    #[test]
    fn sgr_mouse_reports_press_release_and_wheel() {
        let modes = mouse_modes(MouseTracking::Click, MouseEncoding::Sgr);
        let at = ViewportPoint { row: 4, col: 9 };
        let press = encode_mouse(
            Some(MouseButton::Left),
            MouseAction::Press,
            at,
            KeyMods::NONE,
            &modes,
        );
        assert_eq!(press.unwrap(), b"\x1b[<0;10;5M");
        let release = encode_mouse(
            Some(MouseButton::Left),
            MouseAction::Release,
            at,
            KeyMods::NONE,
            &modes,
        );
        assert_eq!(release.unwrap(), b"\x1b[<0;10;5m");
        let wheel = encode_mouse(
            Some(MouseButton::WheelDown),
            MouseAction::Press,
            at,
            KeyMods::NONE,
            &modes,
        );
        assert_eq!(wheel.unwrap(), b"\x1b[<65;10;5M");
        let ctrl_right = encode_mouse(
            Some(MouseButton::Right),
            MouseAction::Press,
            at,
            CTRL,
            &modes,
        );
        assert_eq!(ctrl_right.unwrap(), b"\x1b[<18;10;5M");
    }

    #[test]
    fn motion_is_only_reported_when_the_mode_asks() {
        let at = ViewportPoint { row: 0, col: 0 };
        let click = mouse_modes(MouseTracking::Click, MouseEncoding::Sgr);
        assert!(
            encode_mouse(
                Some(MouseButton::Left),
                MouseAction::Motion,
                at,
                KeyMods::NONE,
                &click
            )
            .is_none()
        );
        let drag = mouse_modes(MouseTracking::Drag, MouseEncoding::Sgr);
        assert_eq!(
            encode_mouse(
                Some(MouseButton::Left),
                MouseAction::Motion,
                at,
                KeyMods::NONE,
                &drag
            )
            .unwrap(),
            b"\x1b[<32;1;1M"
        );
        assert!(encode_mouse(None, MouseAction::Motion, at, KeyMods::NONE, &drag).is_none());
        let motion = mouse_modes(MouseTracking::Motion, MouseEncoding::Sgr);
        assert_eq!(
            encode_mouse(None, MouseAction::Motion, at, KeyMods::NONE, &motion).unwrap(),
            b"\x1b[<35;1;1M"
        );
        let off = Modes::default();
        assert!(
            encode_mouse(
                Some(MouseButton::Left),
                MouseAction::Press,
                at,
                KeyMods::NONE,
                &off
            )
            .is_none()
        );
    }

    #[test]
    fn normal_mouse_encoding_uses_offset_bytes() {
        let modes = mouse_modes(MouseTracking::Click, MouseEncoding::Normal);
        let at = ViewportPoint { row: 1, col: 2 };
        assert_eq!(
            encode_mouse(
                Some(MouseButton::Left),
                MouseAction::Press,
                at,
                KeyMods::NONE,
                &modes
            )
            .unwrap(),
            [0x1b, b'[', b'M', 32, 35, 34]
        );
        assert_eq!(
            encode_mouse(
                Some(MouseButton::Right),
                MouseAction::Release,
                at,
                KeyMods::NONE,
                &modes
            )
            .unwrap(),
            [0x1b, b'[', b'M', 35, 35, 34]
        );
        // Past column 223 the one-byte form cannot encode the position.
        let far = ViewportPoint { row: 0, col: 300 };
        assert!(
            encode_mouse(
                Some(MouseButton::Left),
                MouseAction::Press,
                far,
                KeyMods::NONE,
                &modes
            )
            .is_none()
        );
        let utf8 = mouse_modes(MouseTracking::Click, MouseEncoding::Utf8);
        let bytes = encode_mouse(
            Some(MouseButton::Left),
            MouseAction::Press,
            far,
            KeyMods::NONE,
            &utf8,
        )
        .unwrap();
        assert_eq!(&bytes[..4], &[0x1b, b'[', b'M', 32]);
        assert_eq!(
            std::str::from_utf8(&bytes[4..]).unwrap().chars().next(),
            char::from_u32(333)
        );
    }
}
