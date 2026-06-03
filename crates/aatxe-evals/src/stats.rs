//! Stats engine evals — synthetic A/B benchmark pairs with known ground
//! truth, scored against [`aatxe_core::compare::compare_reports`].
//!
//! ## Why synthetic
//!
//! The deterministic stats engine is the part of aatxe most consumers
//! actually depend on (the gate fires every PR; the council is opt-in).
//! Unit tests prove individual statistics are correct on hand-picked
//! inputs. What we want to know operationally is:
//!
//! * **Null behaviour** — when head and base are drawn from the same
//!   distribution, the gate must not fire. The false-positive rate
//!   should track the configured `alpha` (default 0.05).
//! * **Detection power** — when head is a known X% slower than base, how
//!   often does the gate correctly flag it as a regression at the
//!   default thresholds?
//! * **Noise robustness** — when both sides are noisy enough that the
//!   measured delta sits inside the noise envelope, the gate should
//!   abstain (Neutral / TooNoisy) rather than scream regression.
//! * **Symmetry** — improvements should be detected at the same rate
//!   regressions are, with the opposite verdict. Asymmetry would be a
//!   bug.
//!
//! Each scenario is run `iterations` times with deterministic RNG seeds
//! and the observed verdict distribution is compared to the scenario's
//! expectations.
//!
//! ## Distribution model
//!
//! Benches in the wild produce heavy-tailed timings — GC pauses,
//! scheduler pre-emption, cache effects all spike the upper tail. We
//! generate samples as `base_ns * exp(sigma * z) + outlier_lift`, where
//! `z ~ N(0, 1)` and `outlier_lift` is non-zero with small probability
//! `p_outlier`. This gives roughly the right shape (lognormal core, heavy
//! right tail) without depending on a stats crate.

use aatxe_core::compare::{compare_reports, CompareOptions};
use aatxe_core::types::{BenchRun, Language, NeutralReason, RunReport, Verdict};
use serde::{Deserialize, Serialize};

/// One stats-eval scenario. Each scenario produces a [`StatsScenarioResult`]
/// summarising what fraction of N runs ended up in each verdict bucket.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatsScenario {
    pub name: String,
    pub description: String,
    /// True median delta as a fraction: `0.0 = identical`, `0.10 = head is
    /// 10% slower than base`, `-0.05 = head is 5% faster`.
    pub true_delta: f64,
    /// Per-sample sigma in the lognormal base — roughly the coefficient of
    /// variation for small values. e.g. `0.05` ≈ 5% CV.
    pub base_sigma: f64,
    /// Probability of an outlier sample (per draw). Outliers are pulled
    /// from `Exp(base_ns)` — heavy right-tail spikes.
    pub outlier_p: f64,
    /// Number of samples per side.
    pub samples_per_side: u32,
    /// How many independent trials to run. Verdict fractions are computed
    /// over these.
    pub iterations: u32,
    /// Base median in nanoseconds. Cosmetic — the gate is scale-invariant.
    pub base_median_ns: f64,
    /// The expectation block — what *should* happen if the gate works.
    pub expects: StatsExpectations,
}

/// Pass/fail thresholds for a scenario. Optional — fields not set are
/// treated as "no expectation; record the observed value but don't gate".
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatsExpectations {
    /// Lower bound on regression-verdict fraction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_regression_rate: Option<f64>,
    /// Upper bound on regression-verdict fraction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_regression_rate: Option<f64>,
    /// Lower bound on improvement-verdict fraction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_improvement_rate: Option<f64>,
    /// Upper bound on improvement-verdict fraction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_improvement_rate: Option<f64>,
    /// Lower bound on neutral-verdict fraction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_neutral_rate: Option<f64>,
    /// Lower bound on TooNoisy fraction (within the neutrals).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_too_noisy_rate: Option<f64>,
}

/// Result of one scenario.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatsScenarioResult {
    pub name: String,
    pub iterations: u32,
    pub regression_rate: f64,
    pub improvement_rate: f64,
    pub neutral_rate: f64,
    pub too_noisy_rate: f64,
    pub below_threshold_rate: f64,
    pub not_significant_rate: f64,
    /// Mean p-value (Mann–Whitney) across iterations. Useful for spotting
    /// calibration drift even when the verdict gate itself is fine.
    pub mean_p_value: f64,
    /// Whether every expectation in [`StatsScenario::expects`] held.
    pub passed: bool,
    /// Human-readable list of the expectations that failed (if any).
    #[serde(default)]
    pub failures: Vec<String>,
}

