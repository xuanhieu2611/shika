use std::time::{Duration, Instant};

use super::turn::Transition;
use super::*;
use crate::model::Status;

fn screen(preset: &str, rows: &[&str]) -> Signal {
    detect(
        preset,
        &rows.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        None,
    )
}
fn at(start: Instant, ms: u64) -> Instant {
    start + Duration::from_millis(ms)
}

#[test]
fn chat_status_uses_explicit_results_and_retains_the_turn_clock() {
    let now = Instant::now();
    let mut activity = Activity::new(now);
    assert_eq!(activity.status, Status::Waiting);
    activity.advance_chat(Status::Working, true, now);
    let start = activity.turn_started;
    assert!(
        activity
            .advance_chat(Status::Asking, false, at(now, 50))
            .notify
    );
    activity.advance_chat(Status::Working, false, at(now, 100));
    assert_eq!(activity.turn_started, start);
    let done = activity.advance_chat(Status::Ready, false, at(now, 101));
    assert!(done.ready);
    assert!(!done.notify);
    assert_eq!(activity.status, Status::Ready);
    assert!(
        !activity
            .advance_chat(Status::Ready, false, at(now, 102))
            .notify
    );
    activity.advance_chat(Status::Working, true, at(now, 200));
    assert!(
        activity
            .advance_chat(Status::Ready, false, at(now, 201))
            .notify
    );
}
#[test]
fn a_chat_turn_the_cli_starts_itself_needs_the_submission_mark() {
    // `ChatEvent::Started` maps to a submission. A bare Working after a
    // settled turn continues that turn: old clock and no notification.
    let now = Instant::now();
    let mut bare = Activity::new(now);
    bare.advance_chat(Status::Working, true, now);
    assert!(bare.advance_chat(Status::Ready, false, at(now, 10)).notify);
    bare.advance_chat(Status::Working, false, at(now, 500));
    assert_eq!(bare.turn_started, Some(now));
    assert!(!bare.advance_chat(Status::Ready, false, at(now, 900)).notify);

    let mut started = Activity::new(now);
    started.advance_chat(Status::Working, true, now);
    started.advance_chat(Status::Ready, false, at(now, 10));
    started.advance_chat(Status::Working, true, at(now, 500));
    assert_eq!(started.turn_started, Some(at(now, 500)));
    let done = started.advance_chat(Status::Ready, false, at(now, 900));
    assert!(done.ready && done.notify);
}
#[test]
fn cursor_stop_hint_below_framed_editor_is_working_not_idle() {
    assert_eq!(
        screen(
            "cursor",
            &[
                "Finished earlier output",
                "────",
                "> draft",
                "────",
                "ctrl+c to stop"
            ]
        ),
        Signal::Working
    );
    assert_eq!(
        screen(
            "cursor",
            &[
                "Finished output",
                "────",
                "> ctrl+c to stop",
                "────",
                "/ for commands"
            ]
        ),
        Signal::Idle
    );
}

fn cursor_cli(status: Option<&str>, prompt: &str) -> Vec<String> {
    let mut rows = vec![
        "> quoted plan stays in the transcript".to_string(),
        "The patch is ready to review.".to_string(),
    ];
    if let Some(status) = status {
        rows.push(status.to_string());
    }
    rows.extend([
        "▄▄▄▄▄▄▄▄".to_string(),
        format!("→ {prompt}"),
        "▀▀▀▀▀▀▀▀".to_string(),
        "composer · ~/repo · main".to_string(),
    ]);
    rows
}

#[test]
fn cursor_status_row_stays_working_when_a_quote_is_on_screen() {
    let thinking = "\u{2800}\u{28d4} Thinking".to_string();
    assert_eq!(
        detect("cursor", &cursor_cli(Some(&thinking), ""), None),
        Signal::Working
    );
    for line in [
        "\u{2820}\u{2818} Summarizing",
        "\u{2800}\u{28d4} Reading src/main.rs",
        "\u{2800}\u{28d4} Working  12.4k",
        "\u{28ff} Grepping",
    ] {
        assert_eq!(
            detect("cursor", &cursor_cli(Some(line), "follow up"), None),
            Signal::Working,
            "{line}"
        );
    }
    // A finished response between an old spinner and the prompt is not live.
    let stale = "\u{2800}\u{28d4} Thinking".to_string();
    assert_eq!(
        screen(
            "cursor",
            &[stale.as_str(), "Done.", "▄▄▄▄▄▄▄▄", "→", "▀▀▀▀▀▀▀▀"]
        ),
        Signal::Idle
    );
    assert_eq!(detect("cursor", &cursor_cli(None, ""), None), Signal::Idle);
    assert_eq!(
        detect("cursor", &cursor_cli(None, "Yes"), None),
        Signal::Idle
    );
    assert_eq!(
        screen(
            "cursor",
            &["Thinking about the next step", "▄▄▄▄", "→", "▀▀▀▀"]
        ),
        Signal::Idle
    );
}

