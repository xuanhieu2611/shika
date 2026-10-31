//! A terminal pane and its PTY host: typed-input capture, startup typeahead,
//! and the agent PTY sink.
use super::*;

#[derive(Default)]
pub(crate) struct HostState {
    pub(crate) pty: Option<PtyId>,
    pub(crate) measured: Option<TerminalSize>,
    pub(crate) prompt: PromptCapture,
    pub(crate) submission_capture: PromptCapture,
    pub(crate) submission_recalled: bool,
    pub(crate) title: Option<String>,
    pub(crate) last_output: Option<Instant>,
    /// Focus/mouse reports and resize, separate from draft typing so real
    /// transcript changes are not suppressed while the user edits a prompt.
    pub(crate) last_interaction: Option<Instant>,
    /// The user's last key or paste.
    pub(crate) last_typed: Option<Instant>,
    pub(crate) exited: bool,
    pub(crate) submission: u64,
    pub(crate) last_submission: Option<Instant>,
    /// The latest submission is the prompt the CLI was launched with. The
    /// tick hands that to the activity clock once.
    pub(crate) launch_turn: bool,
    pub(crate) lifecycle: Option<shika_core::AgentActivity>,
    pub(crate) lifecycle_checking: bool,
    pub(crate) pending_input: Vec<Vec<u8>>,
    pub(crate) preparing: bool,
    pub(crate) agent_starting: bool,
    pub(crate) preparation_stage: String,
}

pub(crate) struct Host {
    pub(crate) core: Arc<Core>,
    pub(crate) state: Arc<Mutex<HostState>>,
    pub(crate) capture: bool,
}

impl PtyHost for Host {
    fn write(&self, bytes: &[u8], source: InputSource) {
        let mut s = lock(&self.state);
        // Setup is non-interactive. Never queue installer input and later send
        // it as an unintended prompt to the agent.
        if s.preparing && !(s.agent_starting && source == InputSource::Reply) {
            return;
        }
        let now = Instant::now();
        if source == InputSource::Typed {
            s.last_typed = Some(now);
        }
        if source == InputSource::Report {
            s.last_interaction = Some(now);
        }
        if self.capture && source == InputSource::Typed {
            s.capture_typed(bytes, now);
        }
        if let Some(pty) = s.pty {
            let _ = self.core.write(pty, bytes);
        } else {
            // Engine replies can arrive before Core has registered its PTY.
            // Keep their order, along with any input typed during startup.
            s.pending_input.push(bytes.to_vec());
        }
    }
    fn resize(&self, size: TerminalSize) {
        let mut s = lock(&self.state);
        s.measured = Some(size);
        let now = Instant::now();
        s.last_interaction = Some(now);
        if let Some(pty) = s.pty {
            let _ = self.core.resize(pty, PtySize::new(size.rows, size.cols));
        }
    }
}

impl HostState {
    /// Keep the agent's fixed title without seeding a turn. Leads call this
    /// before any startup typeahead or injected summary can be captured.
    pub(crate) fn keep_title(&mut self) {
        self.prompt.finish();
    }

