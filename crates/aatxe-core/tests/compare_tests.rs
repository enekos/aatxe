//! Verdict logic — golden cases for regression / improvement / neutral / new /
//! removed / out-of-scope.

use aatxe_core::types::{
    AffectedScope, BenchRun, Language, NeutralReason, RunReport, Verdict, SCHEMA_VERSION,
};
use aatxe_core::{compare_reports, has_regressions, CompareOptions};

fn mk_run(name: &str, file: &str, samples: Vec<f64>) -> BenchRun {
    let s = aatxe_core::stats::summarize_samples(&samples);
    let elapsed: f64 = samples.iter().sum();
    BenchRun {
        name: name.to_string(),
        file: file.to_string(),
        iterations: samples.len() as u32,
        batch_size: 1,
        elapsed_ns: elapsed,
        samples,
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
        round_medians: Vec::new(),
        metrics: Vec::new(),
        tags: Vec::new(),
    }
}

fn mk_report(runs: Vec<BenchRun>) -> RunReport {
    RunReport {
        schema_version: SCHEMA_VERSION,
        language: Language::Rust,
        service: "svc".to_string(),
        r#ref: "abcdef0".to_string(),
        runner: "test".to_string(),
        started_at: "2026-06-01T00:00:00Z".to_string(),
        finished_at: "2026-06-01T00:00:01Z".to_string(),
        runs,
        affected_scope: None,
    }
}

#[test]
fn unchanged_run_is_neutral() {
    let s: Vec<f64> = (100..160).map(|x| x as f64).collect();
    let base = mk_report(vec![mk_run("a", "x.rs", s.clone())]);
    let head = mk_report(vec![mk_run("a", "x.rs", s)]);
    let cmp = compare_reports(&base, &head, CompareOptions::default());
    assert_eq!(cmp.diffs[0].verdict, Verdict::Neutral);
    assert!(!has_regressions(&cmp));
}

#[test]
fn slower_run_is_a_regression() {
    let base_samples: Vec<f64> = (100..160).map(|x| x as f64).collect();
    // Head is +30% across the board — well above the default 5% threshold.
    let head_samples: Vec<f64> = base_samples.iter().map(|x| x * 1.3).collect();
    let base = mk_report(vec![mk_run("a", "x.rs", base_samples)]);
    let head = mk_report(vec![mk_run("a", "x.rs", head_samples)]);
    let cmp = compare_reports(&base, &head, CompareOptions::default());
    assert_eq!(cmp.diffs[0].verdict, Verdict::Regression);
    assert!(has_regressions(&cmp));
    assert!(cmp.diffs[0].delta_pct.unwrap() > 0.25);
}

#[test]
fn faster_run_is_an_improvement() {
    let base_samples: Vec<f64> = (100..160).map(|x| x as f64).collect();
    let head_samples: Vec<f64> = base_samples.iter().map(|x| x * 0.7).collect();
    let base = mk_report(vec![mk_run("a", "x.rs", base_samples)]);
    let head = mk_report(vec![mk_run("a", "x.rs", head_samples)]);
    let cmp = compare_reports(&base, &head, CompareOptions::default());
    assert_eq!(cmp.diffs[0].verdict, Verdict::Improvement);
    assert!(!has_regressions(&cmp));
}

#[test]
fn noisy_diff_is_gated_to_neutral() {
    // CV > 25%: spread samples wildly around a mean of 100. The pseudo-random
    // pattern 100 + (i*53 % 200) yields CV ≈ 60% — well above the noise gate.
    let base_samples: Vec<f64> = (0..60)
        .map(|i| 100.0 + ((i as f64 * 53.0) % 200.0))
        .collect();
    // Head differs by only ~3% in median — well below `2 × maxCv`, so the
    // noise gate should fire instead of a regression.
    let head_samples: Vec<f64> = base_samples.iter().map(|x| x * 1.03).collect();
    let base = mk_report(vec![mk_run("a", "x.rs", base_samples)]);
    let head = mk_report(vec![mk_run("a", "x.rs", head_samples)]);
    let cmp = compare_reports(&base, &head, CompareOptions::default());
    let d = &cmp.diffs[0];
    assert!(
        d.base.as_ref().unwrap().cv > 0.25,
        "test setup didn't reach CV>25%: got cv={}",
        d.base.as_ref().unwrap().cv,
    );
    assert_eq!(d.verdict, Verdict::Neutral);
    assert_eq!(d.neutral_reason, Some(NeutralReason::TooNoisy));
}

