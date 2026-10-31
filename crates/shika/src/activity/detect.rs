//! `detect`: reads one live screen and title into a `Signal`, checking
//! overlays, dialogs, working chrome, titles, and idle editors in that order.
use super::Signal;
use super::chrome::{
    cursor_processing_prompt, prompt_body, rule, screen_frame, spinner_title, working_row,
};

/// Detect only anchored status chrome, not words in responses or draft input.
/// Titles are used only for explicit dynamic state markers, not arbitrary text.
pub fn detect(preset: &str, lines: &[String], title: Option<&str>) -> Signal {
    if !matches!(preset, "claude" | "codex" | "cursor" | "pi") {
        return Signal::Unknown;
    }
    let (rows, fenced) = unfenced_rows(lines);
    let tail = last_content_rows(&rows);
    let lower = bottom_text(&tail);
    if fenced || full_screen_view(&lower) {
        return Signal::Unknown;
    }

    // A current prompt bounds history above and drafts below. Permission option
    // selectors use the same glyph, so numbered options are not input prompts.
    // Cursor's composer is a `→` row. Half-block bars frame it; a `>` quote above
    // that row is transcript text, not the editor.
    let prompt = rows.iter().rposition(|s| prompt_body(preset, s).is_some());
    let top = prompt.and_then(|p| (0..p).rev().find(|&i| screen_frame(preset, rows[i])));
    let bottom =
        prompt.and_then(|p| ((p + 1)..rows.len()).find(|&i| screen_frame(preset, rows[i])));
    let boxed = top.zip(bottom);
    let live: Vec<&str> = if let Some(p) = prompt {
        // Only footer controls after the input can block. Never inspect drafts.
        rows[bottom.unwrap_or(p) + 1..].to_vec()
    } else {
        tail.clone()
    };
    if dialog_blocked(preset, &live) {
        return Signal::Blocked;
    }

    // Examine only the final content row above the live editor. A completed
    // response after a historical spinner prevents that spinner matching.
    let pi_top = pi_editor_top(preset, &rows);
    let end = boxed
        .map(|(t, _)| t)
        .or(prompt)
        .or(pi_top)
        .unwrap_or(rows.len());
    let candidate = rows[..end]
        .iter()
        .rev()
        .copied()
        .find(|s| !s.is_empty() && !screen_frame(preset, s));
    let working = candidate.is_some_and(|s| working_row(preset, s))
        || (preset == "cursor"
            && boxed.is_some()
            && live
                .iter()
                .any(|s| s.eq_ignore_ascii_case("ctrl+c to stop")))
        || (preset == "cursor"
            && prompt.is_some_and(|index| cursor_processing_prompt(rows[index])));
    let pi_border = pi_border_working(&rows, pi_top);
    let title = title.map(str::trim);
    if preset == "codex"
        && title.is_some_and(|s| {
            s == "Action Required"
                || s.starts_with("Action Required - ")
                || s == "Codex - Action Required"
        })
    {
        return Signal::Blocked;
    }
    if working || pi_border {
        return Signal::Working;
    }
    if let Some(signal) = title.and_then(|title| title_signal(preset, title)) {
        return signal;
    }
    editor_idle(preset, &rows, prompt, boxed.is_some(), &lower)
}

/// Trimmed rows, with fenced examples blanked so their contents never become
/// live chrome. Also returns whether a fence is still open at the bottom.
fn unfenced_rows(lines: &[String]) -> (Vec<&str>, bool) {
    // Remove fenced examples without making their contents into live chrome.
    let mut fenced = false;
    let rows: Vec<&str> = lines
        .iter()
        .map(|line| {
            let line = line.trim();
            if line.starts_with("```") || line.starts_with("~~~") {
                fenced = !fenced;
                ""
            } else if fenced {
                ""
            } else {
                line
            }
        })
        .collect();
    (rows, fenced)
}

/// The last 12 non-empty rows, in screen order.
fn last_content_rows<'a>(rows: &[&'a str]) -> Vec<&'a str> {
    rows.iter()
        .copied()
        .filter(|s| !s.is_empty())
        .rev()
        .take(12)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

/// The last four content rows, joined and lowercased, for footer hints.
fn bottom_text(tail: &[&str]) -> String {
    tail.iter()
        .rev()
        .take(4)
        .copied()
        .collect::<Vec<_>>()
        .join("\n")
        .to_lowercase()
}

/// Transcript, pager, model picker, or session tree views hide the editor.
fn full_screen_view(lower: &str) -> bool {
    lower.contains("showing detailed transcript")
        || (lower.contains("q to quit") && lower.contains("to scroll"))
        || (lower.contains("select model") && lower.contains("esc to cancel"))
        || (lower.contains("session tree") && lower.contains("esc"))
}

