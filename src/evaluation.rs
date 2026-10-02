//! The evaluation pipeline shared by every command, so that they never
//! disagree: inventory, `dev` filtering, License clarifications, Policy and
//! Warnings.

use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::cache::Cache;
use crate::date::Date;
use crate::inventory::{
    self, Ecosystem, Inventory, LicenseOrigin, LicensedPackage, Package, Scope,
};
use crate::policy::{Policy, Reason, Verdict};
use crate::registry::Answer;
use crate::warning::Warning;
use crate::{clarification, normalize, registry, waiver};

/// The evaluated Packages of a Project.
pub struct Evaluation {
    /// See [`inventory::Inventory::sources`].
    pub sources: Vec<String>,
    /// Sorted by Package.
    pub evaluated: Vec<Evaluated>,
    /// Sorted in reporting order.
    pub warnings: Vec<Warning>,
    /// How many requests were sent to the registry, retries included.
    pub requests: usize,
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
    /// See [`inventory::LicensedPackage::introduction_paths`].
    pub introduction_paths: Vec<Vec<String>>,
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

/// How the commands that evaluate the Project reach the registry License
/// origin: its options on the command line.
#[derive(clap::Args)]
pub struct Remote {
    /// Make no network request: Packages that neither a local License origin nor the cache resolves stay Unresolved
    #[arg(long)]
    pub offline: bool,
    /// Ignore the cached registry answers: request them again and rewrite them
    #[arg(long, conflicts_with = "offline")]
    pub refresh: bool,
    /// Keep the license cache in this directory, instead of LICGUARD_CACHE_DIR or the user's cache directory
    #[arg(long, value_name = "DIR")]
    pub cache_dir: Option<PathBuf>,
}

/// Evaluates the Project at `project` against its Policy; `dev` Dependencies
/// are included when `include_dev` or the Policy says so.
pub fn evaluate(project: &Path, include_dev: bool, remote: &Remote) -> Result<Evaluation> {
    evaluate_with(project, include_dev, remote, None)
}

/// Evaluates the Project at `project` as [`evaluate`] does, `dev`
/// Dependencies included, and gives the Packages named `name` every
/// Introduction path.
pub fn evaluate_all_paths_of(project: &Path, remote: &Remote, name: &str) -> Result<Evaluation> {
    evaluate_with(project, true, remote, Some(name))
}

fn evaluate_with(
    project: &Path,
    include_dev: bool,
    remote: &Remote,
    all_paths_of: Option<&str>,
) -> Result<Evaluation> {
    let policy = Policy::load(project)?;
    let today = Date::today()?;
    let include_dev = include_dev || policy.include_dev;
    let mut inventory = inventory::inventory(project, all_paths_of)?;
    let requests = fetch_licenses(&mut inventory, &policy, remote)?;
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
    let evaluated = licensed
        .into_iter()
        .filter(|(p, _, _)| include_dev || p.scope == Scope::Prod)
        .map(|(p, license, origin)| {
            let mut outcome = policy.evaluate(license.as_deref());
            // A Waiver tolerates the Verdict, never changes the license.
            if outcome.verdict != Verdict::Allow
                && waiver::find(&policy.waivers, &p.package, license.as_deref(), today).is_some()
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
                introduction_paths: p.introduction_paths,
                sources: p.sources,
                line: p.line,
            }
        })
        .collect();
    Ok(Evaluation {
        sources: inventory.sources,
        evaluated,
        warnings,
        requests,
    })
}

/// Takes from the registry the Declared license of every Package that no
/// local License origin declares one for and no License clarification
/// covers, `dev` ones included: Waivers and clarifications are matched
/// against the whole inventory. The registry's answers come from the cache
/// when it has them and `remote` does not refresh it, else from the network
/// unless `remote` is offline. Returns how many requests were sent.
fn fetch_licenses(inventory: &mut Inventory, policy: &Policy, remote: &Remote) -> Result<usize> {
    // Checked even when no Package needs it, so that a wrong value is
    // reported at once.
    let registry = if remote.offline {
        None
    } else {
        Some(registry::url()?)
    };
    let missing: Vec<&mut LicensedPackage> = inventory
        .packages
        .iter_mut()
        .filter(|p| match p.package.ecosystem {
            Ecosystem::Npm => {
                p.from_registry
                    && p.declared_license.is_none()
                    && clarification::find(&policy.clarifications, &p.package).is_none()
            }
        })
        .collect();
    if missing.is_empty() {
        return Ok(0);
    }
    // The cache belongs to a registry, even offline.
    let registry = match registry {
        Some(registry) => registry,
        None => registry::url()?,
    };
    let mut cache = Cache::open(remote.cache_dir.as_deref(), &registry);
    let mut to_fetch = Vec::new();
    for p in missing {
        match cache.get(&p.package).filter(|_| !remote.refresh) {
            Some(license) => take_license(p, license.clone()),
            None if !remote.offline => to_fetch.push(p),
            None => {}
        }
    }
    let packages: Vec<&Package> = to_fetch.iter().map(|p| &p.package).collect();
    let (answers, requests) = registry::answers(&registry, &packages);
    // The first failure in Package order.
    let mut failure = None;
    for (p, answer) in to_fetch.into_iter().zip(answers) {
        match answer {
            Some(Ok(Answer::Found(license))) => {
                cache.insert(&p.package, license.clone());
                take_license(p, license);
            }
            // A `404` is not cached: the registry may know the version later.
            Some(Ok(Answer::NotFound)) | None => {}
            Some(Err(err)) => {
                failure.get_or_insert(err);
            }
        }
    }
    // Even after a failure, so that the next run need not fetch them again.
    cache.save();
    failure.map_or(Ok(requests), Err)
}

/// Gives `package` the Declared license the registry declares for it, if
/// any. A cached one keeps the `registry` origin: the cache only holds what
/// the registry answered.
fn take_license(package: &mut LicensedPackage, license: Option<String>) {
    if license.is_some() {
        package.declared_license = license;
        package.license_origin = Some(LicenseOrigin::Registry);
    }
}
