//! Tournament standings: rank K agents working the same task by the only
//! judges that matter here — the perf gate and the council.
//!
//! Scoring rule (documented in the UI footer as well):
//!
//! ```text
//! score = improvements − 2·regressions − 1.5·council_criticals
//! ```
//!
//! Regressions cost double for the same reason learning-corpus
//! refutations do: a confidently-shipped slowdown is worse than a missed
//! win. Ties break on the summed median delta (more negative = net
//! faster code wins).

use crate::events::Standing;
use crate::state::AgentRecord;
use aatxe_core::types::CompareReport;

/// Sum of per-bench median deltas (fraction). The cheap scalar "did this
/// agent make the whole suite faster or slower".
pub fn median_delta_sum(cmp: &CompareReport) -> f64 {
    cmp.diffs.iter().filter_map(|d| d.delta_pct).sum()
}

pub fn compute_standings(records: &[AgentRecord]) -> Vec<Standing> {
    let mut rows: Vec<Standing> = records
        .iter()
        .map(|r| {
            let (regressions, improvements, delta_sum) = match &r.latest_compare {
                Some(cmp) => (
                    cmp.summary.regressions,
                    cmp.summary.improvements,
                    median_delta_sum(cmp),
                ),
                None => (0, 0, 0.0),
            };
            let criticals = r.council_critical.unwrap_or(0);
            let score =
                f64::from(improvements) - 2.0 * f64::from(regressions) - 1.5 * f64::from(criticals);
            Standing {
                agent_id: r.agent_id.clone(),
                name: r.name.clone(),
                rank: 0,
                score,
                regressions,
                improvements,
                council_critical: r.council_critical,
                median_delta_sum: delta_sum,
            }
        })
        .collect();
    rows.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(
                a.median_delta_sum
                    .partial_cmp(&b.median_delta_sum)
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
    });
    for (i, row) in rows.iter_mut().enumerate() {
        row.rank = (i + 1) as u32;
    }
    rows
}

/// Strategy hints injected per tournament slot so the K agents explore
/// the solution space instead of converging on one approach — the same
/// heterogeneity-via-personas idea the council uses for proposers.
pub const STRATEGY_HINTS: &[&str] = &[
    "minimal-diff: smallest change that achieves the goal",
    "performance-first: optimize the hot path even at some complexity cost",
    "refactor-first: restructure for clarity, then change behavior",
    "test-first: extend test coverage before touching implementation",
    "defensive: prioritize edge cases and error handling",
    "api-design: optimize for the cleanest public surface",
];

#[cfg(test)]
mod tests {
    use super::*;
    use aatxe_core::types::{CompareReport, CompareSide, CompareSummary, Language};
    use std::path::PathBuf;

    fn record(id: &str, cmp: Option<CompareReport>, criticals: Option<u32>) -> AgentRecord {
        AgentRecord {
            agent_id: id.into(),
            name: id.into(),
            task: "t".into(),
            worktree: PathBuf::from("/tmp"),
            branch: format!("aatxe-ui/{id}"),
            tournament_id: Some("t1".into()),
            iterations: 1,
            done: false,
            latest_compare: cmp,
            council_critical: criticals,
        }
    }

    fn cmp_with(regressions: u32, improvements: u32, delta: f64) -> CompareReport {
        CompareReport {
            base: CompareSide {
                r#ref: "base".into(),
                service: "s".into(),
            },
            head: CompareSide {
                r#ref: "head".into(),
                service: "s".into(),
            },
            language: Language::Rust,
            threshold_pct: 0.05,
            alpha: 0.05,
            noisy_cv_threshold: 0.25,
            diffs: vec![aatxe_core::types::BenchDiff {
                name: "b".into(),
                base: None,
                head: None,
                delta_pct: Some(delta),
                mean_delta_pct: None,
                p_value: None,
                p_value_welch: None,
                max_cv: None,
                verdict: aatxe_core::types::Verdict::Neutral,
                neutral_reason: None,
            }],
            summary: CompareSummary {
                regressions,
                improvements,
                ..Default::default()
            },
            affected_scope: None,
        }
    }

    #[test]
    fn improvements_beat_regressions_and_criticals_cost() {
        let records = vec![
            record("clean-win", Some(cmp_with(0, 2, -0.10)), Some(0)),
            record("regressed", Some(cmp_with(1, 2, 0.05)), Some(0)),
            record("critical-flagged", Some(cmp_with(0, 2, -0.08)), Some(2)),
        ];
        let s = compute_standings(&records);
        assert_eq!(s[0].agent_id, "clean-win");
        assert_eq!(s[0].rank, 1);
        assert_eq!(s[1].agent_id, "regressed"); // 2 - 2 = 0 beats 2 - 3 = -1
        assert_eq!(s[2].agent_id, "critical-flagged");
    }

    #[test]
    fn tie_breaks_on_net_speedup() {
        let records = vec![
            record("slower", Some(cmp_with(0, 1, 0.02)), None),
            record("faster", Some(cmp_with(0, 1, -0.07)), None),
        ];
        let s = compute_standings(&records);
        assert_eq!(s[0].agent_id, "faster");
    }

    #[test]
    fn agents_without_data_rank_at_zero_score() {
        let records = vec![
            record("no-data", None, None),
            record("winner", Some(cmp_with(0, 1, -0.01)), None),
        ];
        let s = compute_standings(&records);
        assert_eq!(s[0].agent_id, "winner");
        assert_eq!(s[1].score, 0.0);
    }

    #[test]
    fn median_delta_sum_skips_missing_deltas() {
        let mut cmp = cmp_with(0, 0, -0.04);
        cmp.diffs.push(aatxe_core::types::BenchDiff {
            name: "new-bench".into(),
            base: None,
            head: None,
            delta_pct: None,
            mean_delta_pct: None,
            p_value: None,
            p_value_welch: None,
            max_cv: None,
            verdict: aatxe_core::types::Verdict::New,
            neutral_reason: None,
        });
        assert!((median_delta_sum(&cmp) + 0.04).abs() < 1e-12);
    }
}
