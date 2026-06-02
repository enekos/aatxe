//! Pure statistical helpers.
//!
//! Two significance tests live here:
//! * [`welch_t`] — parametric, kept as a diagnostic. Bench distributions are
//!   typically heavy-tailed, so we don't rely on it as the primary signal.
//! * [`mann_whitney_u`] — non-parametric Wilcoxon rank-sum. This is what
//!   [`crate::compare`] consults by default: it makes no normality assumption
//!   and is robust against the GC-pause / scheduler-pre-emption outliers that
//!   plague microbenchmarks.
//!
//! All functions are pure: no global state, no IO, no panics on empty input
//! (they return `0` or a benign default).

/// Per-sample summary returned by [`summarize_samples`]. All values are in the
/// same units as the input samples (nanoseconds, in aatxe's case).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Summary {
    pub mean: f64,
    pub median: f64,
    pub trimmed_mean: f64,
    pub stddev: f64,
    /// Coefficient of variation: `stddev / mean`.
    pub cv: f64,
    /// Median absolute deviation: `median(|x_i - median(x)|)`.
    pub mad: f64,
    /// Interquartile range: P75 - P25.
    pub iqr: f64,
    pub min: f64,
    pub max: f64,
    pub p50: f64,
    pub p95: f64,
    pub p99: f64,
}

impl Default for Summary {
    fn default() -> Self {
        Self {
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
        }
    }
}

/// Result of the Mann–Whitney U test.
#[derive(Debug, Clone, Copy)]
pub struct MwResult {
    pub u: f64,
    pub z: f64,
    pub p: f64,
}

/// Result of Welch's t-test.
#[derive(Debug, Clone, Copy)]
pub struct WelchResult {
    pub t: f64,
    pub df: f64,
    pub p: f64,
}

/// Arithmetic mean. Returns `0.0` for an empty slice.
pub fn mean(xs: &[f64]) -> f64 {
    if xs.is_empty() {
        return 0.0;
    }
    let sum: f64 = xs.iter().sum();
    sum / xs.len() as f64
}

/// Sample variance (Bessel-corrected). Returns 0 for arrays with < 2 elements.
pub fn variance(xs: &[f64], sample_mean: f64) -> f64 {
    if xs.len() < 2 {
        return 0.0;
    }
    let mut sum_sq = 0.0;
    for &x in xs {
        let d = x - sample_mean;
        sum_sq += d * d;
    }
    sum_sq / (xs.len() - 1) as f64
}

/// Sample standard deviation.
pub fn stddev(xs: &[f64]) -> f64 {
    variance(xs, mean(xs)).sqrt()
}

/// Coefficient of variation (`stddev / mean`). Returns 0 if mean is 0.
pub fn coefficient_of_variation(xs: &[f64]) -> f64 {
    let m = mean(xs);
    if m == 0.0 {
        0.0
    } else {
        variance(xs, m).sqrt() / m
    }
}

/// Sort a copy of `xs` ascending. Caller-owns. Uses partial_cmp; NaN sorts last.
fn sorted_copy(xs: &[f64]) -> Vec<f64> {
    let mut v: Vec<f64> = xs.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    v
}

/// Linear-interpolated percentile from an already-sorted slice.
fn percentile_sorted(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let rank = (p / 100.0) * (sorted.len() - 1) as f64;
    let lo = rank.floor() as usize;
    let hi = rank.ceil() as usize;
    if lo == hi {
        return sorted[lo];
    }
    let frac = rank - lo as f64;
    sorted[lo] * (1.0 - frac) + sorted[hi] * frac
}

/// Linear-interpolated percentile. `p` is in [0, 100]. Input is not mutated.
pub fn percentile(xs: &[f64], p: f64) -> f64 {
    if xs.is_empty() {
        return 0.0;
    }
    percentile_sorted(&sorted_copy(xs), p)
}

/// Median (50th percentile). Outlier-robust point estimate.
pub fn median(xs: &[f64]) -> f64 {
    percentile(xs, 50.0)
}

/// Trimmed mean: drop a `trim` fraction from each end, then average.
pub fn trimmed_mean(xs: &[f64], trim: f64) -> f64 {
    if xs.is_empty() {
        return 0.0;
    }
    let sorted = sorted_copy(xs);
    let cut = (sorted.len() as f64 * trim).floor() as usize;
    let slice = if cut > 0 {
        &sorted[cut..sorted.len() - cut]
    } else {
        &sorted[..]
    };
    mean(slice)
}

