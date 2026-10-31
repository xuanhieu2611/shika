use super::*;

#[test]
fn usage_sink_registers_before_spawn_and_recovers_after_close_and_restart() {
    let scratch = Scratch::new();
    let mut core = core_with_fake_cli(&scratch);
    assert!(core.usage_collection_idle()); // collection is opt-in
    let home = CliHome::at(scratch.path.join("usage-home"));
    core.cli_home = Some(home.clone());
    let held = rusqlite::Connection::open(core.data_dir().join("usage.sqlite")).unwrap();
    held.execute_batch("BEGIN EXCLUSIVE").unwrap();
    core.start_usage_collection();
    let config = home.claude_root().to_string_lossy().replace('\'', "'\\''");
    let data = core.data_dir().to_string_lossy().replace('\'', "'\\''");
    fs::write(scratch.path.join("bin/claude"), format!(
        "#!/bin/sh\ntest -d '{data}/usage-sources' || exit 42\nproject=$(printf '%s' \"$PWD\" | LC_ALL=C tr -c '[:alnum:]' '-')\nmkdir -p '{config}/projects/'\"$project\"\nprintf '%s\\n' '{{\"type\":\"assistant\",\"message\":{{\"id\":\"first\",\"usage\":{{\"input_tokens\":11,\"output_tokens\":7}}}}}}' > '{config}/projects/'\"$project\"'/session.jsonl'\nprintf 'usage_written\\n'\nwhile IFS= read -r line; do :; done\n"
    )).unwrap();
    let project = core
        .add_project(&scratch.repo("usage-repo"))
        .unwrap()
        .project;
    let (sink, rx) = channel_sink();
    let session = core
        .create_session(
            &project.id,
            "claude",
            PtySize::default(),
            sink,
            LaunchOptions::default(),
        )
        .unwrap();
    assert!(collect_until(&rx, "usage_written", Duration::from_secs(5)).contains("usage_written"));
    assert_eq!(
        fs::read_dir(core.data_dir().join("usage-sources"))
            .unwrap()
            .count(),
        1
    );
    let source = home
        .claude_project_dir(&session.worktree)
        .join("session.jsonl");
    // The held database makes collection impossible until after Close.
    // It must not block Close, lose the source, or depend on a live card.
    core.session_discard(&session.id).unwrap();
    assert!(!session.worktree.exists());
    drop(held);
    let deadline = std::time::Instant::now() + Duration::from_secs(6);
    while core.token_usage().claude != 18 && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(core.token_usage().claude, 18);
    let data = core.data_dir().to_path_buf();
    drop(core);
    use std::io::Write;
    fs::OpenOptions::new().append(true).open(source).unwrap().write_all(
        b"{\"type\":\"assistant\",\"message\":{\"id\":\"late\",\"usage\":{\"input_tokens\":2,\"output_tokens\":3}}}\n"
    ).unwrap();
    let mut reopened = Core::open(data).unwrap();
    reopened.cli_home = Some(home);
    reopened.start_usage_collection();
    let deadline = std::time::Instant::now() + Duration::from_secs(6);
    while reopened.token_usage().claude != 23 && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(reopened.token_usage().claude, 23);
    assert!(reopened.sessions().is_empty());
}
