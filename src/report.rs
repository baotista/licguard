use std::fmt::Write;

use crate::evaluation::{Evaluated, Evaluation};
use crate::inventory::LicenseOrigin;
use crate::policy::{Reason, Verdict};

/// The first line of the terminal reports, followed by a blank line.
pub fn header(evaluated: &[Evaluated]) -> String {
    let ecosystems: std::collections::BTreeSet<String> = evaluated
        .iter()
        .map(|e| e.package.ecosystem.to_string())
        .collect();
    let ecosystems = ecosystems.into_iter().collect::<Vec<_>>().join(", ");
    format!(
        "licguard {} — {} packages ({ecosystems})\n\n",
        env!("CARGO_PKG_VERSION"),
        evaluated.len()
    )
}

/// Renders the terminal report of `check`: the Packages that are not
/// allowed, by Verdict then Package, and the Warnings.
pub fn text(evaluation: &Evaluation, violated: bool) -> String {
    let evaluated = &evaluation.evaluated;
    let warnings = &evaluation.warnings;
    let mut out = header(evaluated);

    let mut flagged: Vec<_> = evaluated
        .iter()
        .filter(|e| e.verdict != Verdict::Allow)
        .collect();
    flagged.sort_by(|a, b| (a.verdict, &a.package).cmp(&(b.verdict, &b.package)));
    for e in &flagged {
        writeln!(
            out,
            "{:<7} {:<15} {}@{}{}",
            e.verdict.as_str().to_uppercase(),
            license(e),
            e.package.name,
            e.package.version,
            details(evaluation, e)
        )
        .unwrap();
    }
    if !flagged.is_empty() {
        out.push('\n');
    }
    for warning in warnings {
        writeln!(out, "warning: {warning}").unwrap();
    }
    if !warnings.is_empty() {
        out.push('\n');
    }

    writeln!(out, "{}", counts(evaluation)).unwrap();
    writeln!(out, "{}", outcome(violated)).unwrap();
    out
}

/// The license a report shows for a Package: its Elected license, else its
/// Normalized license. An Unresolved license shows its reason instead.
pub fn license(e: &Evaluated) -> &str {
    e.elected
        .as_deref()
        .or(e.license.as_deref())
        .unwrap_or("(unresolved)")
}

/// What a report says of a Package after its name: `  via` its
/// Introduction path, `  in` its Inventory sources when the Project has
/// several, and `  (`its notes`)`.
pub fn details(evaluation: &Evaluation, e: &Evaluated) -> String {
    let mut notes = Vec::new();
    if e.origin == Some(LicenseOrigin::Clarification) {
        notes.push("clarified".to_string());
    }
    if e.reason == Reason::Unlisted {
        notes.push(e.reason.as_str().to_string());
    }
    if let (Some(_), Some(full)) = (&e.elected, &e.license) {
        notes.push(format!("elected from {full}"));
    }
    let mut out = String::new();
    if let Some(path) = &e.introduction_path {
        write!(out, "  via {}", path.join(" > ")).unwrap();
    }
    if evaluation.several_sources() {
        write!(out, "  in {}", e.sources.join(", ")).unwrap();
    }
    if !notes.is_empty() {
        write!(out, "  ({})", notes.join(", ")).unwrap();
    }
    out
}

/// The count of each Verdict, e.g. `2 deny · 0 review · 4 allow (1 waived)`.
pub fn counts(evaluation: &Evaluation) -> String {
    let mut out = format!(
        "{} deny · {} review · {} allow",
        evaluation.count(Verdict::Deny),
        evaluation.count(Verdict::Review),
        evaluation.count(Verdict::Allow)
    );
    let waived = evaluation.waived();
    if waived > 0 {
        write!(out, " ({waived} waived)").unwrap();
    }
    out
}

/// Whether the gate fails, and its exit code.
pub fn outcome(violated: bool) -> &'static str {
    if violated {
        "✗ Policy violated (exit 1)"
    } else {
        "✓ Policy respected"
    }
}
