//! The actor's protocol-neutral paths, driven by a line-based test protocol
//! and a fake `sh` CLI.
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicU64;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::Value;

use super::super::protocol::{Interrupt, LeadProtocol, Shutdown, Strings};
use super::super::{ChatEvent, ChatLaunch, ChatStatus, ChatTransport, Parsed};
use crate::lead_chat_store::{ChatContent, ChatOrigin};

static STRINGS: Strings = Strings {
    unreadable: "This Test version could not be read",
    other_conversation: "Test returned a different conversation",
    start_failed: "Could not start Test",
    input_failed: "Could not open Test input",
    handshake_failed: "Could not initialize Test",
    restart_failed: "Could not start Test. Try sending again.",
};

/// `id X`, `confirm`, `reply X`, `ack`, `handled`, `instructed`,
/// `lingering`, `fatal`, `missing`, `said X`, `shika`, `final X` and `final`
/// map to the matching `Parsed`;
/// anything else is ignored. `idle` makes the next interrupt `Idle`. A line
/// past `MAX_FRAME` that starts with `skip` may be dropped unread.
struct LineProtocol {
    write_accepts: bool,
    idle: bool,
}
impl LeadProtocol for LineProtocol {
    fn cli(&self) -> &'static str {
        "test"
    }
    fn strings(&self) -> &'static Strings {
        &STRINGS
    }
    fn args(&self, _: &ChatLaunch) -> Vec<String> {
        Vec::new()
    }
    fn handshake(&mut self, _: &ChatLaunch) -> Vec<u8> {
        b"hello\n".to_vec()
    }
    fn ready(&self) -> bool {
        true
    }
    fn send(&mut self, text: &str) -> Vec<u8> {
        format!("send {text}\n").into_bytes()
    }
    fn write_accepts(&self) -> bool {
        self.write_accepts
    }
    fn answer(&mut self, request_id: &str, _: Option<Value>) -> Vec<u8> {
        format!("answer {request_id}\n").into_bytes()
    }
    fn interrupt(&mut self) -> Interrupt {
        if std::mem::take(&mut self.idle) {
            Interrupt::Idle
        } else {
            Interrupt::Frame(b"interrupt\n".to_vec())
        }
    }
    fn stop_signal(&self) -> i32 {
        libc::SIGTERM
    }
    fn shutdown(&self) -> Shutdown {
        Shutdown::CloseStdin
    }
    fn oversized_skippable(&self) -> fn(&[u8]) -> bool {
        |prefix| prefix.starts_with(b"skip")
    }
    fn parse(&mut self, line: &[u8]) -> Parsed {
        let line = std::str::from_utf8(line).unwrap().trim_end();
        let (word, rest) = line.split_once(' ').unwrap_or((line, ""));
        match word {
            "id" => Parsed::Identity(rest.into()),
            "confirm" => Parsed::Confirmed,
            "instructed" => Parsed::Instructed,
            "lingering" => Parsed::Lingering,
            "fatal" => Parsed::Fatal("Test refused the conversation"),
            "missing" => Parsed::Missing("Test has no such conversation"),
            "said" => Parsed::Explained {
                text: format!("Test failed ({rest})"),
                fatal: false,
            },
            "shika" => Parsed::ShikaInput("Shika sent the test its notes."),
            "idle" => {
                self.idle = true;
                Parsed::Ignore
            }
            "reply" => Parsed::Reply(format!("{rest}\n").into_bytes()),
            "ack" => Parsed::Accepted,
            "handled" => Parsed::Handled,
            "final" if rest.is_empty() => Parsed::Final(None),
            "final" => Parsed::Final(Some(rest.into())),
            _ => Parsed::Ignore,
        }
    }
}

