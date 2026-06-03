//! # aatxe-bench
//!
//! Authoring API + JSON emitter for aatxe-compatible Rust microbenchmarks.
//!
//! ## Quick start
//!
//! ```ignore
//! use aatxe_bench::{bench, Suite};
//!
//! fn main() {
//!     let mut suite = Suite::new("my-service");
//!     bench(&mut suite, "parse_phone", || {
//!         let _ = parse_phone("+34 612 345 678");
//!     });
//!     suite.emit_stdout();
//! }
//! ```
//!
//! Running the binary that calls `suite.emit_stdout()` prints a fully-formed
//! [`aatxe_core::types::RunReport`] JSON, which `aatxe run --lang rust`
//! ingests directly.
//!
//! ## Why a builder, not `#[bench]`?
//!
//! The stable Rust toolchain does not ship `#[bench]`. Criterion is the
//! standard alternative but introduces a heavy dependency tree and a
//! HTML-report-centric flow. Aatxe-bench instead exposes a small builder
//! that integrates cleanly with `cargo run --release` — predictable,
//! statically-typed, and trivial to embed in CI.
//!
//! The sampling loop mirrors the JS authoring API:
//! * warmup iterations excluded from the measurement;
//! * adaptive sampling that stops once the CV drops below
//!   [`Options::target_cv`] *or* the time budget expires *or*
//!   [`Options::max_iterations`] is hit;
//! * automatic batch sizing for sub-µs operations.

use aatxe_core::stats::summarize_samples;
use aatxe_core::types::{BenchRun, Language, RunReport, SCHEMA_VERSION};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Re-exported so bench authors can `use aatxe_bench::black_box;` without an
/// extra `std::hint` import. Wrapping a value with `black_box` prevents LLVM
/// from constant-folding or DCE-ing the benched expression.
pub use std::hint::black_box;

/// Defeat dead-code elimination on a benched expression. Returns `v`
/// unchanged so it can be chained inline:
///
/// ```ignore
/// bench(&mut suite, "parse", || { keep(parse_phone("x")); });
/// ```
///
/// Equivalent to [`black_box`] but named for cross-SDK parity with the TS
/// (`keep`) and Go (`Keep`) authoring APIs. Use whichever reads better.
#[inline(always)]
pub fn keep<T>(v: T) -> T {
    black_box(v)
}

/// Per-bench tuning knobs. Defaults match the JS / aatxe-core defaults so a
/// service can swap between languages without changing its CI gate.
#[derive(Debug, Clone, Copy)]
pub struct Options {
    pub warmup: u32,
    pub min_iterations: u32,
    pub max_iterations: u32,
    pub time_budget: Duration,
    pub target_cv: f64,
    pub batch_size: BatchSize,
}

#[derive(Debug, Clone, Copy)]
pub enum BatchSize {
    Auto,
    Fixed(u32),
}

impl Default for Options {
    fn default() -> Self {
        Self {
            warmup: 5,
            min_iterations: 30,
            max_iterations: 200,
            time_budget: Duration::from_millis(1_000),
            target_cv: 0.02,
            batch_size: BatchSize::Auto,
        }
    }
}

/// Collection of benches that will be emitted as a single [`RunReport`].
pub struct Suite {
    service: String,
    r#ref: String,
    runner: String,
    runs: Vec<BenchRun>,
    started_at: String,
}

impl Suite {
    pub fn new(service: impl Into<String>) -> Self {
        warn_if_debug_build_once();
        let service = std::env::var("AATXE_SERVICE")
            .ok()
            .unwrap_or_else(|| service.into());
        let r#ref = std::env::var("AATXE_REF")
            .ok()
            .unwrap_or_else(|| "HEAD".to_string());
        Self {
            service,
            r#ref,
            runner: format!("aatxe-bench/{}", env!("CARGO_PKG_VERSION")),
            runs: Vec::new(),
            started_at: now_iso(),
        }
    }

    /// Run `fn_` under the bench harness and accumulate the result.
    pub fn run<F: FnMut()>(&mut self, name: &str, opts: Options, file: &str, mut fn_: F) {
        let (samples, batch_size, elapsed_ns) = run_loop(opts, &mut fn_);
        let s = summarize_samples(&samples);
        self.runs.push(BenchRun {
            name: name.to_string(),
            file: file.to_string(),
            iterations: samples.len() as u32,
            batch_size,
            elapsed_ns,
            samples: samples.clone(),
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
            metrics: Vec::new(),
            tags: Vec::new(),
        });
    }

