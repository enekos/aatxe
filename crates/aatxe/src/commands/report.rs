//! `aatxe report` — render a CompareReport JSON to a Markdown body.

use crate::cli::ReportArgs;
use aatxe_core::render_markdown;
use aatxe_core::types::CompareReport;
use anyhow::{Context, Result};
use std::fs;
use std::path::PathBuf;

pub fn execute(args: ReportArgs) -> Result<()> {
    let raw = fs::read_to_string(&args.diff)
        .with_context(|| format!("reading {}", args.diff.display()))?;
    let cmp: CompareReport = serde_json::from_str(&raw).context("parsing CompareReport JSON")?;
    let body = render_markdown(&cmp);
    let out = args
        .out
        .unwrap_or_else(|| PathBuf::from("./aatxe-report.md"));
    fs::write(&out, body).with_context(|| format!("writing {}", out.display()))?;
    println!("wrote markdown → {}", out.display());
    Ok(())
}