    /// Feeds typed bytes to the naming and submission capture. A nonempty
    /// submitted line is a candidate turn; embedded paste newlines, reports,
    /// and query replies are not.
    pub(crate) fn capture_typed(&mut self, bytes: &[u8], now: Instant) {
        if let Some(title) = self.prompt.feed(bytes) {
            self.title = Some(title);
        }
        if matches!(bytes, b"\x1b[A" | b"\x1bOA" | b"\x1b[B" | b"\x1bOB") {
            // History lives inside the CLI editor, not in captured keys.
            // Treat a subsequent Enter as a candidate accepted prompt.
            self.submission_recalled = true;
        }
        if matches!(bytes, b"\x15" | b"\x03") {
            self.submission_recalled = false;
        }
        let recalled_enter = self.submission_recalled && matches!(bytes, b"\r" | b"\n" | b"\r\n");
        if self.submission_capture.feed(bytes).is_some() || recalled_enter {
            self.submission += 1;
            self.last_submission = Some(now);
            self.submission_capture = PromptCapture::default();
            self.submission_recalled = false;
        }
    }
    /// Typed input that has not been submitted: text on the line, or a
    /// recalled history entry. A CLI's grey suggestion is not typed, so it
    /// does not count.
    pub(crate) fn has_draft(&self) -> bool {
        self.submission_capture.has_text() || self.submission_recalled
    }
    /// Bytes are arrival metadata, not proof of work. The activity sampler
    /// separately compares live transcript content, excluding the editor.
    pub(crate) fn note_output(&mut self, now: Instant) {
        self.last_output = Some(now);
    }
    pub(crate) fn note_lifecycle(
        &mut self,
        generation: u64,
        report: Option<shika_core::AgentActivity>,
    ) {
        // A read scheduled for the old turn cannot inject its Idle into a
        // newly submitted prompt, even if the filesystem read was delayed.
        if self.submission == generation {
            self.lifecycle = report;
        }
        self.lifecycle_checking = false;
    }
}

/// Feeds an agent PTY into its terminal and records output and exit on its
/// host, for New and Switch agent alike.
pub(crate) fn agent_sink(
    terminal: Terminal,
    state: Arc<Mutex<HostState>>,
) -> impl shika_core::PtySink {
    move |_, event| match event {
        PtyEvent::Output(bytes) => {
            terminal.feed(&bytes);
            lock(&state).note_output(Instant::now());
        }
        PtyEvent::Exit(exit) => {
            terminal.feed(format!("\r\n[process exited with code {}]\r\n", exit.code).as_bytes());
            let mut state = lock(&state);
            state.last_output = Some(Instant::now());
            state.exited = true;
        }
    }
}

pub(crate) fn bind_host(core: &Core, state: &Mutex<HostState>, pty: PtyId) {
    let mut host = lock(state);
    host.pty = Some(pty);
    if let Some(size) = host.measured {
        let _ = core.resize(pty, PtySize::new(size.rows, size.cols));
    }
    for bytes in host.pending_input.drain(..) {
        let _ = core.write(pty, &bytes);
    }
}

pub(crate) struct Pane {
    pub(crate) shell_number: usize,
    pub(crate) view: Entity<TerminalView>,
    pub(crate) terminal: Terminal,
    pub(crate) state: Arc<Mutex<HostState>>,
}

impl Pane {
    pub(crate) fn new(
        core: Arc<Core>,
        capture: bool,
        opacity: f32,
        font_size: f32,
        palette: Palette,
        window: &mut Window,
        cx: &mut Context<Shika>,
    ) -> Self {
        let state = Arc::new(Mutex::new(HostState::default()));
        let terminal = Terminal::new(
            TerminalOptions {
                size: TerminalSize::new(2, 2),
                ..Default::default()
            },
            Host {
                core,
                state: state.clone(),
                capture,
            },
        );
        let view = cx.new(|cx| {
            let mut view = TerminalView::new(
                terminal.clone(),
                TerminalConfig {
                    font_size: px(font_size),
                    ..TerminalConfig::default()
                },
                palette,
                window,
                cx,
            );
            view.set_background_opacity(opacity, cx);
            view
        });
        cx.subscribe(&view, |this, _, event, cx| {
            if let TerminalEvent::OpenLink(uri) = event
                && let Err(text) = open_terminal_link(uri)
            {
                this.message(text);
                cx.notify();
            }
        })
        .detach();
        Self {
            shell_number: 0,
            view,
            terminal,
            state,
        }
    }
}

pub(crate) fn open_terminal_link(uri: &str) -> Result<(), String> {
    if !openable_uri(uri) {
        return Err("Couldn't open link".to_string());
    }
    let mut child = std::process::Command::new("/usr/bin/open")
        .arg(uri)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|_| "Couldn't open link".to_string())?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}
