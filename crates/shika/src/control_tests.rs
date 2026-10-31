use super::*;
use shika_core::SessionKind;
use shika_core::control::{PROTOCOL_VERSION, Request, render_text, send};

fn session(kind: SessionKind) -> Session {
    Session {
        id: "1".into(),
        project_id: "p".into(),
        preset_id: "claude".into(),
        preset_name: "Claude Code".into(),
        title: "New Claude Code".into(),
        manual_title: false,
        branch: String::new(),
        repo: "/repo".into(),
        worktree: "/repo".into(),
        base_ref: None,
        pty: None,
        shell_ptys: Vec::new(),
        cli_titled: false,
        prompt_named: false,
        agent_started: std::time::UNIX_EPOCH,
        kind,
        started_by: None,
    }
}

#[test]
fn a_lead_never_sees_the_checkout_session_or_another_lead() {
    // Every task command (tasks, wait, read, diff, send, key, pr, close)
    // finds its card through `project_tasks`, which applies this rule, so
    // a checkout session is "No task <id> in this project." to a Lead.
    assert!(is_lead_visible(false, Some(&session(SessionKind::Task))));
    assert!(is_lead_visible(false, None));
    assert!(!is_lead_visible(
        false,
        Some(&session(SessionKind::Checkout))
    ));
    assert!(!is_lead_visible(true, Some(&session(SessionKind::Lead))));
    assert!(!is_lead_visible(true, None));
}
use std::sync::Mutex;

/// A server whose `shika` command lives in a per-process scratch data
/// directory, never the real one. Servers started in parallel share it,
/// which `install_command` allows.
fn start_server() -> Server {
    let data = std::env::temp_dir().join(format!("shika-server-test-{}", std::process::id()));
    std::fs::create_dir_all(&data).expect("scratch data directory");
    Server::start(&data).expect("control server")
}

#[test]
fn the_lead_guide_stays_small() {
    // It is the CLI's system prompt on every turn of every conversation.
    let guide = lead_guide(Path::new("/x/bin"));
    assert!(guide.len() <= 7500, "{} bytes", guide.len());
    assert!(!guide.contains('\u{2014}'));
}

fn task(id: &str, status: TaskStatus) -> TaskInfo {
    TaskInfo {
        id: id.into(),
        title: id.into(),
        cli: "codex".into(),
        status,
        elapsed_secs: None,
        branch: String::new(),
        diff_stat: None,
        pr: None,
        started_by_lead: true,
        path: String::new(),
        report: None,
    }
}

fn observed(id: &str, status: TaskStatus, turn: Option<Instant>) -> Observed {
    Observed {
        info: task(id, status),
        turn,
        report_received: None,
    }
}

fn waited(resolved: Option<Resolved>) -> (Vec<String>, Vec<String>, bool, Resolved) {
    let resolved = resolved.expect("a reply");
    let Reply::Waited {
        events,
        still_working,
        timed_out,
    } = resolved.reply.clone()
    else {
        panic!("expected Waited");
    };
    (
        events.into_iter().map(|e| e.task.id).collect(),
        still_working.into_iter().map(|t| t.id).collect(),
        timed_out,
        resolved,
    )
}

#[test]
fn wait_returns_settled_tasks_once_and_again_after_a_new_turn() {
    let t0 = Instant::now();
    let deadline = t0 + Duration::from_secs(100);
    let turn1 = Some(t0);
    let mut ledger = Ledger::default();
    let tasks = [
        observed("a", TaskStatus::Ready, turn1),
        observed("b", TaskStatus::Working, turn1),
    ];
    let (events, working, timed_out, resolved) = waited(resolve(&ledger, &tasks, t0, deadline));
    assert_eq!(
        (events, working, timed_out),
        (vec!["a".into()], vec!["b".into()], false)
    );
    ledger.mark(resolved.marks);

    // The same settlement is not reported twice: keep waiting for b.
    assert!(resolve(&ledger, &tasks, t0, deadline).is_none());

    // b asks, then a later wait sees it settle in Ready as a new state.
    let tasks = [
        observed("a", TaskStatus::Ready, turn1),
        observed("b", TaskStatus::Asking, turn1),
    ];
    let (events, working, _, resolved) = waited(resolve(&ledger, &tasks, t0, deadline));
    assert_eq!((events, working), (vec!["b".into()], vec![]));
    ledger.mark(resolved.marks);
    let tasks = [
        observed("a", TaskStatus::Ready, turn1),
        observed("b", TaskStatus::Ready, turn1),
    ];
    let (events, _, _, resolved) = waited(resolve(&ledger, &tasks, t0, deadline));
    assert_eq!(events, vec!["b".to_string()]);
    ledger.mark(resolved.marks);

    // Nothing unreported and nothing working: answered at once, empty.
    let (events, working, timed_out, _) = waited(resolve(&ledger, &tasks, t0, deadline));
    assert_eq!((events, working, timed_out), (vec![], vec![], false));

    // a works again (a new turn) and settles again: reported again.
    let turn2 = Some(t0 + Duration::from_secs(60));
    let tasks = [
        observed("a", TaskStatus::Working, turn2),
        observed("b", TaskStatus::Ready, turn1),
    ];
    assert!(resolve(&ledger, &tasks, t0, deadline).is_none());
    let tasks = [
        observed("a", TaskStatus::Ready, turn2),
        observed("b", TaskStatus::Ready, turn1),
    ];
    let (events, ..) = waited(resolve(&ledger, &tasks, t0, deadline));
    assert_eq!(events, vec!["a".to_string()]);
}

