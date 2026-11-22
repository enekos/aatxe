//! Stats engine — properties + known-answer tests.

use aatxe_core::stats::{
    coefficient_of_variation, interquartile_range, mann_whitney_u, mean, median,
    median_absolute_deviation, percentile, permutation_rank_sum, stddev, summarize_samples,
    trimmed_mean, welch_t,
};

fn close(a: f64, b: f64, tol: f64) -> bool {
    (a - b).abs() <= tol
}

#[test]
fn mean_handles_empty_and_single() {
    assert_eq!(mean(&[]), 0.0);
    assert_eq!(mean(&[42.0]), 42.0);
    assert!(close(mean(&[1.0, 2.0, 3.0, 4.0]), 2.5, 1e-12));
}

#[test]
fn median_is_outlier_robust() {
    // Median ignores the outlier; mean is pulled by it.
    let xs = [1.0, 2.0, 3.0, 4.0, 1_000_000.0];
    assert_eq!(median(&xs), 3.0);
    assert!(mean(&xs) > 100.0);
}

#[test]
fn percentile_interpolates() {
    let xs = [1.0, 2.0, 3.0, 4.0, 5.0];
    assert!(close(percentile(&xs, 0.0), 1.0, 1e-12));
    assert!(close(percentile(&xs, 50.0), 3.0, 1e-12));
    assert!(close(percentile(&xs, 100.0), 5.0, 1e-12));
    // Linear interp halfway between 1 and 2.
    assert!(close(percentile(&xs, 12.5), 1.5, 1e-12));
}

#[test]
fn stddev_matches_known_value() {
    let xs = [2.0, 4.0, 4.0, 4.0, 5.0, 5.0, 7.0, 9.0];
    // Mean = 5, sum of squared deviations = 32, sample variance = 32/7,
    // so sample stddev = sqrt(32/7) ≈ 2.13809.
    assert!(close(stddev(&xs), (32.0_f64 / 7.0).sqrt(), 1e-9));
}

#[test]
fn cv_returns_zero_when_mean_is_zero() {
    assert_eq!(coefficient_of_variation(&[]), 0.0);
    assert_eq!(coefficient_of_variation(&[0.0, 0.0, 0.0]), 0.0);
}

#[test]
fn trimmed_mean_drops_extremes() {
    // 5% trim of 20 samples drops 1 from each end.
    let mut xs: Vec<f64> = (1..=20).map(|i| i as f64).collect();
    xs[0] = -1_000.0;
    xs[19] = 1_000.0;
    let tm = trimmed_mean(&xs, 0.05);
    // The plain mean would be dragged way away from ~10.5; trimmed mean stays close.
    assert!((tm - 10.5).abs() < 1.0, "trimmed mean was {}", tm);
}

#[test]
fn mad_handles_duplicates() {
    // For [1, 1, 1, 1, 1] median = 1 and all deviations are 0.
    assert_eq!(median_absolute_deviation(&[1.0; 5]), 0.0);
    // [1, 2, 3, 4, 5]: median 3; abs devs [2,1,0,1,2]; median of those = 1.
    assert!(close(
        median_absolute_deviation(&[1.0, 2.0, 3.0, 4.0, 5.0]),
        1.0,
        1e-9
    ));
}

#[test]
fn iqr_is_p75_minus_p25() {
    let xs = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0];
    let iqr = interquartile_range(&xs);
    // p25 = 3, p75 = 7 → IQR = 4 (with linear interp on this set).
    assert!(close(iqr, 4.0, 1e-9));
}

#[test]
fn summary_matches_piecewise_helpers() {
    let xs: Vec<f64> = (1..=100).map(|i| i as f64).collect();
    let s = summarize_samples(&xs);
    assert!(close(s.mean, mean(&xs), 1e-9));
    assert!(close(s.median, median(&xs), 1e-9));
    assert!(close(s.stddev, stddev(&xs), 1e-9));
    assert!(close(s.p95, percentile(&xs, 95.0), 1e-9));
    assert!(close(s.iqr, interquartile_range(&xs), 1e-9));
    assert!(close(s.mad, median_absolute_deviation(&xs), 1e-9));
}

