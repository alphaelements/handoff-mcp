//! Reporting period parsing (FR-515 / SPEC-515).
//!
//! A period is an inclusive range of calendar days. It is written either as an
//! ISO 8601 week (`2026-W41`, Monday through Sunday) or as an explicit date
//! range (`2026-10-05..2026-10-11`).

use anyhow::{anyhow, bail, Result};
use chrono::{Datelike, Duration, NaiveDate, Weekday};

/// Separator between the two dates of an explicit range.
const RANGE_SEPARATOR: &str = "..";

/// Inclusive range of calendar days (`start <= end`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Period {
    pub start: NaiveDate,
    pub end: NaiveDate,
}

impl Period {
    /// Builds a period, rejecting an inverted range.
    pub fn new(start: NaiveDate, end: NaiveDate) -> Result<Self> {
        if end < start {
            bail!("Invalid period: end {end} is before start {start}");
        }
        Ok(Self { start, end })
    }

    /// Parses `YYYY-Www` (ISO week) or `YYYY-MM-DD..YYYY-MM-DD`.
    pub fn parse(s: &str) -> Result<Self> {
        let s = s.trim();
        if let Some((from, to)) = s.split_once(RANGE_SEPARATOR) {
            return Self::new(parse_date(from)?, parse_date(to)?);
        }
        parse_iso_week(s).ok_or_else(|| {
            anyhow!(
                "Invalid period '{s}' (expected an ISO week like 2026-W41 or a range like 2026-10-05..2026-10-11)"
            )
        })
    }

    /// The ISO week (Monday..Sunday) containing `date`.
    pub fn iso_week_of(date: NaiveDate) -> Self {
        let week = date.iso_week();
        // `from_isoywd_opt` cannot fail for a week taken from a real date.
        let start = NaiveDate::from_isoywd_opt(week.year(), week.week(), Weekday::Mon)
            .expect("an ISO week derived from a valid date exists");
        Self {
            start,
            end: start + Duration::days(6),
        }
    }

    /// The calendar month containing `date`.
    pub fn month_of(date: NaiveDate) -> Self {
        let start = date.with_day(1).expect("day 1 exists in every month");
        let next_month_start = if start.month() == 12 {
            NaiveDate::from_ymd_opt(start.year() + 1, 1, 1)
        } else {
            NaiveDate::from_ymd_opt(start.year(), start.month() + 1, 1)
        }
        .expect("the first day of the next month is a valid date");
        Self {
            start,
            end: next_month_start - Duration::days(1),
        }
    }

    /// Parses `YYYY-MM` (a whole calendar month); `None` when `s` is not of
    /// that form or names an impossible month.
    pub fn parse_month(s: &str) -> Option<Self> {
        let (year, month) = s.trim().split_once('-')?;
        if year.len() != 4 || month.len() != 2 {
            return None;
        }
        let first = NaiveDate::from_ymd_opt(year.parse().ok()?, month.parse().ok()?, 1)?;
        Some(Self::month_of(first))
    }

    /// Whether `date` lies within the period (both ends inclusive).
    pub fn contains(&self, date: NaiveDate) -> bool {
        self.start <= date && date <= self.end
    }

    /// Number of days covered (inclusive).
    pub fn days(&self) -> i64 {
        (self.end - self.start).num_days() + 1
    }

    /// The period of the same length immediately after this one.
    pub fn next(&self) -> Self {
        let days = self.days();
        Self {
            start: self.end + Duration::days(1),
            end: self.end + Duration::days(days),
        }
    }
}

/// Strict `YYYY-MM-DD`.
pub fn parse_date(s: &str) -> Result<NaiveDate> {
    let s = s.trim();
    NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .map_err(|_| anyhow!("Invalid date '{s}' (expected YYYY-MM-DD)"))
}

