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

/// What the column's ordering needs to know about one card.
pub struct Placement<'a> {
    pub project: &'a str,
    /// [`Status::rank`]: lower sorts first.
    pub rank: u8,
    /// A Lead card.
    pub lead: bool,
    /// The card's own session id, once it has one.
    pub session: Option<&'a str>,
    /// The Lead session that started this card.
    pub started_by: Option<&'a str>,
}

/// A project whose Lead is saved but not running.
pub struct Resting<'a> {
    pub project: &'a str,
    /// The CLI's display name, for the row's second line.
    pub cli: &'a str,
}

/// What a row of the Leads section stands for. Entries are keyed by project
/// and carry their own source, so the walk order does not care which kind a
/// row is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LeadKind {
    /// A live Lead: an index into the cards.
    Running { card: usize },
    /// A saved Lead with no card and no PTY. Selecting it and pressing Enter
    /// resumes it. It has no workers: those of a stopped Lead are ordinary
    /// cards of their project.
    Resting { cli: String },
}

/// One Lead and the workers it started, which sit directly under it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LeadEntry {
    pub project: String,
    pub kind: LeadKind,
    /// Card indices, sorted by attention.
    pub workers: Vec<usize>,
}

impl LeadEntry {
    /// The entry's cards in walking order: the Lead, then its workers. A
    /// resting Lead has none.
    pub fn cards(&self) -> Vec<usize> {
        match self.kind {
            LeadKind::Running { card } => std::iter::once(card)
                .chain(self.workers.iter().copied())
                .collect(),
            LeadKind::Resting { .. } => Vec::new(),
        }
    }
}

/// A project's own cards, sorted by attention.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectEntry {
    pub project: String,
    pub cards: Vec<usize>,
}

/// The column: Leads first, then every project. A worker belongs to its
/// Lead's entry while that Lead is live; otherwise it is an ordinary card of
/// its project. A project with a saved Lead and no live one has a resting
/// entry, in project order with the live ones.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct ColumnLayout {
    pub leads: Vec<LeadEntry>,
    pub projects: Vec<ProjectEntry>,
}

impl ColumnLayout {
    pub fn new(projects: &[&str], cards: &[Placement], resting: &[Resting]) -> Self {
        let by_attention = |indices: &mut Vec<usize>| indices.sort_by_key(|&i| cards[i].rank);
        let mut leads = Vec::new();
        let mut led = vec![false; cards.len()];
        for project in projects {
            let before = leads.len();
            for (lead, card) in cards.iter().enumerate() {
                if !card.lead || card.project != *project {
                    continue;
                }
                let mut workers = match card.session {
                    Some(id) => (0..cards.len())
                        .filter(|&i| cards[i].started_by == Some(id))
                        .collect::<Vec<_>>(),
                    None => Vec::new(),
                };
                by_attention(&mut workers);
                for &worker in &workers {
                    led[worker] = true;
                }
                led[lead] = true;
                leads.push(LeadEntry {
                    project: (*project).to_string(),
                    kind: LeadKind::Running { card: lead },
                    workers,
                });
            }
            if leads.len() == before
                && let Some(saved) = resting.iter().find(|saved| saved.project == *project)
            {
                leads.push(LeadEntry {
                    project: (*project).to_string(),
                    kind: LeadKind::Resting {
                        cli: saved.cli.to_string(),
                    },
                    workers: Vec::new(),
                });
            }
        }
        let projects = projects
            .iter()
            .map(|project| {
                let mut owned = (0..cards.len())
                    .filter(|&i| !led[i] && cards[i].project == *project)
                    .collect::<Vec<_>>();
                by_attention(&mut owned);
                ProjectEntry {
                    project: (*project).to_string(),
                    cards: owned,
                }
            })
            .collect();
        Self { leads, projects }
    }
}

/// A resumed Lead that exits this soon after launch did not take its
/// conversation: the CLI refused the saved id.
pub const RESUME_GRACE: Duration = Duration::from_secs(5);

/// Whether a Lead launched on a saved conversation at `launched` and found
/// exited at `now` failed to resume. It is then started once more, fresh.
pub fn resume_failed(launched: Instant, now: Instant) -> bool {
    now.saturating_duration_since(launched) < RESUME_GRACE
}

