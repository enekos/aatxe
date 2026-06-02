//! Go adapter: invoke `go test -bench=. -benchmem -json` (or the runner
//! published in `sdk/go`) and translate its output into a [`RunReport`].
//!
//! `go test -bench` emits one JSON event per test/bench, including per-N
//! timings on `Output` lines. We support two formats:
//!
//! 1. **Native Go event stream** — produced by `go test -bench=. -json` with
//!    no SDK changes required. Aatxe parses the per-N timings the standard
//!    `testing.B` library prints, computes derived statistics on its end.
//! 2. **`@aatxe/bench` Go SDK** — produces a fully-formed `RunReport` JSON
//!    in one shot (preferred; mirrors the TS adapter).
//!
//! When the runner output starts with `{"schemaVersion"`, it's the SDK; we
//! deserialise directly. Otherwise we treat it as a Go test event stream.

use crate::adapter::RunSpec;
use aatxe_core::stats::summarize_samples;
use aatxe_core::types::{BenchRun, Language, RunReport, SCHEMA_VERSION};
use anyhow::{anyhow, Result};
use serde::Deserialize;
use std::collections::HashMap;

pub fn execute(spec: &RunSpec) -> Result<RunReport> {
    let raw = super::ts_or_runner::run_runner(spec, Language::Go)?;
    let trimmed = raw.trim_start();
    if trimmed.starts_with("{\"schemaVersion\"") || trimmed.starts_with("{\n  \"schemaVersion\"") {
        let mut report: RunReport = serde_json::from_str(&raw)?;
        if report.schema_version == 0 {
            report.schema_version = SCHEMA_VERSION;
        }
        if report.language != Language::Go {
            return Err(anyhow!("go runner reported language={:?}", report.language));
        }
        return Ok(report);
    }
    // Parse Go's structured test output.
    parse_go_test_events(&raw, spec)
}

#[derive(Debug, Deserialize)]
struct GoEvent {
    #[serde(rename = "Action")]
    action: String,
    #[serde(rename = "Test")]
    test: Option<String>,
    #[serde(rename = "Output")]
    output: Option<String>,
    #[serde(rename = "Package")]
    package: Option<String>,
}

/// Translate Go test events into a [`RunReport`]. We accumulate
/// per-iteration timings emitted by `b.ReportMetric` or the default
/// `b.N ns/op` output lines.
fn parse_go_test_events(raw: &str, spec: &RunSpec) -> Result<RunReport> {
    let started_at = super::now_iso8601();
    let mut by_name: HashMap<String, Vec<f64>> = HashMap::new();
    let mut files: HashMap<String, String> = HashMap::new();

    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() || !line.starts_with('{') {
            continue;
        }
        let Ok(evt) = serde_json::from_str::<GoEvent>(line) else {
            continue;
        };
        if evt.action != "output" {
            continue;
        }
        let Some(out) = evt.output else { continue };
        let Some(name) = parse_bench_name(&out).or(evt.test) else {
            continue;
        };
        if let Some(pkg) = evt.package.as_ref() {
            files.entry(name.clone()).or_insert_with(|| pkg.clone());
        }
        if let Some(ns) = parse_ns_per_op(&out) {
            by_name.entry(name).or_default().push(ns);
        }
    }

    let mut runs: Vec<BenchRun> = Vec::with_capacity(by_name.len());
    for (name, samples) in by_name {
        if samples.is_empty() {
            continue;
        }
        let s = summarize_samples(&samples);
        let elapsed: f64 = samples.iter().sum();
        let file = files.get(&name).cloned().unwrap_or_default();
        runs.push(BenchRun {
            iterations: samples.len() as u32,
            batch_size: 1,
            elapsed_ns: elapsed,
            samples: samples.clone(),
            name,
            file,
            mean: s.mean,
            median: s.median,
            trimmed_mean: s.trimmed_mean,
            stddev: s.stddev,
            cv: s.cv,
            mad: s.mad,
            iqr: s.iqr,
            min: s.min,
            max: s.max,
            p50: s.p50,
            p95: s.p95,
            p99: s.p99,
        });
    }
    runs.sort_by(|a, b| a.name.cmp(&b.name));

    Ok(RunReport {
        schema_version: SCHEMA_VERSION,
        language: Language::Go,
        service: spec.service.clone(),
        r#ref: spec.r#ref.clone(),
        runner: "go test -bench".to_string(),
        started_at,
        finished_at: super::now_iso8601(),
        runs,
        affected_scope: None,
    })
}

/// Lines look like `BenchmarkFoo-8   1000  123.4 ns/op`. We pull the
/// `ns/op` value as a sample. Subsequent reports for the same name are
/// appended.
fn parse_ns_per_op(out: &str) -> Option<f64> {
    if !out.contains("ns/op") {
        return None;
    }
    // Walk tokens; pick the float just before "ns/op".
    let mut prev: Option<&str> = None;
    for tok in out.split_whitespace() {
        if tok == "ns/op" {
            if let Some(s) = prev {
                if let Ok(v) = s.parse::<f64>() {
                    return Some(v);
                }
            }
        }
        prev = Some(tok);
    }
    None
}

fn parse_bench_name(out: &str) -> Option<String> {
    let line = out.trim();
    if !line.starts_with("Benchmark") {
        return None;
    }
    line.split_whitespace().next().map(|s| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ns_per_op_lines() {
        assert_eq!(
            parse_ns_per_op("BenchmarkX-8 1000 123.4 ns/op"),
            Some(123.4)
        );
        assert_eq!(parse_ns_per_op("nothing"), None);
    }

    #[test]
    fn parses_bench_name() {
        assert_eq!(
            parse_bench_name("BenchmarkX-8 1000 123 ns/op"),
            Some("BenchmarkX-8".to_string()),
        );
        assert_eq!(parse_bench_name("=== RUN TestX"), None);
    }
}
