//
// © 2026 PLOMID Technology Solutions
//
// PLOMID
// Platform for Modern Intelligence and Data
//
// Author: Sainath Sapa
// GitHub: https://github.com/sainathsapa
//
// Licensed under the Apache License, Version 2.0;
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     https://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.
//! Date/time arithmetic in PostgreSQL's internal representations.
//!
//! Anchored at 2000-01-01 like PostgreSQL: `date` values are `i32` days since
//! 2000-01-01, timestamps are `i64` microseconds since 2000-01-01 00:00:00,
//! `time` is microseconds since midnight.

use std::fmt;

// Date/time constants are defined once in `plomid_core::constants`.
use plomid_core::MONTH_DAYS;
pub use plomid_core::{POSTGRES_EPOCH_JDATE, USECS_PER_DAY, USECS_PER_SEC};

/// True for Gregorian leap years.
#[must_use]
pub fn is_leap_year(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

/// Calendar date parts (year, month 1-12, day 1-31).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct DateParts {
    /// ISO year (0 = 1 BC).
    pub year: i32,
    /// Month 1..=12.
    pub month: u8,
    /// Day 1..=31.
    pub day: u8,
}

/// Number of days in a month for the given year.
#[must_use]
pub fn days_in_month(year: i32, month: u8) -> i32 {
    if month == 2 && is_leap_year(year) {
        29
    } else {
        MONTH_DAYS[usize::from(month) - 1]
    }
}

#[must_use]
fn valid_date(d: DateParts) -> bool {
    d.month >= 1 && d.month <= 12 && d.day >= 1 && d.day <= days_in_month(d.year, d.month) as u8
}

/// Converts days-since-2000-01-01 to calendar parts (proleptic Gregorian).
#[must_use]
pub fn date_to_parts(days: i32) -> DateParts {
    let z = days + POSTGRES_EPOCH_JDATE + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    if month <= 2 {
        year += 1;
    }
    DateParts {
        year,
        month: month as u8,
        day: day as u8,
    }
}

/// Converts calendar parts to days-since-2000-01-01.
#[must_use]
pub fn parts_to_date(p: DateParts) -> Option<i32> {
    if !valid_date(p) {
        return None;
    }
    let (y, m) = if p.month <= 2 {
        (p.year - 1, i32::from(p.month) + 9)
    } else {
        (p.year, i32::from(p.month) - 3)
    };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * m + 2) / 5 + i32::from(p.day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146_097 + doe - 719_468 - POSTGRES_EPOCH_JDATE)
}

/// Time-of-day parts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct TimeParts {
    /// Hour 0..=23.
    pub hour: u8,
    /// Minute 0..=59.
    pub minute: u8,
    /// Second 0..=59.
    pub second: u8,
    /// Microsecond 0..=999_999.
    pub micros: u32,
}

/// Splits microseconds-since-midnight into time parts.
#[must_use]
pub fn time_to_parts(micros: i64) -> TimeParts {
    let secs = micros / USECS_PER_SEC;
    TimeParts {
        hour: (secs / 3600) as u8,
        minute: ((secs / 60) % 60) as u8,
        second: (secs % 60) as u8,
        micros: micros.rem_euclid(USECS_PER_SEC) as u32,
    }
}

/// Combines time parts into microseconds-since-midnight.
#[must_use]
pub fn parts_to_time(p: TimeParts) -> Option<i64> {
    if p.hour > 23 || p.minute > 59 || p.second > 59 || p.micros > 999_999 {
        return None;
    }
    Some(
        i64::from(p.hour) * 3600 * USECS_PER_SEC
            + i64::from(p.minute) * 60 * USECS_PER_SEC
            + i64::from(p.second) * USECS_PER_SEC
            + i64::from(p.micros),
    )
}

/// Splits a timestamp (micros since 2000-01-01) into date + time parts.
#[must_use]
pub fn timestamp_to_parts(micros: i64) -> (DateParts, TimeParts) {
    let days = micros.div_euclid(USECS_PER_DAY);
    let time = micros.rem_euclid(USECS_PER_DAY);
    (date_to_parts(days as i32), time_to_parts(time))
}

/// Combines date + time parts into microseconds-since-2000-01-01.
#[must_use]
pub fn parts_to_timestamp(days: i32, time_micros: i64) -> i64 {
    i64::from(days) * USECS_PER_DAY + time_micros
}

/// A PostgreSQL interval: months + days + microseconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct Interval {
    /// Months component.
    pub months: i32,
    /// Days component.
    pub days: i32,
    /// Microseconds component.
    pub micros: i64,
}