/// Top-level summary across every scenario.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatsEvalSummary {
    pub scenarios_total: u32,
    pub scenarios_passed: u32,
    /// Pass rate ∈ [0, 1]. Strict — every expectation must hold.
    pub pass_rate: f64,
    /// Observed false-positive rate on the *null* scenario(s) — the most
    /// load-bearing single metric for the comparator: this is the rate at
    /// which the gate would spuriously fail CI when head and base are the
    /// same code.
    pub observed_null_fpr: f64,
    /// Observed TPR on the regression scenario whose true_delta sits just
    /// outside the gate's threshold. Captures detection power on the
    /// hardest legitimate regression.
    pub observed_borderline_tpr: f64,
    pub per_scenario: Vec<StatsScenarioResult>,
}

/// Standard suite of scenarios. The CLI uses this by default; callers
/// can construct their own [`StatsScenario`] vec if they want to probe
/// a specific corner.
pub fn default_scenarios() -> Vec<StatsScenario> {
    vec![
        StatsScenario {
            name: "null".into(),
            description:
                "Head and base drawn from the same distribution. FPR should track alpha (0.05)."
                    .into(),
            true_delta: 0.0,
            base_sigma: 0.05,
            outlier_p: 0.02,
            samples_per_side: 30,
            iterations: 200,
            base_median_ns: 1_000.0,
            expects: StatsExpectations {
                max_regression_rate: Some(0.10),
                max_improvement_rate: Some(0.10),
                min_neutral_rate: Some(0.80),
                ..Default::default()
            },
        },
        StatsScenario {
            name: "regression-clear-10pct".into(),
            description: "Head is 10% slower; clean noise. Gate should fire reliably.".into(),
            true_delta: 0.10,
            base_sigma: 0.05,
            outlier_p: 0.02,
            samples_per_side: 30,
            iterations: 200,
            base_median_ns: 1_000.0,
            expects: StatsExpectations {
                min_regression_rate: Some(0.90),
                max_improvement_rate: Some(0.01),
                ..Default::default()
            },
        },
        StatsScenario {
            name: "regression-borderline-6pct".into(),
            description: "Head is 6% slower — just outside the 5% threshold. \
                          Should still detect most of the time."
                .into(),
            true_delta: 0.06,
            base_sigma: 0.04,
            outlier_p: 0.02,
            samples_per_side: 40,
            iterations: 200,
            base_median_ns: 1_000.0,
            expects: StatsExpectations {
                min_regression_rate: Some(0.55),
                max_improvement_rate: Some(0.02),
                ..Default::default()
            },
        },
        StatsScenario {
            name: "improvement-clear-10pct".into(),
            description: "Head is 10% faster; clean noise. Should detect as improvement.".into(),
            true_delta: -0.10,
            base_sigma: 0.05,
            outlier_p: 0.02,
            samples_per_side: 30,
            iterations: 200,
            base_median_ns: 1_000.0,
            expects: StatsExpectations {
                min_improvement_rate: Some(0.90),
                max_regression_rate: Some(0.01),
                ..Default::default()
            },
        },
        StatsScenario {
            name: "noise-swamps-small-signal".into(),
            description: "5% true regression buried in 30% noise. Noise gate should engage; \
                          neutrals and especially TooNoisy should dominate."
                .into(),
            true_delta: 0.05,
            base_sigma: 0.30,
            outlier_p: 0.05,
            samples_per_side: 30,
            iterations: 200,
            base_median_ns: 1_000.0,
            expects: StatsExpectations {
                max_regression_rate: Some(0.20),
                min_neutral_rate: Some(0.80),
                min_too_noisy_rate: Some(0.60),
                ..Default::default()
            },
        },
        StatsScenario {
            name: "below-threshold-2pct".into(),
            description: "Head is 2% slower — below the 5% meaningful-change threshold. \
                          Should produce neutrals, not regressions."
                .into(),
            true_delta: 0.02,
            base_sigma: 0.04,
            outlier_p: 0.02,
            samples_per_side: 30,
            iterations: 200,
            base_median_ns: 1_000.0,
            expects: StatsExpectations {
                max_regression_rate: Some(0.05),
                min_neutral_rate: Some(0.90),
                ..Default::default()
            },
        },
    ]
}

