//! Managed-update schedule: local times of day in an IANA timezone, every day.
//!
//! A run is due when a trigger has passed since both the last run started and
//! the schedule last changed (so missed triggers are caught up once, but edits
//! only move the next run). A platform that has never run is due immediately.

use std::fmt;
use std::str::FromStr;

use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, Duration, LocalResult, NaiveDateTime, NaiveTime, Offset, TimeZone, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};

/// A local time of day with minute precision, serialised as `"HH:MM"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct TimeOfDay(NaiveTime);

impl FromStr for TimeOfDay {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        NaiveTime::parse_from_str(s, "%H:%M")
            .map(Self)
            .with_context(|| format!("invalid time {s:?}, expected HH:MM"))
    }
}

impl fmt::Display for TimeOfDay {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0.format("%H:%M"))
    }
}

impl TryFrom<String> for TimeOfDay {
    type Error = anyhow::Error;

    fn try_from(s: String) -> Result<Self> {
        s.parse()
    }
}

impl From<TimeOfDay> for String {
    fn from(t: TimeOfDay) -> Self {
        t.to_string()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Schedule {
    pub timezone: Tz,
    /// Sorted, without duplicates. Empty means manual runs only.
    pub times: Vec<TimeOfDay>,
    /// When the timezone or times last changed; earlier triggers are never caught up.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changed: Option<DateTime<Utc>>,
}

impl Default for Schedule {
    fn default() -> Self {
        Self {
            timezone: Tz::UTC,
            times: vec![TimeOfDay(NaiveTime::from_hms_opt(3, 0, 0).unwrap())],
            changed: None,
        }
    }
}

impl Schedule {
    /// Validates an IANA timezone name and `HH:MM` times; times are sorted and de-duplicated.
    pub fn parse(timezone: &str, times: &[impl AsRef<str>]) -> Result<Self> {
        let timezone = timezone
            .parse::<Tz>()
            .map_err(|_| anyhow!("unknown timezone {timezone:?}"))?;
        let mut times = times
            .iter()
            .map(|t| t.as_ref().parse())
            .collect::<Result<Vec<TimeOfDay>>>()?;
        times.sort();
        times.dedup();
        Ok(Self {
            timezone,
            times,
            changed: None,
        })
    }

    /// Adopts `next`'s timezone and times, recording `now` as the change time if they differ.
    pub fn set(&mut self, next: Schedule, now: DateTime<Utc>) {
        if self.timezone != next.timezone || self.times != next.times {
            *self = Schedule {
                changed: Some(now),
                ..next
            };
        }
    }

    /// Whether a run should start now, given when the last run started.
    pub fn is_due(&self, last_started: Option<DateTime<Utc>>, now: DateTime<Utc>) -> bool {
        let Some(trigger) = self.latest_trigger(now) else {
            return false;
        };
        match last_started {
            None => true,
            Some(started) => trigger > started && self.changed.is_none_or(|c| trigger > c),
        }
    }

    /// When the next run will start: `now` if one is due, else the next trigger.
    pub fn next_run(
        &self,
        last_started: Option<DateTime<Utc>>,
        now: DateTime<Utc>,
    ) -> Option<DateTime<Utc>> {
        if self.is_due(last_started, now) {
            Some(now)
        } else {
            self.next_trigger(now)
        }
    }

    /// Most recent trigger at or before `now`.
    pub fn latest_trigger(&self, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        self.triggers_around(now).filter(|t| *t <= now).max()
    }

    /// First trigger strictly after `now`.
    pub fn next_trigger(&self, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        self.triggers_around(now).filter(|t| *t > now).min()
    }

    /// Triggers on the local days before, of and after `now`.
    fn triggers_around(&self, now: DateTime<Utc>) -> impl Iterator<Item = DateTime<Utc>> + '_ {
        let today = now.with_timezone(&self.timezone).date_naive();
        (-1..=1).flat_map(move |offset| {
            let date = today + Duration::days(offset);
            self.times
                .iter()
                .map(move |t| resolve(self.timezone, date.and_time(t.0)))
        })
    }
}

/// Maps a wall-clock time to an instant. In a DST overlap the earlier instant
/// wins; in a DST gap the time is shifted forward by the gap's length.
fn resolve(tz: Tz, local: NaiveDateTime) -> DateTime<Utc> {
    match tz.from_local_datetime(&local) {
        LocalResult::Single(t) | LocalResult::Ambiguous(t, _) => t.to_utc(),
        LocalResult::None => {
            // Read the time with the offset in effect before the gap.
            let before = tz.offset_from_utc_datetime(&(local - Duration::days(1)));
            let secs = before.fix().local_minus_utc() as i64;
            (local - Duration::seconds(secs)).and_utc()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    fn sched(tz: &str, times: &[&str]) -> Schedule {
        Schedule::parse(tz, times).unwrap()
    }

    #[test]
    fn time_of_day_parsing() {
        assert_eq!("16:30".parse::<TimeOfDay>().unwrap().to_string(), "16:30");
        for bad in ["24:00", "12:60", "2pm", "02:00:00", "", "0200"] {
            assert!(bad.parse::<TimeOfDay>().is_err(), "{bad:?} should fail");
        }
        let t: TimeOfDay = serde_json::from_str(r#""03:05""#).unwrap();
        assert_eq!(serde_json::to_string(&t).unwrap(), r#""03:05""#);
        assert!(serde_json::from_str::<TimeOfDay>(r#""nope""#).is_err());
    }

    #[test]
    fn parse_validates_sorts_and_dedups() {
        let s = sched("Europe/London", &["16:00", "02:00", "16:00"]);
        assert_eq!(s.timezone, Tz::Europe__London);
        let times: Vec<String> = s.times.iter().map(|t| t.to_string()).collect();
        assert_eq!(times, ["02:00", "16:00"]);
        // Legacy aliases (as some browsers report) are accepted and keep their name.
        assert_eq!(sched("Asia/Calcutta", &[]).timezone.name(), "Asia/Calcutta");
        assert!(Schedule::parse("Mars/Olympus", &["02:00"]).is_err());
        assert!(Schedule::parse("UTC", &["2am"]).is_err());
        assert!(
            Schedule::parse("UTC", &[] as &[&str])
                .unwrap()
                .times
                .is_empty()
        );
    }

    #[test]
    fn serde_shape_and_defaults() {
        let json = serde_json::to_value(Schedule::default()).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"timezone": "UTC", "times": ["03:00"]})
        );
        let s: Schedule = serde_json::from_str(r#"{"timezone":"Asia/Tokyo"}"#).unwrap();
        assert_eq!(s.timezone, Tz::Asia__Tokyo);
        assert_eq!(s.times, Schedule::default().times);
        assert!(serde_json::from_str::<Schedule>(r#"{"timezone":"Nowhere"}"#).is_err());
    }

    #[test]
    fn triggers_across_days() {
        let s = sched("UTC", &["02:00", "16:00"]);
        let now = at("2026-11-20T10:00:00Z");
        assert_eq!(s.latest_trigger(now), Some(at("2026-11-20T02:00:00Z")));
        assert_eq!(s.next_trigger(now), Some(at("2026-11-20T16:00:00Z")));
        let late = at("2026-11-20T17:00:00Z");
        assert_eq!(s.next_trigger(late), Some(at("2026-11-21T02:00:00Z")));
        let early = at("2026-11-20T01:00:00Z");
        assert_eq!(s.latest_trigger(early), Some(at("2026-11-19T16:00:00Z")));
        // A trigger at exactly `now` is the latest, not the next.
        let exact = at("2026-11-20T16:00:00Z");
        assert_eq!(s.latest_trigger(exact), Some(exact));
        assert_eq!(s.next_trigger(exact), Some(at("2026-11-21T02:00:00Z")));
    }

    #[test]
    fn triggers_use_timezone_and_dst() {
        let ny = sched("America/New_York", &["02:00"]);
        let now = at("2026-01-15T12:00:00Z");
        assert_eq!(ny.latest_trigger(now), Some(at("2026-01-15T07:00:00Z")));
        // London springs forward 01:00 -> 02:00 on 2026-03-29: 01:30 becomes 02:30 BST.
        let gap = sched("Europe/London", &["01:30"]);
        let now = at("2026-03-29T12:00:00Z");
        assert_eq!(gap.latest_trigger(now), Some(at("2026-03-29T01:30:00Z")));
        // London falls back 02:00 -> 01:00 on 2026-10-25: 01:30 fires at the first one.
        let overlap = sched("Europe/London", &["01:30"]);
        let now = at("2026-10-25T12:00:00Z");
        assert_eq!(
            overlap.latest_trigger(now),
            Some(at("2026-10-25T00:30:00Z"))
        );
    }

    #[test]
    fn empty_schedule_never_runs() {
        let s = sched("UTC", &[]);
        let now = at("2026-11-20T10:00:00Z");
        assert_eq!(s.latest_trigger(now), None);
        assert!(!s.is_due(None, now));
        assert_eq!(s.next_run(None, now), None);
    }

    #[test]
    fn due_rules() {
        let mut s = sched("UTC", &["02:00", "16:00"]);
        let now = at("2026-11-20T10:00:00Z");
        assert!(s.is_due(None, now), "never run");
        assert!(
            s.is_due(Some(at("2026-11-19T16:00:00Z")), now),
            "missed 02:00"
        );
        assert!(!s.is_due(Some(at("2026-11-20T02:00:30Z")), now));
        assert_eq!(s.next_run(None, now), Some(now));
        assert_eq!(
            s.next_run(Some(at("2026-11-20T02:00:30Z")), now),
            Some(at("2026-11-20T16:00:00Z"))
        );
        // A trigger before the schedule changed is not caught up.
        s.changed = Some(at("2026-11-20T09:00:00Z"));
        assert!(!s.is_due(Some(at("2026-11-19T16:00:00Z")), now));
        s.changed = Some(at("2026-11-20T01:00:00Z"));
        assert!(s.is_due(Some(at("2026-11-19T16:00:00Z")), now));
    }

    #[test]
    fn set_records_changes_only() {
        let now = at("2026-11-20T15:00:00Z");
        let mut s = sched("UTC", &["03:00"]);
        s.set(sched("UTC", &["03:00"]), now);
        assert_eq!(s.changed, None);
        s.set(sched("UTC", &["03:00", "14:00"]), now);
        assert_eq!(s.changed, Some(now));
        // Adding 14:00 at 15:00 does not start a run; it waits for tomorrow.
        let last = Some(at("2026-11-20T03:00:10Z"));
        assert!(!s.is_due(last, now));
        assert_eq!(s.next_run(last, now), Some(at("2026-11-21T03:00:00Z")));
        let later = at("2026-11-20T16:00:00Z");
        s.set(sched("Europe/London", &["03:00", "14:00"]), later);
        assert_eq!(s.changed, Some(later));
    }
}
