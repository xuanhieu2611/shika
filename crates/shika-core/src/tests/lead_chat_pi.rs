use super::*;

/// A `pi --mode rpc` stand-in on a scratch PATH: logs its arguments and every
/// line it reads to `<scratch>/pi.log`, answers `get_state` with the
/// `--session-id` it was given and the messages its session file holds, and
/// writes that file under the scratch home when a prompt's user message
/// arrives, as Pi does. A file with no message line counts none, as Pi
/// ignores one it cannot read. A plain
/// prompt runs narration, a tool, a cut stream that is retried, and the
/// final reply; `/handled` is taken by an extension command; `please-ask`
/// opens Shika's question dialog; `please-wait` waits for `abort`.
fn pi_script(scratch: &Scratch) -> String {
    let log = scratch.path.join("pi.log");
    let sessions = scratch.path.join("home/.pi/agent/sessions");
    format!(
        r#"#!/bin/sh
log='{log}'
printf 'args:%s\n' "$*" >> "$log"
printf 'sock:%s\n' "$SHIKA_SOCKET" >> "$log"
sid=
prev=
for a in "$@"; do
 [ "$prev" = --session-id ] && sid=$a
 [ "$prev" = --extension ] && printf 'extension:%s\n' "$a" >> "$log"
 prev=$a
done
folder="{sessions}/--$(pwd -P | sed 's|^/||; s|[/:]|-|g')--"
assistant() {{
 printf '{{"type":"message_end","message":{{"role":"assistant","content":[{{"type":"text","text":"%s"}}],"stopReason":"%s"}}}}\n' "$1" "$2"
}}
started() {{
 printf '{{"id":"%s","type":"response","command":"prompt","success":true,"data":{{"disposition":"started"}}}}\n' "$1"
 echo '{{"type":"agent_start"}}'
 file="$folder/2026-11-20T00-00-00-000Z_$sid.jsonl"
 mkdir -p "$folder" && [ -s "$file" ] || echo '{{"type":"message"}}' > "$file"
 echo '{{"type":"message_end","message":{{"role":"user","content":[{{"type":"text","text":"x"}}]}}}}'
}}
settle() {{
 echo '{{"type":"agent_end","messages":[{{"role":"assistant","content":[{{"type":"text","text":"private aggregate"}}]}}]}}'
 printf '{{"type":"agent_settled","aborted":%s}}\n' "$1"
}}
while IFS= read -r line; do
 printf '%s\n' "$line" >> "$log"
 id=$(printf '%s\n' "$line" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')
 case "$line" in
 *'"type":"get_state"'*)
  count=$(cat "$folder"/*_"$sid".jsonl 2>/dev/null | grep -c '"type":"message"')
  printf '{{"id":"%s","type":"response","command":"get_state","success":true,"data":{{"sessionId":"%s","messageCount":%s}}}}\n' "$id" "$sid" "$count";;
 *'"type":"abort"'*)
  settle true
  printf '{{"id":"%s","type":"response","command":"abort","success":true}}\n' "$id";;
 *'"type":"extension_ui_response"'*'"cancelled":true'*) ;;
 *'"type":"extension_ui_response"'*)
  echo '{{"type":"tool_execution_end","toolCallId":"call-1","toolName":"shika_ask_author","result":{{}}}}'
  assistant "Answered" stop
  settle false;;
 *'"type":"prompt"'*)
  case "$line" in
  *'"message":"/handled"'*)
   printf '{{"id":"%s","type":"response","command":"prompt","success":true,"data":{{"disposition":"handled"}}}}\n' "$id";;
  *please-wait*) started "$id";;
  *please-ask*)
   started "$id"
   echo '{{"type":"tool_execution_start","toolCallId":"call-1","toolName":"shika_ask_author","args":{{"questions":[{{"question":"Which base?","options":[{{"label":"main"}},{{"label":"dev"}}]}}]}}}}'
   echo '{{"type":"extension_ui_request","id":"ui-1","method":"input","title":"shika-question:call-1"}}';;
  *)
   started "$id"
   assistant "private narration" toolUse
   echo '{{"type":"tool_execution_start","toolCallId":"t1","toolName":"bash","args":{{}}}}'
   echo '{{"type":"tool_execution_end","toolCallId":"t1","toolName":"bash","result":{{}}}}'
   assistant "private partial" error
   echo '{{"type":"agent_end","willRetry":true,"messages":[]}}'
   echo '{{"type":"auto_retry_start","attempt":1}}'
   echo '{{"type":"agent_start"}}'
   assistant "Final reply" stop
   settle false;;
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

fn log(scratch: &Scratch) -> String {
    fs::read_to_string(scratch.path.join("pi.log")).unwrap_or_default()
}

/// Lines of the fake's log for one command or response type.
fn calls(scratch: &Scratch, kind: &str) -> Vec<String> {
    let needle = format!(r#""type":"{kind}""#);
    log(scratch)
        .lines()
        .filter(|line| line.contains(&needle))
        .map(str::to_string)
        .collect()
}

fn texts(core: &Core, project: &str) -> Vec<String> {
    core.lead_chat_page(project, None)
        .unwrap()
        .iter()
        .filter_map(|r| r.content.text().map(str::to_string))
        .collect()
}

fn pi_core(scratch: &Scratch) -> Core {
    let mut core = core_with_switch_clis(scratch);
    core.cli_home = Some(CliHome::at(scratch.path.join("home")));
    write_exec(&scratch.path, "pi", &pi_script(scratch));
    core
}

#[test]
fn pi_chat_confirms_its_preallocated_session_and_resumes_it_with_current_memory() {
    let scratch = Scratch::new();
    let core = pi_core(&scratch);
    let project = core
        .add_project(&scratch.repo("pi-chat-demo"))
        .unwrap()
        .project;
    core.remember(&project.id, "Keep this note").unwrap();
    let start = core
        .start_chat_lead(&project.id, "pi", lead_launch(&scratch))
        .unwrap();
    let lead = start.session.id.clone();
    assert_eq!(start.session.pty, None);
    assert_eq!(
        start.conversation,
        LeadConversation::Fresh(FreshReason::NoSavedSession)
    );
    // Nothing is saved until Pi writes the session, at the first message.
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

    send(&core, &lead, "Hello");
    let events = collect(&core, &lead, ready);
    let confirmed = events
        .iter()
        .find_map(|e| match e {
            ChatEvent::Confirmed(id) => Some(id.clone()),
            _ => None,
        })
        .expect("the first user message confirms the session");
    assert!(crate::agents::valid_conversation_id(&confirmed));
    core.confirm_lead_chat(&lead, &confirmed).unwrap();
    assert_eq!(
        core.saved_lead(&project.id)
            .unwrap()
            .unwrap()
            .session_id
            .as_deref(),
        Some(confirmed.as_str())
    );
    assert_eq!(texts(&core, &project.id), ["Hello", "Final reply"]);
    assert!(!format!("{:?}", core.lead_chat_page(&project.id, None)).contains("private"));

    let launched = log(&scratch);
    let args = launched.lines().find(|l| l.starts_with("args:")).unwrap();
    assert!(
        args.starts_with("args:--mode rpc --approve --extension "),
        "{args}"
    );
    assert!(
        args.contains(&format!("--session-id {confirmed} --append-system-prompt ")),
        "{args}"
    );
    // The note follows the guide after a newline, past this log line.
    assert!(launched.contains("Keep this note"), "{launched}");
    assert!(launched.contains(&format!(
        "sock:{}",
        scratch.path.join("control/sock").display()
    )));
    let extension = PathBuf::from(
        launched
            .lines()
            .find_map(|l| l.strip_prefix("extension:"))
            .unwrap(),
    );
    assert!(
        fs::read_to_string(&extension)
            .unwrap()
            .contains("shika_ask_author")
    );
    assert_eq!(calls(&scratch, "get_state").len(), 1);

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

    // An extension command takes the prompt: it settles with no bubble.
    send(&core, &lead, "/handled");
    let events = collect(&core, &lead, ready);
    assert!(events.iter().any(|e| matches!(e, ChatEvent::Accepted)));
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, ChatEvent::DeliveryFailed | ChatEvent::Error(_)))
    );

    // Shika's question tool: the native dialog, answered as JSON.
    send(&core, &lead, "please-ask");
    let events = collect(&core, &lead, |e| matches!(e, ChatEvent::Question(_)));
    let question = events
        .into_iter()
        .find_map(|e| match e {
            ChatEvent::Question(q) => Some(q),
            _ => None,
        })
        .unwrap();
    assert_eq!(question.questions[0].question, "Which base?");
    let mut input = question.input.clone();
    input["answers"] = serde_json::json!({"Which base?": "dev"});
    core.lead_chat_answer(&lead, question.request_id.clone(), Some(input));
    collect(&core, &lead, ready);
    let answered = calls(&scratch, "extension_ui_response");
    assert_eq!(answered.len(), 1);
    assert!(
        answered[0].contains(r#""value":"{\"Which base?\":\"dev\"}""#),
        "{}",
        answered[0]
    );

    // Stop generating with the dialog open cancels it before `abort`, and
    // keeps the process.
    send(&core, &lead, "please-ask");
    collect(&core, &lead, |e| matches!(e, ChatEvent::Question(_)));
    core.lead_chat_interrupt(&lead);
    let events = collect(&core, &lead, ready);
    assert!(!events.iter().any(|e| matches!(e, ChatEvent::Exit)));
    let lines = log(&scratch);
    let cancelled = lines.find(r#""cancelled":true"#).expect("a cancel");
    let aborted = lines.find(r#""type":"abort""#).expect("an abort");
    assert!(cancelled < aborted, "{lines}");

    send(&core, &lead, "please-wait");
    collect(&core, &lead, |e| matches!(e, ChatEvent::Accepted));
    core.lead_chat_interrupt(&lead);
    let events = collect(&core, &lead, ready);
    assert!(!events.iter().any(|e| matches!(e, ChatEvent::Exit)));
    assert_eq!(calls(&scratch, "abort").len(), 2);
    assert_eq!(calls(&scratch, "get_state").len(), 1);
    assert_eq!(
        texts(&core, &project.id),
        [
            "Hello",
            "Final reply",
            "/handled",
            "please-ask",
            "Answered",
            "please-ask",
            "Reply stopped",
            "please-wait",
            "Reply stopped",
        ]
    );

    core.stop_lead(&lead).unwrap();
    assert!(
        core.write(worker.pty.unwrap(), b"still running\n").is_ok(),
        "chat interruption and Stop must not stop worker groups"
    );
    assert!(!extension.exists(), "the extension goes with the transport");

    // The saved session resumes with the same id, and a new note reaches
    // it in the new system prompt addendum, which replaces the old one.
    core.remember(&project.id, "Prefer small workers").unwrap();
    let start = core
        .start_chat_lead(&project.id, "pi", lead_launch(&scratch))
        .unwrap();
    assert_eq!(start.conversation, LeadConversation::Resumed);
    send(&core, &start.session.id, "Hello again");
    collect(&core, &start.session.id, ready);
    let relaunch = log(&scratch);
    let relaunch = &relaunch[relaunch.rfind("args:").unwrap()..];
    assert!(
        relaunch.contains(&format!("--session-id {confirmed} ")),
        "{relaunch}"
    );
    assert!(relaunch.contains("Prefer small workers"), "{relaunch}");
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
fn a_pi_chat_lead_quits_by_closing_stdin() {
    let scratch = Scratch::new();
    let core = pi_core(&scratch);
    let project = core
        .add_project(&scratch.repo("pi-quit-demo"))
        .unwrap()
        .project;
    let start = core
        .start_chat_lead(&project.id, "pi", lead_launch(&scratch))
        .unwrap();
    send(&core, &start.session.id, "please-wait");
    collect(&core, &start.session.id, |e| {
        matches!(e, ChatEvent::Accepted)
    });
    let now = Instant::now();
    core.stop_lead_chats();
    assert!(now.elapsed() < Duration::from_secs(2), "no kill deadline");
    let page = core.lead_chat_page(&project.id, None).unwrap();
    assert_eq!(page.last().unwrap().content.text(), Some("Reply stopped"));
}

/// A Pi Lead saved by a terminal-era build resumes its session in chat with
/// empty visible history: nothing is imported and no prompt is sent. Its
/// current guide and memory go in the launch's system prompt addendum.
#[test]
fn a_saved_terminal_pi_lead_resumes_its_session_in_chat_with_empty_history() {
    let scratch = Scratch::new();
    let core = pi_core(&scratch);
    let project = core
        .add_project(&scratch.repo("pi-terminal-era-demo"))
        .unwrap()
        .project;
    core.remember(&project.id, "Keep this note").unwrap();
    const SAVED: &str = "5c0ffee0-0000-4000-8000-0000000000a1";
    let cwd = worktree::lead_worktree_path(&project.path);
    let folder = scratch.path.join("home/.pi/agent/sessions").join(format!(
        "--{}--",
        cwd.to_string_lossy()
            .trim_start_matches('/')
            .replace(['/', ':'], "-")
    ));
    fs::create_dir_all(&folder).unwrap();
    fs::write(
        folder.join(format!("2026-11-19T00-00-00-000Z_{SAVED}.jsonl")),
        "{\"type\":\"message\"}\n",
    )
    .unwrap();
    core.projects
        .set_lead(
            &project.id,
            Some(SavedLead {
                cli: "pi".into(),
                session_id: Some(SAVED.into()),
                reported_workers: Vec::new(),
                model: None,
                chat_history: None,
            }),
        )
        .unwrap();

    let start = core
        .start_chat_lead(&project.id, "pi", lead_launch(&scratch))
        .unwrap();
    let lead = start.session.id.clone();
    assert_eq!(start.conversation, LeadConversation::Resumed);
    let deadline = Instant::now() + Duration::from_secs(10);
    while calls(&scratch, "get_state").is_empty() {
        assert!(Instant::now() < deadline, "no handshake");
        std::thread::sleep(Duration::from_millis(10));
    }
    let args = log(&scratch);
    assert!(args.contains(&format!("--session-id {SAVED} ")), "{args}");
    assert!(args.contains("Keep this note"));
    std::thread::sleep(Duration::from_millis(200));
    assert!(calls(&scratch, "prompt").is_empty());
    assert!(!core.lead_chat_events(&lead).iter().any(|e| matches!(
        e,
        ChatEvent::Record(_) | ChatEvent::Started | ChatEvent::Error(_)
    )));
    assert!(core.lead_chat_page(&project.id, None).unwrap().is_empty());

    send(&core, &lead, "Hello");
    collect(&core, &lead, ready);
    assert_eq!(texts(&core, &project.id), ["Hello", "Final reply"]);
    core.stop_lead(&lead).unwrap();
}

/// Pi starts an empty session under a saved id whose file it cannot read,
/// with no error and no exit. The chat refuses that resume instead of
/// talking to a Lead that lost its context, so the app's refused-resume
/// rule applies: the file is still there, so the id is kept.
#[test]
fn a_resume_pi_brings_back_empty_is_refused_and_keeps_the_saved_id() {
    let scratch = Scratch::new();
    let core = pi_core(&scratch);
    let project = core
        .add_project(&scratch.repo("pi-refused-demo"))
        .unwrap()
        .project;
    const SAVED: &str = "5c0ffee0-0000-4000-8000-0000000000a2";
    let cwd = worktree::lead_worktree_path(&project.path);
    let folder = scratch.path.join("home/.pi/agent/sessions").join(format!(
        "--{}--",
        cwd.to_string_lossy()
            .trim_start_matches('/')
            .replace(['/', ':'], "-")
    ));
    fs::create_dir_all(&folder).unwrap();
    fs::write(
        folder.join(format!("2026-11-19T00-00-00-000Z_{SAVED}.jsonl")),
        "not a session\n",
    )
    .unwrap();
    core.projects
        .set_lead(
            &project.id,
            Some(SavedLead {
                cli: "pi".into(),
                session_id: Some(SAVED.into()),
                reported_workers: Vec::new(),
                model: None,
                chat_history: None,
            }),
        )
        .unwrap();

    let start = core
        .start_chat_lead(&project.id, "pi", lead_launch(&scratch))
        .unwrap();
    assert_eq!(start.conversation, LeadConversation::Resumed);
    let events = collect(&core, &start.session.id, |e| matches!(e, ChatEvent::Exit));
    assert!(
        events
            .iter()
            .any(|e| matches!(e, ChatEvent::Error(text) if text == "Pi could not resume this conversation")),
        "{events:?}"
    );
    assert!(calls(&scratch, "prompt").is_empty());
    assert_eq!(
        core.saved_lead_session_exists(&project.id).unwrap(),
        Some(true)
    );
    let saved = core.saved_lead(&project.id).unwrap().unwrap();
    assert_eq!(saved.session_id.as_deref(), Some(SAVED));
    core.stop_lead(&start.session.id).unwrap();
}
