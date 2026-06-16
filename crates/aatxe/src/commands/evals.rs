//! `aatxe evals` — orchestrates the eval harness end to end.
//!
//! 1. Run the stats eval (deterministic synthetic A/B pairs).
//! 2. Run the council eval over every case in the corpus directory.
//!    LLM client is the deterministic [`crate::llm::stub_client::StubKimi`] by
//!    default; `--council-real-llm` swaps in the [`crate::llm::pi_proxy::PiAgentClient`]
//!    (requires `KIMI_API_KEY` so the spawned `pi` child can reach
//!    Moonshot).
//! 3. Serialise the result to JSON.
//! 4. Optionally render a markdown summary.
//! 5. Optionally diff against a baseline; exit 2 on regression past
//!    tolerance.

use crate::cli::{BackendArg, EvalsArgs};
use crate::commands::Outcome;
use crate::llm::claude_code::{ClaudeCodeClient, ClaudeCodeConfig};
use crate::llm::gemini_http::{self, GeminiClient, GeminiConfig};
use crate::llm::pi_proxy::{PiAgentClient, PiConfig};
use crate::llm::stub_client::StubKimi;
use aatxe_council::llm::LlmClient;
use aatxe_council::pipeline::{run_council_with_files, CouncilOptions};
use aatxe_evals::council::{
    recalibrate_summary_from_records, score_case, score_council, CouncilCase, CouncilCaseResult,
    CouncilEvalOptions, CouncilEvalSummary,
};
use aatxe_evals::report::{
    regressions_against_baseline, EvalReport, EvalTolerances, EVAL_SCHEMA_VERSION,
};
use aatxe_evals::stats::{default_scenarios, run_stats_evals, StatsEvalSummary};
use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

