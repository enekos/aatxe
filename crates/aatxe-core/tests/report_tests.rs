//! Markdown rendering — surface-level golden checks; we don't pin the whole
//! string because cosmetic tweaks shouldn't break tests, but we *do* pin
//! invariants downstream tooling cares about (sticky marker on line 1, etc.).

use aatxe_core::report::{format_ns, STICKY_MARKER};
use aatxe_core::types::{
    AffectedScope, BenchRun, CompareReport, CompareSide, CompareSummary, Language, NeutralReason,
    Verdict,
};
use aatxe_core::{render_markdown, types::BenchDiff};

fn run(name: &str) -> BenchRun {
    BenchRun {
        name: name.to_string(),
        file: format!("{}.rs", name),
        iterations: 50,
        batch_size: 1,
        elapsed_ns: 5000.0,
        samples: (0..50).map(|i| 100.0 + i as f64).collect(),
        mean: 124.5,
        median: 124.5,
        trimmed_mean: 124.5,
        stddev: 14.5,
        cv: 0.116,
        mad: 12.0,
        iqr: 24.0,
        min: 100.0,
        max: 149.0,
        p50: 124.5,
        p95: 146.0,
        p99: 148.0,
        metrics: Vec::new(),
        tags: Vec::new(),
    }
}

fn mk_cmp(diffs: Vec<BenchDiff>) -> CompareReport {
    let mut summary = CompareSummary::default();
    for d in &diffs {
        match d.verdict {
            Verdict::Regression => summary.regressions += 1,
            Verdict::Improvement => summary.improvements += 1,
            Verdict::Neutral => summary.neutrals += 1,
            Verdict::New => summary.new += 1,
            Verdict::Removed => summary.removed += 1,
            Verdict::OutOfScope => summary.out_of_scope += 1,
        }
    }
    CompareReport {
        base: CompareSide {
            r#ref: "abcdef0123".to_string(),
            service: "svc".to_string(),
        },
        head: CompareSide {
            r#ref: "fedcba9876".to_string(),
            service: "svc".to_string(),
        },
        language: Language::Rust,
        threshold_pct: 0.05,
        alpha: 0.05,
        noisy_cv_threshold: 0.25,
        diffs,
        summary,
        affected_scope: None,
    }
}

#[test]
fn starts_with_sticky_marker() {
    let cmp = mk_cmp(vec![]);
    let md = render_markdown(&cmp);
    let first_line = md.lines().next().unwrap();
    assert_eq!(first_line, STICKY_MARKER);
}

#[test]
fn no_diffs_renders_clean_headline() {
    let cmp = mk_cmp(vec![]);
    let md = render_markdown(&cmp);
    assert!(md.contains("Performance · no significant changes"));
    assert!(md.contains("Methodology"));
}

#[test]
fn regression_count_in_headline() {
    let diff = BenchDiff {
        name: "a".into(),
        base: Some(run("a")),
        head: Some(run("a")),
        delta_pct: Some(0.30),
        mean_delta_pct: Some(0.30),
        p_value: Some(1e-6),
        p_value_welch: Some(1e-6),
        max_cv: Some(0.1),
        verdict: Verdict::Regression,
        neutral_reason: None,
    };
    let cmp = mk_cmp(vec![diff]);
    let md = render_markdown(&cmp);
    assert!(md.contains("Performance · 1 regression"));
    assert!(md.contains("Significant changes"));
    assert!(md.contains("🔴 Regression"));
    assert!(md.contains("`a`"));
}

#[test]
fn out_of_scope_renders_under_dedicated_section() {
    let diff = BenchDiff {
        name: "skipped-bench".into(),
        base: Some(run("skipped-bench")),
        head: None,
        delta_pct: None,
        mean_delta_pct: None,
        p_value: None,
        p_value_welch: None,
        max_cv: None,
        verdict: Verdict::OutOfScope,
        neutral_reason: None,
    };
    let cmp = mk_cmp(vec![diff]);
    let md = render_markdown(&cmp);
    assert!(md.contains("Out of scope"));
    assert!(md.contains("counted as regressions"));
    assert!(md.contains("skipped-bench"));
}

#[test]
fn noisy_neutral_is_called_out() {
    let diff = BenchDiff {
        name: "a".into(),
        base: Some(run("a")),
        head: Some(run("a")),
        delta_pct: Some(0.04),
        mean_delta_pct: Some(0.04),
        p_value: Some(0.4),
        p_value_welch: Some(0.4),
        max_cv: Some(0.5),
        verdict: Verdict::Neutral,
        neutral_reason: Some(NeutralReason::TooNoisy),
    };
    let cmp = mk_cmp(vec![diff]);
    let md = render_markdown(&cmp);
    assert!(
        md.contains("noise-gated"),
        "missing 'noise-gated' callout: {md}"
    );
    assert!(md.contains("🟡 Noisy"));
}

#[test]
fn format_ns_picks_units() {
    assert_eq!(format_ns(500.0), "500ns");
    assert_eq!(format_ns(1_500.0), "1.50µs");
    assert_eq!(format_ns(2_500_000.0), "2.50ms");
    assert_eq!(format_ns(3_000_000_000.0), "3.00s");
}

#[test]
fn language_label_appears_in_header() {
    let cmp = mk_cmp(vec![]);
    let md = render_markdown(&cmp);
    assert!(md.contains("(Rust)"), "language label missing: {md}");
}

