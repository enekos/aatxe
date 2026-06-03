//! Interactive finding curation — the "review the council before
//! posting" step.
//!
//! Lives in the binary crate because it's an IO concern (stdin/stdout)
//! and the pure council crate stays headless.
//!
//! ## Flow
//!
//! 1. The CLI runs the council and produces a [`CouncilReport`].
//! 2. If interactive mode is on, [`curate_report`] walks every
//!    shippable finding in render order (the same order
//!    [`CouncilReport::shippable`] uses, so what the user sees matches
//!    what would be posted) and prompts for one of:
//!
//!    | key | meaning                                                   |
//!    |-----|-----------------------------------------------------------|
//!    | `k` | **k**eep (default, also ↵)                                |
//!    | `d` | **d**rop — mark this finding as a false positive          |
//!    | `s` | **s**kip-all — keep every remaining finding as-is         |
//!    | `q` | **q**uit-all — drop every remaining finding               |
//!    | `?` | help                                                      |
//!
//! 3. Dropped findings have their verdict flipped to
//!    [`JudgeVerdict::Drop`] in place on the report. The renderer
//!    already filters drops out of `shippable()` so the downstream
//!    markdown + post path "just works" against the mutated report.
//!
//! ## Why not write a separate "curated" path
//!
//! Mutating the existing `judged` list — rather than threading a side
//! channel of "kept indices" through render/post — keeps the contract
//! between curator and renderer the same one the judge already uses:
//! `verdict=Drop` means "don't ship". One place to look for "why didn't
//! this finding appear in the comment?".

use aatxe_council::types::{CouncilReport, JudgeVerdict};
use anyhow::Result;
use std::io::{BufRead, IsTerminal, Write};

/// Outcome of one curation pass — used by the CLI to log a summary line
/// and (in a future commit) auto-emit `aatxe: false-positive on N`
/// directives so dropped findings teach the learning corpus.
#[derive(Debug, Clone, Default)]
pub struct CurationSummary {
    pub kept: u32,
    pub dropped: u32,
    /// Per-finding drop records, indexed against the **pre-curation**
    /// shippable order. Useful for downstream consumers (e.g. an auto-
    /// directive writer) that want to point at "the Nth finding the
    /// council originally shipped".
    pub dropped_indices: Vec<u32>,
}

/// Whether the runtime environment supports an interactive prompt.
/// Default behaviour is "on iff stdin is a TTY".
pub fn stdin_is_tty() -> bool {
    std::io::stdin().is_terminal()
}

/// Drive the curation loop against a report. Mutates `report.judged`
/// to flip dropped findings to [`JudgeVerdict::Drop`] so the
/// downstream renderer + post path filters them out.
///
/// `reader` / `writer` are injected for testability — production uses
/// stdin/stderr.
pub fn curate_report<R: BufRead, W: Write>(
    report: &mut CouncilReport,
    mut reader: R,
    mut writer: W,
) -> Result<CurationSummary> {
    // Snapshot the pre-curation shippable order so prompts and the
    // returned summary refer to a stable indexing scheme.
    let snapshot: Vec<(usize, ShippableMeta)> = report
        .shippable()
        .iter()
        .enumerate()
        .map(|(display_idx, jf)| {
            let judged_idx = report
                .judged
                .iter()
                .position(|cand| std::ptr::eq(cand, *jf))
                .expect("shippable() returns refs into report.judged");
            (
                judged_idx,
                ShippableMeta {
                    display_idx: display_idx as u32,
                    severity: jf.finding.severity.label().to_string(),
                    category: jf.finding.category.label().to_string(),
                    file: jf.finding.file.clone(),
                    line: jf.finding.line,
                    title: jf.finding.title.clone(),
                    rationale: jf.finding.rationale.clone(),
                    suggestion: jf.finding.suggestion.clone(),
                    confidence: jf.confidence,
                },
            )
        })
        .collect();

    let total = snapshot.len();
    if total == 0 {
        writeln!(
            writer,
            "aatxe council: no shippable findings to curate — nothing to do."
        )?;
        return Ok(CurationSummary::default());
    }

    writeln!(
        writer,
        "aatxe council: curating {} shippable finding{} interactively. Keys: [k]eep / [d]rop / [s]kip-all / [q]uit-all / ? help.",
        total,
        if total == 1 { "" } else { "s" },
    )?;

    let mut summary = CurationSummary::default();
    let mut auto_decision: Option<AutoDecision> = None;

    for (judged_idx, meta) in &snapshot {
        // Render the finding header for the user.
        writeln!(writer)?;
        writeln!(
            writer,
            "[{}/{}] {} [{}] {}{}",
            meta.display_idx + 1,
            total,
            meta.severity.to_uppercase(),
            meta.category,
            meta.file,
            meta.line.map(|l| format!(":{l}")).unwrap_or_default(),
        )?;
        writeln!(writer, "  {}", meta.title)?;
        if !meta.rationale.trim().is_empty() {
            writeln!(
                writer,
                "  Rationale: {}",
                trim_one_line(&meta.rationale, 360)
            )?;
        }
        if let Some(s) = &meta.suggestion {
            if !s.trim().is_empty() {
                writeln!(writer, "  Suggestion: {}", trim_one_line(s, 360))?;
            }
        }
        writeln!(writer, "  Confidence: {:.2}", meta.confidence)?;

        // Apply an auto-decision (skip-all / quit-all) without
        // re-prompting.
        let decision = match auto_decision {
            Some(AutoDecision::KeepRest) => Decision::Keep,
            Some(AutoDecision::DropRest) => Decision::Drop,
            None => prompt_decision(&mut reader, &mut writer, &mut auto_decision)?,
        };

        match decision {
            Decision::Keep => {
                summary.kept += 1;
            }
            Decision::Drop => {
                summary.dropped += 1;
                summary.dropped_indices.push(meta.display_idx);
                report.judged[*judged_idx].verdict = JudgeVerdict::Drop;
            }
        }
    }

    writeln!(
        writer,
        "\naatxe council: curation complete — kept {}, dropped {}.",
        summary.kept, summary.dropped,
    )?;
    Ok(summary)
}

