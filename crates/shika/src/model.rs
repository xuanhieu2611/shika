//! Navigation, first-prompt capture, the base branch list, and the CLI title
//! watch, independent of GPUI.
use std::time::{Duration, Instant};

/// Agent is tab zero. Removing a shell preserves other selections and
/// selects the preceding tab when the active shell closes.
pub fn tab_after_close(active: usize, closed: usize, remaining_shells: usize) -> usize {
    if active >= closed {
        active.saturating_sub(1).min(remaining_shells)
    } else {
        active.min(remaining_shells)
    }
}

pub fn adjacent_tab(active: usize, shells: usize, forward: bool) -> usize {
    let count = shells + 1;
    if forward {
        (active + 1) % count
    } else {
        (active + count - 1) % count
    }
}

#[cfg(test)]
mod tab_tests {
    use super::*;

    #[test]
    fn closing_shells_preserves_selection_or_selects_neighbor() {
        assert_eq!(tab_after_close(0, 1, 2), 0);
        assert_eq!(tab_after_close(1, 3, 2), 1);
        assert_eq!(tab_after_close(3, 1, 2), 2);
        assert_eq!(tab_after_close(2, 2, 2), 1);
        assert_eq!(tab_after_close(3, 3, 2), 2);
        assert_eq!(tab_after_close(1, 1, 0), 0);
    }

    #[test]
    fn tab_navigation_wraps_and_handles_agent_only() {
        assert_eq!(adjacent_tab(0, 0, true), 0);
        assert_eq!(adjacent_tab(0, 0, false), 0);
        assert_eq!(adjacent_tab(0, 2, false), 2);
        assert_eq!(adjacent_tab(2, 2, true), 0);
        assert_eq!(adjacent_tab(1, 2, true), 2);
        assert_eq!(adjacent_tab(1, 2, false), 0);
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Waiting,
    Working,
    Asking,
    Ready,
}
impl Status {
    pub fn label(self) -> &'static str {
        match self {
            Self::Waiting => "Waiting",
            Self::Working => "Working",
            Self::Asking => "Asking you",
            Self::Ready => "Ready to check",
        }
    }
    pub fn rank(self) -> u8 {
        match self {
            Self::Asking => 0,
            Self::Ready => 1,
            Self::Working => 2,
            Self::Waiting => 3,
        }
    }
}
/// Both active work and a blocked turn must be explicitly stopped. A pending
/// submitted prompt is protected even before the next UI sampling tick.
pub fn activity_requires_confirmation(
    status: Status,
    exited: bool,
    pending_submission: bool,
) -> bool {
    !exited && (matches!(status, Status::Working | Status::Asking) || pending_submission)
}

/// Minimal scroll-offset adjustment that brings a selected row into view.
/// Oversized rows align their top, rather than oscillating between edges.
pub fn reveal_delta(top: f32, bottom: f32, viewport_top: f32, viewport_bottom: f32) -> f32 {
    if top < viewport_top || bottom - top > viewport_bottom - viewport_top {
        viewport_top - top
    } else if bottom > viewport_bottom {
        viewport_bottom - bottom
    } else {
        0.
    }
}

/// Longest base branch name the Base branch field takes.
pub const BRANCH_MAX: usize = 100;
/// Characters the Base branch field takes. Git refuses more names than this
/// lets through; core checks the rest.
pub fn branch_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '/' | '-' | '_' | '.')
}

/// Indices of `names` that start with the field text. `origin/dev` filters as
/// `dev`, the name the dialog saves. An empty field matches every name.
pub fn branch_matches(names: &[impl AsRef<str>], text: &str) -> Vec<usize> {
    let query = match text.strip_prefix("origin/") {
        Some(rest) if !rest.is_empty() => rest,
        _ => text,
    };
    names
        .iter()
        .enumerate()
        .filter(|(_, name)| name.as_ref().starts_with(query))
        .map(|(index, _)| index)
        .collect()
}