/// What became of a resumed Lead that exited within [`RESUME_GRACE`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResumeExit {
    /// Core is being asked whether the CLI still stores the conversation.
    Checking,
    /// It is known to be gone: forget the id and start fresh, once.
    Missing,
    /// It exists or cannot be told: keep the id and let the Lead rest, so
    /// the author can try again or forget it.
    Kept,
}

impl ResumeExit {
    /// The answer of [`shika_core::Core::saved_lead_session_exists`].
    pub fn from_exists(exists: Option<bool>) -> Self {
        if exists == Some(false) {
            Self::Missing
        } else {
            Self::Kept
        }
    }
}

/// The toast when a resumed Lead's CLI quit at once but its conversation is
/// still there.
pub const RESUME_EXIT_KEPT: &str = "The Lead's CLI exited right after resuming. Select it to try again, or Forget it to start a new conversation.";

/// How often Codex's conversation id is looked for while it is unknown.
pub const DISCOVER_EVERY: Duration = Duration::from_secs(2);

/// The wait for a Lead's conversation id that its CLI picks (Codex: it exists
/// only after the first submitted message). At most one read is in flight and
/// nothing runs once the id is found, so an idle app has no poller.
pub struct Discovery {
    /// [`shika_core::LeadStart::started_at`]: only a newer conversation counts.
    pub since: std::time::SystemTime,
    next: Instant,
    reading: bool,
}

impl Discovery {
    pub fn new(since: std::time::SystemTime, now: Instant) -> Self {
        Self {
            since,
            next: now + DISCOVER_EVERY,
            reading: false,
        }
    }

    /// Whether a read should start now. Call [`Discovery::started`] when it
    /// does. `submitted`: the Lead has had its first submission; Codex creates
    /// its conversation only then, so an idle Lead is never polled.
    pub fn due(&self, now: Instant, submitted: bool) -> bool {
        submitted && !self.reading && now >= self.next
    }

    pub fn started(&mut self) {
        self.reading = true;
    }

    /// The read came back without an id (or failed): look again later.
    pub fn missed(&mut self, now: Instant) {
        self.reading = false;
        self.next = now + DISCOVER_EVERY;
    }
}

/// The one line typed into a restarted Lead when there is news: workers that
/// stopped with the app, or a checkout that was not moved. None when there is
/// nothing to say; an ordinary move to the latest base is not news. One line,
/// because it is a paste: nothing from Git's text may break it.
pub fn restart_summary(
    branches: &[String],
    worktree: &shika_core::LeadWorktree,
    base: &str,
) -> Option<String> {
    use shika_core::LeadWorktree;
    let base = if base.is_empty() { "base" } else { base };
    let mut parts = Vec::new();
    if !branches.is_empty() {
        let list = branches
            .iter()
            .map(|branch| format!("{} (worktree kept)", one_line(branch, 80)))
            .collect::<Vec<_>>()
            .join("; ");
        parts.push(format!(
            "Workers you started before are not running: {list}."
        ));
    }
    match worktree {
        LeadWorktree::KeptDirty => parts.push(format!(
            "Your checkout has local changes, so it was not moved to the latest {base}."
        )),
        LeadWorktree::KeptCommits => parts.push(format!(
            "Your checkout has commits that are on no branch, so it was not moved to the latest {base}."
        )),
        LeadWorktree::RefreshFailed(why) => parts.push(format!(
            "Your checkout could not be moved to the latest {base}: {}.",
            one_line(why, 100).trim_end_matches('.')
        )),
        _ => {}
    }
    if parts.is_empty() {
        return None;
    }
    Some(format!(
        "[shika] Shika restarted. {} Tell the author briefly and ask how to continue.",
        parts.join(" ")
    ))
}

/// `text` on one line: the first non-empty line, control characters dropped,
/// cut to `max` characters.
fn one_line(text: &str, max: usize) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("");
    let clean: String = line.chars().filter(|c| !c.is_control()).collect();
    if clean.chars().count() > max {
        format!(
            "{}...",
            clean.chars().take(max - 3).collect::<String>().trim_end()
        )
    } else {
        clean
    }
}

/// What the Lead's close dialog says. Stop keeps the conversation; Forget
/// deletes the worktree and the link to it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LeadClose {
    /// A live Lead. `working`: a turn is active or blocked on a dialog.
    /// `loss`: what its worktree holds that only Forget discards.
    Stop {
        working: bool,
        loss: shika_core::LeadLoss,
    },
    /// `working` is carried from the Stop step it came from.
    Forget {
        working: bool,
        loss: shika_core::LeadLoss,
    },
}