#[test]
fn three_workers_finishing_together_deliver_every_report_once() {
    let t0 = Instant::now();
    let mut tasks: Vec<_> = ["a", "b", "c"]
        .into_iter()
        .map(|id| {
            let mut task = observed(id, TaskStatus::Ready, Some(t0));
            task.info.report = Some(format!("{id}: tests pass"));
            task.report_received = Some(at(t0, 1.0));
            task
        })
        .collect();
    let mut ledger = Ledger::default();
    let mut bell = Doorbell::default();
    assert!(bell.due(&ledger, &tasks, &gate(), t0).is_empty());
    let due = bell.due(&ledger, &tasks, &gate(), at(t0, 2.0));
    assert_eq!(ids(&due), ["a", "b", "c"]);
    bell.rang(&due);
    let resolved = resolve(&ledger, &tasks, at(t0, 2.0), at(t0, 10.0)).unwrap();
    let Reply::Waited { events, .. } = &resolved.reply else {
        panic!("expected wait")
    };
    assert_eq!(events.len(), 3);
    assert!(events.iter().all(|e| e.task.report.is_some()));
    ledger.mark(resolved.marks);
    assert!(bell.due(&ledger, &tasks, &gate(), at(t0, 3.0)).is_empty());
    assert!(
        waited(resolve(&ledger, &tasks, at(t0, 3.0), at(t0, 10.0)))
            .0
            .is_empty()
    );

    // Ready was delivered before an updated/late report. The same turn
    // must wake the Lead again, but only for that worker.
    tasks[1].info.report = Some("b: updated tests pass".into());
    tasks[1].report_received = Some(at(t0, 4.0));
    assert!(bell.due(&ledger, &tasks, &gate(), at(t0, 4.0)).is_empty());
    assert_eq!(ids(&bell.due(&ledger, &tasks, &gate(), at(t0, 5.6))), ["b"]);
    let (events, _, _, resolved) = waited(resolve(&ledger, &tasks, at(t0, 5.6), at(t0, 10.0)));
    assert_eq!(events, ["b"]);
    ledger.mark(resolved.marks);
    assert!(bell.due(&ledger, &tasks, &gate(), at(t0, 7.0)).is_empty());
}

#[test]
fn a_report_after_a_delivered_ready_without_a_report_is_not_lost() {
    let t0 = Instant::now();
    let mut task = observed("a", TaskStatus::Ready, Some(t0));
    let mut ledger = Ledger::default();
    let resolved = resolve(&ledger, std::slice::from_ref(&task), t0, t0).unwrap();
    ledger.mark(resolved.marks);
    task.info.report = Some("Finished; tests pass".into());
    task.report_received = Some(at(t0, 1.0));
    let (events, _, _, _) = waited(resolve(&ledger, &[task], at(t0, 1.0), t0));
    assert_eq!(events, ["a"]);
}

#[test]
fn report_retries_are_idempotent_but_new_turns_and_edits_are_not() {
    let t0 = Instant::now();
    let mut report = None;
    let make = |text: &str, turn, received| WorkerReport {
        text: text.into(),
        turn,
        received,
    };
    make("Done", Some(t0), t0).record(&mut report);
    make("Done", Some(t0), at(t0, 1.0)).record(&mut report);
    assert_eq!(report.as_ref().unwrap().received, t0);
    make("Tests passed", Some(t0), at(t0, 2.0)).record(&mut report);
    assert_eq!(report.as_ref().unwrap().received, at(t0, 2.0));
    make("Tests passed", Some(at(t0, 3.0)), at(t0, 4.0)).record(&mut report);
    assert_eq!(report.as_ref().unwrap().received, at(t0, 4.0));
}

#[test]
fn fast_reports_belong_to_the_pending_submission_not_the_previous_turn() {
    use crate::activity::{Activity, Signal};
    let t0 = Instant::now();
    let mut activity = Activity::new(t0);
    let mut host = HostState::default();
    host.seed_launch_prompt("Do the work", true, t0);
    let (_, report) = worker_command(
        Command::Report {
            text: "Fast result".into(),
        },
        host.report_turn(&activity, 0),
    );
    activity.hold_for_launch();
    activity.advance(Signal::Working, host.last_submission, None, t0, false);
    assert_eq!(report.unwrap().turn, activity.turn_started);

    // A send from Ready starts a fresh report epoch before the next tick.
    activity.advance(Signal::Idle, None, None, at(t0, 1.0), false);
    activity.advance(Signal::Idle, None, None, at(t0, 2.0), false);
    assert_eq!(activity.status, Status::Ready);
    host.capture_typed(b"More work\r", at(t0, 3.0));
    assert_eq!(host.report_turn(&activity, 1), Some(at(t0, 3.0)));
    activity.advance(
        Signal::Working,
        host.last_submission,
        None,
        at(t0, 3.0),
        false,
    );
    assert_eq!(host.report_turn(&activity, 2), activity.turn_started);
    // Input during Working is not a fresh epoch.
    host.capture_typed(b"Continue\r", at(t0, 4.0));
    assert_eq!(host.report_turn(&activity, 2), Some(at(t0, 3.0)));
}

