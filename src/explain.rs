//! `licguard explain`: why each Package matching a query got its Verdict,
//! over the whole inventory, `dev` Dependencies included.

use std::collections::BTreeSet;
use std::fmt::Write;
use std::path::Path;

use anyhow::{Result, bail};
use serde::Serialize;

use crate::clarification::{self, Clarification};
use crate::date::Date;
use crate::evaluation::{self, Evaluated, Evaluation, Remote};
use crate::inventory::paths::MAX_PATHS;
use crate::inventory::{Ecosystem, Package};
use crate::json::{self, PackageJson};
use crate::policy::Policy;
use crate::waiver::{self, Waiver};
use crate::warning::Warning;

/// The output format of `explain`.
#[derive(Clone, Copy, clap::ValueEnum)]
pub enum Format {
    Text,
    Json,
}

/// Explains every Package of the Project at `project` that `query` names,
/// in `format`. Also returns the Evaluation it comes from. Fails when no
/// Package matches.
pub fn explain(
    project: &Path,
    query: &str,
    format: Format,
    remote: &Remote,
) -> Result<(String, Evaluation)> {
    let written = query;
    let query = Query::parse(written);
    let policy = Policy::load(project)?;
    let today = Date::today()?;
    let evaluation = evaluation::evaluate_all_paths_of(project, remote, query.name)?;
    let matching: Vec<Explained> = evaluation
        .evaluated
        .iter()
        .filter(|e| query.matches(&e.package))
        .map(|e| Explained {
            waiver: waiver::find(&policy.waivers, &e.package, e.license.as_deref(), today),
            clarification: clarification::find(&policy.clarifications, &e.package),
            warnings: evaluation
                .warnings
                .iter()
                .filter(|w| w.concerns(&e.package))
                .collect(),
            evaluated: e,
        })
        .collect();
    if matching.is_empty() {
        bail!(
            "no Package matches `{written}`\nhint: {}",
            hint(&evaluation, &query)
        );
    }
    let out = match format {
        Format::Text => text(&matching),
        Format::Json => json(&matching),
    };
    Ok((out, evaluation))
}

/// A Package that the query names, with what explains its Verdict.
struct Explained<'a> {
    evaluated: &'a Evaluated,
    /// The Waiver that applies to it, if any.
    waiver: Option<&'a Waiver>,
    /// The License clarification that applies to it, if any.
    clarification: Option<&'a Clarification>,
    /// The Warnings about its Waivers and License clarifications.
    warnings: Vec<&'a Warning>,
}

/// The hint for a query that matches no Package: the versions of its name,
/// else the names of the inventory within two edits of it or containing it,
/// closest first, at most five of them.
fn hint(evaluation: &Evaluation, query: &Query) -> String {
    let packages = evaluation
        .evaluated
        .iter()
        .map(|e| &e.package)
        .filter(|p| query.ecosystem.is_none_or(|e| p.ecosystem == e));
    let mut candidates: Vec<String> = packages
        .clone()
        .filter(|p| p.name == query.name)
        .map(|p| format!("{}@{}", p.name, p.version))
        .collect();
    if candidates.is_empty() {
        let names: BTreeSet<&str> = packages.map(|p| p.name.as_str()).collect();
        let mut close: Vec<(usize, &str)> = names
            .into_iter()
            .map(|name| (distance(query.name, name), name))
            .filter(|&(distance, name)| distance <= 2 || name.contains(query.name))
            .collect();
        close.sort();
        candidates = close
            .into_iter()
            .map(|(_, name)| name.to_string())
            .collect();
    }
    if candidates.is_empty() {
        return "run `licguard list --include-dev` to see the whole inventory".to_string();
    }
    candidates.truncate(5);
    format!("did you mean {}?", candidates.join(", "))
}

/// The Levenshtein distance between `a` and `b`: how many characters to
/// insert, delete or replace to turn one into the other.
fn distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    // The distances from a prefix of `a` to each prefix of `b`.
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let above = row[j + 1];
            row[j + 1] = if ca == *cb {
                diagonal
            } else {
                1 + diagonal.min(above).min(row[j])
            };
            diagonal = above;
        }
    }
    row[b.len()]
}

/// How many Introduction paths the text shows for a Package.
const SHOWN_PATHS: usize = 20;

/// The ecosystems a query may name in its prefix, e.g. `npm:`.
const ECOSYSTEMS: [Ecosystem; 1] = [Ecosystem::Npm];

/// What `explain` looks for: a Package name, optionally with its ecosystem
/// and a version.
struct Query<'a> {
    /// `None` for every ecosystem.
    ecosystem: Option<Ecosystem>,
    name: &'a str,
    /// `None` for every version.
    version: Option<&'a str>,
}

impl<'a> Query<'a> {
    /// Reads `[<ecosystem>:]name[@version]`. The prefix is one of
    /// [`ECOSYSTEMS`] only, so that a name with a `:` stays a name. The
    /// version follows the last `@` that does not start the name, so that a
    /// scoped npm name such as `@types/ms` reads right.
    fn parse(query: &'a str) -> Self {
        let (ecosystem, query) = ECOSYSTEMS
            .iter()
            .find_map(|e| {
                query
                    .strip_prefix(&format!("{e}:"))
                    .map(|rest| (Some(*e), rest))
            })
            .unwrap_or((None, query));
        let (name, version) = match query.rfind('@').filter(|&at| at > 0) {
            Some(at) => (&query[..at], Some(&query[at + 1..])),
            None => (query, None),
        };
        Query {
            ecosystem,
            name,
            version,
        }
    }

