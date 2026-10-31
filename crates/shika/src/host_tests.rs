//! Tests for PTY host input capture, submissions, and startup typeahead.
use super::*;

#[test]
fn only_nonempty_typed_submissions_start_candidate_turns() {
    let path = std::env::temp_dir().join(format!(
        "shika-submission-host-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    ));
    let core = Arc::new(Core::open(&path).unwrap());
    let state = Arc::new(Mutex::new(HostState::default()));
    let host = Host {
        core: core.clone(),
        state: state.clone(),
        capture: true,
    };
    host.write(b"\r", InputSource::Typed);
    host.write(b"\x1b[I", InputSource::Report);
    // Even a report/reply containing CR must never name/start a turn.
    host.write(b"mouse\r", InputSource::Report);
    host.write(b"reply\r", InputSource::Reply);
    host.resize(TerminalSize::new(20, 80));
    assert_eq!(lock(&state).submission, 0);
    assert!(lock(&state).title.is_none());
    host.write(b"fix it\r", InputSource::Typed);
    assert_eq!(lock(&state).submission, 1);
    assert_eq!(lock(&state).title.as_deref(), Some("fix it"));
    let submitted = lock(&state).last_submission;
    host.write(b"draft", InputSource::Typed);
    host.write(b"\x15", InputSource::Typed);
    host.write(b"\x1b[200~line one\nline two\x1b[201~", InputSource::Typed);
    assert_eq!(lock(&state).submission, 1);
    assert_eq!(lock(&state).last_submission, submitted);
    host.write(b"\r", InputSource::Typed);
    assert_eq!(lock(&state).submission, 2);
    assert_eq!(lock(&state).title.as_deref(), Some("fix it"));
    host.write(b"cleared draft\x01\x0b\r", InputSource::Typed);
    assert_eq!(lock(&state).submission, 2);
    host.write(b"\x1b[A", InputSource::Typed);
    host.write(b"\r", InputSource::Typed);
    assert_eq!(lock(&state).submission, 3);
    assert_eq!(lock(&state).title.as_deref(), Some("fix it"));
    // Both a pinned Terminal task and additional shells use capture=false.
    let shell_state = Arc::new(Mutex::new(HostState::default()));
    let shell = Host {
        core: core.clone(),
        state: shell_state.clone(),
        capture: false,
    };
    shell.write(b"echo Working...\r", InputSource::Typed);
    shell.write(b"\x1b[A\r", InputSource::Typed);
    shell.write(b"\x1b[200~claude\n\x1b[201~\r", InputSource::Typed);
    assert_eq!(lock(&shell_state).submission, 0);
    assert!(lock(&shell_state).title.is_none());
    assert!(lock(&shell_state).last_typed.is_some());
    drop(shell);
    drop(host);
    drop(core);
    std::fs::remove_dir_all(path).unwrap();
}

#[test]
fn a_launch_prompt_counts_as_the_first_submission_and_names_the_task_once() {
    let path = std::env::temp_dir().join(format!(
        "shika-launch-prompt-host-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    ));
    let core = Arc::new(Core::open(&path).unwrap());
    let state = Arc::new(Mutex::new(HostState::default()));
    let host = Host {
        core: core.clone(),
        state: state.clone(),
        capture: true,
    };
    let now = Instant::now();
    lock(&state).seed_launch_prompt("  Fix the   login bug\nsecond line", true, now);
    {
        let s = lock(&state);
        assert_eq!(s.submission, 1);
        assert_eq!(s.last_submission, Some(now));
        assert!(s.launch_turn);
        // The same name a typed first line would have produced.
        assert_eq!(s.title.as_deref(), Some("Fix the login bug"));
    }
    // The author typing a different first line later neither renames the
    // task nor is lost as a submission.
    lock(&state).title = None;
    host.write(b"something else\r", InputSource::Typed);
    assert!(lock(&state).title.is_none());
    assert_eq!(lock(&state).submission, 2);

    // The Lead keeps its own title.
    let mut lead = HostState::default();
    lead.seed_launch_prompt("You are the Lead", false, now);
    assert_eq!((lead.submission, lead.title.as_deref()), (1, None));

    // A switch prompt is a submission and must not become the task name.
    let mut switched = HostState::default();
    switched.seed_launch_prompt("Continue the backend.", false, now);
    assert_eq!(switched.submission, 1);
    assert!(switched.launch_turn);
    assert!(switched.title.is_none());
    switched.launch_turn = false;
    switched.capture_typed(b"rename the branch\r", now);
    assert!(switched.title.is_none());
    assert_eq!(switched.submission, 2);
    let mut activity = activity::Activity::new(now);
    activity.hold_for_launch();
    let started = activity.advance(activity::Signal::Unknown, Some(now), None, now, false);
    assert!(started.changed);
    assert_eq!(activity.status, Status::Working);
    drop(host);
    drop(core);
    std::fs::remove_dir_all(path).unwrap();
}

#[test]
fn idle_leads_keep_their_title_without_losing_first_submissions() {
    let now = Instant::now();
    // Fresh Claude/Codex/Pi, and resumed Leads, start without a prompt.
    // Disable naming before startup typeahead can reach the host.
    for first in [
        "Please implement login",
        "[shika] Shika restarted. Workers are not running.",
    ] {
        let mut host = HostState::default();
        host.keep_title();
        assert_eq!(host.submission, 0);
        assert!(host.last_submission.is_none());
        assert!(!host.launch_turn);
        host.capture_typed(first.as_bytes(), now);
        assert!(host.has_draft());
        host.capture_typed(b"\r", now);
        assert!(host.title.is_none());
        assert_eq!(host.submission, 1);
        assert_eq!(host.last_submission, Some(now));
        assert!(!host.has_draft());
        let mut activity = activity::Activity::new(now);
        activity.advance(
            activity::Signal::Unknown,
            host.last_submission,
            None,
            now,
            false,
        );
        assert_eq!(activity.status, Status::Working);
        host.capture_typed(b"A second goal\r", now);
        assert!(host.title.is_none());
        assert_eq!(host.submission, 2);
    }
    // Cursor still counts its fresh bootstrap once, without naming.
    let mut cursor = HostState::default();
    cursor.keep_title();
    cursor.seed_launch_prompt("You are the Lead", false, now);
    cursor.capture_typed(b"Please implement login\r", now);
    assert!(cursor.title.is_none());
    assert_eq!(cursor.submission, 2);

    // Ordinary tasks retain first-prompt naming.
    let mut task = HostState::default();
    task.capture_typed(b"Please implement login\r", now);
    assert_eq!(task.title.as_deref(), Some("Please implement login"));
}

/// The tick's consumption of a submission, in order, for a card whose CLI
/// was started with a prompt instead of typed into.
#[test]
fn a_launch_prompt_turn_runs_the_same_steps_as_a_typed_one() {
    let t = Instant::now();
    let at = |ms: u64| t + Duration::from_millis(ms);
    let mut host = HostState::default();
    host.seed_launch_prompt("Add tests", true, t);
    let mut activity = activity::Activity::new(t);
    let mut lifecycle = lifecycle::Lifecycle::default();
    let mut submitted = 0;

    // First tick after the session exists: the submission is consumed,
    // the lifecycle is fenced, and the activity clock is armed.
    assert_ne!(host.submission, submitted);
    submitted = host.submission;
    lifecycle.submitted(host.lifecycle);
    if std::mem::take(&mut host.launch_turn) {
        activity.hold_for_launch();
    }
    let first = activity.advance(
        activity::Signal::Unknown,
        host.last_submission,
        None,
        at(100),
        false,
    );
    assert!(first.changed && !first.notify);
    assert_eq!(activity.status, Status::Working);
    assert_eq!(activity.turn_started, Some(t));
    assert_eq!(submitted, host.submission);

    // Pi: its startup Idle report is not an answer, a later Working
    // report confirms the turn, and a later Idle report finishes it.
    use shika_core::{AgentActivity, AgentActivityState::*};
    let report = |seq, state| Some(AgentActivity { seq, state });
    assert_eq!(lifecycle.observe(report(1, Idle)), None);
    assert_eq!(
        lifecycle.observe(report(2, Working)),
        Some(activity::Signal::Working)
    );
    activity.advance_authoritative(activity::Signal::Working, None, None, at(900), false);
    let idle = lifecycle.observe(report(3, Idle)).unwrap();
    activity.advance_authoritative(idle, None, None, at(5000), false);
    let done = activity.advance_authoritative(idle, None, None, at(5600), false);
    assert!(done.ready && done.notify);
    assert_eq!(activity.turn_started, Some(t));
}

#[test]
fn delayed_lifecycle_read_cannot_cross_a_submission() {
    let mut host = HostState {
        submission: 2,
        lifecycle_checking: true,
        ..HostState::default()
    };
    host.note_lifecycle(
        1,
        Some(shika_core::AgentActivity {
            seq: 3,
            state: shika_core::AgentActivityState::Idle,
        }),
    );
    assert!(host.lifecycle.is_none());
    assert!(!host.lifecycle_checking);
    host.note_lifecycle(
        2,
        Some(shika_core::AgentActivity {
            seq: 4,
            state: shika_core::AgentActivityState::Working,
        }),
    );
    assert_eq!(
        host.lifecycle.unwrap().state,
        shika_core::AgentActivityState::Working
    );
}

#[test]
fn live_terminal_interactions_and_active_enter_preserve_the_turn_timer() {
    let path = std::env::temp_dir().join(format!(
        "shika-activity-host-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    ));
    let core = Arc::new(Core::open(&path).unwrap());
    let state = Arc::new(Mutex::new(HostState::default()));
    let terminal = Terminal::new(
        TerminalOptions {
            size: TerminalSize::new(12, 80),
            ..Default::default()
        },
        Host {
            core: core.clone(),
            state: state.clone(),
            capture: true,
        },
    );
    terminal.write(b"fix it\r");
    let start = lock(&state).last_submission.unwrap();
    let mut clock = activity::Activity::new(start);
    terminal.feed("\x1b[2J\x1b[H✻ Thinking… (12s · esc to interrupt)\r\n────\r\n❯ \r\n────\r\n? for shortcuts".as_bytes());
    let signal = activity::detect("claude", &terminal.live_text_lines(), None);
    assert_eq!(signal, activity::Signal::Working);
    clock.advance(signal, Some(start), None, start, false);
    terminal.feed(b"\x1b[?1004h");
    terminal.report(b"\x1b[I");
    terminal.report(b"\x1b[O");
    terminal.report(b"\x1b[<0;2;3M");
    terminal.scroll(2);
    terminal.resize(TerminalSize::new(14, 90));
    terminal.write(b"mistyped");
    assert_eq!(lock(&state).submission, 1);
    terminal.write(b"\r");
    assert_eq!(lock(&state).submission, 2);
    let signal = activity::detect("claude", &terminal.live_text_lines(), None);
    clock.advance(
        signal,
        lock(&state).last_submission,
        None,
        start + Duration::from_secs(30),
        false,
    );
    assert_eq!(clock.status, Status::Working);
    assert_eq!(clock.turn_started, Some(start));
    assert_eq!(
        model::short_time(
            (start + Duration::from_secs(30)).duration_since(clock.turn_started.unwrap())
        ),
        "30s"
    );
    drop(terminal);
    drop(core);
    std::fs::remove_dir_all(path).unwrap();
}

#[test]
fn setup_input_is_not_queued_as_a_future_agent_prompt() {
    let path = std::env::temp_dir().join(format!(
        "shika-preparation-host-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let core = Arc::new(Core::open(&path).unwrap());
    let state = Arc::new(Mutex::new(HostState {
        preparing: true,
        ..HostState::default()
    }));
    let host = Host {
        core,
        state: state.clone(),
        capture: true,
    };
    host.write(b"installer answer\r", InputSource::Typed);
    host.write(b"\x1b[1;1R", InputSource::Reply);
    let state = lock(&state);
    assert!(state.pending_input.is_empty());
    assert!(state.title.is_none());
    assert_eq!(state.submission, 0);
    assert!(state.last_typed.is_none());
    std::fs::remove_dir_all(path).unwrap();
}

#[test]
fn prepared_agent_startup_preserves_query_replies_but_not_setup_input() {
    let path = std::env::temp_dir().join(format!(
        "shika-prepared-start-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    ));
    let core = Arc::new(Core::open(&path).unwrap());
    let state = Arc::new(Mutex::new(HostState {
        preparing: true,
        agent_starting: true,
        ..HostState::default()
    }));
    let terminal = Terminal::new(
        TerminalOptions {
            size: TerminalSize::new(10, 40),
            ..Default::default()
        },
        Host {
            core: core.clone(),
            state: state.clone(),
            capture: true,
        },
    );
    terminal.feed(b"\x1b[6n");
    terminal.write(b"installer answer\r");
    let host = lock(&state);
    assert_eq!(host.pending_input.concat(), b"\x1b[1;1R");
    assert!(host.title.is_none());
    assert!(host.last_typed.is_none());
    assert_eq!(host.submission, 0);
    drop(host);
    drop(terminal);
    drop(core);
    std::fs::remove_dir_all(path).unwrap();
}

#[test]
fn startup_query_replies_and_typeahead_survive_until_pty_binding() {
    let path = std::env::temp_dir().join(format!(
        "shika-host-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    ));
    let core = Arc::new(Core::open(&path).unwrap());
    let state = Arc::new(Mutex::new(HostState::default()));
    let terminal = Terminal::new(
        TerminalOptions {
            size: TerminalSize::new(10, 40),
            ..Default::default()
        },
        Host {
            core: core.clone(),
            state: state.clone(),
            capture: true,
        },
    );
    // A CLI's cursor-position query may precede Core's create result.
    terminal.feed(b"\x1b[6n");
    terminal.write(b"fix resize\r");
    let host = lock(&state);
    assert_eq!(host.pending_input.concat(), b"\x1b[1;1Rfix resize\r");
    assert_eq!(host.title.as_deref(), Some("fix resize"));
    drop(host);
    drop(terminal);
    drop(core);
    std::fs::remove_dir_all(path).unwrap();
}
