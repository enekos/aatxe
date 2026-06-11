//! Council lane: when an agent finishes, run `aatxe council` over its
//! branch diff and surface the verdict next to the perf trajectory.
//!
//! The council is invoked as a subprocess of the configured `aatxe`
//! binary (default: the very binary serving the UI) rather than linked in
//! — keeps `aatxe-ui` decoupled from `aatxe-council` and reuses the CLI's
//! backend/stub/learning-corpus plumbing wholesale.

use crate::state::CouncilMode;
use anyhow::{bail, Context, Result};
use std::path::Path;
use std::process::Stdio;
use tokio::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CouncilCounts {
    pub critical: u32,
    pub major: u32,
    pub shippable: u32,
}

/// Run the council on a unified diff already written to `diff_path`.
/// Returns the counts plus the rendered sticky markdown.
pub async fn run_council(
    aatxe_bin: &Path,
    mode: CouncilMode,
    diff_path: &Path,
    out_dir: &Path,
    cwd: &Path,
    confidence_floor: f64,
) -> Result<(CouncilCounts, String)> {
    let json_path = out_dir.join("council.json");
    let md_path = out_dir.join("council.md");
    let mut cmd = Command::new(aatxe_bin);
    cmd.arg("council")
        .arg("--diff-file")
        .arg(diff_path)
        .arg("--out")
        .arg(&json_path)
        .arg("--markdown")
        .arg(&md_path)
        .arg("--interactive=false")
        .arg("--confidence-floor")
        .arg(confidence_floor.to_string())
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    match mode {
        CouncilMode::Off => bail!("council mode is off"),
        CouncilMode::Stub => {
            cmd.env("AATXE_COUNCIL_STUB", "1");
        }
        CouncilMode::Real => {
            cmd.arg("--backend").arg("claude-code");
        }
    }
    let out = cmd.output().await.context("spawning aatxe council")?;
    if !out.status.success() {
        bail!(
            "aatxe council exited {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let json = std::fs::read_to_string(&json_path)
        .with_context(|| format!("reading {}", json_path.display()))?;
    let report: serde_json::Value =
        serde_json::from_str(&json).context("parsing council report JSON")?;
    let counts = count_findings(&report, confidence_floor);
    let markdown = std::fs::read_to_string(&md_path).unwrap_or_default();
    Ok((counts, markdown))
}

/// Count shippable findings from a `CouncilReport` JSON. Parsed
/// defensively via `Value` so a field rename in `aatxe-council` degrades
/// to zero counts instead of a UI crash; mirrors
/// `JudgedFinding::survives`: dropped verdicts and below-floor
/// confidences never ship.
pub fn count_findings(report: &serde_json::Value, confidence_floor: f64) -> CouncilCounts {
    let mut counts = CouncilCounts {
        critical: 0,
        major: 0,
        shippable: 0,
    };
    let Some(judged) = report.get("judged").and_then(|j| j.as_array()) else {
        return counts;
    };
    for j in judged {
        let verdict = j.get("verdict").and_then(|v| v.as_str()).unwrap_or("");
        if verdict.eq_ignore_ascii_case("drop") {
            continue;
        }
        let confidence = j.get("confidence").and_then(|c| c.as_f64()).unwrap_or(0.0);
        if confidence < confidence_floor {
            continue;
        }
        counts.shippable += 1;
        match j
            .pointer("/finding/severity")
            .and_then(|s| s.as_str())
            .unwrap_or("")
        {
            s if s.eq_ignore_ascii_case("critical") => counts.critical += 1,
            s if s.eq_ignore_ascii_case("major") => counts.major += 1,
            _ => {}
        }
    }
    counts
}

#[cfg(test)]
mod tests {
    use super::*;

    fn judged(verdict: &str, confidence: f64, severity: &str) -> serde_json::Value {
        serde_json::json!({
            "verdict": verdict,
            "confidence": confidence,
            "finding": { "severity": severity, "title": "t" }
        })
    }

    #[test]
    fn counts_respect_floor_and_drop_verdicts() {
        let report = serde_json::json!({
            "judged": [
                judged("keep", 0.9, "critical"),
                judged("keep", 0.9, "major"),
                judged("downgrade", 0.7, "minor"),
                judged("keep", 0.3, "critical"),   // below floor
                judged("drop", 0.99, "critical"),  // dropped
            ]
        });
        let c = count_findings(&report, 0.55);
        assert_eq!(c.critical, 1);
        assert_eq!(c.major, 1);
        assert_eq!(c.shippable, 3);
    }

    #[test]
    fn malformed_report_degrades_to_zero() {
        let c = count_findings(&serde_json::json!({"unexpected": true}), 0.55);
        assert_eq!(
            c,
            CouncilCounts {
                critical: 0,
                major: 0,
                shippable: 0
            }
        );
    }
}