impl fmt::Display for Interval {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let years = self.months / 12;
        let months = self.months % 12;
        if years != 0 {
            write!(f, "{years} year{}", if years == 1 { "" } else { "s" })?;
            if months != 0 || self.days != 0 || self.micros != 0 {
                write!(f, " ")?;
            }
        }
        if months != 0 {
            write!(f, "{months} mon{}", if months == 1 { "" } else { "s" })?;
            if self.days != 0 || self.micros != 0 {
                write!(f, " ")?;
            }
        }
        if self.days != 0 {
            write!(
                f,
                "{} day{}",
                self.days,
                if self.days == 1 { "" } else { "s" }
            )?;
            if self.micros != 0 {
                write!(f, " ")?;
            }
        }
        if self.micros != 0 || (self.months == 0 && self.days == 0) {
            let t = time_to_parts(self.micros);
            let secs = if t.micros != 0 {
                format!("{:02}.{:06}", t.second, t.micros)
            } else {
                format!("{:02}", t.second)
            };
            write!(f, "{:02}:{:02}:{secs}", t.hour, t.minute)?;
        }
        Ok(())
    }
}

/// Adds a PostgreSQL interval to a timestamp (micros since 2000-01-01).
/// Month arithmetic applies first against the calendar date (clamping the
/// day to the target month length, as PostgreSQL does), then days and
/// microseconds are added as fixed durations. `subtract` reverses the sign.
#[must_use]
pub fn add_interval_to_timestamp(ts_micros: i64, interval: Interval, subtract: bool) -> i64 {
    let sign: i64 = if subtract { -1 } else { 1 };
    let (date, time) = timestamp_to_parts(ts_micros);
    let day_micros = ts_micros.rem_euclid(USECS_PER_DAY);
    let mut days = ts_micros.div_euclid(USECS_PER_DAY) as i32;
    if interval.months != 0 {
        let months_delta = interval.months as i64 * sign;
        let total_months =
            (i64::from(date.year) * 12 + i64::from(date.month - 1)).saturating_add(months_delta);
        let year = total_months.div_euclid(12) as i32;
        let month = (total_months.rem_euclid(12) + 1) as u8;
        let day = date.day.min(days_in_month(year, month) as u8);
        if let Some(shifted) = parts_to_date(DateParts { year, month, day }) {
            days = shifted;
        }
    }
    let shifted_base = i64::from(days) * USECS_PER_DAY + day_micros;
    // Keep the original time-of-day intact; only date shifting above changes it.
    let _ = time;
    shifted_base
        .saturating_add(sign.saturating_mul(i64::from(interval.days)) * USECS_PER_DAY)
        .saturating_add(sign.saturating_mul(interval.micros))
}

/// Formats days-since-2000-01-01 as ISO `YYYY-MM-DD`.
#[must_use]
pub fn format_date(days: i32) -> String {
    let d = date_to_parts(days);
    format!("{:04}-{:02}-{:02}", d.year, d.month, d.day)
}

/// Formats a timestamp as ISO `YYYY-MM-DD HH:MM:SS[.ffffff]`.
///
/// `precision` controls the number of fractional-second digits when the
/// value carries a declared typmod (e.g. `TIMESTAMP(3)`).  `None` means
/// "no explicit precision" and reproduces PostgreSQL's default output:
/// six fractional digits when the sub-second part is non-zero, no decimal
/// point when it is zero.
#[must_use]
pub fn format_timestamp(micros: i64, precision: Option<u16>) -> String {
    let (d, t) = timestamp_to_parts(micros);
    let secs = format_second(t.second, t.micros, precision);
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{secs}",
        d.year, d.month, d.day, t.hour, t.minute
    )
}

/// Formats the `SS` (or `SS.ffffff`) component of a time/timestamp, honouring
/// an optional declared precision.
fn format_second(second: u8, micros: u32, precision: Option<u16>) -> String {
    match precision {
        None => {
            if micros != 0 {
                format!("{:02}.{:06}", second, micros)
            } else {
                format!("{:02}", second)
            }
        }
        Some(0) => format!("{:02}", second),
        Some(p) => {
            let divisor = 10_u64.pow((6 - p) as u32);
            let fractional = (micros as u64) / divisor;
            if fractional == 0 {
                format!("{:02}", second)
            } else {
                format!("{:02}.{:0width$}", second, fractional, width = p as usize)
            }
        }
    }
}

