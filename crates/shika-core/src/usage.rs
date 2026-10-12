//! Local usage metadata, collected independently of terminal rendering.
//! See docs/token-usage.md for recovery, provider limits, and resource bounds.

mod collector;
pub(crate) use collector::Collector;

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UsageReport {
    pub unreadable: bool,
    /// A source failed or still has an unfinished record. Totals are retained.
    pub pending: bool,
    pub empty: bool,
    pub claude: u64,
    pub codex: u64,
    pub days: Vec<UsageDay>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UsageDay {
    pub label: String,
    pub claude: u64,
    pub codex: u64,
}

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
        if !(0..=9999).contains(&year) || !(1..=12).contains(&month) {
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
        if year.len() != 4
            || month.len() != 2
            || day.len() != 2
            || !year
                .bytes()
                .chain(month.bytes())
                .chain(day.bytes())
                .all(|b| b.is_ascii_digit())
        {
            return None;
        }
        Self::from_parts(year.parse().ok()?, month.parse().ok()?, day.parse().ok()?)
    }

    fn iso(self) -> String {
        format!("{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }

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
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        _ => 0,
    }
}

pub(crate) fn local_today() -> Option<Date> {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
    let secs = libc::time_t::try_from(secs).ok()?;
    local_date(secs)
}

fn local_date(secs: libc::time_t) -> Option<Date> {
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&secs, &mut tm).is_null() } {
        return None;
    }
    Date::from_parts(
        tm.tm_year.checked_add(1900)?,
        u32::try_from(tm.tm_mon).ok()?.checked_add(1)?,
        u32::try_from(tm.tm_mday).ok()?,
    )
}

/// Both providers persist RFC 3339 timestamps. Convert the recorded instant
/// using the Mac's local calendar, including its historical DST offset.
pub(crate) fn recorded_date(timestamp: &str) -> Option<Date> {
    local_date(timestamp_seconds(timestamp)?)
}

fn timestamp_seconds(timestamp: &str) -> Option<i64> {
    let bytes = timestamp.as_bytes();
    if !(20..=64).contains(&bytes.len())
        || !bytes.is_ascii()
        || !matches!(bytes[10], b'T' | b't')
        || bytes[13] != b':'
        || bytes[16] != b':'
    {
        return None;
    }
    let date = Date::parse(&timestamp[..10])?;
    let number = |part: &str| -> Option<i64> {
        part.bytes()
            .all(|byte| byte.is_ascii_digit())
            .then_some(())?;
        part.parse().ok()
    };
    let hour = number(&timestamp[11..13])?;
    let minute = number(&timestamp[14..16])?;
    let second = number(&timestamp[17..19])?;
    if hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    let mut zone = &timestamp[19..];
    if let Some(fraction) = zone.strip_prefix('.') {
        let digits = fraction.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 {
            return None;
        }
        zone = &fraction[digits..];
    }
    let offset = match zone.as_bytes() {
        [b'Z' | b'z'] => 0,
        [sign @ (b'+' | b'-'), _, _, b':', _, _] => {
            let hours = number(&zone[1..3])?;
            let minutes = number(&zone[4..6])?;
            if hours > 23 || minutes > 59 {
                return None;
            }
            (hours * 3600 + minutes * 60) * if *sign == b'+' { 1 } else { -1 }
        }
        _ => return None,
    };
    Some(date.epoch_days() * 86400 + hour * 3600 + minute * 60 + second - offset)
}

pub(crate) fn report(data_dir: &Path, today: Option<Date>) -> UsageReport {
    collector::report(data_dir, today)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calendar_week_and_decimal_formatting() {
        let monday = Date::parse("2026-10-12").unwrap();
        let sunday = Date::parse("2026-10-18").unwrap();
        assert_eq!(monday.weekday(), 0);
        assert_eq!(sunday.weekday(), 6);
        assert!(monday.same_week(sunday));
        assert!(!monday.same_week(Date::parse("2026-10-11").unwrap()));
        assert!(!sunday.same_week(Date::parse("2026-10-19").unwrap()));
        assert!(Date::parse("2026-02-29").is_none());
        assert_eq!(
            Date::parse("1970-01-01").unwrap().label(),
            "Thursday, Jan 1"
        );
        assert_eq!(format_tokens(0), "0");
        assert_eq!(format_tokens(1234), "1,234");
        assert_eq!(format_tokens(10_042_955), "10,042,955");
    }

    #[test]
    fn provider_timestamps_preserve_the_instant_across_calendar_boundaries() {
        assert_eq!(timestamp_seconds("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(timestamp_seconds("1969-12-31T16:00:00-08:00"), Some(0));
        assert_eq!(
            timestamp_seconds("1970-01-01T01:00:00.123456+01:00"),
            Some(0)
        );
        assert_eq!(
            recorded_date("2026-10-11T23:30:00-07:00"),
            recorded_date("2026-10-12T06:30:00.123Z")
        );
        for invalid in [
            "2026-02-29T12:00:00Z",
            "2026-10-12T24:00:00Z",
            "2026-10-12T00:60:00Z",
            "2026-10-12T00:00:00+24:00",
            "2026-10-12T00:00:00.Z",
            "2026-10-12T00:00:00",
            "2026-10-12T00:00:00Zextra",
            "2026-10-12T00:00:00+0a:00",
            "0000-01-01T00:00:00+23:59",
            "9999-12-31T23:59:59-23:59",
            "🦌2026-10-12T00:00:00Z",
        ] {
            assert_eq!(recorded_date(invalid), None, "{invalid}");
        }
        assert!(Date::from_parts(-1, 12, 31).is_none());
        assert!(Date::from_parts(10000, 1, 1).is_none());
    }
}
