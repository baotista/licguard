//! Waivers: the Project owners' documented, dated decision to tolerate a
//! Package whose license would otherwise violate the Policy.

use serde::Deserialize;

use crate::date::Date;
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
    /// The `expires` field as written: a `YYYY-MM-DD` string or a TOML local
    /// date.
    #[serde(rename = "expires")]
    written_expires: Option<toml::Value>,
    /// The last day the Waiver applies, set by [`Waiver::validate`].
    #[serde(skip)]
    pub expires: Date,
}

/// The Waiver that applies on `today` to `package` with Normalized license
/// `license`: the first unexpired one that matches it, preferring one for
/// its version.
pub fn find<'a>(
    waivers: &'a [Waiver],
    package: &Package,
    license: Option<&str>,
    today: Date,
) -> Option<&'a Waiver> {
    waivers
        .iter()
        .filter(|w| !w.is_expired(today) && w.matches(package, license))
        .min_by_key(|w| w.version.is_none())
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

    /// Whether the Waiver no longer applies on `today`: it still applies on
    /// its `expires` day.
    pub fn is_expired(&self, today: Date) -> bool {
        self.expires < today
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
        let Some(written) = &self.written_expires else {
            return Err(
                "`expires` is missing\nhint: set the date until which the license is tolerated, e.g. `2027-01-01`"
                    .to_string(),
            );
        };
        let expires = match written {
            toml::Value::String(text) => Date::parse(text),
            // A local date only: a time or an offset makes it a datetime.
            toml::Value::Datetime(date) if date.time.is_none() && date.offset.is_none() => {
                Date::parse(&date.to_string())
            }
            _ => None,
        };
        let Some(expires) = expires else {
            let shown = match written {
                toml::Value::String(text) => text.clone(),
                other => other.to_string(),
            };
            return Err(format!(
                "`expires` `{shown}` is not a calendar date written `YYYY-MM-DD`\nhint: write the date as `expires = \"2027-01-01\"` or `expires = 2027-01-01`"
            ));
        };
        self.expires = expires;
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