#[test]
fn mw_u_detects_shift() {
    // Two clearly-separated distributions.
    let a: Vec<f64> = (0..30).map(|i| i as f64).collect();
    let b: Vec<f64> = (100..130).map(|i| i as f64).collect();
    let r = mann_whitney_u(&a, &b);
    assert!(r.p < 1e-5, "expected very low p, got {}", r.p);
}

#[test]
fn mw_u_high_p_for_identical_distributions() {
    let a: Vec<f64> = (0..30).map(|i| i as f64).collect();
    let b: Vec<f64> = (0..30).map(|i| i as f64).collect();
    let r = mann_whitney_u(&a, &b);
    assert!(r.p > 0.5, "expected p ~1, got {}", r.p);
}

#[test]
fn mw_u_empty_inputs_return_one() {
    let r = mann_whitney_u(&[], &[1.0, 2.0]);
    assert_eq!(r.p, 1.0);
    let r = mann_whitney_u(&[1.0, 2.0], &[]);
    assert_eq!(r.p, 1.0);
}

#[test]
fn mw_u_handles_ties() {
    // All ties → U is at its mean, z = 0, p = 1.
    let a = vec![5.0; 20];
    let b = vec![5.0; 20];
    let r = mann_whitney_u(&a, &b);
    assert!(close(r.p, 1.0, 1e-9), "p was {}", r.p);
}

#[test]
fn welch_t_detects_shift() {
    let a: Vec<f64> = (0..30).map(|i| i as f64).collect();
    let b: Vec<f64> = (100..130).map(|i| i as f64).collect();
    let r = welch_t(&a, &b);
    assert!(r.p < 1e-5, "expected very low p, got {}", r.p);
}

#[test]
fn welch_t_tiny_inputs_safe() {
    let r = welch_t(&[1.0], &[2.0, 3.0]);
    assert_eq!(r.p, 1.0);
}

#[test]
fn summarize_single_sample() {
    // n=1: variance/stddev/cv all degenerate to 0; mean = median = sample.
    let s = summarize_samples(&[42.0]);
    assert_eq!(s.mean, 42.0);
    assert_eq!(s.median, 42.0);
    assert_eq!(s.min, 42.0);
    assert_eq!(s.max, 42.0);
    assert_eq!(s.stddev, 0.0);
    assert_eq!(s.cv, 0.0);
    assert_eq!(s.iqr, 0.0);
    assert_eq!(s.mad, 0.0);
}

#[test]
fn summarize_all_zero_samples() {
    // A deterministic-zero bench (e.g. an early-return) shouldn't produce NaN.
    let s = summarize_samples(&[0.0; 50]);
    assert_eq!(s.mean, 0.0);
    assert_eq!(s.median, 0.0);
    assert_eq!(s.cv, 0.0, "CV must coerce to 0 when mean=0, not NaN");
    assert_eq!(s.mad, 0.0);
    assert_eq!(s.iqr, 0.0);
    assert_eq!(s.p95, 0.0);
}

#[test]
fn summarize_handles_constant_samples() {
    // Every sample identical → zero spread, but mean/median/percentiles match.
    let s = summarize_samples(&[100.0; 30]);
    assert_eq!(s.mean, 100.0);
    assert_eq!(s.median, 100.0);
    assert_eq!(s.p99, 100.0);
    assert_eq!(s.stddev, 0.0);
    assert_eq!(s.cv, 0.0);
    assert_eq!(s.iqr, 0.0);
    assert_eq!(s.mad, 0.0);
}

#[test]
fn summarize_handles_negative_samples() {
    // Aatxe never sees negative bench durations, but the math must still
    // close — min/max must reflect the negatives and CV computes from the
    // mean as usual (sign flip is fine).
    let s = summarize_samples(&[-3.0, -1.0, 1.0, 3.0]);
    assert_eq!(s.min, -3.0);
    assert_eq!(s.max, 3.0);
    assert_eq!(s.mean, 0.0);
    assert_eq!(
        s.cv, 0.0,
        "mean=0 ⇒ CV must coerce to 0 even with non-zero stddev"
    );
    assert!(s.stddev > 0.0);
}

#[test]
fn mw_u_z_is_zero_when_distributions_overlap_perfectly() {
    // Two identical sorted arrays: U lands exactly at its mean ⇒ z = 0.
    let a: Vec<f64> = vec![1.0, 2.0, 3.0, 4.0, 5.0];
    let b = a.clone();
    let r = mann_whitney_u(&a, &b);
    // Continuity correction can knock z slightly off zero, but p ≈ 1.
    assert!(r.p > 0.5, "p should be high; got {}", r.p);
}

