//! Local token totals for Claude Code and Codex sessions Shika launched.
//!
//! A Ready card and a CLI exit each read once. The read is the newest
//! session's cumulative total, and only the increase since the last read of
//! that session is added to the local day. Cursor and Pi are not recorded.
//! Nothing here is a price, a bill, or a quality score.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::cli_title::{self, CliHome};

/// Last 64KB of a Claude session file. The cost-state record is a summary
/// line, so the tail is enough and the rest of the transcript stays on disk.
const CLAUDE_TAIL: u64 = 64 * 1024;

/// How long a Codex read waits when `state_*.sqlite` is busy, then gives up.
const CODEX_BUSY: Duration = Duration::from_millis(200);

const TOKEN_FIELDS: [&str; 5] = [
    "inputTokens",
    "outputTokens",
    "thinkingTokens",
    "cacheReadInputTokens",
    "cacheCreationInputTokens",
];

/// What View > Usage shows. `empty` means no day in the file has tokens.
/// `claude` and `codex` are this local week only (Monday through Sunday).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UsageReport {
    pub empty: bool,
    pub claude: u64,
    pub codex: u64,
    /// Days in this week that have tokens, newest first.
    pub days: Vec<UsageDay>,
}

/// One saved day. A CLI with no tokens that day is zero here; the file omits
/// the key rather than storing a zero.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UsageDay {
    pub label: String,
    pub claude: u64,
    pub codex: u64,
}

