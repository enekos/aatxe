//! Microbenchmarks for the aatxe **statistical brain** — `aatxe-core`.
//!
//! This is the most meta bench in the repo: every other bench (council,
//! big-diff, ast, the language SDKs) funnels its raw samples through
//! [`aatxe_core::stats::summarize_samples`], and every CI gate funnels two
//! `RunReport`s through [`aatxe_core::compare::compare_reports`]. Those two
//! functions — plus the Mann–Whitney U ranker that produces the verdict's
//! significance signal — run on *every* aatxe invocation and touch no
//! network. They are exactly the code a perf regression would hurt most and
//! the code no existing bench covered.
//!
//! Workloads are generated deterministically (a SplitMix64 PRNG, no `rand`
//! dependency) so the bench input is frozen across commits — a regression
//! gate is only meaningful if the workload doesn't drift. Sample counts
//! mirror the harness's own defaults (`max_iterations = 200`) and a
//! realistic CI compare (≈40 benches/side).
//!
//! Emits a single `RunReport` JSON on stdout, service-tagged `aatxe-core`,
//! so `aatxe run --lang rust` / `aatxe compare` ingest it directly.

use aatxe_bench::{bench, black_box, Suite};
use aatxe_core::affected::extract_specifiers;
use aatxe_core::compare::{compare_reports, CompareOptions};
use aatxe_core::stats::{mann_whitney_u, median_absolute_deviation, summarize_samples, welch_t};
use aatxe_core::types::{BenchRun, Language, RunReport, SCHEMA_VERSION};

fn main() {
    let mut suite = Suite::new("aatxe-core");

    // 200 samples = the harness's `max_iterations` default — the exact size
    // `summarize_samples` sees at the end of a real bench loop.
    let samples_a = gen_samples(200, 0x5eed_0a11, 100.0, 12.0);
    let samples_b = gen_samples(200, 0x5eed_0b22, 108.0, 14.0); // ~8% shifted head

    // --- 1. Per-bench summary (runs at the end of EVERY bench loop) -------
    bench(&mut suite, "stats::summarize_samples", || {
        let s = summarize_samples(black_box(&samples_a));
        black_box(s);
    });

    // --- 2. Mann–Whitney U — the primary verdict signal in `compare` -----
    // O(n log n): two sorts + a merge-walk rank pass with tie correction.
    bench(&mut suite, "stats::mann_whitney_u", || {
        let r = mann_whitney_u(black_box(&samples_a), black_box(&samples_b));
        black_box(r);
    });

    // --- 3. MAD — outlier-robust dispersion, merge-walk from sorted halves
    bench(&mut suite, "stats::median_absolute_deviation", || {
        let m = median_absolute_deviation(black_box(&samples_a));
        black_box(m);
    });

    // --- 4. Welch's t — the diagnostic significance signal --------------
    bench(&mut suite, "stats::welch_t", || {
        let r = welch_t(black_box(&samples_a), black_box(&samples_b));
        black_box(r);
    });

    // --- 5. compare_reports — the end-to-end gate over a realistic pair --
    // 40 benches/side, each carrying 100 samples → exercises the HashMap
    // pairing, per-pair MW-U + Welch, noise gate, and summary roll-up.
    let base_report = gen_report("base", 40, 0xba5e);
    let head_report = gen_report("head", 40, 0x8ead);
    let opts = CompareOptions::default();
    bench(&mut suite, "compare::compare_reports", || {
        let cmp = compare_reports(black_box(&base_report), black_box(&head_report), opts);
        black_box(cmp);
    });

    // --- 6. extract_specifiers — the hot inner loop of affected-set ------
    // Runs once per file during import-graph traversal; regex-driven, so its
    // cost scales with repo size. Benched for all three languages.
    bench(&mut suite, "affected::extract_specifiers::rust", || {
        let v = extract_specifiers(black_box(RUST_IMPORTS), Language::Rust);
        black_box(v);
    });
    bench(&mut suite, "affected::extract_specifiers::ts", || {
        let v = extract_specifiers(black_box(TS_IMPORTS), Language::Ts);
        black_box(v);
    });
    bench(&mut suite, "affected::extract_specifiers::go", || {
        let v = extract_specifiers(black_box(GO_IMPORTS), Language::Go);
        black_box(v);
    });

    suite.emit_stdout();
}