struct Fixture {
    root: PathBuf,
}
impl Fixture {
    /// `body` is the `case` arms of a loop over stdin lines; every line read
    /// is logged first, and EOF logs `eof`.
    fn new(body: &str) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let root = loop {
            let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "shika-lead-actor-{}-{nanos}-{n}",
                std::process::id()
            ));
            match fs::create_dir(&path) {
                Ok(()) => break path,
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(err) => panic!("could not reserve actor fixture: {err}"),
            }
        };
        let log = root.join("log");
        let script = root.join("cli");
        fs::write(
            &script,
            format!(
                "#!/bin/sh\nwhile IFS= read -r line; do\n printf '%s\\n' \"$line\" >> '{}'\n case \"$line\" in\n{body}\n esac\ndone\necho eof >> '{}'\n",
                log.display(),
                log.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        fs::create_dir(root.join("data")).unwrap();
        Self { root }
    }
    fn start(&self, conversation: &str, write_accepts: bool) -> ChatTransport {
        ChatTransport::start_with(
            ChatLaunch {
                cli: "test",
                usage: None,
                usage_threads: None,
                program: self.root.join("cli"),
                cwd: self.root.clone(),
                path: std::env::var("PATH").unwrap_or_default(),
                env: Vec::new(),
                instructions: "guide".into(),
                model: None,
                notes: Vec::new(),
                conversation: conversation.into(),
                resumed: !conversation.is_empty(),
                instructed: false,
                history: None,
                data: self.root.join("data"),
                project: "actor-test".into(),
            },
            Box::new(LineProtocol {
                write_accepts,
                idle: false,
            }),
        )
        .unwrap()
    }
    fn log(&self) -> String {
        fs::read_to_string(self.root.join("log")).unwrap_or_default()
    }
    fn db(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(self.root.join("data/leads/actor-test/chat/history.sqlite"))
            .unwrap()
    }
    /// Every record's kind and text, in order, with the turn number.
    fn rows(&self) -> Vec<(i64, String)> {
        let db = self.db();
        let mut query = db
            .prepare("SELECT turn,content FROM records ORDER BY id")
            .unwrap();
        query
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
            .unwrap()
            .map(|row| {
                let (turn, content) = row.unwrap();
                let content: ChatContent = serde_json::from_str(&content).unwrap();
                let label = match content {
                    ChatContent::Author { text } => format!("author {text}"),
                    ChatContent::Lead { text } => format!("lead {text}"),
                    ChatContent::Event { text, .. } => format!("event {text}"),
                    ChatContent::Outgoing { .. } => "outgoing".into(),
                    ChatContent::Accepted => "accepted".into(),
                    ChatContent::Settled => "settled".into(),
                    ChatContent::Context { cli, conversation } => {
                        format!("context {cli} {conversation}")
                    }
                };
                (turn, label)
            })
            .collect()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn until(transport: &ChatTransport, done: impl Fn(&ChatEvent) -> bool) -> Vec<ChatEvent> {
    let deadline = Instant::now() + Duration::from_secs(7);
    let mut events = Vec::new();
    loop {
        let batch = transport.drain();
        let found = batch.iter().any(&done);
        events.extend(batch);
        if found {
            return events;
        }
        assert!(Instant::now() < deadline, "event deadline: {events:?}");
        std::thread::sleep(Duration::from_millis(10));
    }
}
fn ready(event: &ChatEvent) -> bool {
    matches!(event, ChatEvent::Status(ChatStatus::Ready))
}
fn wait_for(path: &Path, text: &str) {
    let deadline = Instant::now() + Duration::from_secs(7);
    while !fs::read_to_string(path).unwrap_or_default().contains(text) {
        assert!(Instant::now() < deadline, "{text} never reached {path:?}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn a_reported_identity_binds_the_history_and_a_later_turn_confirms_it() {
    let fixture =
        Fixture::new(" hello) echo 'id thread-1';;\n 'send hi') echo confirm; echo 'final Hi';;");
    let transport = fixture.start("", true);
    wait_for(&fixture.root.join("log"), "hello");
    std::thread::sleep(Duration::from_millis(200));
    // Bound, not saved: the CLI may not keep a conversation with no turn.
    assert!(
        !transport
            .drain()
            .iter()
            .any(|e| matches!(e, ChatEvent::Confirmed(_)))
    );
    transport
        .send("hi".into(), ChatOrigin::Author, Vec::new())
        .unwrap();
    let events = until(&transport, ready);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, ChatEvent::Confirmed(id) if id == "thread-1"))
    );
    transport.stop();
    assert_eq!(fixture.rows()[0], (0, "context test thread-1".into()));
}

#[test]
fn delivered_instructions_are_recorded_for_the_bound_conversation() {
    let fixture = Fixture::new(" hello) echo 'id thread-1'; echo instructed;;");
    let transport = fixture.start("", true);
    wait_for(&fixture.root.join("log"), "hello");
    let deadline = Instant::now() + Duration::from_secs(7);
    let store =
        || crate::lead_chat_store::ChatStore::open(&fixture.root.join("data"), "actor-test");
    while !store().unwrap().instructed("test", "thread-1", "guide") {
        assert!(Instant::now() < deadline, "instructions never recorded");
        std::thread::sleep(Duration::from_millis(10));
    }
    transport.stop();
    assert!(
        !store()
            .unwrap()
            .instructed("test", "thread-1", "guide plus a note")
    );
}

#[test]
fn a_fatal_failure_ends_the_transport_with_its_message() {
    let fixture = Fixture::new(" hello) echo fatal;;");
    let transport = fixture.start("thread-1", true);
    let events = until(&transport, |e| matches!(e, ChatEvent::Exit));
    assert!(
        events.iter().any(
            |e| matches!(e, ChatEvent::Error(text) if text == "Test refused the conversation")
        )
    );
    transport.stop();
    assert_eq!(
        fixture.rows(),
        [
            (0, "context test thread-1".into()),
            (0, "event Test refused the conversation".into())
        ]
    );
}

#[test]
fn an_idle_interrupt_settles_at_once_and_keeps_the_process() {
    let fixture =
        Fixture::new(" 'send wait') echo ack; echo idle;;\n 'send again') echo 'final Again';;");
    let transport = fixture.start("thread-1", false);
    transport
        .send("wait".into(), ChatOrigin::Author, Vec::new())
        .unwrap();
    until(&transport, |e| matches!(e, ChatEvent::Accepted));
    std::thread::sleep(Duration::from_millis(100));
    transport.interrupt();
    until(&transport, ready);
    transport
        .send("again".into(), ChatOrigin::Author, Vec::new())
        .unwrap();
    until(&transport, ready);
    transport.stop();
    // No interrupt frame was written and no second process started.
    assert_eq!(fixture.log(), "hello\nsend wait\nsend again\neof\n");
    assert!(
        fixture
            .rows()
            .contains(&(1, "event Reply stopped".to_string()))
    );
}

#[test]
fn a_stop_that_leaves_a_tool_running_ends_the_process_and_the_next_send_resumes() {
    let fixture = Fixture::new(
        " 'send long') echo ack;;\n interrupt) echo lingering;;\n 'send again') echo 'final Again';;",
    );
    let transport = fixture.start("thread-1", false);
    transport
        .send("long".into(), ChatOrigin::Author, Vec::new())
        .unwrap();
    until(&transport, |e| matches!(e, ChatEvent::Accepted));
    let now = Instant::now();
    transport.interrupt();
    let events = until(&transport, ready);
    // The stop signal (SIGTERM here), not the 2 second kill deadline.
    assert!(now.elapsed() < Duration::from_secs(2), "{events:?}");
    assert!(!events.iter().any(|e| matches!(e, ChatEvent::Exit)));
    transport
        .send("again".into(), ChatOrigin::Author, Vec::new())
        .unwrap();
    let events = until(&transport, ready);
    assert!(events.iter().any(|e| matches!(e, ChatEvent::Resuming)));
    transport.stop();
    // The first process ended without reading EOF; a second one handshook.
    assert_eq!(
        fixture.log(),
        "hello\nsend long\ninterrupt\nhello\nsend again\neof\n"
    );
    assert!(
        fixture
            .rows()
            .contains(&(1, "event Reply stopped".to_string()))
    );
}

#[test]
fn a_soft_stop_the_cli_ignores_gets_the_stop_signal_then_the_process_restarts() {
    let fixture = Fixture::new(" 'send long') echo ack;;\n 'send again') echo 'final Again';;");
    let transport = fixture.start("thread-1", false);
    transport
        .send("long".into(), ChatOrigin::Author, Vec::new())
        .unwrap();
    until(&transport, |e| matches!(e, ChatEvent::Accepted));
    let now = Instant::now();
    transport.interrupt();
    until(&transport, ready);
    let waited = now.elapsed();
    assert!(
        waited >= Duration::from_secs(2) && waited < Duration::from_secs(4),
        "{waited:?}"
    );
    transport
        .send("again".into(), ChatOrigin::Author, Vec::new())
        .unwrap();
    until(&transport, ready);
    transport.stop();
    assert_eq!(
        fixture.log(),
        "hello\nsend long\ninterrupt\nhello\nsend again\neof\n"
    );
}

#[test]
fn a_different_reported_identity_fails_and_ends_the_transport() {
    let fixture = Fixture::new(" hello) echo 'id other';;");
    let transport = fixture.start("thread-1", true);
    let events = until(&transport, |e| matches!(e, ChatEvent::Exit));
    assert!(
        events
            .iter()
            .any(|e| matches!(e, ChatEvent::Error(text) if text == STRINGS.other_conversation))
    );
    assert!(!events.iter().any(|e| matches!(e, ChatEvent::Confirmed(_))));
    transport.stop();
    // Bound at start to the id Shika holds; the reported one never replaces it.
    assert_eq!(
        fixture.rows(),
        [
            (0, "context test thread-1".into()),
            (0, format!("event {}", STRINGS.other_conversation))
        ]
    );
}

#[test]
fn a_reply_is_written_back_at_once_without_a_turn() {
    let fixture = Fixture::new(" hello) echo 'reply pong';;");
    let transport = fixture.start("thread-1", true);
    wait_for(&fixture.root.join("log"), "pong");
    transport.stop();
    assert_eq!(fixture.log(), "hello\npong\neof\n");
    assert_eq!(fixture.rows(), [(0, "context test thread-1".into())]);
}

#[test]
fn a_send_waits_for_the_cli_to_acknowledge_it() {
    let fixture = Fixture::new(
        " 'send hi') sleep 1; echo ack; echo 'final Done';;\n 'send bye') echo 'final Bye';;",
    );
    let transport = fixture.start("thread-1", false);
    transport
        .send("hi".into(), ChatOrigin::Author, Vec::new())
        .unwrap();
    wait_for(&fixture.root.join("log"), "send hi");
    std::thread::sleep(Duration::from_millis(200));
    let written = transport.drain();
    assert!(
        !written.iter().any(|e| matches!(
            e,
            ChatEvent::Accepted | ChatEvent::Status(ChatStatus::Working)
        )),
        "a written frame is not acceptance: {written:?}"
    );
    let events = until(&transport, ready);
    assert!(events.iter().any(|e| matches!(e, ChatEvent::Accepted)));
    // A turn that finishes before any acknowledgement was accepted as well.
    transport
        .send("bye".into(), ChatOrigin::Author, Vec::new())
        .unwrap();
    let events = until(&transport, ready);
    assert!(events.iter().any(|e| matches!(e, ChatEvent::Accepted)));
    transport.stop();
    assert_eq!(
        fixture.rows(),
        [
            (0, "context test thread-1"),
            (1, "author hi"),
            (1, "outgoing"),
            (1, "accepted"),
            (1, "lead Done"),
            (1, "settled"),
            (2, "author bye"),
            (2, "outgoing"),
            (2, "accepted"),
            (2, "lead Bye"),
            (2, "settled"),
        ]
        .map(|(turn, label)| (turn, label.to_string()))
    );
}

#[test]
fn a_handled_send_settles_with_no_bubble() {
    let fixture = Fixture::new(" 'send /status') echo handled;;");
    let transport = fixture.start("thread-1", false);
    transport
        .send("/status".into(), ChatOrigin::Author, Vec::new())
        .unwrap();
    let events = until(&transport, ready);
    assert!(events.iter().any(|e| matches!(e, ChatEvent::Accepted)));
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, ChatEvent::DeliveryFailed))
    );
    transport.stop();
    assert_eq!(
        fixture.rows(),
        [
            (0, "context test thread-1"),
            (1, "author /status"),
            (1, "outgoing"),
            (1, "accepted"),
            (1, "settled"),
        ]
        .map(|(turn, label)| (turn, label.to_string()))
    );
}

