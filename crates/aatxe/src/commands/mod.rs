//! Dispatch table for the CLI. Each subcommand lives in its own module and
//! returns an [`Outcome`] so `main` can map it to a process exit code.

use anyhow::Result;

pub mod affected;
pub mod baseline;
pub mod comment;
pub mod compare;
pub mod evals;
pub mod list;
pub mod perf_vs;
pub mod report;
pub mod run;
#[cfg(feature = "ui")]
pub mod ui;

use crate::cli::{Cli, Command};

/// Process-level outcome. Subcommands that don't have a regression-gate
/// concept always return [`Outcome::Ok`].
pub enum Outcome {
    Ok,
    Regressions,
}

pub fn run(cli: Cli) -> Result<Outcome> {
    match cli.command {
        Command::Run(a) => run::execute(a).map(|_| Outcome::Ok),
        Command::Compare(a) => compare::execute(a),
        Command::Report(a) => report::execute(a).map(|_| Outcome::Ok),
        Command::Comment(a) => comment::execute(a).map(|_| Outcome::Ok),
        Command::Affected(a) => affected::execute(a).map(|_| Outcome::Ok),
        Command::List(a) => list::execute(a).map(|_| Outcome::Ok),
        Command::Evals(a) => evals::execute(a),
        Command::PerfVs(a) => perf_vs::execute(a),
        Command::Baseline(a) => baseline::execute(a),
        #[cfg(feature = "ui")]
        Command::Ui(a) => ui::execute(a).map(|_| Outcome::Ok),
    }
}
