//! Tests for Switch agent turns and naming.
use super::*;

#[test]
fn an_empty_switch_does_not_submit_and_only_a_draft_task_is_named() {
    let now = Instant::now();
    assert_eq!(switch_prompt(""), None);
    assert_eq!(switch_prompt("  \n"), None);
    assert_eq!(
        switch_prompt("Continue the backend."),
        Some("Continue the backend.")
    );

    let mut idle = HostState::default();
    let waiting = apply_switch_turn(&mut idle, None, false, now);
    assert_eq!(idle.submission, 0);
    assert!(!idle.launch_turn);
    assert!(idle.title.is_none());
    assert_eq!(waiting.status, Status::Waiting);
    idle.capture_typed(b"rename the branch\r", now);
    assert!(idle.title.is_none());
    assert_eq!(idle.submission, 1);

    let mut prompted = HostState::default();
    let working = apply_switch_turn(&mut prompted, Some("Continue the backend."), false, now);
    assert_eq!(prompted.submission, 1);
    assert!(!prompted.launch_turn);
    assert!(prompted.title.is_none());
    assert_eq!(working.status, Status::Working);
    prompted.capture_typed(b"rename the branch\r", now);
    assert!(prompted.title.is_none());
    assert_eq!(prompted.submission, 2);

    // A task still on its draft branch is named as after New.
    let mut draft = HostState::default();
    let waiting = apply_switch_turn(&mut draft, None, true, now);
    assert_eq!((draft.submission, waiting.status), (0, Status::Waiting));
    draft.capture_typed(b"Add the export button\r", now);
    assert_eq!(draft.title.as_deref(), Some("Add the export button"));
    assert_eq!(draft.submission, 1);

    let mut draft = HostState::default();
    let working = apply_switch_turn(&mut draft, Some("Add the export button"), true, now);
    assert_eq!((draft.submission, working.status), (1, Status::Working));
    assert_eq!(draft.title.as_deref(), Some("Add the export button"));
    draft.capture_typed(b"rename again\r", now);
    assert_eq!(draft.submission, 2);
}
