//! `Activity`: the clock-driven turn state machine, its debounce, startup
//! grace, quiet fallback, and notification budget.
use std::time::{Duration, Instant};

use super::Signal;
use crate::model::{QUIET, Status};

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

    /// Chat has explicit turn boundaries. Never apply terminal debounce or
    /// quiet-time inference to an authoritative terminal result.
    pub fn advance_chat(&mut self, status: Status, submitted: bool, now: Instant) -> Transition {
        match status {
            Status::Ready if matches!(self.status, Status::Working | Status::Asking) => {
                self.finish(now, self.status)
            }
            Status::Working | Status::Asking => self.advance_authoritative(
                if status == Status::Asking {
                    Signal::Blocked
                } else {
                    Signal::Working
                },
                submitted.then_some(now),
                None,
                now,
                false,
            ),
            _ => Transition::default(),
        }
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
            self.start_turn(submission, now);
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
        self.record_output(output, start, now);
        let mut notify = false;
        match signal {
            Signal::Working | Signal::Blocked => {
                notify = self.confirm_live(signal, now);
            }
            Signal::Idle => {
                if self.idle_settled(authoritative, start, now) {
                    return self.finish(now, old);
                }
            }
            Signal::Unknown => {
                if self.unknown_settled(start, now) {
                    return self.finish(now, old);
                }
            }
        }
        Transition {
            changed: old != self.status,
            notify,
            ready: false,
        }
    }

    /// A fresh turn epoch: the timer, evidence, and notification budget restart.
    fn start_turn(&mut self, submission: Option<Instant>, now: Instant) {
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

    /// Credits meaningful output inside this turn, once per new timestamp.
    fn record_output(&mut self, output: Option<Instant>, start: Instant, now: Instant) {
        if let Some(t) = output
            .filter(|t| *t <= now && *t >= start && self.last_output.is_none_or(|last| *t > last))
        {
            self.last_output = Some(t);
            self.last_evidence = Some(self.last_evidence.unwrap_or(t).max(t));
            self.reset_idle();
        }
    }

    /// Live Working or Blocked chrome confirms the turn. Returns whether this
    /// is the turn's first blocker, which notifies.
    fn confirm_live(&mut self, signal: Signal, now: Instant) -> bool {
        let mut notify = false;
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
        notify
    }

    /// Counts an Idle observation. True once the turn may finish: eligible,
    /// seen twice, and idle for the debounce.
    fn idle_settled(&mut self, authoritative: bool, start: Instant, now: Instant) -> bool {
        let first = *self.idle_since.get_or_insert(now);
        self.idle_observations = self.idle_observations.saturating_add(1);
        let booting = self.launch_turn && !self.confirmed;
        let eligible = authoritative
            || self.confirmed
            || (self.last_output.is_some() && !booting)
            || now.saturating_duration_since(start) >= SUBMISSION_GRACE;
        eligible
            && self.idle_observations >= 2
            && now.saturating_duration_since(first) >= IDLE_DEBOUNCE
    }

    /// Unknown resets the idle count. True once the quiet fallback or the
    /// submission grace finishes the turn; Asking is never finished here.
    fn unknown_settled(&mut self, start: Instant, now: Instant) -> bool {
        self.reset_idle();
        // An unreadable permission overlay is not evidence that it was
        // answered. Preserve Asking until visible resume, idle or exit.
        if self.status != Status::Asking {
            let booting = self.launch_turn && !self.confirmed;
            let quiet = self.last_output.is_some()
                && !booting
                && now.saturating_duration_since(self.last_evidence.unwrap_or(start)) >= QUIET;
            let grace = self.submitted
                && (self.last_output.is_none() || booting)
                && now.saturating_duration_since(self.last_evidence.unwrap_or(start))
                    >= SUBMISSION_GRACE;
            if quiet || grace {
                return true;
            }
        }
        false
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
