//! Calendar-style recurring task schedule contract and occurrence evaluation.

use std::{collections::HashSet, str::FromStr};

use chrono::{
    DateTime, Datelike, Duration, LocalResult, NaiveDate, NaiveDateTime, TimeZone, Utc, Weekday,
};
use chrono_tz::Tz;
use cron::Schedule;
use serde::{Deserialize, Serialize};
use thiserror::Error;

const MAX_CALENDAR_SEARCH_DAYS: i64 = 366 * 200;

/// Complete timezone-aware definition for one recurring task series.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskScheduleDefinition {
    pub starts_at: DateTime<Utc>,
    pub timezone: String,
    pub recurrence: TaskScheduleRecurrence,
    #[serde(default)]
    pub end: TaskScheduleEnd,
}

/// Supported recurrence families. Cron remains an advanced escape hatch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "frequency", rename_all = "snake_case")]
pub enum TaskScheduleRecurrence {
    Interval {
        every: u32,
        unit: TaskScheduleIntervalUnit,
    },
    Daily {
        interval: u32,
    },
    Weekly {
        interval: u32,
        weekdays: Vec<TaskScheduleWeekday>,
    },
    Monthly {
        interval: u32,
        day_of_month: u8,
    },
    Yearly {
        interval: u32,
        month: u8,
        day: u8,
    },
    Cron {
        expression: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskScheduleIntervalUnit {
    Seconds,
    Minutes,
    Hours,
    Days,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskScheduleWeekday {
    Monday,
    Tuesday,
    Wednesday,
    Thursday,
    Friday,
    Saturday,
    Sunday,
}

impl TaskScheduleWeekday {
    const fn chrono(self) -> Weekday {
        match self {
            Self::Monday => Weekday::Mon,
            Self::Tuesday => Weekday::Tue,
            Self::Wednesday => Weekday::Wed,
            Self::Thursday => Weekday::Thu,
            Self::Friday => Weekday::Fri,
            Self::Saturday => Weekday::Sat,
            Self::Sunday => Weekday::Sun,
        }
    }
}

/// Boundary for a recurring series.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TaskScheduleEnd {
    #[default]
    Never,
    On {
        date: NaiveDate,
    },
    After {
        occurrences: u32,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TaskScheduleDefinitionError {
    #[error("schedule timezone must be a valid IANA timezone")]
    InvalidTimezone,
    #[error("schedule interval must be greater than zero")]
    ZeroInterval,
    #[error("weekly schedules require at least one weekday")]
    MissingWeekdays,
    #[error("weekly schedule weekdays must be unique")]
    DuplicateWeekday,
    #[error("monthly day must be between 1 and 31")]
    InvalidDayOfMonth,
    #[error("yearly month and day must form a valid calendar date")]
    InvalidAnnualDate,
    #[error("advanced cron expression is invalid: {0}")]
    InvalidCron(String),
    #[error("series occurrence limit must be greater than zero")]
    ZeroOccurrenceLimit,
    #[error("series end date cannot be before the first occurrence")]
    EndBeforeStart,
    #[error("structured recurrence does not include its start date")]
    StartDoesNotMatch,
    #[error("schedule has no occurrence within the supported calendar range")]
    NoFutureOccurrence,
    #[error("schedule occurrence overflowed")]
    Overflow,
}

impl TaskScheduleDefinition {
    /// Validate the complete recurrence at the transport boundary.
    pub fn validate(&self) -> Result<(), TaskScheduleDefinitionError> {
        let timezone = self.timezone()?;
        let interval = match &self.recurrence {
            TaskScheduleRecurrence::Interval { every, .. } => *every,
            TaskScheduleRecurrence::Daily { interval }
            | TaskScheduleRecurrence::Weekly { interval, .. }
            | TaskScheduleRecurrence::Monthly { interval, .. }
            | TaskScheduleRecurrence::Yearly { interval, .. } => *interval,
            TaskScheduleRecurrence::Cron { .. } => 1,
        };
        if interval == 0 {
            return Err(TaskScheduleDefinitionError::ZeroInterval);
        }
        match &self.recurrence {
            TaskScheduleRecurrence::Weekly { weekdays, .. } => {
                if weekdays.is_empty() {
                    return Err(TaskScheduleDefinitionError::MissingWeekdays);
                }
                if weekdays.iter().copied().collect::<HashSet<_>>().len() != weekdays.len() {
                    return Err(TaskScheduleDefinitionError::DuplicateWeekday);
                }
            }
            TaskScheduleRecurrence::Monthly { day_of_month, .. }
                if !(1..=31).contains(day_of_month) =>
            {
                return Err(TaskScheduleDefinitionError::InvalidDayOfMonth);
            }
            TaskScheduleRecurrence::Yearly { month, day, .. }
                if NaiveDate::from_ymd_opt(2000, u32::from(*month), u32::from(*day)).is_none() =>
            {
                return Err(TaskScheduleDefinitionError::InvalidAnnualDate);
            }
            TaskScheduleRecurrence::Cron { expression } => {
                Schedule::from_str(expression)
                    .map_err(|error| TaskScheduleDefinitionError::InvalidCron(error.to_string()))?;
            }
            TaskScheduleRecurrence::Interval { .. }
            | TaskScheduleRecurrence::Daily { .. }
            | TaskScheduleRecurrence::Monthly { .. }
            | TaskScheduleRecurrence::Yearly { .. } => {}
        }
        if matches!(self.end, TaskScheduleEnd::After { occurrences: 0 }) {
            return Err(TaskScheduleDefinitionError::ZeroOccurrenceLimit);
        }
        let start_local = self.starts_at.with_timezone(&timezone);
        if let TaskScheduleEnd::On { date } = self.end
            && date < start_local.date_naive()
        {
            return Err(TaskScheduleDefinitionError::EndBeforeStart);
        }
        if matches!(
            self.recurrence,
            TaskScheduleRecurrence::Daily { .. }
                | TaskScheduleRecurrence::Weekly { .. }
                | TaskScheduleRecurrence::Monthly { .. }
                | TaskScheduleRecurrence::Yearly { .. }
        ) && !self.calendar_date_matches(start_local.date_naive(), start_local.date_naive())
        {
            return Err(TaskScheduleDefinitionError::StartDoesNotMatch);
        }
        Ok(())
    }

    /// First bounded occurrence strictly after `after`.
    ///
    /// `completed_occurrences` counts materialized runs in this series and is
    /// used by the `after` end condition.
    pub fn next_after(
        &self,
        after: DateTime<Utc>,
        completed_occurrences: u32,
    ) -> Result<Option<DateTime<Utc>>, TaskScheduleDefinitionError> {
        self.validate()?;
        if let TaskScheduleEnd::After { occurrences } = self.end
            && completed_occurrences >= occurrences
        {
            return Ok(None);
        }
        let candidate = match &self.recurrence {
            TaskScheduleRecurrence::Interval { every, unit } => {
                self.next_interval(after, *every, *unit)?
            }
            TaskScheduleRecurrence::Cron { expression } => self.next_cron(after, expression)?,
            TaskScheduleRecurrence::Daily { .. }
            | TaskScheduleRecurrence::Weekly { .. }
            | TaskScheduleRecurrence::Monthly { .. }
            | TaskScheduleRecurrence::Yearly { .. } => self.next_calendar(after)?,
        };
        let timezone = self.timezone()?;
        if let TaskScheduleEnd::On { date } = self.end
            && candidate.with_timezone(&timezone).date_naive() > date
        {
            return Ok(None);
        }
        Ok(Some(candidate))
    }

    /// Produce a bounded occurrence preview without mutating schedule state.
    pub fn preview(
        &self,
        after: DateTime<Utc>,
        completed_occurrences: u32,
        limit: usize,
    ) -> Result<Vec<DateTime<Utc>>, TaskScheduleDefinitionError> {
        let mut cursor = after;
        let mut completed = completed_occurrences;
        let mut values = Vec::with_capacity(limit);
        while values.len() < limit {
            let Some(next) = self.next_after(cursor, completed)? else {
                break;
            };
            values.push(next);
            cursor = next;
            completed = completed.saturating_add(1);
        }
        Ok(values)
    }

    fn timezone(&self) -> Result<Tz, TaskScheduleDefinitionError> {
        self.timezone
            .parse::<Tz>()
            .map_err(|_| TaskScheduleDefinitionError::InvalidTimezone)
    }

    fn next_interval(
        &self,
        after: DateTime<Utc>,
        every: u32,
        unit: TaskScheduleIntervalUnit,
    ) -> Result<DateTime<Utc>, TaskScheduleDefinitionError> {
        let unit_seconds = match unit {
            TaskScheduleIntervalUnit::Seconds => 1_i64,
            TaskScheduleIntervalUnit::Minutes => 60_i64,
            TaskScheduleIntervalUnit::Hours => 3_600,
            TaskScheduleIntervalUnit::Days => 86_400,
        };
        let step = unit_seconds
            .checked_mul(i64::from(every))
            .ok_or(TaskScheduleDefinitionError::Overflow)?;
        if after < self.starts_at {
            return Ok(self.starts_at);
        }
        let elapsed = after.signed_duration_since(self.starts_at).num_seconds();
        let jumps = elapsed / step + 1;
        self.starts_at
            .checked_add_signed(Duration::seconds(
                step.checked_mul(jumps)
                    .ok_or(TaskScheduleDefinitionError::Overflow)?,
            ))
            .ok_or(TaskScheduleDefinitionError::Overflow)
    }

    fn next_cron(
        &self,
        after: DateTime<Utc>,
        expression: &str,
    ) -> Result<DateTime<Utc>, TaskScheduleDefinitionError> {
        let timezone = self.timezone()?;
        let parsed = Schedule::from_str(expression)
            .map_err(|error| TaskScheduleDefinitionError::InvalidCron(error.to_string()))?;
        let lower_bound = after.max(
            self.starts_at
                .checked_sub_signed(Duration::nanoseconds(1))
                .unwrap_or(self.starts_at),
        );
        parsed
            .after(&lower_bound.with_timezone(&timezone))
            .next()
            .map(|value| value.with_timezone(&Utc))
            .ok_or(TaskScheduleDefinitionError::NoFutureOccurrence)
    }

    fn next_calendar(
        &self,
        after: DateTime<Utc>,
    ) -> Result<DateTime<Utc>, TaskScheduleDefinitionError> {
        let timezone = self.timezone()?;
        let start_local = self.starts_at.with_timezone(&timezone);
        let after_local = after.with_timezone(&timezone);
        let first_date = start_local.date_naive().max(after_local.date_naive());
        let start_date = start_local.date_naive();
        let local_time = start_local.time();
        for offset in 0..=MAX_CALENDAR_SEARCH_DAYS {
            let Some(date) = first_date.checked_add_signed(Duration::days(offset)) else {
                return Err(TaskScheduleDefinitionError::Overflow);
            };
            if !self.calendar_date_matches(start_date, date) {
                continue;
            }
            let local = NaiveDateTime::new(date, local_time);
            let candidate = local_occurrences(timezone, local)
                .into_iter()
                .next()
                .filter(|candidate| *candidate >= self.starts_at && *candidate > after);
            if let Some(candidate) = candidate {
                return Ok(candidate);
            }
        }
        Err(TaskScheduleDefinitionError::NoFutureOccurrence)
    }

    fn calendar_date_matches(&self, start: NaiveDate, candidate: NaiveDate) -> bool {
        if candidate < start {
            return false;
        }
        match &self.recurrence {
            TaskScheduleRecurrence::Daily { interval } => {
                candidate.signed_duration_since(start).num_days() % i64::from(*interval) == 0
            }
            TaskScheduleRecurrence::Weekly { interval, weekdays } => {
                let start_week =
                    start - Duration::days(i64::from(start.weekday().num_days_from_monday()));
                let candidate_week = candidate
                    - Duration::days(i64::from(candidate.weekday().num_days_from_monday()));
                let weeks = candidate_week.signed_duration_since(start_week).num_days() / 7;
                weeks % i64::from(*interval) == 0
                    && weekdays
                        .iter()
                        .any(|weekday| weekday.chrono() == candidate.weekday())
            }
            TaskScheduleRecurrence::Monthly {
                interval,
                day_of_month,
            } => {
                let months = i64::from(candidate.year() - start.year()) * 12
                    + i64::from(candidate.month())
                    - i64::from(start.month());
                months % i64::from(*interval) == 0 && candidate.day() == u32::from(*day_of_month)
            }
            TaskScheduleRecurrence::Yearly {
                interval,
                month,
                day,
            } => {
                (candidate.year() - start.year()) % i32::try_from(*interval).unwrap_or(i32::MAX)
                    == 0
                    && candidate.month() == u32::from(*month)
                    && candidate.day() == u32::from(*day)
            }
            TaskScheduleRecurrence::Interval { .. } | TaskScheduleRecurrence::Cron { .. } => false,
        }
    }
}

fn local_occurrences(timezone: Tz, local: NaiveDateTime) -> Vec<DateTime<Utc>> {
    match timezone.from_local_datetime(&local) {
        LocalResult::Single(value) => vec![value.with_timezone(&Utc)],
        LocalResult::Ambiguous(first, second) => {
            let mut occurrences = vec![first.with_timezone(&Utc), second.with_timezone(&Utc)];
            occurrences.sort_unstable();
            occurrences
        }
        LocalResult::None => shift_through_timezone_gap(timezone, local),
    }
}

fn shift_through_timezone_gap(timezone: Tz, local: NaiveDateTime) -> Vec<DateTime<Utc>> {
    let before = (1..=180).find(|minutes| {
        !matches!(
            timezone.from_local_datetime(&(local - Duration::minutes(*minutes))),
            LocalResult::None
        )
    });
    let after = (1..=180).find(|minutes| {
        !matches!(
            timezone.from_local_datetime(&(local + Duration::minutes(*minutes))),
            LocalResult::None
        )
    });
    let (Some(before), Some(after)) = (before, after) else {
        return Vec::new();
    };
    let shifted = local + Duration::minutes(before + after - 1);
    match timezone.from_local_datetime(&shifted) {
        LocalResult::Single(value) => vec![value.with_timezone(&Utc)],
        LocalResult::Ambiguous(first, second) => {
            let mut occurrences = vec![first.with_timezone(&Utc), second.with_timezone(&Utc)];
            occurrences.sort_unstable();
            occurrences
        }
        LocalResult::None => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};

    use super::*;

    fn definition(recurrence: TaskScheduleRecurrence) -> TaskScheduleDefinition {
        TaskScheduleDefinition {
            starts_at: Utc.with_ymd_and_hms(2026, 3, 2, 15, 0, 0).unwrap(),
            timezone: "America/Chicago".to_string(),
            recurrence,
            end: TaskScheduleEnd::Never,
        }
    }

    #[test]
    fn weekly_calendar_recurrence_preserves_local_time_across_dst() {
        let schedule = definition(TaskScheduleRecurrence::Weekly {
            interval: 1,
            weekdays: vec![TaskScheduleWeekday::Monday],
        });
        let occurrences = schedule
            .preview(Utc.with_ymd_and_hms(2026, 3, 1, 0, 0, 0).unwrap(), 0, 3)
            .unwrap();
        assert_eq!(
            occurrences[0],
            Utc.with_ymd_and_hms(2026, 3, 2, 15, 0, 0).unwrap()
        );
        assert_eq!(
            occurrences[1],
            Utc.with_ymd_and_hms(2026, 3, 9, 14, 0, 0).unwrap()
        );
    }

    #[test]
    fn every_two_weeks_supports_multiple_weekdays() {
        let schedule = definition(TaskScheduleRecurrence::Weekly {
            interval: 2,
            weekdays: vec![TaskScheduleWeekday::Monday, TaskScheduleWeekday::Thursday],
        });
        let occurrences = schedule
            .preview(Utc.with_ymd_and_hms(2026, 3, 1, 0, 0, 0).unwrap(), 0, 4)
            .unwrap();
        assert_eq!(
            occurrences[2],
            Utc.with_ymd_and_hms(2026, 3, 16, 14, 0, 0).unwrap()
        );
    }

    #[test]
    fn after_occurrence_limit_terminates_the_series() {
        let mut schedule = definition(TaskScheduleRecurrence::Daily { interval: 1 });
        schedule.end = TaskScheduleEnd::After { occurrences: 2 };
        assert_eq!(
            schedule
                .preview(Utc.with_ymd_and_hms(2026, 3, 1, 0, 0, 0).unwrap(), 0, 5,)
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn end_date_is_interpreted_in_the_schedule_timezone() {
        let mut schedule = definition(TaskScheduleRecurrence::Daily { interval: 1 });
        schedule.end = TaskScheduleEnd::On {
            date: NaiveDate::from_ymd_opt(2026, 3, 3).unwrap(),
        };
        assert_eq!(
            schedule
                .preview(Utc.with_ymd_and_hms(2026, 3, 1, 0, 0, 0).unwrap(), 0, 5,)
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn nonexistent_recurring_wall_time_shifts_by_the_dst_gap() {
        let schedule = TaskScheduleDefinition {
            starts_at: Utc.with_ymd_and_hms(2026, 3, 7, 8, 30, 0).unwrap(),
            timezone: "America/Chicago".to_string(),
            recurrence: TaskScheduleRecurrence::Daily { interval: 1 },
            end: TaskScheduleEnd::Never,
        };
        let occurrences = schedule
            .preview(Utc.with_ymd_and_hms(2026, 3, 7, 8, 30, 0).unwrap(), 1, 1)
            .unwrap();
        assert_eq!(
            occurrences[0],
            Utc.with_ymd_and_hms(2026, 3, 8, 8, 30, 0).unwrap()
        );
    }
}