/// Median Absolute Deviation: `median(|x_i - median(x)|)`.
pub fn median_absolute_deviation(xs: &[f64]) -> f64 {
    if xs.is_empty() {
        return 0.0;
    }
    let sorted = sorted_copy(xs);
    let med = percentile_sorted(&sorted, 50.0);
    mad_from_sorted(&sorted, med)
}

/// Compute MAD from a sorted slice + its median without re-sorting the
/// absolute deviations. The deviation array is built as a merge of the two
/// already-sorted halves around the median.
fn mad_from_sorted(sorted: &[f64], med: f64) -> f64 {
    let n = sorted.len();
    if n == 0 {
        return 0.0;
    }

    // Boundary of elements equal to median.
    let mut left = 0usize;
    while left < n && sorted[left] < med {
        left += 1;
    }
    let mut right: isize = n as isize - 1;
    while right >= 0 && sorted[right as usize] > med {
        right -= 1;
    }
    let eq_count = (right as i64 - left as i64 + 1).max(0) as usize;

    let mut merged: Vec<f64> = vec![0.0; eq_count];
    merged.reserve(n - eq_count);

    // Walk outward: left side gives `med - x` (increasing as we move down),
    // right side gives `x - med` (increasing as we move up).
    let mut li: isize = left as isize - 1;
    let mut ri: usize = (right + 1).max(0) as usize;
    while li >= 0 && ri < n {
        let lv = med - sorted[li as usize];
        let rv = sorted[ri] - med;
        if lv <= rv {
            merged.push(lv);
            li -= 1;
        } else {
            merged.push(rv);
            ri += 1;
        }
    }
    while li >= 0 {
        merged.push(med - sorted[li as usize]);
        li -= 1;
    }
    while ri < n {
        merged.push(sorted[ri] - med);
        ri += 1;
    }

    percentile_sorted(&merged, 50.0)
}

/// Interquartile range: P75 - P25.
pub fn interquartile_range(xs: &[f64]) -> f64 {
    if xs.is_empty() {
        return 0.0;
    }
    let sorted = sorted_copy(xs);
    percentile_sorted(&sorted, 75.0) - percentile_sorted(&sorted, 25.0)
}

/// Compute all common statistics for a sample in a single pass + one sort.
/// Avoids the redundant sorting that occurs when calling `median`, `percentile`,
/// `trimmed_mean`, and `iqr` independently.
pub fn summarize_samples(samples: &[f64]) -> Summary {
    let n = samples.len();
    if n == 0 {
        return Summary::default();
    }

    // Single-pass mean (Welford), variance, min, max.
    let mut mean_ = 0.0_f64;
    let mut m2 = 0.0_f64;
    let mut min = samples[0];
    let mut max = samples[0];
    for (i, &x) in samples.iter().enumerate() {
        if x < min {
            min = x;
        }
        if x > max {
            max = x;
        }
        let delta = x - mean_;
        mean_ += delta / (i + 1) as f64;
        let delta2 = x - mean_;
        m2 += delta * delta2;
    }
    let variance_ = if n < 2 { 0.0 } else { m2 / (n - 1) as f64 };
    let stddev_ = variance_.sqrt();
    let cv_ = if mean_ == 0.0 { 0.0 } else { stddev_ / mean_ };

    let sorted = sorted_copy(samples);
    let median_ = percentile_sorted(&sorted, 50.0);
    let p95_ = percentile_sorted(&sorted, 95.0);
    let p99_ = percentile_sorted(&sorted, 99.0);
    let p25_ = percentile_sorted(&sorted, 25.0);
    let p75_ = percentile_sorted(&sorted, 75.0);
    let iqr_ = p75_ - p25_;

    // 5% trimmed mean.
    let cut = (n as f64 * 0.05).floor() as usize;
    let trimmed_slice = if cut > 0 && 2 * cut < n {
        &sorted[cut..n - cut]
    } else {
        &sorted[..]
    };
    let trimmed_mean_ = mean(trimmed_slice);

    let mad_ = mad_from_sorted(&sorted, median_);

    Summary {
        mean: mean_,
        median: median_,
        trimmed_mean: trimmed_mean_,
        stddev: stddev_,
        cv: cv_,
        mad: mad_,
        iqr: iqr_,
        min,
        max,
        p50: median_,
        p95: p95_,
        p99: p99_,
    }
}

