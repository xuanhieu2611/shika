//! Tests for the New picker choices.
use super::*;

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