fn parse_iso_week(s: &str) -> Option<Period> {
    let (year, week) = s.split_once("-W")?;
    if year.len() != 4 || week.len() != 2 {
        return None;
    }
    let year: i32 = year.parse().ok()?;
    let week: u32 = week.parse().ok()?;
    let start = NaiveDate::from_isoywd_opt(year, week, Weekday::Mon)?;
    Some(Period {
        start,
        end: start + Duration::days(6),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> NaiveDate {
        parse_date(s).unwrap()
    }

    #[test]
    fn iso_week_spans_monday_to_sunday() {
        let p = Period::parse("2026-W41").unwrap();
        assert_eq!(p.start, d("2026-10-05"));
        assert_eq!(p.end, d("2026-10-11"));
        assert_eq!(p.start.weekday(), Weekday::Mon);
        assert_eq!(p.end.weekday(), Weekday::Sun);
    }

    #[test]
    fn iso_week_one_can_start_in_the_previous_year() {
        let p = Period::parse("2026-W01").unwrap();
        assert_eq!(p.start, d("2025-12-29"));
        assert_eq!(p.end, d("2026-01-04"));
    }

    #[test]
    fn iso_week_53_exists_only_in_long_years() {
        assert!(Period::parse("2026-W53").is_ok());
        assert!(Period::parse("2025-W53").is_err());
        assert!(Period::parse("2026-W00").is_err());
        assert!(Period::parse("2026-W54").is_err());
    }

    #[test]
    fn date_range_is_inclusive() {
        let p = Period::parse("2026-10-06..2026-10-12").unwrap();
        assert_eq!(p.start, d("2026-10-06"));
        assert_eq!(p.end, d("2026-10-12"));
        assert_eq!(p.days(), 7);
        assert!(p.contains(d("2026-10-06")));
        assert!(p.contains(d("2026-10-12")));
        assert!(!p.contains(d("2026-10-05")));
        assert!(!p.contains(d("2026-10-13")));
    }

    #[test]
    fn single_day_range_is_valid() {
        let p = Period::parse("2026-10-06..2026-10-06").unwrap();
        assert_eq!(p.days(), 1);
    }

    #[test]
    fn invalid_periods_are_rejected_with_a_clear_message() {
        for bad in [
            "",
            "2026",
            "2026-41",
            "26-W41",
            "2026-W4",
            "2026-10-12..2026-10-06",
            "2026-10-06..",
            "..2026-10-06",
            "2026-13-01..2026-13-02",
            "yesterday",
        ] {
            let err = Period::parse(bad).unwrap_err().to_string();
            assert!(!err.is_empty(), "{bad}");
        }
        assert!(Period::parse("2026-W4")
            .unwrap_err()
            .to_string()
            .contains("2026-W41"));
    }

    #[test]
    fn surrounding_whitespace_is_tolerated() {
        assert!(Period::parse(" 2026-W41 ").is_ok());
        assert!(Period::parse("2026-10-06 .. 2026-10-12").is_ok());
    }

    #[test]
    fn iso_week_of_a_date() {
        let p = Period::iso_week_of(d("2026-10-08"));
        assert_eq!(p, Period::parse("2026-W41").unwrap());
        // The ISO year differs from the calendar year around New Year.
        let p = Period::iso_week_of(d("2027-01-01"));
        assert_eq!(p, Period::parse("2026-W53").unwrap());
    }

    #[test]
    fn next_period_has_the_same_length() {
        let p = Period::parse("2026-W41").unwrap().next();
        assert_eq!(p, Period::parse("2026-W42").unwrap());
        let r = Period::parse("2026-10-01..2026-10-03").unwrap().next();
        assert_eq!(r.start, d("2026-10-04"));
        assert_eq!(r.end, d("2026-10-06"));
    }

    #[test]
    fn month_of_spans_the_whole_calendar_month() {
        let p = Period::month_of(d("2026-10-08"));
        assert_eq!((p.start, p.end), (d("2026-10-01"), d("2026-10-31")));
        // February in a leap year, and the December -> January rollover.
        let p = Period::month_of(d("2028-02-10"));
        assert_eq!((p.start, p.end), (d("2028-02-01"), d("2028-02-29")));
        let p = Period::month_of(d("2026-12-31"));
        assert_eq!((p.start, p.end), (d("2026-12-01"), d("2026-12-31")));
    }

    #[test]
    fn parse_month_accepts_only_yyyy_mm() {
        let p = Period::parse_month("2026-02").unwrap();
        assert_eq!((p.start, p.end), (d("2026-02-01"), d("2026-02-28")));
        assert!(Period::parse_month(" 2026-10 ").is_some());
        for bad in [
            "",
            "2026",
            "2026-1",
            "2026-13",
            "2026-00",
            "2026-W41",
            "2026-10-01",
            "x-10",
        ] {
            assert!(Period::parse_month(bad).is_none(), "{bad}");
        }
    }
}
