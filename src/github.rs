//! The report of `check --format github`, for GitHub Actions: workflow
//! commands that annotate the Inventory sources with the Violations and the
//! `licguard.toml` with the Warnings, and a Markdown job summary.

use std::env;
use std::fmt::Write as _;
use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::{Component, Path, PathBuf};

use anyhow::{Result, anyhow};

use crate::evaluation::{Evaluated, Evaluation};
use crate::report;
use crate::warning::Warning;

/// Renders one `::error` command per Violation, on the entry of its Package
/// in the first of its Inventory sources, one `::warning` command per
/// Warning, on the Project's `licguard.toml`, then the counts.
pub fn check(evaluation: &Evaluation, project: &Path, strict: bool, violated: bool) -> String {
    let mut out = String::new();
    for e in violations(evaluation, strict) {
        let mut properties = vec![("file", annotation_path(&project.join(&e.sources[0])))];
        if let Some(line) = e.line {
            properties.push(("line", line.to_string()));
        }
        properties.push((
            "title",
            format!(
                "licguard: {} {} {}@{}",
                e.verdict.as_str().to_uppercase(),
                report::license(e),
                e.package.name,
                e.package.version
            ),
        ));
        let message = format!("{}{}", report::license(e), report::details(evaluation, e));
        command(&mut out, "error", &properties, &message);
    }
    for warning in &evaluation.warnings {
        let mut properties = match warning {
            Warning::UnsupportedComponent { source, line, .. } => {
                let mut properties = vec![("file", annotation_path(&project.join(source)))];
                if let Some(line) = line {
                    properties.push(("line", line.to_string()));
                }
                properties
            }
            _ => vec![("file", annotation_path(&project.join("licguard.toml")))],
        };
        properties.push(("title", format!("licguard: {}", warning.kind())));
        command(&mut out, "warning", &properties, &warning.to_string());
    }
    writeln!(
        out,
        "{} — {}",
        report::counts(evaluation),
        report::outcome(violated)
    )
    .unwrap();
    out
}

/// Appends the Markdown summary of the check to the job summary, the file
/// named by `GITHUB_STEP_SUMMARY`, when it is set: the counts, a table of the
/// Violations and the list of Warnings.
pub fn write_summary(evaluation: &Evaluation, strict: bool, violated: bool) -> Result<()> {
    let Some(file) = env::var_os("GITHUB_STEP_SUMMARY") else {
        return Ok(());
    };
    let mut out = format!(
        "## licguard\n\n{} — {}\n\n",
        report::counts(evaluation),
        report::outcome(violated)
    );
    let violations = violations(evaluation, strict);
    if violations.is_empty() {
        out.push_str("No Violation.\n");
    } else {
        out.push_str("| Verdict | License | Package | Via |\n| --- | --- | --- | --- |\n");
    }
    for e in violations {
        let via = e
            .introduction_path
            .as_ref()
            .map_or(String::new(), |path| path.join(" > "));
        writeln!(
            out,
            "| {} | `{}` | `{}@{}` | {via} |",
            e.verdict.as_str().to_uppercase(),
            report::license(e),
            e.package.name,
            e.package.version
        )
        .unwrap();
    }
    if !evaluation.warnings.is_empty() {
        out.push_str("\n### Warnings\n\n");
        for warning in &evaluation.warnings {
            writeln!(out, "- {warning}").unwrap();
        }
    }
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(&file)
        .and_then(|mut summary| summary.write_all(out.as_bytes()))
        .map_err(|err| {
            anyhow!(
                "cannot write the job summary to {}: {err}\nhint: GITHUB_STEP_SUMMARY must name a writable file; unset it to skip the job summary",
                Path::new(&file).display()
            )
        })
}

/// The Violations, by Verdict then Package.
fn violations(evaluation: &Evaluation, strict: bool) -> Vec<&Evaluated> {
    let mut violations: Vec<_> = evaluation
        .evaluated
        .iter()
        .filter(|e| e.verdict.is_violation(strict))
        .collect();
    violations.sort_by(|a, b| (a.verdict, &a.package).cmp(&(b.verdict, &b.package)));
    violations
}

/// Writes the workflow command `::<name> <properties>::<message>`.
fn command(out: &mut String, name: &str, properties: &[(&str, String)], message: &str) {
    let properties: Vec<String> = properties
        .iter()
        .map(|(key, value)| format!("{key}={}", escape_property(value)))
        .collect();
    writeln!(
        out,
        "::{name} {}::{}",
        properties.join(","),
        escape_data(message)
    )
    .unwrap();
}

/// Escapes a workflow command's message.
fn escape_data(text: &str) -> String {
    text.replace('%', "%25")
        .replace('\r', "%0D")
        .replace('\n', "%0A")
}

/// Escapes a workflow command's property value.
fn escape_property(text: &str) -> String {
    escape_data(text).replace(':', "%3A").replace(',', "%2C")
}

/// How an annotation names `path`, with `/` separators: relative to
/// `GITHUB_WORKSPACE` when it is under it, else as is. A relative `path` is
/// thus kept, since GitHub Actions runs steps from the workspace.
fn annotation_path(path: &Path) -> String {
    let workspace = env::var_os("GITHUB_WORKSPACE");
    let path = workspace
        .and_then(|workspace| path.strip_prefix(workspace).ok())
        .unwrap_or(path);
    let path: PathBuf = path
        .components()
        .filter(|c| *c != Component::CurDir)
        .collect();
    let path = path.to_string_lossy();
    if cfg!(windows) {
        path.replace('\\', "/")
    } else {
        path.into_owned()
    }
}
