//! `aatxe compare` — pure JSON-in, JSON-out (and optional Markdown).
//!
//! Doing this in the binary instead of as a library call keeps the CI shape
//! simple: run two `aatxe run` invocations producing JSON, then a single
//! `aatxe compare` to derive the verdict.

use crate::cli::CompareArgs;
use crate::commands::Outcome;
use aatxe_core::types::RunReport;
use aatxe_core::{compare_reports, has_regressions, merge_rounds, render_markdown, CompareOptions};
use anyhow::{anyhow, Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

pub fn execute(args: CompareArgs) -> Result<Outcome> {
    // clap guarantees exactly one of --base / --against-local is present.
    let base_paths = if args.base.is_empty() {
        let p = crate::commands::baseline::resolve_for_compare(
            args.baseline_dir.as_deref(),
            &args.baseline_name,
        )?;
        eprintln!(
            "aatxe compare: base = local baseline '{}' ({})",
            args.baseline_name,
            p.display()
        );
        vec![p]
    } else {
        args.base.clone()
    };
    let base = read_side(&base_paths, "base")?;
    let head = read_side(&args.head, "head")?;

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

fn read_report(p: &Path) -> Result<RunReport> {
    let raw = fs::read_to_string(p)?;
    let parsed = serde_json::from_str(&raw)?;
    Ok(parsed)
}

/// Read one side's reports and pool them into a single multi-round report.
fn read_side(paths: &[PathBuf], side: &str) -> Result<RunReport> {
    let reports = paths
        .iter()
        .map(|p| {
            read_report(p).with_context(|| format!("reading {side} report from {}", p.display()))
        })
        .collect::<Result<Vec<_>>>()?;
    merge_rounds(&reports).ok_or_else(|| anyhow!("no {side} reports given"))
}

fn summary_to_stderr(cmp: &aatxe_core::types::CompareReport) {
    let s = &cmp.summary;
    eprintln!(
        "aatxe summary: {} regression(s) · {} improvement(s) · {} neutral · {} new · {} removed · {} out-of-scope",
        s.regressions, s.improvements, s.neutrals, s.new, s.removed, s.out_of_scope,
    );
}
