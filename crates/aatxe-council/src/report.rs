//! Render a [`CouncilReport`] as a sticky Markdown PR comment.
//!
//! The marker [`STICKY_MARKER`] is intentionally distinct from
//! `aatxe-core::report::STICKY_MARKER` (`<!-- aatxe:report -->`) so a PR
//! can carry both the perf-regression comment and the council comment as
//! two separate sticky bodies without colliding.

use crate::types::{CouncilReport, FindingCategory, JudgeVerdict, JudgedFinding, Severity};
use std::collections::BTreeMap;

pub const STICKY_MARKER: &str = "<!-- aatxe:council -->";

/// Render the council report as a sticky Markdown body.
pub fn render_markdown(report: &CouncilReport) -> String {
    let shipped = report.shippable();
    let hidden_by_confidence: usize = report
        .judged
        .iter()
        .filter(|jf| jf.verdict != JudgeVerdict::Drop && jf.confidence < report.confidence_floor)
        .count();
    let dropped_by_judge: usize = report
        .judged
        .iter()
        .filter(|jf| jf.verdict == JudgeVerdict::Drop)
        .count();
    let downgraded: usize = report
        .judged
        .iter()
        .filter(|jf| {
            jf.verdict == JudgeVerdict::Downgrade && jf.confidence >= report.confidence_floor
        })
        .count();

    let mut out = Vec::new();
    out.push(STICKY_MARKER.to_string());
    out.push(format!("## {}", headline(&shipped)));
    out.push(String::new());

    // Inputs / pipeline stats
    out.push(format!(
        "Repo `{}` · PR #{} · model `{}` · {} file(s) reviewed of {} in diff · pipeline ran in {} ms",
        report.repo,
        report.pr,
        report.model,
        report.files_reviewed,
        report.files_total,
        report.total_duration_ms,
    ));
    out.push(format!(
        "Proposers: {} · candidates after dedup: {} · judge dropped {} · downgraded {} · hidden (conf < {}): {}",
        report.proposer_reviews.len(),
        report.synthesized.len(),
        dropped_by_judge,
        downgraded,
        format_pct(report.confidence_floor),
        hidden_by_confidence,
    ));
    let total_tokens = report.total_prompt_tokens + report.total_completion_tokens;
    if total_tokens > 0 {
        out.push(format!(
            "Tokens: {} prompt + {} completion = {} total",
            report.total_prompt_tokens, report.total_completion_tokens, total_tokens
        ));
    }
    // Surface partial-failure state at the top so reviewers don't miss it.
    let failed_agents: Vec<&str> = report
        .proposer_reviews
        .iter()
        .filter(|r| r.error.is_some())
        .map(|r| r.agent.as_str())
        .collect();
    if !failed_agents.is_empty() {
        out.push(format!(
            "> ⚠ Degraded run — proposer(s) failed: {}. Findings from those personas are missing.",
            failed_agents.join(", ")
        ));
    }
    if let Some(err) = &report.judge_error {
        out.push(format!(
            "> ⚠ Judge call failed ({}); every candidate ships unfiltered at default confidence.",
            err
        ));
    }
    out.push(String::new());

    if shipped.is_empty() {
        out.push("> Council found no actionable findings on this PR. ✅".to_string());
    } else {
        // Group by category for skimmability.
        let mut by_cat: BTreeMap<&'static str, Vec<&JudgedFinding>> = BTreeMap::new();
        for jf in &shipped {
            by_cat
                .entry(jf.finding.category.label())
                .or_default()
                .push(*jf);
        }
        for (cat, items) in by_cat {
            out.push(format!("### {} ({})", capitalize(cat), items.len()));
            out.push(String::new());
            for jf in items {
                out.push(format_finding_block(jf));
                out.push(String::new());
            }
        }
    }

    // Council telemetry — collapsed so the PR view stays clean.
    out.push("<details><summary>Council telemetry</summary>".to_string());
    out.push(String::new());
    out.push("| Agent | Findings raised | Duration (ms) | Tokens (p/c) | Error |".to_string());
    out.push("| --- | ---: | ---: | ---: | --- |".to_string());
    for ar in &report.proposer_reviews {
        let tokens = match (ar.prompt_tokens, ar.completion_tokens) {
            (Some(p), Some(c)) => format!("{p}/{c}"),
            (Some(p), None) => format!("{p}/—"),
            (None, Some(c)) => format!("—/{c}"),
            (None, None) => "—".into(),
        };
        let err = ar.error.as_deref().map(escape_md).unwrap_or_default();
        out.push(format!(
            "| `{}` | {} | {} | {} | {} |",
            ar.agent,
            ar.findings.len(),
            ar.duration_ms
                .map(|d| d.to_string())
                .unwrap_or_else(|| "—".into()),
            tokens,
            if err.is_empty() {
                "—".to_string()
            } else {
                err
            },
        ));
    }
    out.push(String::new());
    out.push("</details>".to_string());

    // Methodology — same structural pattern as aatxe-core::report.
    out.push(String::new());
    out.push("<details><summary>Methodology</summary>".to_string());
    out.push(String::new());
    out.push(
        "Four specialist proposers (correctness, security, performance, \
         maintainability) review the diff in parallel with persona-specific \
         system prompts. A deterministic synthesiser dedupes overlapping \
         findings across personas (token-Jaccard ≥ 0.55 on titles within \
         ±3 lines), promoting severity to the max. A separate judge agent \
         then scores each surviving candidate for confidence in [0,1] and \
         assigns one of `keep|downgrade|drop` — findings dropped or below \
         the confidence floor never appear above. Generated files, \
         lockfiles, and vendored code are filtered before any LLM call."
            .to_string(),
    );
    out.push(String::new());
    out.push("</details>".to_string());

    out.join("\n")
}