#[test]
fn an_abandoned_wait_releases_the_doorbell_gate_before_its_deadline() {
    let t0 = Instant::now();
    let mut lead = LeadState::new("synthetic".into());
    let (reply, _answer) = mpsc::channel();
    let (confirm, delivered) = mpsc::channel();
    lead.waiters.push(Waiter {
        named: vec![],
        deadline: at(t0, 600.0),
        reply: Responder::new(reply, None),
        delivered,
    });
    let tasks = [observed("a", TaskStatus::Ready, Some(t0))];
    let closed = Gate {
        waiting: !lead.waiters.is_empty(),
        ..gate()
    };
    lead.doorbell.due(&lead.ledger, &tasks, &closed, t0);
    assert!(
        lead.doorbell
            .due(&lead.ledger, &tasks, &closed, at(t0, 2.0))
            .is_empty()
    );
    assert!(lead.waiters[0].connected());
    drop(confirm); // the connection thread noticed EOF
    lead.waiters.retain(Waiter::connected);
    assert!(lead.waiters.is_empty());
    let open = Gate {
        waiting: !lead.waiters.is_empty(),
        ..gate()
    };
    assert_eq!(
        ids(&lead.doorbell.due(&lead.ledger, &tasks, &open, at(t0, 2.1))),
        ["a"]
    );
    assert!(!lead.ledger.is_reported(&tasks[0]));
}

#[test]
fn control_commands_use_a_shell_safe_absolute_path_in_all_prompts() {
    let dir = Path::new("/tmp/control space'quote/bin");
    let command = command_path(dir);
    assert_eq!(command, "'/tmp/control space'\"'\"'quote/bin/shika'");
    let echoed = std::process::Command::new("/bin/sh")
        .args(["-c", &format!("printf '%s' {command}")])
        .output()
        .unwrap();
    assert!(echoed.status.success());
    assert_eq!(
        String::from_utf8(echoed.stdout).unwrap(),
        dir.join("shika").display().to_string()
    );
    assert!(lead_prompt("Test", dir).contains(&format!("Run {command} help and {command} memory")));
    assert!(lead_guide(dir).contains(&command));
    assert!(worker_prompt("Test", dir).contains(&format!("run: {command} report")));
}

#[test]
fn wait_times_out_naming_what_still_works_and_exits_count_as_settled() {
    let t0 = Instant::now();
    let ledger = Ledger::default();
    let tasks = [
        observed("a", TaskStatus::Working, Some(t0)),
        observed("b", TaskStatus::Starting, None),
        observed("c", TaskStatus::Waiting, None),
    ];
    assert!(resolve(&ledger, &tasks, t0, t0 + Duration::from_secs(5)).is_none());
    let (events, working, timed_out, _) = waited(resolve(
        &ledger,
        &tasks,
        t0 + Duration::from_secs(5),
        t0 + Duration::from_secs(5),
    ));
    assert_eq!(
        (events, working, timed_out),
        (vec![], vec!["a".to_string(), "b".to_string()], true)
    );
    let tasks = [observed("a", TaskStatus::Exited, Some(t0))];
    let (events, _, timed_out, _) =
        waited(resolve(&ledger, &tasks, t0, t0 + Duration::from_secs(5)));
    assert_eq!((events, timed_out), (vec!["a".to_string()], false));
    // No tasks at all: nothing to wait for.
    let (events, working, timed_out, _) = waited(resolve(&ledger, &[], t0, t0));
    assert_eq!((events, working, timed_out), (vec![], vec![], false));
}

#[test]
fn undelivered_events_stay_unreported() {
    let t0 = Instant::now();
    let deadline = t0 + Duration::from_secs(10);
    let mut ledger = Ledger::default();
    let tasks = [observed("a", TaskStatus::Ready, Some(t0))];
    // Resolving alone marks nothing: the caller marks after confirmation.
    let (events, ..) = waited(resolve(&ledger, &tasks, t0, deadline));
    assert_eq!(events, vec!["a".to_string()]);
    let (events, ..) = waited(resolve(&ledger, &tasks, t0, deadline));
    assert_eq!(events, vec!["a".to_string()]);

    // A write that failed, or a client that left first, drops the marks.
    for outcome in [Some(false), None] {
        let (_, _, _, resolved) = waited(resolve(&ledger, &tasks, t0, deadline));
        let (confirm, delivered) = mpsc::channel();
        let mut pending = vec![Unconfirmed {
            delivered,
            marks: resolved.marks,
        }];
        apply_confirmations(&mut ledger, &mut pending);
        assert_eq!(pending.len(), 1, "still in flight");
        match outcome {
            Some(written) => confirm.send(written).unwrap(),
            None => drop(confirm),
        }
        apply_confirmations(&mut ledger, &mut pending);
        assert!(pending.is_empty());
        let (events, ..) = waited(resolve(&ledger, &tasks, t0, deadline));
        assert_eq!(events, vec!["a".to_string()]);
    }
}

#[test]
fn a_confirmed_write_marks_the_events_reported() {
    let t0 = Instant::now();
    let deadline = t0 + Duration::from_secs(10);
    let mut ledger = Ledger::default();
    let tasks = [observed("a", TaskStatus::Ready, Some(t0))];
    let (_, _, _, resolved) = waited(resolve(&ledger, &tasks, t0, deadline));
    let (confirm, delivered) = mpsc::channel();
    let mut pending = vec![Unconfirmed {
        delivered,
        marks: resolved.marks,
    }];
    // Until the write is confirmed the event is still owed.
    apply_confirmations(&mut ledger, &mut pending);
    let (events, ..) = waited(resolve(&ledger, &tasks, t0, deadline));
    assert_eq!(events, vec!["a".to_string()]);
    confirm.send(true).unwrap();
    apply_confirmations(&mut ledger, &mut pending);
    assert!(pending.is_empty());
    let (events, ..) = waited(resolve(&ledger, &tasks, t0, deadline));
    assert!(events.is_empty());
}

