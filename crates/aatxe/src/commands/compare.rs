//! `aatxe compare` — pure JSON-in, JSON-out (and optional Markdown).
//!
//! Doing this in the binary instead of as a library call keeps the CI shape
//! simple: run two `aatxe run` invocations producing JSON, then a single
//! `aatxe compare` to derive the verdict.

use crate::cli::CompareArgs;
use crate::commands::Outcome;
use aatxe_core::types::RunReport;
use aatxe_core::{compare_reports, has_regressions, render_markdown, CompareOptions};
use anyhow::{Context, Result};
use std::fs;
use std::path::PathBuf;

pub fn execute(args: CompareArgs) -> Result<Outcome> {
    let base: RunReport = read_report(&args.base)
        .with_context(|| format!("reading base report from {}", args.base.display()))?;
    let head: RunReport = read_report(&args.head)
        .with_context(|| format!("reading head report from {}", args.head.display()))?;

    let opts = CompareOptions {
        threshold_pct: args.threshold,
        alpha: args.alpha,
        noisy_cv_threshold: args.noisy_cv,
    };
    let cmp = compare_reports(&base, &head, opts);

    let out = args
        .out
        .unwrap_or_else(|| PathBuf::from("./aatxe-report.json"));
    let json = serde_json::to_string_pretty(&cmp)?;
    fs::write(&out, json).with_context(|| format!("writing {}", out.display()))?;
    println!("wrote compare report → {}", out.display());

    if let Some(md_out) = args.markdown.as_ref() {
        let body = render_markdown(&cmp);
        fs::write(md_out, body)
            .with_context(|| format!("writing markdown to {}", md_out.display()))?;
        println!("wrote markdown body → {}", md_out.display());
    }

    summary_to_stderr(&cmp);

    if args.fail_on_regression && has_regressions(&cmp) {
        return Ok(Outcome::Regressions);
    }
    Ok(Outcome::Ok)
}

fn read_report(p: &PathBuf) -> Result<RunReport> {
    let raw = fs::read_to_string(p)?;
    let parsed = serde_json::from_str(&raw)?;
    Ok(parsed)
}

fn summary_to_stderr(cmp: &aatxe_core::types::CompareReport) {
    let s = &cmp.summary;
    eprintln!(
        "aatxe summary: {} regression(s) · {} improvement(s) · {} neutral · {} new · {} removed · {} out-of-scope",
        s.regressions, s.improvements, s.neutrals, s.new, s.removed, s.out_of_scope,
    );
}