#[test]
fn cursor_processing_placeholder_is_working_and_a_draft_of_it_is_not() {
    assert_eq!(
        screen("cursor", &["▄▄▄▄▄▄", "→ ctrl+c to stop", "▀▀▀▀▀▀"]),
        Signal::Working
    );
    assert_eq!(
        screen(
            "cursor",
            &["▄▄▄▄▄▄", "→                ctrl+c to stop", "▀▀▀▀▀▀"]
        ),
        Signal::Working
    );
}

#[test]
fn cursor_visible_status_does_not_notify_until_it_clears() {
    let t = Instant::now();
    let thinking = "\u{2800}\u{28d4} Thinking".to_string();
    let working = cursor_cli(Some(&thinking), "");
    assert_eq!(detect("cursor", &working, None), Signal::Working);
    let mut activity = Activity::new(t);
    let started = activity.advance(Signal::Working, Some(t), Some(t), t, false);
    assert!(started.changed);
    assert!(!started.notify);
    assert!(
        !activity
            .advance(
                detect("cursor", &working, None),
                Some(t),
                Some(t),
                at(t, 30_000),
                false
            )
            .notify
    );
    let idle = cursor_cli(None, "");
    assert_eq!(detect("cursor", &idle, None), Signal::Idle);
    assert!(
        !activity
            .advance(Signal::Idle, None, None, at(t, 30_100), false)
            .ready
    );
    let done = activity.advance(Signal::Idle, None, None, at(t, 30_700), false);
    assert!(done.notify);
    assert!(done.ready);
}

#[test]
fn known_presets_only_and_empty_screens() {
    for preset in ["claude", "codex", "cursor", "pi", "kiro", "agent"] {
        assert_eq!(screen(preset, &[]), Signal::Unknown);
    }
    assert_eq!(screen("agent", &["ctrl+c to stop"]), Signal::Unknown);
}

#[test]
fn live_working_chrome_for_all_presets() {
    for (preset, rows) in [
        (
            "claude",
            vec![
                "✻ Thinking… (12s · esc to interrupt)",
                "────",
                "❯ draft",
                "────",
                "? for shortcuts",
            ],
        ),
        (
            "codex",
            vec![
                "• Working (1m 2s • esc to interrupt)",
                "› draft",
                "100% context left",
            ],
        ),
        (
            "cursor",
            vec!["⬢ Thinking", "╭────╮", "│ > draft", "╰────╯"],
        ),
        (
            "pi",
            vec!["── ⠋ Working ─────", "draft", "────────", "model / auto 1%"],
        ),
    ] {
        assert_eq!(screen(preset, &rows), Signal::Working, "{preset}");
    }
    assert_eq!(screen("cursor", &["ctrl+c to stop"]), Signal::Working);
    assert_eq!(screen("pi", &["⠙ Working"]), Signal::Working);
    assert_eq!(screen("claude", &["✻ Thinking…"]), Signal::Working);
}

#[test]
fn live_idle_editors_and_ordinary_questions() {
    for (preset, rows) in [
        (
            "claude",
            vec![
                "Which approach do you prefer?",
                "────",
                "❯",
                "────",
                "? for shortcuts",
            ],
        ),
        (
            "codex",
            vec!["Which approach do you prefer?", "›", "100% context left"],
        ),
        ("cursor", vec!["╭────╮", "│ >", "╰────╯"]),
        ("pi", vec!["────────", "", "────────", "model / auto 1%"]),
    ] {
        assert_eq!(screen(preset, &rows), Signal::Idle, "{preset}");
    }
    assert_eq!(
        screen("claude", &["Do you want to proceed?", "Yes or no?"]),
        Signal::Unknown
    );
}

#[test]
fn live_permission_controls_for_all_presets() {
    for (preset, rows) in [
        (
            "claude",
            vec![
                "────",
                "Do you want to proceed?",
                "❯ 1. Yes",
                "2. No",
                "Enter to confirm · Esc to cancel",
            ],
        ),
        (
            "codex",
            vec![
                "Allow command?",
                "› 1. Yes",
                "Press enter to confirm or esc to cancel",
            ],
        ),
        (
            "cursor",
            vec![
                "Waiting for approval",
                "Run this command?",
                "→ Run (once) (y)",
                "Skip (esc or n)",
            ],
        ),
        (
            "cursor",
            vec![
                "Write to this file?",
                "→ Proceed (y)",
                "Reject & propose changes (esc or n or p)",
            ],
        ),
        (
            "pi",
            vec![
                "────",
                "Project trust",
                "→ Trust project",
                "↑↓ navigate  enter save  esc cancel",
                "────",
            ],
        ),
    ] {
        assert_eq!(screen(preset, &rows), Signal::Blocked, "{preset}");
    }
}

