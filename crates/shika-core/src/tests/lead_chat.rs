use super::*;

/// Every CLI prints its arguments and environment (`SWITCH_CLI`), and the
/// Core reads CLI files from a scratch home, never the real one.
#[test]
fn chat_interrupt_cancels_a_blocked_pipe_write_and_quit_reaps_the_child() {
    let scratch = Scratch::new();
    let mut core = core_with_switch_clis(&scratch);
    core.cli_home = Some(CliHome::at(scratch.path.join("home")));
    write_exec(&scratch.path, "claude", "#!/bin/sh\nsleep 30\n");
    let project = core
        .add_project(&scratch.repo("pipe-demo"))
        .unwrap()
        .project;
    let start = core
        .start_claude_lead(&project.id, lead_launch(&scratch))
        .unwrap();
    core.lead_chat_send(
        &start.session.id,
        "large input ".repeat(100_000),
        ChatOrigin::Author,
        Vec::new(),
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(7);
    loop {
        if core
            .lead_chat_events(&start.session.id)
            .iter()
            .any(|e| matches!(e, ChatEvent::Record(_)))
        {
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    core.lead_chat_interrupt(&start.session.id);
    loop {
        if core
            .lead_chat_events(&start.session.id)
            .iter()
            .any(|e| matches!(e, ChatEvent::Status(ChatStatus::Ready)))
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "blocked stdin must be interruptible"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let now = Instant::now();
    core.stop_lead_chats();
    assert!(now.elapsed() < Duration::from_secs(4));
}

#[test]
fn claude_chat_has_no_pty_confirms_identity_and_keeps_history_until_forget() {
    let scratch = Scratch::new();
    let mut core = core_with_switch_clis(&scratch);
    core.cli_home = Some(CliHome::at(scratch.path.join("home")));
    write_exec(
        &scratch.path,
        "claude",
        r#"#!/bin/sh
id=
while [ "$#" -gt 0 ]; do
 case "$1" in --session-id|--resume) id="$2"; shift;; esac
 shift
done
while IFS= read -r line; do
 case "$line" in
 *shika-initialize*) printf '{"type":"control_response","response":{"request_id":"shika-initialize","subtype":"success"}}\n';;
 *'"type":"user"'*)
 printf '{"type":"system","subtype":"init","session_id":"%s"}\n' "$id"
 case "$line" in *please-interrupt*) sleep 30;; esac
 printf '{"type":"assistant","session_id":"%s","message":{"id":"m1","content":[{"type":"text","text":"private commentary"}]}}\n' "$id"
 printf '{"type":"assistant","session_id":"%s","message":{"id":"m1","content":[{"type":"tool_use","id":"t1","name":"Bash","input":{}}]}}\n' "$id"
 printf '{"type":"assistant","session_id":"%s","message":{"id":"m2","content":[{"type":"text","text":"Final reply"}]}}\n' "$id"
 printf '{"type":"result","session_id":"%s","subtype":"success","is_error":false,"result":"private aggregate"}\n' "$id"
 ;;
 esac
done
"#,
    );
    let project = core
        .add_project(&scratch.repo("chat-demo"))
        .unwrap()
        .project;
    core.remember(&project.id, "Keep this note").unwrap();
    let start = core
        .start_claude_lead(&project.id, lead_launch(&scratch))
        .unwrap();
    assert_eq!(start.session.pty, None);
    assert_eq!(
        core.saved_lead(&project.id).unwrap().unwrap().session_id,
        None
    );
    let history = core
        .saved_lead(&project.id)
        .unwrap()
        .unwrap()
        .chat_history
        .unwrap();
    let collect = |predicate: &dyn Fn(&ChatEvent) -> bool| {
        let deadline = Instant::now() + Duration::from_secs(7);
        let mut events = Vec::new();
        loop {
            let batch = core.lead_chat_events(&start.session.id);
            let done = batch.iter().any(predicate);
            events.extend(batch);
            if done {
                return events;
            }
            assert!(Instant::now() < deadline, "chat event deadline");
            std::thread::sleep(Duration::from_millis(10));
        }
    };
    core.lead_chat_send(
        &start.session.id,
        "Hello".into(),
        ChatOrigin::Author,
        Vec::new(),
    )
    .unwrap();
    let events = collect(&|e| matches!(e, ChatEvent::Status(ChatStatus::Ready)));
    let confirmed = events
        .iter()
        .find_map(|e| {
            if let ChatEvent::Confirmed(id) = e {
                Some(id)
            } else {
                None
            }
        })
        .unwrap();
    core.confirm_lead_chat(&start.session.id, confirmed)
        .unwrap();
    let page = core.lead_chat_page(&project.id, None).unwrap();
    assert_eq!(page.len(), 2);
    assert_eq!(page[1].content.text(), Some("Final reply"));
    assert!(!format!("{page:?}").contains("private"));
    let (worker_sink, _worker_output) = channel_sink();
    let worker = core
        .create_session(
            &project.id,
            "claude",
            PtySize::default(),
            worker_sink,
            LaunchOptions {
                started_by: Some(start.session.id.clone()),
                ..Default::default()
            },
        )
        .unwrap();
    core.lead_chat_send(
        &start.session.id,
        "please-interrupt".into(),
        ChatOrigin::Author,
        Vec::new(),
    )
    .unwrap();
    collect(&|e| matches!(e, ChatEvent::Status(ChatStatus::Working)));
    core.lead_chat_interrupt(&start.session.id);
    collect(&|e| matches!(e, ChatEvent::Status(ChatStatus::Ready)));
    assert!(
        core.lead_chat_page(&project.id, None)
            .unwrap()
            .iter()
            .any(|r| r.content.text() == Some("Reply stopped"))
    );
    core.lead_chat_send(
        &start.session.id,
        "Continue".into(),
        ChatOrigin::Author,
        Vec::new(),
    )
    .unwrap();
    collect(&|e| matches!(e, ChatEvent::Status(ChatStatus::Ready)));
    core.stop_lead(&start.session.id).unwrap();
    assert!(
        core.write(worker.pty.unwrap(), b"still running\n").is_ok(),
        "chat interruption and Stop must not stop worker groups"
    );
    assert_eq!(
        core.saved_lead(&project.id)
            .unwrap()
            .unwrap()
            .chat_history
            .as_deref(),
        Some(history.as_str())
    );
    assert!(core.lead_chat_page(&project.id, None).unwrap().len() >= 5);
    assert_eq!(
        core.confirm_lead_chat(&start.session.id, confirmed),
        Err(Error::UnknownSession)
    );
    core.forget_lead(&project.id, false).unwrap();
    assert!(
        !core
            .data_dir()
            .join("leads")
            .join(&project.id)
            .join("chat")
            .exists()
    );
    assert_eq!(core.lead_memory(&project.id).unwrap().len(), 1);
}

