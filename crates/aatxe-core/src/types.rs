//! On-disk and in-memory data model.
//!
//! These types form the public contract between language-specific bench
//! runners and aatxe. A runner — written in TS, Go, or Rust — emits JSON
//! conforming to [`RunReport`]; aatxe reads it and produces a [`CompareReport`]
//! comparing it against a base.
//!
//! The schema is intentionally tolerant of older reports: derived statistics
//! (mean, median, p95, …) are stored *and* recoverable from `samples`, so a
//! consumer can recompute them if it doesn't trust the producer.

use serde::{Deserialize, Serialize};

/// Schema version for the on-disk JSON report. Bumped on incompatible changes.
///
/// `v1` was an earlier legacy shape; aatxe starts at `v2` and adds
/// [`RunReport::affected_scope`] and [`Language`].
pub const SCHEMA_VERSION: u32 = 2;

/// Language that produced a [`RunReport`]. Used by the CLI to pick the right
/// adapter when running, and surfaced in the markdown report so reviewers see
/// at a glance which lane reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    /// TypeScript / JavaScript (`*.bench.ts`, `*.bench.js`).
    Ts,
    /// Go (`*_bench_test.go` paired with `go test -bench`).
    Go,
    /// Rust (`*_bench.rs` or criterion-style benches).
    Rust,
}

impl Language {
    /// File-extension globs aatxe will discover by default for this language.
    pub fn default_globs(self) -> &'static [&'static str] {
        match self {
            Language::Ts => &["**/*.bench.ts", "**/*.bench.js"],
            Language::Go => &["**/*_bench_test.go"],
            Language::Rust => &["**/benches/**/*.rs"],
        }
    }

    /// Source-file extensions to follow when walking the import graph.
    pub fn source_extensions(self) -> &'static [&'static str] {
        match self {
            Language::Ts => &[".ts", ".tsx", ".js", ".cjs", ".mjs"],
            Language::Go => &[".go"],
            Language::Rust => &[".rs"],
        }
    }

    /// Display name used in the markdown report.
    pub fn label(self) -> &'static str {
        match self {
            Language::Ts => "TypeScript",
            Language::Go => "Go",
            Language::Rust => "Rust",
        }
    }
}

/// A non-time metric attached to a [`BenchRun`].
///
/// The base statistics (mean, median, p95, …) describe **time per
/// operation** — the universally-supported axis the comparator gates on
/// today. `Metric` is the optional extension point for everything else a
/// bench might want to report: throughput in bytes-per-second, allocation
/// counts, peak heap, custom domain metrics.
///
/// Adding a metric does **not** bump the schema version. Consumers that
/// don't know about it ignore it; the regression gate continues to operate
/// on the primary time-per-op signal.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Metric {
    /// Human-readable name, e.g. `"throughput"`, `"allocations"`, `"peak_heap"`.
    pub name: String,
    /// Numeric value in the units declared by [`Self::unit`].
    pub value: f64,
    /// SI-style unit string, e.g. `"bytes/s"`, `"allocs/op"`, `"bytes"`.
    pub unit: String,
    /// When `Some(false)` a *higher* value is the improvement direction
    /// (e.g. throughput). When `Some(true)` or `None` the comparator
    /// treats lower as better (matching the time-per-op default).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub lower_is_better: Option<bool>,
}