#[test]
fn new_and_removed_inventory_changes() {
    let base = mk_report(vec![mk_run("only-base", "x.rs", vec![100.0; 50])]);
    let head = mk_report(vec![mk_run("only-head", "x.rs", vec![100.0; 50])]);
    let cmp = compare_reports(&base, &head, CompareOptions::default());
    let by_name: std::collections::HashMap<_, _> = cmp
        .diffs
        .iter()
        .map(|d| (d.name.clone(), d.verdict))
        .collect();
    assert_eq!(by_name["only-base"], Verdict::Removed);
    assert_eq!(by_name["only-head"], Verdict::New);
}

#[test]
fn out_of_scope_when_affected_run_skipped_bench() {
    let base = mk_report(vec![
        mk_run(
            "kept",
            "kept.rs",
            (0..50).map(|i| 100.0 + i as f64).collect(),
        ),
        mk_run("skipped", "skipped.rs", vec![100.0; 50]),
    ]);
    let mut head = mk_report(vec![mk_run(
        "kept",
        "kept.rs",
        (0..50).map(|i| 100.0 + i as f64).collect(),
    )]);
    head.affected_scope = Some(AffectedScope {
        base: "origin/master".to_string(),
        changed_files: vec!["kept.rs".to_string()],
        bench_files: vec!["kept.rs".to_string()],
        skipped_bench_files: vec!["skipped.rs".to_string()],
    });
    let cmp = compare_reports(&base, &head, CompareOptions::default());
    let by_name: std::collections::HashMap<_, _> = cmp
        .diffs
        .iter()
        .map(|d| (d.name.clone(), d.verdict))
        .collect();
    assert_eq!(
        by_name["skipped"],
        Verdict::OutOfScope,
        "skipped bench must not be Removed"
    );
    assert!(!has_regressions(&cmp), "out-of-scope must not gate CI");
}

#[test]
fn tunable_threshold_can_loosen_or_tighten_gate() {
    let base_samples: Vec<f64> = (100..160).map(|x| x as f64).collect();
    let head_samples: Vec<f64> = base_samples.iter().map(|x| x * 1.10).collect();
    let base = mk_report(vec![mk_run("a", "x.rs", base_samples)]);
    let head = mk_report(vec![mk_run("a", "x.rs", head_samples)]);

    // Default gate (5%) flags this as a regression.
    let strict = compare_reports(&base, &head, CompareOptions::default());
    assert_eq!(strict.diffs[0].verdict, Verdict::Regression);

    // Loose gate (15%) labels it neutral with reason below-threshold.
    let loose = compare_reports(
        &base,
        &head,
        CompareOptions {
            threshold_pct: 0.15,
            ..CompareOptions::default()
        },
    );
    assert_eq!(loose.diffs[0].verdict, Verdict::Neutral);
    assert_eq!(
        loose.diffs[0].neutral_reason,
        Some(NeutralReason::BelowThreshold)
    );
}

#[test]
fn tunable_alpha_can_relax_significance_gate() {
    // 30% slower head, but only 10 samples → the MW-U p-value is around 1e-5.
    // With alpha=1e-10, the verdict should drop to Neutral / NotSignificant.
    let base_samples: Vec<f64> = (100..110).map(|x| x as f64).collect();
    let head_samples: Vec<f64> = base_samples.iter().map(|x| x * 1.30).collect();
    let base = mk_report(vec![mk_run("a", "x.rs", base_samples)]);
    let head = mk_report(vec![mk_run("a", "x.rs", head_samples)]);

    let strict = compare_reports(
        &base,
        &head,
        CompareOptions {
            alpha: 1e-10,
            ..CompareOptions::default()
        },
    );
    assert_eq!(strict.diffs[0].verdict, Verdict::Neutral);
    assert_eq!(
        strict.diffs[0].neutral_reason,
        Some(NeutralReason::NotSignificant),
        "expected NotSignificant with alpha=1e-10",
    );
}

#[test]
fn tunable_noisy_cv_threshold_can_open_or_close_gate() {
    // CV around 16% on both sides; +6% median delta.
    // Default gate (25%) ⇒ not noisy ⇒ verdict driven by significance.
    let base_samples: Vec<f64> = (0..60).map(|i| 100.0 + (i as f64 * 11.0 % 50.0)).collect();
    let head_samples: Vec<f64> = base_samples.iter().map(|x| x * 1.06).collect();
    let base = mk_report(vec![mk_run("a", "x.rs", base_samples)]);
    let head = mk_report(vec![mk_run("a", "x.rs", head_samples)]);

    let cv_observed = base.runs[0].cv;
    // Sanity: the synthetic samples should sit between 10–25% CV.
    assert!(
        (0.10..0.25).contains(&cv_observed),
        "test fixture drifted; cv={cv_observed}"
    );

    // Tighten the noise gate below the observed CV → noise-gated.
    let tightened = compare_reports(
        &base,
        &head,
        CompareOptions {
            noisy_cv_threshold: cv_observed - 0.02,
            ..CompareOptions::default()
        },
    );
    assert_eq!(tightened.diffs[0].verdict, Verdict::Neutral);
    assert_eq!(
        tightened.diffs[0].neutral_reason,
        Some(NeutralReason::TooNoisy),
    );
}

