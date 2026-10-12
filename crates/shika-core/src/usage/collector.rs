//! One coalescing background worker. Only registered Shika worktrees are read.
//! Counters, receipt IDs, and completed-record offsets commit together in SQLite.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use md5::{Digest, Md5};
use rusqlite::{Connection, OpenFlags, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};

use super::{Date, UsageDay, UsageReport, local_today, recorded_date};
use crate::cli_title::{self, CliHome};

const QUANTUM: u64 = 256 * 1024;
const COALESCE: Duration = Duration::from_millis(250);
const RETRIES: [u64; 5] = [1, 2, 5, 15, 30];
const BUSY: Duration = Duration::from_millis(200);

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "cli", rename_all = "lowercase", deny_unknown_fields)]
enum Target {
    Claude { directories: Vec<PathBuf> },
    Codex { root: PathBuf, cwds: Vec<PathBuf> },
}

impl Target {
    fn new(home: &CliHome, preset: &str, cwd: &Path) -> Option<Self> {
        if !matches!(preset, "claude" | "codex") {
            return None;
        }
        let forms = cli_title::cwd_forms(cwd);
        match preset {
            "claude" => Some(Self::Claude {
                directories: forms.iter().map(|p| home.claude_project_dir(p)).collect(),
            }),
            "codex" => Some(Self::Codex {
                root: cwd.join(home.codex_root()),
                cwds: forms,
            }),
            _ => None,
        }
    }

    fn id(&self) -> String {
        fn path(digest: &mut Md5, path: &Path) {
            let bytes = path.as_os_str().as_encoded_bytes();
            digest.update((bytes.len() as u64).to_le_bytes());
            digest.update(bytes);
        }
        let mut digest = Md5::new();
        match self {
            Self::Claude { directories } => {
                digest.update(b"claude");
                for directory in directories {
                    path(&mut digest, directory);
                }
            }
            Self::Codex { root, cwds } => {
                digest.update(b"codex");
                path(&mut digest, root);
                for cwd in cwds {
                    path(&mut digest, cwd);
                }
            }
        }
        format!("{:x}", digest.finalize())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
enum Priority {
    Live,
    Continuation,
    History,
}

struct Request {
    target: Arc<Target>,
    due: Instant,
    attempt: usize,
    priority: Priority,
}

#[derive(Default)]
struct Requests {
    jobs: BTreeMap<String, Request>,
    live: BTreeSet<(Instant, String)>,
    history: BTreeSet<(Instant, String)>,
    continuations: BTreeSet<(Instant, String)>,
    signals: BTreeMap<String, Arc<Signal>>,
    live_streak: usize,
}

impl Requests {
    fn deadlines(&mut self, priority: Priority) -> &mut BTreeSet<(Instant, String)> {
        match priority {
            Priority::Live => &mut self.live,
            Priority::History => &mut self.history,
            Priority::Continuation => &mut self.continuations,
        }
    }

    fn schedule(&mut self, id: String, mut request: Request) {
        if let Some(previous) = self.jobs.remove(&id) {
            self.deadlines(previous.priority)
                .remove(&(previous.due, id.clone()));
            request.due = request.due.min(previous.due);
            request.attempt = request.attempt.min(previous.attempt);
            request.priority = request.priority.min(previous.priority);
        }
        self.deadlines(request.priority)
            .insert((request.due, id.clone()));
        self.jobs.insert(id, request);
    }

    fn take_due(&mut self, now: Instant) -> Option<Request> {
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
        if let Some(signal) = self.signals.get(&id) {
            // Clear before the read. Output arriving during collection schedules
            // another drain rather than disappearing behind this one.
            signal.notified.store(false, Ordering::Release);
            if Arc::strong_count(signal) == 1 {
                self.signals.remove(&id);
            }
        }
        self.jobs.remove(&id)
    }

    fn delay(&self) -> Option<Duration> {
        self.live
            .first()
            .into_iter()
            .chain(self.history.first())
            .chain(self.continuations.first())
            .map(|(due, _)| due.saturating_duration_since(Instant::now()))
            .min()
    }
}

struct Signal {
    id: String,
    target: Arc<Target>,
    notified: AtomicBool,
}

struct Queue {
    requests: Mutex<Requests>,
    worker: OnceLock<thread::Thread>,
    dirty: AtomicBool,
    recovery: AtomicBool,
    stopped: Arc<AtomicBool>,
    failed: AtomicBool,
    failures: Mutex<BTreeSet<String>>,
}

impl Queue {
    fn new() -> Self {
        Self {
            requests: Mutex::new(Requests::default()),
            worker: OnceLock::new(),
            dirty: AtomicBool::new(false),
            recovery: AtomicBool::new(true),
            stopped: Arc::new(AtomicBool::new(false)),
            failed: AtomicBool::new(false),
            failures: Mutex::new(BTreeSet::new()),
        }
    }

    fn wake(&self) {
        if let Some(worker) = self.worker.get() {
            // Unlike a condvar, unpark keeps one token if it races with park.
            worker.unpark();
        }
    }

    fn schedule(&self, target: Arc<Target>, delay: Duration, attempt: usize, priority: Priority) {
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
                },
            );
        self.wake();
    }

