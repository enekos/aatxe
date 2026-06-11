//! `aatxe baseline` — snapshot RunReports locally so `aatxe compare
//! --against-local` works without CI artifacts.
//!
//! This is the local-trial half of the adoption story: a dev benches once,
//! saves the result, edits code, benches again, and compares — all before
//! the repo has any aatxe CI wiring. Baselines are per-machine state, so
//! the default location (`<repo-root>/.aatxe/`) is made self-gitignoring
//! on first save.

use crate::cli::{
    BaselineArgs, BaselineCommand, BaselineListArgs, BaselineRmArgs, BaselineSaveArgs,
    BaselineShowArgs,
};
use crate::commands::Outcome;
use aatxe_core::types::RunReport;
use anyhow::{bail, Context, Result};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

pub fn execute(args: BaselineArgs) -> Result<Outcome> {
    match args.command {
        BaselineCommand::Save(a) => save(a),
        BaselineCommand::Show(a) => show(a),
        BaselineCommand::List(a) => list(a),
        BaselineCommand::Rm(a) => rm(a),
    }
    .map(|_| Outcome::Ok)
}

fn save(args: BaselineSaveArgs) -> Result<()> {
    validate_name(&args.name)?;
    let report = read_report(&args.report)
        .with_context(|| format!("reading RunReport from {}", args.report.display()))?;

    let dir = baselines_dir(args.dir.as_deref())?;
    fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    if args.dir.is_none() {
        ensure_self_gitignore(&dir)?;
    }

    let path = baseline_path(&dir, &args.name);
    fs::write(&path, serde_json::to_string_pretty(&report)?)
        .with_context(|| format!("writing {}", path.display()))?;
    println!(
        "saved baseline '{}' ← {} ({} · ref {} · {} bench{}) → {}",
        args.name,
        args.report.display(),
        report.service,
        report.r#ref,
        report.runs.len(),
        if report.runs.len() == 1 { "" } else { "es" },
        path.display(),
    );
    Ok(())
}

fn show(args: BaselineShowArgs) -> Result<()> {
    validate_name(&args.name)?;
    let dir = baselines_dir(args.dir.as_deref())?;
    let path = baseline_path(&dir, &args.name);
    let report = load_baseline(&path, &args.name)?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    println!(
        "baseline '{}' — {} · ref {} · runner {} · started {}",
        args.name, report.service, report.r#ref, report.runner, report.started_at,
    );
    for run in &report.runs {
        println!("  {:<40} median {:>12.1} ns", run.name, run.median);
    }
    Ok(())
}

fn list(args: BaselineListArgs) -> Result<()> {
    let dir = baselines_dir(args.dir.as_deref())?;
    let mut entries: Vec<(String, PathBuf)> = match fs::read_dir(&dir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                let p = e.path();
                let name = p.file_stem()?.to_str()?.to_string();
                (p.extension()?.to_str()? == "json").then_some((name, p))
            })
            .collect(),
        Err(_) => Vec::new(),
    };
    if entries.is_empty() {
        println!(
            "no baselines in {} — run `aatxe run` then `aatxe baseline save`",
            dir.display()
        );
        return Ok(());
    }
    entries.sort();
    for (name, path) in entries {
        match read_report(&path) {
            Ok(r) => println!(
                "{:<20} {} · ref {} · {} bench{}",
                name,
                r.service,
                r.r#ref,
                r.runs.len(),
                if r.runs.len() == 1 { "" } else { "es" },
            ),
            Err(_) => println!("{:<20} (invalid RunReport JSON)", name),
        }
    }
    Ok(())
}

fn rm(args: BaselineRmArgs) -> Result<()> {
    validate_name(&args.name)?;
    let dir = baselines_dir(args.dir.as_deref())?;
    let path = baseline_path(&dir, &args.name);
    if !path.exists() {
        bail!("no baseline named '{}' at {}", args.name, path.display());
    }
    fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
    println!("removed baseline '{}' ({})", args.name, path.display());
    Ok(())
}

/// Resolve the saved baseline for `aatxe compare --against-local`.
/// Errors with the save-first hint when the named baseline doesn't exist.
pub fn resolve_for_compare(dir_override: Option<&Path>, name: &str) -> Result<PathBuf> {
    validate_name(name)?;
    let dir = baselines_dir(dir_override)?;
    let path = baseline_path(&dir, name);
    if !path.exists() {
        bail!(
            "no local baseline named '{}' at {} — run `aatxe run` then `aatxe baseline save`",
            name,
            path.display()
        );
    }
    Ok(path)
}

fn read_report(p: &Path) -> Result<RunReport> {
    let raw = fs::read_to_string(p)?;
    Ok(serde_json::from_str(&raw)?)
}

fn load_baseline(path: &Path, name: &str) -> Result<RunReport> {
    if !path.exists() {
        bail!(
            "no baseline named '{}' at {} — run `aatxe run` then `aatxe baseline save`",
            name,
            path.display()
        );
    }
    read_report(path).with_context(|| format!("reading baseline from {}", path.display()))
}