/// Welch's t-test (unequal variances). Two-tailed p-value via the Student-t
/// CDF with Welch–Satterthwaite degrees of freedom.
///
/// Kept as a diagnostic — [`mann_whitney_u`] is the primary signal because
/// it makes no normality assumption.
pub fn welch_t(a: &[f64], b: &[f64]) -> WelchResult {
    if a.len() < 2 || b.len() < 2 {
        return WelchResult {
            t: 0.0,
            df: 0.0,
            p: 1.0,
        };
    }
    let ma = mean(a);
    let mb = mean(b);
    let va = variance(a, ma);
    let vb = variance(b, mb);
    let na = a.len() as f64;
    let nb = b.len() as f64;
    let se_sq = va / na + vb / nb;
    if se_sq == 0.0 {
        return WelchResult {
            t: 0.0,
            df: na + nb - 2.0,
            p: 1.0,
        };
    }
    let t = (ma - mb) / se_sq.sqrt();
    let df =
        (se_sq * se_sq) / ((va * va) / (na * na * (na - 1.0)) + (vb * vb) / (nb * nb * (nb - 1.0)));
    let p = 2.0 * (1.0 - student_t_cdf(t.abs(), df));
    WelchResult {
        t,
        df,
        p: p.clamp(0.0, 1.0),
    }
}

/// Mann–Whitney U test (Wilcoxon rank-sum). Non-parametric: tests whether `a`
/// and `b` are stochastically shifted, without any distributional assumption.
///
/// Returns the smaller U statistic, the standardised z-score, and a two-tailed
/// p-value via the normal approximation with continuity + tie corrections. The
/// normal approximation is accurate once both samples have ~8+ observations;
/// below that the value should be treated as a weak signal.
pub fn mann_whitney_u(a: &[f64], b: &[f64]) -> MwResult {
    let n_a = a.len();
    let n_b = b.len();
    if n_a == 0 || n_b == 0 {
        return MwResult {
            u: 0.0,
            z: 0.0,
            p: 1.0,
        };
    }

    let sorted_a = sorted_copy(a);
    let sorted_b = sorted_copy(b);

    let mut i = 0usize;
    let mut j = 0usize;
    let mut rank = 1u64;
    let mut rank_sum_a = 0.0_f64;
    let mut tie_correction = 0.0_f64;

    while i < n_a || j < n_b {
        // Choose the smaller of the two current heads.
        let val = if j >= n_b || (i < n_a && sorted_a[i] <= sorted_b[j]) {
            sorted_a[i]
        } else {
            sorted_b[j]
        };
        let start_a = i;
        let start_b = j;
        while i < n_a && sorted_a[i] == val {
            i += 1;
        }
        while j < n_b && sorted_b[j] == val {
            j += 1;
        }
        let count_a = (i - start_a) as u64;
        let count_b = (j - start_b) as u64;
        let total = count_a + count_b;
        let end_rank = rank + total - 1;
        let avg_rank = (rank + end_rank) as f64 / 2.0;
        rank_sum_a += count_a as f64 * avg_rank;
        if total > 1 {
            let t = total as f64;
            tie_correction += t * t * t - t;
        }
        rank = end_rank + 1;
    }

    let n_a_f = n_a as f64;
    let n_b_f = n_b as f64;
    let u_a = rank_sum_a - (n_a_f * (n_a_f + 1.0)) / 2.0;
    let u_b = n_a_f * n_b_f - u_a;
    let u = u_a.min(u_b);

    let n_total = n_a_f + n_b_f;
    let mean_u = (n_a_f * n_b_f) / 2.0;
    let var_u =
        ((n_a_f * n_b_f) / 12.0) * (n_total + 1.0 - tie_correction / (n_total * (n_total - 1.0)));
    if var_u <= 0.0 {
        return MwResult { u, z: 0.0, p: 1.0 };
    }

    // Continuity correction: shift |U - μ| by 0.5 toward μ.
    let diff = ((u - mean_u).abs() - 0.5).max(0.0);
    let z = diff / var_u.sqrt();
    let p = 2.0 * standard_normal_cdf(-z);
    MwResult {
        u,
        z,
        p: p.clamp(0.0, 1.0),
    }
}

