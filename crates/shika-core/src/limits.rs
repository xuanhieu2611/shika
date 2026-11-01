//! Subscription quota observations, separate from local token accounting.
//! No credentials, inference, HTTP client, history scan, or idle poller.
mod claude;
mod codex;
mod wire;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::{Core, cli_title::CliHome};
pub(crate) use wire::CodexLimits;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct QuotaWindow {
    pub used_percent: f64,
    pub resets_at: i64,
    pub observed_at: i64,
}

impl QuotaWindow {
    pub fn expired(self, now: i64) -> bool {
        self.resets_at <= now
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct QuotaSnapshot {
    pub five_hour: Option<QuotaWindow>,
    pub weekly: Option<QuotaWindow>,
}

impl QuotaSnapshot {
    pub(crate) fn is_empty(self) -> bool {
        self.five_hour.is_none() && self.weekly.is_none()
    }

    pub(crate) fn merge(&mut self, incoming: Self) {
        for (current, next) in [
            (&mut self.five_hour, incoming.five_hour),
            (&mut self.weekly, incoming.weekly),
        ] {
            if let Some(next) = next
                && current.is_none_or(|old| {
                    next.observed_at > old.observed_at
                        || (next.observed_at == old.observed_at
                            && next.resets_at == old.resets_at
                            && next.used_percent > old.used_percent)
                })
            {
                *current = Some(next);
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuotaStatus {
    Observed,
    WaitingForActivity,
    Unavailable,
    SignInRequired,
    TimedOut,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SubscriptionQuota {
    pub cli: &'static str,
    /// Stable within this app run. Paths and account credentials never reach UI copy.
    pub profile: usize,
    pub snapshot: QuotaSnapshot,
    pub status: QuotaStatus,
}

pub(crate) fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs().min(i64::MAX as u64) as i64)
}

#[derive(Default)]
struct State {
    entries: BTreeMap<(&'static str, PathBuf), Entry>,
    next_profile: usize,
    generation: u64,
}

struct Entry {
    profile: usize,
    /// New launches fence older processes/history from a possibly different login.
    since: i64,
    generation: u64,
    passive: bool,
    snapshot: QuotaSnapshot,
    status: QuotaStatus,
    claude: Option<Arc<claude::Source>>,
    attempt: Option<Instant>,
    refreshing: bool,
}

pub(crate) struct Launch {
    cli: &'static str,
    root: PathBuf,
    source: Option<Arc<claude::Source>>,
}

pub(crate) struct Limits {
    helper: PathBuf,
    state: Mutex<State>,
    changed: Condvar,
    probes: codex::Probes,
}

impl Limits {
    pub(crate) fn new(helper: PathBuf) -> Self {
        Self {
            helper,
            state: Mutex::new(State::default()),
            changed: Condvar::new(),
            probes: codex::Probes::default(),
        }
    }

    fn register(&self, cli: &'static str, root: PathBuf, source: Option<Arc<claude::Source>>) {
        self.insert(cli, root, source, true);
    }

    fn insert(
        &self,
        cli: &'static str,
        root: PathBuf,
        source: Option<Arc<claude::Source>>,
        replace: bool,
    ) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let key = (cli, root);
        if !replace && state.entries.contains_key(&key) {
            return;
        }
        // Bound configuration discovery, not one allocation per task forever.
        if !state.entries.contains_key(&key) && state.entries.len() >= 16 {
            return;
        }
        let (profile, attempt, refreshing) = state
            .entries
            .get(&key)
            .map(|e| (e.profile, e.attempt, e.refreshing))
            .unwrap_or_else(|| {
                state.next_profile += 1;
                (state.next_profile, None, false)
            });
        state.generation = state.generation.saturating_add(1);
        let generation = state.generation;
        let status = if cli == "claude" && source.is_none() {
            QuotaStatus::Unavailable
        } else {
            QuotaStatus::WaitingForActivity
        };
        state.entries.insert(
            key,
            Entry {
                profile,
                since: now().saturating_add(1),
                generation,
                passive: true,
                snapshot: QuotaSnapshot::default(),
                status,
                claude: source,
                // A new launch invalidates data, not the per-root refresh budget.
                attempt,
                refreshing,
            },
        );
        self.changed.notify_all();
    }

    pub(crate) fn prepare(
        &self,
        home: &CliHome,
        cli: &str,
        cwd: &Path,
        args: &mut Vec<String>,
    ) -> Option<Launch> {
        match cli {
            "claude" => {
                let root = profile_root(cwd, &home.claude_root());
                let source = claude::Source::install(&self.helper, &root, cwd, args).map(Arc::new);
                Some(Launch {
                    cli: "claude",
                    root,
                    source,
                })
            }
            "codex" => Some(Launch {
                cli: "codex",
                root: profile_root(cwd, &home.codex_root()),
                source: None,
            }),
            _ => None,
        }
    }

    pub(crate) fn launched(&self, launch: Launch) {
        self.register(launch.cli, launch.root, launch.source);
    }

    pub(crate) fn observe_codex(&self, root: &Path, snapshot: QuotaSnapshot) {
        let root = profile_root(Path::new("/"), root);
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(entry) = state.entries.get_mut(&("codex", root)) {
            if !entry.passive {
                return;
            }
            let filter =
                |window: Option<QuotaWindow>| window.filter(|w| w.observed_at >= entry.since);
            let incoming = QuotaSnapshot {
                five_hour: filter(snapshot.five_hour),
                weekly: filter(snapshot.weekly),
            };
            if !incoming.is_empty() {
                entry.snapshot.merge(incoming);
                entry.status = QuotaStatus::Observed;
            }
        }
    }

    fn snapshot(&self) -> Vec<SubscriptionQuota> {
        // File reads stay outside the state lock and off the UI thread.
        let sources: Vec<_> = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entries
            .iter()
            .filter_map(|(key, e)| Some((key.clone(), e.claude.clone()?)))
            .collect();
        for (key, source) in sources {
            if let Some(snapshot) = source.read() {
                let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(entry) = state.entries.get_mut(&key)
                    && entry
                        .claude
                        .as_ref()
                        .is_some_and(|s| Arc::ptr_eq(s, &source))
                {
                    entry.snapshot.merge(snapshot);
                    entry.status = QuotaStatus::Observed;
                }
            }
        }
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entries
            .iter()
            .map(|((cli, _), e)| SubscriptionQuota {
                cli,
                profile: e.profile,
                snapshot: e.snapshot,
                status: e.status,
            })
            .collect()
    }

    fn refresh_codex(&self, core: &Core) {
        let Some(program) = core.path_env().get("codex") else {
            return;
        };
        let Some(home) = &core.cli_home else { return };
        let default_root = home.codex_root();
        // Relative CODEX_HOME has no meaning without a task cwd. Never guess it.
        if default_root.is_absolute() && default_root.is_dir() {
            let root = profile_root(Path::new("/"), &default_root);
            self.insert("codex", root, None, false);
        }
        let jobs: Vec<_> = {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            state
                .entries
                .iter_mut()
                .filter_map(|((cli, root), entry)| {
                    if *cli != "codex"
                        || entry.refreshing
                        || entry
                            .attempt
                            .is_some_and(|at| at.elapsed() < Duration::from_secs(60))
                    {
                        return None;
                    }
                    entry.attempt = Some(Instant::now());
                    entry.refreshing = true;
                    Some((root.clone(), entry.generation, entry.attempt))
                })
                .take(4)
                .collect()
        };
        let results = std::thread::scope(|scope| {
            let workers: Vec<_> = jobs
                .into_iter()
                .map(|(root, generation, attempt)| {
                    let probe_root = root.clone();
                    let worker = scope.spawn(move || {
                        codex::read(program, core.path_env().path(), &probe_root, &self.probes)
                    });
                    (root, generation, attempt, worker)
                })
                .collect();
            workers
                .into_iter()
                .map(|(root, generation, attempt, worker)| {
                    (
                        root,
                        generation,
                        attempt,
                        worker.join().unwrap_or(Err(QuotaStatus::Unavailable)),
                    )
                })
                .collect::<Vec<_>>()
        });
        for (root, generation, attempt, result) in results {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(entry) = state.entries.get_mut(&("codex", root))
                && entry.attempt == attempt
            {
                entry.refreshing = false;
                if entry.generation != generation {
                    continue;
                }
                match result {
                    Ok(snapshot) => {
                        // A fresh authenticated read is authoritative, including absent windows.
                        entry.snapshot = snapshot;
                        entry.passive = false;
                        entry.status = QuotaStatus::Observed;
                    }
                    Err(status) => {
                        // Do not present old-account data after sign-out/API-key login.
                        if status == QuotaStatus::SignInRequired {
                            entry.snapshot = QuotaSnapshot::default();
                            entry.passive = false;
                        }
                        entry.status = status;
                    }
                }
            }
        }
        self.changed.notify_all();
        // A reopened Settings joins an existing read rather than showing an
        // empty snapshot forever after its original request was fenced out.
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        drop(
            self.changed
                .wait_timeout_while(state, Duration::from_secs(9), |state| {
                    state.entries.values().any(|entry| entry.refreshing)
                }),
        );
    }
}

fn profile_root(cwd: &Path, root: &Path) -> PathBuf {
    let root = cwd.join(root);
    std::fs::canonicalize(&root).unwrap_or(root)
}

impl Core {
    /// App-only opt-in. Core tests and library users never enable provider reads implicitly.
    pub fn start_subscription_limits(&self, helper: PathBuf) {
        self.limits.get_or_init(|| Arc::new(Limits::new(helper)));
    }

    /// Quit cancels the owned metadata probes without waiting on provider I/O.
    pub fn stop_subscription_limits(&self) {
        if let Some(limits) = self.limits.get() {
            limits.probes.stop();
            let mut state = limits.state.lock().unwrap_or_else(|e| e.into_inner());
            for entry in state.entries.values_mut() {
                entry.refreshing = false;
            }
            limits.changed.notify_all();
        }
    }

    /// Blocking, off-thread snapshot. `refresh` permits a bounded, coalesced Codex
    /// metadata probe (at most once per minute/profile), never an inference turn.
    pub fn subscription_limits(&self, refresh: bool) -> Vec<SubscriptionQuota> {
        let Some(limits) = self.limits.get() else {
            return Vec::new();
        };
        if refresh {
            limits.refresh_codex(self);
        }
        limits.snapshot()
    }
}

/// Internal app entry point. Always dispatched before opening Core or GPUI.
/// Only allowlisted quota fields are saved; the original status command gets
/// its unchanged input and stdout. Never writes provider settings.
pub fn run_quota_statusline(args: &[String]) -> i32 {
    claude::run(args)
}

#[cfg(test)]
mod tests;