    fn source(self: &Arc<Self>, target: Arc<Target>) -> Source {
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

    fn ingest(&self) {
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
                },
            );
        }
    }

    fn next(&self, data_dir: &Path, recovery: &mut Recovery) -> Option<Request> {
        self.worker.get_or_init(thread::current);
        loop {
            if self.stopped.load(Ordering::Relaxed) {
                return None;
            }
            self.ingest();
            recovery.step(data_dir, self);
            let mut requests = self.requests.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(request) = requests.take_due(Instant::now()) {
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
    signal: Arc<Signal>,
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

fn persist_target(data_dir: &Path, target: &Target) -> std::io::Result<()> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let dir = data_dir.join("usage-sources");
    fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{}.json", target.id()));
    if read_target(&path)
        .ok()
        .is_some_and(|saved| saved.id() == target.id())
    {
        return Ok(());
    }
    let temp = dir.join(format!(
        ".{}-{}.tmp",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)?;
        serde_json::to_writer(&mut file, target)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        fs::rename(&temp, &path)?;
        File::open(&dir)?.sync_all()
    })();
    let _ = fs::remove_file(temp);
    result
}

fn read_target(path: &Path) -> std::io::Result<Target> {
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > 64 * 1024 {
        return Err(std::io::ErrorKind::InvalidData.into());
    }
    serde_json::from_reader(file.take(64 * 1024))
        .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))
}

const RECOVERY_BATCH: usize = 16;
const RECOVERY_WINDOW: usize = 64;

#[derive(Default)]
struct Recovery {
    entries: Option<fs::ReadDir>,
}

impl Recovery {
    fn step(&mut self, data_dir: &Path, queue: &Queue) {
        // Keep one additional pass if Usage opens during enumeration: a
        // registration already visited may have become readable since then.
        if self.entries.is_none() && queue.recovery.swap(false, Ordering::AcqRel) {
            match fs::read_dir(data_dir.join("usage-sources")) {
                Ok(entries) => {
                    queue
                        .failures
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .remove("registry");
                    self.entries = Some(entries);
                }
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => {
                    queue
                        .failures
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .insert("registry".into());
                }
            }
        }
        for _ in 0..RECOVERY_BATCH {
            let historical = {
                let requests = queue.requests.lock().unwrap_or_else(|e| e.into_inner());
                requests.history.len() + requests.continuations.len()
            };
            if queue.stopped.load(Ordering::Relaxed) || historical >= RECOVERY_WINDOW {
                return;
            }
            let Some(entries) = self.entries.as_mut() else {
                return;
            };
            let Some(entry) = entries.next() else {
                self.entries = None;
                return;
            };
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => {
                    queue
                        .failures
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .insert("registry".into());
                    continue;
                }
            };
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "json") {
                let failure = format!("metadata:{}", path.display());
                match read_target(&path) {
                    Ok(target) => {
                        queue
                            .failures
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .remove(&failure);
                        queue.schedule(Arc::new(target), COALESCE, 0, Priority::History);
                    }
                    Err(_) => {
                        queue
                            .failures
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .insert(failure);
                    }
                }
            }
        }
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

fn database(data_dir: &Path) -> PathBuf {
    data_dir.join("usage.sqlite")
}

