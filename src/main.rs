mod inventory;
mod policy;
mod report;

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;
use clap::{Parser, Subcommand};

use inventory::Scope;
use policy::Policy;
use report::Evaluated;

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
            let mut evaluated: Vec<Evaluated> = inventory::npm::inventory(&path)?
                .into_iter()
                .filter(|p| include_dev || p.scope == Scope::Prod)
                .map(|p| {
                    let license = p.declared_license.as_deref().and_then(policy::normalize);
                    let outcome = policy.evaluate(license.as_deref());
                    Evaluated {
                        verdict: outcome.verdict,
                        reason: outcome.reason,
                        elected: outcome.elected,
                        license,
                        package: p.package,
                        introduction_path: p.introduction_path,
                    }
                })
                .collect();
            evaluated.sort_by(|a, b| (a.verdict, &a.package).cmp(&(b.verdict, &b.package)));
            let violated = evaluated.iter().any(|e| e.verdict.is_violation(strict));
            print!("{}", report::text(&evaluated, violated));
            Ok(if violated {
                ExitCode::from(1)
            } else {
                ExitCode::SUCCESS
            })
        }
    }
}
