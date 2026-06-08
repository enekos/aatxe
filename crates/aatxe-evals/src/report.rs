//! Reportable shape — what `aatxe evals` writes to disk and what CI
//! diffs against the baseline.

use crate::council::CouncilEvalSummary;
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
    /// Aatxe + council version strings (e.g. cargo pkg version).
    pub aatxe_version: String,
    /// Optional — set when the council eval ran. None when only stats ran.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub council: Option<CouncilEvalSummary>,
    /// Optional — set when the stats eval ran.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stats: Option<StatsEvalSummary>,
    /// Whether the council eval used the deterministic stub LLM. Stub
    /// runs prove plumbing; only `real_llm = true` runs prove model
    /// quality. We carry this flag into the report so baselines can't
    /// silently regress from "real" to "stub".
    #[serde(default)]
    pub council_used_real_llm: bool,
}

pub const EVAL_SCHEMA_VERSION: u32 = 1;

/// Maximum allowed regression in each headline metric before
/// [`regressions_against_baseline`] flags it. Numbers are absolute deltas:
/// a `critical_recall` tolerance of `0.05` means the new run is allowed to
/// be up to 5 percentage points worse than the baseline before the gate
/// fires.
#[derive(Debug, Clone, Copy)]
pub struct EvalTolerances {
    pub critical_recall_drop: f64,
    pub critical_precision_drop: f64,
    pub critical_f1_drop: f64,
    pub severity_calibration_mae_rise: f64,
    pub judge_brier_score_rise: f64,
    pub avg_fp_per_case_rise: f64,
    pub forbidden_path_findings_rise: u32,
    pub stats_pass_rate_drop: f64,
    pub stats_null_fpr_rise: f64,
    pub stats_borderline_tpr_drop: f64,
}

impl Default for EvalTolerances {
    fn default() -> Self {
        Self {
            critical_recall_drop: 0.05,
            critical_precision_drop: 0.10,
            critical_f1_drop: 0.05,
            severity_calibration_mae_rise: 0.30,
            judge_brier_score_rise: 0.05,
            avg_fp_per_case_rise: 0.50,
            forbidden_path_findings_rise: 0,
            stats_pass_rate_drop: 0.0,
            stats_null_fpr_rise: 0.05,
            stats_borderline_tpr_drop: 0.10,
        }
    }
}

impl EvalTolerances {
    /// Looser tolerances for real-LLM baselines. Real LLM calls are
    /// non-deterministic: temperature isn't fixable across providers,
    /// proposers run in parallel with race-dependent ordering, and the
    /// judge's confidence numbers naturally jitter ±0.1 between runs.
    /// The stub tolerances ([`Default`]) are calibrated for a
    /// deterministic backend and would false-trigger on every real-LLM
    /// re-run. These are the per-metric bands that empirically survive
    /// the real-claude baseline's measured run-to-run variance.
    ///
    /// `forbidden_path_findings_rise` stays at 0 — a finding on a
    /// lockfile is a calibration bug regardless of backend.
    pub fn for_real_llm() -> Self {
        Self {
            critical_recall_drop: 0.10,
            critical_precision_drop: 0.10,
            critical_f1_drop: 0.10,
            severity_calibration_mae_rise: 0.30,
            judge_brier_score_rise: 0.10,
            avg_fp_per_case_rise: 1.00,
            forbidden_path_findings_rise: 0,
            stats_pass_rate_drop: 0.0,
            stats_null_fpr_rise: 0.05,
            stats_borderline_tpr_drop: 0.10,
        }
    }