#[test]
fn stale_chrome_and_drafts_do_not_match() {
    assert_eq!(
        screen(
            "claude",
            &["✻ Thinking…", "The result is done.", "────", "❯", "────"]
        ),
        Signal::Idle
    );
    assert_eq!(
        screen(
            "claude",
            &[
                "────",
                "❯ ✻ Thinking…",
                "Do you want to proceed?",
                "Enter to confirm · Esc to cancel",
                "────"
            ]
        ),
        Signal::Idle
    );
    assert_eq!(
        screen(
            "codex",
            &[
                "• Working (8s • esc to interrupt)",
                "Done.",
                "› Explain Working (8s • esc to interrupt)",
                "100% context left"
            ]
        ),
        Signal::Idle
    );
    assert_eq!(
        screen(
            "claude",
            &[
                "Do you want to proceed?",
                "❯ 1. Yes",
                "Enter to confirm · Esc to cancel",
                "Done.",
                "────",
                "❯",
                "────"
            ]
        ),
        Signal::Idle
    );
    assert_eq!(
        screen(
            "pi",
            &["────────", "⠋ Working", "────────", "model / auto 1%"]
        ),
        Signal::Idle
    );
}

#[test]
fn viewers_and_quoted_examples_are_unknown() {
    for (preset, rows) in [
        (
            "claude",
            vec![
                "✻ Thinking…",
                "Showing detailed transcript · ctrl+o to toggle",
            ],
        ),
        (
            "codex",
            vec![
                "Working (5s • esc to interrupt)",
                "↑/↓ to scroll · q to quit",
            ],
        ),
        ("pi", vec!["```", "⠋ Working", "```"]),
        ("cursor", vec!["`ctrl+c to stop`"]),
        ("claude", vec!["> ✻ Thinking…"]),
        (
            "codex",
            vec!["The footer says: Press enter to confirm or esc to cancel"],
        ),
    ] {
        assert_eq!(screen(preset, &rows), Signal::Unknown, "{preset}");
    }
    let rows = vec!["Showing detailed transcript".to_string()];
    assert_eq!(detect("claude", &rows, Some("⠋ task")), Signal::Unknown);
}

#[test]
fn titles_require_explicit_state_not_task_text() {
    for preset in ["claude", "codex"] {
        assert_eq!(detect(preset, &[], Some("⠋ task")), Signal::Working);
        for title in ["Working on tests", "Explain Action Required", "task"] {
            assert_eq!(detect(preset, &[], Some(title)), Signal::Unknown);
        }
    }
    assert_eq!(detect("claude", &[], Some("✳ task")), Signal::Idle);
    assert_eq!(detect("codex", &[], Some("✳ task")), Signal::Unknown);
    assert_eq!(detect("codex", &[], Some("Codex ⠋ task")), Signal::Working);
    assert_eq!(
        detect("codex", &[], Some("Action Required")),
        Signal::Blocked
    );
    assert_eq!(detect("pi", &[], Some("⠋ task")), Signal::Unknown);
}

#[test]
fn timer_grammar_and_failed_reconnect() {
    for line in [
        "Working (1s)",
        "Working (2m 3s • esc to interrupt)",
        "Working (1h 2m 3s)",
    ] {
        assert_eq!(screen("codex", &[line]), Signal::Working);
    }
    for line in [
        "Working (things)",
        "Working (2m)",
        "Working (2s • example)",
        "Reconnect failed (12s)",
        "`Working (2s)`",
    ] {
        assert_eq!(screen("codex", &[line]), Signal::Unknown);
    }
}

#[test]
fn initial_waiting_never_starts_from_output_or_idle() {
    let t = Instant::now();
    let mut a = Activity::new(t);
    for signal in [Signal::Unknown, Signal::Idle, Signal::Unknown] {
        assert_eq!(
            a.advance(signal, None, Some(t), at(t, 10000), false),
            Transition::default()
        );
        assert_eq!(a.status, Status::Waiting);
        assert_eq!(a.since, t);
        assert_eq!(a.turn_started, None);
    }
}

#[test]
fn submission_has_grace_and_does_not_replay() {
    let t = Instant::now();
    let mut a = Activity::new(t);
    assert!(
        a.advance(Signal::Idle, Some(t), Some(at(t, 0)), t, false)
            .changed
    );
    // No output supplied: idle editor echoes cannot prematurely finish.
    let mut a = Activity::new(t);
    a.advance(Signal::Idle, Some(t), None, t, false);
    assert!(
        !a.advance(Signal::Idle, Some(t), None, at(t, 500), false)
            .ready
    );
    assert!(
        !a.advance(Signal::Unknown, Some(t), None, at(t, 7999), false)
            .ready
    );
    assert_eq!(
        a.advance(Signal::Unknown, Some(t), None, at(t, 8000), false),
        Transition {
            changed: true,
            notify: true,
            ready: true
        }
    );
    assert_eq!(
        a.advance(
            Signal::Unknown,
            Some(t),
            Some(at(t, 9000)),
            at(t, 10000),
            false
        ),
        Transition::default()
    );
    assert_eq!(a.status, Status::Ready);
}

