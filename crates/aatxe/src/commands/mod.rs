//! Dispatch table for the CLI. Each subcommand lives in its own module and
//! returns an [`Outcome`] so `main` can map it to a process exit code.

use anyhow::Result;

pub mod affected;
pub mod baseline;
pub mod comment;
pub mod compare;
pub mod council;
pub mod evals;
pub mod learn;
pub mod list;
pub mod perf_vs;
pub mod report;
pub mod run;

use crate::cli::{Cli, Command};

/// Process-level outcome. Subcommands that don't have a regression-gate
/// concept always return [`Outcome::Ok`]. The council shares the
/// "fail-on-X" semantics: critical findings map to [`Outcome::Regressions`]
/// so the same CI exit-code contract works for both gates.
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
        Command::Council(a) => council::execute(a),
        Command::Evals(a) => evals::execute(a),
        Command::Learn(a) => learn::execute(a),
        Command::PerfVs(a) => perf_vs::execute(a),
        Command::Baseline(a) => baseline::execute(a),
    }
}
