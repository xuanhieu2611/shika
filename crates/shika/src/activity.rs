//! Conservative live-screen detection and a clock-driven activity state machine.
//!
//! `detect` expects the terminal's current, ANSI-decoded screen in row order,
//! never scrollback or a PTY chunk. Private CLI chrome can change: unfamiliar
//! layouts deliberately return Unknown. Textual questions are not blockers.
//! This implementation is original; Herdr's manifests were consulted as research
//! only (the inspected checkout declares Apache-2.0, not MIT).
use std::time::{Duration, Instant};

use crate::model::{QUIET, Status, is_agent_output};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Signal {
    Unknown,
    Idle,
    Working,
    Blocked,
}

/// Detect only anchored status chrome, not words in responses or draft input.
/// Titles are used only for explicit dynamic state markers, not arbitrary text.
pub fn detect(preset: &str, lines: &[String], title: Option<&str>) -> Signal {
    if !matches!(preset, "claude" | "codex" | "cursor" | "pi") {
        return Signal::Unknown;
    }
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
    let tail: Vec<&str> = rows
        .iter()
        .copied()
        .filter(|s| !s.is_empty())
        .rev()
        .take(12)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    let lower = tail
        .iter()
        .rev()
        .take(4)
        .copied()
        .collect::<Vec<_>>()
        .join("\n")
        .to_lowercase();
    if fenced
        || lower.contains("showing detailed transcript")
        || (lower.contains("q to quit") && lower.contains("to scroll"))
        || (lower.contains("select model") && lower.contains("esc to cancel"))
        || (lower.contains("session tree") && lower.contains("esc"))
    {
        return Signal::Unknown;
    }

    // A current prompt bounds history above and drafts below. Permission option
    // selectors use the same glyph, so numbered options are not input prompts.
    let prompt = rows.iter().rposition(|s| prompt_body(preset, s).is_some());
    let top = prompt.and_then(|p| (0..p).rev().find(|&i| rule(rows[i])));
    let bottom = prompt.and_then(|p| ((p + 1)..rows.len()).find(|&i| rule(rows[i])));
    let boxed = top.zip(bottom);
    let live: Vec<&str> = if let Some(p) = prompt {
        // Only footer controls after the input can block. Never inspect drafts.
        rows[bottom.unwrap_or(p) + 1..].to_vec()
    } else {
        tail.clone()
    };
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
    let blocked = match preset {
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
    };
    if blocked {
        return Signal::Blocked;
    }

    // Examine only the final content row above the live editor. A completed
    // response after a historical spinner prevents that spinner matching.
    let pi_top = if preset == "pi" {
        rows.iter().rposition(|s| rule(s)).and_then(|bottom| {
            (0..bottom)
                .rev()
                .find(|&i| rule(rows[i]) || rows[i].starts_with("── "))
        })
    } else {
        None
    };
    let end = boxed
        .map(|(t, _)| t)
        .or(prompt)
        .or(pi_top)
        .unwrap_or(rows.len());
    let candidate = rows[..end]
        .iter()
        .rev()
        .copied()
        .find(|s| !s.is_empty() && !rule(s));
    let working = candidate.is_some_and(|s| working_row(preset, s))
        || (preset == "cursor"
            && boxed.is_some()
            && live
                .iter()
                .any(|s| s.eq_ignore_ascii_case("ctrl+c to stop")));
    // Pi's current indicator is embedded in the editor's top border.
    let pi_border = pi_top.is_some_and(|i| {
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
    });
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
    if let Some(title) = title {
        let state_title = if preset == "codex" {
            title
                .strip_prefix("Codex - ")
                .or_else(|| title.strip_prefix("Codex "))
                .unwrap_or(title)
        } else {
            title
        };
        if matches!(preset, "claude" | "codex") && spinner_title(state_title) {
            return Signal::Working;
        }
        // Claude uses a static sparkle instead of its animated busy title.
        // Unlike an arbitrary Codex/Pi title, this is an explicit idle marker.
        if preset == "claude" && title.starts_with("✳ ") && title.len() > "✳ ".len() {
            return Signal::Idle;
        }
    }
    // A draft can coexist with a running turn. Only framed editors or known
    // prompt footer hints establish Idle, and Working always wins above.
    if boxed.is_some()
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

fn prompt_body<'a>(preset: &str, row: &'a str) -> Option<&'a str> {
    let row = row.trim_start_matches('│').trim();
    let marker = match preset {
        "claude" => '❯',
        "codex" => '›',
        "cursor" => {
            if row.starts_with('❯') {
                '❯'
            } else {
                '>'
            }
        }
        _ => return None,
    };
    let body = row.strip_prefix(marker)?.trim();
    let lower = body.to_lowercase();
    if (body.chars().next().is_some_and(|c| c.is_ascii_digit())
        && (body.contains(". ") || body.contains(") ")))
        || matches!(
            lower.as_str(),
            "yes" | "no" | "accept" | "decline" | "trust and continue"
        )
        || lower.starts_with("yes, ")
    {
        return None;
    }
    Some(body)
}

