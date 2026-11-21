use super::*;

const SESSION: &str = "00000000-0000-4000-8000-0000000000c1";

/// An `agent acp` stand-in on a scratch PATH. `--version` prints the
/// version given. In ACP mode it logs its arguments and every line it reads
/// to `<scratch>/agent.log`, logs whether the Lead's rule exists (and holds
/// the memory note) when a session is created or loaded, answers
/// `session/new` with one fixed session, and keeps a session only once a
/// prompt ran, as Cursor does: loading any other id is "not found", and a
/// load replays the history first. A plain prompt sends a thought,
/// narration, a report request, a tool whose id holds a newline, and the
/// final reply; `please-ask` opens a question; `please-wait` waits for
/// `session/cancel`.
fn agent_script(scratch: &Scratch, version: &str) -> String {
    r#"#!/bin/sh
if [ "$1" = --version ]; then echo '@VERSION@'; exit 0; fi
log='@LOG@'
store='@STORE@'
S=@SESSION@
printf 'args:%s\n' "$*" >> "$log"
printf 'sock:%s\n' "$SHIKA_SOCKET" >> "$log"
rule=.cursor/rules/shika-lead.mdc
upd() {
 printf '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"%s","update":%s}}\n' "$S" "$1"
}
chunk() {
 upd '{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"'"$1"'"}}'
}
end() {
 printf '{"jsonrpc":"2.0","id":%s,"result":{"stopReason":"%s"}}\n' "$1" "$2"
}
pending=
while IFS= read -r line; do
 printf '%s\n' "$line" >> "$log"
 id=$(printf '%s\n' "$line" | sed -n 's/^{"id":\([0-9]*\),.*/\1/p')
 case "$line" in
 '{"id":90,'*)
  chunk Answered
  end "$pending" end_turn;;
 '{"id":91,'*) ;;
 *'"method":"initialize"'*)
  printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":1,"agentCapabilities":{"loadSession":true}}}\n' "$id";;
 *'"method":"session/new"'*)
  if grep -q 'Keep this note' "$rule" 2>/dev/null; then echo new-rule:yes >> "$log"; else echo new-rule:no >> "$log"; fi
  printf '{"jsonrpc":"2.0","id":%s,"result":{"sessionId":"%s","modes":{}}}\n' "$id" "$S"
  upd '{"sessionUpdate":"available_commands_update","availableCommands":[]}';;
 *'"method":"session/load"'*)
  if [ -e "$rule" ]; then echo load-rule:yes >> "$log"; else echo load-rule:no >> "$log"; fi
  sid=$(printf '%s\n' "$line" | sed -n 's/.*"sessionId":"\([^"]*\)".*/\1/p')
  if [ -f "$store/$sid" ]; then
   upd '{"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"private replayed question"}}'
   chunk 'private replayed answer'
   printf '{"jsonrpc":"2.0","id":%s,"result":{"modes":{}}}\n' "$id"
  else
   printf '{"jsonrpc":"2.0","id":%s,"error":{"code":-32602,"message":"Invalid params","data":{"message":"Session %s not found"}}}\n' "$id" "$sid"
  fi;;
 *'"method":"session/cancel"'*)
  end "$pending" cancelled;;
 *'"method":"session/prompt"'*)
  pending=$id
  mkdir -p "$store" && : > "$store/$S"
  upd '{"sessionUpdate":"agent_thought_chunk","content":{"type":"text","text":"private thought"}}'
  case "$line" in
  *please-wait*) ;;
  *please-ask*)
   printf '%s\n' '{"jsonrpc":"2.0","id":90,"method":"cursor/ask_question","params":{"toolCallId":"call-9\nfc_9_0","title":"Base","questions":[{"id":"q1","prompt":"Which base?","allowMultiple":false,"options":[{"id":"o1","label":"main"},{"id":"o2","label":"dev"}]}]}}';;
  *)
   chunk 'private narration'
   printf '%s\n' '{"jsonrpc":"2.0","id":91,"method":"cursor/update_todos","params":{"todos":[]}}'
   upd '{"sessionUpdate":"tool_call","toolCallId":"call-1\nfc_1_0","status":"pending","kind":"execute"}'
   upd '{"sessionUpdate":"tool_call_update","toolCallId":"call-1\nfc_1_0","status":"completed","rawOutput":{"stdout":"private output"}}'
   chunk 'Final '
   chunk reply
   end "$id" end_turn;;
  esac;;
 esac
