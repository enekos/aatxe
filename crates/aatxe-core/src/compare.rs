//! Compare two [`RunReport`]s and produce a [`CompareReport`].
//!
//! ## Verdict — three independent signals
//!
//! 1. **Effect size**: relative change in *median*. Median is preferred over
//!    mean because bench distributions have heavy right tails (GC pauses,
//!    scheduler pre-emption) that pull the mean upward and inflate variance.
//! 2. **Significance**: Mann–Whitney U two-tailed p-value. Non-parametric;
//!    makes no normality assumption.
//! 3. **Noise gate**: if either side's CV is high *and* the measured delta is
//!    small relative to that noise, the diff is labelled `Neutral / TooNoisy`
//!    rather than `Regression`. Prevents the PR comment getting spammed by
//!    hopelessly noisy benches.
//!
//! ## Rounds — the unit of replication
//!
//! Samples from one process run are not independent: they share that run's
//! caches, CPU frequency and neighbours. When both sides carry at least
//! [`MIN_ROUNDS`] round medians (see [`crate::rounds::merge_rounds`]), all
//! three signals move to the round level: the delta is the ratio of the
//! median round medians, significance is an exact permutation test across
//! rounds, and the noise gate reads the between-round CV. Drift between
//! process runs then widens the noise instead of faking significance.
//! Below [`MIN_ROUNDS`] the comparator falls back to per-sample statistics.

use crate::stats::{
    coefficient_of_variation, mann_whitney_u, median, permutation_rank_sum, summarize_samples,
    welch_t,
};
use crate::types::{
    BenchDiff, BenchRun, CompareReport, CompareSide, CompareSummary, NeutralReason, RunReport,
    Verdict,
};
use std::collections::{BTreeSet, HashMap, HashSet};

/// Round medians each side needs before the verdict is computed across rounds
/// instead of samples. Four is the smallest count at which the permutation
/// test can reach p < 0.05 (4 vs 4 → minimum p ≈ 0.029).
pub const MIN_ROUNDS: usize = 4;

/// Knobs governing the verdict. Use [`CompareOptions::default`] for the
/// well-tested defaults; override individually if you want a tighter or
/// looser gate for a specific service.
#[derive(Debug, Clone, Copy)]
pub struct CompareOptions {
    /// Minimum |median delta| (as fraction of base median) to call a change
    /// meaningful. Default 0.05 (5%).
    pub threshold_pct: f64,
    /// Significance level for the Mann–Whitney U test. Default 0.05.
    pub alpha: f64,
    /// If either side's CV exceeds this and the measured delta is smaller
    /// than `2 × maxCv`, the diff is classified neutral with reason
    /// `TooNoisy`. Default 0.25 (25%).
    pub noisy_cv_threshold: f64,
}

impl Default for CompareOptions {
    fn default() -> Self {
        Self {
            threshold_pct: 0.05,
            alpha: 0.05,
            noisy_cv_threshold: 0.25,
        }
    }
}

/// Compare two run reports and classify each bench.
///
/// Pairing is by bench name (whitespace-significant). Benches present only on
/// one side are tagged [`Verdict::New`] or [`Verdict::Removed`]. When the
/// head report carries an [`AffectedScope`](crate::types::AffectedScope), benches whose file lives among
/// the explicitly-skipped files are tagged [`Verdict::OutOfScope`] instead of
/// [`Verdict::Removed`] — they must not gate CI as regressions.
pub fn compare_reports(
    base: &RunReport,
    head: &RunReport,
    options: CompareOptions,
) -> CompareReport {
    let mut base_by_name: HashMap<&str, BenchRun> = HashMap::new();
    for r in &base.runs {
        base_by_name.insert(r.name.as_str(), normalize(r));
    }
    let mut head_by_name: HashMap<&str, BenchRun> = HashMap::new();
    for r in &head.runs {
        head_by_name.insert(r.name.as_str(), normalize(r));
    }
    let mut all_names: BTreeSet<&str> = BTreeSet::new();
    all_names.extend(base_by_name.keys().copied());
    all_names.extend(head_by_name.keys().copied());

    let skipped_basenames: Option<HashSet<String>> = head.affected_scope.as_ref().map(|a| {
        a.skipped_bench_files
            .iter()
            .map(|p| basename_of(p).to_string())
            .collect()
    });

    let mut diffs: Vec<BenchDiff> = Vec::with_capacity(all_names.len());
    for name in all_names {
        let b = base_by_name.get(name).cloned();
        let h = head_by_name.get(name).cloned();
        diffs.push(diff_one(
            name.to_string(),
            b,
            h,
            options,
            skipped_basenames.as_ref(),
        ));
    }

    let summary = summarize(&diffs);

    CompareReport {
        base: CompareSide {
            r#ref: base.r#ref.clone(),
            service: base.service.clone(),
        },
        head: CompareSide {
            r#ref: head.r#ref.clone(),
            service: head.service.clone(),
        },
        language: head.language,
        threshold_pct: options.threshold_pct,
        alpha: options.alpha,
        noisy_cv_threshold: options.noisy_cv_threshold,
        diffs,
        summary,
        affected_scope: head.affected_scope.clone(),
    }
}

/// True if the comparator marked any bench as a regression. Use this for the
/// CI exit-code gate (non-zero ⇒ fail).
pub fn has_regressions(cmp: &CompareReport) -> bool {
    cmp.summary.regressions > 0
}

fn summarize(diffs: &[BenchDiff]) -> CompareSummary {
    let mut s = CompareSummary::default();
    for d in diffs {
        match d.verdict {
            Verdict::Regression => s.regressions += 1,
            Verdict::Improvement => s.improvements += 1,
            Verdict::Neutral => s.neutrals += 1,
            Verdict::New => s.new += 1,
            Verdict::Removed => s.removed += 1,
            Verdict::OutOfScope => s.out_of_scope += 1,
        }
    }
    s
}