#[test]
fn improvement_only_run_gets_its_own_headline() {
    let diff = BenchDiff {
        name: "fast".into(),
        base: Some(run("fast")),
        head: Some(run("fast")),
        delta_pct: Some(-0.30),
        mean_delta_pct: Some(-0.30),
        p_value: Some(1e-6),
        p_value_welch: Some(1e-6),
        max_cv: Some(0.1),
        verdict: Verdict::Improvement,
        neutral_reason: None,
    };
    let cmp = mk_cmp(vec![diff]);
    let md = render_markdown(&cmp);
    assert!(md.contains("1 improvement"));
    assert!(md.contains("🟢 Improvement"));
    // The methodology block legitimately uses the word; make sure we just
    // don't claim a regression in the headline or table.
    assert!(!md.contains("🔴 Regression"));
    assert!(!md.contains("Performance · 1 regression"));
}

#[test]
fn pipe_character_in_bench_name_is_escaped() {
    // The name appears inside a markdown table cell; an un-escaped `|` would
    // shred the column layout for every downstream renderer.
    let mut b = run("legacy");
    b.name = "parser | edge case".into();
    let diff = BenchDiff {
        name: b.name.clone(),
        base: Some(b.clone()),
        head: Some(b),
        delta_pct: Some(0.30),
        mean_delta_pct: Some(0.30),
        p_value: Some(1e-6),
        p_value_welch: Some(1e-6),
        max_cv: Some(0.1),
        verdict: Verdict::Regression,
        neutral_reason: None,
    };
    let cmp = mk_cmp(vec![diff]);
    let md = render_markdown(&cmp);
    assert!(md.contains("parser \\| edge case"));
    // And the raw unescaped form must not leak inside the table row.
    let table_line = md
        .lines()
        .find(|l| l.contains("Regression"))
        .expect("regression row");
    assert!(
        !table_line.contains("parser | edge case"),
        "row contains an un-escaped pipe: {table_line}"
    );
}

#[test]
fn affected_scope_header_is_rendered() {
    let mut cmp = mk_cmp(vec![]);
    cmp.affected_scope = Some(AffectedScope {
        base: "origin/master".into(),
        changed_files: vec!["src/a.ts".into(), "src/b.ts".into()],
        bench_files: vec!["a.bench.ts".into()],
        skipped_bench_files: vec!["b.bench.ts".into(), "c.bench.ts".into()],
    });
    let md = render_markdown(&cmp);
    assert!(md.contains("Affected-scope run vs `origin/master`"));
    assert!(md.contains("ran 1 of 3 bench file(s)"));
    assert!(md.contains("2 file(s) changed"));
    assert!(md.contains("2 bench file(s) skipped"));
}

#[test]
fn inventory_section_renders_only_new_when_no_removed() {
    let diff = BenchDiff {
        name: "fresh".into(),
        base: None,
        head: Some(run("fresh")),
        delta_pct: None,
        mean_delta_pct: None,
        p_value: None,
        p_value_welch: None,
        max_cv: None,
        verdict: Verdict::New,
        neutral_reason: None,
    };
    let cmp = mk_cmp(vec![diff]);
    let md = render_markdown(&cmp);
    assert!(md.contains("**New (1):**"));
    assert!(
        !md.contains("**Removed"),
        "should not render an empty Removed section"
    );
}

#[test]
fn inventory_section_renders_only_removed_when_no_new() {
    let diff = BenchDiff {
        name: "ghost".into(),
        base: Some(run("ghost")),
        head: None,
        delta_pct: None,
        mean_delta_pct: None,
        p_value: None,
        p_value_welch: None,
        max_cv: None,
        verdict: Verdict::Removed,
        neutral_reason: None,
    };
    let cmp = mk_cmp(vec![diff]);
    let md = render_markdown(&cmp);
    assert!(md.contains("**Removed (1):**"));
    assert!(
        !md.contains("**New"),
        "should not render an empty New section"
    );
}

#[test]
fn very_long_bench_name_does_not_break_table() {
    // Defensive: long names shouldn't truncate or wrap into a malformed row.
    let long = "a-really-very-extremely-quite-rather-long-bench-name-".repeat(8);
    let mut b = run("x");
    b.name = long.clone();
    let diff = BenchDiff {
        name: long.clone(),
        base: Some(b.clone()),
        head: Some(b),
        delta_pct: Some(0.30),
        mean_delta_pct: Some(0.30),
        p_value: Some(1e-6),
        p_value_welch: Some(1e-6),
        max_cv: Some(0.1),
        verdict: Verdict::Regression,
        neutral_reason: None,
    };
    let cmp = mk_cmp(vec![diff]);
    let md = render_markdown(&cmp);
    let row = md
        .lines()
        .find(|l| l.contains("Regression"))
        .expect("regression row");
    // Exactly eight `|` separators for an 8-column table → row has 9 pipes.
    assert_eq!(row.matches('|').count(), 9, "row: {row}");
    assert!(row.contains(&long));
}

#[test]
fn noisy_callout_uses_plural_form_when_multiple() {
    let mk_neutral = |name: &str| BenchDiff {
        name: name.into(),
        base: Some(run(name)),
        head: Some(run(name)),
        delta_pct: Some(0.04),
        mean_delta_pct: Some(0.04),
        p_value: Some(0.4),
        p_value_welch: Some(0.4),
        max_cv: Some(0.5),
        verdict: Verdict::Neutral,
        neutral_reason: Some(NeutralReason::TooNoisy),
    };
    let cmp = mk_cmp(vec![mk_neutral("a"), mk_neutral("b")]);
    let md = render_markdown(&cmp);
    assert!(md.contains("2 benches had CV"), "missing plural form: {md}");
}

#[test]
fn format_ns_handles_non_finite() {
    assert_eq!(format_ns(f64::NAN), "—");
    assert_eq!(format_ns(f64::INFINITY), "—");
}
