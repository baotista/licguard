//! The terminal table of `list`: the full inventory.

use std::collections::BTreeMap;

use crate::evaluation::{Evaluated, Evaluation};
use crate::report;

/// How `list` groups the lines of its table into sections.
#[derive(Clone, Copy, clap::ValueEnum)]
pub enum GroupBy {
    /// One section per Normalized license, alphabetically, Unresolved last
    License,
    /// One section per Verdict, most severe first
    Verdict,
}

/// Renders one line per Package, sorted by Package, in sections when
/// `group_by` is set. The columns are aligned across sections.
pub fn table(evaluation: &Evaluation, group_by: Option<GroupBy>) -> String {
    let rows: Vec<Vec<String>> = evaluation
        .evaluated
        .iter()
        .map(|e| row(e, evaluation.several_sources()))
        .collect();
    let columns = rows.first().map_or(0, Vec::len);
    let widths: Vec<usize> = (0..columns)
        .map(|i| rows.iter().map(|r| r[i].chars().count()).max().unwrap_or(0))
        .collect();
    let line = |row: &[String]| {
        let cells: Vec<String> = row
            .iter()
            .zip(&widths)
            .map(|(cell, width)| format!("{cell:<width$}"))
            .collect();
        format!("{}\n", cells.join("  ").trim_end())
    };

    let mut out = report::header(&evaluation.evaluated);
    let Some(group_by) = group_by else {
        for row in &rows {
            out.push_str(&line(row));
        }
        return out;
    };
    let sections: Vec<(String, Vec<&Vec<String>>)> = match group_by {
        GroupBy::License => {
            // Keyed by (whether Unresolved, license) to put Unresolved last.
            let mut sections = BTreeMap::new();
            for (e, row) in evaluation.evaluated.iter().zip(&rows) {
                let key = (e.license.is_none(), license(e));
                sections.entry(key).or_insert_with(Vec::new).push(row);
            }
            sections
                .into_iter()
                .map(|((_, license), rows)| (license.to_string(), rows))
                .collect()
        }
        GroupBy::Verdict => {
            let mut sections = BTreeMap::new();
            for (e, row) in evaluation.evaluated.iter().zip(&rows) {
                sections.entry(e.verdict).or_insert_with(Vec::new).push(row);
            }
            sections
                .into_iter()
                .map(|(verdict, rows)| (verdict.as_str().to_uppercase(), rows))
                .collect()
        }
    };
    for (i, (title, rows)) in sections.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str(title);
        out.push('\n');
        for row in rows {
            out.push_str(&line(row));
        }
    }
    out
}

/// The Normalized license, or `(unresolved)`.
fn license(e: &Evaluated) -> &str {
    e.license.as_deref().unwrap_or("(unresolved)")
}

/// The cells of a Package's line: Verdict, Normalized license, Package,
/// Verdict reason, License origin, Introduction path and, when `sources`,
/// its Inventory sources.
fn row(e: &Evaluated, sources: bool) -> Vec<String> {
    let mut cells = vec![
        e.verdict.as_str().to_uppercase(),
        license(e).to_string(),
        format!("{}@{}", e.package.name, e.package.version),
        e.reason.as_str().to_string(),
        e.origin.map_or("-", |o| o.as_str()).to_string(),
        e.introduction_path
            .as_ref()
            .map_or(String::new(), |path| format!("via {}", path.join(" > "))),
    ];
    if sources {
        cells.push(format!("in {}", e.sources.join(", ")));
    }
    cells
}
