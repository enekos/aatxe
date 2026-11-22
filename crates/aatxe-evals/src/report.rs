//! Reportable shape — what `aatxe evals` writes to disk and what CI
//! diffs against the baseline.

use crate::stats::StatsEvalSummary;
use serde::{Deserialize, Serialize};

/// Top-level eval report. Schema-versioned so we can evolve fields without
/// invalidating older baseline JSONs.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvalReport {
    pub schema_version: u32,
    /// Wall-clock ISO8601 of when the eval started. Set by the CLI.
    pub started_at: String,
    /// Aatxe version string (cargo pkg version).
    pub aatxe_version: String,
    /// Set when the stats eval ran.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stats: Option<StatsEvalSummary>,
}

pub const EVAL_SCHEMA_VERSION: u32 = 1;

/// Maximum allowed regression in each headline metric before
/// [`regressions_against_baseline`] flags it. Numbers are absolute deltas:
/// a `stats_null_fpr_rise` of `0.05` means the new run may fire on the null
/// up to 5 percentage points more often than the baseline before the gate
/// fires.
#[derive(Debug, Clone, Copy)]
pub struct EvalTolerances {
    pub stats_pass_rate_drop: f64,
    pub stats_null_fpr_rise: f64,
    pub stats_borderline_tpr_drop: f64,
}

impl Default for EvalTolerances {
    fn default() -> Self {
        Self {
            stats_pass_rate_drop: 0.0,
            stats_null_fpr_rise: 0.05,
            stats_borderline_tpr_drop: 0.10,
        }
    }
}

/// One regression — a metric that moved in the bad direction by more than
/// the tolerance allows. The `direction` is always "worse" — fields that
/// improve aren't flagged.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvalRegression {
    pub metric: String,
    pub baseline: f64,
    pub current: f64,
    /// Positive = worsened by this magnitude vs baseline. E.g. recall
    /// dropping by 0.07 reports `delta = 0.07`.
    pub delta_worse: f64,
    pub tolerance: f64,
    pub note: String,
}

/// Diff a current report against a baseline and return the metrics that
/// regressed past tolerance. Empty result = clean run.
pub fn regressions_against_baseline(
    baseline: &EvalReport,
    current: &EvalReport,
    tolerances: EvalTolerances,
) -> Vec<EvalRegression> {
    let mut out: Vec<EvalRegression> = Vec::new();
    if let (Some(b), Some(c)) = (baseline.stats.as_ref(), current.stats.as_ref()) {
        check_drop(
            &mut out,
            "stats.pass_rate",
            b.pass_rate,
            c.pass_rate,
            tolerances.stats_pass_rate_drop,
            "fewer scenarios passed their expectations",
        );
        check_rise(
            &mut out,
            "stats.observed_null_fpr",
            b.observed_null_fpr,
            c.observed_null_fpr,
            tolerances.stats_null_fpr_rise,
            "comparator firing as regression under the null more often",
        );
        check_drop(
            &mut out,
            "stats.observed_borderline_tpr",
            b.observed_borderline_tpr,
            c.observed_borderline_tpr,
            tolerances.stats_borderline_tpr_drop,
            "comparator's detection power on borderline regressions dropped",
        );
    }

    out
}

fn check_drop(
    out: &mut Vec<EvalRegression>,
    metric: &str,
    baseline: f64,
    current: f64,
    tolerance: f64,
    note: &str,
) {
    let delta = baseline - current;
    if delta > tolerance {
        out.push(EvalRegression {
            metric: metric.into(),
            baseline,
            current,
            delta_worse: delta,
            tolerance,
            note: note.into(),
        });
    }
}

fn check_rise(
    out: &mut Vec<EvalRegression>,
    metric: &str,
    baseline: f64,
    current: f64,
    tolerance: f64,
    note: &str,
) {
    let delta = current - baseline;
    if delta > tolerance {
        out.push(EvalRegression {
            metric: metric.into(),
            baseline,
            current,
            delta_worse: delta,
            tolerance,
            note: note.into(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_stats(pass_rate: f64, null_fpr: f64, borderline_tpr: f64) -> StatsEvalSummary {
        StatsEvalSummary {
            scenarios_total: 6,
            scenarios_passed: 6,
            pass_rate,
            observed_null_fpr: null_fpr,
            observed_borderline_tpr: borderline_tpr,
            per_scenario: vec![],
        }
    }

    fn mk_report(stats: StatsEvalSummary) -> EvalReport {
        EvalReport {
            schema_version: EVAL_SCHEMA_VERSION,
            started_at: "2026-01-01T00:00:00Z".into(),
            aatxe_version: "0.0.0".into(),
            stats: Some(stats),
        }
    }

    #[test]
    fn flat_run_against_itself_has_no_regressions() {
        let r = mk_report(mk_stats(1.0, 0.04, 0.85));
        assert!(regressions_against_baseline(&r, &r, EvalTolerances::default()).is_empty());
    }

    #[test]
    fn null_fpr_rise_is_flagged() {
        let base = mk_report(mk_stats(1.0, 0.02, 0.85));
        let cur = mk_report(mk_stats(1.0, 0.12, 0.85));
        let regs = regressions_against_baseline(&base, &cur, EvalTolerances::default());
        assert_eq!(regs.len(), 1);
        assert_eq!(regs[0].metric, "stats.observed_null_fpr");
    }

    #[test]
    fn any_scenario_failure_is_flagged() {
        let base = mk_report(mk_stats(1.0, 0.02, 0.85));
        let cur = mk_report(mk_stats(0.9, 0.02, 0.85));
        let regs = regressions_against_baseline(&base, &cur, EvalTolerances::default());
        assert!(regs.iter().any(|r| r.metric == "stats.pass_rate"));
    }

    #[test]
    fn improvements_are_not_flagged() {
        let base = mk_report(mk_stats(0.9, 0.08, 0.70));
        let cur = mk_report(mk_stats(1.0, 0.01, 0.95));
        assert!(regressions_against_baseline(&base, &cur, EvalTolerances::default()).is_empty());
    }

    #[test]
    fn legacy_baseline_with_council_section_still_parses() {
        let json = r#"{"schemaVersion":1,"startedAt":"x","aatxeVersion":"0.1.0",
            "council":{"casesTotal":5},"councilUsedRealLlm":false}"#;
        let r: EvalReport = serde_json::from_str(json).unwrap();
        assert!(r.stats.is_none());
    }
}
