//! Rust adapter: invoke a `cargo`-based runner that uses the `aatxe-bench`
//! SDK.
//!
//! For now this delegates to a generic runner shape — the user supplies the
//! command via `AATXE_RUST_RUNNER` (whitespace-separated). The default uses
//! `cargo run --release --bin aatxe-rust-runner -- --json` if present.

use crate::adapter::RunSpec;
use aatxe_core::types::{Language, RunReport, SCHEMA_VERSION};
use anyhow::{anyhow, Context, Result};

pub fn execute(spec: &RunSpec) -> Result<RunReport> {
    let raw = super::ts_or_runner::run_runner(spec, Language::Rust)?;
    let mut report: RunReport =
        serde_json::from_str(&raw).context("rust runner did not produce a valid RunReport JSON")?;
    if report.schema_version == 0 {
        report.schema_version = SCHEMA_VERSION;
    }
    if report.language != Language::Rust {
        return Err(anyhow!(
            "rust runner reported language={:?}",
            report.language
        ));
    }
    Ok(report)
}
