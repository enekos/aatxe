//! Bench orchestration: turn a directory + [`BenchSpec`] into a
//! [`RunReport`], async edition of the `perf-vs` flow.
//!
//! Two spec shapes, mirroring the project's "runners at the boundary"
//! rule: aatxe's own cargo bench binaries, or an arbitrary shell command
//! whose stdout ends with a `RunReport` JSON — which is what makes the
//! dashboard usable from any repo with an aatxe SDK, not just this one.

use aatxe_core::types::{RunReport, SCHEMA_VERSION};
use anyhow::{anyhow, bail, Context, Result};
use std::path::Path;
use std::process::Stdio;
use tokio::process::Command;

#[derive(Debug, Clone)]
pub enum BenchSpec {
    /// `cargo build --release --bin <name>` then run it; stdout is a
    /// `RunReport` (possibly after non-JSON status lines). Multiple bins
    /// merge into one report, same as `perf-vs --bench all`.
    CargoBins(Vec<String>),
    /// `sh -c <cmd>` in the target directory; stdout ends with a
    /// `RunReport` JSON.
    Command(String),
}

impl BenchSpec {
    pub fn label(&self) -> String {
        match self {
            BenchSpec::CargoBins(bins) => bins.join("+"),
            BenchSpec::Command(c) => {
                let head: String = c.chars().take(60).collect();
                format!("cmd: {head}")
            }
        }
    }
}

/// Run the bench in `dir`, stamping `ref_label` into the report so the
/// comparator output names the side honestly.
pub async fn run_bench(spec: &BenchSpec, dir: &Path, ref_label: &str) -> Result<RunReport> {
    let mut report = match spec {
        BenchSpec::CargoBins(bins) => {
            let mut merged: Option<RunReport> = None;
            for bin in bins {
                let r = build_and_run_bin(dir, bin)
                    .await
                    .with_context(|| format!("bench bin {bin} in {}", dir.display()))?;
                match merged.as_mut() {
                    None => merged = Some(r),
                    Some(acc) => {
                        acc.runs.extend(r.runs);
                        acc.finished_at = r.finished_at;
                    }
                }
            }
            merged.ok_or_else(|| anyhow!("no bench binaries configured"))?
        }
        BenchSpec::Command(cmd) => {
            let out = Command::new("sh")
                .arg("-c")
                .arg(cmd)
                .current_dir(dir)
                .stdin(Stdio::null())
                .stderr(Stdio::piped())
                .output()
                .await
                .with_context(|| format!("spawning bench command in {}", dir.display()))?;
            if !out.status.success() {
                bail!(
                    "bench command exited {}: {}",
                    out.status,
                    String::from_utf8_lossy(&out.stderr).trim()
                );
            }
            parse_report(&out.stdout).context("parsing bench command stdout")?
        }
    };
    report.r#ref = ref_label.to_string();
    if report.schema_version == 0 {
        report.schema_version = SCHEMA_VERSION;
    }
    if report.runs.is_empty() {
        bail!("bench produced an empty RunReport");
    }
    Ok(report)
}

async fn build_and_run_bin(dir: &Path, bin: &str) -> Result<RunReport> {
    let status = Command::new("cargo")
        .args(["build", "--release", "--bin", bin])
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .status()
        .await
        .context("spawning cargo build")?;
    if !status.success() {
        bail!("cargo build --bin {bin} failed (status {status})");
    }
    let bin_path = dir.join("target/release").join(bin);
    let out = Command::new(&bin_path)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .await
        .with_context(|| format!("spawning {}", bin_path.display()))?;
    if !out.status.success() {
        bail!("{bin} exited with status {}", out.status);
    }
    parse_report(&out.stdout).with_context(|| format!("parsing {bin} stdout"))
}

fn parse_report(stdout: &[u8]) -> Result<RunReport> {
    let text = String::from_utf8_lossy(stdout);
    let json = strip_non_json_prefix(&text);
    serde_json::from_str(json).with_context(|| {
        let head: String = text.chars().take(200).collect();
        format!("RunReport JSON expected, got: {head:?}")
    })
}

