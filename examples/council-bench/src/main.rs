//! Microbenchmarks for the aatxe agent council *pure* logic.
//!
//! We deliberately bench only the pieces that don't touch the network:
//! diff parsing, path filtering, chunking, prompt assembly, JSON response
//! parsing, deterministic synthesis. The full pipeline (`run_council`) is
//! also benched against a [`aatxe_council::llm::StubClient`] so the bench
//! captures orchestration overhead (thread spawn, hashmap allocations,
//! etc.) without LLM latency drowning it out.
//!
//! Emit a single `RunReport` JSON on stdout — `aatxe run --lang rust`
//! ingests it directly. The benchmark suite is service-tagged
//! `aatxe-council` so the regression gate is namespaced.

use aatxe_bench::{bench, Suite};
use aatxe_council::diff::{
    chunk_for_review, filter_ignored, parse_unified_diff, ChunkPolicy, DEFAULT_IGNORED_PATTERNS,
};
use aatxe_council::llm::StubClient;
use aatxe_council::parse::{parse_findings_json, parse_judge_verdicts};
use aatxe_council::persona::Persona;
use aatxe_council::pipeline::{run_council, CouncilOptions};
use aatxe_council::prompt::{build_judge_request, build_proposer_request};
use aatxe_council::synth::{dedup_and_rank, SynthOptions};
use aatxe_council::types::{Finding, FindingCategory, Severity};

const DIFF_SAMPLE: &str = include_str!("../fixtures/sample.diff");

fn main() {
    let mut suite = Suite::new("aatxe-council");

    // --- 1. Diff parsing -------------------------------------------------
    bench(&mut suite, "diff::parse_unified_diff", || {
        let v = parse_unified_diff(std::hint::black_box(DIFF_SAMPLE));
        std::hint::black_box(v);
    });

    // --- 2. Path filtering -----------------------------------------------
    let files = parse_unified_diff(DIFF_SAMPLE);
    bench(&mut suite, "diff::filter_ignored", || {
        let cloned = std::hint::black_box(files.clone());
        let (kept, dropped) = filter_ignored(cloned, DEFAULT_IGNORED_PATTERNS);
        std::hint::black_box((kept, dropped));
    });

    // --- 3. Chunking -----------------------------------------------------
    let kept = filter_ignored(files.clone(), DEFAULT_IGNORED_PATTERNS).0;
    bench(&mut suite, "diff::chunk_for_review", || {
        let chunks = chunk_for_review(std::hint::black_box(&kept), ChunkPolicy::default());
        std::hint::black_box(chunks);
    });

    // --- 4. Prompt assembly (proposer) -----------------------------------
    let chunks = chunk_for_review(&kept, ChunkPolicy::default());
    let one_chunk = chunks
        .first()
        .cloned()
        .expect("fixture has files after filter");
    bench(&mut suite, "prompt::build_proposer_request", || {
        let req = build_proposer_request(
            "kimi-k2.6",
            Persona::Correctness,
            std::hint::black_box(&one_chunk),
            "",
            "",
        );
        std::hint::black_box(req);
    });

    // --- 5. JSON response parsing (proposer) -----------------------------
    let canned_findings = r#"{"findings":[
        {"file":"src/x.rs","line":10,"severity":"major","category":"correctness","title":"unwrap on None","rationale":"will panic on production input"},
        {"file":"src/x.rs","line":42,"severity":"critical","category":"security","title":"SSRF in fetch","rationale":"url host is not validated"},
        {"file":"src/y.rs","line":3,"severity":"nit","title":"naming","rationale":"call it foo_bar not fooBar"}
    ]}"#;
    bench(&mut suite, "parse::parse_findings_json", || {
        let f = parse_findings_json(std::hint::black_box(canned_findings), Persona::Correctness);
        std::hint::black_box(f);
    });

    // --- 6. Synth dedup + rank -------------------------------------------
    let many_findings = make_synth_workload(64);
    bench(&mut suite, "synth::dedup_and_rank", || {
        let out = dedup_and_rank(
            std::hint::black_box(many_findings.clone()),
            SynthOptions::default(),
        );
        std::hint::black_box(out);
    });

    // --- 7. Judge prompt + parse -----------------------------------------
    let candidates = dedup_and_rank(many_findings.clone(), SynthOptions::default());
    bench(&mut suite, "prompt::build_judge_request", || {
        let req = build_judge_request("kimi-k2.6", std::hint::black_box(&candidates), "");
        std::hint::black_box(req);
    });
    let canned_verdicts = make_canned_verdicts(candidates.len());
    bench(&mut suite, "parse::parse_judge_verdicts", || {
        let v = parse_judge_verdicts(std::hint::black_box(&canned_verdicts), candidates.len());
        std::hint::black_box(v);
    });

    // --- 8. End-to-end with a stub LLM -----------------------------------
    let stub = StubClient::default()
        .with("specialty: correctness", canned_findings)
        .with("specialty: security", "{\"findings\":[]}")
        .with("specialty: performance", "{\"findings\":[]}")
        .with("specialty: maintainability", "{\"findings\":[]}")
        .with(
            "judge on the aatxe",
            r#"{"verdicts":[{"index":0,"verdict":"keep","confidence":0.9}]}"#,
        );
    let opts = CouncilOptions {
        model: "stub".into(),
        repo: "x/y".into(),
        pr: 1,
        ..CouncilOptions::default()
    };
    bench(&mut suite, "pipeline::run_council_with_stub", || {
        let report =
            run_council(std::hint::black_box(DIFF_SAMPLE), &opts, &stub).expect("stub never fails");
        std::hint::black_box(report);
    });

    suite.emit_stdout();
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
            file: format!("src/mod_{}.rs", i % 16),
            line: Some(((i * 7) % 200) as u32 + 1),
            severity: match i % 4 {
                0 => Severity::Critical,
                1 => Severity::Major,
                2 => Severity::Minor,
                _ => Severity::Nit,
            },
            category: cats[i % cats.len()],
            title: format!("issue cluster {} variant {}", i % 8, i % 3),
            rationale: format!("rationale {i}"),
            suggestion: None,
            raised_by: Some(cats[i % cats.len()].label().into()),
        });
    }
    v
}

fn make_canned_verdicts(n: usize) -> String {
    let mut s = String::from("{\"verdicts\":[");
    for i in 0..n {
        if i > 0 {
            s.push(',');
        }
        let v = match i % 5 {
            0 => "drop",
            1 => "downgrade",
            _ => "keep",
        };
        let conf = 0.4 + ((i % 6) as f64) * 0.1;
        s.push_str(&format!(
            "{{\"index\":{i},\"verdict\":\"{v}\",\"confidence\":{conf}}}"
        ));
    }
    s.push_str("]}");
    s
}
