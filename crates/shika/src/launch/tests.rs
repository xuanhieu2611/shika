//! Tests for the New picker choices, location switch, and prompt.
use super::*;

#[test]
fn switching_location_preserves_the_selected_agent_and_project() {
    let mut picker = PickerDialog {
        project: "project-a".into(),
        index: 1,
        mode: PickerMode::Task,
        prompt: None,
        error: None,
    };
    assert!(picker.toggle_worktree());
    assert!(picker.mode == PickerMode::Checkout);
    assert_eq!(picker.index, 1);
    assert_eq!(picker.project, "project-a");
    assert!(picker.toggle_worktree());
    assert!(picker.mode == PickerMode::Task);
    assert_eq!(picker.index, 1);
}

#[test]
fn switching_location_keeps_a_selected_terminal() {
    for agent_count in [0, 1, 4] {
        let mut picker = PickerDialog {
            project: "project-a".into(),
            index: agent_count,
            mode: PickerMode::Task,
            prompt: None,
            error: None,
        };
        assert!(picker.toggle_worktree());
        assert!(picker.mode == PickerMode::Checkout);
        assert_eq!(picker.index, agent_count);
        assert!(picker.toggle_worktree());
        assert!(picker.mode == PickerMode::Task);
        assert_eq!(picker.index, agent_count);
    }
}

#[test]
fn lead_picker_cannot_switch_location() {
    let mut picker = PickerDialog {
        project: "project-a".into(),
        index: 1,
        mode: PickerMode::Lead,
        prompt: None,
        error: None,
    };
    assert!(!picker.toggle_worktree());
    assert!(picker.mode == PickerMode::Lead);
    assert_eq!(picker.index, 1);
}

#[test]
fn terminal_is_a_task_and_checkout_choice_even_without_agents_but_never_a_lead() {
    let terminal = shika_core::terminal_choice();
    assert_eq!(PickerMode::Task.choices(vec![]), vec![terminal.clone()]);
    assert_eq!(PickerMode::Checkout.choices(vec![]), vec![terminal.clone()]);
    assert!(PickerMode::Lead.choices(vec![]).is_empty());
    let agent = CliPreset {
        id: "claude".into(),
        name: "Claude Code".into(),
        binary: "claude".into(),
        path: Some("/test/claude".into()),
        args: vec![],
    };
    assert_eq!(
        PickerMode::Task.choices(vec![agent.clone()]),
        vec![agent.clone(), terminal.clone()]
    );
    assert_eq!(
        PickerMode::Lead.choices(vec![agent.clone()]),
        vec![agent.clone()]
    );
    assert_eq!(
        PickerMode::Checkout.choices(vec![agent.clone()]),
        vec![agent, terminal]
    );
}

#[test]
fn first_new_reviews_setup_only_when_something_is_suggested() {
    let draft =
        |existing: Option<shika_core::PreparationConfig>, copies: &[&str], commands: &[&str]| {
            shika_core::PreparationDraft {
                existing,
                config: shika_core::PreparationConfig {
                    commands: commands.iter().map(|s| s.to_string()).collect(),
                    copy_files: copies.iter().map(|s| s.to_string()).collect(),
                    timeout_seconds: 600,
                    clone_dependencies: false,
                },
                note: String::new(),
            }
        };
    assert!(first_new_review(draft(None, &[], &[])).is_none());
    assert!(first_new_review(draft(None, &[".env"], &[])).is_some());
    assert!(first_new_review(draft(None, &[], &["npm ci"])).is_some());
    let saved = draft(None, &[".env"], &["npm ci"]).config;
    assert!(first_new_review(draft(Some(saved), &[".env"], &["npm ci"])).is_none());
}

fn preset(id: &str) -> CliPreset {
    CliPreset {
        id: id.into(),
        name: id.into(),
        binary: id.into(),
        path: Some(format!("/test/{id}").into()),
        args: vec![],
    }
}

#[test]
fn an_empty_prompt_starts_as_before_and_terminal_ignores_the_field() {
    let claude = preset("claude");
    assert_eq!(launch_prompt("", &claude), None);
    assert_eq!(launch_prompt("  \n\t ", &claude), None);
    assert_eq!(
        launch_prompt("\n  Fix the login bug\nthen test it  \n", &claude).as_deref(),
        Some("Fix the login bug\nthen test it")
    );
    let terminal = shika_core::terminal_choice();
    assert_eq!(launch_prompt("Fix the login bug", &terminal), None);
}

#[test]
fn the_prompt_field_types_letters_and_keeps_numbers_and_space_only_while_empty() {
    for key in ["enter", "escape", "tab", "up", "down"] {
        assert!(picker_takes_from_prompt(key, true), "{key}");
        assert!(picker_takes_from_prompt(key, false), "{key}");
    }
    for key in ["space", "1", "9"] {
        assert!(picker_takes_from_prompt(key, true), "{key}");
        assert!(!picker_takes_from_prompt(key, false), "{key}");
    }
    for key in ["j", "k", "0", "a", "backspace", "left"] {
        assert!(!picker_takes_from_prompt(key, true), "{key}");
        assert!(!picker_takes_from_prompt(key, false), "{key}");
    }
}

#[test]
fn a_new_prompt_is_the_first_submission_and_names_the_task() {
    let lead_start = Mutex::new(None);
    for launch in [
        Launch::Task {
            prompt: Some("Fix the   login bug\nsecond line".into()),
        },
        Launch::Checkout {
            prompt: Some("Fix the   login bug\nsecond line".into()),
        },
    ] {
        assert_eq!(
            launch.author_prompt(),
            Some("Fix the   login bug\nsecond line")
        );
        let state = Mutex::new(HostState {
            preparing: true,
            ..HostState::default()
        });
        start::mark_host_started(&state, &launch, &lead_start);
        let host = lock(&state);
        assert!(!host.preparing);
        assert_eq!(host.submission, 1);
        assert!(host.launch_turn);
        assert!(host.last_submission.is_some());
        assert_eq!(host.title.as_deref(), Some("Fix the login bug"));
    }
    for launch in [
        Launch::Task { prompt: None },
        Launch::Checkout { prompt: None },
    ] {
        assert_eq!(launch.author_prompt(), None);
        let state = Mutex::new(HostState::default());
        start::mark_host_started(&state, &launch, &lead_start);
        let host = lock(&state);
        assert_eq!(host.submission, 0, "nothing is typed yet");
        assert!(!host.launch_turn);
        assert_eq!(host.title, None);
    }
}
