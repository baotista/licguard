//! Waivers: the Project owners' documented, dated decision to tolerate a
//! Package whose license would otherwise violate the Policy.

use serde::Deserialize;

use crate::inventory::Package;
use crate::{normalize, policy};

/// A `[[waivers]]` entry of `licguard.toml`. Its required fields default to
/// empty so that [`Waiver::validate`], not serde, reports them missing, with
/// a hint.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Waiver {
    #[serde(default)]
    pub package: String,
    /// `None` when it applies to all versions.
    pub version: Option<String>,
    /// The Normalized license the Waiver tolerates, once validated.
    #[serde(default)]
    pub license: String,
    #[serde(default)]
    pub reason: String,
    /// A `YYYY-MM-DD` string or a TOML local date, once validated.
    #[serde(default)]
    pub expires: Option<toml::Value>,
}

impl Waiver {
    /// Whether the Waiver applies to `package` with Normalized license
    /// `license`; an Unresolved Package (`None`) never matches.
    pub fn matches(&self, package: &Package, license: Option<&str>) -> bool {
        package.name == self.package
            && self
                .version
                .as_ref()
                .is_none_or(|version| *version == package.version)
            && license == Some(self.license.as_str())
    }

    /// Checks the entry and puts its license in canonical form. The error
    /// names the field at fault and ends with a hint.
    pub fn validate(&mut self) -> Result<(), String> {
        for (field, value, hint) in [
            (
                "package",
                &self.package,
                "set the exact name of the Package to waive",
            ),
            (
                "license",
                &self.license,
                "set the Normalized license to tolerate as an SPDX expression, e.g. `LGPL-3.0-only`",
            ),
            (
                "reason",
                &self.reason,
                "state why the license is tolerated, e.g. `Approved by legal, ticket LEGAL-142`",
            ),
        ] {
            if value.trim().is_empty() {
                return Err(format!("`{field}` is missing or empty\nhint: {hint}"));
            }
        }
        match &self.expires {
            None => {
                return Err(
                    "`expires` is missing\nhint: set the date until which the license is tolerated, e.g. `2027-01-01`"
                        .to_string(),
                );
            }
            Some(toml::Value::String(text)) if is_calendar_date(text) => {}
            // A local date only: a time or an offset makes it a datetime.
            Some(toml::Value::Datetime(date))
                if date.time.is_none()
                    && date.offset.is_none()
                    && is_calendar_date(&date.to_string()) => {}
            Some(value) => {
                let shown = match value {
                    toml::Value::String(text) => text.clone(),
                    other => other.to_string(),
                };
                return Err(format!(
                    "`expires` `{shown}` is not a calendar date written `YYYY-MM-DD`\nhint: write the date as `expires = \"2027-01-01\"` or `expires = 2027-01-01`"
                ));
            }
        }
        let Ok(expression) = policy::parse(&self.license) else {
            return Err(format!(
                "`license` `{}` is not a valid SPDX expression\nhint: use SPDX identifiers and upper-case operators, e.g. `MIT OR Apache-2.0`; see https://spdx.org/licenses/",
                self.license
            ));
        };
        if normalize::is_unknown(&expression) {
            return Err(format!(
                "`license` `{}` asserts no license\nhint: an Unresolved Package cannot be waived; add a License clarification for it instead",
                self.license
            ));
        }
        self.license = normalize::render(&expression);
        Ok(())
    }
}

/// Whether `text` is a real calendar date written `YYYY-MM-DD`.
fn is_calendar_date(text: &str) -> bool {
    let well_formed = text.len() == 10
        && text.bytes().enumerate().all(|(i, byte)| match i {
            4 | 7 => byte == b'-',
            _ => byte.is_ascii_digit(),
        });
    if !well_formed {
        return false;
    }
    let number = |range: std::ops::Range<usize>| text[range].parse::<u32>().unwrap();
    let (year, month, day) = (number(0..4), number(5..7), number(8..10));
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return false,
    };
    (1..=days).contains(&day)
}
