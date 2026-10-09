//! The checks mark on a card after Create PR. Memory only, one watch per
//! card, and only for the PR Shika created or reused.
//!
//! Light by construction: GitHub is read while checks run for the pushed
//! commit, with a slowing interval, and never once they pass. A red mark is
//! read every five minutes, so a re-run on GitHub can clear it. Every watch
//! reads one local ref every few seconds, so a push from the task's shell
//! or agent starts the next round.
use shika_core::{ChecksState, PrChecks, PublishedPr};
use std::time::{Duration, Instant};

/// Checks need a moment to register after a push.
const FIRST_POLL: Duration = Duration::from_secs(10);
/// An empty rollup this soon after a push is more likely "not yet" than
/// "this repository has no CI".
const NO_CHECKS_GRACE: Duration = Duration::from_secs(180);
/// gh can briefly report the previous head after a push.
const EXPECT_GRACE: Duration = Duration::from_secs(120);
/// Stop reading GitHub for a run that never finishes. A new push restarts.
const GIVE_UP: Duration = Duration::from_secs(6 * 60 * 60);
const REF_EVERY: Duration = Duration::from_secs(10);
const MAX_INTERVAL: Duration = Duration::from_secs(300);

/// What the card paints after the PR number.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mark {
    None,
    Pending,
    Passed,
    Failed,
}

/// The next read a watch wants. The caller runs it off the main thread and
/// reports back with [`PrWatch::polled`] or [`PrWatch::ref_read`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Due {
    /// `gh pr view` for the head and checks.
    Poll,
    /// The local `origin/<branch>` ref.
    Ref,
}

/// A finished read, carried back to the main thread.
pub enum Read {
    Poll(Option<PrChecks>),
    Ref(Option<String>),
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Polled {
    pub changed: bool,
    /// The mark just turned red: post one notification.
    pub failed: bool,
    /// Merged or closed: drop the watch and its mark.
    pub closed: bool,
}

pub struct PrWatch {
    pub repository: String,
    pub number: u64,
    pub url: String,
    /// The PR head the state belongs to, once GitHub has reported it.
    head: Option<String>,
    /// A pushed commit GitHub has not reported yet, and when it was seen.
    expect: Option<(String, Instant)>,
    state: ChecksState,
    /// When watching the current head began. Drives the interval and the
    /// grace for checks to appear.
    since: Instant,
    /// The next GitHub read; None once settled.
    poll_at: Option<Instant>,
    ref_at: Instant,
    failures: u32,
    in_flight: bool,
}

impl PrWatch {
    /// None when the URL gave no PR number.
    pub fn new(pr: &PublishedPr, now: Instant) -> Option<Self> {
        Some(Self {
            repository: pr.repository.clone(),
            number: pr.number?,
            url: pr.url.clone(),
            head: None,
            expect: Some((pr.head.clone(), now)),
            state: ChecksState::Pending,
            since: now,
            poll_at: Some(now + FIRST_POLL),
            ref_at: now + REF_EVERY,
            failures: 0,
            in_flight: false,
        })
    }

    pub fn mark(&self) -> Mark {
        match self.state {
            ChecksState::NoChecks => Mark::None,
            ChecksState::Pending => Mark::Pending,
            ChecksState::Passed => Mark::Passed,
            ChecksState::Failed => Mark::Failed,
        }
    }

    /// At most one read in flight. A due GitHub read goes first.
    pub fn due(&mut self, now: Instant) -> Option<Due> {
        if self.in_flight {
            return None;
        }
        let due = if self.poll_at.is_some_and(|at| now >= at) {
            Due::Poll
        } else if now >= self.ref_at {
            Due::Ref
        } else {
            return None;
        };
        self.in_flight = true;
        Some(due)
    }