    /// Whether `package` is one the query names: matching is exact and
    /// case-sensitive.
    fn matches(&self, package: &Package) -> bool {
        self.ecosystem.is_none_or(|e| package.ecosystem == e)
            && package.name == self.name
            && self.version.is_none_or(|v| package.version == v)
    }
}

/// One block of `key: value` lines per Package, separated by blank lines.
fn text(matching: &[Explained]) -> String {
    let mut out = String::new();
    for (i, explained) in matching.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        // An empty `key` continues the previous line's value.
        let mut line = |key: &str, value: &str| {
            let key = if key.is_empty() {
                String::new()
            } else {
                format!("{key}:")
            };
            writeln!(out, "{key:<20}{value}").unwrap();
        };
        let e = explained.evaluated;
        let package = &e.package;
        line(
            "Package",
            &format!(
                "{}@{} ({})",
                package.name, package.version, package.ecosystem
            ),
        );
        line("Scope", e.scope.as_str());
        line(
            "Declared license",
            e.declared_license.as_deref().unwrap_or("(none)"),
        );
        line(
            "Normalized license",
            e.license.as_deref().unwrap_or("(unresolved)"),
        );
        if let Some(elected) = &e.elected {
            line("Elected license", elected);
        }
        line("License origin", e.origin.map_or("(none)", |o| o.as_str()));
        line("Verdict", e.verdict.as_str());
        line("Verdict reason", e.reason.as_str());
        if let Some(w) = explained.waiver {
            line(
                "Waiver",
                &format!(
                    "{}, {}, expires {}",
                    subject(&w.package, w.version.as_deref()),
                    w.license,
                    w.expires
                ),
            );
            line("Waiver reason", &w.reason);
        }
        if let Some(c) = explained.clarification {
            line(
                "Clarification",
                &format!(
                    "{}, {}",
                    subject(&c.package, c.version.as_deref()),
                    c.license
                ),
            );
            line("Evidence", &c.evidence);
        }
        for warning in &explained.warnings {
            line("Warning", &warning.to_string());
        }
        let (shown, more) = shown_paths(&e.introduction_paths);
        let mut paths = shown.iter().map(|path| path.join(" > "));
        line(
            "Introduction paths",
            &paths.next().unwrap_or_else(|| "(none)".to_string()),
        );
        for path in paths {
            line("", &path);
        }
        if e.introduction_paths.len() > MAX_PATHS {
            line("", &format!("(+{more} or more)"));
        } else if more > 0 {
            line("", &format!("(+{more} more)"));
        }
        line("Inventory sources", &e.sources.join(", "));
    }
    out
}

/// The document of `--format json`: each Package as `list --format json`
/// gives it, with what else the text shows, in the same order.
fn json(matching: &[Explained]) -> String {
    #[derive(Serialize)]
    struct Document<'a> {
        packages: Vec<ExplainedJson<'a>>,
    }
    #[derive(Serialize)]
    struct ExplainedJson<'a> {
        #[serde(flatten)]
        package: PackageJson<'a>,
        /// As many as the text shows.
        introduction_paths: &'a [Vec<String>],
        /// How many the text leaves out.
        more_introduction_paths: usize,
        waiver: Option<WaiverJson<'a>>,
        clarification: Option<ClarificationJson<'a>>,
        /// Their text, as in the terminal.
        warnings: Vec<String>,
    }
    #[derive(Serialize)]
    struct WaiverJson<'a> {
        package: &'a str,
        version: Option<&'a str>,
        license: &'a str,
        reason: &'a str,
        expires: String,
    }
    #[derive(Serialize)]
    struct ClarificationJson<'a> {
        package: &'a str,
        version: Option<&'a str>,
        license: &'a str,
        evidence: &'a str,
    }
    json::render(&Document {
        packages: matching
            .iter()
            .map(|explained| {
                let e = explained.evaluated;
                let (shown, more) = shown_paths(&e.introduction_paths);
                ExplainedJson {
                    package: PackageJson::from(e),
                    introduction_paths: shown,
                    more_introduction_paths: more,
                    waiver: explained.waiver.map(|w| WaiverJson {
                        package: &w.package,
                        version: w.version.as_deref(),
                        license: &w.license,
                        reason: &w.reason,
                        expires: w.expires.to_string(),
                    }),
                    clarification: explained.clarification.map(|c| ClarificationJson {
                        package: &c.package,
                        version: c.version.as_deref(),
                        license: &c.license,
                        evidence: &c.evidence,
                    }),
                    warnings: explained.warnings.iter().map(|w| w.to_string()).collect(),
                }
            })
            .collect(),
    })
}

/// The Introduction paths the output shows out of `all`, and how many it
/// leaves out, counting up to [`MAX_PATHS`].
fn shown_paths(all: &[Vec<String>]) -> (&[Vec<String>], usize) {
    let shown = all.len().min(SHOWN_PATHS);
    (&all[..shown], all.len().min(MAX_PATHS) - shown)
}

/// `package`, or `package@version` for an entry of one version only.
fn subject(package: &str, version: Option<&str>) -> String {
    match version {
        Some(version) => format!("{package}@{version}"),
        None => package.to_string(),
    }
}
