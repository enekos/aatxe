//! `aatxe perf-vs` — local A/B perf comparison across two worktrees.
//!
//! The CI gate's contract is "compare two `RunReport`s and exit non-zero
//! on regression." This command lifts that contract out of GitHub Actions
//! so the iteration loop is `edit → perf-vs → edit` (~30 s) instead of
//! `edit → commit → push → wait` (5+ min).
//!
//! Flow:
//!   1. Resolve `--against` to a SHA, refuse if it equals HEAD.
//!   2. `git worktree add --detach <wt>/<sha-short> <sha>` (reuse if it
//!      already exists at the same SHA — cheap rebuild path).
//!   3. Build the bench binary in HEAD and the worktree (parallel).
//!   4. Run the bench in each, producing two `RunReport` JSONs.
//!   5. `compare_reports` + render markdown; print summary to stdout.

use crate::cli::{PerfBenchArg, PerfVsArgs};
use crate::commands::Outcome;
use aatxe_core::types::{BenchRun, Language, RunReport, SCHEMA_VERSION};
use aatxe_core::{compare_reports, has_regressions, render_markdown, CompareOptions};
use anyhow::{anyhow, bail, Context, Result};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub fn execute(args: PerfVsArgs) -> Result<Outcome> {
    let repo_root = repo_root()?;
    let head_sha = resolve_ref(&repo_root, "HEAD")?;
    let base_sha = resolve_ref(&repo_root, &args.against)
        .with_context(|| format!("resolving --against ref '{}'", args.against))?;
    if base_sha == head_sha {
        bail!(
            "--against '{}' resolves to the same commit as HEAD ({}); nothing to compare",
            args.against,
            short(&head_sha)
        );
    }

    let wt_parent = args
        .worktree_dir
        .clone()
        .unwrap_or_else(|| default_worktree_parent(&repo_root));
    let wt_path = wt_parent.join(short(&base_sha));
    let out_dir = args.out_dir.clone().unwrap_or_else(|| {
        repo_root.join("tmp/perf-vs").join(format!(
            "{}-{}",
            short(&base_sha),
            bench_slug(args.bench)
        ))
    });
    fs::create_dir_all(&out_dir)
        .with_context(|| format!("creating out-dir {}", out_dir.display()))?;
    fs::create_dir_all(&wt_parent)
        .with_context(|| format!("creating worktree parent {}", wt_parent.display()))?;

    ensure_worktree(&repo_root, &wt_path, &base_sha, args.verbose)?;

    let benches = expand_bench(args.bench);
    eprintln!(
        "perf-vs: HEAD {} vs {} ({}) · bench: {} · worktree: {}",
        short(&head_sha),
        args.against,
        short(&base_sha),
        benches
            .iter()
            .map(|b| bench_slug(*b))
            .collect::<Vec<_>>()
            .join("+"),
        wt_path.display()
    );

    let head_report = build_and_run_all(&repo_root, &benches, "head", &head_sha, args.verbose)?;
    let base_report = build_and_run_all(&wt_path, &benches, "base", &base_sha, args.verbose)?;

    let head_json = out_dir.join("head.json");
    let base_json = out_dir.join("base.json");
    fs::write(&head_json, serde_json::to_string_pretty(&head_report)?)?;
    fs::write(&base_json, serde_json::to_string_pretty(&base_report)?)?;

    let opts = CompareOptions {
        threshold_pct: args.threshold,
        alpha: args.alpha,
        noisy_cv_threshold: args.noisy_cv,
    };
    let cmp = compare_reports(&base_report, &head_report, opts);

    let cmp_json = out_dir.join("cmp.json");
    let cmp_md = out_dir.join("cmp.md");
    fs::write(&cmp_json, serde_json::to_string_pretty(&cmp)?)?;
    let md = render_markdown(&cmp);
    fs::write(&cmp_md, &md)?;

    println!("{}", md);
    eprintln!(
        "perf-vs: wrote {} {} {} {}",
        head_json.display(),
        base_json.display(),
        cmp_json.display(),
        cmp_md.display()
    );
    eprintln!(
        "perf-vs summary: {} regression(s) · {} improvement(s) · {} neutral · {} new · {} removed",
        cmp.summary.regressions,
        cmp.summary.improvements,
        cmp.summary.neutrals,
        cmp.summary.new,
        cmp.summary.removed,
    );

    if args.rm_worktree {
        let _ = Command::new("git")
            .args(["worktree", "remove", "--force"])
            .arg(&wt_path)
            .current_dir(&repo_root)
            .status();
    }

    if args.fail_on_regression && has_regressions(&cmp) {
        return Ok(Outcome::Regressions);
    }
    Ok(Outcome::Ok)
}

fn repo_root() -> Result<PathBuf> {
    let out = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .context("running `git rev-parse --show-toplevel`")?;
    if !out.status.success() {
        bail!(
            "not inside a git repo: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(PathBuf::from(
        String::from_utf8(out.stdout)?.trim().to_string(),
    ))
}