fn open_store(data_dir: &Path) -> rusqlite::Result<Connection> {
    let path = database(data_dir);
    match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
    {
        Ok(_) => {}
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
            if !fs::symlink_metadata(&path).is_ok_and(|m| m.is_file()) {
                return Err(rusqlite::Error::InvalidQuery);
            }
        }
        Err(_) => return Err(rusqlite::Error::InvalidQuery),
    }
    let mut conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    conn.busy_timeout(BUSY)?;
    let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    if version > 2 {
        return Err(rusqlite::Error::InvalidQuery);
    }
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "FULL")?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS days(day TEXT NOT NULL, cli TEXT NOT NULL, tokens INTEGER NOT NULL CHECK(tokens >= 0), PRIMARY KEY(day, cli));
         CREATE TABLE IF NOT EXISTS sessions(cli TEXT NOT NULL, id TEXT NOT NULL, observed INTEGER NOT NULL DEFAULT 0, baseline INTEGER NOT NULL DEFAULT 0, PRIMARY KEY(cli, id));
         CREATE TABLE IF NOT EXISTS receipts(cli TEXT NOT NULL, id TEXT NOT NULL, session TEXT NOT NULL, a INTEGER NOT NULL, b INTEGER NOT NULL, c INTEGER NOT NULL, d INTEGER NOT NULL, PRIMARY KEY(cli, id));
         CREATE INDEX IF NOT EXISTS receipts_session ON receipts(cli,session);
         CREATE TABLE IF NOT EXISTS files(path TEXT PRIMARY KEY, device INTEGER NOT NULL, inode INTEGER NOT NULL, offset INTEGER NOT NULL, modified TEXT NOT NULL);
         CREATE TABLE IF NOT EXISTS issues(source TEXT PRIMARY KEY);
         CREATE TABLE IF NOT EXISTS metadata(key TEXT PRIMARY KEY, value TEXT NOT NULL);"
    )?;
    let modified: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info('files') WHERE name='modified')",
        [],
        |r| r.get(0),
    )?;
    if !modified {
        conn.execute_batch("ALTER TABLE files ADD COLUMN modified TEXT NOT NULL DEFAULT ''")?;
    }
    import_json(&mut conn, data_dir)?;
    conn.pragma_update(None, "user_version", 2)?;
    Ok(conn)
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Legacy {
    #[serde(default)]
    days: BTreeMap<String, LegacyDay>,
    #[serde(default)]
    sessions: BTreeMap<String, BTreeMap<String, i64>>,
}
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyDay {
    claude: Option<i64>,
    codex: Option<i64>,
}

fn read_legacy(dir: &Path) -> std::io::Result<Legacy> {
    // One-time metadata import, never an unbounded allocation or FIFO read.
    const LIMIT: u64 = 64 * 1024 * 1024;
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(dir.join("usage.json"))?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > LIMIT {
        return Err(std::io::ErrorKind::InvalidData.into());
    }
    serde_json::from_reader(file.take(LIMIT))
        .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))
}

fn import_json(conn: &mut Connection, dir: &Path) -> rusqlite::Result<()> {
    if conn
        .query_row(
            "SELECT value FROM metadata WHERE key = 'imported'",
            [],
            |r| r.get::<_, String>(0),
        )
        .optional()?
        .is_some()
    {
        return Ok(());
    }
    let legacy = match read_legacy(dir) {
        Ok(legacy) => legacy,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Legacy::default(),
        Err(_) => return Err(rusqlite::Error::InvalidQuery),
    };
    let tx = conn.transaction()?;
    for (day, count) in legacy.days {
        if Date::parse(&day).is_none() {
            return Err(rusqlite::Error::InvalidQuery);
        }
        for (cli, total) in [("claude", count.claude), ("codex", count.codex)] {
            if let Some(total) = total {
                if total < 0 {
                    return Err(rusqlite::Error::InvalidQuery);
                }
                tx.execute(
                    "INSERT INTO days VALUES (?1, ?2, ?3)",
                    params![day, cli, total],
                )?;
            }
        }
    }
    for (cli, sessions) in legacy.sessions {
        if !matches!(cli.as_str(), "claude" | "codex") {
            continue;
        }
        for (id, baseline) in sessions {
            if baseline < 0 {
                return Err(rusqlite::Error::InvalidQuery);
            }
            tx.execute(
                "INSERT INTO sessions(cli,id,baseline) VALUES (?1,?2,?3)",
                params![cli, id, baseline],
            )?;
        }
    }
    tx.execute("INSERT INTO metadata VALUES ('imported','1')", [])?;
    tx.commit()
}

#[derive(Default, Debug)]
struct Envelope {
    kind: String,
    timestamp: Option<String>,
    message: Option<Message>,
    payload: Option<Payload>,
}

