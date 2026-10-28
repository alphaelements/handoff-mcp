//! Working-calendar model shared by auto-scheduling and capacity reporting.
//!
//! A [`Calendar`] answers two questions for a date: is it a work day, and how
//! many working hours does it offer. Project-level and per-assignee settings are
//! read from `config.toml` (`[calendar]` and `[assignees.<key>]`).

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use chrono::{Datelike, Duration, NaiveDate};
use toml_edit::{DocumentMut, Table};

use crate::storage::config::weekday_to_num;

/// Weekday names indexed 0=Sun..6=Sat, as used by `day_hours` keys.
pub const WEEKDAY_NAMES: [&str; 7] = ["sun", "mon", "tue", "wed", "thu", "fri", "sat"];

/// Remaining capacity at or below this many hours is treated as an exhausted day.
pub const EPSILON: f64 = 0.01;

/// Upper bound on days scanned by [`advance_by_hours`]; guards against a
/// zero-capacity calendar, which would otherwise loop forever.
const MAX_ADVANCE_DAYS: u32 = 10_000;

/// Default working hours per day when the config does not say otherwise.
const DEFAULT_WORK_HOURS_PER_DAY: f64 = 8.0;

/// Default `max_utilization` (percent): auto-scheduling may fill a day up to
/// this share of its working hours.
pub const DEFAULT_MAX_UTILIZATION: f64 = 120.0;

/// Default `overwork_limit_percent`: ceiling for how far a day's capacity may be
/// stretched in capacity reports.
pub const DEFAULT_OVERWORK_LIMIT_PERCENT: f64 = 150.0;

/// Default non-working weekdays (Sun, Sat).
const DEFAULT_CLOSED_WEEKDAYS: [u32; 2] = [0, 6];

#[derive(Debug, Clone)]
pub struct Calendar {
    pub work_hours_per_day: f64,
    pub closed_weekdays: Vec<u32>,
    pub closed_dates: Vec<String>,
    pub open_dates: Vec<String>,
    /// Per-weekday-name or per-YYYY-MM-DD working-hour overrides.
    pub day_hours: HashMap<String, f64>,
    /// Auto-schedule packing ceiling as a percentage of a day's working hours.
    pub max_utilization: f64,
    /// Capacity-report overwork ceiling as a percentage of a day's working hours.
    pub overwork_limit_percent: f64,
}

impl Default for Calendar {
    fn default() -> Self {
        Self {
            work_hours_per_day: DEFAULT_WORK_HOURS_PER_DAY,
            closed_weekdays: DEFAULT_CLOSED_WEEKDAYS.to_vec(),
            closed_dates: Vec::new(),
            open_dates: Vec::new(),
            day_hours: HashMap::new(),
            max_utilization: DEFAULT_MAX_UTILIZATION,
            overwork_limit_percent: DEFAULT_OVERWORK_LIMIT_PERCENT,
        }
    }
}

impl Calendar {
    /// Effective working hours for a specific date. A date-specific override in
    /// `day_hours` takes precedence over a weekday-name override, which takes
    /// precedence over `work_hours_per_day`. Does not consult closed days; see
    /// [`Calendar::capacity_for`] for that.
    pub fn hours_for(&self, date: &NaiveDate) -> f64 {
        let date_str = date.format("%Y-%m-%d").to_string();
        if let Some(h) = self.day_hours.get(&date_str) {
            return *h;
        }
        let name = WEEKDAY_NAMES[weekday_index(date) as usize];
        if let Some(h) = self.day_hours.get(name) {
            return *h;
        }
        self.work_hours_per_day
    }

    /// Hours auto-schedule may pack into a date: [`Calendar::hours_for`] scaled
    /// by `max_utilization`. Does not consult closed days.
    pub fn effective_capacity(&self, date: &NaiveDate) -> f64 {
        self.hours_for(date) * self.max_utilization / 100.0
    }

    /// Upper bound of hours a date may carry in capacity reports before it is
    /// considered over-allocated: [`Calendar::capacity_for`] scaled by
    /// `overwork_limit_percent`.
    pub fn overwork_limit_hours(&self, date: &NaiveDate) -> f64 {
        self.capacity_for(date) * self.overwork_limit_percent / 100.0
    }

    pub fn is_work_day(&self, date: &NaiveDate) -> bool {
        let date_str = date.format("%Y-%m-%d").to_string();

        if self.closed_dates.contains(&date_str) {
            return false;
        }

        if self.open_dates.contains(&date_str) {
            return true;
        }

        !self.closed_weekdays.contains(&weekday_index(date))
    }

