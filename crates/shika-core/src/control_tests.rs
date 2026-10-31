use super::*;
use std::os::unix::net::UnixListener;

fn args(words: &str) -> Vec<String> {
    words.split_whitespace().map(str::to_string).collect()
}

fn task(id: &str, status: TaskStatus) -> TaskInfo {
    TaskInfo {
        id: id.into(),
        title: "Fix login".into(),
        cli: "codex".into(),
        status,
        elapsed_secs: Some(192),
        branch: "fix-login".into(),
        diff_stat: Some(TaskDiffStat {
            files: 2,
            added: 64,
            removed: 3,
        }),
        pr: Some(12),
        started_by_lead: true,
        path: "/repo/.worktrees/fix-login".into(),
        report: None,
    }
}

#[test]
fn requests_and_replies_round_trip_as_json() {
    let commands = [
        Command::Help,
        Command::Tasks,
        Command::New {
            cli: "claude".into(),
            base: Some("dev".into()),
            prompt: "Fix \"quotes\"\nand lines".into(),
        },
        Command::Status { task: "a".into() },
        Command::Wait {
            tasks: vec!["a".into(), "b".into()],
            timeout_secs: 5,
        },
        Command::Read {
            task: "a".into(),
            lines: 40,
        },
        Command::Diff {
            task: "a".into(),
            stat: true,
        },
        Command::Send {
            task: "a".into(),
            text: "yes\n\"ok\"".into(),
            enter: false,
        },
        Command::Key {
            task: "a".into(),
            keys: vec!["down".into(), "enter".into()],
        },
        Command::Pr {
            task: "a".into(),
            title: None,
            body: None,
        },
        Command::Pr {
            task: "a".into(),
            title: Some("Fix \"login\"".into()),
            body: Some("Why\n\n- tested".into()),
        },
        Command::Close { task: "a".into() },
        Command::Memory,
        Command::Remember {
            text: "Use \"pnpm\"".into(),
        },
        Command::Forget { note: 2 },
        Command::Report {
            text: "Fixed it.\n\"tests\" pass".into(),
        },
    ];
    for command in commands {
        let request = Request::new("tok", command);
        let json = serde_json::to_string(&request).unwrap();
        assert!(!json.contains('\n'));
        assert_eq!(serde_json::from_str::<Request>(&json).unwrap(), request);
    }
    let replies = [
        Reply::Help { text: "hi".into() },
        Reply::Tasks {
            tasks: vec![task("a", TaskStatus::Working)],
        },
        Reply::Started {
            task: task("a", TaskStatus::Starting),
        },
        Reply::Status {
            task: task("a", TaskStatus::Asking),
        },
        Reply::Waited {
            events: vec![WaitEvent {
                task: task("a", TaskStatus::Ready),
            }],
            still_working: vec![task("b", TaskStatus::Working)],
            timed_out: false,
        },
        Reply::Text {
            text: "screen".into(),
        },
        Reply::Done {
            message: "sent".into(),
        },
        Reply::Memory {
            notes: vec![MemoryNote {
                text: "Use pnpm.".into(),
                created: 1,
            }],
        },
        Reply::Refused {
            reason: "no".into(),
        },
        Reply::Error {
            message: "bad".into(),
        },
    ];
    for reply in replies {
        let json = serde_json::to_string(&reply).unwrap();
        assert_eq!(serde_json::from_str::<Reply>(&json).unwrap(), reply);
    }
    assert_eq!(
        serde_json::to_string(&Request::new("t", Command::Tasks)).unwrap(),
        r#"{"version":6,"token":"t","command":{"type":"tasks"}}"#
    );
    // `pr` title and body are optional on the wire.
    assert_eq!(
        serde_json::from_str::<Command>(r#"{"type":"pr","task":"a"}"#).unwrap(),
        Command::Pr {
            task: "a".into(),
            title: None,
            body: None,
        }
    );
    assert_eq!(
        serde_json::to_string(&Command::Pr {
            task: "a".into(),
            title: Some("T".into()),
            body: None,
        })
        .unwrap(),
        r#"{"type":"pr","task":"a","title":"T","body":null}"#
    );
}

#[test]
fn a_report_is_parsed_and_shown_under_its_task() {
    let parse = |words: &str| parse_args(&args(words));
    assert_eq!(
        parse("report Fixed it, tests pass"),
        Ok((
            Command::Report {
                text: "Fixed it, tests pass".into()
            },
            false
        ))
    );
    assert_eq!(
        parse("--json report -- -x broke"),
        Ok((
            Command::Report {
                text: "-x broke".into()
            },
            true
        ))
    );
    assert!(parse("report").unwrap_err().contains("report needs text"));
    assert!(
        parse("report --bogus x")
            .unwrap_err()
            .contains("Unknown option")
    );
    assert!(is_command("report"));

    let mut reported = task("a1", TaskStatus::Ready);
    reported.report = Some("Added the form.\nTests: cargo test, 12 passed.".into());
    let text = render_text(&Reply::Waited {
        events: vec![WaitEvent { task: reported }],
        still_working: vec![],
        timed_out: false,
    });
    assert!(
        text.ends_with("path=/repo/.worktrees/fix-login\n  report:\n    Added the form.\n    Tests: cargo test, 12 passed."),
        "{text}"
    );
    // No report, no extra lines.
    assert!(
        !render_text(&Reply::Status {
            task: task("a1", TaskStatus::Ready)
        })
        .contains("report")
    );
}

#[test]
fn memory_commands_parse_and_render() {
    let parse = |words: &str| parse_args(&args(words));
    assert_eq!(parse("memory"), Ok((Command::Memory, false)));
    assert_eq!(parse("memory --json"), Ok((Command::Memory, true)));
    assert_eq!(
        parse("remember Use pnpm, not npm"),
        Ok((
            Command::Remember {
                text: "Use pnpm, not npm".into()
            },
            false
        ))
    );
    assert_eq!(
        parse("--json remember -- --force is banned"),
        Ok((
            Command::Remember {
                text: "--force is banned".into()
            },
            true
        ))
    );
    assert_eq!(parse("forget 3"), Ok((Command::Forget { note: 3 }, false)));
    assert_eq!(
        parse("forget --json 12"),
        Ok((Command::Forget { note: 12 }, true))
    );
    assert!(
        parse("remember")
            .unwrap_err()
            .contains("remember needs text")
    );
    assert!(parse("forget 0").unwrap_err().contains("1 or more"));

    let notes = |texts: &[&str]| Reply::Memory {
        notes: texts
            .iter()
            .map(|text| MemoryNote {
                text: (*text).into(),
                created: 0,
            })
            .collect(),
    };
    assert_eq!(render_text(&notes(&[])), "No notes.");
    assert_eq!(
        render_text(&notes(&["Use pnpm.", "No mocks."])),
        "1. Use pnpm.\n2. No mocks."
    );
    let done = |message: &str| Reply::Done {
        message: message.into(),
    };
    assert_eq!(render_text(&done("remembered 3")), "remembered 3");
    assert_eq!(render_text(&done("forgot 2")), "forgot 2");
    let full = Reply::Refused {
        reason: "Memory is full (16,000 bytes). Forget a note first.".into(),
    };
    assert_eq!(
        render_text(&full),
        "refused: Memory is full (16,000 bytes). Forget a note first."
    );
    assert_eq!(full.exit_code(), 1);
    assert_eq!(notes(&[]).exit_code(), 0);
    assert!(USAGE.contains("remember <text...>") && USAGE.contains("forget <n>"));
}

#[test]
fn reports_are_cleaned_and_bounded() {
    assert_eq!(
        clean_report("  done\tnow\x1b[31m\r\nline two\u{7} \n"),
        Ok("done now[31m\nline two".to_string())
    );
    assert!(clean_report(" \n\x07 ").is_err());
    let long = "x".repeat(MAX_REPORT_BYTES);
    assert_eq!(clean_report(&long).unwrap().len(), MAX_REPORT_BYTES);
    let err = clean_report(&format!("{long}x")).unwrap_err();
    assert!(err.contains("4096"), "{err}");
}

#[test]
fn a_version_mismatch_is_refused_with_a_reason() {
    let mut request = Request::new("t", Command::Help);
    assert_eq!(request.version_refusal(), None);
    request.version = PROTOCOL_VERSION + 1;
    let Some(Reply::Refused { reason }) = request.version_refusal() else {
        panic!("expected a refusal");
    };
    assert!(reason.contains("protocol"), "{reason}");
}

#[test]
fn text_is_one_compact_line_per_task() {
    let text = render_text(&Reply::Tasks {
        tasks: vec![task("a1", TaskStatus::Ready), {
            let mut idle = task("b2", TaskStatus::Working);
            idle.diff_stat = None;
            idle.elapsed_secs = Some(5);
            idle.pr = None;
            idle.started_by_lead = false;
            idle.branch = String::new();
            idle.path = "/my repo/.worktrees/x".into();
            idle
        }],
    });
    assert_eq!(
        text,
        "a1 ready \"Fix login\" cli=codex branch=fix-login diff=2 files +64 -3 elapsed=3m12s pr=#12 by-lead path=/repo/.worktrees/fix-login\nb2 working \"Fix login\" cli=codex elapsed=5s path=\"/my repo/.worktrees/x\""
    );
    assert_eq!(render_text(&Reply::Tasks { tasks: vec![] }), "No tasks.");
    let waited = render_text(&Reply::Waited {
        events: vec![WaitEvent {
            task: task("a1", TaskStatus::Asking),
        }],
        still_working: vec![task("b2", TaskStatus::Working)],
        timed_out: false,
    });
    assert!(waited.starts_with("asking: a1 asking"), "{waited}");
    assert!(waited.contains("still working:\n  b2 working"), "{waited}");
    let timeout = render_text(&Reply::Waited {
        events: vec![],
        still_working: vec![],
        timed_out: true,
    });
    assert!(timeout.starts_with("timed out"), "{timeout}");
    assert_eq!(
        render_text(&Reply::Refused {
            reason: "full".into()
        }),
        "refused: full"
    );
    assert_eq!(Reply::Refused { reason: "x".into() }.exit_code(), 1);
    assert_eq!(
        Reply::Error {
            message: "x".into()
        }
        .exit_code(),
        2
    );
    assert_eq!(Reply::Help { text: "x".into() }.exit_code(), 0);
}

#[test]
fn arguments_parse_into_commands() {
    let ok = |words: &str| parse_args(&args(words)).unwrap();
    assert_eq!(ok("help"), (Command::Help, false));
    assert_eq!(ok("tasks --json"), (Command::Tasks, true));
    assert_eq!(ok("--json tasks"), (Command::Tasks, true));
    assert_eq!(
        ok("status abc --json"),
        (Command::Status { task: "abc".into() }, true)
    );
    assert_eq!(
        ok("new --cli codex fix the login bug"),
        (
            Command::New {
                cli: "codex".into(),
                base: None,
                prompt: "fix the login bug".into(),
            },
            false
        )
    );
    assert_eq!(
        ok("new --json --base dev --cli pi add tests"),
        (
            Command::New {
                cli: "pi".into(),
                base: Some("dev".into()),
                prompt: "add tests".into(),
            },
            true
        )
    );
    // Words after the prompt starts are the prompt, flags included.
    assert_eq!(
        ok("new --cli claude explain --json output"),
        (
            Command::New {
                cli: "claude".into(),
                base: None,
                prompt: "explain --json output".into(),
            },
            false
        )
    );
    assert_eq!(
        ok("new --cli claude -- --odd"),
        (
            Command::New {
                cli: "claude".into(),
                base: None,
                prompt: "--odd".into(),
            },
            false
        )
    );
    let pr = |title: Option<&str>, body: Option<&str>| Command::Pr {
        task: "abc".into(),
        title: title.map(str::to_string),
        body: body.map(str::to_string),
    };
    assert_eq!(ok("pr abc"), (pr(None, None), false));
    assert_eq!(
        ok("--json close abc"),
        (Command::Close { task: "abc".into() }, true)
    );
    assert_eq!(ok("pr abc --json"), (pr(None, None), true));
    // A quoted value is one argument, spaces, dashes, and lines included.
    let quoted = |words: &[&str]| {
        parse_args(&words.iter().map(|w| w.to_string()).collect::<Vec<_>>()).unwrap()
    };
    assert_eq!(
        quoted(&["pr", "--title", "Fix the login", "abc"]),
        (pr(Some("Fix the login"), None), false)
    );
    assert_eq!(
        quoted(&[
            "pr",
            "abc",
            "--body",
            "Why:\n- it broke",
            "--json",
            "--title",
            "-x"
        ]),
        (pr(Some("-x"), Some("Why:\n- it broke")), true)
    );
    assert_eq!(
        ok("wait"),
        (
            Command::Wait {
                tasks: vec![],
                timeout_secs: 100,
            },
            false
        )
    );
    assert_eq!(
        ok("read abc"),
        (
            Command::Read {
                task: "abc".into(),
                lines: 0
            },
            false
        )
    );
    assert_eq!(
        ok("read --lines 200 abc --json"),
        (
            Command::Read {
                task: "abc".into(),
                lines: 200
            },
            true
        )
    );
    assert_eq!(
        ok("diff abc --stat"),
        (
            Command::Diff {
                task: "abc".into(),
                stat: true
            },
            false
        )
    );
    assert_eq!(
        ok("send abc yes please"),
        (
            Command::Send {
                task: "abc".into(),
                text: "yes please".into(),
                enter: true
            },
            false
        )
    );
    // Flags come before the text; after it they are the text.
    assert_eq!(
        ok("send abc --no-enter --json 1 --no-enter"),
        (
            Command::Send {
                task: "abc".into(),
                text: "1 --no-enter".into(),
                enter: false
            },
            true
        )
    );
    assert_eq!(
        ok("send abc -- -1"),
        (
            Command::Send {
                task: "abc".into(),
                text: "-1".into(),
                enter: true
            },
            false
        )
    );
    assert_eq!(
        ok("key abc Down down ENTER y 7 --json"),
        (
            Command::Key {
                task: "abc".into(),
                keys: ["down", "down", "enter", "y", "7"]
                    .map(String::from)
                    .to_vec()
            },
            true
        )
    );
    assert_eq!(key_name("Escape").as_deref(), Some("escape"));
    assert_eq!(key_name("A").as_deref(), Some("a"));
    assert_eq!(key_name("ctrl-c"), None);
    assert_eq!(key_name(""), None);
    assert_eq!(
        ok("wait a b --timeout 30 --json"),
        (
            Command::Wait {
                tasks: vec!["a".into(), "b".into()],
                timeout_secs: 30,
            },
            true
        )
    );
}

#[test]
fn bad_arguments_return_a_short_usage() {
    for words in [
        "",
        "--json",
        "frobnicate",
        "--json frob 1",
        "--data-dir /x",
        "help extra",
        "tasks extra",
        "status",
        "status a b",
        "status --nope",
        "new fix it",
        "new --cli",
        "new --cli claude",
        "new --cli gemini fix",
        "new --cli claude --bogus fix",
        "wait --timeout",
        "wait --timeout soon",
        "wait --nope",
        "read",
        "read a b",
        "read a --lines",
        "read a --lines many",
        "read a --lines 2001",
        "read a --nope",
        "diff",
        "diff a --nope",
        "diff a b",
        "send",
        "send a",
        "send a --no-enter",
        "send --nope a hi",
        "send a --bogus hi",
        "key",
        "key a",
        "key a f13",
        "key a enter --nope",
        "key a ab",
        "memory extra",
        "remember",
        "remember --bogus x",
        "forget",
        "forget 0",
        "forget -1",
        "forget two",
        "forget 1 2",
        "forget --nope 1",
        "pr",
        "pr a b",
        "pr --nope",
        "pr a --title",
        "pr a --body",
        "pr --title x",
        "pr a --title x --title y",
        "pr a --body x --body y",
        "close",
        "close a --title x",
        "close a b",
        "close a --nope",
    ] {
        let err = parse_args(&args(words)).unwrap_err();
        assert!(err.contains("usage: shika"), "{words:?}: {err}");
    }
    let err = parse_args(&args("frob 1")).unwrap_err();
    assert!(err.starts_with("Unknown command frob."), "{err}");
    let too_many = format!("key a {}", ["tab"; MAX_KEYS + 1].join(" "));
    assert!(parse_args(&args(&too_many)).is_err());
    let long = "x".repeat(MAX_PR_BODY_CHARS + 1);
    for values in [
        ["--title", " "],
        ["--title", "two\nlines"],
        ["--body", "\n"],
        ["--body", "nul\0"],
        ["--body", long.as_str()],
    ] {
        let words: Vec<String> = ["pr", "a", values[0], values[1]]
            .iter()
            .map(|w| w.to_string())
            .collect();
        let err = parse_args(&words).unwrap_err();
        assert!(err.contains("usage: shika"), "{values:?}: {err}");
    }
}

#[test]
fn command_words_decide_client_mode() {
    for word in [
        "help", "tasks", "new", "status", "wait", "read", "diff", "send", "key", "pr", "close",
        "memory", "remember", "forget",
    ] {
        assert!(is_command(word));
    }
    assert!(!is_command("--json"));
    assert!(!is_command("-psn_0_12345"));
    // Outside a Lead terminal only a command word makes the client.
    assert!(!is_client_invocation(&[], false));
    assert!(!is_client_invocation(&args("--data-dir /x"), false));
    assert!(!is_client_invocation(&args("--diagnostics-file /x"), false));
    assert!(!is_client_invocation(&args("-psn_0_12345"), false));
    assert!(is_client_invocation(&args("tasks"), false));
    assert!(is_client_invocation(&args("--json tasks"), false));
    assert!(!is_client_invocation(&args("--json"), false));
    // Inside one, never the app: bare, mistyped, or an app flag.
    assert!(is_client_invocation(&[], true));
    assert!(is_client_invocation(&args("frob 1"), true));
    assert!(is_client_invocation(&args("--data-dir /x"), true));
    assert!(is_client_invocation(&args("tasks"), true));
}

#[test]
fn tokens_are_random_hex() {
    let (a, b) = (new_token(), new_token());
    assert_eq!(a.len(), 32);
    assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    assert_ne!(a, b);
}

/// A data directory reserved with `create_dir`, never adopted, and removed
/// on drop.
struct Data(PathBuf);

impl Data {
    fn new() -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        loop {
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "shika-control-test-{}-{}-{n}",
                std::process::id(),
                new_token()
            ));
            match fs::create_dir(&path) {
                Ok(()) => return Self(path),
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(err) => panic!("could not reserve a data directory: {err}"),
            }
        }
    }
}