fn headline(shipped: &[&JudgedFinding]) -> String {
    if shipped.is_empty() {
        return "Council review · no findings".into();
    }
    let critical = shipped
        .iter()
        .filter(|f| f.finding.severity == Severity::Critical)
        .count();
    let major = shipped
        .iter()
        .filter(|f| f.finding.severity == Severity::Major)
        .count();
    let minor = shipped
        .iter()
        .filter(|f| f.finding.severity == Severity::Minor)
        .count();
    let nit = shipped
        .iter()
        .filter(|f| f.finding.severity == Severity::Nit)
        .count();

    if critical > 0 {
        return format!(
            "Council review · {} critical · {} major · {} minor · {} nit",
            critical, major, minor, nit
        );
    }
    if major > 0 {
        return format!(
            "Council review · {} major · {} minor · {} nit",
            major, minor, nit
        );
    }
    if minor > 0 {
        return format!("Council review · {} minor · {} nit", minor, nit);
    }
    format!("Council review · {} nit", nit)
}

fn format_finding_block(jf: &JudgedFinding) -> String {
    let mut s = String::new();
    let loc = if jf.finding.file.is_empty() {
        "(PR-wide)".to_string()
    } else if let Some(line) = jf.finding.line {
        format!("`{}:{}`", jf.finding.file, line)
    } else {
        format!("`{}`", jf.finding.file)
    };
    let badge = jf.finding.severity.badge();
    let extra = if jf.verdict == JudgeVerdict::Downgrade {
        " · _downgraded by judge_"
    } else {
        ""
    };
    s.push_str(&format!(
        "**{badge}** · {loc} · confidence {}{extra}",
        format_pct(jf.confidence),
    ));
    s.push('\n');
    s.push_str(&format!("- **{}**\n", escape_md(&jf.finding.title)));
    s.push_str(&format!("- {}\n", escape_md(&jf.finding.rationale)));
    if let Some(sug) = &jf.finding.suggestion {
        s.push_str(&format!("- _Suggestion:_ {}\n", escape_md(sug)));
    }
    if let Some(by) = &jf.finding.raised_by {
        s.push_str(&format!("- _Raised by:_ `{}`\n", by));
    }
    if let Some(note) = &jf.judge_note {
        s.push_str(&format!("- _Judge note:_ {}\n", escape_md(note)));
    }
    s
}

fn format_pct(f: f64) -> String {
    format!("{:.0}%", (f * 100.0).round())
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_ascii_uppercase().to_string() + chars.as_str(),
        None => String::new(),
    }
}

fn escape_md(s: &str) -> String {
    // We only need light escaping — the body is inside list bullets, so
    // pipes and backticks are the real hazards. Newlines are collapsed
    // to keep one finding per visual paragraph.
    s.replace('|', "\\|").replace('\r', "").replace('\n', " ")
}

