//! Shared helper for adapters: spawn the runner command and capture stdout.
//!
//! Each language has a default invocation that aatxe ships expecting, plus
//! an env-var override for downstream consumers that wire their own runner.
//!
//! * **TS**: `AATXE_TS_RUNNER`, default `npx --no-install aatxe-ts-runner`
//! * **Go**: `AATXE_GO_RUNNER`, default `go test -bench=. -benchmem -json ./...`
//! * **Rust**: `AATXE_RUST_RUNNER`, default `cargo run --release -q --bin aatxe-rust-runner -- --json`

use crate::adapter::RunSpec;
use aatxe_core::types::Language;
use anyhow::{anyhow, Context, Result};
use std::process::{Command, Stdio};

pub fn run_runner(spec: &RunSpec, lang: Language) -> Result<String> {
    let (env_key, default) = match lang {
        Language::Ts => ("AATXE_TS_RUNNER", "npx --no-install aatxe-ts-runner"),
        Language::Go => (
            "AATXE_GO_RUNNER",
            "go test -bench=. -benchmem -run=^$ -json ./...",
        ),
        Language::Rust => (
            "AATXE_RUST_RUNNER",
            "cargo run --release -q --bin aatxe-rust-runner -- --json",
        ),
    };
    let raw = std::env::var(env_key).unwrap_or_else(|_| default.to_string());
    let tokens: Vec<&str> = raw.split_whitespace().collect();
    let Some((program, args)) = tokens.split_first() else {
        return Err(anyhow!("empty runner command for {:?}", lang));
    };
    let mut cmd = Command::new(program);
    cmd.args(args);
    cmd.current_dir(&spec.cwd);
    if let Some(filter) = spec.filter.as_ref() {
        cmd.env("AATXE_FILTER", filter);
    }
    if !spec.patterns.is_empty() {
        cmd.env("AATXE_PATTERNS", spec.patterns.join(":"));
    }
    if let Some(files) = spec.bench_files.as_ref() {
        let joined: Vec<String> = files.iter().map(|p| p.display().to_string()).collect();
        cmd.env("AATXE_BENCH_FILES", joined.join(":"));
    }
    cmd.env("AATXE_SERVICE", &spec.service);
    cmd.env("AATXE_REF", &spec.r#ref);
    let verbose = spec.verbose;
    cmd.stdin(Stdio::null());
    if verbose {
        cmd.stdout(Stdio::inherit());
    } else {
        cmd.stdout(Stdio::piped());
    }
    cmd.stderr(Stdio::inherit());

    let output = cmd
        .output()
        .with_context(|| format!("spawning runner: {}", raw))?;
    if !output.status.success() {
        return Err(anyhow!(
            "runner exited with status {}",
            output.status.code().unwrap_or(-1)
        ));
    }
    if verbose {
        return Err(anyhow!(
            "--verbose is incompatible with stdout-based RunReport ingestion; \
             re-run without --verbose or pipe through the runner's --out flag"
        ));
    }
    let raw = String::from_utf8(output.stdout).context("runner stdout was not valid UTF-8")?;
    Ok(raw)
}
