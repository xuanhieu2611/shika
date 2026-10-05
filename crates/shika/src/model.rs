//! Navigation, first-prompt capture, and the CLI title watch, independent
//! of GPUI.
use std::time::{Duration, Instant};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Waiting,
    Working,
    Ready,
}
impl Status {
    pub fn label(self) -> &'static str {
        match self {
            Self::Waiting => "Waiting",
            Self::Working => "Working",
            Self::Ready => "Ready to check",
        }
    }
    pub fn rank(self) -> u8 {
        match self {
            Self::Ready => 0,
            Self::Working => 1,
            Self::Waiting => 2,
        }
    }
}
/// Output this long after the user's last key, click, scroll, or resize is
/// the agent's own work. Sooner, it is an echo or a redraw.
pub const ECHO: Duration = Duration::from_secs(1);
/// Whether a card turning Ready should notify. One notification per turn: a
/// turn starts when the user types (`typed`), and `notified` is the `typed`
/// that already notified. The agent must also have kept going on its own,
/// with its last output (`acted`) at least [`ECHO`] after the user's last
/// input of any kind, so typing a draft, focusing the terminal, or scrolling
/// it never notifies.
pub fn notify_ready(
    typed: Option<Instant>,
    notified: Option<Instant>,
    input: Option<Instant>,
    acted: Instant,
) -> bool {
    typed.is_some() && typed != notified && input.is_none_or(|input| acted >= input + ECHO)
}
/// Looking for the title the agent CLI gives its own session, which then
/// names the card and branch. From the first submitted line, it checks
/// every [`TitleWatch::EVERY`], one check at a time, and gives up after
/// [`TitleWatch::FOR`]. Without a title the card keeps its prompt name.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TitleWatch {
    #[default]
    Idle,
    Watching {
        until: Instant,
        next: Instant,
        checking: bool,
    },
    Done,
}
impl TitleWatch {
    pub const EVERY: Duration = Duration::from_secs(2);
    pub const FOR: Duration = Duration::from_secs(120);
    /// Starts once. Later calls change nothing.
    pub fn start(&mut self, now: Instant) {
        if *self == Self::Idle {
            *self = Self::Watching {
                until: now + Self::FOR,
                next: now + Self::EVERY,
                checking: false,
            };
        }
    }
    /// True when a check should run now, and marks it running.
    pub fn due(&mut self, now: Instant) -> bool {
        let Self::Watching {
            until,
            next,
            checking,
        } = self
        else {
            return false;
        };
        if *checking || now < *next {
            return false;
        }
        if now >= *until {
            *self = Self::Done;
            return false;
        }
        *checking = true;
        true
    }
    /// A check found no title yet.
    pub fn checked(&mut self, now: Instant) {
        if let Self::Watching { next, checking, .. } = self {
            *checking = false;
            *next = now + Self::EVERY;
        }
    }
    pub fn finish(&mut self) {
        *self = Self::Done;
    }
}
#[derive(Default)]
pub struct PromptCapture {
    line: String,
    escape: u8,
    csi: String,
    paste: bool,
    done: bool,
    utf8: Vec<u8>,
    mouse_remaining: u8,
}
impl PromptCapture {
    pub fn feed(&mut self, bytes: &[u8]) -> Option<String> {
        if self.done {
            return None;
        }
        self.utf8.extend_from_slice(bytes);
        let valid = match std::str::from_utf8(&self.utf8) {
            Ok(_) => self.utf8.len(),
            Err(err) => err.valid_up_to(),
        };
        let text = String::from_utf8_lossy(&self.utf8[..valid]).into_owned();
        self.utf8.drain(..valid);
        for ch in text.chars() {
            if self.mouse_remaining > 0 {
                self.mouse_remaining -= 1;
                continue;
            }
            if self.escape == 1 {
                self.escape = match ch {
                    '[' => {
                        self.csi.clear();
                        2
                    }
                    'O' => 3,
                    ']' => 4,
                    // Option+Backspace deletes the word before the cursor.
                    '\u{7f}' | '\u{8}' => {
                        self.delete_word();
                        0
                    }
                    _ => 0,
                };
                continue;
            }
            if self.escape == 2 {
                if ('@'..='~').contains(&ch) {
                    if ch == 'M' && self.csi.is_empty() {
                        self.mouse_remaining = 3;
                    }
                    if ch == '~' && self.csi == "200" {
                        self.paste = true;
                    }
                    if ch == '~' && self.csi == "201" {
                        self.paste = false;
                    }
                    self.escape = 0;
                } else {
                    self.csi.push(ch);
                }
                continue;
            }
            if self.escape == 3 {
                self.escape = 0;
                continue;
            }
            if self.escape == 4 {
                if ch == '\u{7}' {
                    self.escape = 0;
                } else if ch == '\u{1b}' {
                    self.escape = 5;
                }
                continue;
            }
            if self.escape == 5 {
                self.escape = if ch == '\\' { 0 } else { 4 };
                continue;
            }
            match ch {
                '\u{1b}' => self.escape = 1,
                '\r' | '\n' if self.paste => self.line.push(' '),
                '\r' | '\n' => {
                    let title = self.line.split_whitespace().collect::<Vec<_>>().join(" ");
                    self.line.clear();
                    if !title.is_empty() {
                        self.done = true;
                        return Some(title);
                    }
                }
                '\u{7f}' | '\u{8}' => {
                    self.line.pop();
                }
                '\u{15}' | '\u{3}' => self.line.clear(),
                '\u{17}' => self.delete_word(),
                ch if !ch.is_control() => self.line.push(ch),
                _ => {}
            }
        }
        None
    }
    fn delete_word(&mut self) {
        while self.line.ends_with(char::is_whitespace) {
            self.line.pop();
        }
        while !self.line.is_empty() && !self.line.ends_with(char::is_whitespace) {
            self.line.pop();
        }
    }
}
pub fn card_title(title: &str) -> String {
    if title.chars().count() <= 80 {
        title.into()
    } else {
        format!("{}…", title.chars().take(79).collect::<String>())
    }
}
/// Keep the selected card inside the three-card window.
pub fn visible_indices(len: usize, selected: Option<usize>) -> std::ops::Range<usize> {
    let start = selected
        .unwrap_or(0)
        .saturating_sub(2)
        .min(len.saturating_sub(3));
    start..(start + 3).min(len)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn the_title_watch_checks_one_at_a_time_then_gives_up() {
        let t0 = Instant::now();
        let mut w = TitleWatch::default();
        assert!(!w.due(t0 + Duration::from_secs(60)));
        w.start(t0);
        w.start(t0 + Duration::from_secs(30));
        assert!(!w.due(t0 + Duration::from_secs(1)));
        assert!(w.due(t0 + TitleWatch::EVERY));
        // Still running: no second check, even when the next one is due.
        assert!(!w.due(t0 + Duration::from_secs(10)));
        w.checked(t0 + Duration::from_secs(10));
        assert!(!w.due(t0 + Duration::from_secs(11)));
        assert!(w.due(t0 + Duration::from_secs(12)));
        w.checked(t0 + Duration::from_secs(12));
        assert!(!w.due(t0 + TitleWatch::FOR));
        assert_eq!(w, TitleWatch::Done);
        let mut w = TitleWatch::default();
        w.start(t0);
        w.finish();
        assert!(!w.due(t0 + Duration::from_secs(5)));
        w.start(t0);
        assert_eq!(w, TitleWatch::Done);
    }
    #[test]
    fn one_notification_per_turn() {
        let t0 = Instant::now();
        let at = |s: u64| t0 + Duration::from_secs(s);
        // Nothing typed yet: the CLI's banner settling is not a turn.
        assert!(!notify_ready(None, None, Some(at(0)), at(5)));
        // Typed at 1, the agent wrote until 10.
        assert!(notify_ready(Some(at(1)), None, Some(at(1)), at(10)));
        // That turn notified. Focus or scroll at 20 redraws at once, and
        // even output long after stays quiet until the user types again.
        assert!(!notify_ready(
            Some(at(1)),
            Some(at(1)),
            Some(at(20)),
            at(20)
        ));
        assert!(!notify_ready(Some(at(1)), Some(at(1)), Some(at(1)), at(40)));
        // A draft typed at 50 only echoes.
        assert!(!notify_ready(
            Some(at(50)),
            Some(at(1)),
            Some(at(50)),
            at(50)
        ));
        // Submitted at 60, the agent answered until 63.
        assert!(notify_ready(
            Some(at(60)),
            Some(at(1)),
            Some(at(60)),
            at(63)
        ));
    }
    #[test]
    fn navigation_never_exceeds_three() {
        assert_eq!(visible_indices(6, Some(5)), 3..6);
        assert_eq!(visible_indices(6, Some(0)), 0..3);
    }
    #[test]
    fn captures_only_first_nonempty_submission() {
        let mut c = PromptCapture::default();
        assert_eq!(
            c.feed(b"\r\x1b[Ahello worl\x7fd\r"),
            Some("hello word".into())
        );
        assert_eq!(c.feed(b"later\r"), None);
    }
    #[test]
    fn pasted_unicode_title_and_word_deletion() {
        let mut c = PromptCapture::default();
        assert_eq!(
            c.feed("fix 日本語 extra\u{17}\r".as_bytes()),
            Some("fix 日本語".into())
        );
    }
    #[test]
    fn option_backspace_deletes_a_word() {
        // Typed, then cleared with Option+Backspace (ESC DEL) and retyped.
        // The capture used to keep the cleared words and named the branch
        // `okay-we-need-to-do-somethiokay-we-need-a-way-to`.
        let mut c = PromptCapture::default();
        let mut typed = b"okay we need to do somethi".to_vec();
        typed.extend(b"\x1b\x7f".repeat(6));
        typed.extend(b"okay we need a way to send escape\r");
        assert_eq!(
            c.feed(&typed),
            Some("okay we need a way to send escape".into())
        );
        let mut c = PromptCapture::default();
        assert_eq!(c.feed(b"fix the  \x1b\x08bug\r"), Some("fix bug".into()));
    }
    #[test]
    fn split_utf8_is_preserved() {
        let mut c = PromptCapture::default();
        assert_eq!(c.feed(&[0xe6, 0x97]), None);
        assert_eq!(c.feed(&[0xa5, b'\r']), Some("日".into()));
    }
    #[test]
    fn bracketed_paste_newline_does_not_submit() {
        let mut c = PromptCapture::default();
        assert_eq!(c.feed(b"\x1b[200~fix one\nfix two\x1b[201~"), None);
        assert_eq!(c.feed(b"\r"), Some("fix one fix two".into()));
    }
    #[test]
    fn legacy_mouse_bytes_do_not_name_a_task() {
        let mut c = PromptCapture::default();
        assert_eq!(c.feed(b"\x1b[M !!fix\r"), Some("fix".into()));
    }
    #[test]
    fn terminal_responses_are_ignored() {
        let mut c = PromptCapture::default();
        assert_eq!(
            c.feed(b"\x1b[1;2R\x1b]10;rgb:ffff/ffff/ffff\x1b\\fix\r"),
            Some("fix".into())
        );
    }
}