pub fn execute(args: EvalsArgs) -> Result<Outcome> {
    if args.recalibrate_from.is_some() {
        return recalibrate(args);
    }

    let started_at = current_iso8601();

    let stats_summary: Option<StatsEvalSummary> = if args.stats {
        eprintln!(
            "aatxe evals: running stats scenarios ({} default cases)…",
            default_scenarios().len()
        );
        Some(run_stats_evals(&default_scenarios()))
    } else {
        None
    };

    let (council_summary, council_used_real_llm) = if args.council {
        let corpus = args
            .corpus
            .clone()
            .unwrap_or_else(|| PathBuf::from("evals/council/cases"));
        if !corpus.is_dir() {
            return Err(anyhow!(
                "council corpus directory does not exist: {}",
                corpus.display()
            ));
        }
        let cases = load_corpus(&corpus)?;
        eprintln!(
            "aatxe evals: running council on {} case(s) from {}",
            cases.len(),
            corpus.display()
        );
        let client: Box<dyn LlmClient> = if args.council_real_llm {
            match args.backend {
                BackendArg::PiProxy => {
                    if std::env::var("KIMI_API_KEY").is_err() {
                        return Err(anyhow!(
                            "--council-real-llm --backend pi-proxy requires KIMI_API_KEY \
                             in the environment (the pi child uses it to reach Moonshot; \
                             drop the flag for a stub-LLM smoke test, or pass \
                             --backend claude-code to use your Claude Code subscription)"
                        ));
                    }
                    Box::new(PiAgentClient::new(PiConfig::from_env()))
                }
                BackendArg::ClaudeCode => {
                    Box::new(ClaudeCodeClient::new(ClaudeCodeConfig::from_env()))
                }
                BackendArg::Gemini => {
                    let cfg = GeminiConfig::from_env().map_err(|e| {
                        anyhow!(
                            "--council-real-llm --backend gemini: {e} \
                             (drop the flag for a stub-LLM smoke test)"
                        )
                    })?;
                    Box::new(GeminiClient::new(cfg))
                }
            }
        } else {
            Box::new(StubKimi)
        };
        let mut per_case: Vec<CouncilCaseResult> = Vec::with_capacity(cases.len());
        for (case, diff_path, files_dir_abs) in cases {
            let diff_text = fs::read_to_string(&diff_path)
                .with_context(|| format!("reading diff {}", diff_path.display()))?;
            let files_map: HashMap<String, String> = match &files_dir_abs {
                Some(dir) => load_files_dir(dir)
                    .with_context(|| format!("loading files dir {}", dir.display()))?,
                None => HashMap::new(),
            };
            let ctx_count = files_map.len();
            // Compute AST scope from the post-PR file contents. The diff's
            // changed paths drive which symbols are the focus; the full
            // `files_map` provides cross-file caller attribution. Falls
            // back to an empty string when no parseable lang is in scope.
            let changed_paths: Vec<String> = aatxe_council::diff::parse_unified_diff(&diff_text)
                .into_iter()
                .map(|f| f.path)
                .collect();
            let ast_scope =
                crate::ast::ast_scope::build_scope_for_review(&files_map, &changed_paths);
            eprintln!(
                "  • case {} → {} ({} file fixtures, AST scope: {} bytes)",
                case.name,
                diff_path.display(),
                ctx_count,
                ast_scope.len()
            );
            let opts = CouncilOptions {
                model: if args.council_real_llm {
                    match args.backend {
                        BackendArg::PiProxy => {
                            let pi_cfg = PiConfig::from_env();
                            format!("pi+{}", pi_cfg.model)
                        }
                        BackendArg::ClaudeCode => {
                            let cc = ClaudeCodeConfig::from_env();
                            cc.model
                                .clone()
                                .map(|m| format!("claude-code+{m}"))
                                .unwrap_or_else(|| "claude-code".to_string())
                        }
                        BackendArg::Gemini => {
                            format!("gemini+{}", gemini_http::model_from_env())
                        }
                    }
                } else {
                    "stub".into()
                },
                confidence_floor: args.confidence_floor,
                ast_scope,
                ..CouncilOptions::default()
            };
            let report = run_council_with_files(&diff_text, &files_map, &opts, client.as_ref())
                .with_context(|| format!("council pipeline failed on case {}", case.name))?;
            let scored = score_case(
                &case,
                &report,
                CouncilEvalOptions {
                    confidence_floor: args.confidence_floor,
                },
            );
            per_case.push(scored);
        }
        (Some(score_council(per_case)), args.council_real_llm)
    } else {
        (None, false)
    };

    let report = EvalReport {
        schema_version: EVAL_SCHEMA_VERSION,
        started_at,
        aatxe_version: env!("CARGO_PKG_VERSION").to_string(),
        council: council_summary,
        stats: stats_summary,
        council_used_real_llm,
    };

    let out = args
        .out
        .clone()
        .unwrap_or_else(|| PathBuf::from("./aatxe-evals.json"));
    let json = serde_json::to_string_pretty(&report).context("serialising EvalReport")?;
    fs::write(&out, &json).with_context(|| format!("writing {}", out.display()))?;
    eprintln!("aatxe evals: wrote {}", out.display());

    if let Some(md_out) = args.markdown.as_ref() {
        let body = render_markdown(&report);
        fs::write(md_out, body)
            .with_context(|| format!("writing markdown to {}", md_out.display()))?;
        eprintln!("aatxe evals: wrote {}", md_out.display());
    }

    print_summary_to_stderr(&report);

    if let Some(baseline_path) = args.baseline.as_ref() {
        let baseline_raw = fs::read_to_string(baseline_path)
            .with_context(|| format!("reading baseline from {}", baseline_path.display()))?;
        let baseline: EvalReport = serde_json::from_str(&baseline_raw)
            .with_context(|| format!("parsing baseline JSON from {}", baseline_path.display()))?;
        // Auto-select tolerances by baseline type: real-LLM baselines
        // get the looser real-LLM band, stub baselines get strict deltas.
        // M2.5 — `EvalTolerances::auto_for` keeps `--baseline real-claude.json`
        // gates from false-triggering on every non-deterministic re-run.
        let tolerances = EvalTolerances::auto_for(&baseline);
        if baseline.council_used_real_llm {
            eprintln!(
                "aatxe evals: baseline is real-LLM → using looser tolerances (FP/case rise ≤ {:.2}, F1 drop ≤ {:.2})",
                tolerances.avg_fp_per_case_rise, tolerances.critical_f1_drop
            );
        }
        let regs = regressions_against_baseline(&baseline, &report, tolerances);
        if !regs.is_empty() {
            eprintln!("aatxe evals: {} regression(s) vs baseline:", regs.len());
            for r in &regs {
                eprintln!(
                    "  ✗ {} — baseline {:.4} → current {:.4} (worse by {:.4}, tolerance {:.4})",
                    r.metric, r.baseline, r.current, r.delta_worse, r.tolerance
                );
                eprintln!("      {}", r.note);
            }
            if !args.no_fail {
                return Ok(Outcome::Regressions);
            }
        } else {
            eprintln!("aatxe evals: ✓ no regressions vs baseline");
        }
    }
    Ok(Outcome::Ok)
}