#[test]
fn the_connection_thread_confirms_a_written_reply() {
    let server = start_server();
    let socket = server.socket();
    let (seen, confirmations) = mpsc::channel();
    let seen = Mutex::new(seen);
    answer_with(server, move |incoming| {
        let _ = incoming.reply.send(Reply::Help { text: "hi".into() });
        let _ = seen.lock().unwrap().send(incoming.delivered);
    });
    send(&socket, &Request::new("t", Command::Help)).unwrap();
    let delivered = confirmations.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(delivered.recv_timeout(Duration::from_secs(5)).unwrap());
}

/// A stand-in for the UI thread: answers every request on its own thread.
fn answer_with(server: Server, handler: impl Fn(Incoming) + Send + 'static) {
    std::thread::spawn(move || {
        loop {
            match server.next() {
                Some(incoming) => handler(incoming),
                None => std::thread::sleep(Duration::from_millis(10)),
            }
        }
    });
}

#[test]
fn requests_reach_the_ui_thread_and_replies_come_back() {
    let server = start_server();
    let socket = server.socket();
    assert!(server.bin_dir().join("shika").exists());
    answer_with(server, |incoming| {
        let reply = match (incoming.token.as_str(), incoming.command) {
            ("good", Command::Tasks) => Reply::Tasks {
                tasks: vec![task("a", TaskStatus::Ready)],
            },
            ("good", _) => Reply::Help { text: "hi".into() },
            _ => refused("unknown token"),
        };
        let _ = incoming.reply.send(reply);
    });

    let reply = send(&socket, &Request::new("good", Command::Tasks)).unwrap();
    assert!(matches!(reply, Reply::Tasks { tasks } if tasks.len() == 1));
    let reply = send(&socket, &Request::new("bad", Command::Tasks)).unwrap();
    assert_eq!(reply.exit_code(), 1);

    let mut stale = Request::new("good", Command::Help);
    stale.version = PROTOCOL_VERSION + 1;
    assert_eq!(send(&socket, &stale).unwrap().exit_code(), 1);
}

#[test]
fn a_client_that_left_is_noticed_before_its_reply_is_sent() {
    let server = start_server();
    let socket = server.socket();
    let (arrived, request_seen) = mpsc::channel();
    let (go, proceed) = mpsc::channel::<()>();
    let (delivered, outcome) = mpsc::channel();
    let (arrived, proceed) = (Mutex::new(arrived), Mutex::new(proceed));
    answer_with(server, move |incoming| {
        let _ = arrived.lock().unwrap().send(());
        // Hold the request until the client is gone, as a wait would.
        let _ = proceed.lock().unwrap().recv();
        let sent = incoming.reply.send(Reply::Help {
            text: "late".into(),
        });
        let _ = delivered.send(sent.is_ok());
    });
    let stream = UnixStream::connect(&socket).unwrap();
    let request = serde_json::to_vec(&Request::new("t", Command::Help)).unwrap();
    {
        use std::io::Write;
        let mut writer = &stream;
        writer.write_all(&request).unwrap();
        writer.write_all(b"\n").unwrap();
    }
    request_seen.recv_timeout(Duration::from_secs(5)).unwrap();
    drop(stream);
    // The connection thread checks the socket twice a second; once it
    // sees the client gone it drops its end, so the send fails and a wait
    // would keep its events unreported.
    std::thread::sleep(Duration::from_millis(1500));
    go.send(()).unwrap();
    assert!(!outcome.recv_timeout(Duration::from_secs(5)).unwrap());
}

#[test]
fn dialogs_open_only_when_nothing_else_is_pending_or_open() {
    assert_eq!(dialog_gate_refusal(false, false, None), None);
    let pending = dialog_gate_refusal(true, false, Some("Settings")).unwrap();
    assert!(
        pending.contains("Another dialog from this Lead"),
        "{pending}"
    );
    let open = dialog_gate_refusal(false, false, Some("Settings")).unwrap();
    assert!(
        open.contains("Settings open") && open.contains("try again later"),
        "{open}"
    );
    let busy = dialog_gate_refusal(false, true, None).unwrap();
    assert!(busy.contains("busy"), "{busy}");
}

#[test]
fn pr_is_gated_only_by_busy_and_the_authors_dialogs_on_that_task() {
    assert_eq!(pr_gate_refusal("a", false, None), None);
    let open = pr_gate_refusal("a", true, Some("the Create PR dialog")).unwrap();
    assert_eq!(
        open,
        "The author has the Create PR dialog open for a. Try again after they finish it."
    );
    let busy = pr_gate_refusal("a", true, None).unwrap();
    assert!(busy.contains("busy"), "{busy}");
}

