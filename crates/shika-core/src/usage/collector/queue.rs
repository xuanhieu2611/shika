//! The worker's schedule: live, continuation, and history deadlines, the PTY
//! signals that wake it, and the source handle an agent's PTY sink holds.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use super::COALESCE;
use super::registry::{RECOVERY_WINDOW, Recovery, Target};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub(super) enum Priority {
    Live,
    Continuation,
    History,
}

pub(super) struct Request {
    pub(super) target: Arc<Target>,
    pub(super) due: Instant,
    pub(super) attempt: usize,
    pub(super) priority: Priority,
    pub(super) consumed_notification: bool,
}

#[derive(Default)]
pub(super) struct Requests {
    pub(super) jobs: BTreeMap<String, Request>,
    pub(super) live: BTreeSet<(Instant, String)>,
    pub(super) history: BTreeSet<(Instant, String)>,
    pub(super) continuations: BTreeSet<(Instant, String)>,
    pub(super) signals: BTreeMap<String, Arc<Signal>>,
    pub(super) live_streak: usize,
}

impl Requests {
    pub(super) fn deadlines(&mut self, priority: Priority) -> &mut BTreeSet<(Instant, String)> {
        match priority {
            Priority::Live => &mut self.live,
            Priority::History => &mut self.history,
            Priority::Continuation => &mut self.continuations,
        }
    }

    pub(super) fn schedule(&mut self, id: String, mut request: Request) {
        if let Some(previous) = self.jobs.remove(&id) {
            self.deadlines(previous.priority)
                .remove(&(previous.due, id.clone()));
            request.due = request.due.min(previous.due);
            request.attempt = request.attempt.min(previous.attempt);
            request.priority = request.priority.min(previous.priority);
            request.consumed_notification |= previous.consumed_notification;
        }
        self.deadlines(request.priority)
            .insert((request.due, id.clone()));
        self.jobs.insert(id, request);
    }

    pub(super) fn take_due(&mut self, now: Instant) -> Option<Request> {
        let live = self.live.first().filter(|(due, _)| *due <= now);
        let historical = if self
            .continuations
            .first()
            .is_some_and(|(due, _)| *due <= now)
        {
            Some(Priority::Continuation)
        } else if self.history.first().is_some_and(|(due, _)| *due <= now) {
            Some(Priority::History)
        } else {
            None
        };
        let priority = match (live, historical) {
            (Some(_), Some(priority)) if self.live_streak >= 3 => priority,
            (Some(_), _) => Priority::Live,
            (_, Some(priority)) => priority,
            _ => return None,
        };
        let (_, id) = self.deadlines(priority).pop_first().unwrap();
        self.live_streak = if priority == Priority::Live {
            (self.live_streak + 1).min(3)
        } else {
            0
        };
        let request = self.jobs.remove(&id)?;
        if let Some(signal) = self.signals.get(&id) {
            // Only clear a notification this job actually ingested. A retry
            // can become due between ingest and notify; clearing its new flag
            // would lose the fresh event and its renewed settling/retry burst.
            if request.consumed_notification {
                signal.notified.store(false, Ordering::Release);
            }
            if Arc::strong_count(signal) == 1 && !signal.notified.load(Ordering::Acquire) {
                self.signals.remove(&id);
            }
        }
        Some(request)
    }

    pub(super) fn delay(&self) -> Option<Duration> {
        self.live
            .first()
            .into_iter()
            .chain(self.history.first())
            .chain(self.continuations.first())
            .map(|(due, _)| due.saturating_duration_since(Instant::now()))
            .min()
    }
}

pub(super) struct Signal {
    pub(super) id: String,
    pub(super) target: Arc<Target>,
    pub(super) notified: AtomicBool,
}

pub(super) struct Queue {
    pub(super) requests: Mutex<Requests>,
    pub(super) worker: OnceLock<thread::Thread>,
    pub(super) dirty: AtomicBool,
    pub(super) recovery: AtomicBool,
    pub(super) enumerating: AtomicBool,
    pub(super) active: AtomicBool,
    pub(super) stopped: Arc<AtomicBool>,
    pub(super) failed: AtomicBool,
    pub(super) failures: Mutex<BTreeSet<String>>,
}