/// A recognizable permission or question dialog in the live footer rows.
fn dialog_blocked(preset: &str, live: &[&str]) -> bool {
    let text = live.join("\n").to_lowercase();
    let has = |needle: &str| text.contains(needle);
    let controls = live.iter().any(|s| {
        let s = s.to_lowercase();
        // Reject prose/quoted examples by requiring a control at line start.
        s.starts_with("esc ")
            || s.starts_with("enter ")
            || s.starts_with("press enter ")
            || s.starts_with("↑")
            || s.starts_with("tab ")
    });
    let selected = live.iter().any(|s| {
        s.starts_with('❯')
            || s.starts_with('›')
            || s.starts_with('→')
            || s.starts_with("1.")
            || s.starts_with("1)")
    });
    match preset {
        "claude" => {
            (controls
                && has("esc to cancel")
                && (has("enter to confirm") || has("enter to select")))
                || (selected
                    && has("esc to cancel")
                    && (has("do you want to proceed?")
                        || has("do you want to allow this connection?")
                        || has("run a dynamic workflow?")
                        || has("requests your input")))
        }
        "codex" => {
            (controls
                && (has("press enter to confirm or esc to cancel")
                    || has("enter to submit answer")
                    || has("enter to submit all")))
                || (selected
                    && (has("allow command?")
                        || has("trust this folder?")
                        || has("do you trust the contents of this directory?"))
                    && (has("esc") || has("enter") || has("trust and continue")))
        }
        "cursor" => {
            selected
                && ((has("run this command?")
                    && has("waiting for approval")
                    && (has("run (once) (y)") || has("skip (esc or n)")))
                    || (has("write to this file?")
                        && has("proceed (y)")
                        && (has("reject & propose changes") || has("esc or n or p"))))
        }
        "pi" => {
            selected
                && has("project trust")
                && controls
                && has("navigate")
                && has("enter")
                && has("save")
                && has("cancel")
        }
        _ => false,
    }
}

/// Pi's editor top border: the rule above the last rule, or its `── ` status border.
fn pi_editor_top(preset: &str, rows: &[&str]) -> Option<usize> {
    if preset == "pi" {
        rows.iter().rposition(|s| rule(s)).and_then(|bottom| {
            (0..bottom)
                .rev()
                .find(|&i| rule(rows[i]) || rows[i].starts_with("── "))
        })
    } else {
        None
    }
}

/// Pi's current indicator is embedded in the editor's top border.
fn pi_border_working(rows: &[&str], pi_top: Option<usize>) -> bool {
    pi_top.is_some_and(|i| {
        let s = rows[i];
        s.starts_with("── ")
            && s.ends_with("───")
            && working_row(
                "pi",
                s.trim_start_matches('─')
                    .trim()
                    .trim_end_matches('─')
                    .trim(),
            )
            && rows[i + 1..].iter().filter(|s| rule(s)).count() == 1
    })
}

/// Explicit state markers in the terminal title: a spinner title is Working,
/// Claude's static sparkle is Idle. Any other title says nothing.
fn title_signal(preset: &str, title: &str) -> Option<Signal> {
    let state_title = if preset == "codex" {
        title
            .strip_prefix("Codex - ")
            .or_else(|| title.strip_prefix("Codex "))
            .unwrap_or(title)
    } else {
        title
    };
    if matches!(preset, "claude" | "codex") && spinner_title(state_title) {
        return Some(Signal::Working);
    }
    // Claude uses a static sparkle instead of its animated busy title.
    // Unlike an arbitrary Codex/Pi title, this is an explicit idle marker.
    if preset == "claude" && title.starts_with("✳ ") && title.len() > "✳ ".len() {
        return Some(Signal::Idle);
    }
    None
}

/// Idle only from a framed editor, a known prompt footer hint, or Pi's editor
/// with its model footer; otherwise Unknown.
fn editor_idle(
    preset: &str,
    rows: &[&str],
    prompt: Option<usize>,
    boxed: bool,
    lower: &str,
) -> Signal {
    // A draft can coexist with a running turn. Only framed editors or known
    // prompt footer hints establish Idle, and Working always wins above.
    if boxed
        || (prompt.is_some()
            && (lower.contains("? for shortcuts")
                || lower.contains("context left")
                || lower.contains("tab to queue")
                || lower.contains("/ for commands")))
    {
        return Signal::Idle;
    }
    if preset == "pi" {
        let borders = rows.iter().filter(|s| rule(s)).count();
        if borders >= 2
            && rows.iter().rev().find(|s| !s.is_empty()).is_some_and(|s| {
                // Pi's footer includes the model and context usage. A plain pair of
                // transcript rules is not an editor.
                s.contains('%') && (s.contains("auto") || s.contains("/"))
            })
        {
            return Signal::Idle;
        }
    }
    Signal::Unknown
}