/// Offline floor recalibration driver.
///
/// Loads a prior eval JSON, validates that it carries per-finding records
/// (otherwise the user needs to re-run the LLM), then re-derives the
/// council summary at each requested floor. Prints a markdown delta table
/// to stdout, optionally writes it to `--out`. Doesn't trigger the gate —
/// recalibration is a planning tool, not a CI assertion.
fn recalibrate(args: EvalsArgs) -> Result<Outcome> {
    let from = args
        .recalibrate_from
        .as_ref()
        .expect("guarded by execute()");
    if args.recalibrate_floors.is_empty() {
        return Err(anyhow!(
            "--recalibrate-from given but --recalibrate-floors is empty"
        ));
    }

    let raw = fs::read_to_string(from)
        .with_context(|| format!("reading prior eval JSON from {}", from.display()))?;
    let prior: EvalReport = serde_json::from_str(&raw)
        .with_context(|| format!("parsing prior eval JSON from {}", from.display()))?;
    let Some(prior_council) = prior.council.clone() else {
        return Err(anyhow!(
            "prior eval JSON {} has no council summary — nothing to recalibrate",
            from.display()
        ));
    };

    let corpus = args
        .corpus
        .clone()
        .unwrap_or_else(|| PathBuf::from("evals/council/cases"));
    if !corpus.is_dir() {
        return Err(anyhow!(
            "corpus dir {} not found (needed to re-match findings against ground truth)",
            corpus.display()
        ));
    }
    let cases_by_name: HashMap<String, CouncilCase> = load_corpus(&corpus)?
        .into_iter()
        .map(|(c, _, _)| (c.name.clone(), c))
        .collect();

    let mut rows: Vec<(f64, CouncilEvalSummary)> =
        Vec::with_capacity(args.recalibrate_floors.len());
    for &floor in &args.recalibrate_floors {
        if !(0.0..=1.0).contains(&floor) {
            return Err(anyhow!(
                "floor {floor} is out of range — must be in [0.0, 1.0]"
            ));
        }
        let summary = recalibrate_summary_from_records(
            &cases_by_name,
            &prior_council,
            CouncilEvalOptions {
                confidence_floor: floor,
            },
        )
        .map_err(|e| anyhow!("recalibration failed at floor {floor}: {e}"))?;
        rows.push((floor, summary));
    }

    let table = render_recalibration_table(from, &rows);
    println!("{table}");

    if let Some(out_path) = &args.out {
        fs::write(out_path, &table)
            .with_context(|| format!("writing recalibration table to {}", out_path.display()))?;
        eprintln!(
            "aatxe evals: wrote recalibration table to {}",
            out_path.display()
        );
    }

    Ok(Outcome::Ok)
}

