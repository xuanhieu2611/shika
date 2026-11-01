use super::*;

#[test]
fn quota_launch_is_opt_in_registers_only_after_spawn_and_keeps_prompt_last() {
    let scratch = Scratch::new();
    let mut core = core_with_fake_cli(&scratch);
    core.cli_home = Some(CliHome::at(scratch.path.join("quota-home")));
    let root = core.cli_home.as_ref().unwrap().claude_root();
    fs::create_dir_all(&root).unwrap();
    let settings =
        r#"{"statusLine":{"type":"command","command":"printf 'custom status'","padding":3}}"#;
    fs::write(root.join("settings.json"), settings).unwrap();
    let preset = core
        .cli_catalog()
        .presets
        .into_iter()
        .find(|p| p.id == "claude")
        .unwrap();
    assert!(core.subscription_limits(false).is_empty());
    core.start_subscription_limits(scratch.path.join("fixture helper"));
    let (sink, _) = channel_sink();
    assert!(
        core.open_agent(
            &preset,
            scratch.path.join("missing-program"),
            &scratch.path,
            PtySize::default(),
            None,
            false,
            vec![],
            None,
            sink,
        )
        .is_err()
    );
    assert!(core.subscription_limits(false).is_empty());
    let (sink, rx) = channel_sink();
    let (pty, _) = core
        .open_agent(
            &preset,
            preset.path.clone().unwrap(),
            &scratch.path,
            PtySize::default(),
            None,
            false,
            vec!["--dangerously-skip-permissions".into()],
            Some("quota fixture prompt"),
            sink,
        )
        .unwrap();
    let output = collect_until(&rx, "cwd:", Duration::from_secs(5));
    let args = output
        .lines()
        .find(|line| line.starts_with("args:"))
        .unwrap()
        .trim_end();
    assert!(args.contains("--settings "));
    assert!(args.contains("--shika-statusline"));
    assert!(args.contains("custom status"));
    assert!(args.ends_with("quota fixture prompt"));
    let quotas = core.subscription_limits(false);
    assert_eq!(quotas.len(), 1);
    assert_eq!(quotas[0].cli, "claude");
    assert_eq!(quotas[0].status, crate::QuotaStatus::WaitingForActivity);
    assert_eq!(
        fs::read_to_string(root.join("settings.json")).unwrap(),
        settings
    );
    core.ptys.close(pty);
    core.stop_subscription_limits();
}
