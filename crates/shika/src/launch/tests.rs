//! Tests for the New picker choices and location switch.
use super::*;

#[test]
fn switching_location_preserves_the_selected_agent_and_project() {
    let mut picker = PickerDialog {
        project: "project-a".into(),
        index: 1,
        mode: PickerMode::Task,
    };
    assert!(picker.toggle_worktree(3));
    assert!(picker.mode == PickerMode::Checkout);
    assert_eq!(picker.index, 1);
    assert_eq!(picker.project, "project-a");
    assert!(picker.toggle_worktree(3));
    assert!(picker.mode == PickerMode::Task);
    assert_eq!(picker.index, 1);
}

#[test]
fn switching_off_a_selected_terminal_resets_to_the_first_agent() {
    for agent_count in [0, 1, 4] {
        let mut picker = PickerDialog {
            project: "project-a".into(),
            index: agent_count,
            mode: PickerMode::Task,
        };
        assert!(picker.toggle_worktree(agent_count));
        assert!(picker.mode == PickerMode::Checkout);
        assert_eq!(picker.index, 0);
        assert!(picker.toggle_worktree(agent_count));
        assert!(picker.mode == PickerMode::Task);
        assert_eq!(picker.index, 0);
    }
}

#[test]
fn lead_picker_cannot_switch_location() {
    let mut picker = PickerDialog {
        project: "project-a".into(),
        index: 1,
        mode: PickerMode::Lead,
    };
    assert!(!picker.toggle_worktree(3));
    assert!(picker.mode == PickerMode::Lead);
    assert_eq!(picker.index, 1);
}

#[test]
fn terminal_is_a_task_choice_even_without_agents_but_never_a_lead_or_checkout() {
    let terminal = shika_core::terminal_choice();
    assert_eq!(PickerMode::Task.choices(vec![]), vec![terminal.clone()]);
    assert!(PickerMode::Lead.choices(vec![]).is_empty());
    assert!(PickerMode::Checkout.choices(vec![]).is_empty());
    let agent = CliPreset {
        id: "claude".into(),
        name: "Claude Code".into(),
        binary: "claude".into(),
        path: Some("/test/claude".into()),
        args: vec![],
    };
    assert_eq!(
        PickerMode::Task.choices(vec![agent.clone()]),
        vec![agent.clone(), terminal]
    );
    assert_eq!(
        PickerMode::Lead.choices(vec![agent.clone()]),
        vec![agent.clone()]
    );
    assert_eq!(
        PickerMode::Checkout.choices(vec![agent.clone()]),
        vec![agent]
    );
}