    /// Capacity of a date for reporting: an explicit per-date `day_hours`
    /// override always wins, otherwise a closed day offers 0 hours and an open
    /// day offers [`Calendar::hours_for`].
    pub fn capacity_for(&self, date: &NaiveDate) -> f64 {
        let date_str = date.format("%Y-%m-%d").to_string();
        if let Some(h) = self.day_hours.get(&date_str) {
            return *h;
        }
        if !self.is_work_day(date) {
            return 0.0;
        }
        self.hours_for(date)
    }
}

/// Weekday index with 0=Sun..6=Sat (chrono itself is Mon=0).
pub fn weekday_index(date: &NaiveDate) -> u32 {
    match date.weekday() {
        chrono::Weekday::Sun => 0,
        chrono::Weekday::Mon => 1,
        chrono::Weekday::Tue => 2,
        chrono::Weekday::Wed => 3,
        chrono::Weekday::Thu => 4,
        chrono::Weekday::Fri => 5,
        chrono::Weekday::Sat => 6,
    }
}

pub fn next_work_day(from: NaiveDate, cal: &Calendar) -> NaiveDate {
    let mut date = from;
    while !cal.is_work_day(&date) {
        date += Duration::days(1);
    }
    date
}

/// Advance from `start` (assumed to be a work day) consuming `hours` of effort.
/// The first day offers `initial_capacity` hours (the unused remainder of a day
/// already partly filled by a previous task); each later work day offers
/// `cal.effective_capacity(date)`, so a half-capacity Friday makes a task take
/// an extra day. Returns the last work day the task occupies and the capacity still free
/// on that day, with remainders below [`EPSILON`] clamped to 0.0.
/// (referral ref-...004309 §5)
pub fn advance_by_hours(
    start: NaiveDate,
    hours: f64,
    initial_capacity: f64,
    cal: &Calendar,
) -> (NaiveDate, f64) {
    let mut date = start;
    let mut remaining = hours - initial_capacity.max(0.0);
    let mut guard = 0;
    while remaining > 1e-9 && guard < MAX_ADVANCE_DAYS {
        date = next_work_day(date + Duration::days(1), cal);
        remaining -= cal.effective_capacity(&date).max(0.0);
        guard += 1;
    }
    let free = -remaining;
    (date, if free < EPSILON { 0.0 } else { free })
}

fn read_doc(config_path: &Path) -> Result<DocumentMut> {
    let raw = std::fs::read_to_string(config_path).with_context(|| "Failed to read config")?;
    raw.parse().with_context(|| "Failed to parse config")
}

fn number(item: &toml_edit::Item) -> Option<f64> {
    item.as_float()
        .or_else(|| item.as_integer().map(|i| i as f64))
}

fn string_array(table: &Table, key: &str) -> Option<Vec<String>> {
    table.get(key).and_then(|v| v.as_array()).map(|arr| {
        arr.iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect()
    })
}

fn weekday_array(table: &Table, key: &str) -> Option<Vec<u32>> {
    table.get(key).and_then(|v| v.as_array()).map(|arr| {
        arr.iter()
            .filter_map(|v| {
                v.as_integer()
                    .map(|i| i as u32)
                    .or_else(|| v.as_str().and_then(weekday_to_num))
            })
            .collect()
    })
}

/// Parse a `[*.day_hours]` table into a map of weekday-name/date -> hours.
pub fn parse_day_hours(table: &Table) -> HashMap<String, f64> {
    table
        .iter()
        .filter_map(|(key, item)| {
            item.as_value()
                .and_then(|v| v.as_float().or_else(|| v.as_integer().map(|i| i as f64)))
                .map(|h| (key.to_string(), h))
        })
        .collect()
}

/// Apply `max_utilization` (> 0) and `overwork_limit_percent` (>= 100) from a
/// config table; out-of-range values are ignored so the inherited value stays.
fn apply_percentages(cal: &mut Calendar, table: &Table) {
    if let Some(p) = table
        .get("max_utilization")
        .and_then(number)
        .filter(|p| *p > 0.0)
    {
        cal.max_utilization = p;
    }
    if let Some(p) = table
        .get("overwork_limit_percent")
        .and_then(number)
        .filter(|p| *p >= 100.0)
    {
        cal.overwork_limit_percent = p;
    }
}