#[test]
fn a_frame_interrupt_settles_the_turn_and_keeps_the_process() {
    let fixture = Fixture::new(
        " 'send long') echo ack;;\n interrupt) echo 'final private partial';;\n 'send again') echo 'final Second';;",
    );
    let transport = fixture.start("thread-1", false);
    transport
        .send("long".into(), ChatOrigin::Author, Vec::new())
        .unwrap();
    until(&transport, |e| matches!(e, ChatEvent::Accepted));
    transport.interrupt();
    let events = until(&transport, ready);
    assert!(events.iter().any(|e| matches!(e, ChatEvent::Stopping)));
    assert!(!events.iter().any(|e| matches!(e, ChatEvent::Exit)));
    transport
        .send("again".into(), ChatOrigin::Author, Vec::new())
        .unwrap();
    until(&transport, ready);
    transport.stop();
    // One process: one handshake, and Stop closed its stdin.
    assert_eq!(
        fixture.log(),
        "hello\nsend long\ninterrupt\nsend again\neof\n"
    );
    assert_eq!(
        fixture.rows(),
        [
            (0, "context test thread-1"),
            (1, "author long"),
            (1, "outgoing"),
            (1, "accepted"),
            (1, "event Reply stopped"),
            (1, "settled"),
            (2, "author again"),
            (2, "outgoing"),
            (2, "accepted"),
            (2, "lead Second"),
            (2, "settled"),
        ]
        .map(|(turn, label)| (turn, label.to_string()))
    );
}

