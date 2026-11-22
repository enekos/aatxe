//! `aatxe evals` — runs the stats eval harness end to end.
//!
//! 1. Run the stats eval (deterministic synthetic A/B pairs).
//! 2. Serialise the result to JSON.
//! 3. Optionally render a markdown summary.
//! 4. Optionally diff against a baseline; exit 2 on regression past
//!    tolerance.

use crate::cli::EvalsArgs;
use crate::commands::Outcome;
use aatxe_evals::report::{
    regressions_against_baseline, EvalReport, EvalTolerances, EVAL_SCHEMA_VERSION,
};
use aatxe_evals::stats::{default_scenarios, run_stats_evals};
use anyhow::{Context, Result};
use std::fs;
use std::path::PathBuf;

pub fn execute(args: EvalsArgs) -> Result<Outcome> {
    let started_at = current_iso8601();

    eprintln!(
        "aatxe evals: running stats scenarios ({} default cases)…",
        default_scenarios().len()
    );
    let report = EvalReport {
        schema_version: EVAL_SCHEMA_VERSION,
        started_at,
        aatxe_version: env!("CARGO_PKG_VERSION").to_string(),
        stats: Some(run_stats_evals(&default_scenarios())),
    };

    let out = args
        .out
        .clone()
        .unwrap_or_else(|| PathBuf::from("./aatxe-evals.json"));
    let json = serde_json::to_string_pretty(&report).context("serialising EvalReport")?;
    fs::write(&out, &json).with_context(|| format!("writing {}", out.display()))?;
    eprintln!("aatxe evals: wrote {}", out.display());

    if let Some(md_out) = args.markdown.as_ref() {
        let body = render_markdown(&report);
        fs::write(md_out, body)
            .with_context(|| format!("writing markdown to {}", md_out.display()))?;
        eprintln!("aatxe evals: wrote {}", md_out.display());
    }

    print_summary_to_stderr(&report);

    if let Some(baseline_path) = args.baseline.as_ref() {
        let baseline_raw = fs::read_to_string(baseline_path)
            .with_context(|| format!("reading baseline from {}", baseline_path.display()))?;
        let baseline: EvalReport = serde_json::from_str(&baseline_raw)
            .with_context(|| format!("parsing baseline JSON from {}", baseline_path.display()))?;
        let tolerances = EvalTolerances::default();
        let regs = regressions_against_baseline(&baseline, &report, tolerances);
        if !regs.is_empty() {
            eprintln!("aatxe evals: {} regression(s) vs baseline:", regs.len());
            for r in &regs {
                eprintln!(
                    "  ✗ {} — baseline {:.4} → current {:.4} (worse by {:.4}, tolerance {:.4})",
                    r.metric, r.baseline, r.current, r.delta_worse, r.tolerance
                );
                eprintln!("      {}", r.note);
            }
            if !args.no_fail {
                return Ok(Outcome::Regressions);
            }
        } else {
            eprintln!("aatxe evals: ✓ no regressions vs baseline");
        }
    }
    Ok(Outcome::Ok)
}

fn print_summary_to_stderr(r: &EvalReport) {
    eprintln!("aatxe evals summary:");
    if let Some(s) = &r.stats {
        eprintln!(
            "  stats    · scenarios={} passed={} pass_rate={:.3} null_FPR={:.3} borderline_TPR={:.3}",
            s.scenarios_total,
            s.scenarios_passed,
            s.pass_rate,
            s.observed_null_fpr,
            s.observed_borderline_tpr,
        );
    }
}

fn render_markdown(r: &EvalReport) -> String {
    use std::fmt::Write;
    let mut s = String::new();
    let _ = writeln!(s, "<!-- aatxe:evals -->");
    let _ = writeln!(s, "## aatxe evals — {} ({})", r.started_at, r.aatxe_version);
    let _ = writeln!(s);
    if let Some(stats) = &r.stats {
        let _ = writeln!(
            s,
            "### Stats engine ({} scenarios, {} passed)",
            stats.scenarios_total, stats.scenarios_passed
        );
        let _ = writeln!(s);
        let _ = writeln!(
            s,
            "Observed null FPR: **{:.3}** (target ≤ 0.05). Borderline 6% regression TPR: **{:.3}**.",
            stats.observed_null_fpr, stats.observed_borderline_tpr
        );
        let _ = writeln!(s);
        let _ = writeln!(
            s,
            "| scenario | regression | improvement | neutral (noisy) | mean p | pass |"
        );
        let _ = writeln!(s, "|---|---|---|---|---|---|");
        for p in &stats.per_scenario {
            let _ = writeln!(
                s,
                "| `{}` | {:.3} | {:.3} | {:.3} ({:.3}) | {:.3} | {} |",
                p.name,
                p.regression_rate,
                p.improvement_rate,
                p.neutral_rate,
                p.too_noisy_rate,
                p.mean_p_value,
                if p.passed { "✓" } else { "✗" }
            );
        }
        let _ = writeln!(s);
        let any_failures: Vec<&aatxe_evals::stats::StatsScenarioResult> =
            stats.per_scenario.iter().filter(|p| !p.passed).collect();
        if !any_failures.is_empty() {
            let _ = writeln!(s, "<details><summary>Scenario failures</summary>");
            let _ = writeln!(s);
            for p in any_failures {
                let _ = writeln!(s, "- `{}`:", p.name);
                for f in &p.failures {
                    let _ = writeln!(s, "  - {}", f);
                }
            }
            let _ = writeln!(s, "</details>");
        }
    }
    s
}

fn current_iso8601() -> String {
    // Match the format aatxe core uses elsewhere — strict UTC ISO8601.
    use time::format_description::well_known::Iso8601;
    use time::OffsetDateTime;
    let now = OffsetDateTime::now_utc();
    now.format(&Iso8601::DEFAULT)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_markdown_includes_sticky_marker_and_stats_section() {
        let report = EvalReport {
            schema_version: EVAL_SCHEMA_VERSION,
            started_at: "1970-01-01T00:00:00Z".into(),
            aatxe_version: "test".into(),
            stats: Some(run_stats_evals(&default_scenarios())),
        };
        let md = render_markdown(&report);
        assert!(md.contains("<!-- aatxe:evals -->"));
        assert!(md.contains("Stats engine"));
        assert!(md.contains("null FPR"));
    }
}