/// Highlight after the field changes: the first prefix match. An empty field
/// has none, so Enter keeps the default branch. No match has none, so Enter
/// fetches the typed name.
pub fn branch_highlight_after_type(match_count: usize, text: &str) -> Option<usize> {
    if text.is_empty() || match_count == 0 {
        None
    } else {
        Some(0)
    }
}

/// The branch Enter saves. A highlight wins. Otherwise the field, which is
/// empty for the default branch, or a name to fetch when nothing matches.
pub fn branch_to_apply<'a>(
    text: &'a str,
    matches: &[&'a str],
    highlight: Option<usize>,
) -> &'a str {
    highlight
        .and_then(|index| matches.get(index).copied())
        .unwrap_or(text)
}

/// Move through filtered rows. From no highlight, Down starts at the first
/// and Up at the last.
pub fn move_branch_highlight(current: Option<usize>, len: usize, delta: isize) -> Option<usize> {
    if len == 0 {
        return None;
    }
    let len = len as isize;
    let next = match current {
        Some(index) => index as isize + delta,
        None if delta < 0 => -1,
        None => 0,
    };
    Some(next.rem_euclid(len) as usize)
}
/// Exclusion window for changed screen content after focus/mouse reports or
/// resize. Draft typing is excluded structurally, not by this timer.
pub const ECHO: Duration = Duration::from_secs(1);
/// Unknown live chrome may settle a started turn after this much quiet.
pub const QUIET: Duration = Duration::from_secs(2);
/// Whether changed screen content is outside a geometry/report redraw window.
/// This is supporting evidence only, never proof of work or a new turn.
pub fn is_agent_output(input: Option<Instant>, acted: Instant) -> bool {
    input.is_none_or(|input| acted >= input + ECHO)
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
    cursor: usize,
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
                    match ch {
                        'D' => self.left(),
                        'C' => self.right(),
                        'H' => self.cursor = 0,
                        'F' => self.cursor = self.line.len(),
                        '~' if matches!(self.csi.as_str(), "1" | "7") => self.cursor = 0,
                        '~' if matches!(self.csi.as_str(), "4" | "8") => {
                            self.cursor = self.line.len()
                        }
                        '~' if self.csi == "3" => {
                            self.delete_forward();
                        }
                        _ => {}
                    }
                    self.escape = 0;
                } else {
                    self.csi.push(ch);
                }
                continue;
            }
            if self.escape == 3 {
                match ch {
                    'D' => self.left(),
                    'C' => self.right(),
                    'H' => self.cursor = 0,
                    'F' => self.cursor = self.line.len(),
                    _ => {}
                }
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
                '\r' | '\n' if self.paste => self.insert(' '),
                '\r' | '\n' => {
                    let title = self.line.split_whitespace().collect::<Vec<_>>().join(" ");
                    self.line.clear();
                    self.cursor = 0;
                    if !title.is_empty() {
                        self.done = true;
                        return Some(title);
                    }
                }
                '\u{7f}' | '\u{8}' => self.backspace(),
                '\u{1}' => self.cursor = 0,
                '\u{5}' => self.cursor = self.line.len(),
                '\u{2}' => self.left(),
                '\u{6}' => self.right(),
                '\u{4}' => self.delete_forward(),
                '\u{b}' => self.line.truncate(self.cursor),
                '\u{15}' => {
                    self.line.drain(..self.cursor);
                    self.cursor = 0;
                }
                '\u{3}' => {
                    self.line.clear();
                    self.cursor = 0;
                }
                '\u{17}' => self.delete_word(),
                ch if !ch.is_control() => self.insert(ch),
                _ => {}
            }
        }
        None
    }
    fn insert(&mut self, ch: char) {
        self.line.insert(self.cursor, ch);
        self.cursor += ch.len_utf8();
    }
    fn left(&mut self) {
        self.cursor = self.line[..self.cursor]
            .char_indices()
            .next_back()
            .map_or(0, |(i, _)| i);
    }
    fn right(&mut self) {
        if let Some(ch) = self.line[self.cursor..].chars().next() {
            self.cursor += ch.len_utf8();
        }
    }
    fn backspace(&mut self) {
        let end = self.cursor;
        self.left();
        self.line.drain(self.cursor..end);
    }
    fn delete_forward(&mut self) {
        if self.cursor < self.line.len() {
            self.line.remove(self.cursor);
        }
    }
    fn delete_word(&mut self) {
        while self.line[..self.cursor].ends_with(char::is_whitespace) {
            self.backspace();
        }
        while self.cursor > 0 && !self.line[..self.cursor].ends_with(char::is_whitespace) {
            self.backspace();
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
/// `1 agent`, `2 agents`.
pub fn plural(n: usize, word: &str) -> String {
    if n == 1 {
        format!("1 {word}")
    } else {
        format!("{n} {word}s")
    }
}
/// Time on a card: `42s`, `51m`, `1h 3m`.
pub fn short_time(elapsed: Duration) -> String {
    let seconds = elapsed.as_secs();
    if seconds < 60 {
        return format!("{seconds}s");
    }
    let minutes = seconds / 60;
    if minutes < 60 {
        format!("{minutes}m")
    } else {
        format!("{}h {}m", minutes / 60, minutes % 60)
    }
}
/// What a ready task changed: `2 files +64 −3`, with a real minus sign.
pub fn diff_stat_label(files: usize, insertions: usize, deletions: usize) -> String {
    format!(
        "{} +{insertions} \u{2212}{deletions}",
        plural(files, "file")
    )
}
/// A path with the home directory written as `~`.
pub fn tilde(path: &std::path::Path, home: Option<&std::path::Path>) -> String {
    match home.and_then(|home| path.strip_prefix(home).ok()) {
        Some(rest) if rest.as_os_str().is_empty() => "~".into(),
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}
/// How long a Ready card must stay selected in the active window before its
/// dot clears, so pressing `j` past a card does not count as reading it.
pub const SEEN_AFTER: Duration = Duration::from_secs(1);
/// Times how long one card has stayed on screen: selected, by session id,
/// while the window is active.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Dwell(Option<(String, Instant)>);
impl Dwell {
    /// Records the card on screen now, `None` when no card is selected or the
    /// window is inactive, and whether it has stayed for [`SEEN_AFTER`].
    pub fn observe(&mut self, card: Option<&str>, now: Instant) -> bool {
        if let (Some(id), Some((current, since))) = (card, &self.0)
            && id == current
        {
            return now.duration_since(*since) >= SEEN_AFTER;
        }
        self.0 = card.map(|id| (id.to_string(), now));
        false
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dwell_counts_only_one_card_on_screen_without_a_break() {
        let t = Instant::now();
        let mut dwell = Dwell::default();
        assert!(!dwell.observe(Some("a"), t));
        assert!(!dwell.observe(Some("a"), t + Duration::from_millis(900)));
        assert!(dwell.observe(Some("a"), t + SEEN_AFTER));
        // Passing over a card with `j` restarts the clock.
        assert!(!dwell.observe(Some("b"), t + Duration::from_millis(1100)));
        assert!(!dwell.observe(Some("a"), t + Duration::from_millis(1200)));
        // Leaving the window restarts it too.
        assert!(!dwell.observe(None, t + Duration::from_millis(2300)));
        assert!(!dwell.observe(Some("a"), t + Duration::from_millis(2400)));
        assert!(dwell.observe(Some("a"), t + Duration::from_millis(3400)));
    }

    #[test]
    fn selection_reveal_moves_only_the_clipped_edge() {
        assert_eq!(reveal_delta(120., 180., 100., 400.), 0.);
        assert_eq!(reveal_delta(80., 140., 100., 400.), 20.);
        assert_eq!(reveal_delta(380., 440., 100., 400.), -40.);
        assert_eq!(reveal_delta(100., 400., 100., 400.), 0.);
        assert_eq!(reveal_delta(120., 520., 100., 400.), -20.);
        assert_eq!(reveal_delta(100., 500., 100., 400.), 0.);
    }

    #[test]
    fn the_base_list_filters_by_prefix_and_resets_the_highlight() {
        let names = ["dev", "develop", "main", "staging"];
        assert_eq!(branch_matches(&names, ""), vec![0, 1, 2, 3]);
        assert_eq!(branch_matches(&names, "de"), vec![0, 1]);
        assert_eq!(branch_matches(&names, "main"), vec![2]);
        assert!(branch_matches(&names, "nope").is_empty());
        assert_eq!(branch_matches(&names, "origin/de"), vec![0, 1]);
        assert_eq!(branch_highlight_after_type(2, "de"), Some(0));
        assert_eq!(
            branch_highlight_after_type(branch_matches(&names, "").len(), ""),
            None
        );
        assert_eq!(branch_highlight_after_type(0, "nope"), None);
        let matches = branch_matches(&names, "de");
        let shown: Vec<&str> = matches.iter().map(|index| names[*index]).collect();
        assert_eq!(branch_to_apply("de", &shown, Some(0)), "dev");
        assert_eq!(branch_to_apply("de", &shown, Some(1)), "develop");
        assert_eq!(
            branch_to_apply(
                "dev",
                &shown,
                branch_highlight_after_type(shown.len(), "dev")
            ),
            "dev"
        );
        assert_eq!(branch_to_apply("nope", &[], None), "nope");
        assert_eq!(branch_to_apply("", &["dev", "main"], None), "");
        assert_eq!(move_branch_highlight(None, 3, 1), Some(0));
        assert_eq!(move_branch_highlight(None, 3, -1), Some(2));
        assert_eq!(move_branch_highlight(Some(0), 3, -1), Some(2));
        assert_eq!(move_branch_highlight(Some(1), 0, 1), None);
    }

    #[test]
    fn the_base_field_takes_branch_characters_only() {
        for ch in ['d', 'E', '7', '/', '-', '_', '.'] {
            assert!(branch_char(ch), "{ch}");
        }
        for ch in [' ', '~', '^', ':', '?', '*', '[', '\\', '@', 'é'] {
            assert!(!branch_char(ch), "{ch}");
        }
    }
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
    fn safe_close_protects_active_blocked_and_unprocessed_turns_not_drafts() {
        for status in [
            Status::Waiting,
            Status::Ready,
            Status::Working,
            Status::Asking,
        ] {
            assert!(!activity_requires_confirmation(status, true, true));
            assert!(activity_requires_confirmation(status, false, true));
            assert_eq!(
                activity_requires_confirmation(status, false, false),
                matches!(status, Status::Working | Status::Asking)
            );
        }
    }

    #[test]
    fn native_cursor_editing_and_clearing_do_not_submit_empty_drafts() {
        let mut capture = PromptCapture::default();
        assert!(capture.feed(b"draft\x01\x0b\r").is_none());
        assert!(capture.feed("界a".as_bytes()).is_none());
        assert!(capture.feed(b"\x1b[D\x7f").is_none());
        assert_eq!(capture.feed(b"b\r").as_deref(), Some("ba"));
    }

    #[test]
    fn card_text() {
        assert_eq!(short_time(Duration::from_secs(42)), "42s");
        assert_eq!(short_time(Duration::from_secs(51 * 60 + 59)), "51m");
        assert_eq!(short_time(Duration::from_secs(63 * 60 + 5)), "1h 3m");
        assert_eq!(diff_stat_label(2, 64, 3), "2 files +64 \u{2212}3");
        assert_eq!(diff_stat_label(1, 0, 0), "1 file +0 \u{2212}0");
        let home = std::path::Path::new("/Users/developer");
        let path = std::path::Path::new("/Users/developer/code/shika");
        assert_eq!(tilde(path, Some(home)), "~/code/shika");
        assert_eq!(tilde(home, Some(home)), "~");
        assert_eq!(tilde(std::path::Path::new("/opt/x"), Some(home)), "/opt/x");
        assert_eq!(tilde(path, None), "/Users/developer/code/shika");
        // A sibling that only shares the prefix text is not under home.
        assert_eq!(
            tilde(std::path::Path::new("/Users/developer-other"), Some(home)),
            "/Users/developer-other"
        );
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