/// Render a side-by-side markdown comparison: baseline floor + one column
/// per other floor with delta vs baseline. Numeric columns use the same
/// precision as [`print_summary_to_stderr`].
fn render_recalibration_table(source: &Path, rows: &[(f64, CouncilEvalSummary)]) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "## Confidence-floor recalibration sweep\n\n_source:_ `{}`\n\n",
        source.display()
    ));

    // Header row.
    out.push_str("| metric |");
    for (floor, _) in rows {
        out.push_str(&format!(" floor={:.2} |", floor));
    }
    out.push('\n');
    out.push_str("|---|");
    for _ in rows {
        out.push_str("---:|");
    }
    out.push('\n');

    let baseline = &rows[0].1;
    type MetricRow = (&'static str, fn(&CouncilEvalSummary) -> f64);
    let metric_rows: &[MetricRow] = &[
        ("cases_fully_recalled", |s| s.cases_fully_recalled as f64),
        ("cases_over_cap", |s| s.cases_over_cap as f64),
        ("critical_recall", |s| s.critical_recall),
        ("critical_precision", |s| s.critical_precision),
        ("critical_F1", |s| s.critical_f1),
        ("severity_calibration_MAE", |s| s.severity_calibration_mae),
        ("judge_brier_score", |s| s.judge_brier_score),
        ("FP/case", |s| s.avg_false_positives_per_case),
        ("forbidden_path_findings", |s| {
            s.forbidden_path_findings as f64
        }),
    ];
    for (label, getter) in metric_rows {
        out.push_str(&format!("| {label} |"));
        for (i, (_, s)) in rows.iter().enumerate() {
            let v = getter(s);
            if i == 0 {
                out.push_str(&format!(" {v:.4} |"));
            } else {
                let b = getter(baseline);
                let delta = v - b;
                let sign = if delta > 0.0 { "+" } else { "" };
                out.push_str(&format!(" {v:.4} ({sign}{delta:.4}) |"));
            }
        }
        out.push('\n');
    }
    out
}

#[derive(Debug, Deserialize)]
struct CorpusIndex {
    cases: Vec<String>,
}

fn load_corpus(dir: &Path) -> Result<Vec<(CouncilCase, PathBuf, Option<PathBuf>)>> {
    let index_path = dir.join("_index.json");
    let index: CorpusIndex = if index_path.is_file() {
        let raw = fs::read_to_string(&index_path)
            .with_context(|| format!("reading {}", index_path.display()))?;
        serde_json::from_str(&raw).with_context(|| format!("parsing {}", index_path.display()))?
    } else {
        let mut cases = Vec::new();
        for entry in fs::read_dir(dir).with_context(|| format!("listing {}", dir.display()))? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().to_string();
            if name.ends_with(".json") && !name.starts_with('_') {
                cases.push(name);
            }
        }
        cases.sort();
        CorpusIndex { cases }
    };
    let mut out = Vec::with_capacity(index.cases.len());
    for case_name in index.cases {
        let case_path = dir.join(&case_name);
        let raw = fs::read_to_string(&case_path)
            .with_context(|| format!("reading {}", case_path.display()))?;
        let case: CouncilCase = serde_json::from_str(&raw)
            .with_context(|| format!("parsing case JSON {}", case_path.display()))?;
        let diff_path = dir.join(&case.diff);
        let files_dir_abs = case.files_dir.as_ref().map(|d| dir.join(d));
        // Eagerly fail when a case declares a fixtures dir that's missing —
        // otherwise the case silently degrades to diff-only review and we
        // get a confusing "no context" baseline regression weeks later.
        if let Some(p) = &files_dir_abs {
            if !p.is_dir() {
                return Err(anyhow!(
                    "case {} declares filesDir = {} but {} is not a directory",
                    case.name,
                    case.files_dir.as_deref().unwrap_or(""),
                    p.display()
                ));
            }
        }
        out.push((case, diff_path, files_dir_abs));
    }
    Ok(out)
}

/// Walk `dir` recursively and return a map of repo-relative POSIX path →
/// file contents. Paths are computed by stripping `dir` from each file's
/// absolute path and normalising separators. Symlinks are NOT followed —
/// fixture dirs are check-out artefacts and should be self-contained.
fn load_files_dir(dir: &Path) -> Result<HashMap<String, String>> {
    let mut out = HashMap::new();
    let mut stack: Vec<PathBuf> = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        for entry in
            fs::read_dir(&current).with_context(|| format!("listing {}", current.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            let file_type = entry.file_type()?;
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                stack.push(path);
                continue;
            }
            if !file_type.is_file() {
                continue;
            }
            let rel = path.strip_prefix(dir).with_context(|| {
                format!("path {} not under base {}", path.display(), dir.display())
            })?;
            let rel_str = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            let body = fs::read_to_string(&path)
                .with_context(|| format!("reading fixture {}", path.display()))?;
            out.insert(rel_str, body);
        }
    }
    Ok(out)
}

