mod clarification;
mod inventory;
mod normalize;
mod policy;
mod report;
mod warning;

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;
use clap::{Parser, Subcommand};

use inventory::Scope;
use policy::Policy;
use report::Evaluated;
use warning::Warning;

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Evaluate the Project against its Policy and set the exit code
    Check {
        /// Project directory
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Treat `review` Verdicts as Violations
        #[arg(long)]
        strict: bool,
        /// Also evaluate `dev` Dependencies
        #[arg(long)]
        include_dev: bool,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(code) => code,
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::from(2)
        }
    }
}

fn run(cli: Cli) -> Result<ExitCode> {
    match cli.command {
        Command::Check {
            path,
            strict,
            include_dev,
        } => {
            let policy = Policy::load(&path)?;
            let include_dev = include_dev || policy.include_dev;
            let inventory = inventory::inventory(&path)?;
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
            warnings.sort();
            // With a single Inventory source, naming it adds nothing.
            let several_sources = inventory.sources.len() > 1;
            let mut evaluated: Vec<Evaluated> = inventory
                .packages
                .into_iter()
                .filter(|p| include_dev || p.scope == Scope::Prod)
                .map(|p| {
                    let clarification = clarification::find(&policy.clarifications, &p.package);
                    // A clarification's license is already a Normalized license.
                    let license = match clarification {
                        Some(c) => Some(c.license.clone()),
                        None => p.declared_license.as_deref().and_then(normalize::normalize),
                    };
                    let outcome = policy.evaluate(license.as_deref());
                    Evaluated {
                        verdict: outcome.verdict,
                        reason: outcome.reason,
                        elected: outcome.elected,
                        license,
                        clarified: clarification.is_some(),
                        package: p.package,
                        introduction_path: p.introduction_path,
                        sources: if several_sources {
                            p.sources
                        } else {
                            Vec::new()
                        },
                    }
                })
                .collect();
            evaluated.sort_by(|a, b| (a.verdict, &a.package).cmp(&(b.verdict, &b.package)));
            let violated = evaluated.iter().any(|e| e.verdict.is_violation(strict));
            print!("{}", report::text(&evaluated, &warnings, violated));
            Ok(if violated {
                ExitCode::from(1)
            } else {
                ExitCode::SUCCESS
            })
        }
    }
}