#[test]
fn a_launch_prompt_starts_a_turn_that_boot_output_cannot_finish() {
    let t = Instant::now();
    let mut a = Activity::new(t);
    a.hold_for_launch();
    let start = a.advance(Signal::Unknown, Some(t), None, t, false);
    assert!(start.changed && !start.ready);
    assert_eq!((a.status, a.turn_started), (Status::Working, Some(t)));
    // The banner draws, then the CLI sits at an idle editor before it
    // takes the prompt. A typed submission would finish here.
    for ms in [300, 600, 1100, 2000, 5000] {
        let step = a.advance(Signal::Idle, Some(t), Some(at(t, 200)), at(t, ms), false);
        assert!(!step.ready && !step.notify, "{ms}");
    }
    for ms in [6000, 7500] {
        let step = a.advance(Signal::Unknown, Some(t), Some(at(t, 200)), at(t, ms), false);
        assert!(!step.ready, "{ms}");
    }
    // Real work confirms the turn; from then on it ends like any other.
    a.advance(
        Signal::Working,
        Some(t),
        Some(at(t, 8000)),
        at(t, 8000),
        false,
    );
    assert_eq!(a.status, Status::Working);
    a.advance(Signal::Idle, Some(t), Some(at(t, 9000)), at(t, 9000), false);
    let done = a.advance(Signal::Idle, Some(t), Some(at(t, 9000)), at(t, 9600), false);
    assert_eq!(
        done,
        Transition {
            changed: true,
            notify: true,
            ready: true
        }
    );
    assert_eq!(a.turn_started, Some(t));
}

#[test]
fn a_launch_prompt_turn_still_ends_by_grace_if_work_is_never_recognized() {
    let t = Instant::now();
    let mut a = Activity::new(t);
    a.hold_for_launch();
    a.advance(Signal::Unknown, Some(t), None, t, false);
    a.advance(
        Signal::Unknown,
        Some(t),
        Some(at(t, 100)),
        at(t, 100),
        false,
    );
    assert!(
        !a.advance(Signal::Unknown, Some(t), None, at(t, 7999), false)
            .ready
    );
    assert!(
        a.advance(Signal::Unknown, Some(t), None, at(t, 8100), false)
            .ready
    );
    // A launch turn also ends at an idle editor once the grace passed.
    let mut a = Activity::new(t);
    a.hold_for_launch();
    a.advance(Signal::Idle, Some(t), Some(at(t, 50)), t, false);
    assert!(
        !a.advance(Signal::Idle, Some(t), None, at(t, 7900), false)
            .ready
    );
    assert!(
        a.advance(Signal::Idle, Some(t), None, at(t, 8000), false)
            .ready
    );
}

#[test]
fn a_launch_prompt_does_not_change_later_typed_turns() {
    let t = Instant::now();
    let mut a = Activity::new(t);
    a.hold_for_launch();
    a.advance(Signal::Working, Some(t), None, t, false);
    a.advance(Signal::Idle, None, None, at(t, 1000), false);
    assert!(
        a.advance(Signal::Idle, None, None, at(t, 1500), false)
            .ready
    );
    // A typed submission afterwards is an ordinary candidate turn.
    let typed = at(t, 3000);
    a.advance(Signal::Idle, Some(typed), Some(at(t, 3100)), typed, false);
    a.advance(
        Signal::Idle,
        Some(typed),
        Some(at(t, 3100)),
        at(t, 3200),
        false,
    );
    assert!(
        a.advance(
            Signal::Idle,
            Some(typed),
            Some(at(t, 3100)),
            at(t, 3700),
            false
        )
        .ready
    );
}

#[test]
fn working_and_asking_keep_the_turn_epoch() {
    let t = Instant::now();
    let mut a = Activity::new(t);
    a.advance(Signal::Working, None, None, t, false);
    for ms in [100, 1000, 9000] {
        a.advance(
            Signal::Working,
            Some(at(t, ms)),
            Some(at(t, ms)),
            at(t, ms),
            false,
        );
        assert_eq!(a.since, t);
        assert_eq!(a.turn_started, Some(t));
    }
    let blocked = a.advance(
        Signal::Blocked,
        Some(at(t, 10000)),
        None,
        at(t, 10000),
        false,
    );
    assert!(blocked.changed && blocked.notify && !blocked.ready);
    assert_eq!(a.status, Status::Asking);
    assert_eq!(a.since, t);
    assert!(
        !a.advance(Signal::Blocked, None, None, at(t, 100000), false)
            .notify
    );
    assert_eq!(
        a.advance(Signal::Unknown, None, None, at(t, 200000), false),
        Transition::default()
    );
    a.advance(
        Signal::Working,
        Some(at(t, 200100)),
        None,
        at(t, 200100),
        false,
    );
    assert_eq!(a.since, t);
    assert_eq!(a.turn_started, Some(t));
    a.advance(Signal::Idle, None, None, at(t, 200200), false);
    assert_eq!(
        a.advance(Signal::Idle, None, None, at(t, 200700), false),
        Transition {
            changed: true,
            notify: false,
            ready: true
        }
    );
    assert_eq!(a.turn_started, Some(t));
}