/// Recompute derived statistics from raw samples if the producer didn't.
///
/// Modern (schema v2) reports already carry all derived fields, so this is
/// essentially a no-op on them. Legacy / minimal producers may emit only
/// `samples` + `name`, in which case we fill in the rest here.
fn normalize(r: &BenchRun) -> BenchRun {
    if r.samples.is_empty() {
        return r.clone();
    }
    // Heuristic: zero variance + zero mean + non-trivial samples ⇒ definitely
    // not filled in. Recompute unconditionally if any of the derived fields
    // looks suspect.
    let suspect = (r.mean == 0.0 && r.median == 0.0)
        || (r.elapsed_ns == 0.0 && !r.samples.is_empty())
        || (r.batch_size == 0);
    if !suspect {
        return r.clone();
    }
    let s = summarize_samples(&r.samples);
    BenchRun {
        name: r.name.clone(),
        file: r.file.clone(),
        iterations: r.samples.len() as u32,
        batch_size: if r.batch_size == 0 { 1 } else { r.batch_size },
        elapsed_ns: if r.elapsed_ns == 0.0 {
            r.samples.iter().sum()
        } else {
            r.elapsed_ns
        },
        samples: r.samples.clone(),
        mean: s.mean,
        median: s.median,
        trimmed_mean: s.trimmed_mean,
        stddev: s.stddev,
        cv: s.cv,
        mad: s.mad,
        iqr: s.iqr,
        min: s.min,
        max: s.max,
        p50: s.p50,
        p95: s.p95,
        p99: s.p99,
        round_medians: r.round_medians.clone(),
        metrics: r.metrics.clone(),
        tags: r.tags.clone(),
    }
}

fn basename_of(path: &str) -> &str {
    let i = path.rfind(['/', '\\']).map(|i| i + 1).unwrap_or(0);
    &path[i..]
}

fn diff_one(
    name: String,
    base: Option<BenchRun>,
    head: Option<BenchRun>,
    opts: CompareOptions,
    skipped_basenames: Option<&HashSet<String>>,
) -> BenchDiff {
    match (base, head) {
        (None, Some(h)) => BenchDiff {
            name,
            base: None,
            head: Some(h),
            delta_pct: None,
            mean_delta_pct: None,
            p_value: None,
            p_value_welch: None,
            max_cv: None,
            rounds: None,
            verdict: Verdict::New,
            neutral_reason: None,
        },
        (Some(b), None) => {
            // If the head was scoped via `--affected` and the bench file lives
            // among the explicitly-skipped files, this is an intentional skip,
            // not a removal — and must not be counted as a regression.
            let in_skipped = skipped_basenames
                .map(|set| set.contains(basename_of(&b.file)))
                .unwrap_or(false);
            let verdict = if in_skipped {
                Verdict::OutOfScope
            } else {
                Verdict::Removed
            };
            BenchDiff {
                name,
                base: Some(b),
                head: None,
                delta_pct: None,
                mean_delta_pct: None,
                p_value: None,
                p_value_welch: None,
                max_cv: None,
                rounds: None,
                verdict,
                neutral_reason: None,
            }
        }
        (None, None) => unreachable!("at least one side must be present"),
        (Some(b), Some(h)) => {
            let mean_delta_pct = relative_delta(b.mean, h.mean);
            let welch = welch_t(&h.samples, &b.samples);
            let by_rounds =
                b.round_medians.len() >= MIN_ROUNDS && h.round_medians.len() >= MIN_ROUNDS;
            let (delta_pct, p_value, max_cv, rounds) = if by_rounds {
                let delta = relative_delta(median(&b.round_medians), median(&h.round_medians));
                let p = permutation_rank_sum(&h.round_medians, &b.round_medians);
                let cv = coefficient_of_variation(&b.round_medians)
                    .max(coefficient_of_variation(&h.round_medians));
                let n = b.round_medians.len().min(h.round_medians.len());
                (delta, p, cv, Some(u32::try_from(n).unwrap_or(u32::MAX)))
            } else {
                let delta = relative_delta(b.median, h.median);
                let p = mann_whitney_u(&h.samples, &b.samples).p;
                (delta, p, b.cv.max(h.cv), None)
            };

            let significant = p_value < opts.alpha;
            let meaningful = delta_pct.abs() >= opts.threshold_pct;
            let noisy = max_cv > opts.noisy_cv_threshold && delta_pct.abs() < 2.0 * max_cv;

            let (verdict, neutral_reason) = if noisy {
                (Verdict::Neutral, Some(NeutralReason::TooNoisy))
            } else if significant && meaningful && delta_pct > 0.0 {
                (Verdict::Regression, None)
            } else if significant && meaningful && delta_pct < 0.0 {
                (Verdict::Improvement, None)
            } else if !meaningful {
                (Verdict::Neutral, Some(NeutralReason::BelowThreshold))
            } else {
                (Verdict::Neutral, Some(NeutralReason::NotSignificant))
            };

            BenchDiff {
                name,
                base: Some(b),
                head: Some(h),
                delta_pct: Some(delta_pct),
                mean_delta_pct: Some(mean_delta_pct),
                p_value: Some(p_value),
                p_value_welch: Some(welch.p),
                max_cv: Some(max_cv),
                rounds,
                verdict,
                neutral_reason,
            }
        }
    }
}

fn relative_delta(base: f64, head: f64) -> f64 {
    if base == 0.0 {
        0.0
    } else {
        (head - base) / base
    }
}
