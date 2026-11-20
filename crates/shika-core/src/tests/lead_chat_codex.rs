use super::*;

const THREAD: &str = "00000000-0000-7000-8000-0000000000aa";

/// A `codex app-server` stand-in on a scratch PATH: logs every line it reads
/// to `<scratch>/codex.log`, answers the handshake with one fixed thread, and
/// writes that thread's rollout when a turn starts, as Codex does. Each turn
/// sends commentary, a tool, the final message, a subagent's message and a
/// retried error; `please-wait` and `please-run` instead wait for an
/// interrupt, the second with a command still running.
fn codex_script(scratch: &Scratch) -> String {
    let log = scratch.path.join("codex.log");
    let sessions = scratch.path.join("home/.codex/sessions/2026/11/20");
    format!(
        r#"#!/bin/sh
log='{log}'
printf 'args:%s\n' "$*" >> "$log"
printf 'sock:%s\n' "$SHIKA_SOCKET" >> "$log"
T={THREAD}
turn=
item() {{
 printf '{{"method":"item/%s","params":{{"threadId":"%s","turnId":"%s","item":%s}}}}\n' "$1" "$2" "$turn" "$3"
}}
while IFS= read -r line; do
 printf '%s\n' "$line" >> "$log"
 id=$(printf '%s\n' "$line" | sed -n 's/^{{"id":\([0-9]*\),.*/\1/p')
 case "$line" in
 *'"method":"initialize"'*)
  printf '{{"id":%s,"result":{{"userAgent":"shika/0.162.1 (fake)","codexHome":"/fake"}}}}\n' "$id";;
 *'"method":"thread/start"'*|*'"method":"thread/resume"'*)
  printf '{{"id":%s,"result":{{"thread":{{"id":"%s","source":"vscode"}}}}}}\n' "$id" "$T";;
 *'"method":"thread/inject_items"'*)
  printf '{{"id":%s,"result":{{}}}}\n' "$id";;
 *'"method":"turn/interrupt"'*)
  printf '{{"id":%s,"result":{{}}}}\n' "$id"
  printf '{{"method":"turn/completed","params":{{"threadId":"%s","turn":{{"id":"%s","status":"interrupted"}}}}}}\n' "$T" "$turn";;
 *'"method":"turn/start"'*)
  mkdir -p '{sessions}' && : > '{sessions}/rollout-2026-11-20T00-00-00-'"$T"'.jsonl'
  turn="turn-$id"
  printf '{{"id":%s,"result":{{"turn":{{"id":"%s","status":"inProgress"}}}}}}\n' "$id" "$turn"
  printf '{{"method":"turn/started","params":{{"threadId":"%s","turn":{{"id":"%s"}}}}}}\n' "$T" "$turn"
  item completed "$T" '{{"type":"agentMessage","id":"m1","text":"private commentary","phase":"commentary"}}'
  case "$line" in
  *please-wait*) ;;
  *please-run*) item started "$T" '{{"type":"commandExecution","id":"c1"}}';;
  *)
   item started "$T" '{{"type":"commandExecution","id":"c2"}}'
   item completed "$T" '{{"type":"commandExecution","id":"c2"}}'
   printf '{{"method":"error","params":{{"threadId":"%s","turnId":"%s","willRetry":true,"error":{{"message":"Reconnecting... 1/2"}}}}}}\n' "$T" "$turn"
   item completed "$T" '{{"type":"agentMessage","id":"m2","text":"Final reply","phase":null}}'
   item completed subagent-thread '{{"type":"agentMessage","id":"m3","text":"private child","phase":"final_answer"}}'
   printf '{{"method":"turn/completed","params":{{"threadId":"%s","turn":{{"id":"%s","status":"completed","items":[{{"type":"agentMessage","text":"private aggregate"}}]}}}}}}\n' "$T" "$turn";;
  esac;;
 esac
done
"#,
        log = log.display(),
        sessions = sessions.display(),
    )
}