fn print_summary_to_stderr(r: &EvalReport) {
    eprintln!("aatxe evals summary:");
    if let Some(c) = &r.council {
        eprintln!(
            "  council  · cases={} fully_recalled={} over_cap={} judge_err={} critical_recall={:.3} crit_F1={:.3} sev_MAE={:.3} brier={:.3} fp/case={:.3} forbidden={} avg_latency={}ms ({}p/{}c tok)",
            c.cases_total,
            c.cases_fully_recalled,
            c.cases_over_cap,
            c.cases_with_judge_error,
            c.critical_recall,
            c.critical_f1,
            c.severity_calibration_mae,
            c.judge_brier_score,
            c.avg_false_positives_per_case,
            c.forbidden_path_findings,
            c.avg_latency_ms,
            c.total_prompt_tokens,
            c.total_completion_tokens,
        );
    }
    if let Some(s) = &r.stats {
        eprintln!(
            "  stats    · scenarios={} passed={} pass_rate={:.3} null_FPR={:.3} borderline_TPR={:.3}",
            s.scenarios_total,
            s.scenarios_passed,
            s.pass_rate,
            s.observed_null_fpr,
            s.observed_borderline_tpr,
        );
    }
    eprintln!("  council_used_real_llm={}", r.council_used_real_llm);
}

fn render_markdown(r: &EvalReport) -> String {
    use std::fmt::Write;
    let mut s = String::new();
    let _ = writeln!(s, "<!-- aatxe:evals -->");
    let _ = writeln!(s, "## aatxe evals — {} ({})", r.started_at, r.aatxe_version);
    let _ = writeln!(s);
    let _ = writeln!(
        s,
        "Council LLM: {}",
        if r.council_used_real_llm {
            "**real Kimi**"
        } else {
            "_stub (deterministic)_"
        }
    );
    let _ = writeln!(s);
    if let Some(c) = &r.council {
        let _ = writeln!(s, "### Council quality ({} cases)", c.cases_total);
        let _ = writeln!(s);
        let _ = writeln!(s, "| metric | value |");
        let _ = writeln!(s, "|---|---|");
        let _ = writeln!(
            s,
            "| Cases fully recalled | {}/{} |",
            c.cases_fully_recalled, c.cases_total
        );
        let _ = writeln!(s, "| Cases over `maxFindings` cap | {} |", c.cases_over_cap);
        let _ = writeln!(
            s,
            "| Cases with judge error | {} |",
            c.cases_with_judge_error
        );
        let _ = writeln!(s, "| Critical recall | {:.3} |", c.critical_recall);
        let _ = writeln!(s, "| Critical precision | {:.3} |", c.critical_precision);
        let _ = writeln!(s, "| Critical F1 | {:.3} |", c.critical_f1);
        let _ = writeln!(
            s,
            "| Severity calibration MAE | {:.3} (0=perfect, 3=max) |",
            c.severity_calibration_mae
        );
        let _ = writeln!(
            s,
            "| Judge Brier score | {:.3} (0=perfect, 0.25=chance) |",
            c.judge_brier_score
        );
        let _ = writeln!(
            s,
            "| False positives per case | {:.3} |",
            c.avg_false_positives_per_case
        );
        let _ = writeln!(
            s,
            "| Forbidden-path findings | {} |",
            c.forbidden_path_findings
        );
        let _ = writeln!(s, "| Avg latency | {} ms |", c.avg_latency_ms);
        let _ = writeln!(
            s,
            "| Tokens (prompt/completion) | {} / {} |",
            c.total_prompt_tokens, c.total_completion_tokens
        );
        let _ = writeln!(s);
        let _ = writeln!(s, "<details><summary>Per-case breakdown</summary>");
        let _ = writeln!(s);
        let _ = writeln!(
            s,
            "| case | caught/total | unmatched | forbidden | over_cap | brier |"
        );
        let _ = writeln!(s, "|---|---|---|---|---|---|");
        for c in &c.per_case {
            let brier_avg = if c.brier_n == 0 {
                0.0
            } else {
                c.brier_sum / c.brier_n as f64
            };
            let _ = writeln!(
                s,
                "| `{}` | {}/{} | {} | {} | {} | {:.3} |",
                c.name,
                c.expected_caught,
                c.expected_total,
                c.findings_unmatched,
                c.findings_forbidden,
                if c.max_findings_violated {
                    "yes"
                } else {
                    "—"
                },
                brier_avg,
            );
        }
        let _ = writeln!(s, "</details>");
        let _ = writeln!(s);
    }
    if let Some(stats) = &r.stats {
        let _ = writeln!(
            s,
            "### Stats engine ({} scenarios, {} passed)",
            stats.scenarios_total, stats.scenarios_passed
        );
        let _ = writeln!(s);
        let _ = writeln!(
            s,
            "Observed null FPR: **{:.3}** (target ≤ 0.05). Borderline 6% regression TPR: **{:.3}**.",
            stats.observed_null_fpr, stats.observed_borderline_tpr
        );
        let _ = writeln!(s);
        let _ = writeln!(
            s,
            "| scenario | regression | improvement | neutral (noisy) | mean p | pass |"
        );
        let _ = writeln!(s, "|---|---|---|---|---|---|");
        for p in &stats.per_scenario {
            let _ = writeln!(
                s,
                "| `{}` | {:.3} | {:.3} | {:.3} ({:.3}) | {:.3} | {} |",
                p.name,
                p.regression_rate,
                p.improvement_rate,
                p.neutral_rate,
                p.too_noisy_rate,
                p.mean_p_value,
                if p.passed { "✓" } else { "✗" }
            );
        }
        let _ = writeln!(s);
        let any_failures: Vec<&aatxe_evals::stats::StatsScenarioResult> =
            stats.per_scenario.iter().filter(|p| !p.passed).collect();
        if !any_failures.is_empty() {
            let _ = writeln!(s, "<details><summary>Scenario failures</summary>");
            let _ = writeln!(s);
            for p in any_failures {
                let _ = writeln!(s, "- `{}`:", p.name);
                for f in &p.failures {
                    let _ = writeln!(s, "  - {}", f);
                }
            }
            let _ = writeln!(s, "</details>");
        }
    }
    s
}

