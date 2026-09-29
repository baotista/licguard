use std::fmt::Write;

use crate::inventory::Package;
use crate::policy::Verdict;

pub struct Evaluated {
    pub package: Package,
    /// The Normalized license; `None` when Unresolved.
    pub license: Option<String>,
    pub verdict: Verdict,
}

/// Renders the terminal report. Expects `evaluated` already sorted.
pub fn text(evaluated: &[Evaluated]) -> String {
    let mut out = String::new();
    let ecosystems: std::collections::BTreeSet<String> = evaluated
        .iter()
        .map(|e| e.package.ecosystem.to_string())
        .collect();
    let ecosystems = ecosystems.into_iter().collect::<Vec<_>>().join(", ");
    writeln!(
        out,
        "licguard {} — {} packages ({ecosystems})\n",
        env!("CARGO_PKG_VERSION"),
        evaluated.len()
    )
    .unwrap();

    let flagged: Vec<_> = evaluated
        .iter()
        .filter(|e| e.verdict != Verdict::Allow)
        .collect();
    for e in &flagged {
        let verdict = match e.verdict {
            Verdict::Deny => "DENY",
            Verdict::Review => "REVIEW",
            Verdict::Allow => unreachable!(),
        };
        let license = e.license.as_deref().unwrap_or("(unresolved)");
        writeln!(
            out,
            "{verdict:<7} {license:<15} {}@{}",
            e.package.name, e.package.version
        )
        .unwrap();
    }
    if !flagged.is_empty() {
        out.push('\n');
    }

    let count = |v| evaluated.iter().filter(|e| e.verdict == v).count();
    let (deny, review, allow) = (
        count(Verdict::Deny),
        count(Verdict::Review),
        count(Verdict::Allow),
    );
    writeln!(out, "{deny} deny · {review} review · {allow} allow").unwrap();
    if deny > 0 {
        writeln!(out, "✗ Policy violated (exit 1)").unwrap();
    } else {
        writeln!(out, "✓ Policy respected").unwrap();
    }
    out
}
