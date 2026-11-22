//! Pool several single-run [`RunReport`]s into one multi-round report.
//!
//! A *round* is one independent process run of a bench suite. Samples within
//! a round share that process's cache state, frequency, placement and
//! neighbours, so they are not independent of each other — between-round
//! drift of a few percent is normal on shared runners. Comparing two single
//! rounds with a per-sample test turns that drift into a spurious "p ≈ 0".
//!
//! Running each side in several rounds — interleaved, so drift hits both
//! equally — and keeping each round's median lets [`crate::compare`] test
//! across rounds instead of across samples.

use crate::stats::summarize_samples;
use crate::types::{BenchRun, RunReport};

/// Merge single- or multi-round reports for the same side into one report.
///
/// Benches are paired by name; their samples are pooled and the derived
/// statistics recomputed, while [`BenchRun::round_medians`] records each
/// round's median (a report that already carries round medians contributes
/// all of them). Bench order follows first appearance; report metadata comes
/// from the first report, `finished_at` from the last. Returns `None` for an
/// empty slice.
pub fn merge_rounds(reports: &[RunReport]) -> Option<RunReport> {
    let (first, rest) = reports.split_first()?;
    if rest.is_empty() {
        return Some(first.clone());
    }

    let mut merged: Vec<BenchRun> = Vec::new();
    for report in reports {
        for run in &report.runs {
            match merged.iter_mut().find(|m| m.name == run.name) {
                Some(acc) => absorb(acc, run),
                None => {
                    let mut seed = run.clone();
                    seed.round_medians = round_medians_of(run);
                    merged.push(seed);
                }
            }
        }
    }
    for run in &mut merged {
        restat(run);
    }

    let mut out = first.clone();
    out.runs = merged;
    if let Some(last) = rest.last() {
        out.finished_at.clone_from(&last.finished_at);
    }
    Some(out)
}

fn round_medians_of(run: &BenchRun) -> Vec<f64> {
    if run.round_medians.is_empty() {
        let median = if run.median > 0.0 {
            run.median
        } else {
            summarize_samples(&run.samples).median
        };
        vec![median]
    } else {
        run.round_medians.clone()
    }
}

fn absorb(acc: &mut BenchRun, run: &BenchRun) {
    acc.samples.extend_from_slice(&run.samples);
    acc.iterations = acc.iterations.saturating_add(run.iterations);
    acc.elapsed_ns += run.elapsed_ns;
    acc.round_medians.extend(round_medians_of(run));
}

fn restat(run: &mut BenchRun) {
    if run.samples.is_empty() {
        return;
    }
    let s = summarize_samples(&run.samples);
    run.mean = s.mean;
    run.median = s.median;
    run.trimmed_mean = s.trimmed_mean;
    run.stddev = s.stddev;
    run.cv = s.cv;
    run.mad = s.mad;
    run.iqr = s.iqr;
    run.min = s.min;
    run.max = s.max;
    run.p50 = s.p50;
    run.p95 = s.p95;
    run.p99 = s.p99;
}