#[test]
fn schema_v1_legacy_report_is_normalised() {
    // Simulate a v1 producer: only `samples` populated; derived fields zero.
    let zeros = |name: &str, samples: Vec<f64>| BenchRun {
        name: name.into(),
        file: "x.rs".into(),
        iterations: 0,
        batch_size: 0,
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
        round_medians: Vec::new(),
        metrics: Vec::new(),
        tags: Vec::new(),
    };
    let base_samples: Vec<f64> = (100..160).map(|x| x as f64).collect();
    let head_samples: Vec<f64> = base_samples.iter().map(|x| x * 1.3).collect();
    let base = mk_report(vec![zeros("a", base_samples)]);
    let head = mk_report(vec![zeros("a", head_samples)]);
    let cmp = compare_reports(&base, &head, CompareOptions::default());
    // The comparator must have re-derived median/cv from samples and still
    // produced a Regression verdict despite the zero-filled input.
    assert_eq!(cmp.diffs[0].verdict, Verdict::Regression);
    assert!(cmp.diffs[0].delta_pct.unwrap() > 0.20);
}

#[test]
fn has_regressions_is_false_for_pure_improvement() {
    let base_samples: Vec<f64> = (100..160).map(|x| x as f64).collect();
    let head_samples: Vec<f64> = base_samples.iter().map(|x| x * 0.6).collect();
    let base = mk_report(vec![mk_run("a", "x.rs", base_samples)]);
    let head = mk_report(vec![mk_run("a", "x.rs", head_samples)]);
    let cmp = compare_reports(&base, &head, CompareOptions::default());
    assert_eq!(cmp.diffs[0].verdict, Verdict::Improvement);
    assert!(!has_regressions(&cmp), "improvement-only must not gate CI");
}

#[test]
fn summary_counts_match_verdicts() {
    let s: Vec<f64> = (100..160).map(|x| x as f64).collect();
    let s_slow: Vec<f64> = s.iter().map(|x| x * 1.3).collect();
    let s_fast: Vec<f64> = s.iter().map(|x| x * 0.6).collect();

    let base = mk_report(vec![
        mk_run("slow", "a.rs", s.clone()),
        mk_run("fast", "b.rs", s.clone()),
        mk_run("removed", "c.rs", s.clone()),
    ]);
    let head = mk_report(vec![
        mk_run("slow", "a.rs", s_slow),
        mk_run("fast", "b.rs", s_fast),
        mk_run("new", "d.rs", s),
    ]);
    let cmp = compare_reports(&base, &head, CompareOptions::default());
    assert_eq!(cmp.summary.regressions, 1);
    assert_eq!(cmp.summary.improvements, 1);
    assert_eq!(cmp.summary.new, 1);
    assert_eq!(cmp.summary.removed, 1);
}

#[test]
fn out_of_scope_not_marked_when_head_basename_does_not_match() {
    // Head ran in --affected mode but the *skipped* file is named differently
    // from the base file → comparator can't infer a match → fall back to
    // `removed` so the user sees what's missing.
    let s: Vec<f64> = (100..160).map(|x| x as f64).collect();
    let base = mk_report(vec![mk_run("gone", "ghost.rs", s)]);
    let mut head = mk_report(vec![]);
    head.affected_scope = Some(AffectedScope {
        base: "origin/master".into(),
        changed_files: vec!["unrelated.rs".into()],
        bench_files: vec!["unrelated.rs".into()],
        // Skipped file doesn't share a basename with `ghost.rs`.
        skipped_bench_files: vec!["something-else.rs".into()],
    });
    let cmp = compare_reports(&base, &head, CompareOptions::default());
    assert_eq!(cmp.diffs[0].verdict, Verdict::Removed);
}

#[test]
fn json_round_trip_preserves_shape() {
    let base_samples: Vec<f64> = (100..160).map(|x| x as f64).collect();
    let head_samples: Vec<f64> = base_samples.iter().map(|x| x * 1.3).collect();
    let base = mk_report(vec![mk_run("a", "x.rs", base_samples)]);
    let head = mk_report(vec![mk_run("a", "x.rs", head_samples)]);
    let cmp = compare_reports(&base, &head, CompareOptions::default());
    let json = serde_json::to_string(&cmp).expect("serialize");
    let parsed: aatxe_core::types::CompareReport =
        serde_json::from_str(&json).expect("deserialize");
    assert_eq!(parsed.summary.regressions, cmp.summary.regressions);
    assert_eq!(parsed.diffs.len(), cmp.diffs.len());
    assert_eq!(parsed.diffs[0].verdict, Verdict::Regression);
}