#[test]
fn idle_requires_two_observations_and_half_a_second() {
    let t = Instant::now();
    let mut a = Activity::new(t);
    a.advance(Signal::Working, None, None, t, false);
    assert!(
        !a.advance(Signal::Idle, None, None, at(t, 10000), false)
            .ready
    );
    assert!(
        !a.advance(Signal::Idle, None, None, at(t, 10499), false)
            .ready
    );
    assert!(
        a.advance(Signal::Idle, None, None, at(t, 10500), false)
            .ready
    );
    assert_eq!(
        a.advance(Signal::Idle, None, None, at(t, 20000), false),
        Transition::default()
    );
}

#[test]
fn intermittent_idle_is_not_completion() {
    let t = Instant::now();
    for interrupt in [Signal::Unknown, Signal::Working, Signal::Blocked] {
        let mut a = Activity::new(t);
        a.advance(Signal::Working, None, None, t, false);
        a.advance(Signal::Idle, None, None, at(t, 100), false);
        a.advance(interrupt, None, None, at(t, 400), false);
        assert!(!a.advance(Signal::Idle, None, None, at(t, 800), false).ready);
        assert!(
            a.advance(Signal::Idle, None, None, at(t, 1300), false)
                .ready
        );
    }
}

#[test]
fn unknown_quiet_requires_meaningful_output_and_two_seconds() {
    let t = Instant::now();
    let mut a = Activity::new(t);
    a.advance(Signal::Working, None, None, t, false);
    assert!(
        !a.advance(Signal::Unknown, None, None, at(t, 100000), false)
            .ready
    );
    a.advance(
        Signal::Unknown,
        None,
        Some(at(t, 100000)),
        at(t, 100000),
        false,
    );
    assert!(
        !a.advance(
            Signal::Unknown,
            None,
            Some(at(t, 100000)),
            at(t, 101999),
            false
        )
        .ready
    );
    assert!(
        a.advance(
            Signal::Unknown,
            None,
            Some(at(t, 100000)),
            at(t, 102000),
            false
        )
        .ready
    );
}

#[test]
fn known_working_never_times_out_and_last_observation_anchors_quiet() {
    let t = Instant::now();
    let mut a = Activity::new(t);
    a.advance(Signal::Working, Some(t), Some(t), t, false);
    for ms in [8000, 100000, 1000000] {
        assert!(
            !a.advance(Signal::Working, Some(t), Some(t), at(t, ms), false)
                .ready
        );
    }
    assert!(
        !a.advance(Signal::Unknown, None, Some(t), at(t, 1001999), false)
            .ready
    );
    assert!(
        a.advance(Signal::Unknown, None, Some(t), at(t, 1002000), false)
            .ready
    );
}

#[test]
fn notification_budget_resets_only_for_a_new_turn() {
    let t = Instant::now();
    let mut a = Activity::new(t);
    assert!(a.advance(Signal::Blocked, None, None, t, false).notify);
    a.advance(Signal::Idle, Some(at(t, 1)), None, at(t, 100), false);
    let done = a.advance(Signal::Idle, None, None, at(t, 600), false);
    assert!(done.ready && !done.notify);
    assert!(
        !a.advance(Signal::Unknown, Some(at(t, 1)), None, at(t, 700), false)
            .changed
    );
    assert!(
        a.advance(Signal::Working, Some(at(t, 800)), None, at(t, 800), false)
            .changed
    );
    assert_eq!(a.since, at(t, 800));
    a.advance(Signal::Idle, None, None, at(t, 900), false);
    assert!(
        a.advance(Signal::Idle, None, None, at(t, 1400), false)
            .notify
    );
}

#[test]
fn output_during_idle_debounce_restarts_confirmation() {
    let t = Instant::now();
    let mut a = Activity::new(t);
    a.advance(Signal::Working, None, None, t, false);
    a.advance(Signal::Idle, None, None, at(t, 100), false);
    assert!(
        !a.advance(Signal::Idle, None, Some(at(t, 600)), at(t, 600), false)
            .ready
    );
    assert!(
        a.advance(Signal::Idle, None, Some(at(t, 600)), at(t, 1100), false)
            .ready
    );
}

#[test]
fn exit_finishes_once_even_from_waiting_or_after_blocked() {
    let t = Instant::now();
    for initial in [Signal::Unknown, Signal::Working, Signal::Blocked] {
        let mut a = Activity::new(t);
        a.advance(initial, None, None, t, false);
        let done = a.advance(Signal::Working, None, None, at(t, 10), true);
        assert!(done.ready && done.changed);
        assert_eq!(done.notify, initial != Signal::Blocked);
        assert_eq!(
            a.advance(Signal::Working, Some(at(t, 20)), None, at(t, 20), true),
            Transition::default()
        );
        assert_eq!(a.status, Status::Ready);
    }
}

