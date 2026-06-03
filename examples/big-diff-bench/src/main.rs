//! Big-diff microbenchmarks for aatxe council pure logic.
//!
//! Stress-tests diff parsing, filtering, chunking, and synthesis with
//! realistically large PR diffs (hundreds to thousands of files, megabytes
//! of total body).  Emits METRIC lines for the autoresearch loop.

use aatxe_bench::{bench, Suite};
use aatxe_council::diff::{
    chunk_for_review_owned, filter_ignored, parse_unified_diff, ChunkPolicy,
    DEFAULT_IGNORED_PATTERNS,
};
use aatxe_council::persona::Persona;
use aatxe_council::prompt::build_proposer_request;
use aatxe_council::synth::{dedup_and_rank, SynthOptions};
use aatxe_council::types::{Finding, FindingCategory, Severity};

fn main() {
    let mut suite = Suite::new("aatxe-big-diff");

    // --- Workload sizes --------------------------------------------------
    // small  = 50 files × 100 lines  ≈ 50  KB
    // medium = 500 files × 200 lines ≈ 700 KB
    // large  = 1000 files × 500 lines≈ 3.5 MB
    // huge   = 2000 files × 1000 lines≈ 14 MB
    let small = generate_diff(50, 100, 0.10);
    let medium = generate_diff(500, 200, 0.10);
    let large = generate_diff(1000, 500, 0.10);
    let huge = generate_diff(2000, 1000, 0.10);

    for (label, diff) in [
        ("small", small),
        ("medium", medium),
        ("large", large),
        ("huge", huge),
    ] {
        let diff_bytes = diff.len();
        let diff_mb = diff_bytes as f64 / (1024.0 * 1024.0);

        // 1. Parse
        let parsed = {
            let d = diff.clone();
            bench(&mut suite, &format!("diff::parse_{label}"), || {
                let v = parse_unified_diff(std::hint::black_box(&d));
                std::hint::black_box(v);
            });
            parse_unified_diff(&diff)
        };

        // 2. Filter
        let filter_pool: std::cell::RefCell<Vec<Vec<aatxe_council::diff::ParsedFile>>> =
            std::cell::RefCell::new((0..3).map(|_| parsed.clone()).collect());
        let filter_idx = std::cell::Cell::new(0usize);
        let filtered = {
            bench(&mut suite, &format!("diff::filter_{label}"), || {
                let mut pool = filter_pool.borrow_mut();
                let idx = filter_idx.get();
                let slot = idx % pool.len();
                let input = pool[slot].drain(..).collect::<Vec<_>>();
                filter_idx.set(idx + 1);
                let (kept, dropped) =
                    filter_ignored(std::hint::black_box(input), DEFAULT_IGNORED_PATTERNS);
                // Recover kept files back into the pool; dropped paths are discarded.
                pool[slot] = kept;
                std::hint::black_box(dropped);
            });
            filter_ignored(parsed, DEFAULT_IGNORED_PATTERNS).0
        };

        // 3. Chunk
        // Pool a few clones so the owned-chunk benchmark can consume one
        // per iteration without re-cloning inside the measured closure.
        let chunk_pool: std::cell::RefCell<Vec<Vec<aatxe_council::diff::ParsedFile>>> =
            std::cell::RefCell::new((0..3).map(|_| filtered.clone()).collect());
        let chunk_idx = std::cell::Cell::new(0usize);
        let filtered_len = filtered.len();
        let chunks = {
            bench(&mut suite, &format!("diff::chunk_{label}"), || {
                let mut pool = chunk_pool.borrow_mut();
                let idx = chunk_idx.get();
                let slot = idx % pool.len();
                let input = pool[slot].drain(..).collect::<Vec<_>>();
                chunk_idx.set(idx + 1);
                let c = chunk_for_review_owned(std::hint::black_box(input), ChunkPolicy::default());
                // Recover the drained files back into the pool slot so the
                // next iteration can reuse it.
                pool[slot] = c.into_iter().flat_map(|ch| ch.files).collect();
                std::hint::black_box(());
            });
            chunk_for_review_owned(filtered, ChunkPolicy::default())
        };

        // 4. Proposer prompt (first chunk, or empty)
        if let Some(first) = chunks.first() {
            let chunk = first.clone();
            bench(&mut suite, &format!("prompt::proposer_{label}"), || {
                let req = build_proposer_request(
                    "kimi-k2.5",
                    Persona::Correctness,
                    std::hint::black_box(&chunk),
                    "", // ast_scope
                    "", // learned_guidance
                );
                std::hint::black_box(req);
            });
        }

        // Print synthetic METRIC lines for autoresearch ingestion
        println!("METRIC diff_mb_{label}={diff_mb:.3}");
        println!("METRIC files_parsed_{label}={}", filtered_len);
        println!("METRIC chunks_{label}={}", chunks.len());
    }

    // --- Synth dedup at fixed scale (independent of diff size) -----------
    let many_findings = make_synth_workload(256);
    bench(&mut suite, "synth::dedup_256", || {
        let out = dedup_and_rank(
            std::hint::black_box(many_findings.clone()),
            SynthOptions::default(),
        );
        std::hint::black_box(out);
    });

    suite.emit_stdout();
}

/// Generate a synthetic unified diff with `n_files` files, each having
/// roughly `lines_per_file` changed lines.  `noise_ratio` of the files
/// are lockfiles / generated files that should be filtered out.
fn generate_diff(n_files: usize, lines_per_file: usize, noise_ratio: f64) -> String {
    use std::fmt::Write;
    let noise_count = ((n_files as f64) * noise_ratio) as usize;
    let mut out = String::with_capacity(n_files * lines_per_file * 40);
    for i in 0..n_files {
        let path = if i < noise_count {
            format!("vendor/dep{i}/package-lock.json")
        } else {
            format!("src/module{i}/logic.rs")
        };
        let old_path = format!("a/{path}");
        let new_path = format!("b/{path}");
        let _ = writeln!(out, "diff --git {old_path} {new_path}");
        out.push_str("index 0000001..0000002 100644\n");
        let _ = writeln!(out, "--- {old_path}");
        let _ = writeln!(out, "+++ {new_path}");
        let _ = writeln!(out, "@@ -1,{lines_per_file} +1,{lines_per_file} @@");
        for line in 0..lines_per_file {
            let _ = writeln!(out, "-old line {line} of file {i}");
            let _ = writeln!(out, "+new line {line} of file {i}");
        }
    }
    out
}

fn make_synth_workload(n: usize) -> Vec<Finding> {
    let mut v = Vec::with_capacity(n);
    let cats = [
        FindingCategory::Correctness,
        FindingCategory::Security,
        FindingCategory::Performance,
        FindingCategory::Maintainability,
    ];
    for i in 0..n {
        v.push(Finding {
            file: format!("src/mod_{}.rs", i % 32),
            line: Some(((i * 7) % 400) as u32 + 1),
            severity: match i % 4 {
                0 => Severity::Critical,
                1 => Severity::Major,
                2 => Severity::Minor,
                _ => Severity::Nit,
            },
            category: cats[i % cats.len()],
            title: format!("issue cluster {} variant {}", i % 16, i % 5),
            rationale: format!("rationale {i}"),
            suggestion: None,
            raised_by: Some(cats[i % cats.len()].label().into()),
        });
    }
    v
}
