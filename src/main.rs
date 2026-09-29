mod clarification;
mod date;
mod evaluation;
mod github;
mod init;
mod inventory;
mod json;
mod normalize;
mod policy;
mod report;
mod table;
mod waiver;
mod warning;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Result, anyhow, bail};
use clap::{Parser, Subcommand};

use table::GroupBy;

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

/// The help text of the commands that evaluate Waivers.
const TODAY_HELP: &str = "Waiver expiry is evaluated as of today (UTC); set LICGUARD_TODAY=YYYY-MM-DD to evaluate it as of another date, e.g. to re-run an old CI job.";

#[derive(Subcommand)]
enum Command {
    /// Evaluate the Project against its Policy and set the exit code
    #[command(after_help = TODAY_HELP)]
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
        /// Output format
        #[arg(long, value_enum, default_value_t = CheckFormat::Text)]
        format: CheckFormat,
        /// Write the report to this file instead of stdout
        #[arg(long, value_name = "FILE")]
        output: Option<PathBuf>,
    },
    /// Show every Package with its license, Verdict and License origin
    #[command(after_help = TODAY_HELP)]
    List {
        /// Project directory
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Also list `dev` Dependencies
        #[arg(long)]
        include_dev: bool,
        /// Output format
        #[arg(long, value_enum, default_value_t = ListFormat::Table)]
        format: ListFormat,
        /// Group the table's lines into sections
        #[arg(long, value_enum)]
        group_by: Option<GroupBy>,
        /// Write the inventory to this file instead of stdout
        #[arg(long, value_name = "FILE")]
        output: Option<PathBuf>,
    },
    /// Write a neutral template Policy to the Project's licguard.toml
    Init {
        /// Project directory
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Overwrite an existing licguard.toml
        #[arg(long)]
        force: bool,
    },
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum CheckFormat {
    Text,
    Json,
    /// GitHub Actions annotations and job summary
    Github,
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum ListFormat {
    Table,
    Json,
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
            format,
            output,
        } => {
            let evaluation = evaluation::evaluate(&path, include_dev)?;
            let violated = evaluation
                .evaluated
                .iter()
                .any(|e| e.verdict.is_violation(strict));
            let out = match format {
                CheckFormat::Text => report::text(&evaluation, violated),
                CheckFormat::Json => json::check(&evaluation, strict, violated),
                CheckFormat::Github => {
                    github::write_summary(&evaluation, strict, violated)?;
                    github::check(&evaluation, &path, strict, violated)
                }
            };
            emit(&out, output.as_deref())?;
            Ok(if violated {
                ExitCode::from(1)
            } else {
                ExitCode::SUCCESS
            })
        }
        Command::List {
            path,
            include_dev,
            format,
            group_by,
            output,
        } => {
            if matches!(format, ListFormat::Json) && group_by.is_some() {
                bail!(
                    "`--group-by` applies only to `--format table`\nhint: remove `--group-by`; JSON consumers can group the packages themselves"
                );
            }
            let evaluation = evaluation::evaluate(&path, include_dev)?;
            let out = match format {
                ListFormat::Table => table::table(&evaluation, group_by),
                ListFormat::Json => json::list(&evaluation),
            };
            emit(&out, output.as_deref())?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Init { path, force } => {
            print!("{}", init::init(&path, force)?);
            Ok(ExitCode::SUCCESS)
        }
    }
}

/// Writes `out` to the `output` file, else to stdout.
fn emit(out: &str, output: Option<&Path>) -> Result<()> {
    match output {
        Some(file) => fs::write(file, out).map_err(|err| {
            anyhow!(
                "cannot write {}: {err}\nhint: check that its directory exists and is writable",
                file.display()
            )
        }),
        None => {
            print!("{out}");
            Ok(())
        }
    }
}