    /// Finalise and emit the report as a JSON [`RunReport`] on stdout.
    /// Use this from your runner's `main()` so `aatxe run --lang rust` can
    /// ingest the output.
    pub fn emit_stdout(self) {
        let report = self.into_report();
        println!("{}", serde_json::to_string_pretty(&report).expect("json"));
    }

    pub fn into_report(self) -> RunReport {
        RunReport {
            schema_version: SCHEMA_VERSION,
            language: Language::Rust,
            service: self.service,
            r#ref: self.r#ref,
            runner: self.runner,
            started_at: self.started_at,
            finished_at: now_iso(),
            runs: self.runs,
            affected_scope: None,
        }
    }
}

/// Convenience wrapper that uses default [`Options`] and a synthetic file tag.
pub fn bench<F: FnMut()>(suite: &mut Suite, name: &str, fn_: F) {
    suite.run(name, Options::default(), "<inline>", fn_);
}

/// Sampling loop. Returns the per-iteration durations (in ns), the resolved
/// batch size, and the total measured wall-time (warmup excluded).
fn run_loop<F: FnMut()>(opts: Options, fn_: &mut F) -> (Vec<f64>, u32, f64) {
    // Resolve batch size: 'auto' calibrates so each timer reading takes ~50µs.
    let batch_size = match opts.batch_size {
        BatchSize::Fixed(n) => n.max(1),
        BatchSize::Auto => calibrate_batch_size(fn_),
    };

    // Warmup — discarded.
    for _ in 0..opts.warmup {
        run_batch(fn_, batch_size);
    }

    let mut samples: Vec<f64> = Vec::with_capacity(opts.max_iterations as usize);
    let total_start = Instant::now();
    let mut elapsed_ns = 0.0_f64;
    for i in 0..opts.max_iterations {
        let t0 = Instant::now();
        run_batch(fn_, batch_size);
        let batch_ns = t0.elapsed().as_nanos() as f64;
        samples.push(batch_ns / batch_size as f64);
        elapsed_ns += batch_ns;
        if i + 1 >= opts.min_iterations {
            let cv = aatxe_core::stats::coefficient_of_variation(&samples);
            let budget_done = total_start.elapsed() >= opts.time_budget;
            let cv_done = opts.target_cv > 0.0 && cv > 0.0 && cv <= opts.target_cv;
            if cv_done || budget_done {
                break;
            }
        }
    }
    (samples, batch_size, elapsed_ns)
}

static DEBUG_BUILD_WARNED: AtomicBool = AtomicBool::new(false);

/// Print a one-time stderr warning when the bench harness is invoked in a
/// build with `debug_assertions` enabled (i.e. `cargo run` without `--release`).
/// Debug builds are typically 5-50x slower and produce uncomparable numbers.
fn warn_if_debug_build_once() {
    if !cfg!(debug_assertions) {
        return;
    }
    if DEBUG_BUILD_WARNED.swap(true, Ordering::Relaxed) {
        return;
    }
    eprintln!(
        "aatxe-bench: WARNING — running in a debug build (debug_assertions=on). \
         Numbers will not be comparable to release builds. Re-run with `--release`."
    );
}

#[inline(always)]
fn run_batch<F: FnMut()>(fn_: &mut F, batch_size: u32) {
    for _ in 0..batch_size {
        fn_();
    }
}

/// Pick a batch size so each sample takes ~50µs. Amortises the ~100ns
/// `Instant::now()` call overhead for sub-µs benches.
fn calibrate_batch_size<F: FnMut()>(fn_: &mut F) -> u32 {
    let mut n: u32 = 1;
    loop {
        let t0 = Instant::now();
        run_batch(fn_, n);
        let dt = t0.elapsed();
        if dt >= Duration::from_micros(50) || n >= 1_048_576 {
            return n;
        }
        n = n.saturating_mul(2);
    }
}

