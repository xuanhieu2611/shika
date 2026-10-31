//! What the Lead is doing: the header's worker summary and the running
//! turn's activity steps, memory only.
use crate::Status;
use shika_core::control::TaskStatus;

/// Counts of the workers this Lead started, by status. The header shows them
/// as one line, most urgent first. A worker whose CLI exited counts as
/// waiting.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WorkerSummary {
    pub asking: usize,
    pub ready: usize,
    pub working: usize,
    pub waiting: usize,
}

impl WorkerSummary {
    /// Counts workers by status. A worker still starting counts as working
    /// and one whose CLI exited as waiting.
    pub fn count(statuses: impl IntoIterator<Item = TaskStatus>) -> Self {
        let mut summary = Self::default();
        for status in statuses {
            match status {
                TaskStatus::Asking => summary.asking += 1,
                TaskStatus::Ready => summary.ready += 1,
                TaskStatus::Starting | TaskStatus::Working => summary.working += 1,
                TaskStatus::Waiting | TaskStatus::Exited => summary.waiting += 1,
            }
        }
        summary
    }
    /// "1 asking · 2 ready", in attention order with zero counts left out, or
    /// "No workers".
    pub fn label(&self) -> String {
        let parts = [
            (self.asking, "asking"),
            (self.ready, "ready"),
            (self.working, "working"),
            (self.waiting, "waiting"),
        ]
        .iter()
        .filter(|(count, _)| *count > 0)
        .map(|(count, word)| format!("{count} {word}"))
        .collect::<Vec<_>>();
        if parts.is_empty() {
            "No workers".into()
        } else {
            parts.join(" \u{b7} ")
        }
    }
    /// The status of the dot: the most urgent one present, none with no
    /// workers.
    pub fn dot(&self) -> Option<Status> {
        if self.asking > 0 {
            Some(Status::Asking)
        } else if self.ready > 0 {
            Some(Status::Ready)
        } else if self.working > 0 {
            Some(Status::Working)
        } else if self.waiting > 0 {
            Some(Status::Waiting)
        } else {
            None
        }
    }
}

/// The steps shown under the newest bubble while a turn runs: up to three
/// finished ones, then the current one. Memory only, per turn.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ActivitySteps {
    finished: Vec<String>,
    current: Option<String>,
}

impl ActivitySteps {
    /// Replaces the steps. Only the newest three finished ones are kept.
    pub fn set(&mut self, finished: Vec<String>, current: Option<String>) {
        let skip = finished.len().saturating_sub(3);
        self.finished = finished.into_iter().skip(skip).collect();
        self.current = current;
    }
    pub fn clear(&mut self) {
        self.finished.clear();
        self.current = None;
    }
    /// The lines to show, oldest first: finished steps with a leading
    /// check mark (true), then the current line (false). `fallback` is the
    /// current line when no step is running.
    pub(super) fn lines<'a>(&'a self, fallback: &'a str) -> Vec<(bool, String)> {
        let mut lines = self
            .finished
            .iter()
            .map(|step| (true, format!("\u{2713} {step}")))
            .collect::<Vec<_>>();
        lines.push((
            false,
            self.current.as_deref().unwrap_or(fallback).to_string(),
        ));
        lines
    }
}

#[cfg(test)]
mod summary_tests {
    use super::*;

    fn summary(asking: usize, ready: usize, working: usize, waiting: usize) -> WorkerSummary {
        WorkerSummary {
            asking,
            ready,
            working,
            waiting,
        }
    }

    #[test]
    fn summary_label_follows_attention_order_and_skips_zeros() {
        assert_eq!(summary(0, 0, 0, 0).label(), "No workers");
        assert_eq!(summary(1, 2, 0, 0).label(), "1 asking \u{b7} 2 ready");
        assert_eq!(summary(0, 0, 2, 0).label(), "2 working");
        assert_eq!(summary(0, 0, 0, 3).label(), "3 waiting");
        assert_eq!(
            summary(1, 1, 1, 1).label(),
            "1 asking \u{b7} 1 ready \u{b7} 1 working \u{b7} 1 waiting"
        );
    }

    #[test]
    fn summary_dot_is_the_most_urgent_status() {
        assert_eq!(summary(0, 0, 0, 0).dot(), None);
        assert_eq!(summary(1, 2, 3, 4).dot(), Some(Status::Asking));
        assert_eq!(summary(0, 2, 3, 4).dot(), Some(Status::Ready));
        assert_eq!(summary(0, 0, 3, 4).dot(), Some(Status::Working));
        assert_eq!(summary(0, 0, 0, 4).dot(), Some(Status::Waiting));
    }

    #[test]
    fn steps_keep_the_newest_three_finished_with_check_marks() {
        let mut steps = ActivitySteps::default();
        assert_eq!(
            steps.lines("Working..."),
            vec![(false, "Working...".into())]
        );
        steps.set(
            ["a", "b", "c", "d"].map(String::from).to_vec(),
            Some("Reading x...".into()),
        );
        assert_eq!(
            steps.lines("Working..."),
            vec![
                (true, "\u{2713} b".to_string()),
                (true, "\u{2713} c".to_string()),
                (true, "\u{2713} d".to_string()),
                (false, "Reading x...".to_string()),
            ]
        );
        steps.clear();
        assert_eq!(
            steps.lines("Stopping..."),
            vec![(false, "Stopping...".into())]
        );
    }

    #[test]
    fn workers_are_counted_by_status_with_exited_as_waiting() {
        use TaskStatus::*;
        assert_eq!(
            WorkerSummary::count([Asking, Ready, Ready, Starting, Working, Waiting, Exited]),
            summary(1, 2, 2, 2)
        );
        assert_eq!(WorkerSummary::count([]), WorkerSummary::default());
    }
}