// --- deterministic workload generation -----------------------------------

/// SplitMix64 — tiny, allocation-free, deterministic. We only need a stable
/// stream of values; statistical quality is irrelevant for a frozen bench
/// fixture. Returns a value in [0, 1).
fn next_unit(state: &mut u64) -> f64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    let bits = z ^ (z >> 31);
    // 53-bit mantissa → [0, 1).
    (bits >> 11) as f64 / (1u64 << 53) as f64
}

/// Generate `n` samples around `center` ns with roughly `spread` ns of jitter
/// plus an occasional outlier — the shape of a real microbench distribution
/// (right-skewed, a few slow tails from scheduler hiccups).
fn gen_samples(n: usize, seed: u64, center: f64, spread: f64) -> Vec<f64> {
    let mut state = seed;
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let jitter = (next_unit(&mut state) - 0.5) * 2.0 * spread;
        // 1-in-20 slow outlier (~2.5x), like a preemption.
        let outlier = if i % 20 == 7 { center * 1.5 } else { 0.0 };
        out.push((center + jitter + outlier).max(0.1));
    }
    out
}

/// Build a `RunReport` with `n` benches, each fully summarized from 100
/// samples — exactly what a runner emits and what `compare_reports` ingests.
fn gen_report(r#ref: &str, n: usize, seed: u64) -> RunReport {
    let mut runs = Vec::with_capacity(n);
    for i in 0..n {
        let center = 50.0 + (i as f64) * 3.0;
        let bench_seed = seed ^ (i as u64).wrapping_mul(2_654_435_761);
        let samples = gen_samples(100, bench_seed, center, center * 0.1);
        let s = summarize_samples(&samples);
        runs.push(BenchRun {
            name: format!("bench::case_{i:02}"),
            file: format!("src/mod_{}.rs", i % 8),
            iterations: samples.len() as u32,
            batch_size: 64,
            elapsed_ns: samples.iter().sum::<f64>() * 64.0,
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
            metrics: Vec::new(),
            tags: Vec::new(),
        });
    }
    RunReport {
        schema_version: SCHEMA_VERSION,
        language: Language::Rust,
        service: "aatxe-core".into(),
        r#ref: r#ref.into(),
        runner: "core-bench/synthetic".into(),
        started_at: "2026-01-01T00:00:00Z".into(),
        finished_at: "2026-01-01T00:00:01Z".into(),
        runs,
        affected_scope: None,
    }
}

// --- frozen import-heavy snippets for extract_specifiers -------------------
// Real-shaped import blocks (the only part of a file the extractor reads).
// Kept inline + const so the workload never drifts.

const RUST_IMPORTS: &str = r#"
use std::collections::{HashMap, HashSet, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use crate::types::{BenchRun, RunReport, Language};
use crate::stats::summarize_samples;
use super::compare::CompareOptions;
mod affected;
mod report;
extern crate serde;
use serde::{Serialize, Deserialize};
use anyhow::{Context, Result, bail};
"#;

const TS_IMPORTS: &str = r#"
import { Suite, bench } from "@aatxe/bench";
import { compareReports } from "../core/compare";
import type { RunReport, BenchRun } from "./types";
import * as path from "node:path";
import fs from "node:fs/promises";
export { Suite } from "@aatxe/bench";
const lazy = await import("./lazy.js");
require("legacy-shim");
"#;

const GO_IMPORTS: &str = r#"
package bench

import "fmt"
import "os"

import (
	"encoding/json"
	"sort"
	"time"

	"github.com/enekos/aatxe/sdk/go/internal/stats"
	core "github.com/enekos/aatxe/core"
)
"#;
