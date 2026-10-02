//! The JSON documents of `list` and `check`, for machines. Their key order
//! is fixed by the field order of the structs below.

use serde::Serialize;

use crate::evaluation::{Evaluated, Evaluation};
use crate::policy::Verdict;
use crate::warning::Warning;

/// The document of `list --format json`: every Package, sorted by Package.
pub fn list(evaluation: &Evaluation) -> String {
    #[derive(Serialize)]
    struct List<'a> {
        packages: Vec<PackageJson<'a>>,
    }
    render(&List {
        packages: evaluation.evaluated.iter().map(PackageJson::from).collect(),
    })
}

/// The document of `check --format json`: whether the gate fails, the count
/// of each Verdict, the Violations (sorted by Package) and the Warnings.
pub fn check(evaluation: &Evaluation, strict: bool, violated: bool) -> String {
    #[derive(Serialize)]
    struct Check<'a> {
        violated: bool,
        summary: Summary,
        violations: Vec<PackageJson<'a>>,
        warnings: Vec<WarningJson<'a>>,
    }
    #[derive(Serialize)]
    struct Summary {
        deny: usize,
        review: usize,
        allow: usize,
        /// How many of the `allow` Verdicts come from a Waiver.
        waived: usize,
    }
    render(&Check {
        violated,
        summary: Summary {
            deny: evaluation.count(Verdict::Deny),
            review: evaluation.count(Verdict::Review),
            allow: evaluation.count(Verdict::Allow),
            waived: evaluation.waived(),
        },
        violations: evaluation
            .evaluated
            .iter()
            .filter(|e| e.verdict.is_violation(strict))
            .map(PackageJson::from)
            .collect(),
        warnings: evaluation.warnings.iter().map(WarningJson::from).collect(),
    })
}

/// A Warning: its kind, its text as in the terminal report, the Package it
/// is about, for a Waiver, its expiry date and the days left and, for an
/// SBOM component, the SBOM.
#[derive(Serialize)]
struct WarningJson<'a> {
    kind: &'static str,
    message: String,
    package: &'a str,
    version: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    expires: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    days: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    source: Option<&'a str>,
}

impl<'a> From<&'a Warning> for WarningJson<'a> {
    fn from(warning: &'a Warning) -> Self {
        let kind = warning.kind();
        let message = warning.to_string();
        match warning {
            Warning::UnmatchedClarification { package, version } => WarningJson {
                kind,
                message,
                package,
                version: version.as_deref(),
                expires: None,
                days: None,
                source: None,
            },
            Warning::UnmatchedWaiver {
                package,
                version,
                expires,
            } => WarningJson {
                kind,
                message,
                package,
                version: version.as_deref(),
                expires: Some(expires.to_string()),
                days: None,
                source: None,
            },
            Warning::ExpiredWaiver {
                package,
                version,
                expires,
            } => WarningJson {
                kind,
                message,
                package,
                version: version.as_deref(),
                expires: Some(expires.to_string()),
                days: None,
                source: None,
            },
            Warning::ExpiringWaiver {
                package,
                version,
                expires,
                days,
            } => WarningJson {
                kind,
                message,
                package,
                version: version.as_deref(),
                expires: Some(expires.to_string()),
                days: Some(*days),
                source: None,
            },
            Warning::UnsupportedComponent {
                component,
                version,
                source,
                ..
            } => WarningJson {
                kind,
                message,
                package: component,
                version: version.as_deref(),
                expires: None,
                days: None,
                source: Some(source),
            },
        }
    }
}

/// An evaluated Package.
#[derive(Serialize)]
struct PackageJson<'a> {
    ecosystem: String,
    name: &'a str,
    version: &'a str,
    scope: &'static str,
    declared_license: Option<&'a str>,
    /// The Normalized license; `null` when Unresolved.
    license: Option<&'a str>,
    elected: Option<&'a str>,
    verdict: &'static str,
    reason: &'static str,
    origin: Option<&'static str>,
    introduction_path: Option<&'a [String]>,
    sources: &'a [String],
}

impl<'a> From<&'a Evaluated> for PackageJson<'a> {
    fn from(e: &'a Evaluated) -> Self {
        PackageJson {
            ecosystem: e.package.ecosystem.to_string(),
            name: &e.package.name,
            version: &e.package.version,
            scope: e.scope.as_str(),
            declared_license: e.declared_license.as_deref(),
            license: e.license.as_deref(),
            elected: e.elected.as_deref(),
            verdict: e.verdict.as_str(),
            reason: e.reason.as_str(),
            origin: e.origin.map(|o| o.as_str()),
            introduction_path: e.introduction_path.as_deref(),
            sources: &e.sources,
        }
    }
}

/// Pretty-prints with 2-space indentation and a trailing newline.
fn render(document: &impl Serialize) -> String {
    let mut out = serde_json::to_string_pretty(document).expect("the document serializes");
    out.push('\n');
    out
}