impl Drop for Data {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn the_socket_directory_is_private_and_removed_and_the_command_is_stable() {
    let data = Data::new();
    let dir = ControlDir::create(&data.0).unwrap();
    let root = dir.socket_path().parent().unwrap().to_path_buf();
    assert!(root.is_absolute());
    assert_eq!(root, root.canonicalize().unwrap());
    assert!(
        root.file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with(&format!("shika-control-{}-", std::process::id()))
    );
    assert_eq!(
        fs::metadata(&root).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert!(dir.socket_path().as_os_str().len() <= MAX_SOCKET_PATH);
    // The command lives under the data directory, not the temp one.
    assert_eq!(dir.bin_dir(), data.0.join("bin"));
    let link = dir.bin_dir().join("shika");
    assert_eq!(
        fs::read_link(&link).unwrap(),
        std::env::current_exe().unwrap().canonicalize().unwrap()
    );
    assert!(link.exists());
    assert!(!root.join("bin").exists());
    let other = ControlDir::create(&data.0).unwrap();
    assert_ne!(other.socket_path(), dir.socket_path());
    assert_eq!(other.bin_dir(), dir.bin_dir());
    drop(dir);
    assert!(!root.exists());
    // Quitting leaves the command: a resumed Lead may still name it.
    assert!(other.bin_dir().join("shika").exists());
}

#[test]
fn installing_the_command_replaces_a_stale_link_and_leaves_no_temporary_one() {
    let data = Data::new();
    let bin = data.0.join("bin");
    fs::create_dir_all(&bin).unwrap();
    symlink("/nonexistent/old/shika", bin.join("shika")).unwrap();
    assert_eq!(install_command(&data.0).unwrap(), bin);
    let exe = std::env::current_exe().unwrap().canonicalize().unwrap();
    assert_eq!(fs::read_link(bin.join("shika")).unwrap(), exe);
    let names: Vec<_> = fs::read_dir(&bin)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["shika"], "no temporary link is left behind");
    assert_eq!(install_command(&data.0).unwrap(), bin);
    assert_eq!(fs::read_link(bin.join("shika")).unwrap(), exe);

