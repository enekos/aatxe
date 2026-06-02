//! TypeScript adapter: invoke the `@aatxe/bench` Node.js runner.
//!
//! The Node-side SDK (`sdk/ts`) exposes a `bench(name, fn, opts?)` API and a
//! `aatxe-ts-runner` CLI that loads `*.bench.ts` files, runs the registered
//! benches with the standard sampler, and prints a [`RunReport`] JSON to
//! stdout. Aatxe just deserialises that JSON.
//!
//! Override the invocation via the `AATXE_TS_RUNNER` env var (whitespace-
//! separated tokens). Default: `npx --no-install aatxe-ts-runner`.

use crate::adapter::RunSpec;
use aatxe_core::types::{Language, RunReport, SCHEMA_VERSION};
use anyhow::{anyhow, Context, Result};

pub fn execute(spec: &RunSpec) -> Result<RunReport> {
    let raw = super::ts_or_runner::run_runner(spec, Language::Ts)?;
    let mut report: RunReport =
        serde_json::from_str(&raw).context("ts runner did not produce a valid RunReport JSON")?;
    if report.schema_version == 0 {
        report.schema_version = SCHEMA_VERSION;
    }
    if report.language != Language::Ts {
        return Err(anyhow!("ts runner reported language={:?}", report.language));
    }
    Ok(report)
}