#[derive(Debug, Clone)]
struct ShippableMeta {
    display_idx: u32,
    severity: String,
    category: String,
    file: String,
    line: Option<u32>,
    title: String,
    rationale: String,
    suggestion: Option<String>,
    confidence: f64,
}

#[derive(Debug, Clone, Copy)]
enum Decision {
    Keep,
    Drop,
}

#[derive(Debug, Clone, Copy)]
enum AutoDecision {
    KeepRest,
    DropRest,
}

fn prompt_decision<R: BufRead, W: Write>(
    reader: &mut R,
    writer: &mut W,
    auto: &mut Option<AutoDecision>,
) -> Result<Decision> {
    loop {
        write!(
            writer,
            "  [k]eep / [d]rop / [s]kip-all / [q]uit-all (default k): "
        )?;
        writer.flush()?;
        let mut buf = String::new();
        let n = reader.read_line(&mut buf)?;
        if n == 0 {
            // EOF on stdin — treat as "keep" so a non-interactive
            // misconfiguration doesn't quietly drop the entire review.
            return Ok(Decision::Keep);
        }
        match buf.trim().to_ascii_lowercase().as_str() {
            "" | "k" | "keep" | "y" => return Ok(Decision::Keep),
            "d" | "drop" | "n" => return Ok(Decision::Drop),
            "s" | "skip" | "skip-all" => {
                *auto = Some(AutoDecision::KeepRest);
                return Ok(Decision::Keep);
            }
            "q" | "quit" | "quit-all" => {
                *auto = Some(AutoDecision::DropRest);
                return Ok(Decision::Drop);
            }
            "?" | "h" | "help" => {
                writeln!(
                    writer,
                    "    k: keep this finding (default).\n\
                     \x20   d: drop — mark as false positive, hidden from the rendered comment.\n\
                     \x20   s: skip-all — keep every remaining finding.\n\
                     \x20   q: quit-all — drop every remaining finding.\n\
                     \x20   ?: this help."
                )?;
                // Loop to re-prompt.
            }
            other => {
                writeln!(writer, "    unknown choice {other:?} — try k/d/s/q or ?")?;
            }
        }
    }
}

/// Collapse multi-line free-form prose to a single line and cap at
/// `max` characters. Keeps the curation prompt scannable on terminals
/// without word-wrap support.
fn trim_one_line(s: &str, max: usize) -> String {
    let mut collapsed: String = s
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if collapsed.chars().count() > max {
        let truncated: String = collapsed.chars().take(max - 1).collect();
        collapsed = format!("{truncated}…");
    }
    collapsed
}

#[cfg(test)]
mod tests {
    use super::*;
    use aatxe_council::types::{
        AgentReview, CouncilReport, Finding, FindingCategory, JudgedFinding, Severity,
    };

    fn finding(file: &str, sev: Severity, title: &str) -> JudgedFinding {
        JudgedFinding {
            finding: Finding {
                file: file.into(),
                line: Some(1),
                severity: sev,
                category: FindingCategory::Correctness,
                title: title.into(),
                rationale: "r".into(),
                suggestion: None,
                raised_by: None,
            },
            verdict: JudgeVerdict::Keep,
            confidence: 0.9,
            judge_note: None,
        }
    }

    fn report_with(findings: Vec<JudgedFinding>) -> CouncilReport {
        CouncilReport {
            model: "stub".into(),
            repo: "x/y".into(),
            pr: 1,
            files_total: 1,
            files_reviewed: 1,
            proposer_reviews: Vec::<AgentReview>::new(),
            synthesized: Vec::new(),
            judged: findings,
            confidence_floor: 0.5,
            total_duration_ms: 0,
            total_prompt_tokens: 0,
            total_completion_tokens: 0,
            judge_error: None,
        }
    }