fn resolve_ref(repo: &Path, r#ref: &str) -> Result<String> {
    let out = Command::new("git")
        .args(["rev-parse", "--verify"])
        .arg(format!("{}^{{commit}}", r#ref))
        .current_dir(repo)
        .output()
        .context("running `git rev-parse`")?;
    if !out.status.success() {
        bail!(
            "git rev-parse failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8(out.stdout)?.trim().to_string())
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(8)]
}

fn default_worktree_parent(repo: &Path) -> PathBuf {
    repo.parent()
        .map(|p| p.join("aatxe-worktrees"))
        .unwrap_or_else(|| repo.join(".aatxe-worktrees"))
}

fn ensure_worktree(repo: &Path, wt_path: &Path, sha: &str, verbose: bool) -> Result<()> {
    if wt_path.exists() {
        // If a worktree already lives here, sanity-check it points at the
        // expected SHA. If not, refuse — don't silently bench the wrong
        // code.
        let head = resolve_ref(wt_path, "HEAD")
            .with_context(|| format!("inspecting existing worktree at {}", wt_path.display()))?;
        if head != sha {
            bail!(
                "worktree at {} points at {} but we need {}; remove it (or pass --worktree-dir) and retry",
                wt_path.display(),
                short(&head),
                short(sha)
            );
        }
        if verbose {
            eprintln!("perf-vs: reusing worktree at {}", wt_path.display());
        }
        return Ok(());
    }

    let status = Command::new("git")
        .args(["worktree", "add", "--detach"])
        .arg(wt_path)
        .arg(sha)
        .current_dir(repo)
        .stdout(if verbose {
            Stdio::inherit()
        } else {
            Stdio::null()
        })
        .stderr(Stdio::inherit())
        .status()
        .context("running `git worktree add`")?;
    if !status.success() {
        bail!("git worktree add failed (status {})", status);
    }
    Ok(())
}

fn expand_bench(b: PerfBenchArg) -> Vec<PerfBenchArg> {
    match b {
        PerfBenchArg::All => vec![PerfBenchArg::Council, PerfBenchArg::BigDiff],
        single => vec![single],
    }
}

fn bench_slug(b: PerfBenchArg) -> &'static str {
    match b {
        PerfBenchArg::Council => "council",
        PerfBenchArg::BigDiff => "big-diff",
        PerfBenchArg::All => "all",
    }
}

fn bench_binary(b: PerfBenchArg) -> &'static str {
    match b {
        PerfBenchArg::Council => "aatxe-council-bench",
        PerfBenchArg::BigDiff => "aatxe-big-diff-bench",
        PerfBenchArg::All => unreachable!("expand_bench unrolls All before reaching here"),
    }
}

fn build_and_run_all(
    cwd: &Path,
    benches: &[PerfBenchArg],
    side: &str,
    sha: &str,
    verbose: bool,
) -> Result<RunReport> {
    let mut merged: Option<RunReport> = None;
    for b in benches {
        let report = build_and_run(cwd, *b, side, sha, verbose)
            .with_context(|| format!("running {} on {}", bench_slug(*b), side))?;
        match merged.as_mut() {
            None => merged = Some(report),
            Some(acc) => {
                acc.runs.extend(report.runs);
                acc.finished_at = report.finished_at;
            }
        }
    }
    merged.ok_or_else(|| anyhow!("no benches selected"))
}

fn build_and_run(
    cwd: &Path,
    bench: PerfBenchArg,
    side: &str,
    sha: &str,
    verbose: bool,
) -> Result<RunReport> {
    let bin = bench_binary(bench);
    eprintln!("perf-vs: building {} ({})", bin, side);
    let build_status = Command::new("cargo")
        .args(["build", "--release", "--bin", bin])
        .current_dir(cwd)
        .stdout(if verbose {
            Stdio::inherit()
        } else {
            Stdio::null()
        })
        .stderr(Stdio::inherit())
        .status()
        .with_context(|| format!("spawning cargo build for {}", bin))?;
    if !build_status.success() {
        bail!("cargo build {} failed (status {})", bin, build_status);
    }

    let bin_path = cwd.join("target/release").join(bin);
    if !bin_path.exists() {
        bail!(
            "expected bench binary at {} but it doesn't exist",
            bin_path.display()
        );
    }
    eprintln!("perf-vs: running  {} ({})", bin, side);
    let output = Command::new(&bin_path)
        .env("AATXE_SERVICE", service_for(bench))
        .current_dir(cwd)
        .stderr(if verbose {
            Stdio::inherit()
        } else {
            Stdio::null()
        })
        .output()
        .with_context(|| format!("spawning {}", bin_path.display()))?;
    if !output.status.success() {
        bail!(
            "{} exited with status {}: {}",
            bin,
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    // Big-diff-bench interleaves `METRIC k=v` lines (for the autoresearch
    // loop) with the trailing RunReport JSON; council-bench emits the JSON
    // directly. Strip any leading non-JSON lines before parsing so both
    // protocols round-trip through the same code path.
    let stdout_str = String::from_utf8_lossy(&output.stdout);
    let json_slice = strip_non_json_prefix(&stdout_str);
    let mut report: RunReport = serde_json::from_str(json_slice).with_context(|| {
        format!(
            "parsing RunReport from {} stdout (first 200 bytes: {:?})",
            bin,
            &stdout_str[..stdout_str.len().min(200)]
        )
    })?;
    // Stamp the ref we're actually benching. The bench binary uses
    // env-driven defaults; overriding here keeps the report honest if
    // someone runs it outside this command.
    report.r#ref = short(sha).to_string();
    if report.schema_version == 0 {
        report.schema_version = SCHEMA_VERSION;
    }
    sanity_check_report(&report, bench)?;
    Ok(report)
}

fn service_for(bench: PerfBenchArg) -> &'static str {
    match bench {
        PerfBenchArg::Council => "aatxe-council",
        PerfBenchArg::BigDiff => "aatxe-big-diff",
        PerfBenchArg::All => unreachable!(),
    }
}

/// Bench binaries may print arbitrary status lines on stdout before the
/// trailing `RunReport` JSON object. Find the first `{` after a newline
/// (or at the very start) and return the slice from there.
fn strip_non_json_prefix(s: &str) -> &str {
    let bytes = s.as_bytes();
    if let Some(i) = bytes.iter().position(|&b| b == b'{') {
        // Guard against `{` inside a stray prefix line — only accept it
        // when it's at the start of the input or right after a newline.
        if i == 0 || bytes[i - 1] == b'\n' {
            return &s[i..];
        }
        // Walk forward looking for a newline followed by `{`.
        for (j, &b) in bytes.iter().enumerate().skip(i) {
            if b == b'{' && j > 0 && bytes[j - 1] == b'\n' {
                return &s[j..];
            }
        }
    }
    s
}

fn sanity_check_report(report: &RunReport, bench: PerfBenchArg) -> Result<()> {
    if report.runs.is_empty() {
        bail!("{} produced an empty RunReport", bench_slug(bench));
    }
    if report.language != Language::Rust {
        bail!(
            "expected language=rust in {} RunReport, got {:?}",
            bench_slug(bench),
            report.language
        );
    }
    // Touch BenchRun to make sure the import is load-bearing (avoids a
    // clippy warning for an unused import on the type re-export above).
    let _: Option<&BenchRun> = report.runs.first();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::PerfBenchArg;

    #[test]
    fn bench_slug_round_trip() {
        assert_eq!(bench_slug(PerfBenchArg::Council), "council");
        assert_eq!(bench_slug(PerfBenchArg::BigDiff), "big-diff");
        assert_eq!(bench_slug(PerfBenchArg::All), "all");
    }

    #[test]
    fn expand_all_into_concrete_benches() {
        let v = expand_bench(PerfBenchArg::All);
        assert_eq!(v.len(), 2);
        assert!(v.contains(&PerfBenchArg::Council));
        assert!(v.contains(&PerfBenchArg::BigDiff));
    }

    #[test]
    fn expand_single_passes_through() {
        assert_eq!(
            expand_bench(PerfBenchArg::Council),
            vec![PerfBenchArg::Council]
        );
    }

    #[test]
    fn short_truncates_long_sha_to_eight() {
        assert_eq!(short("abcdef1234567890"), "abcdef12");
    }

    #[test]
    fn short_keeps_short_sha_intact() {
        assert_eq!(short("abc"), "abc");
    }

    #[test]
    fn strip_non_json_prefix_drops_metric_lines() {
        let raw =
            "METRIC diff_mb_small=0.234\nMETRIC files_parsed_small=45\n{\"schemaVersion\":2}\n";
        assert_eq!(strip_non_json_prefix(raw), "{\"schemaVersion\":2}\n");
    }

    #[test]
    fn strip_non_json_prefix_passes_through_clean_json() {
        let raw = "{\"schemaVersion\":2,\"runs\":[]}";
        assert_eq!(strip_non_json_prefix(raw), raw);
    }

    #[test]
    fn strip_non_json_prefix_ignores_brace_inside_prefix_line() {
        let raw = "DEBUG something{weird}\n{\"ok\":true}\n";
        assert_eq!(strip_non_json_prefix(raw), "{\"ok\":true}\n");
    }

    #[test]
    fn default_worktree_parent_uses_repo_parent() {
        let repo = PathBuf::from("/Users/eneko/eneko_projects/aatxe");
        let parent = default_worktree_parent(&repo);
        assert_eq!(
            parent,
            PathBuf::from("/Users/eneko/eneko_projects/aatxe-worktrees")
        );
    }
}
