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
        let verdict = match e.verdict {
            Verdict::Deny => "DENY",
            Verdict::Review => "REVIEW",
            Verdict::Allow => unreachable!(),
        };
        // An Unresolved license already shows its reason in the license column.
        let license = e
            .elected
            .as_deref()
            .or(e.license.as_deref())
            .unwrap_or("(unresolved)");
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
        let via = match &e.introduction_path {
            Some(path) => format!("  via {}", path.join(" > ")),
            None => String::new(),
        };
        let sources = if evaluation.several_sources() {
            format!("  in {}", e.sources.join(", "))
        } else {
            String::new()
        };
        let reason = if notes.is_empty() {
            String::new()
        } else {
            format!("  ({})", notes.join(", "))
        };
        writeln!(
            out,
            "{verdict:<7} {license:<15} {}@{}{via}{sources}{reason}",
            e.package.name, e.package.version
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

    let (deny, review, allow) = (
        evaluation.count(Verdict::Deny),
        evaluation.count(Verdict::Review),
        evaluation.count(Verdict::Allow),
    );
    writeln!(out, "{deny} deny · {review} review · {allow} allow").unwrap();
    if violated {
        writeln!(out, "✗ Policy violated (exit 1)").unwrap();
    } else {
        writeln!(out, "✓ Policy respected").unwrap();
    }
    out
}