impl<'de> Deserialize<'de> for Envelope {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = Envelope;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a provider metadata record")
            }
            fn visit_map<M: serde::de::MapAccess<'de>>(
                self,
                mut map: M,
            ) -> Result<Envelope, M::Error> {
                let mut record = Envelope::default();
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "type" => record.kind = map.next_value()?,
                        "timestamp" => record.timestamp = map.next_value()?,
                        "message" if record.kind.is_empty() || record.kind == "assistant" => {
                            record.message = map.next_value()?;
                        }
                        "payload"
                            if record.kind.is_empty()
                                || matches!(
                                    record.kind.as_str(),
                                    "token_usage_record" | "event_msg"
                                ) =>
                        {
                            record.payload = map.next_value()?;
                        }
                        _ => {
                            map.next_value::<serde::de::IgnoredAny>()?;
                        }
                    }
                }
                Ok(record)
            }
        }
        deserializer.deserialize_map(Visitor)
    }
}

#[derive(Default, Deserialize, Debug)]
struct Message {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    usage: Option<ClaudeUsage>,
}
#[derive(Default, Deserialize, Debug)]
struct ClaudeUsage {
    input_tokens: i64,
    output_tokens: i64,
    #[serde(default)]
    cache_read_input_tokens: i64,
    #[serde(default)]
    cache_creation_input_tokens: i64,
}
#[derive(Default, Deserialize, Debug)]
struct Payload {
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    info: Option<TokenInfo>,
    #[serde(default)]
    response_id: Option<String>,
    #[serde(default)]
    thread_id: Option<String>,
    #[serde(default)]
    usage: Option<CodexUsage>,
}
#[derive(Default, Deserialize, Debug)]
struct CodexUsage {
    total_tokens: i64,
}

#[derive(Default, Deserialize, Debug)]
struct TokenInfo {
    total_token_usage: CodexUsage,
}