    /// Pick the appropriate tolerances for a given baseline: real-LLM
    /// baselines auto-relax to [`Self::for_real_llm`]; stub baselines
    /// stay on [`Default::default`]. The CLI uses this to avoid burdening
    /// the user with a `--tolerances` flag in the common case.
    pub fn auto_for(baseline: &EvalReport) -> Self {
        if baseline.council_used_real_llm {
            Self::for_real_llm()
        } else {
            Self::default()
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
    // Refuse to compare a real-LLM baseline against a stub run — that's
    // not apples-to-apples and would otherwise hide a regression.
    if baseline.council_used_real_llm && !current.council_used_real_llm {
        out.push(EvalRegression {
            metric: "council_real_llm".into(),
            baseline: 1.0,
            current: 0.0,
            delta_worse: 1.0,
            tolerance: 0.0,
            note: "baseline used real LLM; current run used stub. Re-run with real Kimi or update baseline.".into(),
        });
    }

    if let (Some(b), Some(c)) = (baseline.council.as_ref(), current.council.as_ref()) {
        check_drop(
            &mut out,
            "council.critical_recall",
            b.critical_recall,
            c.critical_recall,
            tolerances.critical_recall_drop,
            "critical-severity bugs not flagged at the same rate",
        );
        check_drop(
            &mut out,
            "council.critical_precision",
            b.critical_precision,
            c.critical_precision,
            tolerances.critical_precision_drop,
            "more false positives among critical findings",
        );
        check_drop(
            &mut out,
            "council.critical_f1",
            b.critical_f1,
            c.critical_f1,
            tolerances.critical_f1_drop,
            "combined precision+recall on critical findings degraded",
        );
        check_rise(
            &mut out,
            "council.severity_calibration_mae",
            b.severity_calibration_mae,
            c.severity_calibration_mae,
            tolerances.severity_calibration_mae_rise,
            "model is mis-severing findings more often",
        );
        check_rise(
            &mut out,
            "council.judge_brier_score",
            b.judge_brier_score,
            c.judge_brier_score,
            tolerances.judge_brier_score_rise,
            "judge confidence is less calibrated",
        );
        check_rise(
            &mut out,
            "council.avg_false_positives_per_case",
            b.avg_false_positives_per_case,
            c.avg_false_positives_per_case,
            tolerances.avg_fp_per_case_rise,
            "false-positive rate per case rose",
        );
        if c.forbidden_path_findings
            > b.forbidden_path_findings + tolerances.forbidden_path_findings_rise
        {
            out.push(EvalRegression {
                metric: "council.forbidden_path_findings".into(),
                baseline: b.forbidden_path_findings as f64,
                current: c.forbidden_path_findings as f64,
                delta_worse: (c.forbidden_path_findings - b.forbidden_path_findings) as f64,
                tolerance: tolerances.forbidden_path_findings_rise as f64,
                note: "findings landed on forbidden paths (lockfiles, generated code)".into(),
            });
        }
    }

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
    use crate::council::CouncilEvalSummary;
    use crate::stats::StatsEvalSummary;

    fn mk_council(
        critical_recall: f64,
        critical_precision: f64,
        critical_f1: f64,
        sev_mae: f64,
        brier: f64,
        fp_per_case: f64,
        forbidden: u32,
    ) -> CouncilEvalSummary {
        CouncilEvalSummary {
            cases_total: 5,
            cases_fully_recalled: 4,
            cases_over_cap: 0,
            cases_with_judge_error: 0,
            recall: crate::council::SeverityRecall::default(),
            critical_precision,
            critical_recall,
            critical_f1,
            severity_calibration_mae: sev_mae,
            judge_brier_score: brier,
            avg_false_positives_per_case: fp_per_case,
            forbidden_path_findings: forbidden,
            avg_latency_ms: 1000,
            total_prompt_tokens: 10000,
            total_completion_tokens: 4000,
            per_case: vec![],
        }
    }

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

    fn mk_report(
        council: Option<CouncilEvalSummary>,
        stats: Option<StatsEvalSummary>,
    ) -> EvalReport {
        EvalReport {
            schema_version: EVAL_SCHEMA_VERSION,
            started_at: "1970-01-01T00:00:00Z".into(),
            aatxe_version: "test".into(),
            council,
            stats,
            council_used_real_llm: false,
        }
    }

    #[test]
    fn flat_run_against_itself_has_no_regressions() {
        let c = mk_council(0.95, 0.90, 0.92, 0.20, 0.05, 0.2, 0);
        let s = mk_stats(1.0, 0.04, 0.85);
        let r = mk_report(Some(c.clone()), Some(s.clone()));
        let regs = regressions_against_baseline(&r, &r, EvalTolerances::default());
        assert!(regs.is_empty());
    }

    #[test]
    fn critical_recall_drop_is_flagged() {
        let baseline = mk_report(Some(mk_council(0.95, 0.90, 0.92, 0.20, 0.05, 0.2, 0)), None);
        let current = mk_report(Some(mk_council(0.70, 0.90, 0.78, 0.20, 0.05, 0.2, 0)), None);
        let regs = regressions_against_baseline(&baseline, &current, EvalTolerances::default());
        assert!(regs.iter().any(|r| r.metric == "council.critical_recall"));
    }

    #[test]
    fn forbidden_path_finding_appears_even_at_zero_tolerance() {
        let baseline = mk_report(Some(mk_council(0.95, 0.9, 0.92, 0.2, 0.05, 0.2, 0)), None);
        let current = mk_report(Some(mk_council(0.95, 0.9, 0.92, 0.2, 0.05, 0.2, 1)), None);
        let regs = regressions_against_baseline(&baseline, &current, EvalTolerances::default());
        assert!(
            regs.iter()
                .any(|r| r.metric == "council.forbidden_path_findings"),
            "default tolerance must not let lockfile findings slide"
        );
    }

    #[test]
    fn stub_run_against_real_baseline_is_rejected() {
        let mut b = mk_report(Some(mk_council(0.95, 0.9, 0.92, 0.2, 0.05, 0.2, 0)), None);
        b.council_used_real_llm = true;
        let c = mk_report(Some(mk_council(0.95, 0.9, 0.92, 0.2, 0.05, 0.2, 0)), None);
        let regs = regressions_against_baseline(&b, &c, EvalTolerances::default());
        assert!(regs.iter().any(|r| r.metric == "council_real_llm"));
    }

    #[test]
    fn improvements_are_not_flagged() {
        let baseline = mk_report(Some(mk_council(0.70, 0.70, 0.70, 0.50, 0.20, 1.0, 1)), None);
        let current = mk_report(Some(mk_council(0.95, 0.95, 0.95, 0.10, 0.02, 0.1, 0)), None);
        let regs = regressions_against_baseline(&baseline, &current, EvalTolerances::default());
        assert!(regs.is_empty());
    }

    // --------------------------- M2.5 real-LLM ---------------------------

    #[test]
    fn real_llm_tolerances_are_looser_than_default() {
        let stub = EvalTolerances::default();
        let real = EvalTolerances::for_real_llm();
        // FP/case has the biggest variance between real-LLM runs (judge
        // jitter compounds across 24 cases) — must be the most relaxed.
        assert!(real.avg_fp_per_case_rise > stub.avg_fp_per_case_rise);
        // Recall/F1 also relax because real LLM can lose a marginal catch
        // and we don't want every flaky run to gate the build.
        assert!(real.critical_recall_drop > stub.critical_recall_drop);
        assert!(real.critical_f1_drop > stub.critical_f1_drop);
        // Brier rises a bit since judge confidence is non-deterministic.
        assert!(real.judge_brier_score_rise > stub.judge_brier_score_rise);
        // Forbidden-path findings MUST stay at 0 — a finding on a
        // lockfile / generated file is a calibration bug regardless of
        // backend.
        assert_eq!(real.forbidden_path_findings_rise, 0);
        // MAE doesn't get further relaxation — real-LLM severity calls
        // are no more variable than the existing 0.30 band already permits.
        assert_eq!(
            real.severity_calibration_mae_rise,
            stub.severity_calibration_mae_rise
        );
    }

    #[test]
    fn auto_for_picks_real_llm_tolerances_when_baseline_used_real_llm() {
        let mut real_baseline = mk_report(
            Some(mk_council(0.75, 1.0, 0.857, 0.30, 0.35, 2.375, 0)),
            None,
        );
        real_baseline.council_used_real_llm = true;
        let t = EvalTolerances::auto_for(&real_baseline);
        let real = EvalTolerances::for_real_llm();
        assert_eq!(t.avg_fp_per_case_rise, real.avg_fp_per_case_rise);
        assert_eq!(t.critical_f1_drop, real.critical_f1_drop);
    }

    #[test]
    fn auto_for_picks_default_tolerances_when_baseline_is_stub() {
        let stub_baseline = mk_report(Some(mk_council(0.95, 0.9, 0.92, 0.2, 0.05, 0.2, 0)), None);
        let t = EvalTolerances::auto_for(&stub_baseline);
        let default = EvalTolerances::default();
        assert_eq!(t.avg_fp_per_case_rise, default.avg_fp_per_case_rise);
        assert_eq!(t.critical_f1_drop, default.critical_f1_drop);
    }

    #[test]
    fn real_llm_band_accepts_observed_run_to_run_variance() {
        // Sanity: the committed real-claude.json numbers (critical_f1=0.857,
        // FP/case=2.375) should not produce a regression when the same eval
        // re-runs and lands within typical jitter (±0.05 on F1, ±0.5 on FP).
        let mut baseline = mk_report(
            Some(mk_council(0.75, 1.0, 0.857, 0.30, 0.35, 2.375, 0)),
            None,
        );
        baseline.council_used_real_llm = true;
        // Plausible jitter: F1 dips 0.08 (within 0.10 band), FP rises 0.8
        // (within 1.0 band), brier wobbles +0.07 (within 0.10), recall
        // dips 0.05 (within 0.10).
        let mut current = mk_report(
            Some(mk_council(0.70, 1.0, 0.777, 0.30, 0.42, 3.175, 0)),
            None,
        );
        current.council_used_real_llm = true;
        let regs =
            regressions_against_baseline(&baseline, &current, EvalTolerances::auto_for(&baseline));
        assert!(
            regs.is_empty(),
            "real-LLM jitter within band must not gate: {regs:?}"
        );
    }

    #[test]
    fn real_llm_band_still_catches_a_real_regression() {
        // FP/case jumping from 2.375 to 5.0 (+2.625) blows past the
        // looser 1.0 band → must gate.
        let mut baseline = mk_report(
            Some(mk_council(0.75, 1.0, 0.857, 0.30, 0.35, 2.375, 0)),
            None,
        );
        baseline.council_used_real_llm = true;
        let mut current = mk_report(Some(mk_council(0.75, 1.0, 0.857, 0.30, 0.35, 5.0, 0)), None);
        current.council_used_real_llm = true;
        let regs =
            regressions_against_baseline(&baseline, &current, EvalTolerances::auto_for(&baseline));
        assert!(
            regs.iter()
                .any(|r| r.metric == "council.avg_false_positives_per_case"),
            "FP/case spike must still gate under real-LLM band: {regs:?}"
        );
    }
}