impl LeadClose {
    pub fn title(&self) -> &'static str {
        match self {
            Self::Stop { .. } => "Stop the Lead?",
            Self::Forget { .. } => "Forget the Lead?",
        }
    }

    /// The dialog's paragraphs, in order.
    pub fn facts(&self) -> Vec<&'static str> {
        match *self {
            Self::Stop { working, .. } => {
                let mut facts = Vec::new();
                if working {
                    facts.push("The agent is still working.");
                }
                facts.push(
                    "Its conversation is kept. Select it to resume. Workers it started stay as your tasks.",
                );
                facts
            }
            Self::Forget { working, loss } => {
                let mut facts = Vec::new();
                if working {
                    facts.push("The agent is still working.");
                }
                facts.push(
                    "Forget deletes the Lead's worktree and Shika's link to its conversation. Its Shika chat history will be deleted. The CLI keeps its own history. Its memory notes stay for the next Lead.",
                );
                if loss.uncommitted {
                    facts.push("Its worktree has uncommitted changes, which will be discarded.");
                }
                if loss.unreferenced_commits {
                    facts.push(
                        "Its worktree has commits that are on no branch, which will be lost.",
                    );
                }
                facts
            }
        }
    }

    /// The step Forget moves a Stop dialog to.
    pub fn forget(&self) -> Self {
        match *self {
            Self::Stop { working, loss } | Self::Forget { working, loss } => {
                Self::Forget { working, loss }
            }
        }
    }

    /// Whether Forget needs the author's agreement to lose something.
    pub fn discards(&self) -> bool {
        match *self {
            Self::Stop { loss, .. } | Self::Forget { loss, .. } => loss.any(),
        }
    }
}

#[cfg(test)]
mod layout_tests {
    use super::*;