impl Queue {
    pub(super) fn new() -> Self {
        Self {
            requests: Mutex::new(Requests::default()),
            worker: OnceLock::new(),
            dirty: AtomicBool::new(false),
            recovery: AtomicBool::new(true),
            enumerating: AtomicBool::new(false),
            active: AtomicBool::new(false),
            stopped: Arc::new(AtomicBool::new(false)),
            failed: AtomicBool::new(false),
            failures: Mutex::new(BTreeSet::new()),
        }
    }

    pub(super) fn wake(&self) {
        if let Some(worker) = self.worker.get() {
            // Unlike a condvar, unpark keeps one token if it races with park.
            worker.unpark();
        }
    }

    pub(super) fn schedule(
        &self,
        target: Arc<Target>,
        delay: Duration,
        attempt: usize,
        priority: Priority,
    ) {
        if self.stopped.load(Ordering::Relaxed) {
            return;
        }
        self.requests
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .schedule(
                target.id(),
                Request {
                    target,
                    due: Instant::now() + delay,
                    attempt,
                    priority,
                    consumed_notification: false,
                },
            );
        self.wake();
    }

    pub(super) fn source(self: &Arc<Self>, target: Arc<Target>) -> Source {
        let id = target.id();
        let signal = self
            .requests
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .signals
            .entry(id.clone())
            .or_insert_with(|| {
                Arc::new(Signal {
                    id,
                    target,
                    notified: AtomicBool::new(false),
                })
            })
            .clone();
        Source {
            signal,
            queue: self.clone(),
        }
    }

    pub(super) fn ingest(&self) {
        if !self.dirty.swap(false, Ordering::AcqRel) {
            return;
        }
        let mut requests = self.requests.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        // Only the worker visits live handles. The PTY never touches this lock,
        // the deadline indexes, or the historical registry.
        let signals: Vec<_> = requests
            .signals
            .values()
            .filter(|signal| signal.notified.load(Ordering::Acquire))
            .cloned()
            .collect();
        requests.signals.retain(|_, signal| {
            Arc::strong_count(signal) > 1 || signal.notified.load(Ordering::Acquire)
        });
        for signal in signals {
            requests.schedule(
                signal.id.clone(),
                Request {
                    target: signal.target.clone(),
                    due: now + COALESCE,
                    attempt: 0,
                    priority: Priority::Live,
                    consumed_notification: true,
                },
            );
        }
    }

    pub(super) fn next(&self, data_dir: &Path, recovery: &mut Recovery) -> Option<Request> {
        self.worker.get_or_init(thread::current);
        loop {
            if self.stopped.load(Ordering::Relaxed) {
                return None;
            }
            self.ingest();
            recovery.step(data_dir, self);
            let mut requests = self.requests.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(request) = requests.take_due(Instant::now()) {
                // Publish execution under the same lock that removed the job.
                // Diagnostics cannot mistake a popped job for quiescence.
                self.active.store(true, Ordering::Release);
                return Some(request);
            }
            let delay = requests.delay();
            let advance = (recovery.entries.is_some() || self.recovery.load(Ordering::Acquire))
                && requests.history.len() + requests.continuations.len() < RECOVERY_WINDOW;
            drop(requests);
            if advance {
                // Enumeration without a collection job still yields between
                // bounded batches. Notification can interrupt this yield.
                thread::park_timeout(COALESCE);
            } else if let Some(delay) = delay {
                thread::park_timeout(delay);
            } else {
                thread::park();
            }
        }
    }
}

/// A source handle on an agent's existing PTY sink. No file I/O or queue lock.
pub(crate) struct Source {
    pub(super) signal: Arc<Signal>,
    queue: Arc<Queue>,
}

impl Source {
    pub(crate) fn notify(&self) {
        if !self.queue.stopped.load(Ordering::Relaxed)
            && !self.signal.notified.swap(true, Ordering::AcqRel)
        {
            self.queue.dirty.store(true, Ordering::Release);
            self.queue.wake();
        }
    }
}