#[test]
fn pr_results_map_to_replies_and_exit_codes() {
    use shika_core::Error;
    let url = "https://github.com/o/r/pull/7".to_string();
    let done = pr_reply(Ok(url.clone()));
    assert_eq!(done, Reply::Done { message: url });
    assert_eq!(done.exit_code(), 0);
    // What only the author can resolve before publishing is a refusal.
    for error in [
        Error::PublishNoTarget(Some("dev".into())),
        Error::PublishNoTarget(None),
        Error::TaskBranchChanged {
            expected: "a".into(),
            current: "b".into(),
        },
    ] {
        let reply = pr_reply(Err(error.clone()));
        assert_eq!(reply, refused(error.to_string()));
        assert_eq!(reply.exit_code(), 1);
    }
    let no_target = render_text(&pr_reply(Err(Error::PublishNoTarget(Some("dev".into())))));
    assert!(
        no_target.contains("dev") && no_target.contains("Create PR"),
        "{no_target}"
    );
    // A failed step is an error that says what was kept.
    let failed = pr_reply(Err(Error::Publish("push rejected".into())));
    assert_eq!(failed.exit_code(), 2);
    assert_eq!(
        render_text(&failed),
        "error: Could not publish PR: push rejected Completed commits and pushes were kept."
    );
    assert_eq!(
        pr_reply(Err(Error::UnknownSession)),
        Reply::Error {
            message: "The task is no longer in Shika.".into()
        }
    );
}

#[test]
fn pr_needs_a_task_that_finished_a_turn() {
    for status in [
        TaskStatus::Working,
        TaskStatus::Starting,
        TaskStatus::Asking,
        TaskStatus::Exited,
    ] {
        let reason = pr_status_refusal("a", status).expect("refused");
        assert!(reason.starts_with("a "), "{reason}");
    }
    assert_eq!(pr_status_refusal("a", TaskStatus::Ready), None);
    assert_eq!(pr_status_refusal("a", TaskStatus::Waiting), None);
}

#[test]
fn dialog_outcomes_map_to_replies_and_exit_codes() {
    let closed = close_outcome(2).to_string();
    let done = dialog_reply(Some(closed.clone()), None, true, true);
    assert_eq!(
        done,
        Reply::Done {
            message: closed.clone()
        }
    );
    assert_eq!(done.exit_code(), 0);
    // Success wins over a failure the author then retried past.
    assert_eq!(
        dialog_reply(Some(closed.clone()), Some("x".into()), true, true),
        done
    );
    // A failure the author gave up on is an error with its text.
    let failed = dialog_reply(None, Some("Could not push changes.".into()), true, true);
    assert_eq!(failed.exit_code(), 2);
    assert!(matches!(&failed, Reply::Error { message } if message == "Could not push changes."));
    // Cancelling is a refusal with a fixed reason.
    let cancelled = dialog_reply(None, None, true, true);
    assert_eq!(cancelled, refused(crate::lead_steps::CANCELLED));
    assert_eq!(cancelled.exit_code(), 1);
    // The dialog went away under it.
    assert_eq!(dialog_reply(None, None, false, true).exit_code(), 2);
    assert_eq!(dialog_reply(None, None, true, false).exit_code(), 2);
    // Success is still success if the Lead closed afterwards.
    assert_eq!(
        dialog_reply(Some("closed".into()), None, false, false).exit_code(),
        0
    );
}

#[test]
fn close_outcomes_say_what_the_author_chose() {
    assert_eq!(close_outcome(0), "closed");
    for action in 1..=3 {
        assert!(close_outcome(action).starts_with("closed ("), "{action}");
    }
    assert_ne!(close_outcome(1), close_outcome(2));
    assert_ne!(close_outcome(2), close_outcome(3));
}

#[test]
fn a_pr_or_close_blocks_until_it_ends_and_always_gets_an_answer() {
    let server = start_server();
    let socket = server.socket();
    answer_with(server, |incoming| {
        // The stand-in UI thread: publishing, or the author, takes a
        // moment.
        std::thread::sleep(Duration::from_millis(600));
        let reply = match incoming.command {
            Command::Pr {
                task,
                title: Some(title),
                body: Some(body),
            } if task == "ship" && title == "Fix \"login\"" && body == "Why\n- tested" => {
                Reply::Done {
                    message: "https://github.com/o/r/pull/7".into(),
                }
            }
            Command::Pr { .. } => {
                refused(shika_core::Error::PublishNoTarget(Some("dev".into())).to_string())
            }
            Command::Close { task } if task == "gone" => {
                // The card vanished: the pending reply is dropped.
                drop(incoming.reply);
                return;
            }
            Command::Close { .. } => Reply::Done {
                message: "closed".into(),
            },
            _ => Reply::Help { text: "hi".into() },
        };
        let _ = incoming.reply.send(reply);
    });
    let ask = |command| send(&socket, &Request::new("t", command)).unwrap();
    let shipped = ask(Command::Pr {
        task: "ship".into(),
        title: Some("Fix \"login\"".into()),
        body: Some("Why\n- tested".into()),
    });
    assert_eq!(render_text(&shipped), "https://github.com/o/r/pull/7");
    let no_target = ask(Command::Pr {
        task: "other".into(),
        title: None,
        body: None,
    });
    assert_eq!(no_target.exit_code(), 1);
    assert!(render_text(&no_target).starts_with("refused: The task's starting base dev"));
    assert_eq!(
        render_text(&ask(Command::Close { task: "a".into() })),
        "closed"
    );
    let dropped = ask(Command::Close {
        task: "gone".into(),
    });
    assert_eq!(dropped.exit_code(), 2);
}