// --- distribution helpers ---

fn standard_normal_cdf(x: f64) -> f64 {
    0.5 * (1.0 + erf(x / std::f64::consts::SQRT_2))
}

/// Abramowitz & Stegun 7.1.26 approximation. Max abs error ~1.5e-7 — plenty
/// for benchmark p-values where we only care about the threshold crossing.
fn erf(x: f64) -> f64 {
    let sign = if x < 0.0 { -1.0 } else { 1.0 };
    let a = x.abs();
    let t = 1.0 / (1.0 + 0.3275911 * a);
    let poly = ((((1.061405429 * t - 1.453152027) * t + 1.421413741) * t - 0.284496736) * t
        + 0.254829592)
        * t;
    sign * (1.0 - poly * (-a * a).exp())
}

fn student_t_cdf(x: f64, df: f64) -> f64 {
    if df <= 0.0 {
        return 0.5;
    }
    let xt = df / (df + x * x);
    let ib = regularized_incomplete_beta(xt, df / 2.0, 0.5);
    if x >= 0.0 {
        1.0 - 0.5 * ib
    } else {
        0.5 * ib
    }
}

fn regularized_incomplete_beta(x: f64, a: f64, b: f64) -> f64 {
    if x <= 0.0 {
        return 0.0;
    }
    if x >= 1.0 {
        return 1.0;
    }
    let ln_beta = ln_gamma(a) + ln_gamma(b) - ln_gamma(a + b);
    let front = (x.ln() * a + (1.0 - x).ln() * b - ln_beta).exp() / a;
    front * beta_continued_fraction(x, a, b)
}

fn beta_continued_fraction(x: f64, a: f64, b: f64) -> f64 {
    let max_iter = 200;
    let epsilon = 1e-15;
    let fpmin = 1e-300;
    let qab = a + b;
    let qap = a + 1.0;
    let qam = a - 1.0;
    let mut c = 1.0;
    let mut d = 1.0 - (qab * x) / qap;
    if d.abs() < fpmin {
        d = fpmin;
    }
    d = 1.0 / d;
    let mut h = d;
    for m in 1..=max_iter {
        let m_f = m as f64;
        let m2 = 2.0 * m_f;
        let mut aa = (m_f * (b - m_f) * x) / ((qam + m2) * (a + m2));
        d = 1.0 + aa * d;
        if d.abs() < fpmin {
            d = fpmin;
        }
        c = 1.0 + aa / c;
        if c.abs() < fpmin {
            c = fpmin;
        }
        d = 1.0 / d;
        h *= d * c;
        aa = (-(a + m_f) * (qab + m_f) * x) / ((a + m2) * (qap + m2));
        d = 1.0 + aa * d;
        if d.abs() < fpmin {
            d = fpmin;
        }
        c = 1.0 + aa / c;
        if c.abs() < fpmin {
            c = fpmin;
        }
        d = 1.0 / d;
        let del = d * c;
        h *= del;
        if (del - 1.0).abs() < epsilon {
            break;
        }
    }
    h
}

/// Lanczos approximation for `ln Γ(z)`. Accurate to ~15 digits for z > 0.
fn ln_gamma(z: f64) -> f64 {
    const G: usize = 7;
    const C: [f64; 9] = [
        0.999_999_999_999_809_9,
        676.520_368_121_885_1,
        -1_259.139_216_722_402_8,
        771.323_428_777_653_1,
        -176.615_029_162_140_6,
        12.507_343_278_686_905,
        -0.138_571_095_265_720_1,
        9.984_369_578_019_572e-6,
        1.505_632_735_149_311_6e-7,
    ];
    if z < 0.5 {
        return (std::f64::consts::PI / (std::f64::consts::PI * z).sin()).ln() - ln_gamma(1.0 - z);
    }
    let z = z - 1.0;
    let mut x = C[0];
    for (i, coef) in C.iter().enumerate().skip(1) {
        x += coef / (z + i as f64);
    }
    let t = z + G as f64 + 0.5;
    0.5 * (2.0 * std::f64::consts::PI).ln() + (z + 0.5) * t.ln() - t + x.ln()
}
