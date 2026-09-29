//! Calendar dates, for Waiver expiry: no time of day, no time zone.

use std::env;
use std::fmt;

use anyhow::{Result, anyhow};
use jiff::Zoned;
use jiff::civil;

/// The environment variable that overrides today's date, for reproducible
/// runs and tests.
const TODAY: &str = "LICGUARD_TODAY";

/// A calendar date.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Date(civil::Date);

impl Date {
    /// Parses a real calendar date written `YYYY-MM-DD`.
    pub fn parse(text: &str) -> Option<Date> {
        // jiff also accepts other forms, e.g. `20260601`, `+002026-06-01` or
        // `2026-06-01T10:00`, so the shape is checked first.
        let well_formed = text.len() == 10
            && text.bytes().enumerate().all(|(i, byte)| match i {
                4 | 7 => byte == b'-',
                _ => byte.is_ascii_digit(),
            });
        if !well_formed {
            return None;
        }
        text.parse().ok().map(Date)
    }

    /// Today's date: `LICGUARD_TODAY` when set, else the current date in the
    /// system's local time zone.
    pub fn today() -> Result<Date> {
        let Some(value) = env::var_os(TODAY) else {
            return Ok(Date(Zoned::now().date()));
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
        (later.0 - self.0).get_days().into()
    }
}

impl fmt::Display for Date {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}