/// Run every scenario and produce a summary.
pub fn run_stats_evals(scenarios: &[StatsScenario]) -> StatsEvalSummary {
    let per_scenario: Vec<StatsScenarioResult> = scenarios.iter().map(run_one_scenario).collect();
    let scenarios_total = per_scenario.len() as u32;
    let scenarios_passed = per_scenario.iter().filter(|r| r.passed).count() as u32;
    let pass_rate = if scenarios_total == 0 {
        0.0
    } else {
        scenarios_passed as f64 / scenarios_total as f64
    };
    let observed_null_fpr = per_scenario
        .iter()
        .find(|r| r.name == "null")
        .map(|r| r.regression_rate + r.improvement_rate)
        .unwrap_or(0.0);
    let observed_borderline_tpr = per_scenario
        .iter()
        .find(|r| r.name == "regression-borderline-6pct")
        .map(|r| r.regression_rate)
        .unwrap_or(0.0);
    StatsEvalSummary {
        scenarios_total,
        scenarios_passed,
        pass_rate,
        observed_null_fpr,
        observed_borderline_tpr,
        per_scenario,
    }
}

fn run_one_scenario(scenario: &StatsScenario) -> StatsScenarioResult {
    let mut counts = VerdictCounts::default();
    let mut neutral_too_noisy: u32 = 0;
    let mut neutral_below_threshold: u32 = 0;
    let mut neutral_not_significant: u32 = 0;
    let mut p_value_sum: f64 = 0.0;
    let mut p_value_n: u32 = 0;

    // Deterministic seeding — same eval run produces identical numbers
    // across machines. The seed schedule mixes the scenario name with the
    // iteration index so different scenarios don't share RNG state.
    let seed_base = hash_str(&scenario.name);

    for iter in 0..scenario.iterations {
        let mut rng = SplitMix64::new(seed_base.wrapping_add(iter as u64));
        let base_samples = synth_samples(
            &mut rng,
            scenario.base_median_ns,
            scenario.base_sigma,
            scenario.outlier_p,
            scenario.samples_per_side,
        );
        let head_samples = synth_samples(
            &mut rng,
            scenario.base_median_ns * (1.0 + scenario.true_delta),
            scenario.base_sigma,
            scenario.outlier_p,
            scenario.samples_per_side,
        );
        let base = mk_run_report(
            "scenario",
            "base",
            scenario.base_median_ns,
            scenario.base_sigma,
            base_samples,
        );
        let head = mk_run_report(
            "scenario",
            "head",
            scenario.base_median_ns * (1.0 + scenario.true_delta),
            scenario.base_sigma,
            head_samples,
        );
        let cmp = compare_reports(&base, &head, CompareOptions::default());
        let diff = &cmp.diffs[0];
        if let Some(p) = diff.p_value {
            p_value_sum += p;
            p_value_n += 1;
        }
        match diff.verdict {
            Verdict::Regression => counts.regression += 1,
            Verdict::Improvement => counts.improvement += 1,
            Verdict::Neutral => {
                counts.neutral += 1;
                match diff.neutral_reason {
                    Some(NeutralReason::TooNoisy) => neutral_too_noisy += 1,
                    Some(NeutralReason::BelowThreshold) => neutral_below_threshold += 1,
                    Some(NeutralReason::NotSignificant) => neutral_not_significant += 1,
                    None => {}
                }
            }
            Verdict::New | Verdict::Removed | Verdict::OutOfScope => {
                unreachable!("synthetic run always has both sides present")
            }
        }
    }
    let n = scenario.iterations.max(1) as f64;
    let regression_rate = counts.regression as f64 / n;
    let improvement_rate = counts.improvement as f64 / n;
    let neutral_rate = counts.neutral as f64 / n;
    let too_noisy_rate = neutral_too_noisy as f64 / n;
    let below_threshold_rate = neutral_below_threshold as f64 / n;
    let not_significant_rate = neutral_not_significant as f64 / n;
    let mean_p_value = if p_value_n == 0 {
        0.0
    } else {
        p_value_sum / p_value_n as f64
    };
    let mut failures: Vec<String> = Vec::new();
    let e = &scenario.expects;
    if let Some(lo) = e.min_regression_rate {
        if regression_rate < lo {
            failures.push(format!(
                "regression_rate {regression_rate:.3} < min {lo:.3}"
            ));
        }
    }
    if let Some(hi) = e.max_regression_rate {
        if regression_rate > hi {
            failures.push(format!(
                "regression_rate {regression_rate:.3} > max {hi:.3}"
            ));
        }
    }
    if let Some(lo) = e.min_improvement_rate {
        if improvement_rate < lo {
            failures.push(format!(
                "improvement_rate {improvement_rate:.3} < min {lo:.3}"
            ));
        }
    }
    if let Some(hi) = e.max_improvement_rate {
        if improvement_rate > hi {
            failures.push(format!(
                "improvement_rate {improvement_rate:.3} > max {hi:.3}"
            ));
        }
    }
    if let Some(lo) = e.min_neutral_rate {
        if neutral_rate < lo {
            failures.push(format!("neutral_rate {neutral_rate:.3} < min {lo:.3}"));
        }
    }
    if let Some(lo) = e.min_too_noisy_rate {
        if too_noisy_rate < lo {
            failures.push(format!("too_noisy_rate {too_noisy_rate:.3} < min {lo:.3}"));
        }
    }
    let passed = failures.is_empty();
    StatsScenarioResult {
        name: scenario.name.clone(),
        iterations: scenario.iterations,
        regression_rate,
        improvement_rate,
        neutral_rate,
        too_noisy_rate,
        below_threshold_rate,
        not_significant_rate,
        mean_p_value,
        passed,
        failures,
    }
}