#[test]
fn stop_closes_stdin_and_the_cli_exits_on_its_own() {
    let fixture = Fixture::new(" 'send hi') echo 'final Hi';;");
    let transport = fixture.start("thread-1", true);
    transport
        .send("hi".into(), ChatOrigin::Author, Vec::new())
        .unwrap();
    until(&transport, ready);
    let now = Instant::now();
    transport.stop();
    assert!(now.elapsed() < Duration::from_secs(2), "no kill deadline");
    assert_eq!(fixture.log(), "hello\nsend hi\neof\n");
}

#[test]
fn an_oversized_line_the_protocol_never_reads_is_dropped_and_any_other_fails() {
    // 9 MB, past `MAX_FRAME`, then the turn's end on the next line.
    let fixture = Fixture::new(
        " 'send big') printf skip; head -c 9000000 /dev/zero | tr '\\0' x; echo; echo 'final After';;\n 'send bad') printf keep; head -c 9000000 /dev/zero | tr '\\0' x; echo; echo 'final Never';;",
    );
    let transport = fixture.start("thread-1", true);
    transport
        .send("big".into(), ChatOrigin::Author, Vec::new())
        .unwrap();
    let events = until(&transport, ready);
    assert!(!events.iter().any(|e| matches!(e, ChatEvent::Error(_))));
    transport
        .send("bad".into(), ChatOrigin::Author, Vec::new())
        .unwrap();
    let events = until(&transport, |e| matches!(e, ChatEvent::Exit));
    assert!(
        events
            .iter()
            .any(|e| matches!(e, ChatEvent::Error(error) if error == STRINGS.unreadable))
    );
    let rows = fixture.rows();
    assert!(rows.contains(&(1, "lead After".into())), "{rows:?}");
    assert!(!rows.iter().any(|(_, row)| row.contains("Never")));
}

