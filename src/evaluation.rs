//! The evaluation pipeline shared by every command, so that they never
//! disagree: inventory, `dev` filtering, License clarifications, Policy and
//! Warnings.

use std::path::Path;

use anyhow::Result;

use crate::date::Date;
use crate::inventory::{self, LicenseOrigin, Package, Scope};
use crate::policy::{Policy, Reason, Verdict};
use crate::warning::Warning;
use crate::{clarification, normalize};

/// The evaluated Packages of a Project.
pub struct Evaluation {
    /// See [`inventory::Inventory::sources`].
    pub sources: Vec<String>,
    /// Sorted by Package.
    pub evaluated: Vec<Evaluated>,
    /// Sorted in reporting order.
    pub warnings: Vec<Warning>,
}

/// One Package evaluated against the Policy.
pub struct Evaluated {
    pub package: Package,
    pub scope: Scope,
    /// See [`inventory::LicensedPackage::declared_license`].
    pub declared_license: Option<String>,
    /// The Normalized license; `None` when Unresolved.
    pub license: Option<String>,
    /// Where `license` came from; `None` when nothing declared one.
    pub origin: Option<LicenseOrigin>,
    pub verdict: Verdict,
    pub reason: Reason,
    /// See [`crate::policy::Outcome::elected`].
    pub elected: Option<String>,
    /// See [`inventory::LicensedPackage::introduction_path`].
    pub introduction_path: Option<Vec<String>>,
    /// See [`inventory::LicensedPackage::sources`].
    pub sources: Vec<String>,
    /// See [`inventory::LicensedPackage::line`].
    pub line: Option<usize>,
}

impl Evaluation {
    /// Whether naming each Package's Inventory sources tells anything.
    pub fn several_sources(&self) -> bool {
        self.sources.len() > 1
    }

    /// How many Packages received `verdict`.
    pub fn count(&self, verdict: Verdict) -> usize {
        self.evaluated
            .iter()
            .filter(|e| e.verdict == verdict)
            .count()
    }

    /// How many `allow` Verdicts come from a Waiver.
    pub fn waived(&self) -> usize {
        self.evaluated
            .iter()
            .filter(|e| e.reason == Reason::Waived)
            .count()
    }
}

/// Evaluates the Project at `project` against its Policy; `dev` Dependencies
/// are included when `include_dev` or the Policy says so.
pub fn evaluate(project: &Path, include_dev: bool) -> Result<Evaluation> {
    let policy = Policy::load(project)?;
    let today = Date::today()?;
    let include_dev = include_dev || policy.include_dev;
    let inventory = inventory::inventory(project)?;
    // Matched against the whole inventory: a clarification for an
    // excluded `dev` Dependency still applies to something.
    let mut warnings: Vec<Warning> = policy
        .clarifications
        .iter()
        .filter(|c| !inventory.packages.iter().any(|p| c.matches(&p.package)))
        .map(|c| Warning::UnmatchedClarification {
            package: c.package.clone(),
            version: c.version.clone(),
        })
        .collect();
    // The Normalized license of every Package, before `dev` filtering.
    let licensed: Vec<_> = inventory
        .packages
        .into_iter()
        .map(|p| {
            let clarification = clarification::find(&policy.clarifications, &p.package);
            // A clarification's license is already a Normalized license.
            let (license, origin) = match clarification {
                Some(c) => (Some(c.license.clone()), Some(LicenseOrigin::Clarification)),
                None => (
                    p.declared_license.as_deref().and_then(normalize::normalize),
                    p.license_origin,
                ),
            };
            (p, license, origin)
        })
        .collect();
    // Matched against the whole inventory too: a Waiver for an excluded
    // `dev` Dependency still applies to something.
    for w in &policy.waivers {
        let (package, version, expires) = (w.package.clone(), w.version.clone(), w.expires);
        let days = today.days_until(w.expires);
        if !licensed
            .iter()
            .any(|(p, license, _)| w.matches(&p.package, license.as_deref()))
        {
            warnings.push(Warning::UnmatchedWaiver {
                package,
                version,
                expires,
            });
        } else if w.is_expired(today) {
            warnings.push(Warning::ExpiredWaiver {
                package,
                version,
                expires,
            });
        } else if days <= i64::from(policy.waiver_expiry_warning_days) {
            warnings.push(Warning::ExpiringWaiver {
                package,
                version,
                expires,
                days: days as u32,
            });
        }
    }
    warnings.sort();
    let waivers: Vec<_> = policy
        .waivers
        .iter()
        .filter(|w| !w.is_expired(today))
        .collect();
    let evaluated = licensed
        .into_iter()
        .filter(|(p, _, _)| include_dev || p.scope == Scope::Prod)
        .map(|(p, license, origin)| {
            let mut outcome = policy.evaluate(license.as_deref());
            // A Waiver tolerates the Verdict, never changes the license.
            if outcome.verdict != Verdict::Allow
                && waivers
                    .iter()
                    .any(|w| w.matches(&p.package, license.as_deref()))
            {
                outcome.verdict = Verdict::Allow;
                outcome.reason = Reason::Waived;
            }
            Evaluated {
                verdict: outcome.verdict,
                reason: outcome.reason,
                elected: outcome.elected,
                license,
                origin,
                declared_license: p.declared_license,
                scope: p.scope,
                package: p.package,
                introduction_path: p.introduction_path,
                sources: p.sources,
                line: p.line,
            }
        })
        .collect();
    Ok(Evaluation {
        sources: inventory.sources,
        evaluated,
        warnings,
    })
}
