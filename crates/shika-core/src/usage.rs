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
}