#[test]
fn dropping_the_server_removes_its_directory() {
    let server = start_server();
    let socket = server.socket();
    assert!(socket.exists());
    drop(server);
    assert!(!socket.exists());
}

#[test]
fn launch_failures_split_into_refusals_and_errors() {
    use shika_core::Error;
    assert_eq!(
        failure_reply(&Error::InvalidPrompt("x".into())).exit_code(),
        1
    );
    assert_eq!(
        failure_reply(&Error::NoSuchBranch("x".into())).exit_code(),
        1
    );
    assert_eq!(failure_reply(&Error::UnknownCli).exit_code(), 2);
    // A declined note is a refusal with its reason; a bad file is an
    // error that names the file.
    assert_eq!(
        failure_reply(&Error::MemoryRefused("Memory is full.".into())),
        refused("Memory is full.")
    );
    let unreadable = failure_reply(&Error::MemoryUnreadable("/d/leads/p/memory.json".into()));
    assert_eq!(unreadable.exit_code(), 2);
    assert!(render_text(&unreadable).contains("/d/leads/p/memory.json"));
}

fn gate() -> Gate {
    Gate {
        waiting: false,
        idle: true,
        draft: false,
        last_typed: None,
        alive: true,
    }
}

fn at(t0: Instant, secs: f64) -> Instant {
    t0 + Duration::from_secs_f64(secs)
}

fn ids(due: &[&Observed]) -> Vec<String> {
    due.iter().map(|task| task.info.id.clone()).collect()
}

#[test]
fn the_doorbell_batches_settlements_then_rings_once() {
    let t0 = Instant::now();
    let turn = Some(t0);
    let reported = Ledger::default();
    let mut bell = Doorbell::default();
    let tasks = [observed("a", TaskStatus::Ready, turn)];
    // Nothing rings before the batch window ends.
    assert!(bell.due(&reported, &tasks, &gate(), t0).is_empty());
    assert!(bell.due(&reported, &tasks, &gate(), at(t0, 1.0)).is_empty());
    // A second worker settling inside the window joins the same ring.
    let tasks = [
        observed("a", TaskStatus::Ready, turn),
        observed("b", TaskStatus::Asking, turn),
        observed("c", TaskStatus::Working, turn),
    ];
    let due = bell.due(&reported, &tasks, &gate(), at(t0, 1.6));
    assert_eq!(ids(&due), ["a", "b"]);
    bell.rang(&due);
    // Never twice for the same (task, status, turn).
    assert!(bell.due(&reported, &tasks, &gate(), at(t0, 9.0)).is_empty());
    // Asking then Ready is a different status: rings again, after its own batch.
    let tasks = [observed("b", TaskStatus::Ready, turn)];
    assert!(
        bell.due(&reported, &tasks, &gate(), at(t0, 10.0))
            .is_empty()
    );
    let due = bell.due(&reported, &tasks, &gate(), at(t0, 11.6));
    assert_eq!(ids(&due), ["b"]);
    bell.rang(&due);
    // A new turn of the same task rings again.
    let again = [observed("a", TaskStatus::Ready, Some(at(t0, 20.0)))];
    assert!(
        bell.due(&reported, &again, &gate(), at(t0, 21.0))
            .is_empty()
    );
    assert_eq!(
        ids(&bell.due(&reported, &again, &gate(), at(t0, 22.6))),
        ["a"]
    );
}

#[test]
fn the_doorbell_skips_what_a_wait_reported_and_what_is_not_settled() {
    let t0 = Instant::now();
    let turn = Some(t0);
    let mut reported = Ledger::default();
    reported.mark(vec![("a".into(), (TaskStatus::Ready, turn, None))]);
    let mut bell = Doorbell::default();
    let tasks = [
        observed("a", TaskStatus::Ready, turn),
        observed("w", TaskStatus::Waiting, None),
        observed("s", TaskStatus::Starting, None),
    ];
    assert!(bell.due(&reported, &tasks, &gate(), at(t0, 5.0)).is_empty());
    let tasks = [observed("x", TaskStatus::Exited, turn)];
    bell.due(&reported, &tasks, &gate(), t0);
    assert_eq!(
        ids(&bell.due(&reported, &tasks, &gate(), at(t0, 2.0))),
        ["x"]
    );
}

#[test]
fn the_doorbell_waits_for_every_condition_and_rings_when_they_clear() {
    let t0 = Instant::now();
    let turn = Some(t0);
    let reported = Ledger::default();
    let tasks = [observed("a", TaskStatus::Ready, turn)];
    let closed = |gate: Gate| {
        let mut bell = Doorbell::default();
        bell.due(&reported, &tasks, &gate, t0);
        assert!(bell.due(&reported, &tasks, &gate, at(t0, 2.0)).is_empty());
        // The condition clears: it rings at once, the batch window is over.
        assert_eq!(
            ids(&bell.due(&reported, &tasks, &Gate { ..self::gate() }, at(t0, 2.1))),
            ["a"]
        );
    };
    closed(Gate {
        waiting: true,
        ..gate()
    });
    closed(Gate {
        idle: false,
        ..gate()
    });
    closed(Gate {
        draft: true,
        ..gate()
    });
    closed(Gate {
        alive: false,
        ..gate()
    });
    closed(Gate {
        last_typed: Some(at(t0, 0.5)),
        ..gate()
    });
    // Typing is quiet after three seconds.
    let typed = Gate {
        last_typed: Some(t0),
        ..gate()
    };
    assert!(typed.open(at(t0, 3.1)));
    assert!(!typed.open(at(t0, 2.9)));
}