fn project_calendar_from_doc(doc: &DocumentMut) -> Calendar {
    let mut cal = Calendar::default();
    if let Some(calendar) = doc.get("calendar").and_then(|v| v.as_table()) {
        if let Some(h) = calendar.get("work_hours_per_day").and_then(number) {
            cal.work_hours_per_day = h;
        }
        if let Some(w) = weekday_array(calendar, "closed_weekdays") {
            cal.closed_weekdays = w;
        }
        if let Some(d) = string_array(calendar, "closed_dates") {
            cal.closed_dates = d;
        }
        if let Some(d) = string_array(calendar, "open_dates") {
            cal.open_dates = d;
        }
        if let Some(dh) = calendar.get("day_hours").and_then(|v| v.as_table()) {
            cal.day_hours = parse_day_hours(dh);
        }
        apply_percentages(&mut cal, calendar);
    }
    cal
}

/// Layer one `[assignees.<key>]` table over `base`: scalar settings replace,
/// closed/open dates append, `day_hours` override key-by-key.
fn apply_assignee(base: &Calendar, a: &Table) -> Calendar {
    let mut cal = base.clone();
    if let Some(h) = a.get("work_hours_per_day").and_then(number) {
        cal.work_hours_per_day = h;
    }
    if let Some(w) = weekday_array(a, "closed_weekdays") {
        cal.closed_weekdays = w;
    }
    if let Some(d) = string_array(a, "closed_dates") {
        cal.closed_dates.extend(d);
    }
    if let Some(d) = string_array(a, "open_dates") {
        cal.open_dates.extend(d);
    }
    if let Some(dh) = a.get("day_hours").and_then(|v| v.as_table()) {
        cal.day_hours.extend(parse_day_hours(dh));
    }
    cal
}

/// Project-level calendar from `config.toml`; defaults when the file is absent.
pub fn parse_project_calendar(config_path: &Path) -> Result<Calendar> {
    if !config_path.exists() {
        return Ok(Calendar::default());
    }
    Ok(project_calendar_from_doc(&read_doc(config_path)?))
}

/// Calendar per `[assignees.<key>]`, each layered over the project calendar.
pub fn parse_assignee_calendars(config_path: &Path) -> Result<HashMap<String, Calendar>> {
    let mut result = HashMap::new();
    if !config_path.exists() {
        return Ok(result);
    }
    let doc = read_doc(config_path)?;
    let base = project_calendar_from_doc(&doc);
    if let Some(assignees) = doc.get("assignees").and_then(|v| v.as_table()) {
        for (key, item) in assignees.iter() {
            if let Some(a) = item.as_table() {
                result.insert(key.to_string(), apply_assignee(&base, a));
            }
        }
    }
    Ok(result)
}

