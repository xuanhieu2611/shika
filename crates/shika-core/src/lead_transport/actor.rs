//! The Lead chat actor: the one thread that owns the CLI process, writes the
//! frames its protocol builds, acts on what it decodes, and persists the turns.
use std::io::{BufRead, BufReader, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;

use super::protocol::{Interrupt, LeadProtocol, Shutdown};
use super::{
    ChatEvent, ChatLaunch, ChatStatus, CommandIn, INCOMPLETE, MAX_FRAME, Parsed, UNSUPPORTED,
};
use crate::lead_chat_store::{ChatContent, ChatOrigin, ChatResult, ChatStore};

pub(super) struct Process {
    child: Child,
    /// None once closed for a `Shutdown::CloseStdin`.
    stdin: Option<ChildStdin>,
    reader_done: bool,
    question: Option<String>,
    input_epoch: u64,
    started: Instant,
}
impl Process {
    pub(super) fn spawn(
        launch: &ChatLaunch,
        protocol: &mut dyn LeadProtocol,
        generation: u64,
        input: mpsc::SyncSender<CommandIn>,
    ) -> ChatResult<Self> {
        let strings = protocol.strings();
        let mut cmd = Command::new(&launch.program);
        cmd.args(protocol.args(launch))
            .current_dir(&launch.cwd)
            .env("PATH", &launch.path)
            .env("PWD", &launch.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        for key in crate::worktree::GIT_REDIRECTS
            .into_iter()
            .chain(crate::pty::CONTROL_VARS)
            .chain(protocol.env_remove().iter().copied())
        {
            cmd.env_remove(key);
        }
        for (k, v) in &launch.env {
            cmd.env(k, v);
        }
        // Spawning keeps the caller's signal mask, and the app's threads
        // block SIGINT and SIGTERM: inherited, it made a chat CLI deaf to
        // every stop signal but SIGKILL. Start it as a worker PTY starts.
        // SAFETY: only async-signal-safe calls between fork and exec.
        unsafe {
            cmd.pre_exec(reset_signals);
        }
        let mut child = cmd.spawn().map_err(|_| strings.start_failed.to_string())?;
        let mut stdin = child.stdin.take().expect("piped stdin");
        let flags = unsafe { libc::fcntl(stdin.as_raw_fd(), libc::F_GETFL) };
        if flags < 0
            || unsafe { libc::fcntl(stdin.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) }
                < 0
        {
            unsafe {
                libc::kill(-(child.id() as i32), libc::SIGKILL);
            }
            let _ = child.wait();
            return Err(strings.input_failed.into());
        }
        let handshake = protocol.handshake(launch);
        if stdin
            .write_all(&handshake)
            .and_then(|_| stdin.flush())
            .is_err()
        {
            let _ = child.kill();
            let _ = child.wait();
            return Err(strings.handshake_failed.into());
        }
        let stdout = child.stdout.take().expect("piped stdout");
        let mut stderr = child.stderr.take().expect("piped stderr");
        thread::spawn(move || {
            let _ = std::io::copy(&mut stderr, &mut std::io::sink());
        });
        let skippable = protocol.oversized_skippable();
        thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let mut line = Vec::new();
                let read = (&mut reader)
                    .take(MAX_FRAME + 1)
                    .read_until(b'\n', &mut line);
                match read {
                    Ok(0) => break,
                    Ok(_) if line.len() as u64 <= MAX_FRAME && line.ends_with(b"\n") => {
                        if input.send(CommandIn::Line(generation, line)).is_err() {
                            break;
                        }
                    }
                    // A record the protocol never reads: drop the rest of
                    // the line, which is all the protocol would have done.
                    Ok(_) if !line.ends_with(b"\n") && skippable(&line) => {
                        if !matches!(reader.skip_until(b'\n'), Ok(n) if n > 0) {
                            break;
                        }
                    }
                    _ => {
                        let _ = input.send(CommandIn::BadFrame(generation));
                        let _ = std::io::copy(&mut reader, &mut std::io::sink());
                        break;
                    }
                }
            }
            let _ = input.send(CommandIn::Eof(generation));
        });
        Ok(Self {
            child,
            stdin: Some(stdin),
            reader_done: false,
            question: None,
            input_epoch: 0,
            started: Instant::now(),
        })
    }
    fn write(&mut self, bytes: &[u8], cancel: &AtomicU64, epoch: u64) -> std::io::Result<()> {
        match &mut self.stdin {
            Some(stdin) => write_frame(stdin, bytes, cancel, epoch),
            None => Err(std::io::ErrorKind::BrokenPipe.into()),
        }
    }
    fn signal(&mut self, signal: i32) {
        // The child is unreaped here, so its process group cannot be reused.
        unsafe {
            libc::kill(-(self.child.id() as i32), signal);
        }
    }
}
impl Drop for Process {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            self.signal(libc::SIGKILL);
            let _ = self.child.wait();
        }
    }
}

