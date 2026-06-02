//! Shared data types for the council pipeline.
//!
//! All fields are `serde`-serialisable so the CLI can write a [`CouncilReport`]
//! to disk between subcommand invocations, mirroring how `aatxe compare`
//! writes a `CompareReport` for `aatxe report` / `aatxe comment` to consume.

use serde::{Deserialize, Serialize};

/// Severity ladder used by the council. Order matters — higher variant wins
/// when the synthesiser merges duplicate findings from different agents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Nit,
    Minor,
    Major,
    Critical,
}

impl Severity {
    pub fn label(self) -> &'static str {
        match self {
            Severity::Nit => "nit",
            Severity::Minor => "minor",
            Severity::Major => "major",
            Severity::Critical => "critical",
        }
    }

    pub fn badge(self) -> &'static str {
        match self {
            Severity::Nit => "💬 nit",
            Severity::Minor => "🟡 minor",
            Severity::Major => "🟠 major",
            Severity::Critical => "🔴 critical",
        }
    }

    /// Parse from a free-form string (LLM outputs are sloppy). Falls back to
    /// [`Severity::Minor`] when the label is unrecognised — better to surface
    /// an under-severed finding than drop it.
    pub fn parse_lenient(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "critical" | "blocker" | "severe" => Severity::Critical,
            "major" | "high" | "warning" => Severity::Major,
            "minor" | "low" | "medium" => Severity::Minor,
            "nit" | "info" | "trivial" | "style" => Severity::Nit,
            _ => Severity::Minor,
        }
    }
}

/// Coarse category — the proposer persona that surfaced the finding.
/// Used by the report renderer to group findings and by the synthesiser to
/// promote multi-persona overlap to higher severity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FindingCategory {
    Correctness,
    Security,
    Performance,
    Maintainability,
    /// Reserved for the judge's own additions (rare — usually the judge only
    /// filters).
    Judge,
}

impl FindingCategory {
    pub fn label(self) -> &'static str {
        match self {
            FindingCategory::Correctness => "correctness",
            FindingCategory::Security => "security",
            FindingCategory::Performance => "performance",
            FindingCategory::Maintainability => "maintainability",
            FindingCategory::Judge => "judge",
        }
    }
}

/// Single proposer finding. The LLM is asked to return a JSON array of
/// objects with these fields; missing fields default sensibly.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Finding {
    /// Repo-relative POSIX path. Empty when the finding is whole-PR.
    #[serde(default)]
    pub file: String,
    /// 1-based line number into the *new* (head) revision. `None` if N/A
    /// (e.g. PR-level finding) or the model omitted it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    pub severity: Severity,
    pub category: FindingCategory,
    pub title: String,
    pub rationale: String,
    /// Optional suggested fix or remediation. Free-form prose.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggestion: Option<String>,
    /// Which agent originally surfaced the finding. Populated by the
    /// pipeline, not the model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raised_by: Option<String>,
}

/// One proposer agent's full output for a chunk.
///
/// When the LLM call itself failed (rate limit exhausted, transport error
/// past the retry budget, model returned unparseable garbage past the
/// parser's tolerance) the pipeline still emits an [`AgentReview`] with an
/// empty `findings` list and the human-readable failure recorded in
/// [`Self::error`]. The rest of the council keeps running — a single
/// persona's outage must not abort the whole review.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentReview {
    pub agent: String,
    pub category: FindingCategory,
    pub findings: Vec<Finding>,
    /// Wall-clock duration of the LLM call, milliseconds. `None` when the
    /// caller didn't measure (e.g. inside synchronous tests).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// One-line error description when the LLM call failed for this
    /// persona. `None` on a successful call. Surfaced in the rendered
    /// council telemetry table so reviewers can spot a degraded run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Prompt tokens reported by the backend for this call. `None` when
    /// the backend didn't report or the call failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_tokens: Option<u32>,
    /// Completion tokens reported by the backend for this call. `None`
    /// when the backend didn't report or the call failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_tokens: Option<u32>,
}

/// What the judge decided about an individual finding after the self-review.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum JudgeVerdict {
    /// Keep at original severity.
    Keep,
    /// Keep but downgrade one rung. `Critical→Major`, `Major→Minor`, etc.
    Downgrade,
    /// Drop entirely — the judge believes this is a false positive or
    /// duplicate that the dedup missed.
    Drop,
}

/// A finding plus its judge verdict and confidence score.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JudgedFinding {
    pub finding: Finding,
    pub verdict: JudgeVerdict,
    /// `[0.0, 1.0]` — judge's confidence that this finding is real and
    /// actionable. Findings below the council's confidence floor are
    /// hidden from the final report.
    pub confidence: f64,
    /// One-line rationale from the judge. Surfaced in `<details>` blocks in
    /// the markdown report so reviewers can see *why* a finding was
    /// downgraded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub judge_note: Option<String>,
}

impl JudgedFinding {
    /// Whether this finding survives the judge — i.e. is shippable to the PR.
    pub fn survives(&self, confidence_floor: f64) -> bool {
        if self.verdict == JudgeVerdict::Drop {
            return false;
        }
        self.confidence >= confidence_floor
    }
}