/// Unknown fields (including conversation content) are skipped by Serde's
/// streaming reader, not deserialized into strings or serde_json::Value.
fn receipt(
    tx: &Transaction<'_>,
    file: &InputFile,
    record: Envelope,
    day: Date,
) -> rusqlite::Result<()> {
    let day = record.timestamp.as_deref().and_then(recorded_date).unwrap_or(day);
    if file.cli == "codex" && record.kind == "event_msg" {
        if let Some(payload) = record.payload
            && payload.kind == "token_count"
            && payload
                .info
                .is_some_and(|info| info.total_token_usage.total_tokens > 0)
        {
            // Older rollouts expose context counters, not deduplicable API
            // receipts. Report that gap instead of quietly displaying zero or
            // counting inherited/compaction context as fresh consumption.
            let seen: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM receipts WHERE cli='codex' AND session=?1)",
                [&file.session],
                |r| r.get(0),
            )?;
            if !seen {
                tx.execute(
                    "INSERT OR IGNORE INTO issues VALUES (?1)",
                    [format!("legacy:{}", file.session)],
                )?;
            }
        }
        return Ok(());
    }
    let (id, session, counts) = match (file.cli, record.kind.as_str()) {
        ("claude", "assistant") => {
            let Some(Message {
                id,
                usage: Some(usage),
            }) = record.message
            else {
                return Ok(());
            };
            let Some(id) = id else {
                return Err(rusqlite::Error::InvalidQuery);
            };
            (
                id,
                file.session.clone(),
                [
                    usage.input_tokens,
                    usage.output_tokens,
                    usage.cache_read_input_tokens,
                    usage.cache_creation_input_tokens,
                ],
            )
        }
        ("codex", "token_usage_record") => {
            let Some(Payload {
                response_id: Some(id),
                thread_id: Some(session),
                usage: Some(usage),
                ..
            }) = record.payload
            else {
                return Err(rusqlite::Error::InvalidQuery);
            };
            // Forked rollouts can contain their parent's old receipts.
            if session != file.session {
                return Ok(());
            }
            (id, session, [usage.total_tokens, 0, 0, 0])
        }
        _ => return Ok(()),
    };
    if file.cli == "codex" {
        tx.execute(
            "DELETE FROM issues WHERE source=?1",
            [format!("legacy:{session}")],
        )?;
    }
    if id.is_empty() || id.len() > 256 || counts.iter().any(|n| *n < 0) {
        return Err(rusqlite::Error::InvalidQuery);
    }
    let previous = tx
        .query_row(
            "SELECT session,a,b,c,d FROM receipts WHERE cli=?1 AND id=?2",
            params![file.cli, id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    [row.get::<_, i64>(1)?, row.get(2)?, row.get(3)?, row.get(4)?],
                ))
            },
        )
        .optional()?;
    let (owner, old) = previous.unwrap_or((session, [0; 4]));
    if let Some(&value) = old.iter().find(|&&n| n < 0) {
        return Err(rusqlite::Error::IntegralValueOutOfRange(1, value));
    }
    let next = std::array::from_fn::<_, 4, _>(|i| old[i].max(counts[i]));
    let increase = next
        .iter()
        .zip(old)
        .try_fold(0i64, |sum, (new, old)| sum.checked_add(new - old))
        .ok_or(rusqlite::Error::InvalidQuery)?;
    if increase == 0 {
        return Ok(());
    }
    tx.execute("INSERT INTO receipts VALUES (?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(cli,id) DO UPDATE SET a=excluded.a,b=excluded.b,c=excluded.c,d=excluded.d", params![file.cli,id,owner,next[0],next[1],next[2],next[3]])?;
    tx.execute(
        "INSERT OR IGNORE INTO sessions(cli,id) VALUES (?1,?2)",
        params![file.cli, owner],
    )?;
    let (observed, baseline): (i64, i64) = tx.query_row(
        "SELECT observed,baseline FROM sessions WHERE cli=?1 AND id=?2",
        params![file.cli, owner],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    if observed < 0 || baseline < 0 {
        return Err(rusqlite::Error::IntegralValueOutOfRange(
            0,
            observed.min(baseline),
        ));
    }
    let next = observed
        .checked_add(increase)
        .ok_or(rusqlite::Error::IntegralValueOutOfRange(0, observed))?;
    let delta = next.max(baseline) - observed.max(baseline);
    tx.execute(
        "UPDATE sessions SET observed=?3 WHERE cli=?1 AND id=?2",
        params![file.cli, owner, next],
    )?;
    if delta > 0 {
        let day = day.iso();
        let old: i64 = tx
            .query_row(
                "SELECT tokens FROM days WHERE day=?1 AND cli=?2",
                params![day, file.cli],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(0);
        if old < 0 {
            return Err(rusqlite::Error::IntegralValueOutOfRange(0, old));
        }
        let total = old
            .checked_add(delta)
            .ok_or(rusqlite::Error::IntegralValueOutOfRange(0, old))?;
        tx.execute("INSERT INTO days VALUES (?1,?2,?3) ON CONFLICT(day,cli) DO UPDATE SET tokens=excluded.tokens", params![day,file.cli,total])?;
    }
    Ok(())
}

struct InputFile {
    path: PathBuf,
    cli: &'static str,
    session: String,
}

fn registered_jsonl(dir: &Path) -> std::io::Result<(Vec<PathBuf>, bool)> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok((Vec::new(), false)),
        Err(err) => return Err(err),
    };
    let mut files = Vec::new();
    let mut pending = false;
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => {
                pending = true;
                continue;
            }
        };
        if entry.path().extension().is_some_and(|ext| ext == "jsonl") {
            if entry.file_type().is_ok_and(|kind| kind.is_file()) {
                files.push(entry.path());
            } else {
                // Do not follow symlinks or let one unsafe/missing entry hide
                // other valid sessions in this registered directory.
                pending = true;
            }
        }
    }
    Ok((files, pending))
}

fn input_files(target: &Target) -> rusqlite::Result<(Vec<InputFile>, bool)> {
    let mut files = BTreeMap::new();
    let mut pending = false;
    match target {
        Target::Claude { directories } => {
            for dir in directories {
                let (paths, incomplete) = match registered_jsonl(dir) {
                    Ok(paths) => paths,
                    Err(_) => {
                        pending = true;
                        continue;
                    }
                };
                pending |= incomplete;
                for path in paths {
                    let Some(name) = path.file_name().and_then(|n| n.to_str()).map(str::to_owned)
                    else {
                        pending = true;
                        continue;
                    };
                    let children = path.with_extension("").join("subagents");
                    let (children, incomplete) =
                        registered_jsonl(&children).unwrap_or((Vec::new(), true));
                    pending |= incomplete;
                    for child in children {
                        files.insert(
                            child.clone(),
                            InputFile {
                                path: child,
                                cli: "claude",
                                session: name.clone(),
                            },
                        );
                    }
                    files.insert(
                        path.clone(),
                        InputFile {
                            path,
                            cli: "claude",
                            session: name,
                        },
                    );
                }
            }
        }
        Target::Codex { root, cwds } => {
            let Some(db) = cli_title::newest_state_db(root) else {
                return Ok((Vec::new(), false));
            };
            let conn = Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
            conn.busy_timeout(BUSY)?;
            let start = Instant::now();
            conn.progress_handler(1000, Some(move || start.elapsed() > BUSY));
            let edges: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='thread_spawn_edges')", [], |r| r.get(0))?;
            let sql = if edges {
                "WITH RECURSIVE owned(id) AS (
                   SELECT id FROM threads WHERE cwd=?1 AND source='cli'
                   UNION SELECT child_thread_id FROM thread_spawn_edges JOIN owned ON parent_thread_id=owned.id
                 ) SELECT threads.id,rollout_path FROM threads JOIN owned ON threads.id=owned.id"
            } else {
                // A matching cwd alone does not establish child ownership.
                // Keep valid CLI-root totals, but surface the discovery gap.
                pending = true;
                "SELECT id,rollout_path FROM threads WHERE cwd=?1 AND source='cli'"
            };
            for cwd in cwds {
                let mut stmt = conn.prepare(sql)?;
                let rows = stmt.query_map([cwd.to_string_lossy().as_ref()], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        PathBuf::from(r.get::<_, String>(1)?),
                    ))
                })?;
                for row in rows {
                    let (session, path) = row?;
                    files.insert(
                        path.clone(),
                        InputFile {
                            path,
                            cli: "codex",
                            session,
                        },
                    );
                }
            }
        }
    }
    Ok((files.into_values().collect(), pending))
}