/// Default dispositions and an empty signal mask for the child, as
/// `portable-pty` gives worker PTYs.
fn reset_signals() -> std::io::Result<()> {
    for signal in [
        libc::SIGCHLD,
        libc::SIGHUP,
        libc::SIGINT,
        libc::SIGQUIT,
        libc::SIGTERM,
        libc::SIGALRM,
    ] {
        // SAFETY: a zeroed `sigaction` with SIG_DFL is a valid argument.
        unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = libc::SIG_DFL;
            libc::sigaction(signal, &action, std::ptr::null_mut());
        }
    }
    // SAFETY: an empty set from `sigemptyset` unblocks every signal.
    unsafe {
        let mut empty: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut empty);
        if libc::sigprocmask(libc::SIG_SETMASK, &empty, std::ptr::null_mut()) != 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

/// Nonblocking pipe IO lets Stop interrupt a child that stopped reading stdin.
/// Any partial/failed delivery is never replayed automatically.
fn write_frame(
    stdin: &mut ChildStdin,
    bytes: &[u8],
    cancel: &AtomicU64,
    epoch: u64,
) -> std::io::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut offset = 0;
    while offset < bytes.len() {
        if cancel.load(Ordering::Acquire) != epoch {
            return Err(std::io::ErrorKind::Interrupted.into());
        }
        match stdin.write(&bytes[offset..]) {
            Ok(0) => return Err(std::io::ErrorKind::WriteZero.into()),
            Ok(count) => offset += count,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    return Err(std::io::ErrorKind::TimedOut.into());
                }
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    stdin.flush()
}
/// What the actor loop does after handling one input.
enum Flow {
    /// Go on with this round.
    Continue,
    /// Start the next round at once, skipping this round's exit check.
    NextRound,
    /// Leave the loop: the actor is done.
    Exit,
}
pub(super) struct Actor {
    pub(super) stop: Arc<AtomicBool>,
    /// Set on `Parsed::Missing`; read by `ChatTransport::conversation_missing`.
    pub(super) missing: Arc<AtomicBool>,
    pub(super) cancel_seen: u64,
    pub(super) cancel: Arc<AtomicU64>,
    pub(super) launch: ChatLaunch,
    pub(super) protocol: Box<dyn LeadProtocol>,
    pub(super) store: ChatStore,
    pub(super) output: mpsc::Sender<ChatEvent>,
    pub(super) frames: mpsc::SyncSender<CommandIn>,
    pub(super) child: Option<Process>,
    pub(super) generation: u64,
    pub(super) next_turn: i64,
    pub(super) turn: Option<i64>,
    pub(super) stopping: Option<Instant>,
    /// The stop in progress is an `Interrupt::Frame`: the process stays, and
    /// its lines are still read until the turn ends.
    pub(super) soft_stop: bool,
    /// The stop in progress sent `LeadProtocol::stop_signal`; SIGKILL is next.
    pub(super) signalled: bool,
    /// The turn's frame was written; the CLI has not acknowledged it yet.
    pub(super) unacknowledged: bool,
    pub(super) exiting: bool,
    pub(super) save_failed: bool,
    pub(super) pending: Vec<(i64, ChatContent)>,
    pub(super) active_input: Option<(ChatOrigin, String)>,
    pub(super) failed: bool,
}
impl Actor {
    fn emit(&self, event: ChatEvent) {
        if let Some(source) = &self.launch.usage {
            source.notify();
        }
        let _ = self.output.send(event);
    }
    /// Counted here too, so rows buffered after a save failure keep their
    /// numbers when a turn the CLI starts itself needs the next one.
    fn allocate_turn(&mut self) -> i64 {
        let turn = self.store.next_turn().unwrap_or(0).max(self.next_turn);
        self.next_turn = turn + 1;
        turn
    }
    fn persist(&mut self, turn: i64, content: ChatContent) -> bool {
        self.persist_batch(vec![(turn, content)])
    }
    fn persist_batch(&mut self, records: Vec<(i64, ChatContent)>) -> bool {
        if self.save_failed {
            self.pending.extend(records);
            return false;
        }
        match self.store.append_batch(records.clone()) {
            Ok(records) => {
                for record in records {
                    if record.content.text().is_some() {
                        self.emit(ChatEvent::Record(record));
                    }
                }
                true
            }
            Err(_) => {
                self.pending.extend(records);
                self.save_failed = true;
                self.emit(ChatEvent::SaveFailed);
                false
            }
        }
    }
    fn settle(&mut self, error: Option<&str>) {
        if self.turn.is_none()
            && let Some(text) = error
        {
            self.persist(
                0,
                ChatContent::Event {
                    text: text.into(),
                    key: None,
                },
            );
        }
        self.complete(None, error);
    }
    fn complete(&mut self, text: Option<String>, error: Option<&str>) {
        let input = self.active_input.take();
        self.unacknowledged = false;
        if let Some(turn) = self.turn.take() {
            let mut records = Vec::new();
            if let Some(text) = text {
                records.push((turn, ChatContent::Lead { text }));
            }
            if let Some(text) = error {
                // A turn the CLI started has no input to offer again.
                if let Some((origin, input)) = input {
                    if text != "Reply stopped" && origin == ChatOrigin::Author {
                        self.emit(ChatEvent::Rejected(input));
                    }
                    self.emit(ChatEvent::DeliveryFailed);
                }
                records.push((
                    turn,
                    ChatContent::Event {
                        text: text.into(),
                        key: None,
                    },
                ));
            }
            records.push((turn, ChatContent::Settled));
            self.persist_batch(records);
            self.emit(ChatEvent::Status(ChatStatus::Ready));
        }
    }
    /// Stop generating. After a failure that ends the transport, the process
    /// goes too.
    fn interrupt(&mut self) {
        self.stop_turn(self.failed);
    }
    /// Stops the turn the protocol's way, or with the protocol's stop signal
    /// to the group when `hard` or the frame cannot be written. A hard stop
    /// also ends a soft stop in progress, with a new kill deadline.
    fn stop_turn(&mut self, hard: bool) {
        if self.stopping.is_some() && !(hard && self.soft_stop) {
            return;
        }
        if self.child.is_none() {
            // A process already gone left nothing to stop but the turn, if
            // any: quitting after Stop generating must not add a row.
            if self.turn.is_some() {
                self.settle(Some("Reply stopped"));
            }
            return;
        }
        let interrupt = self.protocol.interrupt();
        if !hard && matches!(interrupt, Interrupt::Idle) {
            if self.turn.is_some() {
                self.settle(Some("Reply stopped"));
            }
            return;
        }
        let signal = self.protocol.stop_signal();
        let child = self.child.as_mut().expect("child checked above");
        let soft = match interrupt {
            Interrupt::Frame(bytes) if !hard => {
                let epoch = self.cancel.load(Ordering::Acquire);
                child.write(&bytes, &self.cancel, epoch).is_ok()
            }
            _ => false,
        };
        if !soft {
            child.signal(signal);
            self.signalled = true;
        }
        let escalated = self.soft_stop && !soft;
        self.soft_stop = soft;
        if self.stopping.is_none() {
            self.stopping = Some(Instant::now());
            self.emit(ChatEvent::Stopping);
        } else if escalated {
            self.stopping = Some(Instant::now());
        }
    }
    /// A soft stop ended with the turn: settle it as stopped and keep the
    /// process, unless the transport already failed and must exit.
    fn finish_soft_stop(&mut self) {
        if self.failed {
            self.stop_turn(true);
            return;
        }
        self.soft_stop = false;
        self.stopping = None;
        if self.turn.is_some() {
            self.settle(Some("Reply stopped"));
        } else {
            self.emit(ChatEvent::Status(ChatStatus::Ready));
        }
    }
    pub(super) fn run(mut self, rx: mpsc::Receiver<CommandIn>, frames: mpsc::Receiver<CommandIn>) {
        loop {
            self.observe_cancel();
            if self.stop.load(Ordering::Acquire) && self.begin_exit() {
                break;
            }
            self.fail_slow_initialize();
            self.poll_protocol();
            let input = match rx.try_recv() {
                Ok(command) => Ok(command),
                Err(mpsc::TryRecvError::Empty) => frames.recv_timeout(Duration::from_millis(25)),
                Err(mpsc::TryRecvError::Disconnected) => break,
            };
            match self.handle(input) {
                Flow::Continue => {}
                Flow::NextRound => continue,
                Flow::Exit => break,
            }
            self.kill_slow_stop();
            if self.reap_exited_child() {
                break;
            }
        }
    }
    /// A cancel bump (Interrupt or Stop from the handle) interrupts the turn.
    fn observe_cancel(&mut self) {
        let cancel = self.cancel.load(Ordering::Acquire);
        if cancel != self.cancel_seen {
            self.cancel_seen = cancel;
            if self.turn.is_some() {
                self.interrupt();
            }
        }
    }
    /// Stop: interrupt and exit once the child is gone. True when there is no
    /// child left to wait for.
    fn begin_exit(&mut self) -> bool {
        self.exiting = true;
        match self.protocol.shutdown() {
            Shutdown::Signal => self.stop_turn(true),
            Shutdown::CloseStdin => self.close_input(),
        }
        self.child.is_none()
    }
    /// Closes stdin for a CLI that exits on EOF. Its lines are no longer read;
    /// the usual slow-stop deadline kills it if it lingers.
    fn close_input(&mut self) {
        let Some(child) = &mut self.child else {
            if self.stopping.is_none() && self.turn.is_some() {
                self.settle(Some("Reply stopped"));
            }
            return;
        };
        child.stdin = None;
        self.soft_stop = false;
        self.signalled = false;
        if self.stopping.is_none() {
            self.stopping = Some(Instant::now());
            self.emit(ChatEvent::Stopping);
        }
    }
    /// A CLI whose handshake has not finished within 10 seconds is unreadable.
    fn fail_slow_initialize(&mut self) {
        if self.stopping.is_none()
            && !self.protocol.ready()
            && self
                .child
                .as_ref()
                .is_some_and(|child| child.started.elapsed() >= Duration::from_secs(10))
        {
            let unreadable = self.protocol.strings().unreadable;
            self.failed = true;
            self.settle(Some(unreadable));
            self.emit(ChatEvent::Error(unreadable.into()));
            self.interrupt();
        }
    }
    /// Lets the protocol expire requests while no line arrives, and act on
    /// the results it queued from the last line.
    fn poll_protocol(&mut self) {
        // Bounded, so a protocol bug cannot hold the actor.
        for _ in 0..16 {
            if self.child.is_none() || !(self.stopping.is_none() || self.soft_stop) {
                return;
            }
            match self.protocol.poll(Instant::now()) {
                Parsed::Ignore => return,
                parsed => self.act(parsed),
            }
        }
    }
    /// One command from the handle or one frame from the CLI's reader.
    fn handle(&mut self, input: Result<CommandIn, mpsc::RecvTimeoutError>) -> Flow {
        match input {
            Ok(CommandIn::Answer { request_id, answer }) if self.stopping.is_none() => {
                self.answer_question(request_id, answer);
            }
            Ok(CommandIn::Stop) => {
                if self.begin_exit() {
                    return Flow::Exit;
                }
            }
            Ok(CommandIn::Interrupt) if self.turn.is_some() => self.interrupt(),
            Ok(CommandIn::RetrySave) => self.retry_save(),
            Ok(CommandIn::Send {
                text,
                origin,
                events,
                epoch,
            }) if self.turn.is_none()
                && self.stopping.is_none()
                && !self.save_failed
                && epoch == self.cancel.load(Ordering::Acquire) =>
            {
                return self.send(text, origin, events, epoch);
            }
            Ok(CommandIn::Send { .. }) => {
                self.emit(ChatEvent::Error(
                    "The Lead is still replying or the conversation needs to be saved.".into(),
                ));
                self.emit(ChatEvent::DeliveryFailed);
            }
            Ok(CommandIn::Line(generation, line))
                if generation == self.generation && (self.stopping.is_none() || self.soft_stop) =>
            {
                self.read_line(&line);
            }
            Ok(CommandIn::BadFrame(generation)) if generation == self.generation => {
                let unreadable = self.protocol.strings().unreadable;
                self.failed = true;
                self.settle(Some(unreadable));
                self.emit(ChatEvent::Error(unreadable.into()));
                self.interrupt();
            }
            Ok(CommandIn::Eof(generation)) if generation == self.generation => {
                if let Some(child) = &mut self.child {
                    child.reader_done = true;
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => return Flow::Exit,
            _ => {}
        }
        Flow::Continue
    }
    /// Answers the CLI's pending question, or denies it when `answer` is None.
    fn answer_question(&mut self, request_id: String, answer: Option<Value>) {
        if let Some(child) = &mut self.child
            && child.question.as_ref() == Some(&request_id)
        {
            let bytes = self.protocol.answer(&request_id, answer);
            let epoch = child.input_epoch;
            if child.write(&bytes, &self.cancel, epoch).is_err() {
                self.settle(Some(
                    "The answer could not be delivered. Work may already have started.",
                ));
                self.interrupt();
            } else {
                child.question = None;
                self.emit(ChatEvent::Status(ChatStatus::Working));
            }
        }
    }
    /// Writes the rows a save failure buffered, then reports the save.
    fn retry_save(&mut self) {
        self.save_failed = false;
        let pending = std::mem::take(&mut self.pending);
        self.persist_batch(pending);
        if !self.save_failed {
            self.emit(ChatEvent::Saved);
        }
    }
    /// Starts a turn: persists the outgoing rows, starts the CLI again if it
    /// exited, and writes the message to its stdin. A turn that ends before
    /// delivery skips this round's exit check.
    fn send(
        &mut self,
        text: String,
        origin: ChatOrigin,
        events: Vec<(String, Option<String>)>,
        epoch: u64,
    ) -> Flow {
        let turn = self.allocate_turn();
        self.turn = Some(turn);
        self.active_input = Some((origin.clone(), text.clone()));
        let mut outgoing = Vec::new();
        if origin == ChatOrigin::Author {
            outgoing.push((turn, ChatContent::Author { text: text.clone() }));
        }
        for (text, key) in events {
            outgoing.push((turn, ChatContent::Event { text, key }));
        }
        outgoing.push((turn, ChatContent::Outgoing { origin }));
        if !self.persist_batch(outgoing) {
            self.settle(Some("Message not sent"));
            return Flow::NextRound;
        }
        if self.child.is_none() && !self.respawn_child() {
            return Flow::NextRound;
        }
        let child = self.child.as_mut().expect("started child");
        let bytes = self.protocol.send(&text);
        child.input_epoch = epoch;
        if child.write(&bytes, &self.cancel, epoch).is_err() {
            if self.cancel.load(Ordering::Acquire) != epoch {
                self.interrupt();
                return Flow::NextRound;
            }
            self.settle(Some("Delivery unknown. Work may already have started. Send again only when you are ready to retry."));
            self.interrupt();
        } else if self.protocol.write_accepts() {
            self.accept(turn);
        } else {
            self.unacknowledged = true;
        }
        Flow::Continue
    }
    /// The CLI has the turn.
    fn accept(&mut self, turn: i64) {
        self.persist(turn, ChatContent::Accepted);
        self.emit(ChatEvent::Accepted);
        self.emit(ChatEvent::Status(ChatStatus::Working));
    }
    /// The acknowledgement a written turn waits for. A turn that finishes
    /// first was plainly accepted too.
    fn acknowledge(&mut self) {
        if std::mem::take(&mut self.unacknowledged)
            && let Some(turn) = self.turn
        {
            self.accept(turn);
        }
    }
    /// Starts a new CLI process for a send after the last one exited. False,
    /// with the turn settled, when it could not start.
    fn respawn_child(&mut self) -> bool {
        self.generation += 1;
        if self.launch.resumed {
            self.emit(ChatEvent::Resuming);
        }
        match Process::spawn(
            &self.launch,
            self.protocol.as_mut(),
            self.generation,
            self.frames.clone(),
        ) {
            Ok(child) => self.child = Some(child),
            Err(_) => {
                self.settle(Some(self.protocol.strings().restart_failed));
                return false;
            }
        }
        true
    }
    /// Decodes one line of the current process's output and acts on it.
    fn read_line(&mut self, line: &[u8]) {
        let parsed = if self.child.is_some() {
            self.protocol.parse(line)
        } else {
            Parsed::Ignore
        };
        self.act(parsed);
    }
    /// Acts on one decoded line or poll result.
    fn act(&mut self, parsed: Parsed) {
        if self.soft_stop {
            // Nothing from a turn being stopped reaches the author; its end
            // settles the stop.
            match parsed {
                Parsed::Final(_) | Parsed::Handled => return self.finish_soft_stop(),
                Parsed::Lingering => return self.stop_turn(true),
                Parsed::Started | Parsed::Question(_) | Parsed::Blocked | Parsed::Cancelled(_) => {
                    return;
                }
                _ => {}
            }
        }
        match parsed {
            Parsed::Started if self.turn.is_none() => self.start_cli_turn(),
            Parsed::Confirmed | Parsed::Started => self.confirm(),
            Parsed::Identity(id) => self.identity(id),
            Parsed::Reply(bytes) => self.reply(&bytes),
            Parsed::Accepted => self.acknowledge(),
            Parsed::Handled if self.turn.is_some() => {
                self.acknowledge();
                self.complete(None, None);
            }
            Parsed::Handled => {}
            Parsed::Final(text) => {
                self.acknowledge();
                self.complete(text, None);
            }
            // A turn the CLI stopped itself; nothing of ours waits on its tool.
            Parsed::Lingering => {
                self.acknowledge();
                self.complete(None, None);
            }
            Parsed::Instructed => self.instructed(),
            Parsed::Blocked => {
                self.emit(ChatEvent::Status(ChatStatus::Asking));
                self.emit(ChatEvent::Error(UNSUPPORTED.into()));
            }
            Parsed::Question(question) => {
                if let Some(child) = &mut self.child {
                    child.question = Some(question.request_id.clone());
                }
                self.emit(ChatEvent::Status(ChatStatus::Asking));
                self.emit(ChatEvent::Question(question));
            }
            Parsed::Cancelled(id) => {
                self.emit(ChatEvent::QuestionCancelled(id));
            }
            Parsed::Failed(error) => self.fail_turn(error, false),
            Parsed::Fatal(error) => self.fail_turn(error, true),
            Parsed::Explained { text, fatal } => self.fail_with(text, fatal),
            Parsed::Missing(error) => {
                self.missing.store(true, Ordering::Release);
                self.fail_turn(error, true);
            }
            Parsed::ShikaInput(row) => self.shika_input(row),
            Parsed::Ignore => {}
        }
    }
    /// A turn the CLI started itself while idle, such as after one of the
    /// Lead's background commands finished.
    fn confirm(&mut self) {
        self.launch.resumed = true;
        self.emit(ChatEvent::Confirmed(self.launch.conversation.clone()));
    }
    /// The id a CLI reported. The first one binds the history to it; any
    /// other id fails like an unexpected conversation. It is not confirmed
    /// (saved) yet: the protocol sends `Confirmed` once the CLI keeps the
    /// conversation, which Codex does only from its first turn.
    fn identity(&mut self, id: String) {
        if self.launch.conversation.is_empty() {
            match self.store.bind(self.protocol.cli(), &id) {
                Ok(boundary) => {
                    if let Some(boundary) = boundary {
                        self.emit(ChatEvent::Record(boundary));
                    }
                }
                Err(error) => {
                    self.failed = true;
                    self.settle(Some(&error));
                    self.emit(ChatEvent::Error(error));
                    self.interrupt();
                    return;
                }
            }
            self.launch.conversation = id;
        } else if self.launch.conversation != id {
            self.fail_turn(self.protocol.strings().other_conversation, false);
            return;
        }
        if self.launch.usage.is_none()
            && let Some(threads) = &self.launch.usage_threads
        {
            self.launch.usage = Some(threads.register(&self.launch.conversation));
        }
    }
    /// Records that the conversation holds this launch's instructions. A
    /// failed write only means the next resume sends them again.
    fn instructed(&mut self) {
        if !self.launch.conversation.is_empty()
            && self
                .store
                .set_instructed(
                    self.protocol.cli(),
                    &self.launch.conversation,
                    &self.launch.instructions,
                )
                .is_ok()
        {
            self.launch.instructed = true;
        }
    }
    /// Writes a frame the protocol owes the CLI without the author. A failed
    /// write ends the turn like a reply cut short; a Stop already moving the
    /// cancel epoch handles it instead.
    fn reply(&mut self, bytes: &[u8]) {
        let Some(child) = &mut self.child else {
            return;
        };
        let epoch = self.cancel.load(Ordering::Acquire);
        if child.write(bytes, &self.cancel, epoch).is_err()
            && self.cancel.load(Ordering::Acquire) == epoch
        {
            self.settle(Some(INCOMPLETE));
            self.interrupt();
        }
    }
    fn start_cli_turn(&mut self) {
        self.confirm();
        // No Outgoing row: Accepted alone marks a turn the
        // CLI took without Shika input, so recovery and
        // numbering treat it like any other accepted turn.
        let turn = self.allocate_turn();
        self.turn = Some(turn);
        if let Some(child) = &mut self.child {
            child.input_epoch = self.cancel.load(Ordering::Acquire);
        }
        self.persist(turn, ChatContent::Accepted);
        self.emit(ChatEvent::Started);
        self.emit(ChatEvent::Status(ChatStatus::Working));
    }
    /// Shika's own input the protocol wrote (`Parsed::ShikaInput`): its row
    /// joins the turn in progress, or opens a Shika-origin turn the way a
    /// turn the CLI starts itself is opened. There is no input to offer
    /// again if it fails, and nothing of the app's waits on its acceptance.
    fn shika_input(&mut self, row: &str) {
        let event = ChatContent::Event {
            text: row.into(),
            key: None,
        };
        if let Some(turn) = self.turn {
            self.persist(turn, event);
            return;
        }
        let turn = self.allocate_turn();
        self.turn = Some(turn);
        if let Some(child) = &mut self.child {
            child.input_epoch = self.cancel.load(Ordering::Acquire);
        }
        let origin = ChatOrigin::ShikaEvent;
        self.persist_batch(vec![
            (turn, event),
            (turn, ChatContent::Outgoing { origin }),
            (turn, ChatContent::Accepted),
        ]);
        self.emit(ChatEvent::Started);
        self.emit(ChatEvent::Status(ChatStatus::Working));
    }
    /// A decoding or reply failure settles the turn and interrupts the CLI.
    /// A `fatal` one, an unreadable stream or another conversation also ends
    /// the transport.
    fn fail_turn(&mut self, error: &'static str, fatal: bool) {
        let strings = self.protocol.strings();
        let fatal = fatal || error == strings.unreadable || error == strings.other_conversation;
        let display = if error == strings.unreadable {
            self.child
                .as_ref()
                .and(self.protocol.version())
                .map(|version| format!("{error} ({version})"))
                .unwrap_or_else(|| error.into())
        } else {
            error.into()
        };
        self.fail_with(display, fatal);
    }
    fn fail_with(&mut self, display: String, fatal: bool) {
        self.failed = fatal;
        self.settle(Some(&display));
        self.emit(ChatEvent::Error(display));
        self.interrupt();
    }
    /// A CLI still running 2 seconds into a stop gets the protocol's stop
    /// signal if the stop was gentler (a frame, or closed stdin), and SIGKILL
    /// 2 seconds after that signal.
    fn kill_slow_stop(&mut self) {
        if !self
            .stopping
            .is_some_and(|at| at.elapsed() >= Duration::from_secs(2))
        {
            return;
        }
        let signal = self.protocol.stop_signal();
        let Some(child) = &mut self.child else {
            return;
        };
        if self.signalled {
            child.signal(libc::SIGKILL);
        } else {
            child.signal(signal);
            self.signalled = true;
            self.soft_stop = false;
            self.stopping = Some(Instant::now());
        }
    }
    /// Notices the CLI's exit after all its output was read, settles the turn,
    /// and reports it. True when the actor is done.
    fn reap_exited_child(&mut self) -> bool {
        // Drain all queued output before observing exit, including the final result.
        let exited = self.child.as_mut().is_some_and(|c| {
            (c.reader_done
                || self
                    .stopping
                    .is_some_and(|at| at.elapsed() >= Duration::from_secs(3)))
                && c.child.try_wait().ok().flatten().is_some()
        });
        if exited {
            // EOF is ordered after every frame by the pipe reader. Interrupted
            // children have a bounded fallback when descendants hold a pipe.
            let stopped = self.stopping.take().is_some();
            self.child = None;
            self.soft_stop = false;
            self.signalled = false;
            if !stopped && self.turn.is_none() {
                self.persist(
                    0,
                    ChatContent::Event {
                        text: "The Lead stopped unexpectedly".into(),
                        key: None,
                    },
                );
            }
            if self.turn.is_some() {
                self.settle(Some(if stopped { "Reply stopped" } else { INCOMPLETE }));
            }
            if self.exiting {
                return true;
            }
            if stopped {
                self.emit(ChatEvent::Status(ChatStatus::Ready));
            }
            if !stopped || self.failed {
                self.emit(ChatEvent::Exit);
                return true;
            }
        }
        false
    }
}

#[cfg(test)]
#[path = "actor_tests.rs"]
mod tests;
