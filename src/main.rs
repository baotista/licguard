mod cache;
mod clarification;
mod date;
mod ecosystem;
mod evaluation;
mod github;
mod init;
mod inventory;
mod json;
mod normalize;
mod policy;
mod registry;
mod report;
mod table;
mod timing;
mod waive;
mod waiver;
mod warning;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use anyhow::{Result, anyhow, bail};
use clap::{Parser, Subcommand};

use evaluation::Remote;
use table::GroupBy;

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

/// The help text of the commands that evaluate the Project.
const EVALUATION_HELP: &str = "Waiver expiry is evaluated as of today (local date); set LICGUARD_TODAY=YYYY-MM-DD to evaluate it as of another date, e.g. to re-run an old CI job.

Packages that no local License origin declares a license for get it from the npm registry, https://registry.npmjs.org, unless --offline; set LICGUARD_NPM_REGISTRY=URL to query another one, e.g. a mirror.

The registry's answers are kept in a license cache, one file per registry, and reused by later runs, --offline ones included; published versions never change, so they never expire (--refresh requests them again). A 404 is not cached. The cache is in the directory --cache-dir names, else in the one LICGUARD_CACHE_DIR=DIR names, else in `licguard` under the user's cache directory (~/.cache or XDG_CACHE_HOME on Linux, ~/Library/Caches on macOS, %LOCALAPPDATA% on Windows).";

#[derive(Subcommand)]
enum Command {
    /// Evaluate the Project against its Policy and set the exit code
    #[command(after_help = EVALUATION_HELP)]
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
        #[command(flatten)]
        remote: Remote,
        /// Print how long the run took and how many registry requests it sent on stderr, as when stderr is a terminal
        #[arg(long)]
        timings: bool,
    },
    /// Show every Package with its license, Verdict and License origin
    #[command(after_help = EVALUATION_HELP)]
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
        #[command(flatten)]
        remote: Remote,
        /// Print how long the run took and how many registry requests it sent on stderr, as when stderr is a terminal
        #[arg(long)]
        timings: bool,
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
    /// Write a Waiver to licguard.toml for each current Violation
    #[command(after_help = EVALUATION_HELP)]
    Waive {
        /// Project directory
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Waive every current Violation (required: the only mode for now)
        #[arg(long)]
        all_violations: bool,
        /// Why the Violations are tolerated (required)
        #[arg(long, value_name = "TEXT")]
        reason: Option<String>,
        /// The last day the Waivers apply, written YYYY-MM-DD (required)
        #[arg(long, value_name = "YYYY-MM-DD")]
        expires: Option<String>,
        /// Also waive `review` Verdicts, as `check --strict` fails on them
        #[arg(long)]
        strict: bool,
        /// Also evaluate `dev` Dependencies
        #[arg(long)]
        include_dev: bool,
        #[command(flatten)]
        remote: Remote,
        /// Print how long the run took and how many registry requests it sent on stderr, as when stderr is a terminal
        #[arg(long)]
        timings: bool,
    },
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum CheckFormat {
    Text,
    Json,
    Github,
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum ListFormat {
    Table,
    Json,
}

fn main() -> ExitCode {
    let start = Instant::now();
    let cli = Cli::parse();
    match run(cli, start) {
        Ok(code) => code,
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::from(2)
        }
    }
}

/// Runs the command of `cli`; `start` is when the run started, for its
/// timing line.
fn run(cli: Cli, start: Instant) -> Result<ExitCode> {
    match cli.command {
        Command::Check {
            path,
            strict,
            include_dev,
            format,
            output,
            remote,
            timings,
        } => {
            let evaluation = evaluation::evaluate(&path, include_dev, &remote)?;
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
            timing::print(timings, start, &evaluation);
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
            remote,
            timings,
        } => {
            if matches!(format, ListFormat::Json) && group_by.is_some() {
                bail!(
                    "`--group-by` applies only to `--format table`\nhint: remove `--group-by`; JSON consumers can group the packages themselves"
                );
            }
            let evaluation = evaluation::evaluate(&path, include_dev, &remote)?;
            let out = match format {
                ListFormat::Table => table::table(&evaluation, group_by),
                ListFormat::Json => json::list(&evaluation),
            };
            emit(&out, output.as_deref())?;
            timing::print(timings, start, &evaluation);
            Ok(ExitCode::SUCCESS)
        }
        Command::Init { path, force } => {
            print!("{}", init::init(&path, force)?);
            Ok(ExitCode::SUCCESS)
        }
        Command::Waive {
            path,
            all_violations,
            reason,
            expires,
            strict,
            include_dev,
            remote,
            timings,
        } => {
            let (code, evaluation) = waive::waive(
                &path,
                all_violations,
                reason.as_deref(),
                expires.as_deref(),
                strict,
                include_dev,
                &remote,
            )?;
            timing::print(timings, start, &evaluation);
            Ok(code)
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