/// The shape of `claude-2.1.302-background-local.jsonl`: after the
/// author's turn settles, a background command finishes and the CLI opens
/// a turn of its own with no input from Shika.
#[test]
fn a_turn_the_claude_cli_starts_itself_is_shown_saved_and_numbered() {
    let scratch = Scratch::new();
    let mut core = core_with_switch_clis(&scratch);
    core.cli_home = Some(CliHome::at(scratch.path.join("home")));
    write_exec(
        &scratch.path,
        "claude",
        r#"#!/bin/sh
id=
while [ "$#" -gt 0 ]; do
 case "$1" in --session-id|--resume) id="$2"; shift;; esac
 shift
done
turn() {
 printf '{"type":"system","subtype":"init","session_id":"%s"}\n' "$id"
 printf '{"type":"assistant","session_id":"%s","message":{"id":"%s-a","content":[{"type":"text","text":"private narration"}]}}\n' "$id" "$1"
 printf '{"type":"assistant","session_id":"%s","message":{"id":"%s-a","content":[{"type":"tool_use","id":"%s-t","name":"Bash","input":{}}]}}\n' "$id" "$1" "$1"
 printf '{"type":"user","session_id":"%s","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"%s-t","content":"private tool output"}]}}\n' "$id" "$1"
 printf '{"type":"assistant","session_id":"%s","message":{"id":"%s-b","content":[{"type":"text","text":"%s"}]}}\n' "$id" "$1" "$2"
 printf '{"type":"result","session_id":"%s","subtype":"success","is_error":false,"result":"private aggregate"}\n' "$id"
}
while IFS= read -r line; do
 case "$line" in
 *shika-initialize*) printf '{"type":"control_response","response":{"request_id":"shika-initialize","subtype":"success"}}\n';;
 *start-background*)
 turn m1 "Waiting on the worker."
 sleep 0.3
 printf '{"type":"system","subtype":"task_notification","session_id":"%s","status":"completed"}\n' "$id"
 turn m2 "The worker finished."
 ;;
 *'"type":"user"'*) turn m3 "Next reply";;
 esac