#[derive(Default)]
struct VerdictCounts {
    regression: u32,
    improvement: u32,
    neutral: u32,
}

fn mk_run_report(
    name: &str,
    r#ref: &str,
    _expected_median: f64,
    _sigma: f64,
    samples: Vec<f64>,
) -> RunReport {
    // Leave the derived fields zero — `compare_reports` will recompute them
    // from `samples` because the `suspect` heuristic in
    // `aatxe_core::compare::normalize` fires when mean == median == 0.
    RunReport {
        schema_version: aatxe_core::types::SCHEMA_VERSION,
        language: Language::Rust,
        service: "stats-eval".into(),
        r#ref: r#ref.into(),
        runner: "stats-eval-synth".into(),
        started_at: "1970-01-01T00:00:00Z".into(),
        finished_at: "1970-01-01T00:00:00Z".into(),
        runs: vec![BenchRun {
            name: name.into(),
            file: "synth/scenario.rs".into(),
            iterations: samples.len() as u32,
            batch_size: 1,
            elapsed_ns: 0.0,
            samples,
            mean: 0.0,
            median: 0.0,
            trimmed_mean: 0.0,
            stddev: 0.0,
            cv: 0.0,
            mad: 0.0,
            iqr: 0.0,
            min: 0.0,
            max: 0.0,
            p50: 0.0,
            p95: 0.0,
            p99: 0.0,
            metrics: Vec::new(),
            tags: Vec::new(),
        }],
        affected_scope: None,
    }
}

fn synth_samples(rng: &mut SplitMix64, mu: f64, sigma: f64, outlier_p: f64, n: u32) -> Vec<f64> {
    let mut out = Vec::with_capacity(n as usize);
    for _ in 0..n {
        let z = standard_normal(rng);
        // Lognormal-style scaling: exp(sigma * z) keeps positivity and gives
        // a heavy right tail at sigma > 0.2.
        let mut v = mu * (sigma * z).exp();
        // Heavy-tail outlier: small probability spike modelling a GC pause
        // or a scheduler pre-emption. Bounded to a 30-80% lift (uniform
        // over that range) — large enough to bias the mean but small
        // enough that the gate's noise filter doesn't trip on a single
        // spike at default thresholds.
        let u = rng.next_f64();
        if u < outlier_p {
            let lift_fraction = 0.30 + 0.50 * rng.next_f64();
            v += mu * lift_fraction;
        }
        if v < 0.0 {
            v = 0.0;
        }
        out.push(v);
    }
    out
}

/// Box–Muller standard normal from two uniforms. Returns one variate per
/// call; the discarded second variate is fine because each call grabs two
/// fresh uniforms — slightly wasteful but trivially correct and we do not
/// need cryptographic quality here.
fn standard_normal(rng: &mut SplitMix64) -> f64 {
    let mut u1 = rng.next_f64();
    if u1 < 1e-12 {
        u1 = 1e-12;
    }
    let u2 = rng.next_f64();
    let r = (-2.0_f64 * u1.ln()).sqrt();
    let theta = 2.0_f64 * std::f64::consts::PI * u2;
    r * theta.cos()
}