#[derive(Default)]
struct Outcome {
    more: bool,
    pending: bool,
}

/// A large single JSON value is streamed without allocating its content.
/// Yield between 64KB reads so even a giant tool result cannot monopolize CPU
/// or disk. The quantum is enforced between records, not inside a JSON value.
struct PacedFile {
    file: File,
    bytes: usize,
    remaining: u64,
    end: u64,
    stopped: Arc<AtomicBool>,
}
impl Read for PacedFile {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        if self.stopped.load(Ordering::Relaxed) {
            return Err(std::io::ErrorKind::ConnectionAborted.into());
        }
        if self.bytes >= 64 * 1024 {
            thread::sleep(Duration::from_millis(5));
            self.bytes = 0;
        }
        let limit = self.remaining.min(buffer.len() as u64) as usize;
        let read = self.file.read(&mut buffer[..limit])?;
        self.remaining -= read as u64;
        self.bytes += read;
        Ok(read)
    }
}
impl Seek for PacedFile {
    fn seek(&mut self, from: SeekFrom) -> std::io::Result<u64> {
        let position = self.file.seek(from)?;
        self.remaining = self.end.saturating_sub(position);
        Ok(position)
    }
}

fn modified(metadata: &fs::Metadata) -> String {
    format!(
        "{}:{}:{}:{}",
        metadata.mtime(),
        metadata.mtime_nsec(),
        metadata.ctime(),
        metadata.ctime_nsec()
    )
}

fn resume_offset(previous: &Option<(u64, u64, u64, String)>, metadata: &fs::Metadata) -> u64 {
    previous
        .as_ref()
        .filter(|(dev, inode, offset, stamp)| {
            *dev == metadata.dev()
                && *inode == metadata.ino()
                && *offset <= metadata.len()
                && (*offset < metadata.len() || *stamp == modified(metadata))
        })
        .map_or(0, |row| row.2)
}

fn save_cursor(
    tx: &Transaction<'_>,
    path: &str,
    metadata: &fs::Metadata,
    offset: u64,
) -> rusqlite::Result<()> {
    tx.execute("INSERT INTO files VALUES (?1,?2,?3,?4,?5) ON CONFLICT(path) DO UPDATE SET device=excluded.device,inode=excluded.inode,offset=excluded.offset,modified=excluded.modified", params![path,metadata.dev(),metadata.ino(),offset,modified(metadata)])?;
    Ok(())
}