done
"#,
    );
    let project = core
        .add_project(&scratch.repo("background-demo"))
        .unwrap()
        .project;
    let start = core
        .start_claude_lead(&project.id, lead_launch(&scratch))
        .unwrap();
    let collect = |readies: usize| {
        let deadline = Instant::now() + Duration::from_secs(7);
        let mut events = Vec::new();
        loop {
            events.extend(core.lead_chat_events(&start.session.id));
            if events
                .iter()
                .filter(|e| matches!(e, ChatEvent::Status(ChatStatus::Ready)))
                .count()
                >= readies
            {
                return events;
            }
            assert!(Instant::now() < deadline, "chat event deadline");
            std::thread::sleep(Duration::from_millis(10));
        }
    };
    core.lead_chat_send(
        &start.session.id,
        "start-background".into(),
        ChatOrigin::Author,
        Vec::new(),
    )
    .unwrap();
    let events = collect(2);
    let first_ready = events
        .iter()
        .position(|e| matches!(e, ChatEvent::Status(ChatStatus::Ready)))
        .unwrap();
    let after = &events[first_ready + 1..];
    let started = after
        .iter()
        .position(|e| matches!(e, ChatEvent::Started))
        .expect("the CLI's own turn must start a turn");
    assert!(matches!(
        after[started + 1],
        ChatEvent::Status(ChatStatus::Working)
    ));
    let texts = |events: &[ChatEvent]| {
        events
            .iter()
            .filter_map(|e| match e {
                ChatEvent::Record(r) => r.content.text().map(str::to_string),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(texts(after), ["The worker finished."]);
    assert!(
        !after
            .iter()
            .any(|e| matches!(e, ChatEvent::Rejected(_) | ChatEvent::DeliveryFailed)),
        "a turn the CLI started has no delivery to fail"
    );

    let page = core.lead_chat_page(&project.id, None).unwrap();
    let shown = page
        .iter()
        .map(|r| (r.turn, r.content.clone()))
        .collect::<Vec<_>>();
    assert_eq!(
        shown,
        [
            (
                1,
                ChatContent::Author {
                    text: "start-background".into()
                }
            ),
            (
                1,
                ChatContent::Lead {
                    text: "Waiting on the worker.".into()
                }
            ),
            (
                2,
                ChatContent::Lead {
                    text: "The worker finished.".into()
                }
            ),
        ]
    );
    assert!(!format!("{page:?}").contains("private"));
    let db = rusqlite::Connection::open_with_flags(
        core.data_dir()
            .join("leads")
            .join(&project.id)
            .join("chat/history.sqlite"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let turn_two = db
        .prepare("SELECT content FROM records WHERE turn=2 ORDER BY id")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .map(|r| serde_json::from_str::<ChatContent>(&r.unwrap()).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        turn_two,
        [
            ChatContent::Accepted,
            ChatContent::Lead {
                text: "The worker finished.".into()
            },
            ChatContent::Settled
        ],
        "no author bubble and no Outgoing row for the CLI's own turn"
    );
    drop(db);

    core.lead_chat_send(
        &start.session.id,
        "Next".into(),
        ChatOrigin::Author,
        Vec::new(),
    )
    .unwrap();
    collect(1);
    let page = core.lead_chat_page(&project.id, None).unwrap();
    assert_eq!(
        page[page.len() - 2..]
            .iter()
            .map(|r| (r.turn, r.content.text().unwrap()))
            .collect::<Vec<_>>(),
        [(3, "Next"), (3, "Next reply")]
    );
    core.stop_lead(&start.session.id).unwrap();
}