fn rule(s: &str) -> bool {
    s.chars().count() >= 3
        && s.chars().all(|c| {
            matches!(
                c,
                '─' | '━'
                    | '═'
                    | '╭'
                    | '╮'
                    | '╰'
                    | '╯'
                    | '┌'
                    | '┐'
                    | '└'
                    | '┘'
                    | '├'
                    | '┤'
                    | '┬'
                    | '┴'
                    | '┼'
            )
        })
}

fn spinner(c: char) -> bool {
    matches!(c, '\u{2801}'..='\u{28ff}' | '◐' | '◑' | '◒' | '◓')
}

fn spinner_title(s: &str) -> bool {
    let mut chars = s.chars();
    chars.next().is_some_and(spinner)
        && chars.next() == Some(' ')
        && !chars.as_str().trim().is_empty()
}

fn working_row(preset: &str, s: &str) -> bool {
    let lower = s.to_lowercase();
    let Some(first) = s.chars().next() else {
        return false;
    };
    let rest = s[first.len_utf8()..].trim();
    match preset {
        "claude" => {
            matches!(first, '*' | '·' | '✢' | '✳' | '✶' | '✻' | '✽' | '⏸' | '⏵')
                && (lower.contains("esc to interrupt")
                    || (rest.contains('…') && (rest.ends_with('…') || elapsed_suffix(rest)))
                    || (rest.starts_with("Waiting for ") && rest.ends_with(" to finish")))
        }
        "codex" => {
            let label = s
                .rsplit_once(" (")
                .map(|(label, _)| label)
                .unwrap_or(s)
                .trim_start_matches(['•', '◦'])
                .trim();
            let words: Vec<&str> = label.split_whitespace().collect();
            let activity = words
                .first()
                .is_some_and(|word| word.ends_with("ing") && word.chars().all(char::is_alphabetic));
            // An elapsed duration alone does not make prose into a live status.
            // Reduced-motion chrome has one activity label. Dynamic summaries
            // additionally need the explicit interrupt control, and completed
            // prose must never pin a card Working forever.
            let completed = words.iter().any(|word| {
                matches!(
                    word.to_lowercase()
                        .trim_matches(|c: char| !c.is_alphabetic()),
                    "completed"
                        | "complete"
                        | "finished"
                        | "done"
                        | "took"
                        | "was"
                        | "example"
                        | "result"
                )
            });
            activity
                && !completed
                && (words.len() == 1 || s.contains(" to interrupt)"))
                && !matches!(first, '›' | '■' | '✓' | '✗' | '>' | '`' | '"')
                && elapsed_suffix(s)
        }
        "cursor" => {
            lower == "ctrl+c to stop"
                || lower.starts_with("ctrl+c to stop ·")
                || ((spinner(first) || matches!(first, '⬡' | '⬢'))
                    && rest
                        .split_whitespace()
                        .next()
                        .is_some_and(|s| s.ends_with("ing")))
        }
        "pi" => spinner(first) && (rest == "Working" || rest.starts_with("Working (")),
        _ => false,
    }
}

/// Require the CLI timer grammar, not an arbitrary parenthesized sentence.
fn elapsed_suffix(s: &str) -> bool {
    let Some((_, suffix)) = s.rsplit_once(" (") else {
        return false;
    };
    let Some(suffix) = suffix.strip_suffix(')') else {
        return false;
    };
    let timer = suffix.split(" • ").next().unwrap_or("");
    if suffix
        .split_once(" • ")
        .is_some_and(|(_, hint)| !hint.ends_with(" to interrupt"))
    {
        return false;
    }
    let units: Vec<&str> = timer.split_whitespace().collect();
    !units.is_empty()
        && units.len() <= 3
        && units.last().is_some_and(|s| s.ends_with('s'))
        && units.iter().all(|s| {
            s.len() >= 2
                && matches!(s.as_bytes().last(), Some(b'h' | b'm' | b's'))
                && s[..s.len() - 1].bytes().all(|c| c.is_ascii_digit())
        })
}