/// Deterministic per-round samples: `n` values jittered ±2% around `center`.
fn round_samples(center: f64, n: usize, phase: usize) -> Vec<f64> {
    (0..n)
        .map(|i| center * (1.0 + 0.02 * (((i * 7 + phase * 3) % 11) as f64 / 5.0 - 1.0)))
        .collect()
}

/// One single-round report per entry of `centers`.
fn rounds_of(centers: &[f64]) -> Vec<RunReport> {
    centers
        .iter()
        .enumerate()
        .map(|(i, &c)| mk_report(vec![mk_run("a", "x.rs", round_samples(c, 200, i))]))
        .collect()
}

#[test]
fn single_round_drift_is_a_false_regression_per_sample() {
    // The failure mode rounds exist for: identical code, one process run per
    // side, a 7% between-run offset. 200 samples per side make Mann–Whitney
    // call it p ≈ 0, so the per-sample gate fires.
    let base = mk_report(vec![mk_run("a", "x.rs", round_samples(1_000.0, 200, 0))]);
    let head = mk_report(vec![mk_run("a", "x.rs", round_samples(1_070.0, 200, 1))]);
    let cmp = compare_reports(&base, &head, CompareOptions::default());
    assert_eq!(cmp.diffs[0].verdict, Verdict::Regression);
    assert_eq!(cmp.diffs[0].rounds, None);
}

#[test]
fn round_drift_with_identical_code_is_not_a_regression() {
    // Same code, five interleaved rounds per side, each round drifting by up
    // to ±7%. Between-round spread swamps the side difference → neutral.
    let base =
        aatxe_core::merge_rounds(&rounds_of(&[1_000.0, 1_070.0, 960.0, 1_040.0, 990.0])).unwrap();
    let head =
        aatxe_core::merge_rounds(&rounds_of(&[1_065.0, 980.0, 1_030.0, 950.0, 1_075.0])).unwrap();
    let cmp = compare_reports(&base, &head, CompareOptions::default());
    let d = &cmp.diffs[0];
    assert_eq!(d.rounds, Some(5));
    assert_eq!(d.verdict, Verdict::Neutral, "{d:?}");
    assert!(d.p_value.unwrap() > 0.05);
}

#[test]
fn consistent_regression_across_rounds_is_detected() {
    let base =
        aatxe_core::merge_rounds(&rounds_of(&[1_000.0, 1_010.0, 990.0, 1_005.0, 995.0])).unwrap();
    let head = aatxe_core::merge_rounds(&rounds_of(&[1_100.0, 1_090.0, 1_110.0, 1_095.0, 1_105.0]))
        .unwrap();
    let cmp = compare_reports(&base, &head, CompareOptions::default());
    let d = &cmp.diffs[0];
    assert_eq!(d.rounds, Some(5));
    assert_eq!(d.verdict, Verdict::Regression, "{d:?}");
    assert!((d.delta_pct.unwrap() - 0.10).abs() < 0.01);
}

#[test]
fn too_few_rounds_fall_back_to_samples() {
    let base = aatxe_core::merge_rounds(&rounds_of(&[1_000.0, 1_000.0, 1_000.0])).unwrap();
    let head = aatxe_core::merge_rounds(&rounds_of(&[1_000.0, 1_000.0, 1_000.0])).unwrap();
    let cmp = compare_reports(&base, &head, CompareOptions::default());
    assert_eq!(cmp.diffs[0].rounds, None);
}

#[test]
fn merge_pools_samples_and_records_round_medians() {
    let merged = aatxe_core::merge_rounds(&rounds_of(&[1_000.0, 2_000.0])).unwrap();
    let run = &merged.runs[0];
    assert_eq!(run.samples.len(), 400);
    assert_eq!(run.iterations, 400);
    assert_eq!(run.round_medians.len(), 2);
    // Merging a merged report keeps every round.
    let again =
        aatxe_core::merge_rounds(&[merged.clone(), rounds_of(&[3_000.0]).remove(0)]).unwrap();
    assert_eq!(again.runs[0].round_medians.len(), 3);
    assert!(aatxe_core::merge_rounds(&[]).is_none());
}

#[test]
fn round_medians_survive_derived_stat_recompute() {
    // A producer that leaves `elapsedNs` at 0 triggers `normalize`; the round
    // medians must come through or the verdict silently drops to samples.
    let mut base = aatxe_core::merge_rounds(&rounds_of(&[1_000.0; 5])).unwrap();
    let mut head = aatxe_core::merge_rounds(&rounds_of(&[1_000.0; 5])).unwrap();
    base.runs[0].elapsed_ns = 0.0;
    head.runs[0].elapsed_ns = 0.0;
    let cmp = compare_reports(&base, &head, CompareOptions::default());
    assert_eq!(cmp.diffs[0].rounds, Some(5));
}