#[test]
fn false_ready_then_unsubmitted_resume_keeps_timer_and_notification_budget() {
    let t = Instant::now();
    let mut a = Activity::new(t);
    a.advance(Signal::Working, Some(t), Some(t), t, false);
    a.advance(Signal::Idle, None, None, at(t, 100), false);
    assert!(
        a.advance(Signal::Idle, None, None, at(t, 600), false)
            .notify
    );
    assert_eq!(a.turn_started, Some(t));
    assert!(
        a.advance(Signal::Working, Some(t), None, at(t, 900), false)
            .changed
    );
    assert_eq!(a.since, t);
    assert_eq!(a.turn_started, Some(t));
    assert!(
        !a.advance(Signal::Blocked, None, None, at(t, 1000), false)
            .notify
    );
    a.advance(Signal::Idle, None, None, at(t, 1100), false);
    let done = a.advance(Signal::Idle, None, None, at(t, 1600), false);
    assert!(done.ready && !done.notify);
    a.advance(Signal::Working, Some(at(t, 1700)), None, at(t, 1700), false);
    assert_eq!(a.since, at(t, 1700));
    assert_eq!(a.turn_started, Some(at(t, 1700)));
    assert!(
        a.advance(Signal::Blocked, None, None, at(t, 1800), false)
            .notify
    );
}

#[test]
fn submitted_unknown_turn_can_finish_from_output_without_strong_chrome() {
    let t = Instant::now();
    let mut a = Activity::new(t);
    a.advance(Signal::Unknown, Some(t), None, t, false);
    a.advance(Signal::Unknown, None, Some(at(t, 1000)), at(t, 1000), false);
    assert!(
        !a.advance(Signal::Unknown, None, None, at(t, 2999), false)
            .ready
    );
    assert!(
        a.advance(Signal::Unknown, None, None, at(t, 3000), false)
            .ready
    );
    assert_eq!(a.since, at(t, 3000));
}

#[test]
fn pi_draft_spinners_and_codex_prose_timers_are_not_work() {
    assert_eq!(
        screen("pi", &["────────", "⠋ Working", "────────"]),
        Signal::Unknown
    );
    assert_eq!(
        screen("codex", &["This example took (2s)"]),
        Signal::Unknown
    );
    assert_eq!(
        screen(
            "claude",
            &[
                "Do you want to proceed?",
                "❯ Yes",
                "No",
                "Enter to confirm · Esc to cancel"
            ]
        ),
        Signal::Blocked
    );
}

#[test]
fn arbitrary_unicode_and_truncated_chrome_do_not_panic() {
    for preset in ["claude", "codex", "cursor", "pi"] {
        for row in [
            "",
            "é",
            "⠋",
            "──",
            "Working (é)",
            "Working (🦌s)",
            "›⠁draft",
            "\0",
            "✻ Thinking… (",
            "── ⠋ Working ─",
        ] {
            let _ = screen(preset, &[row]);
            let _ = detect(preset, &[], Some(row));
        }
    }
}

#[test]
fn authoritative_idle_confirms_fast_turns_but_never_starts_one() {
    let t = Instant::now();
    let mut a = Activity::new(t);
    assert_eq!(
        a.advance_authoritative(Signal::Idle, None, None, t, false),
        Transition::default()
    );
    assert_eq!(a.status, Status::Waiting);
    a.advance(Signal::Unknown, Some(t), None, t, false);
    assert!(
        !a.advance_authoritative(Signal::Idle, None, None, at(t, 100), false)
            .ready
    );
    assert!(
        !a.advance_authoritative(Signal::Idle, None, None, at(t, 599), false)
            .ready
    );
    assert!(
        a.advance_authoritative(Signal::Idle, None, None, at(t, 600), false)
            .ready
    );
    assert_eq!(a.turn_started, Some(t));
    assert_eq!(
        a.advance_authoritative(Signal::Unknown, None, Some(at(t, 700)), at(t, 700), false),
        Transition::default()
    );
    a.advance(Signal::Working, Some(at(t, 800)), None, at(t, 800), false);
    a.advance(Signal::Blocked, None, None, at(t, 900), false);
    assert_eq!(
        a.advance_authoritative(Signal::Unknown, None, None, at(t, 100000), false),
        Transition::default()
    );
    assert_eq!(a.status, Status::Asking);
}

#[test]
fn codex_completion_prose_cannot_pin_working() {
    for row in [
        "Building completed (2s)",
        "Reviewing finished (2s)",
        "Building completed (2s • esc to interrupt)",
        "Working on this example (2s)",
    ] {
        assert_eq!(
            screen("codex", &[row, "›", "100% context left"]),
            Signal::Idle,
            "{row}"
        );
    }
    for row in [
        "Building (2s)",
        "Thinking (1m 2s)",
        "• Reviewing (2s • esc to interrupt)",
        "Reviewing implementation (2s • esc to interrupt)",
    ] {
        assert_eq!(
            screen("codex", &[row, "›", "100% context left"]),
            Signal::Working,
            "{row}"
        );
    }
}