#[test]
fn a_failed_ring_can_be_tried_again() {
    let t0 = Instant::now();
    let turn = Some(t0);
    let reported = Ledger::default();
    let tasks = [observed("a", TaskStatus::Ready, turn)];
    let mut bell = Doorbell::default();
    bell.due(&reported, &tasks, &gate(), t0);
    let due = bell.due(&reported, &tasks, &gate(), at(t0, 2.0));
    bell.rang(&due);
    bell.forget(&["a".to_string()]);
    bell.due(&reported, &tasks, &gate(), at(t0, 3.0));
    assert_eq!(
        ids(&bell.due(&reported, &tasks, &gate(), at(t0, 4.6))),
        ["a"]
    );
}

#[test]
fn the_doorbell_line_is_short_and_names_each_task() {
    let mut long = task("18d0b57", TaskStatus::Ready);
    long.title = "Add slugify to textkit with a very long title that goes on".into();
    let mut asking = task("18d0b58", TaskStatus::Asking);
    asking.title = "Add \"word_count\"".into();
    let exited = task("18d0b59", TaskStatus::Exited);
    let line = doorbell_line(&[&long, &asking, &exited]);
    assert_eq!(
        line,
        "[shika] Workers changed: 18d0b57 \"Add slugify to textkit wi...\" is ready; 18d0b58 \"Add 'word_count'\" is asking; 18d0b59 \"18d0b59\" has exited. Run shika wait."
    );
    assert!(!line.contains('\n'));
}

#[test]
fn the_doorbell_marks_reported_tasks() {
    let mut done = task("18d0b57", TaskStatus::Ready);
    done.report = Some("Done. cargo test passes.".into());
    let silent = task("18d0b58", TaskStatus::Ready);
    let line = doorbell_line(&[&done, &silent]);
    assert_eq!(
        line,
        "[shika] Workers changed: 18d0b57 \"18d0b57\" is ready (reported); 18d0b58 \"18d0b58\" is ready. Run shika wait."
    );
    assert!(!line.contains("Done."), "the bell never carries the report");
}

#[test]
fn a_worker_may_only_report_or_ask_for_help() {
    let turn = Some(Instant::now());
    let (reply, stored) = worker_command(
        Command::Report {
            text: "  Added the form.\x07 Tests pass. ".into(),
        },
        turn,
    );
    assert_eq!(
        reply,
        Reply::Done {
            message: "reported".into()
        }
    );
    let stored = stored.expect("stored");
    assert_eq!(stored.text, "Added the form. Tests pass.");
    assert_eq!(stored.turn, turn);

    let (reply, stored) = worker_command(
        Command::Report {
            text: "x".repeat(shika_core::control::MAX_REPORT_BYTES + 1),
        },
        turn,
    );
    assert_eq!(reply.exit_code(), 1);
    assert!(stored.is_none());

    let (reply, _) = worker_command(Command::Help, None);
    assert!(matches!(reply, Reply::Help { text } if text.lines().count() == 3));

    for command in [
        Command::Tasks,
        Command::Status { task: "a".into() },
        Command::New {
            cli: "claude".into(),
            base: None,
            prompt: "x".into(),
        },
        Command::Wait {
            tasks: vec![],
            timeout_secs: 1,
        },
        Command::Read {
            task: "a".into(),
            lines: 0,
        },
        Command::Diff {
            task: "a".into(),
            stat: false,
        },
        Command::Send {
            task: "a".into(),
            text: "x".into(),
            enter: true,
        },
        Command::Key {
            task: "a".into(),
            keys: vec!["enter".into()],
        },
        Command::Pr {
            task: "a".into(),
            title: None,
            body: None,
        },
        Command::Close { task: "a".into() },
        Command::Memory,
        Command::Remember { text: "x".into() },
        Command::Forget { note: 1 },
    ] {
        let (reply, stored) = worker_command(command, turn);
        assert_eq!(
            reply,
            refused("Workers can only run shika report."),
            "a worker ran a Lead command"
        );
        assert!(stored.is_none());
    }
}

#[test]
fn the_worker_footer_follows_the_prompt_and_leaves_its_first_line_alone() {
    let bin_dir = Path::new("/tmp/control/bin");
    let prompt = worker_prompt("Fix the login bug\n\nScope: auth only.\n", bin_dir);
    assert!(prompt.starts_with("Fix the login bug\n\nScope: auth only.\n\nWhen you finish"));
    assert!(prompt.contains("'/tmp/control/bin/shika' report") && prompt.contains("Do not push"));
    assert!(prompt.contains("Wait for it to print reported"));
    assert_eq!(
        crate::model::prompt_title(&prompt).as_deref(),
        Some("Fix the login bug")
    );
    // Still a valid positional prompt: it does not start with a dash.
    assert!(!worker_prompt("ok", bin_dir).starts_with('-'));
}