/// Baseline names become file names — restrict to a safe charset so a name
/// can never traverse out of the baselines directory.
fn validate_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if !ok {
        bail!(
            "invalid baseline name '{}' — use letters, digits, '-', '_', '.' (must not start with '.')",
            name
        );
    }
    Ok(())
}

fn baseline_path(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("{name}.json"))
}

/// Default directory: `<git toplevel>/.aatxe/baselines`, falling back to
/// `./.aatxe/baselines` outside a git repo. An explicit `--dir` wins.
fn baselines_dir(dir_override: Option<&Path>) -> Result<PathBuf> {
    if let Some(d) = dir_override {
        return Ok(d.to_path_buf());
    }
    let root = repo_root_or_cwd()?;
    Ok(root.join(".aatxe").join("baselines"))
}

fn repo_root_or_cwd() -> Result<PathBuf> {
    let out = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output();
    if let Ok(out) = out {
        if out.status.success() {
            if let Ok(s) = String::from_utf8(out.stdout) {
                return Ok(PathBuf::from(s.trim()));
            }
        }
    }
    std::env::current_dir().context("resolving current directory")
}

/// Make `<root>/.aatxe/` ignore itself so local baselines never land in a
/// commit. Same pattern cargo uses for `target/`. Only written when the
/// directory is at its default location — an explicit `--dir` is the
/// user's responsibility.
fn ensure_self_gitignore(baselines_dir: &Path) -> Result<()> {
    let Some(aatxe_dir) = baselines_dir.parent() else {
        return Ok(());
    };
    let gitignore = aatxe_dir.join(".gitignore");
    if !gitignore.exists() {
        fs::write(&gitignore, "*\n").with_context(|| format!("writing {}", gitignore.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal_report() -> &'static str {
        r#"{
            "schemaVersion": 2,
            "language": "rust",
            "service": "svc",
            "ref": "abcdef0123",
            "runner": "test",
            "startedAt": "2026-06-01T00:00:00Z",
            "finishedAt": "2026-06-01T00:00:01Z",
            "runs": [{
                "name": "a",
                "file": "x.rs",
                "iterations": 3,
                "batchSize": 1,
                "elapsedNs": 0.0,
                "samples": [1.0, 2.0, 3.0],
                "mean": 2.0, "median": 2.0, "trimmedMean": 2.0,
                "stddev": 1.0, "cv": 0.5, "mad": 1.0, "iqr": 1.0,
                "min": 1.0, "max": 3.0, "p50": 2.0, "p95": 3.0, "p99": 3.0
            }]
        }"#
    }

    #[test]
    fn valid_names_pass() {
        for n in ["default", "my-branch", "exp_2", "v0.1.1", "A9"] {
            assert!(validate_name(n).is_ok(), "expected '{n}' to be valid");
        }
    }

    #[test]
    fn invalid_names_rejected() {
        for n in ["", "../escape", "a/b", ".hidden", "sp ace", "semi;colon"] {
            assert!(validate_name(n).is_err(), "expected '{n}' to be rejected");
        }
    }

    #[test]
    fn baseline_path_appends_json() {
        let p = baseline_path(Path::new("/x/y"), "default");
        assert_eq!(p, PathBuf::from("/x/y/default.json"));
    }

    #[test]
    fn explicit_dir_override_wins() {
        let d = baselines_dir(Some(Path::new("/tmp/custom"))).unwrap();
        assert_eq!(d, PathBuf::from("/tmp/custom"));
    }

    #[test]
    fn save_show_rm_round_trip() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().join("baselines");
        let report = tmp.path().join("aatxe.json");
        fs::write(&report, minimal_report()).unwrap();

        save(BaselineSaveArgs {
            report: report.clone(),
            name: "default".into(),
            dir: Some(dir.clone()),
        })
        .unwrap();
        let saved = baseline_path(&dir, "default");
        assert!(saved.exists());

        let resolved = resolve_for_compare(Some(&dir), "default").unwrap();
        assert_eq!(resolved, saved);

        show(BaselineShowArgs {
            name: "default".into(),
            dir: Some(dir.clone()),
            json: false,
        })
        .unwrap();

        rm(BaselineRmArgs {
            name: "default".into(),
            dir: Some(dir.clone()),
        })
        .unwrap();
        assert!(!saved.exists());
    }

    #[test]
    fn save_rejects_invalid_report_json() {
        let tmp = tempfile::TempDir::new().unwrap();
        let report = tmp.path().join("broken.json");
        fs::write(&report, "{not json").unwrap();
        let err = save(BaselineSaveArgs {
            report,
            name: "default".into(),
            dir: Some(tmp.path().join("baselines")),
        })
        .unwrap_err();
        assert!(err.to_string().contains("reading RunReport"));
    }

    #[test]
    fn resolve_for_compare_hints_at_save_when_missing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let err = resolve_for_compare(Some(tmp.path()), "nope").unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("aatxe baseline save"), "got: {msg}");
    }

    #[test]
    fn default_location_gets_self_gitignore() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().join(".aatxe").join("baselines");
        fs::create_dir_all(&dir).unwrap();
        ensure_self_gitignore(&dir).unwrap();
        let gi = tmp.path().join(".aatxe").join(".gitignore");
        assert_eq!(fs::read_to_string(gi).unwrap(), "*\n");
    }
}