    fn task(project: &str, rank: u8) -> Placement<'_> {
        Placement {
            project,
            rank,
            lead: false,
            session: None,
            started_by: None,
        }
    }
    fn lead<'a>(project: &'a str, session: Option<&'a str>, rank: u8) -> Placement<'a> {
        Placement {
            project,
            rank,
            lead: true,
            session,
            started_by: None,
        }
    }
    fn worker<'a>(project: &'a str, by: &'a str, rank: u8) -> Placement<'a> {
        Placement {
            project,
            rank,
            lead: false,
            session: None,
            started_by: Some(by),
        }
    }

    #[test]
    fn without_a_lead_projects_sort_by_attention() {
        let cards = [task("a", 3), task("b", 0), task("a", 1)];
        let layout = ColumnLayout::new(&["a", "b"], &cards, &[]);
        assert!(layout.leads.is_empty());
        assert_eq!(layout.projects[0].cards, vec![2, 0]);
        assert_eq!(layout.projects[1].cards, vec![1]);
    }

    #[test]
    fn leads_come_first_with_their_workers_under_them() {
        let cards = [
            task("a", 0),
            worker("b", "lb", 3),
            lead("b", Some("lb"), 2),
            worker("b", "lb", 1),
            lead("a", Some("la"), 3),
            worker("a", "la", 2),
            task("b", 0),
        ];
        let layout = ColumnLayout::new(&["a", "b"], &cards, &[]);
        assert_eq!(layout.leads.len(), 2);
        assert_eq!(layout.leads[0].project, "a");
        assert_eq!(layout.leads[0].kind, LeadKind::Running { card: 4 });
        assert_eq!(layout.leads[0].workers, vec![5]);
        assert_eq!(layout.leads[1].kind, LeadKind::Running { card: 2 });
        assert_eq!(layout.leads[1].workers, vec![3, 1]);
        assert_eq!(layout.projects[0].cards, vec![0]);
        assert_eq!(layout.projects[1].cards, vec![6]);
        assert_eq!(layout.leads[0].cards(), vec![4, 5]);
        assert_eq!(layout.leads[1].cards(), vec![2, 3, 1]);
    }

    #[test]
    fn workers_of_a_closed_lead_become_ordinary_cards() {
        let cards = [worker("a", "gone", 2), task("a", 1)];
        let layout = ColumnLayout::new(&["a"], &cards, &[]);
        assert!(layout.leads.is_empty());
        assert_eq!(layout.projects[0].cards, vec![1, 0]);
    }

    #[test]
    fn a_lead_still_starting_has_a_row_and_no_workers() {
        let cards = [lead("a", None, 3), task("a", 0)];
        let layout = ColumnLayout::new(&["a"], &cards, &[]);
        assert!(layout.leads[0].workers.is_empty());
        assert_eq!(layout.projects[0].cards, vec![1]);
        assert_eq!(layout.leads[0].cards(), vec![0]);
    }

    #[test]
    fn a_saved_lead_rests_in_project_order_unless_one_is_live() {
        let cards = [lead("b", Some("lb"), 2), worker("a", "gone", 1)];
        let saved = [
            Resting {
                project: "a",
                cli: "Codex",
            },
            Resting {
                project: "b",
                cli: "Claude Code",
            },
            Resting {
                project: "c",
                cli: "Pi",
            },
        ];
        let layout = ColumnLayout::new(&["a", "b", "c"], &cards, &saved);
        let kinds: Vec<_> = layout
            .leads
            .iter()
            .map(|entry| (entry.project.as_str(), entry.kind.clone()))
            .collect();
        assert_eq!(
            kinds,
            vec![
                (
                    "a",
                    LeadKind::Resting {
                        cli: "Codex".into()
                    }
                ),
                ("b", LeadKind::Running { card: 0 }),
                ("c", LeadKind::Resting { cli: "Pi".into() }),
            ]
        );
        // A resting Lead has no cards and no workers: the stopped Lead's
        // worker is an ordinary card of its project.
        assert!(layout.leads[0].cards().is_empty());
        assert_eq!(layout.projects[0].cards, vec![1]);
    }

    #[test]
    fn an_empty_project_keeps_its_entry() {
        let cards = [lead("a", Some("la"), 3)];
        let layout = ColumnLayout::new(&["a"], &cards, &[]);
        assert!(layout.projects[0].cards.is_empty());
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
    /// The first prompt is already known, as when the CLI was launched with
    /// it. Typed lines no longer name the task.
    pub fn finish(&mut self) {
        self.done = true;
    }
    /// Text is typed on the line and not yet submitted.
    pub fn has_text(&self) -> bool {
        !self.line.is_empty()
    }
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
/// What a typed first line would have given the card: the first nonblank
/// line of a launch prompt, with its whitespace collapsed.
pub fn prompt_title(prompt: &str) -> Option<String> {
    prompt
        .lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .find(|line| !line.is_empty())
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
/// The notification body when a PR's checks fail.
pub const CHECKS_FAILED: &str = "Checks failed";

/// The tooltip on a card's PR mark. The click opens the PR.
pub fn checks_tip(mark: crate::checks::Mark) -> &'static str {
    use crate::checks::Mark;
    match mark {
        Mark::Pending => "Checks running",
        Mark::Passed => "Checks passed",
        Mark::Failed => CHECKS_FAILED,
        Mark::None => "Open PR",
    }
}

/// What a ready task changed: `2 files +64 −3`, with a real minus sign.
pub fn diff_stat_label(files: usize, insertions: usize, deletions: usize) -> String {
    let (files, added, removed) = diff_stat_parts(files, insertions, deletions);
    format!("{files} {added} {removed}")
}

/// The pieces of [`diff_stat_label`], for a card that colors the counts:
/// (`2 files`, `+64`, `−3`).
pub fn diff_stat_parts(
    files: usize,
    insertions: usize,
    deletions: usize,
) -> (String, String, String) {
    (
        plural(files, "file"),
        format!("+{insertions}"),
        format!("\u{2212}{deletions}"),
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
/// The design's motion curve, `cubic-bezier(.2,.8,.2,1)`, at progress `t`
/// from 0 to 1.
pub fn ease(t: f32) -> f32 {
    // Exact ends, so an animation starts and settles exactly where it should.
    if t <= 0. {
        return 0.;
    }
    if t >= 1. {
        return 1.;
    }
    let curve = |p1: f32, p2: f32, s: f32| {
        let r = 1. - s;
        3. * r * r * s * p1 + 3. * r * s * s * p2 + s * s * s
    };
    // x rises from 0 to 1 along the curve, so halving finds the point at x = t.
    let (mut lo, mut hi) = (0f32, 1f32);
    for _ in 0..24 {
        let mid = (lo + hi) / 2.;
        if curve(0.2, 0.2, mid) < t {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    curve(0.8, 1., (lo + hi) / 2.)
}
/// A closing task's card and terminal fade to their closing look.
pub const CLOSING_FADE: Duration = Duration::from_millis(180);
/// A closed card shrinks and fades out, then its row collapses.
pub const EXIT_SHRINK: Duration = Duration::from_millis(160);
pub const EXIT_COLLAPSE: Duration = Duration::from_millis(220);
pub const EXIT: Duration = Duration::from_millis(160 + 220);
/// A closed card's exit at `t` of [`EXIT`], 0 to 1: how far the card has
/// shrunk and faded, then how far its row has collapsed, both eased. The row
/// starts to collapse once the card is gone.
pub fn card_exit(t: f32) -> (f32, f32) {
    let ms = t.clamp(0., 1.) * EXIT.as_millis() as f32;
    let shrink = ms / EXIT_SHRINK.as_millis() as f32;
    let collapse = (ms - EXIT_SHRINK.as_millis() as f32) / EXIT_COLLAPSE.as_millis() as f32;
    (ease(shrink), ease(collapse))
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
    fn ease_runs_from_zero_to_one_and_front_loads_the_motion() {
        assert_eq!(ease(0.), 0.);
        assert!((ease(1.) - 1.).abs() < 1e-4);
        assert_eq!(ease(-1.), 0.);
        assert!((ease(2.) - 1.).abs() < 1e-4);
        let mut last = 0.;
        for i in 1..=100 {
            let y = ease(i as f32 / 100.);
            assert!(y >= last, "ease falls at {i}");
            last = y;
        }
        // An ease-out: most of the distance is covered early.
        assert!(ease(0.25) > 0.6);
    }

    #[test]
    fn card_exit_shrinks_before_the_row_collapses() {
        assert_eq!(card_exit(0.), (0., 0.));
        let shrunk = EXIT_SHRINK.as_secs_f32() / EXIT.as_secs_f32();
        let (shrink, collapse) = card_exit(shrunk);
        assert!((shrink - 1.).abs() < 1e-4);
        assert_eq!(collapse, 0.);
        let (shrink, collapse) = card_exit(1.);
        assert!((shrink - 1.).abs() < 1e-4);
        assert!((collapse - 1.).abs() < 1e-4);
    }

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

#[cfg(test)]
mod lead_tests {
    use super::*;
    use shika_core::LeadWorktree;

    fn names(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| name.to_string()).collect()
    }

    #[test]
    fn the_restart_summary_names_only_what_is_news() {
        let moved = LeadWorktree::Refreshed {
            from: "a1".into(),
            to: "b2".into(),
        };
        for tree in [LeadWorktree::Created, LeadWorktree::Unchanged, moved] {
            assert_eq!(restart_summary(&[], &tree, "origin/main"), None);
        }
        assert_eq!(
            restart_summary(
                &names(&["fix-a", "fix-b"]),
                &LeadWorktree::Unchanged,
                "origin/main"
            )
            .as_deref(),
            Some(
                "[shika] Shika restarted. Workers you started before are not running: fix-a (worktree kept); fix-b (worktree kept). Tell the author briefly and ask how to continue."
            )
        );
        assert_eq!(
            restart_summary(&[], &LeadWorktree::KeptDirty, "origin/dev").as_deref(),
            Some(
                "[shika] Shika restarted. Your checkout has local changes, so it was not moved to the latest origin/dev. Tell the author briefly and ask how to continue."
            )
        );
        let both =
            restart_summary(&names(&["fix-a"]), &LeadWorktree::KeptDirty, "origin/main").unwrap();
        assert!(both.contains("fix-a (worktree kept). Your checkout has local changes"));
    }

    #[test]
    fn a_failed_move_is_one_short_line() {
        let why = "fatal: unable to read tree\n  second line\u{7}\n";
        let line =
            restart_summary(&[], &LeadWorktree::RefreshFailed(why.into()), "origin/main").unwrap();
        assert!(
            !line.contains('\n') && !line.chars().any(char::is_control),
            "{line}"
        );
        assert!(
            line.contains(
                "could not be moved to the latest origin/main: fatal: unable to read tree."
            ),
            "{line}"
        );
        assert!(!line.contains("second line"));
        let long = "x".repeat(500);
        let line = restart_summary(&[], &LeadWorktree::RefreshFailed(long), "").unwrap();
        assert!(line.len() < 300 && line.contains("latest base:"), "{line}");
    }

    #[test]
    fn a_checkout_with_unreferenced_commits_is_named_in_the_restart_summary() {
        let line = restart_summary(&[], &LeadWorktree::KeptCommits, "origin/main").unwrap();
        assert!(line.contains(
            "Your checkout has commits that are on no branch, so it was not moved to the latest origin/main."
        ));
        assert!(!line.contains('\n'));
    }

    #[test]
    fn a_resume_that_exits_within_five_seconds_failed() {
        let t0 = Instant::now();
        assert!(resume_failed(t0, t0 + Duration::from_millis(900)));
        assert!(resume_failed(t0, t0 + Duration::from_millis(4900)));
        assert!(!resume_failed(t0, t0 + RESUME_GRACE));
        assert!(!resume_failed(t0, t0 + Duration::from_secs(60)));
    }

    #[test]
    fn discovery_reads_every_two_seconds_one_at_a_time() {
        let t0 = Instant::now();
        let mut find = Discovery::new(std::time::SystemTime::now(), t0);
        assert!(!find.due(t0 + Duration::from_millis(1900), true));
        assert!(find.due(t0 + DISCOVER_EVERY, true));
        find.started();
        assert!(!find.due(t0 + Duration::from_secs(30), true));
        let later = t0 + Duration::from_secs(3);
        find.missed(later);
        assert!(!find.due(later + Duration::from_millis(1900), true));
        assert!(find.due(later + DISCOVER_EVERY, true));
    }

    #[test]
    fn the_lead_close_dialog_stops_first_and_forgets_on_request() {
        let loss = shika_core::LeadLoss {
            uncommitted: true,
            unreferenced_commits: false,
        };
        let stop = LeadClose::Stop {
            working: false,
            loss,
        };
        assert_eq!(stop.title(), "Stop the Lead?");
        assert_eq!(
            stop.facts(),
            vec![
                "Its conversation is kept. Select it to resume. Workers it started stay as your tasks."
            ]
        );
        let busy = LeadClose::Stop {
            working: true,
            loss: Default::default(),
        };
        assert_eq!(busy.facts()[0], "The agent is still working.");
        assert_eq!(busy.facts().len(), 2);

        let forget = stop.forget();
        assert_eq!(
            forget,
            LeadClose::Forget {
                working: false,
                loss
            }
        );
        assert_eq!(forget.title(), "Forget the Lead?");
        assert_eq!(
            forget.facts(),
            vec![
                "Forget deletes the Lead's worktree and Shika's link to its conversation. Its Shika chat history will be deleted. The CLI keeps its own history. Its memory notes stay for the next Lead.",
                "Its worktree has uncommitted changes, which will be discarded.",
            ]
        );
        assert!(forget.discards());
        let clean = LeadClose::Forget {
            working: false,
            loss: Default::default(),
        };
        assert_eq!(clean.facts().len(), 1);
        assert!(!clean.discards());
        assert_eq!(forget.forget(), forget);
    }

    #[test]
    fn the_forget_step_keeps_the_working_line_and_names_unreferenced_commits() {
        let step = LeadClose::Stop {
            working: true,
            loss: shika_core::LeadLoss {
                uncommitted: true,
                unreferenced_commits: true,
            },
        }
        .forget();
        let facts = step.facts();
        assert_eq!(facts[0], "The agent is still working.");
        assert_eq!(facts.len(), 4);
        assert_eq!(
            facts[3],
            "Its worktree has commits that are on no branch, which will be lost."
        );
        let commits_only = LeadClose::Forget {
            working: false,
            loss: shika_core::LeadLoss {
                uncommitted: false,
                unreferenced_commits: true,
            },
        };
        assert!(commits_only.discards());
        assert_eq!(commits_only.facts().len(), 2);
    }

    #[test]
    fn an_early_exit_restarts_only_when_the_conversation_is_known_missing() {
        assert_eq!(ResumeExit::from_exists(Some(false)), ResumeExit::Missing);
        assert_eq!(ResumeExit::from_exists(Some(true)), ResumeExit::Kept);
        assert_eq!(ResumeExit::from_exists(None), ResumeExit::Kept);
        assert!(RESUME_EXIT_KEPT.contains("Select it to try again"));
    }

    #[test]
    fn discovery_waits_for_the_first_submission() {
        let t0 = Instant::now();
        let find = Discovery::new(std::time::SystemTime::now(), t0);
        assert!(!find.due(t0 + Duration::from_secs(600), false));
        assert!(find.due(t0 + Duration::from_secs(600), true));
    }
}
