//! License clarifications: the Project owners' assertion of a Package's real
//! license, backed by evidence.

use serde::Deserialize;

use crate::inventory::Package;
use crate::{normalize, policy};

/// A `[[clarifications]]` entry of `licguard.toml`. Its required fields
/// default to empty so that [`Clarification::validate`], not serde, reports
/// them missing, with a hint.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Clarification {
    #[serde(default)]
    pub package: String,
    /// `None` when it applies to all versions.
    pub version: Option<String>,
    /// A Normalized license once validated.
    #[serde(default)]
    pub license: String,
    #[serde(default)]
    pub evidence: String,
}

/// The clarification that applies to `package`: the one for its version,
/// else the one for all its versions.
pub fn find<'a>(
    clarifications: &'a [Clarification],
    package: &Package,
) -> Option<&'a Clarification> {
    clarifications
        .iter()
        .filter(|c| c.matches(package))
        .max_by_key(|c| c.version.is_some())
}

impl Clarification {
    pub fn matches(&self, package: &Package) -> bool {
        package.name == self.package
            && self
                .version
                .as_ref()
                .is_none_or(|version| *version == package.version)
    }

    /// Checks the entry and puts its license in canonical form. The error
    /// names the field at fault and ends with a hint.
    pub fn validate(&mut self) -> Result<(), String> {
        for (field, value, hint) in [
            (
                "package",
                &self.package,
                "set the exact name of the Package to clarify",
            ),
            (
                "license",
                &self.license,
                "set the Package's real license as an SPDX expression, e.g. `MIT`",
            ),
            (
                "evidence",
                &self.evidence,
                "cite where the real license is stated, e.g. the URL of the Package's LICENSE file",
            ),
        ] {
            if value.trim().is_empty() {
                return Err(format!("`{field}` is missing or empty\nhint: {hint}"));
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
                "`license` `{}` asserts no license\nhint: clarify a Package only once its real license is known",
                self.license
            ));
        }
        self.license = normalize::render(&expression);
        Ok(())
    }
}