/// PostgreSQL `age(end, start)`: full years/months/days plus time-of-day,
/// normalized like PG (days borrow 30-day months, micros normalized).
#[must_use]
pub fn age_between_timestamps(end_micros: i64, start_micros: i64) -> Interval {
    let (end_date, end_time) = timestamp_to_parts(end_micros);
    let (start_date, start_time) = timestamp_to_parts(start_micros);
    let mut months = (end_date.year - start_date.year) * 12
        + (i32::from(end_date.month) - i32::from(start_date.month));
    let mut days = i32::from(end_date.day) - i32::from(start_date.day);
    let end_tod = end_time.hour as i64 * 3600 * USECS_PER_SEC
        + end_time.minute as i64 * 60 * USECS_PER_SEC
        + end_time.second as i64 * USECS_PER_SEC
        + i64::from(end_time.micros);
    let start_tod = start_time.hour as i64 * 3600 * USECS_PER_SEC
        + start_time.minute as i64 * 60 * USECS_PER_SEC
        + start_time.second as i64 * USECS_PER_SEC
        + i64::from(start_time.micros);
    let mut micros = end_tod - start_tod;
    if micros < 0 {
        micros += USECS_PER_DAY;
        days -= 1;
    }
    if days < 0 {
        // Borrow one month: PG uses the days in the month preceding `end`.
        let (prev_year, prev_month) = if end_date.month == 1 {
            (end_date.year - 1, 12u8)
        } else {
            (end_date.year, end_date.month - 1)
        };
        days += days_in_month(prev_year, prev_month);
        months -= 1;
    }
    if months < 0 && (days > 0 || micros > 0) {
        months += 1;
        days -= 30;
        if days < 0 {
            // Re-borrow consistently after the month adjustment.
            let (prev_year, prev_month) = if end_date.month == 1 {
                (end_date.year - 1, 12u8)
            } else {
                (end_date.year, end_date.month - 1)
            };
            days += days_in_month(prev_year, prev_month);
            months -= 1;
        }
    }
    Interval {
        months,
        days,
        micros,
    }
}

/// Formats microseconds-since-midnight as `HH:MM:SS[.ffffff]`.
///
/// `precision` controls the number of fractional-second digits when the
/// value carries a declared typmod (e.g. `TIME(3)`).  `None` reproduces
/// PostgreSQL's default output: six fractional digits when the sub-second
/// part is non-zero, no decimal point when it is zero.
#[must_use]
pub fn format_time(micros: i64, precision: Option<u16>) -> String {
    let t = time_to_parts(micros);
    let secs = format_second(t.second, t.micros, precision);
    format!("{:02}:{:02}:{secs}", t.hour, t.minute)
}

#[cfg(test)]
mod tests {
    use super::{
        date_to_parts, days_in_month, format_date, format_time, format_timestamp, is_leap_year,
        parts_to_date, parts_to_time, parts_to_timestamp, time_to_parts, timestamp_to_parts,
        DateParts, TimeParts,
    };

    #[test]
    fn date_roundtrip() {
        let cases = [
            ("2000-01-01", 0),
            ("1999-12-31", -1),
            ("1970-01-01", -10_957),
            ("2024-02-29", 8825),
            ("2026-09-03", 9742),
            ("10000-01-01", 2_921_940),
            ("0001-01-01", -730_119),
        ];
        for (text, days) in cases {
            let mut split = text.split('-');
            let computed = parts_to_date(DateParts {
                year: split.next().unwrap().parse().unwrap(),
                month: split.next().unwrap().parse().unwrap(),
                day: split.next().unwrap().parse().unwrap(),
            })
            .expect(text);
            assert_eq!(computed, days, "{text}");
            assert_eq!(format_date(days), text);
            assert_eq!(date_to_parts(days), date_to_parts(computed));
        }
        assert!(parts_to_date(DateParts {
            year: 2023,
            month: 2,
            day: 29
        })
        .is_none());
        assert!(parts_to_date(DateParts {
            year: 2023,
            month: 13,
            day: 1
        })
        .is_none());
        assert!(is_leap_year(2000) && is_leap_year(2024) && !is_leap_year(1900));
        assert_eq!(days_in_month(2024, 2), 29);
    }

    #[test]
    fn time_and_timestamp() {
        let t = parts_to_time(TimeParts {
            hour: 13,
            minute: 30,
            second: 5,
            micros: 123_456,
        })
        .unwrap();
        assert_eq!(format_time(t, None), "13:30:05.123456");
        assert_eq!(
            time_to_parts(t),
            TimeParts {
                hour: 13,
                minute: 30,
                second: 5,
                micros: 123_456
            }
        );
        assert!(parts_to_time(TimeParts {
            hour: 24,
            minute: 0,
            second: 0,
            micros: 0
        })
        .is_none());

        let ts = parts_to_timestamp(0, t);
        assert_eq!(format_timestamp(ts, None), "2000-01-01 13:30:05.123456");
        let (d, tt) = timestamp_to_parts(ts);
        assert_eq!(
            d,
            DateParts {
                year: 2000,
                month: 1,
                day: 1
            }
        );
        assert_eq!(tt.micros, 123_456);
        // Negative timestamps (before epoch).
        assert_eq!(format_timestamp(-1, None), "1999-12-31 23:59:59.999999");
    }
}
