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
fn dropped_paths_escape_shell_characters() {
    assert_eq!(
        dropped_paths(["/tmp/Screenshot 2026-10-04 at 10.15.32\u{202f}AM.png"]),
        "/tmp/Screenshot\\ 2026-10-04\\ at\\ 10.15.32\u{202f}AM.png"
    );
    assert_eq!(
        dropped_paths(["/a/it's (1).png", "/b/$x&y.jpg"]),
        "/a/it\\'s\\ \\(1\\).png /b/\\$x\\&y.jpg"
    );
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