fn collect(
    conn: &mut Connection,
    target: &Target,
    day: Date,
    stopped: &Arc<AtomicBool>,
) -> rusqlite::Result<Outcome> {
    let (files, discovery_pending) = input_files(target)?;
    let tx = conn.transaction()?;
    let discovery = format!("discovery:{}", target.id());
    if discovery_pending {
        tx.execute("INSERT OR IGNORE INTO issues VALUES (?1)", [&discovery])?;
    } else {
        tx.execute("DELETE FROM issues WHERE source=?1", [&discovery])?;
    }
    let mut outcome = Outcome {
        more: false,
        pending: discovery_pending,
    };
    let mut bytes = 0;
    for file in files {
        if stopped.load(Ordering::Relaxed) {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let path = file.path.to_string_lossy();
        let mut metadata = match fs::symlink_metadata(&file.path) {
            Ok(metadata) if metadata.is_file() => metadata,
            _ => {
                tx.execute("INSERT OR IGNORE INTO issues VALUES (?1)", [path.as_ref()])?;
                outcome.pending = true;
                continue;
            }
        };
        let previous: Option<(u64, u64, u64, String)> = tx
            .query_row(
                "SELECT device,inode,offset,modified FROM files WHERE path=?1",
                [path.as_ref()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?;
        let offset = resume_offset(&previous, &metadata);
        if offset == metadata.len() {
            if previous.as_ref()
                != Some(&(metadata.dev(), metadata.ino(), offset, modified(&metadata)))
            {
                // Persist an observed empty truncation too, even if the next
                // write happens to restore exactly the previous file length.
                save_cursor(&tx, &path, &metadata, offset)?;
            }
            tx.execute("DELETE FROM issues WHERE source=?1", [path.as_ref()])?;
            continue;
        }
        if bytes >= QUANTUM {
            outcome.more = true;
            break;
        }
        let mut handle = match fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&file.path)
        {
            Ok(file) => file,
            Err(_) => {
                tx.execute("INSERT OR IGNORE INTO issues VALUES (?1)", [path.as_ref()])?;
                outcome.pending = true;
                continue;
            }
        };
        // Use the opened inode, not a path stat that may have raced a rename.
        metadata = handle
            .metadata()
            .map_err(|_| rusqlite::Error::InvalidQuery)?;
        if !metadata.is_file() {
            tx.execute("INSERT OR IGNORE INTO issues VALUES (?1)", [path.as_ref()])?;
            outcome.pending = true;
            continue;
        }
        let offset = resume_offset(&previous, &metadata);
        handle
            .seek(SeekFrom::Start(offset))
            .map_err(|_| rusqlite::Error::InvalidQuery)?;
        let mut reader = BufReader::new(PacedFile {
            file: handle,
            bytes: 0,
            remaining: metadata.len().saturating_sub(offset),
            end: metadata.len(),
            stopped: stopped.clone(),
        });
        let mut stream = serde_json::Deserializer::from_reader(&mut reader).into_iter::<Envelope>();
        let mut consumed = 0u64;
        let mut malformed = false;
        let mut pending = false;
        while let Some(record) = stream.next() {
            match record {
                Ok(record) => {
                    tx.execute_batch("SAVEPOINT receipt")?;
                    match receipt(&tx, &file, record, day) {
                        Ok(()) => tx.execute_batch("RELEASE receipt")?,
                        Err(rusqlite::Error::InvalidQuery) => {
                            tx.execute_batch("ROLLBACK TO receipt; RELEASE receipt")?;
                            tx.execute(
                                "INSERT OR IGNORE INTO issues VALUES (?1)",
                                [format!("corrupt:{path}")],
                            )?;
                        }
                        Err(err) => return Err(err),
                    }
                    consumed = stream.byte_offset() as u64;
                    if bytes + consumed >= QUANTUM {
                        outcome.more = true;
                        break;
                    }
                }
                Err(err) if err.is_eof() => {
                    pending = true;
                    break;
                }
                Err(err) if err.is_io() => return Err(rusqlite::Error::InvalidQuery),
                Err(_) => {
                    malformed = true;
                    break;
                }
            }
        }
        if !malformed && !pending {
            consumed = stream.byte_offset() as u64;
        }
        drop(stream);
        if malformed {
            // Skip only the broken JSONL record, retaining an explicit issue.
            // Later valid records must not be stranded behind it forever.
            reader
                .seek(SeekFrom::Start(offset + consumed))
                .map_err(|_| rusqlite::Error::InvalidQuery)?;
            let before = consumed;
            let mut complete = false;
            loop {
                let buffer = reader
                    .fill_buf()
                    .map_err(|_| rusqlite::Error::InvalidQuery)?;
                if buffer.is_empty() {
                    break;
                }
                let (n, done) = match buffer.iter().position(|b| *b == b'\n') {
                    Some(i) => (i + 1, true),
                    None => (buffer.len(), false),
                };
                reader.consume(n);
                consumed += n as u64;
                if done {
                    complete = true;
                    break;
                }
            }
            if complete {
                tx.execute(
                    "INSERT OR IGNORE INTO issues VALUES (?1)",
                    [format!("corrupt:{path}")],
                )?;
                outcome.more = true;
            } else {
                // A torn final record may still be completed by the provider.
                consumed = before;
                pending = true;
            }
        }
        outcome.pending |= pending;
        if pending {
            tx.execute("INSERT OR IGNORE INTO issues VALUES (?1)", [path.as_ref()])?;
        } else {
            tx.execute("DELETE FROM issues WHERE source=?1", [path.as_ref()])?;
        }
        save_cursor(&tx, &path, &metadata, offset + consumed)?;
        bytes += consumed;
    }
    tx.commit()?;
    Ok(outcome)
}

pub(super) fn report(dir: &Path, today: Option<Date>) -> UsageReport {
    let mut report = UsageReport {
        unreadable: false,
        pending: false,
        empty: true,
        claude: 0,
        codex: 0,
        days: Vec::new(),
    };
    let result = (|| -> rusqlite::Result<()> {
        if !database(dir).exists() {
            // Compatibility before the worker's first open. Never writes here.
            let legacy = match read_legacy(dir) {
                Ok(legacy) => legacy,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Legacy::default(),
                Err(_) => return Err(rusqlite::Error::InvalidQuery),
            };
            for (date, day) in legacy.days {
                for (cli, total) in [("claude", day.claude), ("codex", day.codex)] {
                    if let Some(total) = total {
                        add_day(&mut report, today, &date, cli, total)?;
                    }
                }
            }
            return Ok(());
        }
        let conn = Connection::open_with_flags(
            database(dir),
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        conn.busy_timeout(BUSY)?;
        let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version > 2 {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let imported: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM metadata WHERE key='imported')",
            [],
            |r| r.get(0),
        )?;
        if !imported {
            return Err(rusqlite::Error::InvalidQuery);
        }
        report.empty = !conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM days WHERE tokens>0)",
            [],
            |r| r.get::<_, bool>(0),
        )?;
        report.pending = conn.query_row("SELECT EXISTS(SELECT 1 FROM issues)", [], |r| r.get(0))?;
        if let Some(today) = today {
            // Indexed range, not a load of every historic receipt or file.
            let mut stmt = conn.prepare(
                "SELECT day,cli,tokens FROM days WHERE day>=date(?1,?2) AND day<date(?1,?3) ORDER BY day DESC",
            )?;
            let low = format!("-{} days", today.weekday());
            let high = format!("+{} days", 7 - today.weekday());
            let rows = stmt.query_map(params![today.iso(), low, high], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            })?;
            for row in rows {
                let (date, cli, total) = row?;
                add_day(&mut report, Some(today), &date, &cli, total)?;
            }
        }
        Ok(())
    })();
    if result.is_err() {
        report.unreadable = true;
    }
    report
        .days
        .sort_by_key(|day| std::cmp::Reverse(date_label_order(&day.label)));
    report
}

