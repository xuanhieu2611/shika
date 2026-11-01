//! One coalescing background worker. Only registered Shika worktrees are read.
//! Counters, receipt IDs, and completed-record offsets commit together in SQLite.

mod collect;
mod discovery;
mod queue;
mod receipt;
mod registry;
mod store;
mod stream;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::thread;
use std::time::Duration;

use super::local_today;
use crate::cli_title::CliHome;
use collect::collect;
use queue::{Priority, Queue, Request};
use registry::{Recovery, Target, persist_target};
use store::open_store;

pub(crate) use queue::Source;
pub(super) use store::report;

const QUANTUM: u64 = 256 * 1024;
const COALESCE: Duration = Duration::from_millis(250);
const RETRIES: [u64; 5] = [1, 2, 5, 15, 30];
const BUSY: Duration = Duration::from_millis(200);

pub(crate) struct Collector {
    data_dir: PathBuf,
    queue: Arc<Queue>,
}

impl Collector {
    pub(crate) fn start(data_dir: PathBuf) -> Self {
        let queue = Arc::new(Queue::new());
        let worker_queue = queue.clone();
        let worker_dir = data_dir.clone();
        if thread::Builder::new()
            .name("shika-usage".into())
            .spawn(move || {
                run(&worker_dir, &worker_queue);
            })
            .is_err()
        {
            queue.failed.store(true, Ordering::Relaxed);
        }
        Self { data_dir, queue }
    }

    /// Called before spawn, on the launch thread. Journal only path metadata
    /// so even a crash before the first Ready/exit can recover persisted usage.
    pub(crate) fn register(&self, home: &CliHome, preset: &str, cwd: &Path) -> Option<Source> {
        let target = Arc::new(Target::new(home, preset, cwd)?);
        if persist_target(&self.data_dir, &target).is_err() {
            self.queue
                .failures
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(target.id());
        }
        let source = self.queue.source(target);
        source.notify();
        Some(source)
    }

    pub(crate) fn request(&self, home: &CliHome, preset: &str, cwd: &Path) {
        if let Some(target) = Target::new(home, preset, cwd) {
            self.queue
                .schedule(Arc::new(target), COALESCE, 0, Priority::Live);
        }
    }

    /// Explicit recovery request; never scans the user's global CLI history.
    pub(crate) fn recover(&self) {
        if !self.queue.recovery.swap(true, Ordering::AcqRel) {
            self.queue.wake();
        }
    }

    /// Metadata-only diagnostic. Quiescence is independent of collection gaps.
    pub(crate) fn idle(&self) -> bool {
        let requests = self
            .queue
            .requests
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        requests.jobs.is_empty()
            && !self.queue.active.load(Ordering::Acquire)
            && !self.queue.recovery.load(Ordering::Acquire)
            && !self.queue.enumerating.load(Ordering::Acquire)
            && !self.queue.dirty.load(Ordering::Acquire)
            && !requests
                .signals
                .values()
                .any(|signal| signal.notified.load(Ordering::Acquire))
    }

    pub(crate) fn pending(&self) -> bool {
        self.queue.failed.load(Ordering::Relaxed)
            || !self
                .queue
                .failures
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_empty()
    }
}

impl Drop for Collector {
    fn drop(&mut self) {
        self.queue.stopped.store(true, Ordering::Release);
        self.queue.wake();
        // Do not join: quit and terminal lifecycle never wait on this worker.
        // Source metadata and committed offsets survive for next launch.
    }
}

fn run(data_dir: &Path, queue: &Queue) {
    background_priority();
    let mut recovery = Recovery::default();
    let mut connection = None;
    while let Some(request) = queue.next(data_dir, &mut recovery) {
        // Retry a failed registration too. Disk/permission recovery must not
        // require a new task, and available counters can still be saved.
        let registered = persist_target(data_dir, &request.target).is_ok();
        let result = (|| {
            if connection.is_none() {
                connection = Some(open_store(data_dir)?);
            }
            let Some(today) = local_today() else {
                return Err(rusqlite::Error::InvalidQuery);
            };
            collect(
                connection.as_mut().unwrap(),
                &request.target,
                today,
                &queue.stopped,
            )
        })();
        match result {
            Ok(outcome) => {
                let mut failures = queue.failures.lock().unwrap_or_else(|e| e.into_inner());
                if registered {
                    failures.remove(&request.target.id());
                    failures.remove(&format!(
                        "metadata:{}",
                        data_dir
                            .join("usage-sources")
                            .join(format!("{}.json", request.target.id()))
                            .display()
                    ));
                } else {
                    failures.insert(request.target.id());
                }
                drop(failures);
                if outcome.more {
                    // Keep a partially drained historical stream hot. Rotating
                    // through old sources would evict compressed-reader state
                    // and repeatedly decompress their already-read prefixes.
                    let priority = if request.priority == Priority::Live {
                        Priority::Live
                    } else {
                        Priority::Continuation
                    };
                    queue.schedule(request.target, COALESCE, request.attempt, priority);
                } else if outcome.pending || !registered || request.attempt < 2 {
                    let mut request = request;
                    if request.priority == Priority::Continuation {
                        request.priority = Priority::History;
                    }
                    // Re-read after success as well: a provider may flush its
                    // final receipt after painting Ready or emitting PTY EOF.
                    retry(queue, request);
                }
            }
            Err(_) => {
                queue
                    .failures
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(request.target.id());
                connection = None;
                retry(queue, request);
            }
        }
        // Global pacing, not just per source. A large startup backlog must
        // not turn into continuous CPU/disk activity across many worktrees.
        thread::sleep(COALESCE);
        queue.active.store(false, Ordering::Release);
    }
}

fn background_priority() {
    #[cfg(target_os = "macos")]
    {
        unsafe extern "C" {
            fn pthread_set_qos_class_self_np(class: u32, relative_priority: i32) -> i32;
        }
        // Public pthread API and QOS_CLASS_BACKGROUND from the macOS SDK's
        // sys/qos.h. This is only a scheduler hint on this worker thread.
        let _ = unsafe { pthread_set_qos_class_self_np(0x09, 0) };
    }
}

fn retry(queue: &Queue, request: Request) {
    if let Some(secs) = RETRIES.get(request.attempt) {
        queue.schedule(
            request.target,
            Duration::from_secs(*secs),
            request.attempt + 1,
            request.priority,
        );
    }
    // After a bounded burst, stop. The durable source is retried on the next
    // PTY event, Usage open, or app launch, without an idle history poller.
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod receipt_date_tests;