/// SplitMix64 — small, fast, deterministic. Same algorithm Java 8's
/// `SplittableRandom` uses. Adequate for synthetic bench-noise generation.
#[derive(Debug, Clone)]
struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    fn new(seed: u64) -> Self {
        Self { state: seed }
    }
    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E3779B97F4A7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        z ^ (z >> 31)
    }
    fn next_f64(&mut self) -> f64 {
        // 53-bit mantissa → uniform in [0, 1).
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
}

fn hash_str(s: &str) -> u64 {
    // FNV-1a 64-bit — deterministic across builds. Used purely to derive
    // a seed; no security claims.
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn null_scenario_keeps_fpr_low() {
        let s = StatsScenario {
            name: "null".into(),
            description: "".into(),
            true_delta: 0.0,
            base_sigma: 0.05,
            outlier_p: 0.02,
            samples_per_side: 30,
            iterations: 200,
            base_median_ns: 1_000.0,
            expects: StatsExpectations::default(),
        };
        let r = run_one_scenario(&s);
        // FPR (regression + improvement) under the null should be modest.
        let fpr = r.regression_rate + r.improvement_rate;
        assert!(fpr < 0.20, "null FPR too high: {fpr}");
    }

    #[test]
    fn clear_regression_is_detected_most_of_the_time() {
        let s = StatsScenario {
            name: "regression-clear-10pct".into(),
            description: "".into(),
            true_delta: 0.10,
            base_sigma: 0.05,
            outlier_p: 0.02,
            samples_per_side: 30,
            iterations: 200,
            base_median_ns: 1_000.0,
            expects: StatsExpectations::default(),
        };
        let r = run_one_scenario(&s);
        assert!(
            r.regression_rate > 0.85,
            "regression detection too weak: {}",
            r.regression_rate
        );
    }

    #[test]
    fn clear_improvement_is_detected_and_does_not_flip_sign() {
        let s = StatsScenario {
            name: "improvement-clear-10pct".into(),
            description: "".into(),
            true_delta: -0.10,
            base_sigma: 0.05,
            outlier_p: 0.02,
            samples_per_side: 30,
            iterations: 200,
            base_median_ns: 1_000.0,
            expects: StatsExpectations::default(),
        };
        let r = run_one_scenario(&s);
        assert!(
            r.improvement_rate > 0.85,
            "improvement detection too weak: {}",
            r.improvement_rate
        );
        assert!(
            r.regression_rate < 0.02,
            "improvement scenario should not be flagged as regression"
        );
    }

    #[test]
    fn high_noise_routes_through_the_too_noisy_gate() {
        let s = StatsScenario {
            name: "noise-swamps".into(),
            description: "".into(),
            true_delta: 0.05,
            base_sigma: 0.30,
            outlier_p: 0.05,
            samples_per_side: 30,
            iterations: 200,
            base_median_ns: 1_000.0,
            expects: StatsExpectations::default(),
        };
        let r = run_one_scenario(&s);
        assert!(
            r.too_noisy_rate > 0.40,
            "noise gate didn't engage often enough: {}",
            r.too_noisy_rate
        );
    }

    #[test]
    fn default_scenarios_all_pass_their_own_expectations() {
        // This is the load-bearing test: the canned defaults must all pass
        // when run against today's stats engine. If a tightening of the gate
        // ever pushes a default scenario into failure, this test gives the
        // first signal — fix the scenario expectation (with a note in the
        // baseline) or fix the gate.
        let r = run_stats_evals(&default_scenarios());
        let failed: Vec<&StatsScenarioResult> =
            r.per_scenario.iter().filter(|s| !s.passed).collect();
        assert!(
            failed.is_empty(),
            "default scenarios failed: {:#?}",
            failed
                .iter()
                .map(|s| (s.name.clone(), s.failures.clone()))
                .collect::<Vec<_>>()
        );
        assert!(
            r.observed_null_fpr < 0.10,
            "null FPR drift: {}",
            r.observed_null_fpr
        );
    }

    #[test]
    fn determinism_same_seed_same_result() {
        let s = StatsScenario {
            name: "null".into(),
            description: "".into(),
            true_delta: 0.0,
            base_sigma: 0.05,
            outlier_p: 0.02,
            samples_per_side: 30,
            iterations: 50,
            base_median_ns: 1_000.0,
            expects: StatsExpectations::default(),
        };
        let a = run_one_scenario(&s);
        let b = run_one_scenario(&s);
        assert!((a.regression_rate - b.regression_rate).abs() < 1e-12);
        assert!((a.mean_p_value - b.mean_p_value).abs() < 1e-12);
    }
}
