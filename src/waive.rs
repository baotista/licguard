//! `licguard waive`: turns the current Violations into Waivers, each with a
//! reason and an expiry date (ADR-0001), editing `licguard.toml` in place.

use std::fs;
use std::path::Path;
use std::process::ExitCode;

use anyhow::{Result, anyhow, bail};
use toml_edit::{ArrayOfTables, DocumentMut, Item, Table, value};

use crate::date::Date;
use crate::evaluation::{self, Evaluated};
use crate::policy::CONFIG;

/// Writes a Waiver for each Violation of the Project at `root`, as `check`
/// with the same `strict`, `include_dev` and `offline` would report them, and
/// prints what it did. Fails (exit 1) when an Unresolved Violation could not
/// be waived.
pub fn waive(
    root: &Path,
    all_violations: bool,
    reason: Option<&str>,
    expires: Option<&str>,
    strict: bool,
    include_dev: bool,
    offline: bool,
) -> Result<ExitCode> {
    if !all_violations {
        bail!(
            "`--all-violations` is required\nhint: it is the only supported mode for now: pass `--all-violations` to waive every current Violation"
        );
    }
    const REASON_HINT: &str = "hint: state why the Violations are tolerated, e.g. `--reason \"Approved by legal, ticket LEGAL-142\"`";
    let Some(reason) = reason else {
        bail!("`--reason` is required\n{REASON_HINT}");
    };
    let reason = reason.trim();
    if reason.is_empty() {
        bail!("`--reason` is blank\n{REASON_HINT}");
    }
    const EXPIRES_HINT: &str =
        "hint: set the last day the Waivers apply, e.g. `--expires 2027-01-01`";
    let Some(expires) = expires else {
        bail!("`--expires` is required\n{EXPIRES_HINT}");
    };
    let Some(expires) = Date::parse(expires) else {
        bail!(
            "`--expires` `{expires}` is not a calendar date written `YYYY-MM-DD`\n{EXPIRES_HINT}"
        );
    };
    let today = Date::today()?;
    if expires < today {
        bail!("`--expires` `{expires}` is before today ({today})\n{EXPIRES_HINT}");
    }
    let expires = &expires.to_string();
    let evaluation = evaluation::evaluate(root, include_dev, offline)?;
    // Sorted by Package, like the Evaluation, so new entries are too.
    let violations: Vec<_> = evaluation
        .evaluated
        .iter()
        .filter(|e| e.verdict.is_violation(strict))
        .collect();
    if violations.is_empty() {
        println!("nothing to waive");
        return Ok(ExitCode::SUCCESS);
    }
    // A Waiver needs a Normalized license: an Unresolved Package needs a
    // License clarification instead.
    let (unresolved, waivable): (Vec<_>, Vec<_>) =
        violations.into_iter().partition(|e| e.license.is_none());
    if !waivable.is_empty() {
        write(root, &waivable, reason, expires)?;
    }
    if unresolved.is_empty() {
        return Ok(ExitCode::SUCCESS);
    }
    for e in &unresolved {
        eprintln!(
            "cannot waive {}@{}: its license is Unresolved",
            e.package.name, e.package.version
        );
    }
    eprintln!(
        "hint: add a License clarification with evidence of its real license, then run `licguard waive` again"
    );
    Ok(ExitCode::from(1))
}

/// Writes a Waiver for each of `violations` to the Project's `licguard.toml`,
/// renewing an existing one for the same package and version instead, and
/// prints what it did.
fn write(root: &Path, violations: &[&Evaluated], reason: &str, expires: &str) -> Result<()> {
    let path = root.join(CONFIG);
    let text = fs::read_to_string(&path).map_err(|err| {
        anyhow!(
            "cannot read {}: {err}\nhint: check that it is readable",
            path.display()
        )
    })?;
    let mut document: DocumentMut = text.parse()?;
    let Some(waivers) = document
        .entry("waivers")
        .or_insert(Item::ArrayOfTables(ArrayOfTables::new()))
        .as_array_of_tables_mut()
    else {
        bail!(
            "{}: `waivers` is not written as `[[waivers]]` tables\nhint: rewrite each Waiver as a `[[waivers]]` table, as `licguard init` does, so that `licguard waive` can add to them",
            path.display()
        );
    };
    let (mut added, mut renewed) = (0, 0);
    for e in violations {
        let package = &e.package;
        let license = e
            .license
            .as_deref()
            .expect("an Unresolved Package is never waived");
        // A second Waiver for the same package and version would be a
        // configuration error: renew the existing one instead.
        let existing = waivers.iter_mut().find(|w| {
            w.get("package").and_then(Item::as_str) == Some(package.name.as_str())
                && w.get("version").and_then(Item::as_str) == Some(package.version.as_str())
        });
        match existing {
            Some(waiver) => {
                for (key, text) in [
                    ("license", license),
                    ("reason", reason),
                    ("expires", expires),
                ] {
                    set(waiver, key, text);
                }
                renewed += 1;
            }
            None => {
                let mut waiver = Table::new();
                waiver["package"] = value(&package.name);
                waiver["version"] = value(&package.version);
                waiver["license"] = value(license);
                waiver["reason"] = value(reason);
                waiver["expires"] = value(expires);
                waivers.push(waiver);
                added += 1;
            }
        }
    }
    fs::write(&path, document.to_string()).map_err(|err| {
        anyhow!(
            "cannot write {}: {err}\nhint: check that it is writable",
            path.display()
        )
    })?;
    let noun = if added == 1 { "Waiver" } else { "Waivers" };
    println!(
        "added {added} {noun}, renewed {renewed} in {}",
        path.display()
    );
    Ok(())
}

/// Sets `table[key]` to the string `text`, keeping the comments and spacing
/// around a value it replaces.
fn set(table: &mut Table, key: &str, text: &str) {
    match table.get_mut(key).and_then(Item::as_value_mut) {
        Some(old) => {
            let decor = old.decor().clone();
            *old = text.into();
            *old.decor_mut() = decor;
        }
        None => table[key] = value(text),
    }
}