fn now_iso() -> String {
    let t = time::OffsetDateTime::now_utc();
    t.format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bench_records_samples() {
        let mut s = Suite::new("test-svc");
        bench(&mut s, "noop", || {
            // Cheap fn — auto-batched.
            std::hint::black_box(1 + 1);
        });
        let r = s.into_report();
        assert_eq!(r.language, Language::Rust);
        assert_eq!(r.runs.len(), 1);
        let run = &r.runs[0];
        assert!(run.iterations >= 30, "min_iterations should be respected");
        assert!(run.median.is_finite() && run.median >= 0.0);
        assert!(run.batch_size >= 1);
    }

    #[test]
    fn calibrate_returns_at_least_one() {
        let mut f = || {
            std::hint::black_box(2 * 3);
        };
        let n = calibrate_batch_size(&mut f);
        assert!(n >= 1);
    }

    #[test]
    fn fixed_batch_size_is_honoured() {
        let mut s = Suite::new("test-svc");
        let opts = Options {
            batch_size: BatchSize::Fixed(64),
            // Tiny budgets so the test finishes fast.
            min_iterations: 5,
            max_iterations: 5,
            warmup: 0,
            time_budget: Duration::from_millis(10),
            target_cv: 0.0,
        };
        s.run("fixed", opts, "<inline>", || {
            std::hint::black_box(1 + 1);
        });
        let r = s.into_report();
        assert_eq!(r.runs[0].batch_size, 64);
        assert_eq!(r.runs[0].iterations, 5);
    }

    #[test]
    fn multiple_benches_accumulate_into_one_report() {
        let mut s = Suite::new("multi");
        bench(&mut s, "a", || {
            std::hint::black_box(1);
        });
        bench(&mut s, "b", || {
            std::hint::black_box(2);
        });
        bench(&mut s, "c", || {
            std::hint::black_box(3);
        });
        let r = s.into_report();
        assert_eq!(r.runs.len(), 3);
        let names: Vec<&str> = r.runs.iter().map(|x| x.name.as_str()).collect();
        assert_eq!(names, vec!["a", "b", "c"]);
    }

    #[test]
    fn into_report_carries_schema_version_and_language() {
        let s = Suite::new("test-svc");
        let r = s.into_report();
        assert_eq!(r.schema_version, aatxe_core::types::SCHEMA_VERSION);
        assert_eq!(r.language, Language::Rust);
        assert!(
            r.runner.starts_with("aatxe-bench/"),
            "runner string should self-identify, got {:?}",
            r.runner
        );
    }

    #[test]
    fn env_overrides_service_and_ref() {
        std::env::set_var("AATXE_SERVICE", "from-env");
        std::env::set_var("AATXE_REF", "deadbeef");
        let s = Suite::new("ignored");
        let r = s.into_report();
        // Cleanup before asserting so a failure can't leak state.
        std::env::remove_var("AATXE_SERVICE");
        std::env::remove_var("AATXE_REF");
        assert_eq!(r.service, "from-env");
        assert_eq!(r.r#ref, "deadbeef");
    }

    #[test]
    fn elapsed_ns_equals_sum_of_per_sample_times_batch() {
        // Regression: a previous version called Instant::elapsed() twice per
        // iteration, causing elapsed_ns to drift above the true measured time.
        // The invariant we restore: elapsed_ns ≈ sum(samples) * batch_size,
        // exactly equal modulo f64 rounding.
        let mut s = Suite::new("svc");
        let opts = Options {
            batch_size: BatchSize::Fixed(8),
            min_iterations: 10,
            max_iterations: 10,
            warmup: 0,
            time_budget: Duration::from_secs(60),
            target_cv: 0.0,
        };
        s.run("invariant", opts, "<inline>", || {
            std::hint::black_box(1u64.wrapping_mul(7));
        });
        let r = s.into_report();
        let run = &r.runs[0];
        let reconstructed: f64 = run.samples.iter().sum::<f64>() * run.batch_size as f64;
        let delta = (run.elapsed_ns - reconstructed).abs();
        assert!(
            delta <= 1.0,
            "elapsed_ns ({}) drifted from sum(samples)*batch_size ({}) by {}ns",
            run.elapsed_ns,
            reconstructed,
            delta
        );
    }

    #[test]
    fn keep_and_black_box_are_re_exported() {
        // Compile-time check that the ergonomic re-exports exist and apply.
        let v = keep(7u64);
        let w = black_box(v.wrapping_add(1));
        assert_eq!(w, 8);
    }

    #[test]
    fn target_cv_short_circuits_when_distribution_is_tight() {
        // Constant-time fn → CV converges to ~0 quickly; with a generous
        // max_iterations and tight target_cv, we should stop well before max.
        // We do a small amount of real work (500 adds) so measurement noise
        // is small relative to the signal, keeping CV stable.
        let mut s = Suite::new("svc");
        let opts = Options {
            batch_size: BatchSize::Fixed(512),
            min_iterations: 30,
            max_iterations: 200,
            warmup: 2,
            time_budget: Duration::from_secs(60),
            target_cv: 1.0, // generous — should trip fast on a tight distribution
        };
        s.run("tight", opts, "<inline>", || {
            let mut x = 0u64;
            for _ in 0..500 {
                x = x.wrapping_add(std::hint::black_box(1));
            }
            std::hint::black_box(x);
        });
        let r = s.into_report();
        assert!(
            r.runs[0].iterations < 200,
            "expected early stop, got {} iterations",
            r.runs[0].iterations
        );
    }
}