fn current_iso8601() -> String {
    // Match the format aatxe core uses elsewhere — strict UTC ISO8601.
    use time::format_description::well_known::Iso8601;
    use time::OffsetDateTime;
    let now = OffsetDateTime::now_utc();
    now.format(&Iso8601::DEFAULT)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use aatxe_evals::council::CouncilEvalSummary;
    use aatxe_evals::stats::default_scenarios;

    #[test]
    fn render_markdown_includes_sticky_marker_and_both_sections() {
        let report = EvalReport {
            schema_version: EVAL_SCHEMA_VERSION,
            started_at: "1970-01-01T00:00:00Z".into(),
            aatxe_version: "test".into(),
            council: Some(CouncilEvalSummary {
                cases_total: 1,
                cases_fully_recalled: 1,
                cases_over_cap: 0,
                cases_with_judge_error: 0,
                recall: Default::default(),
                critical_precision: 1.0,
                critical_recall: 1.0,
                critical_f1: 1.0,
                severity_calibration_mae: 0.0,
                judge_brier_score: 0.0,
                avg_false_positives_per_case: 0.0,
                forbidden_path_findings: 0,
                avg_latency_ms: 1,
                total_prompt_tokens: 1,
                total_completion_tokens: 1,
                per_case: vec![],
            }),
            stats: Some(run_stats_evals(&default_scenarios())),
            council_used_real_llm: false,
        };
        let md = render_markdown(&report);
        assert!(md.contains("<!-- aatxe:evals -->"));
        assert!(md.contains("Council quality"));
        assert!(md.contains("Stats engine"));
        assert!(md.contains("null FPR"));
    }
}