fn collect(core: &Core, lead: &str, done: impl Fn(&ChatEvent) -> bool) -> Vec<ChatEvent> {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut events = Vec::new();
    loop {
        let batch = core.lead_chat_events(lead);
        let found = batch.iter().any(&done);
        events.extend(batch);
        if found {
            return events;
        }
        assert!(Instant::now() < deadline, "chat event deadline: {events:?}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn ready(event: &ChatEvent) -> bool {
    matches!(event, ChatEvent::Status(ChatStatus::Ready))
}

fn send(core: &Core, lead: &str, text: &str) {
    core.lead_chat_send(lead, text.into(), ChatOrigin::Author, Vec::new())
        .unwrap();
}

/// Lines of the fake's log for one method.
fn calls(scratch: &Scratch, method: &str) -> Vec<String> {
    let needle = format!(r#""method":"{method}""#);
    fs::read_to_string(scratch.path.join("codex.log"))
        .unwrap_or_default()
        .lines()
        .filter(|line| line.contains(&needle))
        .map(str::to_string)
        .collect()
}

#[test]
fn codex_chat_confirms_the_handshake_thread_and_resumes_it_with_new_memory_only() {
    let scratch = Scratch::new();
    let mut core = core_with_switch_clis(&scratch);
    core.cli_home = Some(CliHome::at(scratch.path.join("home")));
    write_exec(&scratch.path, "codex", &codex_script(&scratch));
    let project = core
        .add_project(&scratch.repo("codex-chat-demo"))
        .unwrap()
        .project;
    core.remember(&project.id, "Keep this note").unwrap();
    let start = core
        .start_chat_lead(&project.id, "codex", lead_launch(&scratch))
        .unwrap();
    let lead = start.session.id.clone();
    assert_eq!(start.session.pty, None);
    assert_eq!(
        start.conversation,
        LeadConversation::Fresh(FreshReason::NoSavedSession)
    );
    // Nothing is saved until Codex keeps the thread, at its first turn.
    assert_eq!(
        core.saved_lead(&project.id).unwrap().unwrap().session_id,
        None
    );
    assert_eq!(
        core.discover_lead_session(&lead, start.started_at),
        Ok(None)
    );
    let history = core
        .saved_lead(&project.id)
        .unwrap()
        .unwrap()
        .chat_history
        .unwrap();

    send(&core, &lead, "Hello");
    let events = collect(&core, &lead, ready);
    let confirmed = events
        .iter()
        .find_map(|e| match e {
            ChatEvent::Confirmed(id) => Some(id.clone()),
            _ => None,
        })
        .expect("the first turn confirms the thread");
    assert_eq!(confirmed, THREAD);
    core.confirm_lead_chat(&lead, &confirmed).unwrap();
    assert_eq!(
        core.saved_lead(&project.id)
            .unwrap()
            .unwrap()
            .session_id
            .as_deref(),
        Some(THREAD)
    );
    let page = core.lead_chat_page(&project.id, None).unwrap();
    assert_eq!(
        page.iter()
            .filter_map(|r| r.content.text())
            .collect::<Vec<_>>(),
        ["Hello", "Final reply"]
    );
    assert!(!format!("{page:?}").contains("private"));

    let args = fs::read_to_string(scratch.path.join("codex.log")).unwrap();
    assert!(
        args.contains(
            "args:app-server --enable default_mode_request_user_input --disable goals -c projects={"
        ),
        "{args}"
    );
    assert!(args.contains(&format!(
        "sock:{}",
        scratch.path.join("control/sock").display()
    )));
    let started = calls(&scratch, "thread/start");
    assert_eq!(started.len(), 1);
    assert!(started[0].contains("Keep this note"), "{}", started[0]);

    let (worker_sink, _worker_output) = channel_sink();
    let worker = core
        .create_session(
            &project.id,
            "claude",
            PtySize::default(),
            worker_sink,
            LaunchOptions {
                started_by: Some(lead.clone()),
                ..Default::default()
            },
        )
        .unwrap();

    // Stop generating with nothing running keeps the process.
    send(&core, &lead, "please-wait");
    collect(&core, &lead, |e| matches!(e, ChatEvent::Accepted));
    core.lead_chat_interrupt(&lead);
    let events = collect(&core, &lead, ready);
    assert!(!events.iter().any(|e| matches!(e, ChatEvent::Exit)));
    assert_eq!(calls(&scratch, "turn/interrupt").len(), 1);
    assert_eq!(calls(&scratch, "initialize").len(), 1);

    // With a command still running, the process ends so the command does;
    // the next send resumes the same thread in a new one, without resending
    // instructions it already holds.
    send(&core, &lead, "please-run");
    collect(&core, &lead, |e| matches!(e, ChatEvent::Accepted));
    let now = Instant::now();
    core.lead_chat_interrupt(&lead);
    collect(&core, &lead, ready);
    assert!(
        now.elapsed() < Duration::from_secs(2),
        "SIGTERM, not a kill deadline"
    );
    send(&core, &lead, "Continue");
    let events = collect(&core, &lead, ready);
    assert!(events.iter().any(|e| matches!(e, ChatEvent::Resuming)));
    assert_eq!(calls(&scratch, "initialize").len(), 2);
    assert_eq!(calls(&scratch, "thread/resume").len(), 1);
    assert!(calls(&scratch, "thread/resume")[0].contains(THREAD));
    assert!(calls(&scratch, "thread/inject_items").is_empty());
    let texts = core
        .lead_chat_page(&project.id, None)
        .unwrap()
        .iter()
        .filter_map(|r| r.content.text().map(str::to_string))
        .collect::<Vec<_>>();
    assert_eq!(
        texts,
        [
            "Hello",
            "Final reply",
            "please-wait",
            "Reply stopped",
            "please-run",
            "Reply stopped",
            "Continue",
            "Final reply"
        ]
    );

    core.stop_lead(&lead).unwrap();
    assert!(
        core.write(worker.pty.unwrap(), b"still running\n").is_ok(),
        "chat interruption and Stop must not stop worker groups"
    );

    // A new note reaches the resumed thread once, as a developer message.
    core.remember(&project.id, "Prefer small workers").unwrap();
    let start = core
        .start_chat_lead(&project.id, "codex", lead_launch(&scratch))
        .unwrap();
    assert_eq!(start.conversation, LeadConversation::Resumed);
    send(&core, &start.session.id, "Hello again");
    collect(&core, &start.session.id, ready);
    let injected = calls(&scratch, "thread/inject_items");
    assert_eq!(injected.len(), 1);
    assert!(
        injected[0].contains(r#""role":"developer""#),
        "{}",
        injected[0]
    );
    assert!(
        injected[0].contains("Prefer small workers"),
        "{}",
        injected[0]
    );
    core.stop_lead(&start.session.id).unwrap();
    let start = core
        .start_chat_lead(&project.id, "codex", lead_launch(&scratch))
        .unwrap();
    send(&core, &start.session.id, "And again");
    collect(&core, &start.session.id, ready);
    assert_eq!(calls(&scratch, "thread/inject_items").len(), 1);
    assert_eq!(calls(&scratch, "thread/resume").len(), 3);
    core.stop_lead(&start.session.id).unwrap();

    assert_eq!(
        core.saved_lead(&project.id)
            .unwrap()
            .unwrap()
            .chat_history
            .as_deref(),
        Some(history.as_str())
    );
    assert_eq!(
        core.confirm_lead_chat(&lead, &confirmed),
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
    assert_eq!(core.lead_memory(&project.id).unwrap().len(), 2);
}

#[test]
fn a_codex_chat_lead_quits_with_its_app_server() {
    let scratch = Scratch::new();
    let mut core = core_with_switch_clis(&scratch);
    core.cli_home = Some(CliHome::at(scratch.path.join("home")));
    write_exec(&scratch.path, "codex", &codex_script(&scratch));
    let project = core
        .add_project(&scratch.repo("codex-quit-demo"))
        .unwrap()
        .project;
    let start = core
        .start_chat_lead(&project.id, "codex", lead_launch(&scratch))
        .unwrap();
    send(&core, &start.session.id, "please-run");
    collect(&core, &start.session.id, |e| {
        matches!(e, ChatEvent::Accepted)
    });
    let now = Instant::now();
    core.stop_lead_chats();
    assert!(now.elapsed() < Duration::from_secs(4));
    // Stdin closed with a turn running: the turn settled as stopped.
    let page = core.lead_chat_page(&project.id, None).unwrap();
    assert_eq!(page.last().unwrap().content.text(), Some("Reply stopped"));
}
