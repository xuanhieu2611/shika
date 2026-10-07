//! Arbitrate tiny provider reports without replaying an old turn's state.
use crate::activity::Signal;
use shika_core::{AgentActivity, AgentActivityState};

#[derive(Default)]
pub struct Lifecycle {
    seq: u64,
    floor: u64,
    awaiting_turn: bool,
    signal: Option<Signal>,
}

impl Lifecycle {
    /// Fence both consumed and not-yet-consumed reports from the previous turn.
    pub fn submitted(&mut self, latest: Option<AgentActivity>) {
        self.floor = self.seq.max(latest.map_or(0, |report| report.seq));
        self.awaiting_turn = true;
        self.signal = None;
    }

    pub fn observe(&mut self, report: Option<AgentActivity>) -> Option<Signal> {
        let Some(report) = report else {
            self.signal = None;
            return None;
        };
        if report.seq > self.floor && report.seq >= self.seq {
            // The startup Idle is not a response to the first prompt, even if
            // a slow initial poll delivers it after that prompt was submitted.
            if self.awaiting_turn && report.seq == 1 && report.state == AgentActivityState::Idle {
                return None;
            }
            self.seq = report.seq;
            self.awaiting_turn = false;
            self.signal = Some(match report.state {
                AgentActivityState::Idle => Signal::Idle,
                AgentActivityState::Working => Signal::Working,
                AgentActivityState::Blocked => Signal::Blocked,
            });
        }
        self.signal
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn report(seq: u64, state: AgentActivityState) -> Option<AgentActivity> {
        Some(AgentActivity { seq, state })
    }
    #[test]
    fn consumed_and_unconsumed_previous_turn_reports_are_fenced() {
        let mut cache = Lifecycle::default();
        assert_eq!(
            cache.observe(report(2, AgentActivityState::Working)),
            Some(Signal::Working)
        );
        cache.submitted(report(3, AgentActivityState::Idle));
        assert_eq!(cache.observe(report(3, AgentActivityState::Idle)), None);
        assert_eq!(cache.observe(report(2, AgentActivityState::Working)), None);
        assert_eq!(
            cache.observe(report(4, AgentActivityState::Working)),
            Some(Signal::Working)
        );
        assert_eq!(
            cache.observe(report(5, AgentActivityState::Idle)),
            Some(Signal::Idle)
        );
        cache.submitted(report(5, AgentActivityState::Idle));
        assert_eq!(cache.observe(report(5, AgentActivityState::Idle)), None);
    }
    #[test]
    fn startup_idle_cannot_finish_first_submission_but_fast_completed_turn_can() {
        let mut cache = Lifecycle::default();
        cache.submitted(None);
        assert_eq!(cache.observe(report(1, AgentActivityState::Idle)), None);
        assert_eq!(
            cache.observe(report(3, AgentActivityState::Idle)),
            Some(Signal::Idle)
        );
    }
    #[test]
    fn same_sequence_recovers_after_read_failure_without_replaying_old_turn() {
        let mut cache = Lifecycle::default();
        assert_eq!(
            cache.observe(report(2, AgentActivityState::Working)),
            Some(Signal::Working)
        );
        assert_eq!(cache.observe(None), None);
        assert_eq!(
            cache.observe(report(2, AgentActivityState::Working)),
            Some(Signal::Working)
        );
        cache.submitted(report(2, AgentActivityState::Working));
        assert_eq!(cache.observe(None), None);
        assert_eq!(cache.observe(report(2, AgentActivityState::Working)), None);
    }
}
