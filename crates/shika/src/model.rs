//! Navigation and first-prompt capture, independent of GPUI.
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
                '\u{17}' => {
                    while self.line.ends_with(char::is_whitespace) {
                        self.line.pop();
                    }
                    while !self.line.is_empty() && !self.line.ends_with(char::is_whitespace) {
                        self.line.pop();
                    }
                }
                ch if !ch.is_control() => self.line.push(ch),
                _ => {}
            }
        }
        None
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