/// Statistics computed for a single benchmark run. All time values are in
/// nanoseconds.
///
/// `samples` holds the per-iteration durations (post-warmup, post-batching) so
/// the comparator can recompute the derived statistics if needed (e.g. when
/// reading legacy reports).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BenchRun {
    pub name: String,
    pub file: String,
    /// Number of measured samples (timer readings, **not** fn invocations).
    pub iterations: u32,
    /// Number of fn invocations per timer reading. ≥ 1.
    pub batch_size: u32,
    /// Total wall-time spent measuring (warmup excluded), in nanoseconds.
    pub elapsed_ns: f64,
    /// Per-iteration durations, in nanoseconds.
    pub samples: Vec<f64>,
    pub mean: f64,
    /// Median (P50). Outlier-robust point estimate used as the primary effect-size signal.
    pub median: f64,
    /// Mean after dropping the top and bottom 5% of samples.
    pub trimmed_mean: f64,
    pub stddev: f64,
    /// Coefficient of variation: `stddev / mean`. Dimensionless dispersion.
    pub cv: f64,
    /// Median absolute deviation: `median(|x_i - median(x)|)`.
    pub mad: f64,
    /// Interquartile range: P75 - P25.
    pub iqr: f64,
    pub min: f64,
    pub max: f64,
    pub p50: f64,
    pub p95: f64,
    pub p99: f64,
    /// Optional non-time metrics (throughput, allocations, custom). See
    /// [`Metric`]. Omitted from JSON when empty so v0.1 reports stay
    /// byte-identical to the pre-extension shape.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub metrics: Vec<Metric>,
    /// Optional free-form tags for filtering / grouping in the comparator
    /// (e.g. `["core", "hot-path"]`). Omitted when empty.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub tags: Vec<String>,
}

/// Marker present on head reports produced by `aatxe run --affected`.
///
/// The comparator uses this to distinguish "intentionally not run on this PR"
/// from "deleted by the diff" — the former must not gate CI as a regression.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AffectedScope {
    /// Git ref the diff was computed against, e.g. `origin/master`.
    pub base: String,
    /// Repo-relative POSIX paths returned by `git diff --name-only $base...HEAD`.
    pub changed_files: Vec<String>,
    /// Bench file paths that this run executed.
    pub bench_files: Vec<String>,
    /// Bench file paths discovered but skipped as unaffected.
    pub skipped_bench_files: Vec<String>,
}

/// Top-level on-disk report produced by a single bench run.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunReport {
    pub schema_version: u32,
    pub language: Language,
    pub service: String,
    /// Git ref (commit SHA or symbolic ref) that was benched.
    pub r#ref: String,
    /// Free-form description of the runner — `node v22.14.0`, `go1.22.3`, etc.
    pub runner: String,
    pub started_at: String,
    pub finished_at: String,
    pub runs: Vec<BenchRun>,
    /// Present only when the run was scoped by `--affected`.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub affected_scope: Option<AffectedScope>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Verdict {
    Regression,
    Improvement,
    Neutral,
    New,
    Removed,
    OutOfScope,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NeutralReason {
    BelowThreshold,
    NotSignificant,
    TooNoisy,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BenchDiff {
    pub name: String,
    pub base: Option<BenchRun>,
    pub head: Option<BenchRun>,
    /// Relative median delta: `(head.median - base.median) / base.median`.
    pub delta_pct: Option<f64>,
    /// Relative mean delta. Provided for diagnostic display.
    pub mean_delta_pct: Option<f64>,
    /// Two-tailed Mann–Whitney U p-value.
    pub p_value: Option<f64>,
    /// Two-tailed Welch's t-test p-value (diagnostic).
    pub p_value_welch: Option<f64>,
    /// Max coefficient of variation between base/head. Used by the noise gate.
    pub max_cv: Option<f64>,
    pub verdict: Verdict,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub neutral_reason: Option<NeutralReason>,
}

/// Counts per verdict — convenience for renderers and the CI exit-code gate.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompareSummary {
    pub regressions: u32,
    pub improvements: u32,
    pub neutrals: u32,
    pub new: u32,
    pub removed: u32,
    pub out_of_scope: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompareSide {
    pub r#ref: String,
    pub service: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompareReport {
    pub base: CompareSide,
    pub head: CompareSide,
    pub language: Language,
    /// Minimum |median delta| to call a change "meaningful". Fraction in [0,1].
    pub threshold_pct: f64,
    /// Significance level for the Mann–Whitney U test.
    pub alpha: f64,
    /// Above this CV, noise gating kicks in. Fraction in [0,1].
    pub noisy_cv_threshold: f64,
    pub diffs: Vec<BenchDiff>,
    pub summary: CompareSummary,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub affected_scope: Option<AffectedScope>,
}