/// Bench binaries may print status lines (e.g. `METRIC k=v`) before the
/// trailing JSON object. Same rule as `perf-vs`: accept a `{` only at the
/// start of the input or right after a newline.
fn strip_non_json_prefix(s: &str) -> &str {
    let bytes = s.as_bytes();
    if let Some(i) = bytes.iter().position(|&b| b == b'{') {
        if i == 0 || bytes[i - 1] == b'\n' {
            return &s[i..];
        }
        for (j, &b) in bytes.iter().enumerate().skip(i) {
            if b == b'{' && bytes[j - 1] == b'\n' {
                return &s[j..];
            }
        }
    }
    s
}

#[cfg(test)]
pub(crate) mod test_support {
    use aatxe_core::types::{BenchRun, Language, RunReport, SCHEMA_VERSION};

    /// A minimal-but-valid RunReport with one bench run, for tests.
    pub fn sample_report(bench_name: &str, median: f64) -> RunReport {
        RunReport {
            schema_version: SCHEMA_VERSION,
            language: Language::Rust,
            service: "test".into(),
            r#ref: "deadbeef".into(),
            runner: "test-runner".into(),
            started_at: "2026-06-11T00:00:00Z".into(),
            finished_at: "2026-06-11T00:00:01Z".into(),
            runs: vec![BenchRun {
                name: bench_name.into(),
                file: "bench.rs".into(),
                iterations: 30,
                batch_size: 1,
                elapsed_ns: median * 30.0,
                samples: vec![median; 30],
                mean: median,
                median,
                trimmed_mean: median,
                stddev: 0.0,
                cv: 0.0,
                mad: 0.0,
                iqr: 0.0,
                min: median,
                max: median,
                p50: median,
                p95: median,
                p99: median,
                metrics: vec![],
                tags: vec![],
            }],
            affected_scope: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn strip_prefix_drops_metric_lines() {
        let raw = "METRIC a=1\nMETRIC b=2\n{\"schemaVersion\":2}\n";
        assert_eq!(strip_non_json_prefix(raw), "{\"schemaVersion\":2}\n");
    }

    #[test]
    fn strip_prefix_passes_clean_json() {
        let raw = "{\"x\":1}";
        assert_eq!(strip_non_json_prefix(raw), raw);
    }

    #[test]
    fn strip_prefix_ignores_brace_mid_line() {
        let raw = "warn{ish}\n{\"ok\":true}\n";
        assert_eq!(strip_non_json_prefix(raw), "{\"ok\":true}\n");
    }

    #[test]
    fn label_truncates_long_commands() {
        let spec = BenchSpec::Command("x".repeat(200));
        assert!(spec.label().len() < 80);
    }

    #[test]
    fn parse_report_round_trips_sample() {
        let report = test_support::sample_report("parse", 120.0);
        let json = serde_json::to_string(&report).unwrap();
        let parsed = parse_report(json.as_bytes()).unwrap();
        assert_eq!(parsed.runs.len(), 1);
        assert_eq!(parsed.runs[0].name, "parse");
    }

    #[tokio::test]
    async fn command_spec_runs_and_stamps_ref() {
        let dir = tempfile::tempdir().unwrap();
        let fixture = dir.path().join("report.json");
        let report = test_support::sample_report("micro", 50.0);
        let mut f = std::fs::File::create(&fixture).unwrap();
        write!(f, "{}", serde_json::to_string(&report).unwrap()).unwrap();
        let spec = BenchSpec::Command(format!("cat {}", fixture.display()));
        let got = run_bench(&spec, dir.path(), "a1#3").await.unwrap();
        assert_eq!(got.r#ref, "a1#3");
        assert_eq!(got.runs[0].name, "micro");
    }

    #[tokio::test]
    async fn command_spec_failure_surfaces_stderr() {
        let dir = tempfile::tempdir().unwrap();
        let spec = BenchSpec::Command("echo broken >&2; exit 3".into());
        let err = run_bench(&spec, dir.path(), "x").await.unwrap_err();
        assert!(format!("{err:#}").contains("broken"), "{err:#}");
    }

    #[tokio::test]
    async fn empty_runs_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let mut report = test_support::sample_report("micro", 50.0);
        report.runs.clear();
        let fixture = dir.path().join("empty.json");
        std::fs::write(&fixture, serde_json::to_string(&report).unwrap()).unwrap();
        let spec = BenchSpec::Command(format!("cat {}", fixture.display()));
        let err = run_bench(&spec, dir.path(), "x").await.unwrap_err();
        assert!(format!("{err:#}").contains("empty"), "{err:#}");
    }
}