#[cfg(test)]
#[path = "collector_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "receipt_date_tests.rs"]
mod receipt_date_tests;

// Days are inserted in SQL date order; this function is only for the legacy
// BTreeMap path, which is ascending. Keep actual dates alongside labels below.
fn date_label_order(label: &str) -> usize {
    let names = [
        "Monday",
        "Tuesday",
        "Wednesday",
        "Thursday",
        "Friday",
        "Saturday",
        "Sunday",
    ];
    names
        .iter()
        .position(|name| label.starts_with(name))
        .unwrap_or(0)
}

fn add_day(
    report: &mut UsageReport,
    today: Option<Date>,
    date: &str,
    cli: &str,
    total: i64,
) -> rusqlite::Result<()> {
    let date = Date::parse(date).ok_or(rusqlite::Error::InvalidQuery)?;
    let total = u64::try_from(total).map_err(|_| rusqlite::Error::InvalidQuery)?;
    if total == 0 {
        return Ok(());
    }
    report.empty = false;
    if !today.is_some_and(|today| today.same_week(date)) {
        return Ok(());
    }
    let label = date.label();
    let index = report
        .days
        .iter()
        .position(|d| d.label == label)
        .unwrap_or_else(|| {
            report.days.push(UsageDay {
                label,
                claude: 0,
                codex: 0,
            });
            report.days.len() - 1
        });
    match cli {
        "claude" => {
            report.claude = report.claude.saturating_add(total);
            report.days[index].claude = total;
        }
        "codex" => {
            report.codex = report.codex.saturating_add(total);
            report.days[index].codex = total;
        }
        _ => return Err(rusqlite::Error::InvalidQuery),
    }
    Ok(())
}