#[test]
fn a_missing_conversation_ends_the_transport_and_is_reported() {
    let fixture = Fixture::new(" hello) echo missing;;");
    let transport = fixture.start("thread-1", true);
    assert!(!transport.conversation_missing());
    let events = until(&transport, |e| matches!(e, ChatEvent::Exit));
    assert!(
        events.iter().any(
            |e| matches!(e, ChatEvent::Error(text) if text == "Test has no such conversation")
        )
    );
    assert!(transport.conversation_missing());
    transport.stop();
}

#[test]
fn a_failure_in_the_clis_words_settles_the_turn_and_offers_the_text_again() {
    let fixture = Fixture::new(" 'send hi') echo 'said limit reached';;");
    let transport = fixture.start("thread-1", true);
    transport
        .send("hi".into(), ChatOrigin::Author, Vec::new())
        .unwrap();
    let events = until(&transport, ready);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, ChatEvent::Error(text) if text == "Test failed (limit reached)"))
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, ChatEvent::Rejected(text) if text == "hi"))
    );
    assert!(!events.iter().any(|e| matches!(e, ChatEvent::Exit)));
    assert!(!transport.conversation_missing());
    transport.stop();
}

/// Shika's own input the protocol wrote is a Shika-origin turn with its
/// row, counted as a submission (`Started`), never as the acceptance of
/// something the app sent; inside a turn the row joins that turn.
#[test]
fn shika_input_opens_a_shika_turn_or_joins_the_one_in_progress() {
    let fixture = Fixture::new(
        " hello) echo shika;;\n 'send hi') echo shika; echo 'final Hi';;\n interrupt) echo final;;",
    );
    let transport = fixture.start("thread-1", true);
    let events = until(&transport, |e| matches!(e, ChatEvent::Started));
    assert!(
        events
            .iter()
            .any(|e| matches!(e, ChatEvent::Status(ChatStatus::Working)))
    );
    assert!(!events.iter().any(|e| matches!(e, ChatEvent::Accepted)));
    // The turn is busy: a send is refused until it settles.
    transport
        .send("early".into(), ChatOrigin::Author, Vec::new())
        .unwrap();
    until(&transport, |e| matches!(e, ChatEvent::DeliveryFailed));
    transport.interrupt();
    let events = until(&transport, ready);
    assert!(!events.iter().any(|e| matches!(e, ChatEvent::Rejected(_))));
    transport
        .send("hi".into(), ChatOrigin::Author, Vec::new())
        .unwrap();
    until(&transport, ready);
    transport.stop();
    assert_eq!(
        fixture.rows(),
        [
            (0, "context test thread-1".into()),
            (1, "event Shika sent the test its notes.".into()),
            (1, "outgoing".into()),
            (1, "accepted".into()),
            (1, "event Reply stopped".into()),
            (1, "settled".into()),
            (2, "author hi".into()),
            (2, "outgoing".into()),
            (2, "accepted".into()),
            (2, "event Shika sent the test its notes.".into()),
            (2, "lead Hi".into()),
            (2, "settled".into()),
        ]
    );
}