/// Screen-based evidence for Unknown's quiet fallback. Raw output timestamps
/// are only a sampling watermark, never evidence by themselves. Keep one helper
/// per agent terminal, not per visible tab or per turn.
#[derive(Default)]
pub struct OutputEvidence {
    preset: String,
    transcript: Option<String>,
    observed_output: Option<Instant>,
    meaningful_output: Option<Instant>,
}

impl OutputEvidence {
    /// Returns the stable last meaningful output timestamp. `lines` must be the
    /// live screen, not the scrolled viewport. The first sample establishes a
    /// baseline without crediting startup text. Drafts, footers and live status
    /// animation are excluded; code inside fences is real transcript content.
    ///
    /// `interaction` must be the latest geometry/focus/scroll interaction, NOT
    /// typing. Draft signatures already exclude typing, so applying ECHO to keys
    /// would hide genuine responses while the user types. Within ECHO of that
    /// interaction, changed content is suppressed but its baseline and byte
    /// watermark are still consumed: delayed identical redraws cannot qualify.
    pub fn observe(
        &mut self,
        preset: &str,
        lines: &[String],
        output: Option<Instant>,
        interaction: Option<Instant>,
        now: Instant,
    ) -> Option<Instant> {
        if self.preset != preset {
            self.preset = preset.to_owned();
            self.transcript = None;
            self.observed_output = None;
            self.meaningful_output = None;
        }
        let signature = transcript_signature(preset, lines);
        let changed = self
            .transcript
            .as_ref()
            .is_some_and(|previous| previous != &signature);
        // Always update, including snapshots with no new bytes or suppressed
        // geometry changes. A future/old timestamp cannot retroactively credit
        // an already observed screen change.
        self.transcript = Some(signature);
        if let Some(output) = output
            .filter(|t| *t <= now && self.observed_output.is_none_or(|previous| *t > previous))
        {
            self.observed_output = Some(output);
            let geometry_echo = !is_agent_output(interaction.filter(|t| *t <= now), output);
            if changed && !geometry_echo {
                self.meaningful_output = Some(output);
            }
        }
        self.meaningful_output
    }
}

fn transcript_signature(preset: &str, lines: &[String]) -> String {
    let rows: Vec<&str> = lines.iter().map(|s| s.trim()).collect();
    let mut fenced = false;
    let in_code: Vec<bool> = rows
        .iter()
        .map(|s| {
            let code = fenced;
            if s.starts_with("```") || s.starts_with("~~~") {
                fenced = !fenced;
            }
            code
        })
        .collect();
    // Evidence cares about the input boundary, not whether a prompt is a
    // permission selector. In particular a draft consisting of "Yes" remains
    // input and cannot manufacture output evidence.
    let editor_footer = rows.iter().rev().find(|s| !s.is_empty()).is_some_and(|s| {
        s.contains("context left")
            || s.starts_with("? for shortcuts")
            || (preset == "pi" && s.contains('%') && (s.contains("auto") || s.contains('/')))
    });
    let prompt = rows.iter().enumerate().rposition(|(i, s)| {
        let s = s.trim_start_matches('│').trim();
        let marker = match preset {
            "claude" => s.starts_with('❯'),
            "codex" => s.starts_with('›'),
            "cursor" => s.starts_with('>') || s.starts_with('❯'),
            _ => false,
        };
        // An unfinished response fence must not swallow the real editor. Its
        // framing/footer takes precedence over transcript Markdown context.
        marker
            && (!in_code[i]
                || editor_footer
                || (rows[..i].iter().any(|s| rule(s)) && rows[i + 1..].iter().any(|s| rule(s))))
    });
    let end = if let Some(prompt) = prompt {
        let top = (0..prompt).rev().find(|&i| rule(rows[i]));
        let bottom = ((prompt + 1)..rows.len()).any(|i| rule(rows[i]));
        if bottom {
            top.unwrap_or(prompt)
        } else {
            prompt
        }
    } else if preset == "pi" {
        rows.iter()
            .enumerate()
            .rposition(|(i, s)| (!in_code[i] || editor_footer) && rule(s))
            .and_then(|bottom| {
                (0..bottom).rev().find(|&i| {
                    (!in_code[i] || editor_footer) && (rule(rows[i]) || rows[i].starts_with("── "))
                })
            })
            .unwrap_or(rows.len())
    } else {
        rows.len()
    };
    let mut signature = String::new();
    for (i, row) in rows[..end].iter().enumerate() {
        if !in_code[i]
            && (rule(row)
                || working_row(preset, row)
                || footer_row(row)
                || (preset == "pi" && row.starts_with("── ") && row.ends_with("───")))
        {
            continue;
        }
        // Ignore all whitespace rather than row boundaries. Both word wrapping
        // and hard wrapping long paths on resize then preserve the signature.
        // Whitespace-only transcript edits are deliberately not evidence.
        signature.extend(row.chars().filter(|c| !c.is_whitespace()));
    }
    signature
}

