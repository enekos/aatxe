//! Language adapters: shell out to a native bench runner, return a [`RunReport`].
//!
//! Each adapter implements the same contract — given a [`RunSpec`], produce a
//! [`RunReport`] — but the *mechanism* differs:
//!
//! * **TS** invokes `node` with the official `@aatxe/bench` runner; the
//!   runner emits a RunReport JSON which aatxe ingests directly.
//! * **Go** invokes `go test -bench=. -json`; aatxe parses Go's structured
//!   output into `BenchRun` rows.
//! * **Rust** invokes `cargo bench --message-format=json` (criterion) and
//!   parses its `bench` events.
//!
//! All three normalise the output through [`aatxe_core::stats::summarize_samples`]
//! so downstream consumers see identical statistics regardless of language.

use aatxe_core::affected::{resolve_affected, AffectedOptions};
use aatxe_core::types::{AffectedScope as CoreScope, Language, RunReport};
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

pub mod go;
pub mod real_fs;
pub mod rust;
pub mod ts;
pub(crate) mod ts_or_runner;

/// Common inputs handed to every adapter.
pub struct RunSpec {
    pub cwd: PathBuf,
    pub service: String,
    pub r#ref: String,
    /// Regex filter applied client-side (or pushed down to the runner).
    pub filter: Option<String>,
    /// Caller-supplied discovery patterns (empty ⇒ language defaults).
    pub patterns: Vec<String>,
    /// When `Some`, restrict the run to exactly these bench files (no discovery).
    pub bench_files: Option<Vec<PathBuf>>,
    pub verbose: bool,
}

pub fn execute(lang: Language, spec: &RunSpec) -> Result<RunReport> {
    match lang {
        Language::Ts => ts::execute(spec),
        Language::Go => go::execute(spec),
        Language::Rust => rust::execute(spec),
    }
}

/// Resolve the affected bench-file set using the real filesystem + `git`.
pub fn resolve_affected_scope(
    cwd: &Path,
    lang: Language,
    base: &str,
    patterns: &[String],
) -> Result<CoreScope> {
    let fs = real_fs::RealFs;
    let git = real_fs::RealGit;
    let set = resolve_affected(&AffectedOptions {
        cwd: cwd.to_path_buf(),
        base: base.to_string(),
        language: lang,
        patterns: patterns.to_vec(),
        extra_changed_files: vec![],
        git: &git,
        fs: &fs,
    })
    .context("resolving --affected set")?;

    let affected_set: std::collections::HashSet<PathBuf> =
        set.bench_files.iter().cloned().collect();
    let skipped: Vec<PathBuf> = set
        .all_bench_files
        .iter()
        .filter(|p| !affected_set.contains(*p))
        .cloned()
        .collect();

    Ok(CoreScope {
        base: set.base,
        changed_files: set.changed_files,
        bench_files: set.bench_files.iter().map(|p| relpath(cwd, p)).collect(),
        skipped_bench_files: skipped.iter().map(|p| relpath(cwd, p)).collect(),
    })
}

fn relpath(cwd: &Path, p: &Path) -> String {
    p.strip_prefix(cwd)
        .map(|r| r.display().to_string())
        .unwrap_or_else(|_| p.display().to_string())
}

/// Best-effort short SHA of HEAD. Returns `None` if not in a git repo.
pub fn detect_git_ref(cwd: &Path) -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--short=10", "HEAD"])
        .current_dir(cwd)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let s = String::from_utf8(output.stdout).ok()?;
    let t = s.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

pub(crate) fn now_iso8601() -> String {
    let now = time::OffsetDateTime::now_utc();
    now.format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}