fn evidence_rows(preset: &str, transcript: &[&str], draft: &str, busy: bool) -> Vec<String> {
    let mut rows: Vec<String> = transcript.iter().map(|s| (*s).to_owned()).collect();
    match preset {
        "claude" => {
            if busy {
                rows.push("✻ Thinking… (1s · esc to interrupt)".into());
            }
            rows.extend([
                "────".into(),
                format!("❯ {draft}"),
                "────".into(),
                "? for shortcuts".into(),
            ]);
        }
        "codex" => {
            if busy {
                rows.push("• Working (1s • esc to interrupt)".into());
            }
            rows.extend([format!("› {draft}"), "100% context left".into()]);
        }
        "cursor" => {
            if busy {
                rows.push("⬢ Thinking".into());
            }
            rows.extend(["╭────╮".into(), format!("│ > {draft}"), "╰────╯".into()]);
        }
        "pi" => rows.extend([
            if busy {
                "── ⠋ Working ───"
            } else {
                "────────"
            }
            .into(),
            draft.into(),
            "────────".into(),
            "model / auto 1%".into(),
        ]),
        _ => unreachable!(),
    }
    rows
}

#[test]
fn cursor_status_spinner_is_not_transcript_evidence() {
    let t = Instant::now();
    let mut evidence = OutputEvidence::default();
    let first = cursor_cli(Some("\u{2800}\u{28d4} Thinking"), "draft");
    let spun = cursor_cli(Some("\u{2820}\u{2818} Thinking"), "draft changed");
    assert_eq!(evidence.observe("cursor", &first, Some(t), None, t), None);
    assert_eq!(
        evidence.observe("cursor", &spun, Some(at(t, 300)), None, at(t, 300)),
        None
    );
    let mut answered = spun;
    answered[1] = "The patch changed.".to_string();
    assert_eq!(
        evidence.observe("cursor", &answered, Some(at(t, 400)), None, at(t, 400)),
        Some(at(t, 400))
    );
}

#[test]
fn evidence_ignores_drafts_status_and_identical_redraws_for_every_preset() {
    let t = Instant::now();
    for preset in ["claude", "codex", "cursor", "pi"] {
        let mut evidence = OutputEvidence::default();
        assert_eq!(
            evidence.observe(
                preset,
                &evidence_rows(preset, &["First answer"], "", false),
                Some(t),
                None,
                t
            ),
            None
        );
        assert_eq!(
            evidence.observe(
                preset,
                &evidence_rows(preset, &["First answer"], "Yes", true),
                Some(at(t, 100)),
                None,
                at(t, 100)
            ),
            None
        );
        let changed = evidence_rows(preset, &["Second answer"], "a new draft", true);
        assert_eq!(
            evidence.observe(preset, &changed, Some(at(t, 200)), None, at(t, 200)),
            Some(at(t, 200))
        );
        assert_eq!(
            evidence.observe(preset, &changed, Some(at(t, 300)), None, at(t, 300)),
            Some(at(t, 200))
        );
        assert_eq!(
            evidence.observe(
                preset,
                &evidence_rows(preset, &["Second answer"], "typing more", false),
                Some(at(t, 400)),
                None,
                at(t, 400)
            ),
            Some(at(t, 200))
        );
        // Sustained typing never hides a genuine transcript change, provided
        // the caller follows the geometry-only interaction contract.
        assert_eq!(
            evidence.observe(
                preset,
                &evidence_rows(preset, &["Third answer"], "still typing", false),
                Some(at(t, 500)),
                None,
                at(t, 500)
            ),
            Some(at(t, 500))
        );
    }
}

#[test]
fn evidence_normalizes_word_wrap_hard_wrap_and_padding() {
    let t = Instant::now();
    let mut evidence = OutputEvidence::default();
    evidence.observe(
        "claude",
        &evidence_rows("claude", &["An answer /long/path"], "", false),
        Some(t),
        None,
        t,
    );
    let wrapped = evidence_rows("claude", &["  An ", "answer /long/", "path  "], "", false);
    assert_eq!(
        evidence.observe("claude", &wrapped, Some(at(t, 2000)), None, at(t, 2000)),
        None
    );
    assert_eq!(
        evidence.observe(
            "claude",
            &evidence_rows("claude", &["An changed answer /long/path"], "", false),
            Some(at(t, 3000)),
            None,
            at(t, 3000)
        ),
        Some(at(t, 3000))
    );
}

#[test]
fn evidence_consumes_suppressed_geometry_baseline() {
    let t = Instant::now();
    let mut evidence = OutputEvidence::default();
    evidence.observe(
        "codex",
        &evidence_rows("codex", &["Old top", "Answer"], "", false),
        Some(t),
        None,
        t,
    );
    let clipped = evidence_rows("codex", &["Answer"], "", false);
    assert_eq!(
        evidence.observe(
            "codex",
            &clipped,
            Some(at(t, 500)),
            Some(at(t, 100)),
            at(t, 500)
        ),
        None
    );
    // Even a new redraw timestamp outside ECHO cannot credit that same
    // geometry-induced change later.
    assert_eq!(
        evidence.observe(
            "codex",
            &clipped,
            Some(at(t, 2000)),
            Some(at(t, 100)),
            at(t, 2000)
        ),
        None
    );
    assert_eq!(
        evidence.observe(
            "codex",
            &evidence_rows("codex", &["Answer", "New result"], "", false),
            Some(at(t, 2100)),
            Some(at(t, 100)),
            at(t, 2100)
        ),
        Some(at(t, 2100))
    );
}