/// Calendar for one assignee (or the project calendar when `assignee` is
/// `None` or has no `[assignees.<key>]` table).
pub fn parse_calendar_for(config_path: &Path, assignee: Option<&str>) -> Result<Calendar> {
    if !config_path.exists() {
        return Ok(Calendar::default());
    }
    let doc = read_doc(config_path)?;
    let base = project_calendar_from_doc(&doc);
    let table = assignee.and_then(|key| {
        doc.get("assignees")
            .and_then(|v| v.as_table())
            .and_then(|t| t.get(key))
            .and_then(|v| v.as_table())
    });
    Ok(match table {
        Some(a) => apply_assignee(&base, a),
        None => base,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    // 2026-06-22 is a Monday; 2026-06-26 a Friday.
    const MON: &str = "2026-06-22";
    const FRI: &str = "2026-06-26";

    /// Calendar with a strict 100% utilization cap, so day capacity equals the
    /// raw working hours (keeps the packing arithmetic below easy to read).
    fn cal() -> Calendar {
        Calendar {
            max_utilization: 100.0,
            ..Calendar::default()
        }
    }

    fn assert_close(actual: f64, expected: f64) {
        assert!((actual - expected).abs() < 1e-9, "{actual} != {expected}");
    }

    #[test]
    fn half_hour_task_leaves_seven_and_a_half() {
        let (date, free) = advance_by_hours(d(MON), 0.5, 8.0, &cal());
        assert_eq!(date, d(MON));
        assert_close(free, 7.5);
    }

    #[test]
    fn full_day_task_leaves_nothing() {
        let (date, free) = advance_by_hours(d(MON), 8.0, 8.0, &cal());
        assert_eq!(date, d(MON));
        assert_close(free, 0.0);
    }

    #[test]
    fn twelve_hours_spills_to_next_day() {
        let (date, free) = advance_by_hours(d(MON), 12.0, 8.0, &cal());
        assert_eq!(date, d("2026-06-23"));
        assert_close(free, 4.0);
    }

    #[test]
    fn packing_into_partly_used_day() {
        let (date, free) = advance_by_hours(d(MON), 0.5, 7.5, &cal());
        assert_eq!(date, d(MON));
        assert_close(free, 7.0);
    }

    #[test]
    fn exhausted_start_day_moves_to_next_work_day() {
        let (date, free) = advance_by_hours(d(MON), 3.0, 0.0, &cal());
        assert_eq!(date, d("2026-06-23"));
        assert_close(free, 5.0);
    }

    #[test]
    fn day_hours_friday_override_extends_task() {
        let mut c = cal();
        c.day_hours.insert("fri".to_string(), 4.0);
        // Friday offers 4h, then weekend skipped, Monday offers 8h.
        let (date, free) = advance_by_hours(d(FRI), 6.0, c.hours_for(&d(FRI)), &c);
        assert_eq!(date, d("2026-06-29"));
        assert_close(free, 6.0);
        // A task fitting in Friday's 4h stays on Friday.
        let (date, free) = advance_by_hours(d(FRI), 3.0, c.hours_for(&d(FRI)), &c);
        assert_eq!(date, d(FRI));
        assert_close(free, 1.0);
    }

    #[test]
    fn date_specific_override_beats_weekday_override() {
        let mut c = cal();
        c.day_hours.insert("fri".to_string(), 4.0);
        c.day_hours.insert(FRI.to_string(), 2.0);
        assert_close(c.hours_for(&d(FRI)), 2.0);
        assert_close(c.hours_for(&d("2026-07-03")), 4.0);
        assert_close(c.hours_for(&d(MON)), 8.0);
    }

    #[test]
    fn remainder_below_epsilon_clamps_to_zero() {
        let (date, free) = advance_by_hours(d(MON), 7.996, 8.0, &cal());
        assert_eq!(date, d(MON));
        assert_close(free, 0.0);
        let (_, free) = advance_by_hours(d(MON), 0.005, 0.01, &cal());
        assert_close(free, 0.0);
    }

    #[test]
    fn remainder_at_or_above_epsilon_is_kept() {
        let (_, free) = advance_by_hours(d(MON), 7.9, 8.0, &cal());
        assert_close(free, 0.1);
    }

    #[test]
    fn zero_capacity_calendar_terminates() {
        let mut c = cal();
        c.work_hours_per_day = 0.0;
        let (_, free) = advance_by_hours(d(MON), 1.0, 0.0, &c);
        assert_close(free, 0.0);
    }

    #[test]
    fn capacity_for_closed_day_is_zero_but_date_override_wins() {
        let mut c = cal();
        let sat = d("2026-06-27");
        assert_close(c.capacity_for(&sat), 0.0);
        assert_close(c.capacity_for(&d(MON)), 8.0);
        c.day_hours.insert("2026-06-27".to_string(), 3.0);
        assert_close(c.capacity_for(&sat), 3.0);
    }

    #[test]
    fn capacity_for_open_date_overrides_closed_weekday() {
        let mut c = cal();
        c.open_dates.push("2026-06-27".to_string());
        assert_close(c.capacity_for(&d("2026-06-27")), 8.0);
        c.closed_dates.push(MON.to_string());
        assert_close(c.capacity_for(&d(MON)), 0.0);
    }

    fn write_config(body: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, body).unwrap();
        (dir, path)
    }

    const CONFIG: &str = r#"
[calendar]
work_hours_per_day = 6
closed_weekdays = ["sat", "sun"]
closed_dates = ["2026-07-01"]

[calendar.day_hours]
fri = 4

[assignees.alice]
work_hours_per_day = 5.5
closed_dates = ["2026-07-02"]

[assignees.alice.day_hours]
fri = 2
"#;

    #[test]
    fn parses_project_calendar() {
        let (_dir, path) = write_config(CONFIG);
        let c = parse_project_calendar(&path).unwrap();
        assert_close(c.work_hours_per_day, 6.0);
        assert_eq!(c.closed_weekdays, vec![6, 0]);
        assert_eq!(c.closed_dates, vec!["2026-07-01".to_string()]);
        assert_close(c.day_hours["fri"], 4.0);
    }

    #[test]
    fn missing_config_yields_default_calendar() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let c = parse_project_calendar(&path).unwrap();
        assert_close(c.work_hours_per_day, 8.0);
        assert_eq!(c.closed_weekdays, vec![0, 6]);
        assert!(parse_assignee_calendars(&path).unwrap().is_empty());
        assert_close(
            parse_calendar_for(&path, Some("x"))
                .unwrap()
                .work_hours_per_day,
            8.0,
        );
    }

    #[test]
    fn assignee_calendar_layers_over_project() {
        let (_dir, path) = write_config(CONFIG);
        let all = parse_assignee_calendars(&path).unwrap();
        let alice = &all["alice"];
        assert_close(alice.work_hours_per_day, 5.5);
        // inherited from project
        assert_eq!(alice.closed_weekdays, vec![6, 0]);
        // appended
        assert_eq!(alice.closed_dates.len(), 2);
        // assignee day_hours wins over project key-by-key
        assert_close(alice.day_hours["fri"], 2.0);

        let single = parse_calendar_for(&path, Some("alice")).unwrap();
        assert_close(single.day_hours["fri"], 2.0);
        assert_eq!(single.closed_dates.len(), 2);
    }

    #[test]
    fn unknown_assignee_falls_back_to_project_calendar() {
        let (_dir, path) = write_config(CONFIG);
        let c = parse_calendar_for(&path, Some("bob")).unwrap();
        assert_close(c.work_hours_per_day, 6.0);
        assert_close(c.day_hours["fri"], 4.0);
        let c = parse_calendar_for(&path, None).unwrap();
        assert_close(c.work_hours_per_day, 6.0);
    }

    #[test]
    fn default_calendar_uses_120_percent_utilization_and_150_overwork() {
        let c = Calendar::default();
        assert_close(c.max_utilization, 120.0);
        assert_close(c.overwork_limit_percent, 150.0);
        assert_close(c.effective_capacity(&d(MON)), 9.6);
        assert_close(c.overwork_limit_hours(&d(MON)), 12.0);
    }

    #[test]
    fn effective_capacity_scales_hours_for_by_max_utilization() {
        let mut c = cal();
        c.max_utilization = 50.0;
        assert_close(c.effective_capacity(&d(MON)), 4.0);
        c.day_hours.insert("fri".to_string(), 4.0);
        assert_close(c.effective_capacity(&d(FRI)), 2.0);
    }

    #[test]
    fn advance_by_hours_uses_effective_capacity_for_later_days() {
        let mut c = cal();
        c.max_utilization = 50.0;
        // 8h: first day offers 4h, then two more 4h days -> ends on Wednesday.
        let (date, free) = advance_by_hours(d(MON), 12.0, c.effective_capacity(&d(MON)), &c);
        assert_eq!(date, d("2026-06-24"));
        assert_close(free, 0.0);
        // Default 120%: 12h = 9.6h + 2.4h, leaving 7.2h on Tuesday.
        let c = Calendar::default();
        let (date, free) = advance_by_hours(d(MON), 12.0, c.effective_capacity(&d(MON)), &c);
        assert_eq!(date, d("2026-06-23"));
        assert_close(free, 7.2);
    }

    #[test]
    fn parses_max_utilization_and_overwork_limit() {
        let (_dir, path) = write_config(
            "[calendar]\nmax_utilization = 50\noverwork_limit_percent = 200\n\n[assignees.alice]\nwork_hours_per_day = 6\n",
        );
        let c = parse_project_calendar(&path).unwrap();
        assert_close(c.max_utilization, 50.0);
        assert_close(c.overwork_limit_percent, 200.0);
        // assignee calendars inherit both from the project calendar
        let alice = &parse_assignee_calendars(&path).unwrap()["alice"];
        assert_close(alice.max_utilization, 50.0);
        assert_close(alice.overwork_limit_percent, 200.0);
        let single = parse_calendar_for(&path, Some("alice")).unwrap();
        assert_close(single.max_utilization, 50.0);
    }

    #[test]
    fn unset_or_invalid_utilization_keeps_defaults() {
        let (_dir, path) =
            write_config("[calendar]\nmax_utilization = 0\noverwork_limit_percent = 50\n");
        let c = parse_project_calendar(&path).unwrap();
        assert_close(c.max_utilization, 120.0);
        assert_close(c.overwork_limit_percent, 150.0);
        let (_dir, path) = write_config("[calendar]\nwork_hours_per_day = 6\n");
        assert_close(
            parse_project_calendar(&path).unwrap().max_utilization,
            120.0,
        );
    }
}