    /// A GitHub read finished. None is any failure: keep what is shown and
    /// try again later, more slowly.
    pub fn polled(&mut self, result: Option<PrChecks>, now: Instant) -> Polled {
        self.in_flight = false;
        let Some(checks) = result else {
            self.failures += 1;
            let backoff = interval(now.duration_since(self.since)) * 2u32.pow(self.failures.min(4));
            self.poll_at = Some(now + backoff.min(MAX_INTERVAL));
            return Polled::default();
        };
        self.failures = 0;
        if !checks.open {
            return Polled {
                changed: true,
                closed: true,
                ..Polled::default()
            };
        }
        if let Some((want, at)) = &self.expect {
            if checks.head != *want && now.duration_since(*at) < EXPECT_GRACE {
                // GitHub still shows the previous head; its result is stale.
                self.poll_at = Some(now + interval(now.duration_since(self.since)));
                return Polled::default();
            }
            if checks.head != *want {
                // Someone pushed another commit elsewhere; follow GitHub.
                self.since = now;
            }
            self.expect = None;
        } else if self.head.as_ref() != Some(&checks.head) {
            self.since = now;
        }
        let elapsed = now.duration_since(self.since);
        let state = match checks.state {
            ChecksState::NoChecks if elapsed < NO_CHECKS_GRACE => ChecksState::Pending,
            state => state,
        };
        let changed = state != self.state || self.head.as_ref() != Some(&checks.head);
        let failed = state == ChecksState::Failed && self.state != ChecksState::Failed;
        self.state = state;
        self.head = Some(checks.head);
        self.poll_at = match state {
            _ if elapsed >= GIVE_UP => None,
            ChecksState::Pending => Some(now + interval(elapsed)),
            ChecksState::Failed => Some(now + MAX_INTERVAL),
            ChecksState::Passed | ChecksState::NoChecks => None,
        };
        Polled {
            changed,
            failed,
            closed: false,
        }
    }