    #[test]
    fn keep_decision_leaves_judged_unchanged() {
        let mut report = report_with(vec![finding("a.rs", Severity::Major, "t1")]);
        let mut out = Vec::new();
        let summary = curate_report(&mut report, &b"k\n"[..], &mut out).unwrap();
        assert_eq!(summary.kept, 1);
        assert_eq!(summary.dropped, 0);
        assert_eq!(report.judged[0].verdict, JudgeVerdict::Keep);
        let stdout = String::from_utf8(out).unwrap();
        assert!(stdout.contains("kept 1"));
    }

    #[test]
    fn drop_decision_flips_verdict_so_renderer_hides_it() {
        let mut report = report_with(vec![
            finding("a.rs", Severity::Major, "keep me"),
            finding("b.rs", Severity::Minor, "drop me"),
        ]);
        let mut out = Vec::new();
        let summary = curate_report(&mut report, &b"k\nd\n"[..], &mut out).unwrap();
        assert_eq!(summary.kept, 1);
        assert_eq!(summary.dropped, 1);
        // shippable order is severity-desc → Major (a.rs) first, then
        // Minor (b.rs). So the second input (`d`) dropped b.rs.
        let dropped = report
            .judged
            .iter()
            .find(|jf| jf.verdict == JudgeVerdict::Drop)
            .unwrap();
        assert_eq!(dropped.finding.file, "b.rs");
        // Renderer-facing invariant: shippable() now hides the dropped
        // one.
        assert_eq!(report.shippable().len(), 1);
    }

    #[test]
    fn skip_all_keeps_every_remaining_finding() {
        // First prompt: `s` (skip-all). Second + third prompts should
        // never be asked.
        let mut report = report_with(vec![
            finding("a.rs", Severity::Major, "t1"),
            finding("b.rs", Severity::Minor, "t2"),
            finding("c.rs", Severity::Nit, "t3"),
        ]);
        let mut out = Vec::new();
        let summary = curate_report(&mut report, &b"s\n"[..], &mut out).unwrap();
        assert_eq!(summary.kept, 3);
        assert_eq!(summary.dropped, 0);
    }

    #[test]
    fn quit_all_drops_every_remaining_finding() {
        let mut report = report_with(vec![
            finding("a.rs", Severity::Major, "t1"),
            finding("b.rs", Severity::Minor, "t2"),
        ]);
        let mut out = Vec::new();
        let summary = curate_report(&mut report, &b"q\n"[..], &mut out).unwrap();
        assert_eq!(summary.dropped, 2);
        assert_eq!(report.shippable().len(), 0);
    }

    #[test]
    fn empty_input_defaults_to_keep() {
        // Pressing Enter at the prompt should keep the finding.
        let mut report = report_with(vec![finding("a.rs", Severity::Major, "t1")]);
        let mut out = Vec::new();
        let summary = curate_report(&mut report, &b"\n"[..], &mut out).unwrap();
        assert_eq!(summary.kept, 1);
    }

    #[test]
    fn unknown_input_loops_to_reprompt() {
        // `x` then `d` → unknown choice once, then drop.
        let mut report = report_with(vec![finding("a.rs", Severity::Major, "t1")]);
        let mut out = Vec::new();
        let summary = curate_report(&mut report, &b"x\nd\n"[..], &mut out).unwrap();
        assert_eq!(summary.dropped, 1);
        let stdout = String::from_utf8(out).unwrap();
        assert!(stdout.contains("unknown choice"));
    }

    #[test]
    fn help_input_loops_then_accepts_decision() {
        let mut report = report_with(vec![finding("a.rs", Severity::Major, "t1")]);
        let mut out = Vec::new();
        let summary = curate_report(&mut report, &b"?\nk\n"[..], &mut out).unwrap();
        assert_eq!(summary.kept, 1);
        let stdout = String::from_utf8(out).unwrap();
        assert!(stdout.contains("keep this finding"));
    }

    #[test]
    fn empty_report_short_circuits() {
        let mut report = report_with(Vec::new());
        let mut out = Vec::new();
        let summary = curate_report(&mut report, &b""[..], &mut out).unwrap();
        assert_eq!(summary.kept, 0);
        assert_eq!(summary.dropped, 0);
        let stdout = String::from_utf8(out).unwrap();
        assert!(stdout.contains("nothing to do"));
    }

    #[test]
    fn eof_mid_loop_keeps_remaining() {
        // First decision via `d`, then stdin EOF for the second
        // prompt — must default to Keep and not panic.
        let mut report = report_with(vec![
            finding("a.rs", Severity::Major, "t1"),
            finding("b.rs", Severity::Minor, "t2"),
        ]);
        let mut out = Vec::new();
        let summary = curate_report(&mut report, &b"d\n"[..], &mut out).unwrap();
        assert_eq!(summary.dropped, 1);
        assert_eq!(summary.kept, 1);
    }

    #[test]
    fn trim_one_line_collapses_and_caps() {
        let s = "first line\n\nsecond  line";
        assert_eq!(trim_one_line(s, 100), "first line second  line");
        let long = "a".repeat(500);
        assert!(trim_one_line(&long, 50).chars().count() == 50);
    }
}