// Suppress the "unused" warning when no category-empty test exercises
// `Judge`. The variant is meaningful in `FindingCategory` and exercised
// elsewhere.
#[allow(dead_code)]
const _: FindingCategory = FindingCategory::Judge;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{AgentReview, Finding, FindingCategory, JudgeVerdict, Severity};

    fn mk_judged(
        sev: Severity,
        cat: FindingCategory,
        title: &str,
        file: &str,
        line: Option<u32>,
        verdict: JudgeVerdict,
        confidence: f64,
    ) -> JudgedFinding {
        JudgedFinding {
            finding: Finding {
                file: file.into(),
                line,
                severity: sev,
                category: cat,
                title: title.into(),
                rationale: format!("rationale for {title}"),
                suggestion: None,
                raised_by: Some("correctness+security".into()),
            },
            verdict,
            confidence,
            judge_note: Some("looks real".into()),
        }
    }

    #[test]
    fn renders_sticky_marker_first() {
        let r = CouncilReport {
            model: "stub".into(),
            repo: "x/y".into(),
            pr: 7,
            files_total: 1,
            files_reviewed: 1,
            proposer_reviews: vec![],
            synthesized: vec![],
            judged: vec![],
            confidence_floor: 0.55,
            total_duration_ms: 100,
            total_prompt_tokens: 0,
            total_completion_tokens: 0,
            judge_error: None,
        };
        let md = render_markdown(&r);
        assert!(md.starts_with(STICKY_MARKER));
    }

    #[test]
    fn renders_no_findings_when_empty() {
        let r = CouncilReport {
            model: "stub".into(),
            repo: "x/y".into(),
            pr: 7,
            files_total: 1,
            files_reviewed: 1,
            proposer_reviews: vec![AgentReview {
                agent: "correctness".into(),
                category: FindingCategory::Correctness,
                findings: vec![],
                duration_ms: Some(120),
                error: None,
                prompt_tokens: None,
                completion_tokens: None,
            }],
            synthesized: vec![],
            judged: vec![],
            confidence_floor: 0.55,
            total_duration_ms: 100,
            total_prompt_tokens: 0,
            total_completion_tokens: 0,
            judge_error: None,
        };
        let md = render_markdown(&r);
        assert!(md.contains("no actionable findings"));
        assert!(md.contains("Council review · no findings"));
        assert!(md.contains("Methodology"));
    }

    #[test]
    fn renders_critical_with_badge_and_headline() {
        let r = CouncilReport {
            model: "stub".into(),
            repo: "x/y".into(),
            pr: 7,
            files_total: 1,
            files_reviewed: 1,
            proposer_reviews: vec![],
            synthesized: vec![],
            judged: vec![mk_judged(
                Severity::Critical,
                FindingCategory::Security,
                "SSRF in fetch helper",
                "src/fetch.rs",
                Some(42),
                JudgeVerdict::Keep,
                0.95,
            )],
            confidence_floor: 0.55,
            total_duration_ms: 100,
            total_prompt_tokens: 0,
            total_completion_tokens: 0,
            judge_error: None,
        };
        let md = render_markdown(&r);
        assert!(md.contains("🔴 critical"));
        assert!(md.contains("src/fetch.rs:42"));
        assert!(md.contains("1 critical"));
        assert!(md.contains("Security"));
    }

    #[test]
    fn hides_low_confidence_and_drops_in_summary_counts() {
        let r = CouncilReport {
            model: "stub".into(),
            repo: "x/y".into(),
            pr: 7,
            files_total: 1,
            files_reviewed: 1,
            proposer_reviews: vec![],
            synthesized: vec![],
            judged: vec![
                mk_judged(
                    Severity::Major,
                    FindingCategory::Correctness,
                    "kept",
                    "a.rs",
                    None,
                    JudgeVerdict::Keep,
                    0.9,
                ),
                mk_judged(
                    Severity::Major,
                    FindingCategory::Correctness,
                    "hidden by confidence",
                    "b.rs",
                    None,
                    JudgeVerdict::Keep,
                    0.1,
                ),
                mk_judged(
                    Severity::Major,
                    FindingCategory::Correctness,
                    "dropped",
                    "c.rs",
                    None,
                    JudgeVerdict::Drop,
                    1.0,
                ),
            ],
            confidence_floor: 0.55,
            total_duration_ms: 100,
            total_prompt_tokens: 0,
            total_completion_tokens: 0,
            judge_error: None,
        };
        let md = render_markdown(&r);
        assert!(md.contains("**kept**"));
        assert!(!md.contains("**hidden by confidence**"));
        assert!(!md.contains("**dropped**"));
        assert!(md.contains("judge dropped 1"));
        assert!(md.contains("hidden (conf < 55%): 1"));
    }

    #[test]
    fn renders_downgraded_marker() {
        let r = CouncilReport {
            model: "stub".into(),
            repo: "x/y".into(),
            pr: 7,
            files_total: 1,
            files_reviewed: 1,
            proposer_reviews: vec![],
            synthesized: vec![],
            judged: vec![mk_judged(
                Severity::Minor,
                FindingCategory::Maintainability,
                "naming nit",
                "x.rs",
                Some(1),
                JudgeVerdict::Downgrade,
                0.7,
            )],
            confidence_floor: 0.55,
            total_duration_ms: 100,
            total_prompt_tokens: 0,
            total_completion_tokens: 0,
            judge_error: None,
        };
        let md = render_markdown(&r);
        assert!(md.contains("_downgraded by judge_"));
    }

    #[test]
    fn token_totals_render_when_present() {
        let r = CouncilReport {
            model: "stub".into(),
            repo: "x/y".into(),
            pr: 7,
            files_total: 1,
            files_reviewed: 1,
            proposer_reviews: vec![AgentReview {
                agent: "correctness".into(),
                category: FindingCategory::Correctness,
                findings: vec![],
                duration_ms: Some(50),
                error: None,
                prompt_tokens: Some(800),
                completion_tokens: Some(200),
            }],
            synthesized: vec![],
            judged: vec![],
            confidence_floor: 0.55,
            total_duration_ms: 50,
            total_prompt_tokens: 800,
            total_completion_tokens: 200,
            judge_error: None,
        };
        let md = render_markdown(&r);
        assert!(md.contains("Tokens: 800 prompt + 200 completion = 1000 total"));
        assert!(
            md.contains("800/200"),
            "per-agent tokens should be in the telemetry table"
        );
    }

    #[test]
    fn token_totals_omitted_when_zero() {
        let r = CouncilReport {
            model: "stub".into(),
            repo: "x/y".into(),
            pr: 7,
            files_total: 1,
            files_reviewed: 1,
            proposer_reviews: vec![],
            synthesized: vec![],
            judged: vec![],
            confidence_floor: 0.55,
            total_duration_ms: 0,
            total_prompt_tokens: 0,
            total_completion_tokens: 0,
            judge_error: None,
        };
        let md = render_markdown(&r);
        assert!(!md.contains("Tokens:"));
    }

    #[test]
    fn proposer_failure_surfaces_warning_at_top() {
        let r = CouncilReport {
            model: "stub".into(),
            repo: "x/y".into(),
            pr: 7,
            files_total: 1,
            files_reviewed: 1,
            proposer_reviews: vec![
                AgentReview {
                    agent: "correctness".into(),
                    category: FindingCategory::Correctness,
                    findings: vec![],
                    duration_ms: Some(50),
                    error: None,
                    prompt_tokens: None,
                    completion_tokens: None,
                },
                AgentReview {
                    agent: "security".into(),
                    category: FindingCategory::Security,
                    findings: vec![],
                    duration_ms: Some(120),
                    error: Some("rate limited after 4 attempts".into()),
                    prompt_tokens: None,
                    completion_tokens: None,
                },
            ],
            synthesized: vec![],
            judged: vec![],
            confidence_floor: 0.55,
            total_duration_ms: 200,
            total_prompt_tokens: 0,
            total_completion_tokens: 0,
            judge_error: None,
        };
        let md = render_markdown(&r);
        assert!(md.contains("⚠ Degraded run"));
        assert!(md.contains("security"));
        assert!(md.contains("rate limited after 4 attempts"));
    }

    #[test]
    fn judge_failure_surfaces_warning_at_top() {
        let r = CouncilReport {
            model: "stub".into(),
            repo: "x/y".into(),
            pr: 7,
            files_total: 1,
            files_reviewed: 1,
            proposer_reviews: vec![],
            synthesized: vec![],
            judged: vec![],
            confidence_floor: 0.55,
            total_duration_ms: 200,
            total_prompt_tokens: 0,
            total_completion_tokens: 0,
            judge_error: Some("judge timed out".into()),
        };
        let md = render_markdown(&r);
        assert!(md.contains("⚠ Judge call failed"));
        assert!(md.contains("judge timed out"));
    }

    #[test]
    fn render_escapes_pipes_so_table_does_not_break() {
        let mut f = mk_judged(
            Severity::Major,
            FindingCategory::Correctness,
            "title | with | pipes",
            "a.rs",
            None,
            JudgeVerdict::Keep,
            0.9,
        );
        f.finding.rationale = "rationale | with | pipes".into();
        let r = CouncilReport {
            model: "stub".into(),
            repo: "x/y".into(),
            pr: 7,
            files_total: 1,
            files_reviewed: 1,
            proposer_reviews: vec![],
            synthesized: vec![],
            judged: vec![f],
            confidence_floor: 0.55,
            total_duration_ms: 100,
            total_prompt_tokens: 0,
            total_completion_tokens: 0,
            judge_error: None,
        };
        let md = render_markdown(&r);
        assert!(md.contains("title \\| with \\| pipes"));
    }
}