fn footer_row(row: &str) -> bool {
    let row = row.to_lowercase();
    row.starts_with("? for shortcuts")
        || row.starts_with("esc to ")
        || row.starts_with("enter to ")
        || row.starts_with("press enter to ")
        || row.starts_with("ctrl+c to stop")
        || row.starts_with("↑/↓ to scroll")
        || row.starts_with("showing detailed transcript")
}

const IDLE_DEBOUNCE: Duration = Duration::from_millis(500);
// Enter can be a local command, an echo, or a rejected submission. Without
// meaningful output or confirmed live activity, allow eight seconds to launch
// before treating a tentative turn as Ready, even if an idle editor is visible.
const SUBMISSION_GRACE: Duration = Duration::from_secs(8);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Transition {
    pub changed: bool,
    pub notify: bool,
    /// An edge into Ready, independent of notification suppression. The caller
    /// should mark the completed result unseen even after a blocker notified.
    pub ready: bool,
}

pub struct Activity {
    pub status: Status,
    /// Status epoch; unchanged across Working/Asking so the timer is stable.
    pub since: Instant,
    /// Retained through Ready: an unsubmitted Working signal resumes the same
    /// turn, including its timer and notification budget. Only a fresh submitted
    /// prompt from Waiting/Ready (or first-ever strong signal) creates an epoch.
    pub turn_started: Option<Instant>,
    last_submission: Option<Instant>,
    last_output: Option<Instant>,
    last_evidence: Option<Instant>,
    submitted: bool,
    confirmed: bool,
    /// The next submission is a CLI launch prompt, not a typed line.
    launch_pending: bool,
    /// The current turn began from a launch prompt. Until work is seen, the
    /// CLI is still booting, so boot output and an idle editor are not
    /// evidence that the prompt was answered.
    launch_turn: bool,
    idle_since: Option<Instant>,
    idle_observations: u8,
    notified: bool,
    exited: bool,
}

impl Activity {
    pub fn new(now: Instant) -> Self {
        Self {
            status: Status::Waiting,
            since: now,
            turn_started: None,
            last_submission: None,
            last_output: None,
            last_evidence: None,
            submitted: false,
            confirmed: false,
            launch_pending: false,
            launch_turn: false,
            idle_since: None,
            idle_observations: 0,
            notified: false,
            exited: false,
        }
    }

    /// Marks the next submission as the prompt the CLI was launched with
    /// (`shika new`, or a Lead's first prompt). Nobody typed it, so the CLI's
    /// banner and idle editor while it boots look like output followed by
    /// idle. Such a turn finishes only after work was seen, or after the same
    /// startup grace a typed submission has. Call it right before the
    /// `advance` that consumes that submission.
    pub fn hold_for_launch(&mut self) {
        self.launch_pending = true;
    }

    /// `submission` is the latest actual submitted prompt (not typing or every
    /// Enter in a selector). `output` is the latest *meaningful agent output*,
    /// already filtered for echoes, focus/resize redraws and editor changes.
    /// Both may be repeated on every poll. Future timestamps are ignored.
    /// Unknown plus raw bytes must never be used to start a turn.
    pub fn advance(
        &mut self,
        signal: Signal,
        submission: Option<Instant>,
        output: Option<Instant>,
        now: Instant,
        exited: bool,
    ) -> Transition {
        self.advance_with((signal, false), submission, output, now, exited)
    }