#[test]
fn mw_u_one_element_each_side_does_not_panic() {
    // The normal-approx variance can underflow with n=1+n=1; we expect a
    // benign p=1.0 return rather than a panic / NaN.
    let r = mann_whitney_u(&[1.0], &[2.0]);
    assert!(r.p.is_finite());
    assert!((0.0..=1.0).contains(&r.p));
}

#[test]
fn percentile_clamps_at_endpoints() {
    let xs = [10.0, 20.0, 30.0];
    assert_eq!(percentile(&xs, 0.0), 10.0);
    assert_eq!(percentile(&xs, 100.0), 30.0);
    // Off-by-one: p50 of an odd-length array is the middle element.
    assert_eq!(percentile(&xs, 50.0), 20.0);
}

#[test]
fn trimmed_mean_with_zero_trim_equals_mean() {
    let xs: Vec<f64> = (1..=20).map(|i| i as f64).collect();
    assert!(close(trimmed_mean(&xs, 0.0), mean(&xs), 1e-12));
}

#[test]
fn trimmed_mean_with_oversize_trim_falls_back_to_mean() {
    // Trim 50% of each side would empty the slice; the implementation
    // returns the overall mean rather than NaN/divide-by-zero.
    let xs = vec![1.0, 2.0, 3.0];
    let tm = trimmed_mean(&xs, 0.5);
    assert!(tm.is_finite(), "got non-finite {tm}");
}

#[test]
fn iqr_zero_for_constant_samples() {
    assert_eq!(interquartile_range(&[5.0; 20]), 0.0);
}

#[test]
fn cv_handles_single_element() {
    // Variance is 0 for n<2 ⇒ CV = 0/x = 0.
    assert_eq!(coefficient_of_variation(&[42.0]), 0.0);
}

#[test]
fn stddev_empty_is_zero() {
    assert_eq!(stddev(&[]), 0.0);
}

#[test]
fn median_absolute_deviation_empty_is_zero() {
    assert_eq!(median_absolute_deviation(&[]), 0.0);
}

#[test]
fn permutation_identical_groups_is_not_significant() {
    let xs = [100.0, 101.0, 99.0, 100.5, 100.2];
    assert!(permutation_rank_sum(&xs, &xs) > 0.5);
}

#[test]
fn permutation_fully_separated_5v5_hits_exact_minimum() {
    // Every head round slower than every base round: only the observed split
    // and its mirror are that extreme, so p = 2 / C(10, 5) = 2 / 252.
    let base = [100.0, 101.0, 102.0, 103.0, 104.0];
    let head = [110.0, 111.0, 112.0, 113.0, 114.0];
    let p = permutation_rank_sum(&head, &base);
    assert!(close(p, 2.0 / 252.0, 1e-12), "p = {p}");
}

#[test]
fn permutation_3v3_can_never_reach_alpha() {
    let p = permutation_rank_sum(&[200.0, 201.0, 202.0], &[100.0, 101.0, 102.0]);
    assert!(close(p, 0.1, 1e-12), "p = {p}");
}

#[test]
fn permutation_large_groups_use_deterministic_sampling() {
    let base: Vec<f64> = (0..12).map(|i| 100.0 + i as f64).collect();
    let head: Vec<f64> = (0..12).map(|i| 130.0 + i as f64).collect();
    let p1 = permutation_rank_sum(&head, &base);
    let p2 = permutation_rank_sum(&head, &base);
    assert_eq!(p1, p2);
    assert!(p1 < 0.001, "p = {p1}");
}

#[test]
fn permutation_empty_side_is_not_significant() {
    assert_eq!(permutation_rank_sum(&[], &[1.0]), 1.0);
}

#[test]
fn permutation_one_outlier_round_cannot_carry_the_verdict() {
    // Four head rounds interleave with base; one is a 3× outlier. A mean-based
    // statistic would chase it; the rank sum barely moves.
    let base = [100.0, 102.0, 104.0, 106.0, 108.0];
    let head = [101.0, 103.0, 105.0, 107.0, 300.0];
    assert!(permutation_rank_sum(&head, &base) > 0.4);
}