/// Grouped decimal form, such as `12,345`.
pub fn format_tokens(n: u64) -> String {
    let raw = n.to_string();
    let mut out = String::with_capacity(raw.len() + raw.len() / 3);
    for (index, ch) in raw.chars().enumerate() {
        if index > 0 && (raw.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Date {
    year: i32,
    month: u8,
    day: u8,
}

impl Date {
    fn from_parts(year: i32, month: u32, day: u32) -> Option<Self> {
        if !(1..=12).contains(&month) {
            return None;
        }
        let month = month as u8;
        let day = u8::try_from(day).ok()?;
        if day == 0 || u32::from(day) > days_in_month(year, month) {
            return None;
        }
        Some(Self { year, month, day })
    }

    fn parse(text: &str) -> Option<Self> {
        let (year, rest) = text.split_once('-')?;
        let (month, day) = rest.split_once('-')?;
        if year.len() != 4 || month.len() != 2 || day.len() != 2 {
            return None;
        }
        Self::from_parts(year.parse().ok()?, month.parse().ok()?, day.parse().ok()?)
    }

    fn iso(self) -> String {
        format!("{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }

    /// Monday is 0. 1970-01-01 was a Thursday.
    fn weekday(self) -> usize {
        (self.epoch_days() + 3).rem_euclid(7) as usize
    }

    fn week_start(self) -> i64 {
        self.epoch_days() - self.weekday() as i64
    }

    fn same_week(self, other: Self) -> bool {
        self.week_start() == other.week_start()
    }

    fn label(self) -> String {
        const WEEKDAYS: [&str; 7] = [
            "Monday",
            "Tuesday",
            "Wednesday",
            "Thursday",
            "Friday",
            "Saturday",
            "Sunday",
        ];
        const MONTHS: [&str; 12] = [
            "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
        ];
        format!(
            "{}, {} {}",
            WEEKDAYS[self.weekday()],
            MONTHS[usize::from(self.month) - 1],
            self.day
        )
    }

    /// Days since 1970-01-01. Howard Hinnant's civil-from-days algorithm.
    fn epoch_days(self) -> i64 {
        let mut year = self.year;
        let month = u32::from(self.month);
        if month <= 2 {
            year -= 1;
        }
        let era = if year >= 0 { year } else { year - 399 } / 400;
        let year_of_era = (year - era * 400) as u32;
        let day_of_year = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5
            + u32::from(self.day)
            - 1;
        let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
        i64::from(era) * 146097 + i64::from(day_of_era) - 719468
    }
}

fn days_in_month(year: i32, month: u8) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap(year) => 29,
        2 => 28,
        _ => 0,
    }
}

fn is_leap(year: i32) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

/// The machine's local calendar date, or nothing if the clock cannot be read.
pub(crate) fn local_today() -> Option<Date> {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
    let secs = libc::time_t::try_from(secs).ok()?;
    // `localtime_r` is thread-safe and writes `tm` only when it returns
    // non-null. A null result leaves `tm` unread.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let ok = unsafe { !libc::localtime_r(&secs, &mut tm).is_null() };
    if !ok {
        return None;
    }
    Date::from_parts(
        tm.tm_year.checked_add(1900)?,
        u32::try_from(tm.tm_mon).ok()?.checked_add(1)?,
        u32::try_from(tm.tm_mday).ok()?,
    )
}

fn usage_path(data_dir: &Path) -> PathBuf {
    data_dir.join("usage.json")
}

/// One read of a Claude Code or Codex session. Other presets return without
/// touching the file. A missing total, a locked database, or an unreadable
/// usage file leaves `usage.json` as it was. This lock is the usage file's
/// own, not `Core`'s operations lock, and it covers the CLI read so two
/// overlapping reads cannot add the same increase twice.
pub(crate) fn record(data_dir: &Path, home: &CliHome, preset_id: &str, worktree: &Path, day: Date) {
    if !matches!(preset_id, "claude" | "codex") {
        return;
    }
    let path = usage_path(data_dir);
    with_usage_file(data_dir, || {
        let sample = match preset_id {
            "claude" => read_claude(home, worktree),
            "codex" => read_codex(home, worktree),
            _ => None,
        };
        let Some((session, total)) = sample else {
            return;
        };
        let mut store = match load_store(&path) {
            Loaded::Store(store) => store,
            Loaded::Unreadable => return,
        };
        if apply(&mut store, preset_id, &session, total, &day.iso()) {
            let _ = save_store(&path, &store);
        }
    });
}

/// The dialog's snapshot. Reads `usage.json` and nothing else.
pub(crate) fn report(data_dir: &Path, today: Option<Date>) -> UsageReport {
    let store = match load_store(&usage_path(data_dir)) {
        Loaded::Store(store) => store,
        Loaded::Unreadable => Store::default(),
    };
    report_from(&store, today)
}

fn with_usage_file<T>(dir: &Path, body: impl FnOnce() -> T) -> T {
    static LOCKS: LazyLock<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>> =
        LazyLock::new(|| Mutex::new(HashMap::new()));
    let lock = {
        let mut locks = LOCKS.lock().unwrap_or_else(|err| err.into_inner());
        locks
            .entry(dir.to_path_buf())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    };
    let _guard = lock.lock().unwrap_or_else(|err| err.into_inner());
    body()
}

enum Loaded {
    Store(Store),
    /// The file is there and cannot be parsed. Callers must not replace it.
    Unreadable,
}

fn load_store(path: &Path) -> Loaded {
    match fs::read_to_string(path) {
        Ok(text) if text.trim().is_empty() => Loaded::Store(Store::default()),
        Ok(text) => match serde_json::from_str(&text) {
            Ok(store) => Loaded::Store(store),
            Err(_) => Loaded::Unreadable,
        },
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Loaded::Store(Store::default()),
        Err(_) => Loaded::Unreadable,
    }
}

fn save_store(path: &Path, store: &Store) -> bool {
    let Some(parent) = path.parent() else {
        return false;
    };
    if fs::create_dir_all(parent).is_err() {
        return false;
    }
    let mut store = store.clone();
    store
        .days
        .retain(|_, count| count.claude.is_some() || count.codex.is_some());
    store.sessions.retain(|_, sessions| !sessions.is_empty());
    let Ok(mut json) = serde_json::to_string_pretty(&store) else {
        return false;
    };
    json.push('\n');
    let tmp = path.with_extension("json.tmp");
    if fs::write(&tmp, json).is_err() {
        return false;
    }
    fs::rename(&tmp, path).is_ok()
}

/// `true` when the store changed and should be written.
fn apply(store: &mut Store, cli: &str, session: &str, cumulative: u64, day: &str) -> bool {
    if !matches!(cli, "claude" | "codex") || session.is_empty() {
        return false;
    }
    let previous = store
        .sessions
        .get(cli)
        .and_then(|sessions| sessions.get(session))
        .copied();
    let (increase, baseline_changed) = match previous {
        Some(previous) if cumulative > previous => (cumulative - previous, true),
        Some(previous) if cumulative == previous => return false,
        Some(_) => (0, true),
        None if cumulative == 0 => return false,
        None => (cumulative, true),
    };
    if baseline_changed {
        store
            .sessions
            .entry(cli.to_string())
            .or_default()
            .insert(session.to_string(), cumulative);
    }
    if increase > 0 {
        let count = store.days.entry(day.to_string()).or_default();
        let slot = match cli {
            "claude" => &mut count.claude,
            _ => &mut count.codex,
        };
        *slot = Some(slot.unwrap_or(0).saturating_add(increase));
    }
    true
}

fn report_from(store: &Store, today: Option<Date>) -> UsageReport {
    let empty = !store.days.values().any(DayCount::has_tokens);
    let Some(today) = today else {
        return UsageReport {
            empty,
            claude: 0,
            codex: 0,
            days: Vec::new(),
        };
    };
    let mut dated = Vec::new();
    for (key, count) in &store.days {
        let Some(date) = Date::parse(key) else {
            continue;
        };
        if !today.same_week(date) || !count.has_tokens() {
            continue;
        }
        dated.push((date, count.claude.unwrap_or(0), count.codex.unwrap_or(0)));
    }
    dated.sort_by_key(|row| std::cmp::Reverse(row.0));
    let claude = dated
        .iter()
        .fold(0u64, |sum, row| sum.saturating_add(row.1));
    let codex = dated
        .iter()
        .fold(0u64, |sum, row| sum.saturating_add(row.2));
    let days = dated
        .into_iter()
        .map(|(date, claude, codex)| UsageDay {
            label: date.label(),
            claude,
            codex,
        })
        .collect();
    UsageReport {
        empty,
        claude,
        codex,
        days,
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
struct Store {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    days: BTreeMap<String, DayCount>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    sessions: BTreeMap<String, BTreeMap<String, u64>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
struct DayCount {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    claude: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    codex: Option<u64>,
}

impl DayCount {
    fn has_tokens(&self) -> bool {
        self.claude.unwrap_or(0) > 0 || self.codex.unwrap_or(0) > 0
    }
}

/// The newest `.jsonl` in the project directory for each cwd form. Only that
/// file's tail is read. No cost-state there means this form is skipped; an
/// older session file is not a fallback.
fn read_claude(home: &CliHome, worktree: &Path) -> Option<(String, u64)> {
    for cwd in cli_title::cwd_forms(worktree) {
        let Some(path) = cli_title::jsonl_newest_first(&home.claude_project_dir(&cwd))
            .into_iter()
            .next()
        else {
            continue;
        };
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if let Some(total) = claude_tail_total(&path) {
            return Some((name.to_string(), total));
        }
    }
    None
}

fn claude_tail_total(path: &Path) -> Option<u64> {
    last_cost_total(&read_tail(path)?)
}

fn read_tail(path: &Path) -> Option<String> {
    let mut file = fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let start = len.saturating_sub(CLAUDE_TAIL);
    if start > 0 {
        file.seek(SeekFrom::Start(start)).ok()?;
    }
    let mut bytes = Vec::new();
    file.take(CLAUDE_TAIL).read_to_end(&mut bytes).ok()?;
    if start > 0 {
        let end = bytes.iter().position(|byte| *byte == b'\n')?;
        bytes.drain(..=end);
    }
    String::from_utf8(bytes).ok()
}

/// The last `cost-state` object in `text`. Token fields are summed across
/// models. A tail with no such record, or a last record with no `modelUsage`,
/// is nothing.
fn last_cost_total(text: &str) -> Option<u64> {
    let mut last = None;
    for line in text.split('\n') {
        let line = line.trim_end_matches('\r').trim();
        if line.is_empty() || !line.contains("\"cost-state\"") {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if value.get("type").and_then(Value::as_str) != Some("cost-state") {
            continue;
        }
        last = Some(value);
    }
    sum_model_usage(last?.get("modelUsage")?)
}

fn sum_model_usage(usage: &Value) -> Option<u64> {
    let mut total = 0u64;
    for model in usage.as_object()?.values() {
        let Some(fields) = model.as_object() else {
            continue;
        };
        for key in TOKEN_FIELDS {
            if let Some(n) = fields.get(key).and_then(json_u64) {
                total = total.saturating_add(n);
            }
        }
    }
    Some(total)
}

fn json_u64(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_i64().and_then(|n| u64::try_from(n).ok()))
}

fn read_codex(home: &CliHome, worktree: &Path) -> Option<(String, u64)> {
    let db = cli_title::newest_state_db(&home.codex_root())?;
    let conn = Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
    conn.busy_timeout(CODEX_BUSY).ok()?;
    for cwd in cli_title::cwd_forms(worktree) {
        match codex_row(&conn, &cwd) {
            CodexRow::Found(id, total) => return Some((id, total)),
            CodexRow::Missing => continue,
            CodexRow::Failed => return None,
        }
    }
    None
}

enum CodexRow {
    Found(String, u64),
    Missing,
    Failed,
}

/// One query. A lock, a missing column, or any other error is a failed read.
fn codex_row(conn: &Connection, cwd: &Path) -> CodexRow {
    let cwd = cwd.to_string_lossy();
    match conn.query_row(
        "SELECT id, tokens_used FROM threads
         WHERE cwd = ?1
         ORDER BY updated_at_ms DESC
         LIMIT 1",
        [cwd.as_ref()],
        |row| {
            let id: String = row.get(0)?;
            let tokens: Option<i64> = row.get(1)?;
            Ok((id, tokens))
        },
    ) {
        Ok((id, Some(tokens))) if !id.is_empty() => match u64::try_from(tokens) {
            Ok(tokens) => CodexRow::Found(id, tokens),
            Err(_) => CodexRow::Failed,
        },
        Ok(_) => CodexRow::Failed,
        Err(rusqlite::Error::QueryReturnedNoRows) => CodexRow::Missing,
        Err(_) => CodexRow::Failed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!("shika-usage-{nanos}-{n}"));
            fs::create_dir_all(&path).unwrap();
            Self(path.canonicalize().unwrap())
        }

        fn home(&self) -> CliHome {
            CliHome::at(self.0.join("home"))
        }

        fn data(&self) -> PathBuf {
            self.0.join("data")
        }

        fn usage(&self) -> PathBuf {
            usage_path(&self.data())
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    const WORKTREE: &str = "/Users/x/code/shika/.worktrees/shika-draft-18db";

    fn day(text: &str) -> Date {
        Date::parse(text).unwrap()
    }

    fn claude_dir(home: &CliHome) -> PathBuf {
        home.claude_project_dir(Path::new(WORKTREE))
    }

    fn write_claude(home: &CliHome, name: &str, body: &str, mtime: u64) -> PathBuf {
        let dir = claude_dir(home);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        fs::write(&path, body).unwrap();
        set_mtime(&path, mtime);
        path
    }

    fn cost(total_fields: &str) -> String {
        format!(
            "{{\"type\":\"cost-state\",\"modelUsage\":{{\"claude-opus\":{{{total_fields}}}}}}}\n"
        )
    }

    fn record_claude(scratch: &Scratch, total: u64, on: &str) {
        let home = scratch.home();
        write_claude(
            &home,
            "session.jsonl",
            &cost(&format!("\"inputTokens\":{total}")),
            10,
        );
        record(
            &scratch.data(),
            &home,
            "claude",
            Path::new(WORKTREE),
            day(on),
        );
    }

    fn json(scratch: &Scratch) -> Value {
        serde_json::from_str(&fs::read_to_string(scratch.usage()).unwrap()).unwrap()
    }

    fn set_mtime(path: &Path, secs: u64) {
        let file = fs::File::options().write(true).open(path).unwrap();
        file.set_modified(UNIX_EPOCH + Duration::from_secs(secs))
            .unwrap();
    }

    #[test]
    fn a_read_adds_the_increase_once_and_a_repeat_adds_nothing() {
        let scratch = Scratch::new();
        record_claude(&scratch, 1000, "2026-10-12");
        let shown = report(&scratch.data(), Some(day("2026-10-12")));
        assert_eq!(shown.claude, 1000);
        assert_eq!(shown.codex, 0);
        assert!(!shown.empty);
        let before = fs::read(scratch.usage()).unwrap();
        record_claude(&scratch, 1000, "2026-10-12");
        assert_eq!(fs::read(scratch.usage()).unwrap(), before);
        record_claude(&scratch, 1250, "2026-10-12");
        let shown = report(&scratch.data(), Some(day("2026-10-12")));
        assert_eq!(shown.claude, 1250);
        assert_eq!(json(&scratch)["sessions"]["claude"]["session.jsonl"], 1250);
    }

    #[test]
    fn a_lower_cumulative_is_a_new_baseline_and_adds_nothing() {
        let scratch = Scratch::new();
        record_claude(&scratch, 500, "2026-10-12");
        record_claude(&scratch, 80, "2026-10-12");
        assert_eq!(json(&scratch)["days"]["2026-10-12"]["claude"], 500);
        assert_eq!(json(&scratch)["sessions"]["claude"]["session.jsonl"], 80);
        record_claude(&scratch, 100, "2026-10-12");
        assert_eq!(json(&scratch)["days"]["2026-10-12"]["claude"], 520);
        assert!(json(&scratch)["days"]["2026-10-12"].get("codex").is_none());
    }

    #[test]
    fn a_failed_read_leaves_the_file_unchanged() {
        let scratch = Scratch::new();
        record_claude(&scratch, 40, "2026-10-12");
        let before = fs::read(scratch.usage()).unwrap();
        let home = scratch.home();
        write_claude(&home, "session.jsonl", "{\"type\":\"assistant\"}\n", 10);
        record(
            &scratch.data(),
            &home,
            "claude",
            Path::new(WORKTREE),
            day("2026-10-12"),
        );
        assert_eq!(fs::read(scratch.usage()).unwrap(), before);

        fs::write(scratch.usage(), "{").unwrap();
        write_claude(&home, "session.jsonl", &cost("\"inputTokens\":9"), 10);
        record(
            &scratch.data(),
            &home,
            "claude",
            Path::new(WORKTREE),
            day("2026-10-12"),
        );
        assert_eq!(fs::read_to_string(scratch.usage()).unwrap(), "{");
    }

    #[test]
    fn missing_days_are_a_gap_and_other_presets_write_nothing() {
        let scratch = Scratch::new();
        record_claude(&scratch, 10, "2026-10-12");
        record_claude(&scratch, 15, "2026-10-14");
        let value = json(&scratch);
        assert!(value["days"].get("2026-10-13").is_none());
        assert_eq!(value["days"]["2026-10-12"]["claude"], 10);
        assert_eq!(value["days"]["2026-10-14"]["claude"], 5);
        let home = scratch.home();
        for preset in ["cursor", "pi", "kiro"] {
            record(
                &scratch.data(),
                &home,
                preset,
                Path::new(WORKTREE),
                day("2026-10-12"),
            );
        }
        assert_eq!(json(&scratch), value);
    }

    #[test]
    fn the_week_total_is_the_sum_of_that_weeks_saved_days() {
        let scratch = Scratch::new();
        let home = scratch.home();
        // Distinct session files so each day's cumulative is its own baseline.
        for (name, total, on, mtime) in [
            ("sun.jsonl", 100_u64, "2026-10-11", 1_u64),
            ("mon.jsonl", 40, "2026-10-12", 2),
            ("wed.jsonl", 7, "2026-10-14", 3),
            ("next.jsonl", 9, "2026-10-19", 4),
        ] {
            write_claude(
                &home,
                name,
                &cost(&format!("\"inputTokens\":{total}")),
                mtime,
            );
            // Each file is newer than the last, so this read sees only that file.
            record(
                &scratch.data(),
                &home,
                "claude",
                Path::new(WORKTREE),
                day(on),
            );
        }
        let shown = report(&scratch.data(), Some(day("2026-10-14")));
        assert_eq!(shown.claude, 47);
        assert_eq!(shown.codex, 0);
        assert_eq!(
            shown.days,
            vec![
                UsageDay {
                    label: "Wednesday, Oct 14".into(),
                    claude: 7,
                    codex: 0,
                },
                UsageDay {
                    label: "Monday, Oct 12".into(),
                    claude: 40,
                    codex: 0,
                },
            ]
        );
        let stored = json(&scratch);
        assert_eq!(stored["days"]["2026-10-11"]["claude"], 100);
        assert_eq!(stored["days"]["2026-10-19"]["claude"], 9);
        let outside = report(&scratch.data(), Some(day("2026-10-20")));
        assert!(!outside.empty);
        assert_eq!(outside.claude, 9);
        assert_eq!(outside.days.len(), 1);
    }

    #[test]
    fn an_empty_file_is_the_empty_state_and_last_week_is_not_zeroed() {
        let scratch = Scratch::new();
        let shown = report(&scratch.data(), Some(day("2026-10-14")));
        assert!(shown.empty);
        assert!(shown.days.is_empty());
        record_claude(&scratch, 3, "2026-10-04");
        let shown = report(&scratch.data(), Some(day("2026-10-14")));
        assert!(!shown.empty);
        assert_eq!(shown.claude, 0);
        assert!(shown.days.is_empty());
        assert_eq!(json(&scratch)["days"]["2026-10-04"]["claude"], 3);
    }

    #[test]
    fn monday_through_sunday_is_the_local_week() {
        let monday = day("2026-10-12");
        let sunday = day("2026-10-18");
        assert_eq!(monday.weekday(), 0);
        assert_eq!(sunday.weekday(), 6);
        assert_eq!(day("1970-01-01").label(), "Thursday, Jan 1");
        assert!(monday.same_week(sunday));
        assert!(sunday.same_week(monday));
        assert!(!monday.same_week(day("2026-10-11")));
        assert!(!sunday.same_week(day("2026-10-19")));
        assert_eq!(format_tokens(0), "0");
        assert_eq!(format_tokens(1234), "1,234");
        assert_eq!(format_tokens(10_042_955), "10,042,955");
    }

    #[test]
    fn claude_reads_the_tail_and_sums_the_last_cost_state() {
        let scratch = Scratch::new();
        let home = scratch.home();
        let dir = claude_dir(&home);
        fs::create_dir_all(&dir).unwrap();
        let head = cost(
            "\"inputTokens\":999999,\"outputTokens\":1,\"costUSD\":50,\"webSearchRequests\":4",
        );
        let assistant = concat!(
            "{\"type\":\"assistant\",\"message\":\"mentions \\\"cost-state\\\"\",",
            "\"modelUsage\":{\"m\":{\"inputTokens\":800}}}\n"
        );
        let tail = concat!(
            "{\"type\":\"cost-state\",\"modelUsage\":{",
            "\"one\":{\"inputTokens\":4,\"outputTokens\":1,\"thinkingTokens\":1,\"cacheReadInputTokens\":1,\"cacheCreationInputTokens\":1,\"costUSD\":80},",
            "\"two\":{\"inputTokens\":2,\"outputTokens\":0,\"thinkingTokens\":0,\"cacheReadInputTokens\":0,\"cacheCreationInputTokens\":0}",
            "}}\n"
        );
        let mut bytes = head.into_bytes();
        let filler = b"{\"type\":\"assistant\"}\n";
        while bytes.len() < 96 * 1024 {
            bytes.extend_from_slice(filler);
        }
        bytes.extend_from_slice(assistant.as_bytes());
        bytes.extend_from_slice(tail.as_bytes());
        let path = dir.join("big.jsonl");
        fs::write(&path, &bytes).unwrap();
        // The head's cost-state is outside the tail. 4+1+1+1+1+2 = 10.
        record(
            &scratch.data(),
            &home,
            "claude",
            Path::new(WORKTREE),
            day("2026-10-12"),
        );
        assert_eq!(json(&scratch)["days"]["2026-10-12"]["claude"], 10);

        // A cost-state that sits only in the head is not a total.
        let mut head_only = cost("\"inputTokens\":999999").into_bytes();
        while head_only.len() < 96 * 1024 {
            head_only.extend_from_slice(filler);
        }
        fs::write(&path, head_only).unwrap();
        let before = fs::read(scratch.usage()).unwrap();
        record(
            &scratch.data(),
            &home,
            "claude",
            Path::new(WORKTREE),
            day("2026-10-12"),
        );
        assert_eq!(fs::read(scratch.usage()).unwrap(), before);
    }

    #[test]
    fn claude_uses_the_newest_file_and_ignores_subagents_and_stats_cache() {
        let scratch = Scratch::new();
        let home = scratch.home();
        write_claude(&home, "older.jsonl", &cost("\"inputTokens\":999"), 1);
        write_claude(&home, "newer.jsonl", "{\"type\":\"user\"}\n", 2);
        let before_missing = scratch.usage();
        record(
            &scratch.data(),
            &home,
            "claude",
            Path::new(WORKTREE),
            day("2026-10-12"),
        );
        assert!(!before_missing.exists());

        write_claude(&home, "newer.jsonl", &cost("\"outputTokens\":6"), 3);
        let nested = claude_dir(&home).join("newer").join("subagents");
        fs::create_dir_all(&nested).unwrap();
        let sub = nested.join("agent.jsonl");
        fs::write(&sub, cost("\"inputTokens\":5000")).unwrap();
        set_mtime(&sub, 50);
        let stats = claude_dir(&home).join("stats-cache.json");
        fs::write(&stats, cost("\"inputTokens\":7000")).unwrap();
        set_mtime(&stats, 60);
        record(
            &scratch.data(),
            &home,
            "claude",
            Path::new(WORKTREE),
            day("2026-10-12"),
        );
        assert_eq!(json(&scratch)["days"]["2026-10-12"]["claude"], 6);
        assert_eq!(json(&scratch)["sessions"]["claude"]["newer.jsonl"], 6);
    }

    #[test]
    fn claude_follows_its_config_dir_and_a_resolved_path() {
        let scratch = Scratch::new();
        let config = scratch.0.join("claude-config");
        let home = CliHome::with_homes(scratch.0.join("home"), Some(config), None);
        let real = scratch.0.join("repo/.worktrees/task");
        fs::create_dir_all(&real).unwrap();
        let link_root = scratch.0.join("link-root");
        std::os::unix::fs::symlink(scratch.0.join("repo"), &link_root).unwrap();
        let through = link_root.join(".worktrees/task");
        let dir = home.claude_project_dir(&real);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("a.jsonl"), cost("\"thinkingTokens\":4")).unwrap();
        record(
            &scratch.data(),
            &home,
            "claude",
            &through,
            day("2026-10-12"),
        );
        assert_eq!(json(&scratch)["days"]["2026-10-12"]["claude"], 4);
        assert_eq!(json(&scratch)["sessions"]["claude"]["a.jsonl"], 4);
    }

    fn codex_db(path: &Path, rows: &[(&str, &str, i64, i64)]) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        if path.exists() {
            fs::remove_file(path).unwrap();
        }
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            "CREATE TABLE threads (
                id TEXT PRIMARY KEY,
                cwd TEXT NOT NULL,
                tokens_used INTEGER,
                updated_at_ms INTEGER NOT NULL DEFAULT 0
            );",
        )
        .unwrap();
        for (id, cwd, tokens, updated) in rows {
            conn.execute(
                "INSERT INTO threads (id, cwd, tokens_used, updated_at_ms) VALUES (?1, ?2, ?3, ?4)",
                (id, cwd, tokens, updated),
            )
            .unwrap();
        }
    }

    #[test]
    fn codex_uses_the_newest_thread_in_the_newest_database() {
        let scratch = Scratch::new();
        let root = scratch.0.join("home/.codex");
        codex_db(
            &root.join("state_1.sqlite"),
            &[("old-db", WORKTREE, 999, 50)],
        );
        fs::write(root.join("state_9.sqlite-wal"), "nope").unwrap();
        fs::write(root.join("rollout.jsonl"), cost("\"inputTokens\":999")).unwrap();
        codex_db(
            &root.join("state_2.sqlite"),
            &[
                ("other", "/somewhere/else", 80, 90),
                ("early", WORKTREE, 10, 1),
                ("latest", WORKTREE, 25, 30),
            ],
        );
        let home = CliHome::at(scratch.0.join("home"));
        record(
            &scratch.data(),
            &home,
            "codex",
            Path::new(WORKTREE),
            day("2026-10-12"),
        );
        assert_eq!(json(&scratch)["days"]["2026-10-12"]["codex"], 25);
        assert!(json(&scratch)["days"]["2026-10-12"].get("claude").is_none());
        assert_eq!(json(&scratch)["sessions"]["codex"]["latest"], 25);
        let before = fs::read(scratch.usage()).unwrap();
        record(
            &scratch.data(),
            &home,
            "codex",
            Path::new(WORKTREE),
            day("2026-10-12"),
        );
        assert_eq!(fs::read(scratch.usage()).unwrap(), before);
        // Replace the row's total by writing a new database image. The next
        // read adds only the increase from the saved thread baseline.
        codex_db(
            &root.join("state_2.sqlite"),
            &[("latest", WORKTREE, 40, 30)],
        );
        record(
            &scratch.data(),
            &home,
            "codex",
            Path::new(WORKTREE),
            day("2026-10-13"),
        );
        assert_eq!(json(&scratch)["days"]["2026-10-12"]["codex"], 25);
        assert_eq!(json(&scratch)["days"]["2026-10-13"]["codex"], 15);
        assert!(json(&scratch)["days"].get("2026-10-11").is_none());
    }

    #[test]
    fn a_locked_codex_database_is_skipped() {
        let scratch = Scratch::new();
        let root = scratch.0.join("home/.codex");
        let db = root.join("state_5.sqlite");
        codex_db(&db, &[("t", WORKTREE, 70, 1)]);
        record_claude(&scratch, 2, "2026-10-12");
        let before = fs::read(scratch.usage()).unwrap();
        let held = Connection::open(&db).unwrap();
        held.execute_batch("BEGIN EXCLUSIVE").unwrap();
        let started = std::time::Instant::now();
        record(
            &scratch.data(),
            &CliHome::at(scratch.0.join("home")),
            "codex",
            Path::new(WORKTREE),
            day("2026-10-12"),
        );
        let elapsed = started.elapsed();
        drop(held);
        assert_eq!(fs::read(scratch.usage()).unwrap(), before);
        assert!(
            elapsed >= Duration::from_millis(150),
            "busy wait was {elapsed:?}"
        );
        assert!(
            elapsed < Duration::from_secs(3),
            "busy wait was {elapsed:?}"
        );
    }

    #[test]
    fn codex_home_and_a_query_failure_skip() {
        let scratch = Scratch::new();
        let root = scratch.0.join("codex-home");
        codex_db(&root.join("state_1.sqlite"), &[("t", WORKTREE, 8, 1)]);
        let home = CliHome::with_homes(scratch.0.join("home"), None, Some(root.clone()));
        record(
            &scratch.data(),
            &home,
            "codex",
            Path::new(WORKTREE),
            day("2026-10-12"),
        );
        assert_eq!(json(&scratch)["sessions"]["codex"]["t"], 8);
        fs::write(root.join("state_1.sqlite"), "not a database").unwrap();
        let before = fs::read(scratch.usage()).unwrap();
        record(
            &scratch.data(),
            &home,
            "codex",
            Path::new(WORKTREE),
            day("2026-10-12"),
        );
        assert_eq!(fs::read(scratch.usage()).unwrap(), before);
    }

    #[test]
    fn core_token_usage_reads_the_usage_file_for_the_local_week() {
        let scratch = Scratch::new();
        let core = crate::Core::open(scratch.data()).unwrap();
        let today = local_today().unwrap();
        fs::write(
            scratch.usage(),
            format!(
                "{{\"days\":{{\"2019-01-01\":{{\"claude\":9}},\"{}\":{{\"claude\":12,\"codex\":3}}}}}}\n",
                today.iso()
            ),
        )
        .unwrap();
        let report = core.token_usage();
        assert!(!report.empty);
        assert_eq!(report.claude, 12);
        assert_eq!(report.codex, 3);
        assert_eq!(report.days.len(), 1);
        assert_eq!(report.days[0].label, today.label());
    }
}
