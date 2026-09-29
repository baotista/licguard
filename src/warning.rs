use std::fmt;

use crate::date::Date;

/// A condition reported by a check that needs attention but never fails the
/// gate. Sorting puts Warnings in their reporting order: by kind, then
/// Package, then version.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Warning {
    /// A License clarification that matches no Package of the inventory.
    UnmatchedClarification {
        package: String,
        version: Option<String>,
    },
    /// A Waiver that matches no Package of the inventory, in name, version
    /// and Normalized license.
    UnmatchedWaiver {
        package: String,
        version: Option<String>,
        expires: Date,
    },
    /// A Waiver whose expiry date has passed: it no longer applies.
    ExpiredWaiver {
        package: String,
        version: Option<String>,
        expires: Date,
    },
    /// A Waiver that still applies but expires within
    /// `waiver_expiry_warning_days`, `days` from today.
    ExpiringWaiver {
        package: String,
        version: Option<String>,
        expires: Date,
        days: u32,
    },
}

impl fmt::Display for Warning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Warning::UnmatchedClarification { package, version } => {
                subject(f, "clarification", package, version)?;
                f.write_str(" matches no Package")
            }
            Warning::UnmatchedWaiver {
                package, version, ..
            } => {
                subject(f, "waiver", package, version)?;
                f.write_str(" matches no Package")
            }
            Warning::ExpiredWaiver {
                package,
                version,
                expires,
            } => {
                subject(f, "waiver", package, version)?;
                write!(f, " expired on {expires}")
            }
            Warning::ExpiringWaiver {
                package,
                version,
                expires,
                days,
            } => {
                subject(f, "waiver", package, version)?;
                match days {
                    0 => write!(f, " expires today ({expires})"),
                    1 => write!(f, " expires in 1 day ({expires})"),
                    days => write!(f, " expires in {days} days ({expires})"),
                }
            }
        }
    }
}

/// Writes `<entry> for <package>[@<version>]`.
fn subject(
    f: &mut fmt::Formatter<'_>,
    entry: &str,
    package: &str,
    version: &Option<String>,
) -> fmt::Result {
    write!(f, "{entry} for {package}")?;
    if let Some(version) = version {
        write!(f, "@{version}")?;
    }
    Ok(())
}
