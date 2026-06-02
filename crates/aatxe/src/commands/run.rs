//! `aatxe run` — orchestrate a language adapter to produce a RunReport.
//!
//! Aatxe doesn't time benches itself; per-language adapters (in
//! [`crate::adapter`]) shell out to the native runner (node/npx, `go test
//! -bench`, `cargo bench`) and either ingest a JSON the runner produced or
//! parse the runner's text output into a [`RunReport`].

use crate::adapter::{self, RunSpec};
use crate::cli::RunArgs;
use aatxe_core::types::AffectedScope;
use anyhow::{Context, Result};
use std::fs;
use std::path::PathBuf;

pub fn execute(args: RunArgs) -> Result<()> {
    let cwd = args
        .cwd
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap());
    let lang = args.lang.to_core();
    let service = args.service.clone().unwrap_or_else(|| {
        cwd.file_name()
            .and_then(|s| s.to_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "unknown".to_string())
    });
    let r#ref = args
        .r#ref
        .clone()
        .or_else(|| adapter::detect_git_ref(&cwd))
        .unwrap_or_else(|| "HEAD".to_string());

    // Optional --affected scoping.
    let affected_scope: Option<AffectedScope> = if args.affected {
        let base = args
            .base
            .clone()
            .context("--affected requires --base <ref>")?;
        let scope = adapter::resolve_affected_scope(&cwd, lang, &base, &args.patterns)?;
        Some(scope)
    } else {
        None
    };

    let bench_files: Option<Vec<PathBuf>> = affected_scope
        .as_ref()
        .map(|s| s.bench_files.iter().map(PathBuf::from).collect());

    let spec = RunSpec {
        cwd: cwd.clone(),
        service,
        r#ref,
        filter: args.filter.clone(),
        patterns: args.patterns.clone(),
        bench_files,
        verbose: args.verbose,
    };

    let mut report = adapter::execute(lang, &spec)?;
    report.affected_scope = affected_scope;
    let out = args.out.unwrap_or_else(|| PathBuf::from("./aatxe.json"));
    let json = serde_json::to_string_pretty(&report)?;
    fs::write(&out, json).with_context(|| format!("writing {}", out.display()))?;
    println!(
        "wrote {} bench result(s) → {} (lang={}, service={})",
        report.runs.len(),
        out.display(),
        lang.label(),
        report.service,
    );
    Ok(())
}
