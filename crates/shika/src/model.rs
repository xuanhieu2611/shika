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
    /// The short form used by the summary chips.
    pub fn chip(self) -> &'static str {
        match self {
            Self::Waiting => "waiting",
            Self::Working => "working",
            Self::Ready => "ready",
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
/// Output this long after the user's last key, click, scroll, or resize is
/// the agent's own work. Sooner, it is an echo or a redraw.
pub const ECHO: Duration = Duration::from_secs(1);
/// Quiet this long after the agent's own output, the card is Ready to check.
pub const QUIET: Duration = Duration::from_secs(2);
/// Whether `acted` is the agent's own work. Output sooner than [`ECHO`] after
/// `input` is the terminal echoing a key or redrawing after focus, a click,
/// a scroll, or a resize.
pub fn is_agent_output(input: Option<Instant>, acted: Instant) -> bool {
    input.is_none_or(|input| acted >= input + ECHO)
}
/// Whether a card turning Ready should notify. One notification per turn: a
/// turn starts when the user types (`typed`), and `notified` is the `typed`
/// that already notified. The agent must also have kept going on its own,
/// past [`ECHO`], so typing a draft, focusing the terminal, or scrolling it
/// never notifies.
pub fn notify_ready(
    typed: Option<Instant>,
    notified: Option<Instant>,
    input: Option<Instant>,
    acted: Instant,
) -> bool {
    typed.is_some() && typed != notified && is_agent_output(input, acted)
}
/// Status and the instant it last changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TurnClock {
    pub status: Status,
    pub since: Instant,
}
/// Move Working and Ready from the agent's own output.
///
/// `agent_output` is the newest PTY bytes that [`is_agent_output`] accepted.
/// A draft's echo is not in there. While the echo window is still open the
/// clock holds: a Ready card stays Ready, and a Working card does not finish
/// its quiet timer, so a keystroke cannot hide output that is still arriving.
/// Once the window closes, only `agent_output` can start Working or postpone
/// Ready. `settled` is true when this call ends a turn at Ready, including a
/// turn that started and finished before the clock was painted.
pub fn advance_status(
    mut clock: TurnClock,
    input: Option<Instant>,
    agent_output: Option<Instant>,
    now: Instant,
    exited: bool,
) -> (TurnClock, bool) {
    if clock.status == Status::Waiting {
        return if exited {
            (
                TurnClock {
                    status: Status::Ready,
                    since: now,
                },
                false,
            )
        } else {
            (clock, false)
        };
    }
    let agent = agent_output.unwrap_or(clock.since);
    let echo_open = input.is_some_and(|input| now < input + ECHO);
    let mut worked = clock.status == Status::Working;
    if clock.status == Status::Ready && !echo_open && agent > clock.since {
        clock.status = Status::Working;
        clock.since = agent;
        worked = true;
    }
    if clock.status == Status::Working
        && (exited || (!echo_open && now.duration_since(agent.max(clock.since)) >= QUIET))
    {
        clock.status = Status::Ready;
        clock.since = now;
    }
    let settled = worked && clock.status == Status::Ready;
    (clock, settled)
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
/// `1 agent`, `2 agents`.
pub fn plural(n: usize, word: &str) -> String {
    if n == 1 {
        format!("1 {word}")
    } else {
        format!("{n} {word}s")
    }
}
/// The summary headline: every card, and the projects that have one.
pub fn headline(agents: usize, projects: usize) -> String {
    if agents == 0 {
        "No agents running".into()
    } else {
        format!(
            "{} across {}",
            plural(agents, "agent"),
            plural(projects, "project")
        )
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
    fn selection_reveal_moves_only_the_clipped_edge() {
        assert_eq!(reveal_delta(120., 180., 100., 400.), 0.);
        assert_eq!(reveal_delta(80., 140., 100., 400.), 20.);
        assert_eq!(reveal_delta(380., 440., 100., 400.), -40.);
        assert_eq!(reveal_delta(100., 400., 100., 400.), 0.);
        assert_eq!(reveal_delta(120., 520., 100., 400.), -20.);
        assert_eq!(reveal_delta(100., 500., 100., 400.), 0.);
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
    fn a_draft_does_not_start_or_hold_working() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        // Ready since 7s, after the agent output at 5s. At 10s the user types
        // and the terminal echoes. The card stays Ready through that window.
        let ready = TurnClock {
            status: Status::Ready,
            since: at(7_000),
        };
        let (clock, settled) =
            advance_status(ready, Some(at(10_000)), Some(at(5_000)), at(10_050), false);
        assert_eq!(clock, ready);
        assert!(!settled);
        let (clock, settled) =
            advance_status(ready, Some(at(10_000)), Some(at(5_000)), at(11_000), false);
        assert_eq!(clock.status, Status::Ready);
        assert!(!settled);
        // Real output after the window starts Working, then two quiet seconds
        // ends it.
        let ready = TurnClock {
            status: Status::Ready,
            since: at(0),
        };
        let (clock, settled) =
            advance_status(ready, Some(at(1_000)), Some(at(3_000)), at(3_200), false);
        assert_eq!(clock.status, Status::Working);
        assert_eq!(clock.since, at(3_000));
        assert!(!settled);
        let (clock, settled) =
            advance_status(clock, Some(at(1_000)), Some(at(3_000)), at(5_000), false);
        assert_eq!(clock.status, Status::Ready);
        assert!(settled);
        // Enter already set Working. Typing a draft holds that turn only
        // while the echo window is open, then the old agent output can settle.
        let working = TurnClock {
            status: Status::Working,
            since: at(0),
        };
        let (clock, settled) =
            advance_status(working, Some(at(3_000)), Some(at(1_000)), at(3_400), false);
        assert_eq!(clock.status, Status::Working);
        assert!(!settled);
        let (clock, settled) =
            advance_status(working, Some(at(3_000)), Some(at(1_000)), at(4_000), false);
        assert_eq!(clock.status, Status::Ready);
        assert!(settled);
        // Output that is still arriving after the draft keeps the card Working.
        let (clock, _) =
            advance_status(working, Some(at(3_000)), Some(at(4_200)), at(4_200), false);
        assert_eq!(clock.status, Status::Working);
    }
    #[test]
    fn summary_and_card_text() {
        assert_eq!(headline(0, 0), "No agents running");
        assert_eq!(headline(1, 1), "1 agent across 1 project");
        assert_eq!(headline(6, 2), "6 agents across 2 projects");
        assert_eq!(short_time(Duration::from_secs(42)), "42s");
        assert_eq!(short_time(Duration::from_secs(51 * 60 + 59)), "51m");
        assert_eq!(short_time(Duration::from_secs(63 * 60 + 5)), "1h 3m");
        assert_eq!(diff_stat_label(2, 64, 3), "2 files +64 \u{2212}3");
        assert_eq!(diff_stat_label(1, 0, 0), "1 file +0 \u{2212}0");
        let home = std::path::Path::new("/Users/kai");
        let path = std::path::Path::new("/Users/kai/code/shika");
        assert_eq!(tilde(path, Some(home)), "~/code/shika");
        assert_eq!(tilde(home, Some(home)), "~");
        assert_eq!(tilde(std::path::Path::new("/opt/x"), Some(home)), "/opt/x");
        assert_eq!(tilde(path, None), "/Users/kai/code/shika");
        // A sibling that only shares the prefix text is not under home.
        assert_eq!(
            tilde(std::path::Path::new("/Users/kaiser"), Some(home)),
            "/Users/kaiser"
        );
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