    /// Lifecycle-backed variant (for Pi's event bridge). A fresh authoritative
    /// Idle may confirm a tentative submission without waiting eight seconds;
    /// it still needs two observations and the 500ms idle debounce. The caller
    /// must discard cached/pre-submission Idle reports before accepting a new
    /// prompt. Neither initial Idle nor Unknown can start a turn here.
    pub fn advance_authoritative(
        &mut self,
        signal: Signal,
        submission: Option<Instant>,
        output: Option<Instant>,
        now: Instant,
        exited: bool,
    ) -> Transition {
        self.advance_with((signal, true), submission, output, now, exited)
    }

    fn advance_with(
        &mut self,
        observation: (Signal, bool),
        submission: Option<Instant>,
        output: Option<Instant>,
        now: Instant,
        exited: bool,
    ) -> Transition {
        let (signal, authoritative) = observation;
        let old = self.status;
        if self.exited {
            return Transition::default();
        }
        let submission =
            submission.filter(|t| *t <= now && self.last_submission.is_none_or(|last| *t > last));
        if let Some(t) = submission {
            self.last_submission = Some(t);
        }
        if exited {
            self.exited = true;
            return self.finish(now, old);
        }
        let active = matches!(self.status, Status::Working | Status::Asking);
        let strong = matches!(signal, Signal::Working | Signal::Blocked);
        if !active && (submission.is_some() || (strong && self.turn_started.is_none())) {
            let start = submission.unwrap_or(now);
            self.status = Status::Working;
            self.since = start;
            self.turn_started = Some(start);
            self.last_output = None;
            self.last_evidence = Some(start);
            self.submitted = submission.is_some();
            self.confirmed = false;
            self.launch_turn = std::mem::take(&mut self.launch_pending) && submission.is_some();
            self.notified = false;
            self.reset_idle();
        }
        if !matches!(self.status, Status::Working | Status::Asking) {
            if strong {
                // A false idle observation or a continuation without submission
                // must not reset the timer or grant another notification.
                self.status = Status::Working;
                self.since = self.turn_started.unwrap_or(now);
                self.reset_idle();
            } else {
                return Transition::default();
            }
        }
        let start = self.turn_started.unwrap();
        if let Some(t) = output
            .filter(|t| *t <= now && *t >= start && self.last_output.is_none_or(|last| *t > last))
        {
            self.last_output = Some(t);
            self.last_evidence = Some(self.last_evidence.unwrap_or(t).max(t));
            self.reset_idle();
        }
        let mut notify = false;
        match signal {
            Signal::Working | Signal::Blocked => {
                self.confirmed = true;
                self.last_evidence = Some(now);
                self.reset_idle();
                self.status = if signal == Signal::Blocked {
                    Status::Asking
                } else {
                    Status::Working
                };
                if signal == Signal::Blocked && !self.notified {
                    self.notified = true;
                    notify = true;
                }
            }
            Signal::Idle => {
                let first = *self.idle_since.get_or_insert(now);
                self.idle_observations = self.idle_observations.saturating_add(1);
                let booting = self.launch_turn && !self.confirmed;
                let eligible = authoritative
                    || self.confirmed
                    || (self.last_output.is_some() && !booting)
                    || now.saturating_duration_since(start) >= SUBMISSION_GRACE;
                if eligible
                    && self.idle_observations >= 2
                    && now.saturating_duration_since(first) >= IDLE_DEBOUNCE
                {
                    return self.finish(now, old);
                }
            }
            Signal::Unknown => {
                self.reset_idle();
                // An unreadable permission overlay is not evidence that it was
                // answered. Preserve Asking until visible resume, idle or exit.
                if self.status != Status::Asking {
                    let booting = self.launch_turn && !self.confirmed;
                    let quiet = self.last_output.is_some()
                        && !booting
                        && now.saturating_duration_since(self.last_evidence.unwrap_or(start))
                            >= QUIET;
                    let grace = self.submitted
                        && (self.last_output.is_none() || booting)
                        && now.saturating_duration_since(self.last_evidence.unwrap_or(start))
                            >= SUBMISSION_GRACE;
                    if quiet || grace {
                        return self.finish(now, old);
                    }
                }
            }
        }
        Transition {
            changed: old != self.status,
            notify,
            ready: false,
        }
    }

    fn reset_idle(&mut self) {
        self.idle_since = None;
        self.idle_observations = 0;
    }

    fn finish(&mut self, now: Instant, old: Status) -> Transition {
        let ready = old != Status::Ready;
        let notify = ready && !self.notified;
        self.notified |= notify;
        self.status = Status::Ready;
        if ready {
            self.since = now;
        }
        self.reset_idle();
        Transition {
            changed: ready,
            notify,
            ready,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