    /// The local ref read finished. A commit the shown state is not about
    /// starts a new round of GitHub reads. Returns whether the mark changed.
    pub fn ref_read(&mut self, head: Option<String>, now: Instant) -> bool {
        self.in_flight = false;
        self.ref_at = now + REF_EVERY;
        let Some(head) = head else {
            return false;
        };
        if self.head.as_ref() == Some(&head)
            || self.expect.as_ref().is_some_and(|(want, _)| *want == head)
        {
            return false;
        }
        self.expect = Some((head, now));
        self.since = now;
        self.failures = 0;
        self.poll_at = Some(now + FIRST_POLL);
        let changed = self.state != ChecksState::Pending;
        self.state = ChecksState::Pending;
        changed
    }
}

/// Every 30 seconds for ten minutes, then every minute, then every five.
fn interval(elapsed: Duration) -> Duration {
    if elapsed < Duration::from_secs(600) {
        Duration::from_secs(30)
    } else if elapsed < Duration::from_secs(3600) {
        Duration::from_secs(60)
    } else {
        MAX_INTERVAL
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn published() -> PublishedPr {
        PublishedPr {
            url: "https://github.com/org/repo/pull/42".into(),
            repository: "github.com/org/repo".into(),
            number: Some(42),
            head: "a".into(),
        }
    }
    fn checks(head: &str, state: ChecksState) -> Option<PrChecks> {
        Some(PrChecks {
            head: head.into(),
            open: true,
            state,
        })
    }
    fn at(t: Instant, n: u64) -> Instant {
        t + Duration::from_secs(n)
    }
    /// Whether a GitHub read is due, answering any local ref read on the
    /// way with "no ref", which never restarts a round.
    fn poll_due(w: &mut PrWatch, now: Instant) -> bool {
        loop {
            match w.due(now) {
                Some(Due::Ref) => {
                    w.ref_read(None, now);
                }
                Some(Due::Poll) => return true,
                None => return false,
            }
        }
    }

    #[test]
    fn no_number_means_no_watch() {
        let pr = PublishedPr {
            number: None,
            ..published()
        };
        assert!(PrWatch::new(&pr, Instant::now()).is_none());
    }

    #[test]
    fn starts_pending_and_waits_before_the_first_read() {
        let t = Instant::now();
        let mut w = PrWatch::new(&published(), t).unwrap();
        assert_eq!(w.mark(), Mark::Pending);
        assert!(!poll_due(&mut w, at(t, 5)));
        assert!(poll_due(&mut w, at(t, 10)));
        // One read at a time.
        assert_eq!(w.due(at(t, 11)), None);
    }

    #[test]
    fn a_pass_settles_and_stops_reading_github() {
        let t = Instant::now();
        let mut w = PrWatch::new(&published(), t).unwrap();
        assert!(poll_due(&mut w, at(t, 10)));
        let p = w.polled(checks("a", ChecksState::Passed), at(t, 10));
        assert!(p.changed && !p.failed);
        assert_eq!(w.mark(), Mark::Passed);
        // Only local ref reads from here, never another poll.
        for n in 11..30_000 {
            if let Some(due) = w.due(at(t, n)) {
                assert_eq!(due, Due::Ref);
                assert!(!w.ref_read(Some("a".into()), at(t, n)));
            }
        }
    }

    #[test]
    fn a_failure_notifies_when_the_mark_turns_red() {
        let t = Instant::now();
        let mut w = PrWatch::new(&published(), t).unwrap();
        assert!(poll_due(&mut w, at(t, 10)));
        assert!(w.polled(checks("a", ChecksState::Failed), at(t, 10)).failed);
        assert_eq!(w.mark(), Mark::Failed);
        // Red is read again slowly; still red posts nothing new.
        assert!(!poll_due(&mut w, at(t, 309)));
        assert!(poll_due(&mut w, at(t, 310)));
        assert!(
            !w.polled(checks("a", ChecksState::Failed), at(t, 310))
                .failed
        );
        // A re-run on GitHub clears it.
        assert!(poll_due(&mut w, at(t, 610)));
        w.polled(checks("a", ChecksState::Passed), at(t, 610));
        assert_eq!(w.mark(), Mark::Passed);
    }

    #[test]
    fn a_push_starts_a_new_round_and_a_new_failure_notifies_again() {
        let t = Instant::now();
        let mut w = PrWatch::new(&published(), t).unwrap();
        assert!(poll_due(&mut w, at(t, 10)));
        w.polled(checks("a", ChecksState::Failed), at(t, 10));
        assert_eq!(w.due(at(t, 20)), Some(Due::Ref));
        assert!(w.ref_read(Some("b".into()), at(t, 20)));
        assert_eq!(w.mark(), Mark::Pending);
        assert!(poll_due(&mut w, at(t, 30)));
        assert!(w.polled(checks("b", ChecksState::Failed), at(t, 30)).failed);
    }

    #[test]
    fn the_previous_head_is_not_reported_for_a_new_push() {
        let t = Instant::now();
        let mut w = PrWatch::new(&published(), t).unwrap();
        assert!(poll_due(&mut w, at(t, 10)));
        // GitHub still shows the old head with its finished checks.
        let p = w.polled(checks("old", ChecksState::Passed), at(t, 10));
        assert_eq!(p, Polled::default());
        assert_eq!(w.mark(), Mark::Pending);
        // After the grace, GitHub's head is trusted.
        assert!(poll_due(&mut w, at(t, 130)));
        w.polled(checks("other", ChecksState::Passed), at(t, 130));
        assert_eq!(w.mark(), Mark::Passed);
    }

    #[test]
    fn an_empty_rollup_is_pending_until_the_grace_ends() {
        let t = Instant::now();
        let mut w = PrWatch::new(&published(), t).unwrap();
        assert!(poll_due(&mut w, at(t, 10)));
        w.polled(checks("a", ChecksState::NoChecks), at(t, 10));
        assert_eq!(w.mark(), Mark::Pending);
        assert!(poll_due(&mut w, at(t, 200)));
        w.polled(checks("a", ChecksState::NoChecks), at(t, 200));
        assert_eq!(w.mark(), Mark::None);
        assert!(!poll_due(&mut w, at(t, 10_000)));
    }

    #[test]
    fn the_interval_slows_and_errors_back_off() {
        let t = Instant::now();
        let mut w = PrWatch::new(&published(), t).unwrap();
        assert!(poll_due(&mut w, at(t, 10)));
        w.polled(checks("a", ChecksState::Pending), at(t, 10));
        assert!(!poll_due(&mut w, at(t, 39)));
        assert!(poll_due(&mut w, at(t, 40)));
        w.polled(None, at(t, 40));
        assert_eq!(w.mark(), Mark::Pending);
        assert!(!poll_due(&mut w, at(t, 99)));
        assert!(poll_due(&mut w, at(t, 100)));
        w.polled(checks("a", ChecksState::Pending), at(t, 700));
        assert!(!poll_due(&mut w, at(t, 759)));
        assert!(poll_due(&mut w, at(t, 760)));
    }

    #[test]
    fn a_run_that_never_finishes_stops_being_read() {
        let t = Instant::now();
        let mut w = PrWatch::new(&published(), t).unwrap();
        assert!(poll_due(&mut w, at(t, 10)));
        w.polled(checks("a", ChecksState::Pending), at(t, 10));
        let late = t + GIVE_UP + Duration::from_secs(60);
        assert!(poll_due(&mut w, late));
        w.polled(checks("a", ChecksState::Pending), late);
        assert_eq!(w.mark(), Mark::Pending);
        assert!(!poll_due(&mut w, late + MAX_INTERVAL * 10));
    }

    #[test]
    fn merged_or_closed_drops_the_watch() {
        let t = Instant::now();
        let mut w = PrWatch::new(&published(), t).unwrap();
        assert!(poll_due(&mut w, at(t, 10)));
        let p = w.polled(
            Some(PrChecks {
                head: "a".into(),
                open: false,
                state: ChecksState::Passed,
            }),
            at(t, 10),
        );
        assert!(p.closed);
    }
}
