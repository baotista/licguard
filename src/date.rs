//! Calendar dates, for Waiver expiry: no time of day, no time zone.

use std::env;
use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Result, anyhow};

/// The environment variable that overrides today's date, for reproducible
/// runs and tests.
const TODAY: &str = "LICGUARD_TODAY";

/// A calendar date, as a number of days since 1970-01-01.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Date(i64);

impl Date {
    /// Parses a real calendar date written `YYYY-MM-DD`.
    pub fn parse(text: &str) -> Option<Date> {
        let well_formed = text.len() == 10
            && text.bytes().enumerate().all(|(i, byte)| match i {
                4 | 7 => byte == b'-',
                _ => byte.is_ascii_digit(),
            });
        if !well_formed {
            return None;
        }
        let number = |range: std::ops::Range<usize>| text[range].parse::<u32>().unwrap();
        let (year, month, day) = (number(0..4), number(5..7), number(8..10));
        let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
        let days = match month {
            1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
            4 | 6 | 9 | 11 => 30,
            2 if leap => 29,
            2 => 28,
            _ => return None,
        };
        (1..=days)
            .contains(&day)
            .then(|| Date(days_from_civil(year.into(), month.into(), day.into())))
    }

    /// Today's date: `LICGUARD_TODAY` when set, else the current UTC date.
    pub fn today() -> Result<Date> {
        let Some(value) = env::var_os(TODAY) else {
            let elapsed = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("the system clock is after 1970");
            return Ok(Date((elapsed.as_secs() / 86_400) as i64));
        };
        let text = value.to_string_lossy();
        Date::parse(&text).ok_or_else(|| {
            anyhow!(
                "`{TODAY}` `{text}` is not a calendar date written `YYYY-MM-DD`\nhint: set it to the date to check as of, e.g. `{TODAY}=2027-01-01`, or unset it to use today's date"
            )
        })
    }

    /// The number of days from `self` to `later`; negative when `later` is
    /// before `self`.
    pub fn days_until(self, later: Date) -> i64 {
        later.0 - self.0
    }
}

impl fmt::Display for Date {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (year, month, day) = civil_from_days(self.0);
        write!(f, "{year:04}-{month:02}-{day:02}")
    }
}

// Conversions between a proleptic Gregorian date and a day number, after
// Howard Hinnant's `days_from_civil` and `civil_from_days`.

fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let day_of_year = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let days = days + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = (shifted_month + 2) % 12 + 1;
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}