    // Concurrent installs (parallel tests, or two apps) never fail, and
    // the link is always there for a reader. Checked with lstat: on APFS,
    // resolving a symlink while another is renamed over it can briefly
    // fail with EINVAL, so `exists()` may see false mid-swap.
    std::thread::scope(|scope| {
        for _ in 0..4 {
            scope.spawn(|| {
                for _ in 0..25 {
                    install_command(&data.0).unwrap();
                    assert!(fs::symlink_metadata(bin.join("shika")).is_ok());
                }
            });
        }
    });
    let names: Vec<_> = fs::read_dir(&bin).unwrap().collect();
    assert_eq!(names.len(), 1);
}

#[test]
fn the_command_path_follows_a_data_dir_with_spaces() {
    let data = Data::new();
    let spaced = data.0.join("Application Support").join("com.example.app");
    let bin = install_command(&spaced).unwrap();
    assert_eq!(bin, spaced.join("bin"));
    assert!(bin.join("shika").exists());
    assert_eq!(
        fs::metadata(&bin).unwrap().permissions().mode() & 0o777,
        0o700
    );
}

#[test]
fn a_socket_round_trip_carries_one_request_and_one_reply() {
    let data = Data::new();
    let dir = ControlDir::create(&data.0).unwrap();
    let listener = UnixListener::bind(dir.socket_path()).unwrap();
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let request = read_request(&stream).unwrap();
        let reply = match request.command {
            Command::Status { task } => Reply::Status {
                task: task_for(&task),
            },
            _ => Reply::Refused {
                reason: "unexpected".into(),
            },
        };
        write_reply(&stream, &reply).unwrap();
        request.token
    });
    fn task_for(id: &str) -> TaskInfo {
        TaskInfo {
            id: id.into(),
            title: "t".into(),
            cli: "claude".into(),
            status: TaskStatus::Ready,
            elapsed_secs: None,
            branch: String::new(),
            diff_stat: None,
            pr: None,
            started_by_lead: false,
            path: String::new(),
            report: None,
        }
    }
    let reply = send(
        &dir.socket_path(),
        &Request::new("secret", Command::Status { task: "abc".into() }),
    )
    .unwrap();
    assert_eq!(
        reply,
        Reply::Status {
            task: task_for("abc")
        }
    );
    assert_eq!(server.join().unwrap(), "secret");
}

#[test]
fn a_missing_socket_explains_itself_and_oversized_requests_are_rejected() {
    let err = send(
        Path::new("/nonexistent/shika/sock"),
        &Request::new("t", Command::Help),
    )
    .unwrap_err();
    assert!(matches!(err, ControlError::Connect(_)));
    assert!(err.to_string().contains("Is the app running"), "{err}");

    let mut big = vec![b'a'; MAX_REQUEST_BYTES + 10];
    big.push(b'\n');
    assert_eq!(read_request(&big[..]), Err(ControlError::TooLarge));
    assert!(matches!(
        read_request(&b"not json\n"[..]),
        Err(ControlError::Protocol(_))
    ));
    assert!(matches!(
        read_request(&b""[..]),
        Err(ControlError::Protocol(_))
    ));
    // A well-formed line without a trailing newline still parses.
    let line = serde_json::to_vec(&Request::new("t", Command::Tasks)).unwrap();
    assert_eq!(read_request(&line[..]).unwrap().command, Command::Tasks);
}