done
"#
    .replace("@VERSION@", version)
    .replace("@LOG@", &scratch.path.join("agent.log").to_string_lossy())
    .replace("@STORE@", &scratch.path.join("cursor-store").to_string_lossy())
    .replace("@SESSION@", SESSION)
}

fn cursor_core(scratch: &Scratch, version: &str) -> Core {
    let mut core = core_with_switch_clis(scratch);
    core.cli_home = Some(CliHome::at(scratch.path.join("home")));
    write_exec(&scratch.path, "agent", &agent_script(scratch, version));
    core
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
    fs::read_to_string(scratch.path.join("agent.log")).unwrap_or_default()
}

/// Lines of the fake's log for one method, or that contain `needle`.
fn lines(scratch: &Scratch, needle: &str) -> Vec<String> {
    log(scratch)
        .lines()
        .filter(|line| line.contains(needle))
        .map(str::to_string)
        .collect()
}

fn calls(scratch: &Scratch, method: &str) -> Vec<String> {
    lines(scratch, &format!(r#""method":"{method}""#))
}

fn texts(core: &Core, project: &str) -> Vec<String> {
    core.lead_chat_page(project, None)
        .unwrap()
        .iter()
        .filter_map(|r| r.content.text().map(str::to_string))
        .collect()
}

fn wait_until(what: &str, done: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(Instant::now() < deadline, "{what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn cursor_chat_creates_its_session_with_the_rule_and_resumes_it_with_one_memory_line() {
    let scratch = Scratch::new();
    let core = cursor_core(&scratch, "2026.11.19-f005e9d");
    let project = core
        .add_project(&scratch.repo("cursor-chat-demo"))
        .unwrap()
        .project;
    core.remember(&project.id, "Keep this note").unwrap();
    let start = core
        .start_chat_lead(&project.id, "cursor", lead_launch(&scratch))
        .unwrap();
    let lead = start.session.id.clone();
    let tree = worktree::lead_worktree_path(&project.path);
    assert_eq!(start.session.pty, None);
    assert!(!start.bootstrap_sent, "the rule, not a bootstrap prompt");
    assert_eq!(
        start.conversation,
        LeadConversation::Fresh(FreshReason::NoSavedSession)
    );
    // No `agent create-chat`: the session reports its own id.
    wait_until("no session/new", || {
        !lines(&scratch, "new-rule:").is_empty()
    });
    let launched = log(&scratch);
    assert!(
        launched.contains("args:--force --trust --sandbox disabled acp\n"),
        "{launched}"
    );
    assert!(!launched.contains("create-chat"));
    assert!(launched.contains(&format!(
        "sock:{}",
        scratch.path.join("control/sock").display()
    )));
    assert_eq!(lines(&scratch, "new-rule:"), ["new-rule:yes"]);
    // The rule lived only while `session/new` ran, and never shows in git.
    wait_until("the rule stayed", || !tree.join(".cursor").exists());
    let exclude = fs::read_to_string(project.path.join(".git/info/exclude")).unwrap();
    assert!(
        exclude
            .lines()
            .any(|l| l == "/.cursor/rules/shika-lead.mdc")
    );
    fs::create_dir_all(tree.join(".cursor/rules")).unwrap();
    fs::write(tree.join(".cursor/rules/shika-lead.mdc"), "left behind").unwrap();
    assert!(!worktree::is_dirty(&git_path(), "", &tree).unwrap());
    fs::remove_dir_all(tree.join(".cursor")).unwrap();
    // Nothing is saved until Cursor keeps the session, at its first output.
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
        .expect("the first output confirms the session");
    assert_eq!(confirmed, SESSION);
    core.confirm_lead_chat(&lead, &confirmed).unwrap();
    assert_eq!(texts(&core, &project.id), ["Hello", "Final reply"]);
    assert!(!format!("{:?}", core.lead_chat_page(&project.id, None)).contains("private"));
    // The report request was acknowledged with an empty success. Key order
    // depends on serde_json's features in the build, so compare values.
    let acknowledged = lines(&scratch, r#"{"id":91,"#)
        .iter()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        acknowledged,
        [serde_json::json!({"id": 91, "jsonrpc": "2.0", "result": {}})]
    );

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

    // `cursor/ask_question`: the native dialog, answered with option ids.
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
    let answered = lines(&scratch, r#"{"id":90,"#);
    assert_eq!(answered.len(), 1);
    assert!(
        answered[0].contains(r#""selectedOptionIds":["o2"]"#),
        "{}",
        answered[0]
    );

    // Stop generating cancels the prompt and keeps the process.
    send(&core, &lead, "please-wait");
    collect(&core, &lead, |e| matches!(e, ChatEvent::Accepted));
    core.lead_chat_interrupt(&lead);
    let events = collect(&core, &lead, ready);
    assert!(!events.iter().any(|e| matches!(e, ChatEvent::Exit)));
    assert_eq!(calls(&scratch, "session/cancel").len(), 1);
    assert_eq!(calls(&scratch, "initialize").len(), 1);
    assert_eq!(
        texts(&core, &project.id),
        [
            "Hello",
            "Final reply",
            "please-ask",
            "Answered",
            "please-wait",
            "Reply stopped"
        ]
    );

    core.stop_lead(&lead).unwrap();
    assert!(
        core.write(worker.pty.unwrap(), b"still running\n").is_ok(),
        "chat interruption and Stop must not stop worker groups"
    );

    // The session holds the rule from its creation, and a rule at a load
    // is ignored, so a new note goes as one `[shika]` line, as a Shika
    // turn with its own row, before the author writes anything.
    core.remember(&project.id, "Prefer small workers").unwrap();
    let start = core
        .start_chat_lead(&project.id, "cursor", lead_launch(&scratch))
        .unwrap();
    assert_eq!(start.conversation, LeadConversation::Resumed);
    let resumed = start.session.id.clone();
    let events = collect(&core, &resumed, ready);
    assert!(events.iter().any(|e| matches!(e, ChatEvent::Started)));
    assert!(!events.iter().any(|e| matches!(e, ChatEvent::Accepted)));
    let loaded = calls(&scratch, "session/load");
    assert_eq!(loaded.len(), 1);
    assert!(loaded[0].contains(SESSION), "{}", loaded[0]);
    assert_eq!(lines(&scratch, "load-rule:"), ["load-rule:no"]);
    assert_eq!(calls(&scratch, "session/new").len(), 1);
    let memory = lines(&scratch, "[shika]");
    assert_eq!(memory.len(), 1);
    assert!(memory[0].contains("Keep this note"), "{}", memory[0]);
    assert!(memory[0].contains("Prefer small workers"), "{}", memory[0]);
    let shown = texts(&core, &project.id);
    assert_eq!(
        shown[shown.len() - 2..],
        [
            "Shika sent the Lead its current memory notes.",
            "Final reply"
        ]
    );
    assert!(!format!("{shown:?}").contains("private"));
    send(&core, &resumed, "Hello again");
    collect(&core, &resumed, ready);
    core.stop_lead(&resumed).unwrap();

    // Delivered once: the next resume sends nothing extra.
    let start = core
        .start_chat_lead(&project.id, "cursor", lead_launch(&scratch))
        .unwrap();
    send(&core, &start.session.id, "And again");
    collect(&core, &start.session.id, ready);
    assert_eq!(lines(&scratch, "[shika]").len(), 1);
    assert_eq!(calls(&scratch, "session/load").len(), 2);
    core.stop_lead(&start.session.id).unwrap();

    assert_eq!(
        core.saved_lead(&project.id)
            .unwrap()
            .unwrap()
            .chat_history
            .as_deref(),
        Some(history.as_str())
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

fn git_path() -> PathBuf {
    let output = Command::new("/bin/sh")
        .args(["-c", "command -v git"])
        .output()
        .unwrap();
    PathBuf::from(String::from_utf8_lossy(&output.stdout).trim())
}

/// A Cursor Lead saved by a terminal-era build holds a chat id that ACP
/// cannot load (Decision 2). The load is refused as not found, so the
/// transport ends at once and Core reports the id missing: the app's
/// refused-resume rule clears it and starts a fresh chat Lead once, with
/// its toast, and the memory carries over.
#[test]
fn a_saved_terminal_cursor_lead_is_reported_missing_and_starts_fresh_with_its_memory() {
    let scratch = Scratch::new();
    let core = cursor_core(&scratch, "2026.11.19-f005e9d");
    let project = core
        .add_project(&scratch.repo("cursor-terminal-era-demo"))
        .unwrap()
        .project;
    core.remember(&project.id, "Keep this note").unwrap();
    const SAVED: &str = "5c0ffee0-0000-4000-8000-0000000000c2";
    core.projects
        .set_lead(
            &project.id,
            Some(SavedLead {
                cli: "cursor".into(),
                session_id: Some(SAVED.into()),
                reported_workers: Vec::new(),
                chat_history: None,
            }),
        )
        .unwrap();
    let start = core
        .start_chat_lead(&project.id, "cursor", lead_launch(&scratch))
        .unwrap();
    assert_eq!(start.conversation, LeadConversation::Resumed);
    let events = collect(&core, &start.session.id, |e| matches!(e, ChatEvent::Exit));
    assert!(
        events.iter().any(|e| matches!(
            e,
            ChatEvent::Error(text) if text == "Cursor CLI could not resume this conversation"
        )),
        "{events:?}"
    );
    assert!(calls(&scratch, "session/prompt").is_empty());
    assert_eq!(
        core.saved_lead_session_exists(&project.id).unwrap(),
        Some(false)
    );

    // What `restart_lead_fresh` does in the app.
    core.stop_lead(&start.session.id).unwrap();
    core.set_lead_session(&project.id, None).unwrap();
    let start = core
        .start_chat_lead(&project.id, "cursor", lead_launch(&scratch))
        .unwrap();
    assert_eq!(
        start.conversation,
        LeadConversation::Fresh(FreshReason::NoSavedSession)
    );
    wait_until("no session/new", || {
        !lines(&scratch, "new-rule:").is_empty()
    });
    assert_eq!(lines(&scratch, "new-rule:"), ["new-rule:yes"]);
    // The stopped Lead's transport is gone: its answer no longer counts.
    assert_eq!(core.saved_lead_session_exists(&project.id).unwrap(), None);
    core.stop_lead(&start.session.id).unwrap();
}

#[test]
fn a_cursor_older_than_the_verified_version_gets_no_chat_lead() {
    let scratch = Scratch::new();
    let core = cursor_core(&scratch, "2026.10.01-0ld");
    let project = core
        .add_project(&scratch.repo("cursor-old-demo"))
        .unwrap()
        .project;
    assert_eq!(
        core.start_chat_lead(&project.id, "cursor", lead_launch(&scratch)),
        Err(Error::ChatCliTooOld {
            name: "Cursor CLI".into(),
            minimum: "2026.11.19".into(),
        })
    );
    assert!(core.lead_for_project(&project.id).is_none());
    assert!(!worktree::lead_worktree_path(&project.path).exists());
    assert!(log(&scratch).is_empty(), "nothing ran but --version");
}

#[test]
fn a_cursor_chat_lead_quits_by_closing_stdin() {
    let scratch = Scratch::new();
    let core = cursor_core(&scratch, "2026.11.19-f005e9d");
    let project = core
        .add_project(&scratch.repo("cursor-quit-demo"))
        .unwrap()
        .project;
    let start = core
        .start_chat_lead(&project.id, "cursor", lead_launch(&scratch))
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