/// Top-level council output. Carries the raw per-agent reviews (for
/// observability + benches) as well as the final judged findings the CLI
/// renders into the PR comment.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CouncilReport {
    /// Model id used by every agent (e.g. `kimi-k2.6`).
    pub model: String,
    pub repo: String,
    pub pr: u64,
    /// Total files in the raw diff, before filtering.
    pub files_total: u32,
    /// Files actually sent to the council (post path filter).
    pub files_reviewed: u32,
    /// Per-proposer reviews — verbatim model output, deduped only within
    /// each agent's own batch. Useful for diagnosing whether one persona is
    /// dominating findings.
    pub proposer_reviews: Vec<AgentReview>,
    /// Findings *after* dedup + severity normalization, *before* the judge.
    pub synthesized: Vec<Finding>,
    /// The judge's per-finding verdict + confidence. The final shippable
    /// list is the subset of these for which `survives(confidence_floor)`
    /// returns true.
    pub judged: Vec<JudgedFinding>,
    pub confidence_floor: f64,
    /// Sum across all LLM calls in the pipeline, milliseconds.
    pub total_duration_ms: u64,
    /// Sum of prompt tokens across every proposer + judge call that
    /// reported usage. `0` when no backend reported usage.
    #[serde(default)]
    pub total_prompt_tokens: u32,
    /// Sum of completion tokens across every proposer + judge call that
    /// reported usage. `0` when no backend reported usage.
    #[serde(default)]
    pub total_completion_tokens: u32,
    /// Judge call error, if the judge itself failed. When set, every
    /// candidate ships at the default `Keep` / 0.5 confidence the
    /// parser falls back to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub judge_error: Option<String>,
}

impl CouncilReport {
    /// Final shippable findings — survives-the-judge + above confidence floor,
    /// sorted by severity desc then category then title.
    pub fn shippable(&self) -> Vec<&JudgedFinding> {
        let mut v: Vec<&JudgedFinding> = self
            .judged
            .iter()
            .filter(|jf| jf.survives(self.confidence_floor))
            .collect();
        v.sort_by(|a, b| {
            b.finding
                .severity
                .cmp(&a.finding.severity)
                .then(a.finding.category.label().cmp(b.finding.category.label()))
                .then(a.finding.title.cmp(&b.finding.title))
        });
        v
    }

    /// True when at least one shippable finding has [`Severity::Critical`].
    /// Used by the `--fail-on-critical` exit-code gate.
    pub fn has_critical(&self) -> bool {
        self.shippable()
            .iter()
            .any(|jf| jf.finding.severity == Severity::Critical)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn severity_orders_correctly() {
        assert!(Severity::Nit < Severity::Minor);
        assert!(Severity::Minor < Severity::Major);
        assert!(Severity::Major < Severity::Critical);
    }

    #[test]
    fn severity_parse_lenient() {
        assert_eq!(Severity::parse_lenient("CRITICAL"), Severity::Critical);
        assert_eq!(Severity::parse_lenient(" blocker "), Severity::Critical);
        assert_eq!(Severity::parse_lenient("warning"), Severity::Major);
        assert_eq!(Severity::parse_lenient("nit"), Severity::Nit);
        assert_eq!(Severity::parse_lenient("unknown"), Severity::Minor);
    }

    #[test]
    fn judged_finding_survives_respects_floor() {
        let mut jf = JudgedFinding {
            finding: Finding {
                file: "x".into(),
                line: None,
                severity: Severity::Minor,
                category: FindingCategory::Correctness,
                title: "t".into(),
                rationale: "r".into(),
                suggestion: None,
                raised_by: None,
            },
            verdict: JudgeVerdict::Keep,
            confidence: 0.6,
            judge_note: None,
        };
        assert!(jf.survives(0.5));
        assert!(!jf.survives(0.7));
        jf.verdict = JudgeVerdict::Drop;
        assert!(!jf.survives(0.0));
    }

    #[test]
    fn shippable_filters_and_sorts() {
        let mk = |sev: Severity, title: &str, verdict: JudgeVerdict, conf: f64| JudgedFinding {
            finding: Finding {
                file: String::new(),
                line: None,
                severity: sev,
                category: FindingCategory::Correctness,
                title: title.into(),
                rationale: "r".into(),
                suggestion: None,
                raised_by: None,
            },
            verdict,
            confidence: conf,
            judge_note: None,
        };
        let report = CouncilReport {
            model: "kimi-k2.6".into(),
            repo: "x/y".into(),
            pr: 1,
            files_total: 0,
            files_reviewed: 0,
            proposer_reviews: vec![],
            synthesized: vec![],
            judged: vec![
                mk(Severity::Nit, "a", JudgeVerdict::Keep, 0.9),
                mk(Severity::Critical, "b", JudgeVerdict::Drop, 1.0),
                mk(Severity::Major, "c", JudgeVerdict::Keep, 0.2),
                mk(Severity::Critical, "d", JudgeVerdict::Keep, 0.9),
            ],
            confidence_floor: 0.5,
            total_duration_ms: 0,
            total_prompt_tokens: 0,
            total_completion_tokens: 0,
            judge_error: None,
        };
        let ship = report.shippable();
        assert_eq!(ship.len(), 2);
        assert_eq!(ship[0].finding.title, "d"); // Critical first
        assert_eq!(ship[1].finding.title, "a"); // Nit after
        assert!(report.has_critical());
    }
}
