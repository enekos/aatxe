//! Aatxe CLI entrypoint. Subcommands live under [`commands`].
//!
//! Exit codes:
//! * `0` — success, no regressions (or `--fail-on-regression` was not set).
//! * `1` — usage or runtime error.
//! * `2` — regressions detected and `--fail-on-regression` was passed.

mod adapter;
mod ast_scope;
mod cli;
mod commands;
mod gh_diff;
mod github_http;
mod kimi_http;
mod stub_client;

use std::process::ExitCode;

fn main() -> ExitCode {
    let cli = <cli::Cli as clap::Parser>::parse();
    match commands::run(cli) {
        Ok(commands::Outcome::Ok) => ExitCode::from(0),
        Ok(commands::Outcome::Regressions) => ExitCode::from(2),
        Err(e) => {
            eprintln!("aatxe: {e:#}");
            ExitCode::from(1)
        }
    }
}
