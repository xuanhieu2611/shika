//! The Lead chat actor: the one thread that owns the Claude Code process,
//! writes its input frames, decodes its output, and persists the turns.
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

use serde_json::json;

use super::{
    BAD_STREAM, ChatEvent, ChatLaunch, ChatStatus, ClaudeStream, CommandIn, INCOMPLETE, MAX_FRAME,
    Parsed, UNSUPPORTED,
};
use crate::lead_chat_store::{ChatContent, ChatOrigin, ChatResult, ChatStore};

pub(super) struct Process {
    child: Child,
    stdin: ChildStdin,
    decoder: ClaudeStream,
    reader_done: bool,
    question: Option<String>,
    input_epoch: u64,
    started: Instant,
}
impl Process {
    pub(super) fn spawn(
        launch: &ChatLaunch,
        resumed: bool,
        generation: u64,
        input: mpsc::SyncSender<CommandIn>,
    ) -> ChatResult<Self> {
        let mut cmd = Command::new(&launch.program);
        cmd.args(launch.args(resumed))
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
        {
            cmd.env_remove(key);
        }
        // A parent Claude session must not disable this Lead's persistence.
        cmd.env_remove("CLAUDE_CODE_CHILD_SESSION")
            .env_remove("CLAUDECODE");
        for (k, v) in &launch.env {
            cmd.env(k, v);
        }
        let mut child = cmd
            .spawn()
            .map_err(|_| "Could not start Claude Code".to_string())?;
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
            return Err("Could not open Claude Code input".into());
        }
        if stdin.write_all(b"{\"type\":\"control_request\",\"request_id\":\"shika-initialize\",\"request\":{\"subtype\":\"initialize\"}}\n").and_then(|_|stdin.flush()).is_err() {
            let _=child.kill();let _=child.wait();return Err("Could not initialize Claude Code".into());
        }
        let stdout = child.stdout.take().expect("piped stdout");
        let mut stderr = child.stderr.take().expect("piped stderr");
        thread::spawn(move || {
            let _ = std::io::copy(&mut stderr, &mut std::io::sink());
        });
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
            stdin,
            decoder: ClaudeStream::new(launch.conversation.clone()),
            reader_done: false,
            question: None,
            input_epoch: 0,
            started: Instant::now(),
        })
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
pub(super) struct Actor {
    pub(super) stop: Arc<AtomicBool>,
    pub(super) cancel_seen: u64,
    pub(super) cancel: Arc<AtomicU64>,
    pub(super) launch: ChatLaunch,
    pub(super) store: ChatStore,
    pub(super) output: mpsc::Sender<ChatEvent>,
    pub(super) frames: mpsc::SyncSender<CommandIn>,
    pub(super) child: Option<Process>,
    pub(super) generation: u64,
    pub(super) next_turn: i64,
    pub(super) turn: Option<i64>,
    pub(super) stopping: Option<Instant>,
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
    fn interrupt(&mut self) {
        if self.stopping.is_none() {
            if let Some(child) = &mut self.child {
                child.decoder.candidate = None;
                child.signal(libc::SIGINT);
                self.stopping = Some(Instant::now());
                self.emit(ChatEvent::Stopping);
            } else {
                self.settle(Some("Reply stopped"));
            }
        }
    }
    pub(super) fn run(mut self, rx: mpsc::Receiver<CommandIn>, frames: mpsc::Receiver<CommandIn>) {
        loop {
            let cancel = self.cancel.load(Ordering::Acquire);
            if cancel != self.cancel_seen {
                self.cancel_seen = cancel;
                if self.turn.is_some() {
                    self.interrupt();
                }
            }
            if self.stop.load(Ordering::Acquire) {
                self.exiting = true;
                self.interrupt();
                if self.child.is_none() {
                    break;
                }
            }
            if self.stopping.is_none()
                && self.child.as_ref().is_some_and(|child| {
                    !child.decoder.initialized && child.started.elapsed() >= Duration::from_secs(10)
                })
            {
                self.failed = true;
                self.settle(Some(BAD_STREAM));
                self.emit(ChatEvent::Error(BAD_STREAM.into()));
                self.interrupt();
            }
            let input = match rx.try_recv() {
                Ok(command) => Ok(command),
                Err(mpsc::TryRecvError::Empty) => frames.recv_timeout(Duration::from_millis(25)),
                Err(mpsc::TryRecvError::Disconnected) => break,
            };
            match input {
                Ok(CommandIn::Answer { request_id, answer }) if self.stopping.is_none() => {
                    if let Some(child) = &mut self.child
                        && child.question.as_ref() == Some(&request_id)
                    {
                        let response = if let Some(input) = answer {
                            json!({"behavior":"allow","updatedInput":input})
                        } else {
                            json!({"behavior":"deny","message":"The author cancelled this question."})
                        };
                        let mut bytes=serde_json::to_vec(&json!({"type":"control_response","response":{"subtype":"success","request_id":request_id,"response":response}})).expect("JSON value");
                        bytes.push(b'\n');
                        if write_frame(&mut child.stdin, &bytes, &self.cancel, child.input_epoch)
                            .is_err()
                        {
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
                Ok(CommandIn::Stop) => {
                    self.exiting = true;
                    self.interrupt();
                    if self.child.is_none() {
                        break;
                    }
                }
                Ok(CommandIn::Interrupt) if self.turn.is_some() => self.interrupt(),
                Ok(CommandIn::RetrySave) => {
                    self.save_failed = false;
                    let pending = std::mem::take(&mut self.pending);
                    self.persist_batch(pending);
                    if !self.save_failed {
                        self.emit(ChatEvent::Saved);
                    }
                }
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
                        continue;
                    }
                    if self.child.is_none() {
                        self.generation += 1;
                        if self.launch.resumed {
                            self.emit(ChatEvent::Resuming);
                        }
                        match Process::spawn(
                            &self.launch,
                            self.launch.resumed,
                            self.generation,
                            self.frames.clone(),
                        ) {
                            Ok(child) => self.child = Some(child),
                            Err(_) => {
                                self.settle(Some(
                                    "Could not start Claude Code. Try sending again.",
                                ));
                                continue;
                            }
                        }
                    }
                    let child = self.child.as_mut().expect("started child");
                    child.decoder.begin();
                    child.input_epoch = epoch;
                    let mut bytes = serde_json::to_vec(
                        &json!({"type":"user", "message":{"role":"user", "content":text}}),
                    )
                    .expect("JSON value");
                    bytes.push(b'\n');
                    if write_frame(&mut child.stdin, &bytes, &self.cancel, epoch).is_err() {
                        if self.cancel.load(Ordering::Acquire) != epoch {
                            self.interrupt();
                            continue;
                        }
                        self.settle(Some("Delivery unknown. Work may already have started. Send again only when you are ready to retry."));
                        self.interrupt();
                    } else {
                        self.persist(turn, ChatContent::Accepted);
                        self.emit(ChatEvent::Accepted);
                        self.emit(ChatEvent::Status(ChatStatus::Working));
                    }
                }
                Ok(CommandIn::Send { .. }) => {
                    self.emit(ChatEvent::Error(
                        "The Lead is still replying or the conversation needs to be saved.".into(),
                    ));
                    self.emit(ChatEvent::DeliveryFailed);
                }
                Ok(CommandIn::Line(generation, line))
                    if generation == self.generation && self.stopping.is_none() =>
                {
                    let parsed = self
                        .child
                        .as_mut()
                        .map(|c| c.decoder.parse(&line))
                        .unwrap_or(Parsed::Ignore);
                    match parsed {
                        Parsed::Started if self.turn.is_none() => {
                            self.launch.resumed = true;
                            self.emit(ChatEvent::Confirmed(self.launch.conversation.clone()));
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
                        Parsed::Confirmed | Parsed::Started => {
                            self.launch.resumed = true;
                            self.emit(ChatEvent::Confirmed(self.launch.conversation.clone()))
                        }
                        Parsed::Final(text) => self.complete(text, None),
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
                        Parsed::Failed(error) => {
                            self.failed = error == BAD_STREAM
                                || error == "Claude Code returned a different conversation";
                            let display = if error == BAD_STREAM {
                                self.child
                                    .as_ref()
                                    .and_then(|c| c.decoder.version.as_ref())
                                    .map(|version| format!("{error} ({version})"))
                                    .unwrap_or_else(|| error.into())
                            } else {
                                error.into()
                            };
                            self.settle(Some(&display));
                            self.emit(ChatEvent::Error(display));
                            self.interrupt();
                        }
                        Parsed::Ignore => {}
                    }
                }
                Ok(CommandIn::BadFrame(generation)) if generation == self.generation => {
                    self.failed = true;
                    self.settle(Some(BAD_STREAM));
                    self.emit(ChatEvent::Error(BAD_STREAM.into()));
                    self.interrupt();
                }
                Ok(CommandIn::Eof(generation)) if generation == self.generation => {
                    if let Some(child) = &mut self.child {
                        child.reader_done = true;
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                _ => {}
            }
            if self
                .stopping
                .is_some_and(|at| at.elapsed() >= Duration::from_secs(2))
                && let Some(child) = &mut self.child
            {
                child.signal(libc::SIGKILL);
            }
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
                    break;
                }
                if stopped {
                    self.emit(ChatEvent::Status(ChatStatus::Ready));
                }
                if !stopped || self.failed {
                    self.emit(ChatEvent::Exit);
                    break;
                }
            }
        }
    }
}
