//! A card in the column: what it launched, its panes, and its state.
use super::*;

/// What a card's launch starts. A retry repeats it.
#[derive(Clone)]
pub(crate) enum Launch {
    /// New from the picker: nothing typed yet.
    Task,
    Recover {
        path: PathBuf,
        resume: bool,
    },
    /// An agent in the author's repository root, with no owned git state.
    Checkout,
    /// `shika new`: the CLI starts with the Lead's prompt and the card is
    /// not selected, because the author is typing elsewhere.
    Worker(LaunchOptions),
    /// The project's Lead, started by the author.
    Lead(LeadLaunch),
    /// The project's Lead, started again by the app after its resume was
    /// refused. Not an author launch: it closes no overlay, takes no busy
    /// hold, and the selection and focus only follow the old Lead's.
    /// `focus`: the old Lead's terminal had focus.
    LeadRestart {
        launch: LeadLaunch,
        focus: bool,
    },
}

pub(crate) struct Card {
    pub(crate) session: Option<Session>,
    pub(crate) project: String,
    pub(crate) title: String,
    pub(crate) preset: String,
    pub(crate) status: Status,
    pub(crate) since: Instant,
    pub(crate) activity: activity::Activity,
    pub(crate) evidence: activity::OutputEvidence,
    pub(crate) lifecycle: lifecycle::Lifecycle,
    pub(crate) lifecycle_checked: Option<Instant>,
    /// Unbound host/identity pane for a chat card; never receives a PTY or CLI bytes.
    pub(crate) agent: Pane,
    pub(crate) chat: bool,
    pub(crate) chat_delivery: Option<control::ChatDelivery>,
    pub(crate) shells: Vec<Pane>,
    /// Zero is the pinned agent tab; shells use their index plus one.
    pub(crate) active_tab: usize,
    pub(crate) shell_serial: usize,
    pub(crate) tab_scroll: gpui::ScrollHandle,
    /// The tab under the pointer, which paints its hover in the tab shape.
    pub(crate) hovered_tab: Option<usize>,
    pub(crate) submitted: u64,
    pub(crate) creating: bool,
    pub(crate) title_watch: TitleWatch,
    /// The prompt or the CLI's title has named the card. Until then it reads
    /// "New <CLI>" in a lighter ink.
    pub(crate) named: bool,
    /// What the task changed, fetched each time the card turns Ready.
    pub(crate) diff: Option<DiffStat>,
    /// The checks on the PR that Create PR or a Lead's `shika pr` made or
    /// reused. Memory only.
    pub(crate) pr: Option<checks::PrWatch>,
    /// A Lead's `shika pr` is publishing this worker. Create PR, Close,
    /// Switch agent, and the Lead's own commands on it wait until it ends
    /// ([`control::LEAD_PUBLISHING`]). Memory only.
    pub(crate) lead_publishing: bool,
    /// The `since` of the Ready turn the user has seen. Every turn that ends
    /// gets a new `since`, so its dot returns until it is seen.
    pub(crate) seen: Option<Instant>,
    pub(crate) launch_preset: String,
    pub(crate) launch: Launch,
    /// The Lead session that started this worker.
    pub(crate) started_by: Option<String>,
    /// The control socket state, on the project's Lead card only.
    pub(crate) lead: Option<control::LeadState>,
    /// The `SHIKA_TOKEN` of a Lead-started worker's agent PTY. It may run
    /// only `shika report`, and dies with the card.
    pub(crate) worker_token: Option<String>,
    /// The worker's latest `shika report`. Memory only.
    pub(crate) report: Option<control::WorkerReport>,
    /// A failed launch nobody asked for (a worker): removed by the tick once
    /// no dialog depends on card positions.
    pub(crate) discard: bool,
    pub(crate) launch_control: Option<PreparationControl>,
    pub(crate) launch_error: Option<String>,
    pub(crate) stage: String,
    /// Close is stopping its terminals and removing its worktree: the card
    /// dims and says so, and its terminal fades behind "Closing...".
    pub(crate) closing: bool,
    /// Switch agent's replacement terminal while its process starts. The
    /// tick leaves the card alone meanwhile, so the old agent's exit is not
    /// read as a finished turn.
    pub(crate) next_agent: Option<Pane>,
    /// The CLI exit was already handed to the token read. A later Ready
    /// still reads on its own.
    pub(crate) usage_exit_seen: bool,
    /// What the app tracks for a Lead beyond the card, once it has started.
    pub(crate) lead_run: Option<LeadRun>,
}

/// The column group a card was shown in, by project id: the Leads section
/// (a Lead and its workers) or the project's own cards.
#[derive(Clone, PartialEq, Eq)]
pub(crate) enum Home {
    Leads(String),
    Project(String),
}

impl Card {
    pub(crate) fn is_checkout(&self) -> bool {
        matches!(self.launch, Launch::Checkout)
    }
    pub(crate) fn unseen(&self) -> bool {
        matches!(self.status, Status::Ready | Status::Asking) && self.seen != Some(self.since)
    }
    pub(crate) fn running(&self) -> bool {
        let host = lock(&self.agent.state);
        model::activity_requires_confirmation(
            self.status,
            host.exited,
            host.submission != self.submitted,
        )
    }
    /// Rename task is offered for a started task, never for a Lead.
    pub(crate) fn renamable(&self) -> bool {
        self.session.is_some() && self.lead.is_none()
    }
    pub(crate) fn active_pane(&self) -> &Pane {
        self.active_tab
            .checked_sub(1)
            .and_then(|index| self.shells.get(index))
            .unwrap_or(&self.agent)
    }
    /// Scrolls the active tab into view. The strip's children are the lead
    /// space, the tabs, and the trailing space, so the first and last tabs
    /// reveal the space that holds their outer flare.
    pub(crate) fn reveal_active_tab(&self) {
        let item = match self.active_tab {
            0 => 0,
            tab if tab == self.shells.len() => tab + 2,
            tab => tab + 1,
        };
        self.tab_scroll.scroll_to_item(item);
    }
}

impl Drop for Card {
    fn drop(&mut self) {
        if let Some(control) = &self.launch_control {
            control.cancel_preserving_worktree();
        }
    }
}