#[test]
fn typing_into_a_worker_is_refused_unless_it_is_safe() {
    let state = |status, draft| InputState {
        status,
        draft,
        publishing: false,
    };
    let ok = |status| input_refusal("t", &state(status, false), false);
    assert_eq!(ok(TaskStatus::Ready), None);
    assert_eq!(ok(TaskStatus::Asking), None);
    assert_eq!(ok(TaskStatus::Waiting), None);
    assert!(
        ok(TaskStatus::Working)
            .unwrap()
            .contains("shika key t escape")
    );
    assert!(ok(TaskStatus::Exited).unwrap().contains("has exited"));
    assert!(ok(TaskStatus::Starting).unwrap().contains("starting"));
    // Escape may interrupt a working task.
    assert_eq!(
        input_refusal("t", &state(TaskStatus::Working, false), true),
        None
    );
    // The author's unsent draft is never appended to, even by an interrupt.
    let refusal = input_refusal("t", &state(TaskStatus::Ready, true), false).unwrap();
    assert!(refusal.contains("author has typed"), "{refusal}");
    assert!(input_refusal("t", &state(TaskStatus::Working, true), true).is_some());
    // Nothing types into a worker a `shika pr` is publishing.
    for (status, escape_only) in [(TaskStatus::Ready, false), (TaskStatus::Waiting, true)] {
        let publishing = InputState {
            status,
            draft: false,
            publishing: true,
        };
        assert_eq!(
            input_refusal("t", &publishing, escape_only).as_deref(),
            Some(LEAD_PUBLISHING)
        );
    }
}

#[test]
fn keys_encode_for_the_programs_mode() {
    let normal = Modes::default();
    let app = Modes {
        app_cursor: true,
        ..Modes::default()
    };
    assert_eq!(key_bytes("enter", &normal).unwrap(), b"\r");
    assert_eq!(key_bytes("escape", &normal).unwrap(), b"\x1b");
    assert_eq!(key_bytes("tab", &normal).unwrap(), b"\t");
    assert_eq!(key_bytes("space", &normal).unwrap(), b" ");
    assert_eq!(key_bytes("backspace", &normal).unwrap(), b"\x7f");
    assert_eq!(key_bytes("y", &normal).unwrap(), b"y");
    assert_eq!(key_bytes("7", &normal).unwrap(), b"7");
    assert_eq!(key_bytes("down", &normal).unwrap(), b"\x1b[B");
    assert_eq!(key_bytes("down", &app).unwrap(), b"\x1bOB");
    assert_eq!(key_bytes("left", &app).unwrap(), b"\x1bOD");
    assert_eq!(key_bytes("f1", &normal), None);
    assert_eq!(key_bytes("Y", &normal), None);
    let steps = key_steps(&["down".into(), "enter".into()], &normal).unwrap();
    assert_eq!(steps.len(), 2);
    assert_eq!(steps[0].delay, Duration::ZERO);
    assert_eq!(steps[1].delay, KEY_DELAY);
    assert!(steps.iter().all(|step| !step.count));
    assert!(key_steps(&["nope".into()], &normal).is_err());
}

#[test]
fn a_pasted_line_and_its_separate_enter_are_one_submission() {
    let bracketed = Modes {
        bracketed_paste: true,
        ..Modes::default()
    };
    let steps = paste_steps("Run the tests\nthen commit \x1b[31m", true, &bracketed);
    assert_eq!(steps.len(), 2);
    assert_eq!(
        steps[0].bytes,
        b"\x1b[200~Run the tests\rthen commit [31m\x1b[201~"
    );
    assert_eq!(steps[1].bytes, b"\r");
    assert!(steps[1].delay >= Duration::from_millis(100));
    assert!(steps.iter().all(|step| step.count));
    // The same capture a typed submission goes through counts it once,
    // and leaves no draft behind.
    let mut host = HostState::default();
    let now = Instant::now();
    host.capture_typed(&steps[0].bytes, now);
    assert!(host.has_draft());
    assert_eq!(host.submission, 0);
    host.capture_typed(&steps[1].bytes, now);
    assert_eq!(host.submission, 1);
    assert_eq!(host.last_submission, Some(now));
    assert!(!host.has_draft());

    // Without Enter it is not a submission and is not a captured draft.
    let steps = paste_steps("1", false, &bracketed);
    assert_eq!(steps.len(), 1);
    assert!(!steps[0].count);
    // Without bracketed paste, line breaks cannot submit early.
    let plain = paste_steps("a\nb", true, &Modes::default());
    assert_eq!(plain[0].bytes, b"a b");
}

#[test]
fn injected_steps_are_refused_after_the_author_types_or_the_cli_exits() {
    let path = std::env::temp_dir().join(format!(
        "shika-inject-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    ));
    let core = shika_core::Core::open(&path).unwrap();
    let step = Step {
        bytes: b"x".to_vec(),
        count: true,
        delay: Duration::ZERO,
    };
    let mut host = HostState::default();
    // No PTY: nothing to write into.
    assert!(
        host.inject(&core, &step, None)
            .unwrap_err()
            .contains("exited")
    );
    assert_eq!(host.submission, 0);
    // The author typed after the typing began.
    host.last_typed = Some(Instant::now());
    assert!(
        host.inject(&core, &step, None)
            .unwrap_err()
            .contains("author typed")
    );
    assert!(!host.has_draft());
    std::fs::remove_dir_all(path).unwrap();
}

#[test]
fn read_text_drops_trailing_blank_lines_and_spaces() {
    let lines = ["first  ", "", "  indented", "   ", ""]
        .map(String::from)
        .to_vec();
    assert_eq!(screen_text(lines), "first\n\n  indented");
    assert_eq!(
        screen_text(vec!["".into(), " ".into()]),
        "(the terminal is empty)"
    );
}