#[test]
fn evidence_keeps_fenced_code_including_chrome_like_strings() {
    let t = Instant::now();
    let mut evidence = OutputEvidence::default();
    evidence.observe(
        "codex",
        &evidence_rows(
            "codex",
            &["```", "Working (1s)", "› quoted prompt", "```"],
            "",
            false,
        ),
        Some(t),
        None,
        t,
    );
    let code = evidence_rows(
        "codex",
        &["```", "Working (2s)", "› quoted prompt", "```"],
        "",
        false,
    );
    assert_eq!(
        evidence.observe("codex", &code, Some(at(t, 100)), None, at(t, 100)),
        Some(at(t, 100))
    );
    assert_eq!(
        evidence.observe("codex", &code, Some(at(t, 200)), None, at(t, 200)),
        Some(at(t, 100))
    );
}

#[test]
fn evidence_unfinished_fences_do_not_swallow_drafts() {
    let t = Instant::now();
    for preset in ["claude", "codex", "cursor", "pi"] {
        let mut evidence = OutputEvidence::default();
        let transcript = ["```rust", "fn unfinished() {"];
        evidence.observe(
            preset,
            &evidence_rows(preset, &transcript, "", false),
            Some(t),
            None,
            t,
        );
        assert_eq!(
            evidence.observe(
                preset,
                &evidence_rows(preset, &transcript, "new draft", false),
                Some(at(t, 100)),
                None,
                at(t, 100)
            ),
            None,
            "{preset}"
        );
        assert_eq!(
            evidence.observe(
                preset,
                &evidence_rows(
                    preset,
                    &["```rust", "fn unfinished() {", "return 1;"],
                    "new draft",
                    false
                ),
                Some(at(t, 200)),
                None,
                at(t, 200)
            ),
            Some(at(t, 200)),
            "{preset}"
        );
    }
}

#[test]
fn evidence_requires_new_nonfuture_bytes_and_updates_uncredited_baselines() {
    let t = Instant::now();
    let mut evidence = OutputEvidence::default();
    let first = evidence_rows("pi", &["First"], "", false);
    let second = evidence_rows("pi", &["Second"], "", false);
    evidence.observe("pi", &first, Some(t), None, t);
    assert_eq!(
        evidence.observe("pi", &second, None, None, at(t, 100)),
        None
    );
    assert_eq!(
        evidence.observe("pi", &second, Some(at(t, 200)), None, at(t, 200)),
        None
    );
    let third = evidence_rows("pi", &["Third"], "", false);
    assert_eq!(
        evidence.observe("pi", &third, Some(at(t, 1000)), None, at(t, 300)),
        None
    );
    assert_eq!(
        evidence.observe("pi", &third, Some(at(t, 1000)), None, at(t, 1000)),
        None
    );
    let fourth = evidence_rows("pi", &["Fourth"], "", false);
    assert_eq!(
        evidence.observe("pi", &fourth, Some(at(t, 1100)), None, at(t, 1100)),
        Some(at(t, 1100))
    );
    assert_eq!(
        evidence.observe("pi", &first, Some(at(t, 1000)), None, at(t, 1200)),
        Some(at(t, 1100))
    );
    assert_eq!(
        evidence.observe(
            "claude",
            &evidence_rows("claude", &["Other CLI"], "", false),
            Some(at(t, 1300)),
            None,
            at(t, 1300)
        ),
        None
    );
}

#[test]
fn redraws_cannot_extend_unknown_quiet_fallback() {
    let t = Instant::now();
    let mut evidence = OutputEvidence::default();
    let mut activity = Activity::new(t);
    evidence.observe(
        "claude",
        &evidence_rows("claude", &[], "", false),
        Some(t),
        None,
        t,
    );
    activity.advance(Signal::Unknown, Some(t), None, t, false);
    let output = evidence.observe(
        "claude",
        &evidence_rows("claude", &["Real result"], "", false),
        Some(at(t, 100)),
        None,
        at(t, 100),
    );
    activity.advance(Signal::Unknown, None, output, at(t, 100), false);
    for ms in [500, 1000, 2000, 2100] {
        let output = evidence.observe(
            "claude",
            &evidence_rows("claude", &["Real result"], "draft typing", false),
            Some(at(t, ms)),
            None,
            at(t, ms),
        );
        assert_eq!(output, Some(at(t, 100)));
        assert_eq!(
            activity
                .advance(Signal::Unknown, None, output, at(t, ms), false)
                .ready,
            ms == 2100
        );
    }
}

#[test]
fn future_and_pre_turn_events_are_ignored() {
    let t = Instant::now();
    let mut a = Activity::new(t);
    a.advance(
        Signal::Unknown,
        Some(at(t, 100)),
        Some(at(t, 100)),
        t,
        false,
    );
    assert_eq!(a.status, Status::Waiting);
    a.advance(Signal::Working, None, Some(t), at(t, 100), false);
    assert!(
        !a.advance(Signal::Unknown, None, Some(t), at(t, 10000), false)
            .ready
    );
    assert_eq!(a.since, at(t, 100));
}
