//! The SQLite store: schema and migrations, the one-time legacy `usage.json`
//! import, and the read-only report the Usage dashboard draws.

use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use serde::Deserialize;

use super::super::{Date, UsageCalendar, UsageDay, UsageReport};
use super::BUSY;

pub(super) fn database(data_dir: &Path) -> PathBuf {
    data_dir.join("usage.sqlite")
}

pub(super) fn open_store(data_dir: &Path) -> rusqlite::Result<Connection> {
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
    if version > 3 {
        return Err(rusqlite::Error::InvalidQuery);
    }
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "FULL")?;
    let tx = conn.transaction()?;
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS days(day TEXT NOT NULL, cli TEXT NOT NULL, tokens INTEGER NOT NULL CHECK(tokens >= 0), PRIMARY KEY(day, cli));
         CREATE TABLE IF NOT EXISTS sessions(cli TEXT NOT NULL, id TEXT NOT NULL, observed INTEGER NOT NULL DEFAULT 0, baseline INTEGER NOT NULL DEFAULT 0, PRIMARY KEY(cli, id));
         CREATE TABLE IF NOT EXISTS receipts(cli TEXT NOT NULL, id TEXT NOT NULL, session TEXT NOT NULL, a INTEGER NOT NULL, b INTEGER NOT NULL, c INTEGER NOT NULL, d INTEGER NOT NULL, PRIMARY KEY(cli, id));
         CREATE INDEX IF NOT EXISTS receipts_session ON receipts(cli,session);
         CREATE TABLE IF NOT EXISTS files(path TEXT PRIMARY KEY, device INTEGER NOT NULL, inode INTEGER NOT NULL, offset INTEGER NOT NULL, modified TEXT NOT NULL);
         CREATE TABLE IF NOT EXISTS source_files(source TEXT NOT NULL,path TEXT NOT NULL,cli TEXT NOT NULL,session TEXT NOT NULL,PRIMARY KEY(source,path));
         CREATE TABLE IF NOT EXISTS indexes(source TEXT PRIMARY KEY);
         CREATE TABLE IF NOT EXISTS issues(source TEXT PRIMARY KEY);
         CREATE TABLE IF NOT EXISTS metadata(key TEXT PRIMARY KEY, value TEXT NOT NULL);"
    )?;
    let modified: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info('files') WHERE name='modified')",
        [],
        |r| r.get(0),
    )?;
    if !modified {
        tx.execute_batch("ALTER TABLE files ADD COLUMN modified TEXT NOT NULL DEFAULT ''")?;
    }
    let anchors: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info('files') WHERE name='anchors')",
        [],
        |r| r.get(0),
    )?;
    if !anchors {
        tx.execute_batch("ALTER TABLE files ADD COLUMN anchors TEXT NOT NULL DEFAULT ''")?;
    }
    tx.pragma_update(None, "user_version", 3)?;
    tx.commit()?;
    import_json(&mut conn, data_dir)?;
    Ok(conn)
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Legacy {
    #[serde(default)]
    days: BTreeMap<String, LegacyDay>,
    #[serde(default)]
    sessions: BTreeMap<String, BTreeMap<String, i64>>,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct LegacyDay {
    claude: Option<i64>,
    codex: Option<i64>,
}

pub(super) fn read_legacy(dir: &Path) -> std::io::Result<Legacy> {
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

pub(super) fn import_json(conn: &mut Connection, dir: &Path) -> rusqlite::Result<()> {
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

pub(crate) fn report(dir: &Path, today: Option<Date>) -> UsageReport {
    let mut report = UsageReport {
        unreadable: false,
        pending: false,
        empty: true,
        claude: 0,
        codex: 0,
        days: Vec::new(),
        calendar: today.and_then(UsageCalendar::new),
    };
    let result = (|| -> rusqlite::Result<()> {
        if report.calendar.is_none() {
            return Err(rusqlite::Error::InvalidQuery);
        }
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
        let mut conn = Connection::open_with_flags(
            database(dir),
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        conn.busy_timeout(BUSY)?;
        // One consistent read snapshot for gaps, daily counts and both totals.
        let conn = conn.transaction()?;
        let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version > 3 {
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
            let low = format!("-{} days", 51 * 7 + today.weekday());
            let high = "+1 day";
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

// Days are inserted in SQL date order; this function is only for the legacy
// BTreeMap path, which is ascending. Keep actual dates alongside labels below.
pub(super) fn date_label_order(label: &str) -> usize {
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

pub(super) fn add_day(
    report: &mut UsageReport,
    today: Option<Date>,
    date: &str,
    cli: &str,
    total: i64,
) -> rusqlite::Result<()> {
    let date = Date::parse(date).ok_or(rusqlite::Error::InvalidQuery)?;
    let total = u64::try_from(total).map_err(|_| rusqlite::Error::InvalidQuery)?;
    if !matches!(cli, "claude" | "codex") {
        return Err(rusqlite::Error::InvalidQuery);
    }
    if total == 0 {
        return Ok(());
    }
    report.empty = false;
    if let (Some(calendar), Some(today)) = (&mut report.calendar, today) {
        calendar.add(today, date, cli, total);
    }
    if !today.is_some_and(|today| date <= today && today.same_week(date)) {
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
