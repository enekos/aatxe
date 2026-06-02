//! Render a [`CompareReport`] as a sticky Markdown PR comment.
//!
//! The string [`STICKY_MARKER`] is an HTML comment kept on the first line of
//! every rendered body; [`crate::github`] uses it to find-and-update the
//! existing comment on a PR instead of creating a new one each push.

use crate::types::{BenchDiff, CompareReport, CompareSummary, NeutralReason, Verdict};

/// HTML comment used to identify the aatxe comment on a PR for in-place
/// updates. Must appear as the very first line of any rendered comment body.
pub const STICKY_MARKER: &str = "<!-- aatxe:report -->";

/// Render the compare report as a sticky Markdown comment body.
pub fn render_markdown(cmp: &CompareReport) -> String {
    let sig_rows: Vec<&BenchDiff> = cmp
        .diffs
        .iter()
        .filter(|d| matches!(d.verdict, Verdict::Regression | Verdict::Improvement))
        .collect();
    let new_rows: Vec<&BenchDiff> = cmp
        .diffs
        .iter()
        .filter(|d| d.verdict == Verdict::New)
        .collect();
    let removed_rows: Vec<&BenchDiff> = cmp
        .diffs
        .iter()
        .filter(|d| d.verdict == Verdict::Removed)
        .collect();
    let out_of_scope_rows: Vec<&BenchDiff> = cmp
        .diffs
        .iter()
        .filter(|d| d.verdict == Verdict::OutOfScope)
        .collect();
    let neutral_rows: Vec<&BenchDiff> = cmp
        .diffs
        .iter()
        .filter(|d| d.verdict == Verdict::Neutral)
        .collect();
    let noisy_count = neutral_rows
        .iter()
        .filter(|d| d.neutral_reason == Some(NeutralReason::TooNoisy))
        .count();

    let mut lines: Vec<String> = Vec::new();
    lines.push(STICKY_MARKER.to_string());
    lines.push(format!("## {}", headline_for_summary(&cmp.summary)));
    lines.push(String::new());
    lines.push(format!(
        "Service `{}` ({}) · base `{}` → head `{}` · threshold ±{} · α={}",
        cmp.head.service,
        cmp.language.label(),
        short_ref(&cmp.base.r#ref),
        short_ref(&cmp.head.r#ref),
        pct(cmp.threshold_pct),
        cmp.alpha,
    ));

    if let Some(scope) = &cmp.affected_scope {
        let ran = scope.bench_files.len();
        let skipped = scope.skipped_bench_files.len();
        lines.push(String::new());
        lines.push(format!(
            "> Affected-scope run vs `{}`: ran {} of {} bench file(s) ({} file(s) changed). \
             {} bench file(s) skipped as unaffected — see \"Out of scope\" below.",
            scope.base,
            ran,
            ran + skipped,
            scope.changed_files.len(),
            skipped,
        ));
    }

    if noisy_count > 0 {
        lines.push(String::new());
        let bench_word = if noisy_count == 1 { "bench" } else { "benches" };
        lines.push(format!(
            "> ⚠ {} {} had CV > {}; their results were noise-gated.",
            noisy_count,
            bench_word,
            pct(cmp.noisy_cv_threshold),
        ));
    }
    lines.push(String::new());

    if !sig_rows.is_empty() {
        lines.push("### Significant changes".to_string());
        lines.push(String::new());
        lines.push(render_table(&sig_rows));
        lines.push(String::new());
    }

    if !new_rows.is_empty() || !removed_rows.is_empty() {
        lines.push("### Inventory changes".to_string());
        lines.push(String::new());
        if !new_rows.is_empty() {
            lines.push(format!("**New ({}):**", new_rows.len()));
            for d in &new_rows {
                let med = d
                    .head
                    .as_ref()
                    .map(|h| format_ns(h.median))
                    .unwrap_or_else(|| "—".into());
                lines.push(format!("- `{}` · median {}", escape_md(&d.name), med));
            }
            lines.push(String::new());
        }
        if !removed_rows.is_empty() {
            lines.push(format!("**Removed ({}):**", removed_rows.len()));
            for d in &removed_rows {
                lines.push(format!("- `{}`", escape_md(&d.name)));
            }
            lines.push(String::new());
        }
    }

    if !neutral_rows.is_empty() {
        lines.push(format!(
            "<details><summary>Neutral ({})</summary>",
            neutral_rows.len()
        ));
        lines.push(String::new());
        lines.push(render_table(&neutral_rows));
        lines.push(String::new());
        lines.push("</details>".to_string());
        lines.push(String::new());
    }

    if !out_of_scope_rows.is_empty() {
        lines.push(format!(
            "<details><summary>Out of scope ({}) — not run on this PR</summary>",
            out_of_scope_rows.len()
        ));
        lines.push(String::new());
        lines.push(
            "These benches exist in base but weren't re-run on head because their source files \
             weren't touched by this PR. They are **not** counted as regressions."
                .to_string(),
        );
        lines.push(String::new());
        for d in &out_of_scope_rows {
            let med = d
                .base
                .as_ref()
                .map(|b| format_ns(b.median))
                .unwrap_or_else(|| "—".into());
            lines.push(format!("- `{}` · base median {}", escape_md(&d.name), med));
        }
        lines.push(String::new());
        lines.push("</details>".to_string());
        lines.push(String::new());
    }

    lines.push("<details><summary>Methodology</summary>".to_string());
    lines.push(String::new());
    lines.push(format!(
        "Both refs run on the same CI machine with identical toolchain, back-to-back. \
         Each bench: warmup samples discarded, then adaptive sampling (auto-batched for sub-µs \
         ops) until target CV {} or time budget. Effect size: relative median delta. \
         Significance: Mann–Whitney U two-tailed p-value (non-parametric, no normality \
         assumption). Verdict: regression when |Δmedian| ≥ {} AND p < {} AND not noise-gated. \
         Noise gate: max(CV_base, CV_head) > {} AND |Δmedian| < 2 × max(CV).",
        pct(0.02),
        pct(cmp.threshold_pct),
        cmp.alpha,
        pct(cmp.noisy_cv_threshold),
    ));
    lines.push(String::new());
    lines.push("</details>".to_string());

    lines.join("\n")
}

fn render_table(diffs: &[&BenchDiff]) -> String {
    let headers = [
        "Bench",
        "Base (median)",
        "Head (median)",
        "Δ",
        "p95 Δ",
        "CV (b→h)",
        "p",
        "Verdict",
    ];
    let align = [
        "---", "---:", "---:", "---:", "---:", "---:", "---:", ":---:",
    ];
    let mut out: Vec<String> = Vec::with_capacity(diffs.len() + 2);
    out.push(format!("| {} |", headers.join(" | ")));
    out.push(format!("| {} |", align.join(" | ")));
    for d in diffs {
        let base_med = d
            .base
            .as_ref()
            .map(|b| format_ns(b.median))
            .unwrap_or_else(|| "—".into());
        let head_med = d
            .head
            .as_ref()
            .map(|h| format_ns(h.median))
            .unwrap_or_else(|| "—".into());
        let delta = d
            .delta_pct
            .map(fmt_signed_pct)
            .unwrap_or_else(|| "—".into());
        let p95_delta = match (d.base.as_ref(), d.head.as_ref()) {
            (Some(b), Some(h)) if b.p95 > 0.0 => fmt_signed_pct((h.p95 - b.p95) / b.p95),
            _ => "—".into(),
        };
        let cv = match (d.base.as_ref(), d.head.as_ref()) {
            (Some(b), Some(h)) => format!("{}→{}", pct(b.cv), pct(h.cv)),
            _ => "—".into(),
        };
        let p_val = d
            .p_value
            .map(|p| format!("{:.2e}", p))
            .unwrap_or_else(|| "—".into());
        out.push(format!(
            "| `{}` | {} | {} | {} | {} | {} | {} | {} |",
            escape_md(&d.name),
            base_med,
            head_med,
            delta,
            p95_delta,
            cv,
            p_val,
            verdict_badge(d),
        ));
    }
    out.join("\n")
}

fn headline_for_summary(s: &CompareSummary) -> String {
    if s.regressions > 0 {
        return format!(
            "Performance · {} regression{}",
            s.regressions,
            if s.regressions == 1 { "" } else { "s" }
        );
    }
    if s.improvements > 0 {
        return format!(
            "Performance · {} improvement{}",
            s.improvements,
            if s.improvements == 1 { "" } else { "s" }
        );
    }
    if s.new + s.removed > 0 {
        return "Performance · inventory changed".to_string();
    }
    "Performance · no significant changes".to_string()
}

fn verdict_badge(d: &BenchDiff) -> &'static str {
    match d.verdict {
        Verdict::Regression => "🔴 Regression",
        Verdict::Improvement => "🟢 Improvement",
        Verdict::Neutral => match d.neutral_reason {
            Some(NeutralReason::TooNoisy) => "🟡 Noisy",
            _ => "⚪ Neutral",
        },
        Verdict::New => "🆕 New",
        Verdict::Removed => "🗑 Removed",
        Verdict::OutOfScope => "⏭ Skipped",
    }
}

/// Format a duration in nanoseconds using the most readable unit.
pub fn format_ns(ns: f64) -> String {
    if !ns.is_finite() {
        return "—".to_string();
    }
    if ns < 1_000.0 {
        format!("{:.0}ns", ns)
    } else if ns < 1_000_000.0 {
        format!("{:.2}µs", ns / 1_000.0)
    } else if ns < 1_000_000_000.0 {
        format!("{:.2}ms", ns / 1_000_000.0)
    } else {
        format!("{:.2}s", ns / 1_000_000_000.0)
    }
}

fn pct(frac: f64) -> String {
    let v = frac * 100.0;
    let prec = if frac.abs() < 0.001 {
        2
    } else if frac.abs() < 0.01 {
        1
    } else {
        0
    };
    format!("{:.*}%", prec, v)
}

fn fmt_signed_pct(frac: f64) -> String {
    let v = frac * 100.0;
    let sign = if v > 0.0 { "+" } else { "" };
    format!("{}{:.1}%", sign, v)
}

fn short_ref(r: &str) -> String {
    if r.len() > 7 {
        r[..7].to_string()
    } else {
        r.to_string()
    }
}

fn escape_md(s: &str) -> String {
    s.replace('|', "\\|")
}
