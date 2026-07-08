//! Shared helper for adapters: spawn the runner command and capture stdout.
//!
//! Each language has a default invocation that aatxe ships expecting, plus
//! an env-var override for downstream consumers that wire their own runner.
//!
//! * **TS**: `AATXE_TS_RUNNER`, default `npx --no-install aatxe-ts-runner`
//! * **Go**: `AATXE_GO_RUNNER`, default `go test -bench=. -benchmem -json ./...`
//! * **Rust**: `AATXE_RUST_RUNNER`, default `cargo run --release -q --bin aatxe-rust-runner -- --json`
//!
//! The command runs under [`crate::sandbox::Isolation`], so the same code
//! path serves both a plain host run and an in-microVM run — the caller
//! picks via `--isolation`. Either way we capture stdout (the RunReport) and
//! let stderr stream through.

use crate::adapter::RunSpec;
use crate::sandbox;
use aatxe_core::types::Language;
use anyhow::{anyhow, Context, Result};

pub fn run_runner(spec: &RunSpec, lang: Language) -> Result<String> {
    // Streaming stdout and ingesting a RunReport off stdout are mutually
    // exclusive regardless of isolation — bail early rather than run first.
    if spec.verbose {
        return Err(anyhow!(
            "--verbose is incompatible with stdout-based RunReport ingestion; \
             re-run without --verbose or pipe through the runner's --out flag"
        ));
    }

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

    let mut env: Vec<(String, String)> = Vec::new();
    if let Some(filter) = spec.filter.as_ref() {
        env.push(("AATXE_FILTER".to_string(), filter.clone()));
    }
    if !spec.patterns.is_empty() {
        env.push(("AATXE_PATTERNS".to_string(), spec.patterns.join(":")));
    }
    if let Some(files) = spec.bench_files.as_ref() {
        let joined: Vec<String> = files.iter().map(|p| p.display().to_string()).collect();
        env.push(("AATXE_BENCH_FILES".to_string(), joined.join(":")));
    }
    env.push(("AATXE_SERVICE".to_string(), spec.service.clone()));
    env.push(("AATXE_REF".to_string(), spec.r#ref.clone()));

    let script = sandbox::exec_line(program, args);
    let output = spec
        .isolation
        .run_script(&spec.cwd, &script, &env)
        .with_context(|| format!("running bench runner: {raw}"))?;
    if !output.status.success() {
        return Err(anyhow!(
            "runner exited with status {}",
            output.status.code().unwrap_or(-1)
        ));
    }
    let raw_out = String::from_utf8(output.stdout).context("runner stdout was not valid UTF-8")?;
    Ok(raw_out)
}
