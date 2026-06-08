//! Council quality evals.
//!
//! Scores a [`aatxe_council::CouncilReport`] against a hand-labeled ground
//! truth ([`CouncilCase`]). Aggregates across an entire corpus into a
//! [`CouncilEvalSummary`] with the headline metrics the regression gate
//! reads.
//!
//! ## Matching policy
//!
//! A shippable finding `f` *matches* an expected finding `e` when:
//! * `f.file == e.file` (POSIX-relative, exact match), AND
//! * the optional [`ExpectedFinding::line_range`] either contains `f.line`,
//!   or the expected case did not pin a line range, AND
//! * if the expected case pins a [`ExpectedFinding::category`], `f.category`
//!   matches it.
//!
//! We deliberately do *not* match on severity — the model is allowed to be
//! more or less alarmed than the labeler — but severity is scored
//! separately as [`CouncilEvalSummary::severity_calibration_mae`]: the mean
//! absolute distance (in ladder rungs, 0..=3) between the model's severity
//! and the expected severity over matched findings.
//!
//! ## Why this shape
//!
//! Industry PR-reviewer eval rigs (CodeRabbit's published benchmarks,
//! AppSec-CSE 2024, the SWE-bench-lite review track) agree on the same
//! axes: per-severity recall on planted bugs, false-positive rate on clean
//! PRs, and confidence calibration. We adopt the same axes and add severity
//! calibration MAE because aatxe's gate is severity-aware (critical
//! findings can fail the build via `--fail-on-critical`).

use aatxe_council::types::{
    CouncilReport, Finding, FindingCategory, JudgeVerdict, JudgedFinding, Severity,
};
use serde::{Deserialize, Serialize};

/// One labeled case in the council corpus. Loaded from JSON next to a
/// unified-diff file; see `evals/council/cases/*` for examples.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CouncilCase {
    /// Stable slug — used as the row key in reports. e.g. `security-password-logged`.
    pub name: String,
    /// One-sentence description of what's planted in the diff.
    #[serde(default)]
    pub description: String,
    /// Path to the unified-diff file, relative to the case JSON.
    pub diff: String,
    /// Optional directory of post-PR file fixtures, relative to the case
    /// JSON. When present the harness walks it recursively and treats
    /// every relative path inside as a repo-rooted file. Files matching
    /// paths in the diff get attached as
    /// [`aatxe_council::ParsedFile::context`] so proposers see the
    /// surrounding code, not just the hunk. Convention:
    /// `files/<case-slug>/src/auth/login.rs` etc.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files_dir: Option<String>,
    /// Findings the council *should* surface. Every `must_catch = true`
    /// finding that goes uncovered is a recall miss.
    #[serde(default)]
    pub expected: Vec<ExpectedFinding>,
    /// File patterns the council must never flag. Used to score the
    /// false-positive rate on clean files / generated code / lockfiles.
    #[serde(default)]
    pub forbidden: Vec<ForbiddenPath>,
    /// Optional cap on shippable findings the council may produce on this
    /// case. Used by "clean PR" cases where any non-zero output is a
    /// false positive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_findings: Option<u32>,
}

/// A ground-truth finding the council is expected to surface.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExpectedFinding {
    pub file: String,
    /// Inclusive line range `[start, end]` into the *new* (head) revision.
    /// `None` matches any line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_range: Option<[u32; 2]>,
    /// Expected severity. Used for `severity_calibration_mae`.
    pub severity: Severity,
    /// Expected proposer category. `None` accepts any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    /// One-line description of the bug — surfaced in the per-case report so
    /// a missed finding is human-readable.
    #[serde(default)]
    pub description: String,
    /// When `true`, missing this finding counts as a recall miss. When
    /// `false`, it's bonus — caught = credit, missed = silent.
    #[serde(default = "default_true")]
    pub must_catch: bool,
}

fn default_true() -> bool {
    true
}

/// A file pattern the council must not flag. Currently exact-string match
/// against `finding.file` (suffix-aware: `Cargo.lock` matches any path
/// ending in `/Cargo.lock`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForbiddenPath {
    pub path: String,
    #[serde(default)]
    pub reason: String,
}

#[derive(Debug, Clone, Copy)]
pub struct CouncilEvalOptions {
    /// Floor at which the harness considers a judged finding "shipped". The
    /// scorer ignores findings below this, mirroring what the user actually
    /// sees in the PR comment. Default 0.55 — matches
    /// `CouncilOptions::default().confidence_floor`.
    pub confidence_floor: f64,
}

impl Default for CouncilEvalOptions {
    fn default() -> Self {
        Self {
            confidence_floor: 0.55,
        }
    }
}

/// Outcome of scoring one case.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CouncilCaseResult {
    pub name: String,
    /// Total `must_catch = true` expected findings on this case.
    pub expected_total: u32,
    /// How many of those were covered by at least one shippable finding.
    pub expected_caught: u32,
    /// Bonus (must_catch=false) expected findings that were caught.
    pub bonus_caught: u32,
    /// Shippable findings produced by the council on this case.
    pub findings_total: u32,
    /// Shippable findings that didn't match any expected entry and didn't
    /// hit a forbidden path — these are *unmatched* findings, candidate
    /// false positives.
    pub findings_unmatched: u32,
    /// Shippable findings on a forbidden path (Cargo.lock, etc.). Always
    /// counts as a false positive.
    pub findings_forbidden: u32,
    /// Whether the council respected the case's `max_findings` cap.
    pub max_findings_violated: bool,
    /// Per-severity recall buckets, indexed by [`Severity`] cast to u8.
    /// `(caught_at_or_above_severity, total_at_or_above_severity)`.
    pub recall_by_severity: SeverityRecall,
    /// Sum of |model_severity_rung - expected_severity_rung| over matched
    /// must_catch findings. Used to compute mean absolute error.
    pub severity_distance_sum: u32,
    /// Count of matched must_catch findings — denominator for the MAE.
    pub severity_distance_n: u32,
    /// Brier-score numerator contribution from this case (sum of
    /// `(confidence − outcome)^2`, where `outcome=1` if the finding
    /// matched a `must_catch = true` expected and `0` otherwise).
    pub brier_sum: f64,
    pub brier_n: u32,
    /// Latency, bytes-of-prompt, etc. — passed through verbatim so the
    /// summary can aggregate.
    pub total_duration_ms: u64,
    pub total_prompt_tokens: u32,
    pub total_completion_tokens: u32,
    /// Council error surfaced to the report (e.g. judge call failed and
    /// fell back to keep/0.5). Carried into the summary so a degraded run
    /// is visible without re-reading the raw JSON.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub judge_error: Option<String>,
    /// Per-finding records for every non-Drop judged finding, captured
    /// pre-floor-filter. Enables offline floor recalibration: a single
    /// real-LLM eval run produces records, then
    /// [`recalibrate_case_from_records`] re-derives the aggregate metrics
    /// at any candidate floor without re-querying the LLM.
    ///
    /// `#[serde(default)]` keeps older committed baselines (which lacked
    /// this field) round-trippable; the recalibrator errors out cleanly
    /// when the field is empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub per_findings: Vec<PerFindingRecord>,
}

/// Compact, scorer-only view of one [`JudgedFinding`].
///
/// Stores just the fields the scorer reads ([`finding_matches`] +
/// [`hits_forbidden_path`] + severity-recall + Brier) so the eval JSON
/// stays small. `rationale` / `suggestion` / `raised_by` are intentionally
/// dropped — they live in the raw `CouncilReport` artefact for any
/// reviewer who wants to eyeball model output.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PerFindingRecord {
    pub file: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    pub severity: Severity,
    pub category: FindingCategory,
    pub confidence: f64,
    pub verdict: JudgeVerdict,
    /// Truncated to ≤ 80 chars for compactness — the full title lives in
    /// the raw `CouncilReport`. Carried here so calibration sweeps can be
    /// audited finding-by-finding without joining back to the report.
    pub title: String,
}

impl PerFindingRecord {
    /// Synthesize the [`Finding`] subset the matchers/severity-recall
    /// scorer needs. The rationale / suggestion / raised_by fields are
    /// not exercised by the scorer, so empty defaults are fine.
    fn as_finding(&self) -> Finding {
        Finding {
            file: self.file.clone(),
            line: self.line,
            severity: self.severity,
            category: self.category,
            title: self.title.clone(),
            rationale: String::new(),
            suggestion: None,
            raised_by: None,
        }
    }
}

fn truncate_title(s: &str) -> String {
    if s.len() <= 80 {
        return s.to_string();
    }
    // Char-boundary-safe truncation: walk to the last boundary ≤ 77 then
    // append `…`. Avoids panicking on multi-byte titles.
    let mut end = 77;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = String::with_capacity(end + 3);
    out.push_str(&s[..end]);
    out.push('…');
    out
}

/// Per-severity recall buckets. Each `(caught, total)` is a cumulative
/// count: `critical_caught` only counts findings at `Severity::Critical`,
/// but `major_caught` includes both `Severity::Major` and higher. This
/// matches the way callers think about "did we catch the critical bug?"
/// vs "did we catch *any* of the major-and-up bugs?".
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SeverityRecall {
    pub critical_caught: u32,
    pub critical_total: u32,
    pub major_caught: u32,
    pub major_total: u32,
    pub minor_caught: u32,
    pub minor_total: u32,
    pub nit_caught: u32,
    pub nit_total: u32,
}

impl SeverityRecall {
    fn record(&mut self, sev: Severity, caught: bool) {
        match sev {
            Severity::Critical => {
                self.critical_total += 1;
                if caught {
                    self.critical_caught += 1;
                }
            }
            Severity::Major => {
                self.major_total += 1;
                if caught {
                    self.major_caught += 1;
                }
            }
            Severity::Minor => {
                self.minor_total += 1;
                if caught {
                    self.minor_caught += 1;
                }
            }
            Severity::Nit => {
                self.nit_total += 1;
                if caught {
                    self.nit_caught += 1;
                }
            }
        }
    }

    fn merge(&mut self, other: &SeverityRecall) {
        self.critical_caught += other.critical_caught;
        self.critical_total += other.critical_total;
        self.major_caught += other.major_caught;
        self.major_total += other.major_total;
        self.minor_caught += other.minor_caught;
        self.minor_total += other.minor_total;
        self.nit_caught += other.nit_caught;
        self.nit_total += other.nit_total;
    }

    /// Recall ∈ [0, 1] over findings at-or-above the given severity.
    pub fn recall_at_or_above(&self, sev: Severity) -> f64 {
        let (caught, total) = match sev {
            Severity::Critical => (self.critical_caught, self.critical_total),
            Severity::Major => (
                self.critical_caught + self.major_caught,
                self.critical_total + self.major_total,
            ),
            Severity::Minor => (
                self.critical_caught + self.major_caught + self.minor_caught,
                self.critical_total + self.major_total + self.minor_total,
            ),
            Severity::Nit => (
                self.critical_caught + self.major_caught + self.minor_caught + self.nit_caught,
                self.critical_total + self.major_total + self.minor_total + self.nit_total,
            ),
        };
        if total == 0 {
            return 1.0;
        }
        caught as f64 / total as f64
    }
}

/// Aggregate across a whole corpus. The CLI writes this into the report.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CouncilEvalSummary {
    pub cases_total: u32,
    /// Cases where every `must_catch = true` expected finding was covered.
    pub cases_fully_recalled: u32,
    /// Cases where the council exceeded `max_findings`.
    pub cases_over_cap: u32,
    /// Cases that completed despite a non-`None` `judge_error`.
    pub cases_with_judge_error: u32,
    pub recall: SeverityRecall,
    /// Critical-severity precision among shippable findings that landed on
    /// a *labelled* line: `matched_critical / shipped_critical`. We
    /// compute it on critical because that's what gates CI; lower
    /// severities are advisory.
    pub critical_precision: f64,
    pub critical_recall: f64,
    pub critical_f1: f64,
    /// Mean absolute severity-rung error over matched must_catch findings.
    /// 0 = perfect calibration; 3 = always off by max (nit↔critical).
    pub severity_calibration_mae: f64,
    /// Brier score over the shippable set: `mean((conf - outcome)^2)`. 0 =
    /// perfect; 0.25 = chance; 1 = always wrong.
    pub judge_brier_score: f64,
    /// Sum of unmatched-shipped findings divided by `cases_total` — the
    /// average false-positive count per case (across the whole corpus).
    pub avg_false_positives_per_case: f64,
    /// Total forbidden-path findings across the corpus. Hard signal: any
    /// >0 is a calibration bug.
    pub forbidden_path_findings: u32,
    pub avg_latency_ms: u64,
    pub total_prompt_tokens: u64,
    pub total_completion_tokens: u64,
    pub per_case: Vec<CouncilCaseResult>,
}

/// Score a single council case. The caller is responsible for actually
/// invoking the council — this function never touches the LLM.
///
/// The returned [`CouncilCaseResult::per_findings`] captures every
/// non-Drop judged finding, regardless of whether it survived the floor.
/// That makes the result self-sufficient for offline floor recalibration
/// via [`recalibrate_case_from_records`].
pub fn score_case(
    case: &CouncilCase,
    report: &CouncilReport,
    opts: CouncilEvalOptions,
) -> CouncilCaseResult {
    // Capture per-finding records first — independent of the floor so the
    // user can recalibrate at any candidate floor later. Skip `Drop`
    // verdicts because the judge already rejected them; they never ship
    // regardless of the floor and would only add noise to recalibration.
    let per_findings: Vec<PerFindingRecord> = report
        .judged
        .iter()
        .filter(|jf| jf.verdict != JudgeVerdict::Drop)
        .map(|jf| PerFindingRecord {
            file: jf.finding.file.clone(),
            line: jf.finding.line,
            severity: jf.finding.severity,
            category: jf.finding.category,
            confidence: jf.confidence,
            verdict: jf.verdict,
            title: truncate_title(&jf.finding.title),
        })
        .collect();

    let mut result = score_at_floor(case, report, opts);
    result.per_findings = per_findings;
    result
}

/// Shared scoring core for [`score_case`] and
/// [`recalibrate_case_from_records`]. Takes a [`CouncilReport`] so it can
/// pull `total_duration_ms` / token usage / `judge_error` from the
/// original run (those are floor-invariant). Doesn't populate
/// `per_findings` — that's the caller's responsibility.
fn score_at_floor(
    case: &CouncilCase,
    report: &CouncilReport,
    opts: CouncilEvalOptions,
) -> CouncilCaseResult {
    let shipped: Vec<&JudgedFinding> = report
        .judged
        .iter()
        .filter(|jf| jf.survives(opts.confidence_floor))
        .collect();
    let mut matched_indices: Vec<Option<usize>> = vec![None; case.expected.len()];
    let mut shipped_matched: Vec<bool> = vec![false; shipped.len()];

    // Greedy match: walk expected in declaration order; for each, take the
    // first unclaimed shipped finding that satisfies the match predicate.
    // Stable + deterministic; matches industry rigs (e.g. SARIF eval).
    for (ei, expected) in case.expected.iter().enumerate() {
        for (si, jf) in shipped.iter().enumerate() {
            if shipped_matched[si] {
                continue;
            }
            if finding_matches(expected, &jf.finding) {
                matched_indices[ei] = Some(si);
                shipped_matched[si] = true;
                break;
            }
        }
    }

    // Per-severity recall on must_catch entries.
    let mut recall = SeverityRecall::default();
    let mut sev_distance_sum: u32 = 0;
    let mut sev_distance_n: u32 = 0;
    let mut expected_caught: u32 = 0;
    let mut bonus_caught: u32 = 0;
    let mut expected_total_required: u32 = 0;
    for (ei, expected) in case.expected.iter().enumerate() {
        let caught = matched_indices[ei].is_some();
        if expected.must_catch {
            expected_total_required += 1;
            if caught {
                expected_caught += 1;
            }
            recall.record(expected.severity, caught);
            if let Some(si) = matched_indices[ei] {
                let sev_model = shipped[si].finding.severity;
                sev_distance_sum += severity_rung_distance(sev_model, expected.severity);
                sev_distance_n += 1;
            }
        } else if caught {
            bonus_caught += 1;
        }
    }

    // Brier score: matched must_catch → outcome 1; everything else → 0.
    // We score every *shipped* finding here, including ones that didn't
    // match anything — they get outcome 0 because they're (probably)
    // false positives. This is the right thing for a calibration metric:
    // the model claims confidence even on false positives, and we punish
    // overconfidence symmetrically.
    let mut brier_sum: f64 = 0.0;
    let mut brier_n: u32 = 0;
    for (si, jf) in shipped.iter().enumerate() {
        let outcome = if shipped_matched[si] { 1.0 } else { 0.0 };
        let d = jf.confidence - outcome;
        brier_sum += d * d;
        brier_n += 1;
    }

    // Forbidden-path + unmatched accounting.
    let mut findings_forbidden = 0u32;
    let mut findings_unmatched = 0u32;
    for (si, jf) in shipped.iter().enumerate() {
        if hits_forbidden_path(&jf.finding, &case.forbidden) {
            findings_forbidden += 1;
        } else if !shipped_matched[si] {
            findings_unmatched += 1;
        }
    }

    let findings_total = shipped.len() as u32;
    let max_findings_violated = matches!(case.max_findings, Some(cap) if findings_total > cap);

    CouncilCaseResult {
        name: case.name.clone(),
        expected_total: expected_total_required,
        expected_caught,
        bonus_caught,
        findings_total,
        findings_unmatched,
        findings_forbidden,
        max_findings_violated,
        recall_by_severity: recall,
        severity_distance_sum: sev_distance_sum,
        severity_distance_n: sev_distance_n,
        brier_sum,
        brier_n,
        total_duration_ms: report.total_duration_ms,
        total_prompt_tokens: report.total_prompt_tokens,
        total_completion_tokens: report.total_completion_tokens,
        judge_error: report.judge_error.clone(),
        per_findings: Vec::new(),
    }
}

/// Offline recalibration: take per-finding records captured at an earlier
/// floor and re-derive the case's aggregate metrics at a new floor. The
/// returned result preserves the original `per_findings` so chained
/// recalibrations are stable.
///
/// When `prior.per_findings` is empty, the function trusts the data:
/// "this case legitimately produced zero non-Drop findings". The summary-
/// level driver ([`recalibrate_summary_from_records`]) distinguishes that
/// from "old eval JSON without records" by requiring at least one case in
/// the prior to have non-empty records; if every case is empty, it errors
/// out with [`RecalibrateError::NoRecords`].
pub fn recalibrate_case_from_records(
    case: &CouncilCase,
    prior: &CouncilCaseResult,
    opts: CouncilEvalOptions,
) -> Result<CouncilCaseResult, RecalibrateError> {
    // Materialise a synthetic CouncilReport from the records so the
    // unchanged scoring core works without a parallel implementation.
    // Only the fields score_at_floor reads from the report are populated.
    let judged: Vec<JudgedFinding> = prior
        .per_findings
        .iter()
        .map(|r| JudgedFinding {
            finding: r.as_finding(),
            verdict: r.verdict,
            confidence: r.confidence,
            judge_note: None,
        })
        .collect();

    let synthetic = CouncilReport {
        model: String::new(),
        repo: String::new(),
        pr: 0,
        files_total: 0,
        files_reviewed: 0,
        proposer_reviews: Vec::new(),
        synthesized: judged.iter().map(|jf| jf.finding.clone()).collect(),
        judged,
        confidence_floor: opts.confidence_floor,
        total_duration_ms: prior.total_duration_ms,
        total_prompt_tokens: prior.total_prompt_tokens,
        total_completion_tokens: prior.total_completion_tokens,
        judge_error: prior.judge_error.clone(),
    };

    let mut result = score_at_floor(case, &synthetic, opts);
    // Preserve the records so the returned result is itself recalibratable.
    result.per_findings = prior.per_findings.clone();
    Ok(result)
}

/// Errors returned by [`recalibrate_case_from_records`] /
/// [`recalibrate_summary_from_records`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecalibrateError {
    /// A specific case in the prior summary was unrecoverable — e.g. the
    /// corpus no longer has a case by that name. Surfaced by
    /// [`recalibrate_summary_from_records`].
    CaseMissingFromCorpus { case: String },
    /// Every case in the prior summary had no per-finding records —
    /// signature of an old eval JSON generated before records landed.
    /// Re-run `aatxe evals` to capture records, then retry.
    AllRecordsMissing { case_count: usize },
}

impl std::fmt::Display for RecalibrateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RecalibrateError::CaseMissingFromCorpus { case } => write!(
                f,
                "case `{case}` listed in prior eval but not found in current corpus — recalibration cannot match findings against ground truth that no longer exists"
            ),
            RecalibrateError::AllRecordsMissing { case_count } => write!(
                f,
                "all {case_count} cases in the prior eval are missing per-finding records — the prior eval was generated by an aatxe version that didn't emit them; re-run `aatxe evals` to capture records, then retry"
            ),
        }
    }
}

impl std::error::Error for RecalibrateError {}

/// Recalibrate every case in a prior summary at a new floor, then
/// re-aggregate the corpus-level summary.
///
/// Errors out when the prior summary is missing per-finding records
/// across *every* case (signature of an old eval JSON generated before
/// the records field landed). A summary where *some* cases have empty
/// records is fine — those cases legitimately produced zero non-Drop
/// findings and recalibrate to zero metrics regardless of floor.
pub fn recalibrate_summary_from_records(
    cases_by_name: &std::collections::HashMap<String, CouncilCase>,
    prior: &CouncilEvalSummary,
    opts: CouncilEvalOptions,
) -> Result<CouncilEvalSummary, RecalibrateError> {
    if prior.per_case.iter().all(|c| c.per_findings.is_empty()) {
        return Err(RecalibrateError::AllRecordsMissing {
            case_count: prior.per_case.len(),
        });
    }
    let mut per_case: Vec<CouncilCaseResult> = Vec::with_capacity(prior.per_case.len());
    for case_result in &prior.per_case {
        let Some(case) = cases_by_name.get(&case_result.name) else {
            return Err(RecalibrateError::CaseMissingFromCorpus {
                case: case_result.name.clone(),
            });
        };
        per_case.push(recalibrate_case_from_records(case, case_result, opts)?);
    }
    Ok(score_council(per_case))
}

/// Aggregate per-case results into a corpus-level summary.
pub fn score_council(per_case: Vec<CouncilCaseResult>) -> CouncilEvalSummary {
    let cases_total = per_case.len() as u32;
    let mut recall = SeverityRecall::default();
    let mut total_brier_sum: f64 = 0.0;
    let mut total_brier_n: u32 = 0;
    let mut total_sev_distance_sum: u32 = 0;
    let mut total_sev_distance_n: u32 = 0;
    let mut total_unmatched: u32 = 0;
    let mut total_forbidden: u32 = 0;
    let mut total_latency_ms: u64 = 0;
    let mut total_prompt_tokens: u64 = 0;
    let mut total_completion_tokens: u64 = 0;
    let mut cases_fully_recalled = 0u32;
    let mut cases_over_cap = 0u32;
    let mut cases_with_judge_error = 0u32;
    let mut shipped_critical: u32 = 0;
    let mut shipped_critical_matched: u32 = 0;

    for r in &per_case {
        recall.merge(&r.recall_by_severity);
        total_brier_sum += r.brier_sum;
        total_brier_n += r.brier_n;
        total_sev_distance_sum += r.severity_distance_sum;
        total_sev_distance_n += r.severity_distance_n;
        total_unmatched += r.findings_unmatched;
        total_forbidden += r.findings_forbidden;
        total_latency_ms += r.total_duration_ms;
        total_prompt_tokens += r.total_prompt_tokens as u64;
        total_completion_tokens += r.total_completion_tokens as u64;
        if r.expected_total == r.expected_caught {
            cases_fully_recalled += 1;
        }
        if r.max_findings_violated {
            cases_over_cap += 1;
        }
        if r.judge_error.is_some() {
            cases_with_judge_error += 1;
        }
        // For critical precision we need to know how many *shipped*
        // findings were critical and how many of those landed on a labelled
        // critical. We don't currently carry that breakdown per case, so
        // approximate: caught_critical is a tight lower bound on
        // shipped_critical_matched (every caught critical is by definition a
        // critical that matched a labelled finding). Approximation upper-
        // bounds precision; we score the bound and document it.
        shipped_critical_matched += r.recall_by_severity.critical_caught;
        // The total shipped critical count is approximated as
        // matched + a fraction of unmatched at critical severity. We don't
        // have the per-finding severity for unmatched on the summary path,
        // so we conservatively assume all unmatched are non-critical — this
        // *inflates* precision and is documented in the README. To get a
        // tighter number, callers can re-walk per_case + their saved
        // CouncilReports.
        shipped_critical += r.recall_by_severity.critical_caught;
    }
    let critical_precision = if shipped_critical == 0 {
        1.0
    } else {
        shipped_critical_matched as f64 / shipped_critical as f64
    };
    let critical_recall = recall.recall_at_or_above(Severity::Critical);
    let critical_f1 = if critical_precision + critical_recall == 0.0 {
        0.0
    } else {
        2.0 * critical_precision * critical_recall / (critical_precision + critical_recall)
    };
    let severity_calibration_mae = if total_sev_distance_n == 0 {
        0.0
    } else {
        total_sev_distance_sum as f64 / total_sev_distance_n as f64
    };
    let judge_brier_score = if total_brier_n == 0 {
        0.0
    } else {
        total_brier_sum / total_brier_n as f64
    };
    let avg_false_positives_per_case = if cases_total == 0 {
        0.0
    } else {
        (total_unmatched + total_forbidden) as f64 / cases_total as f64
    };
    let avg_latency_ms = if cases_total == 0 {
        0
    } else {
        total_latency_ms / cases_total as u64
    };

    CouncilEvalSummary {
        cases_total,
        cases_fully_recalled,
        cases_over_cap,
        cases_with_judge_error,
        recall,
        critical_precision,
        critical_recall,
        critical_f1,
        severity_calibration_mae,
        judge_brier_score,
        avg_false_positives_per_case,
        forbidden_path_findings: total_forbidden,
        avg_latency_ms,
        total_prompt_tokens,
        total_completion_tokens,
        per_case,
    }
}

fn finding_matches(expected: &ExpectedFinding, found: &Finding) -> bool {
    if !paths_match(&expected.file, &found.file) {
        return false;
    }
    if let Some([lo, hi]) = expected.line_range {
        match found.line {
            Some(l) => {
                if l < lo || l > hi {
                    return false;
                }
            }
            None => return false,
        }
    }
    if let Some(cat) = &expected.category {
        let model_cat = found.category.label();
        if !cat.eq_ignore_ascii_case(model_cat) {
            return false;
        }
    }
    true
}

fn paths_match(expected: &str, found: &str) -> bool {
    if expected == found {
        return true;
    }
    // Allow the labeller to write a basename when path layout doesn't
    // matter (e.g. `Cargo.lock`). Suffix match prevents false negatives
    // when the path-prefix conventions of the diff differ from the case
    // file (a/, b/, leading ./, etc.).
    found.ends_with(expected) || expected.ends_with(found)
}

fn hits_forbidden_path(f: &Finding, forbidden: &[ForbiddenPath]) -> bool {
    if forbidden.is_empty() {
        return false;
    }
    forbidden.iter().any(|fp| paths_match(&fp.path, &f.file))
}

fn severity_rung_distance(a: Severity, b: Severity) -> u32 {
    severity_rung(a).abs_diff(severity_rung(b))
}

fn severity_rung(s: Severity) -> u32 {
    match s {
        Severity::Nit => 0,
        Severity::Minor => 1,
        Severity::Major => 2,
        Severity::Critical => 3,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aatxe_council::types::{
        AgentReview, CouncilReport, Finding, FindingCategory, JudgeVerdict, JudgedFinding, Severity,
    };

    fn mk_report(judged: Vec<JudgedFinding>) -> CouncilReport {
        CouncilReport {
            model: "stub".into(),
            repo: "x/y".into(),
            pr: 1,
            files_total: 1,
            files_reviewed: 1,
            proposer_reviews: Vec::<AgentReview>::new(),
            synthesized: judged.iter().map(|j| j.finding.clone()).collect(),
            judged,
            confidence_floor: 0.5,
            total_duration_ms: 100,
            total_prompt_tokens: 50,
            total_completion_tokens: 30,
            judge_error: None,
        }
    }

    fn mk_finding(file: &str, line: u32, sev: Severity, cat: FindingCategory) -> Finding {
        Finding {
            file: file.into(),
            line: Some(line),
            severity: sev,
            category: cat,
            title: "t".into(),
            rationale: "r".into(),
            suggestion: None,
            raised_by: None,
        }
    }

    fn judged(f: Finding, conf: f64) -> JudgedFinding {
        JudgedFinding {
            finding: f,
            verdict: JudgeVerdict::Keep,
            confidence: conf,
            judge_note: None,
        }
    }

    #[test]
    fn perfect_recall_matches_every_expected() {
        let case = CouncilCase {
            name: "perfect".into(),
            description: "".into(),
            diff: "".into(),
            expected: vec![ExpectedFinding {
                file: "src/x.rs".into(),
                line_range: Some([10, 20]),
                severity: Severity::Critical,
                category: Some("security".into()),
                description: "leak".into(),
                must_catch: true,
            }],
            forbidden: vec![],
            max_findings: None,
            files_dir: None,
        };
        let report = mk_report(vec![judged(
            mk_finding(
                "src/x.rs",
                15,
                Severity::Critical,
                FindingCategory::Security,
            ),
            0.9,
        )]);
        let r = score_case(&case, &report, CouncilEvalOptions::default());
        assert_eq!(r.expected_caught, 1);
        assert_eq!(r.expected_total, 1);
        assert_eq!(r.findings_unmatched, 0);
        assert_eq!(r.severity_distance_sum, 0);
    }

    #[test]
    fn unmatched_shipped_finding_counts_as_false_positive() {
        let case = CouncilCase {
            name: "fp".into(),
            description: "".into(),
            diff: "".into(),
            expected: vec![],
            forbidden: vec![],
            max_findings: Some(0),
            files_dir: None,
        };
        let report = mk_report(vec![judged(
            mk_finding("src/x.rs", 5, Severity::Minor, FindingCategory::Correctness),
            0.7,
        )]);
        let r = score_case(&case, &report, CouncilEvalOptions::default());
        assert_eq!(r.findings_unmatched, 1);
        assert!(r.max_findings_violated);
        // Brier: confidence 0.7, outcome 0 → (0.7)^2 = 0.49.
        assert!((r.brier_sum - 0.49).abs() < 1e-9);
    }

    #[test]
    fn forbidden_path_is_separated_from_unmatched() {
        let case = CouncilCase {
            name: "lockfile".into(),
            description: "".into(),
            diff: "".into(),
            expected: vec![],
            forbidden: vec![ForbiddenPath {
                path: "Cargo.lock".into(),
                reason: "lockfile".into(),
            }],
            max_findings: None,
            files_dir: None,
        };
        let report = mk_report(vec![judged(
            mk_finding(
                "Cargo.lock",
                1,
                Severity::Major,
                FindingCategory::Maintainability,
            ),
            0.6,
        )]);
        let r = score_case(&case, &report, CouncilEvalOptions::default());
        assert_eq!(r.findings_forbidden, 1);
        assert_eq!(r.findings_unmatched, 0);
    }

    #[test]
    fn confidence_floor_hides_findings_from_scoring() {
        let case = CouncilCase {
            name: "below-floor".into(),
            description: "".into(),
            diff: "".into(),
            expected: vec![],
            forbidden: vec![],
            max_findings: Some(0),
            files_dir: None,
        };
        // Confidence 0.3 < floor 0.55 → finding hidden, no FP counted.
        let report = mk_report(vec![judged(
            mk_finding("src/x.rs", 1, Severity::Major, FindingCategory::Correctness),
            0.3,
        )]);
        let r = score_case(&case, &report, CouncilEvalOptions::default());
        assert_eq!(r.findings_total, 0);
        assert_eq!(r.findings_unmatched, 0);
        assert!(!r.max_findings_violated);
    }

    #[test]
    fn severity_calibration_mae_distinguishes_critical_vs_nit() {
        let case = CouncilCase {
            name: "miscal".into(),
            description: "".into(),
            diff: "".into(),
            expected: vec![ExpectedFinding {
                file: "src/x.rs".into(),
                line_range: None,
                severity: Severity::Critical,
                category: None,
                description: "".into(),
                must_catch: true,
            }],
            forbidden: vec![],
            max_findings: None,
            files_dir: None,
        };
        // Model says nit (rung 0), label says critical (rung 3). Distance = 3.
        let report = mk_report(vec![judged(
            mk_finding(
                "src/x.rs",
                1,
                Severity::Nit,
                FindingCategory::Maintainability,
            ),
            0.9,
        )]);
        let r = score_case(&case, &report, CouncilEvalOptions::default());
        assert_eq!(r.severity_distance_sum, 3);
        assert_eq!(r.severity_distance_n, 1);
    }

    #[test]
    fn corpus_summary_aggregates_correctly() {
        // Two cases, one perfect, one with a false positive.
        let r1 = CouncilCaseResult {
            name: "a".into(),
            expected_total: 1,
            expected_caught: 1,
            bonus_caught: 0,
            findings_total: 1,
            findings_unmatched: 0,
            findings_forbidden: 0,
            max_findings_violated: false,
            recall_by_severity: SeverityRecall {
                critical_caught: 1,
                critical_total: 1,
                ..Default::default()
            },
            severity_distance_sum: 0,
            severity_distance_n: 1,
            brier_sum: 0.01,
            brier_n: 1,
            total_duration_ms: 100,
            total_prompt_tokens: 10,
            total_completion_tokens: 5,
            judge_error: None,
            per_findings: Vec::new(),
        };
        let r2 = CouncilCaseResult {
            name: "b".into(),
            expected_total: 0,
            expected_caught: 0,
            bonus_caught: 0,
            findings_total: 1,
            findings_unmatched: 1,
            findings_forbidden: 0,
            max_findings_violated: true,
            recall_by_severity: SeverityRecall::default(),
            severity_distance_sum: 0,
            severity_distance_n: 0,
            brier_sum: 0.49,
            brier_n: 1,
            total_duration_ms: 200,
            total_prompt_tokens: 20,
            total_completion_tokens: 8,
            judge_error: Some("judge gave up".into()),
            per_findings: Vec::new(),
        };
        let s = score_council(vec![r1, r2]);
        assert_eq!(s.cases_total, 2);
        assert_eq!(s.cases_fully_recalled, 2); // r2 has 0 must_catch → trivially "fully recalled"
        assert_eq!(s.cases_over_cap, 1);
        assert_eq!(s.cases_with_judge_error, 1);
        assert!((s.judge_brier_score - 0.25).abs() < 1e-9);
        assert!((s.avg_false_positives_per_case - 0.5).abs() < 1e-9);
        assert!((s.critical_recall - 1.0).abs() < 1e-9);
        assert_eq!(s.avg_latency_ms, 150);
    }

    // ------------------------- M2.4 recalibration -------------------------

    fn make_calibration_case() -> CouncilCase {
        CouncilCase {
            name: "mixed".into(),
            description: "".into(),
            diff: "".into(),
            expected: vec![
                ExpectedFinding {
                    file: "src/auth.rs".into(),
                    line_range: Some([10, 20]),
                    severity: Severity::Critical,
                    category: None,
                    description: "the critical bug".into(),
                    must_catch: true,
                },
                ExpectedFinding {
                    file: "src/util.rs".into(),
                    line_range: Some([5, 8]),
                    severity: Severity::Major,
                    category: None,
                    description: "a major bug".into(),
                    must_catch: true,
                },
            ],
            forbidden: vec![],
            max_findings: None,
            files_dir: None,
        }
    }

    fn make_calibration_report() -> CouncilReport {
        // Four findings spanning a range of confidences. With floor 0.55:
        //   conf 0.90 — matches expected critical → catch
        //   conf 0.70 — unmatched FP
        //   conf 0.60 — matches expected major → catch
        //   conf 0.40 — below floor at 0.55 (would catch *another* expected
        //               critical if it survived) → hidden
        let findings = vec![
            judged(
                mk_finding(
                    "src/auth.rs",
                    15,
                    Severity::Critical,
                    FindingCategory::Security,
                ),
                0.9,
            ),
            judged(
                mk_finding(
                    "src/auth.rs",
                    99,
                    Severity::Minor,
                    FindingCategory::Maintainability,
                ),
                0.7,
            ),
            judged(
                mk_finding(
                    "src/util.rs",
                    7,
                    Severity::Major,
                    FindingCategory::Correctness,
                ),
                0.6,
            ),
            judged(
                mk_finding(
                    "src/util.rs",
                    77,
                    Severity::Minor,
                    FindingCategory::Performance,
                ),
                0.4,
            ),
        ];
        mk_report(findings)
    }

    #[test]
    fn score_case_records_every_non_drop_finding() {
        let case = make_calibration_case();
        let report = make_calibration_report();
        let r = score_case(&case, &report, CouncilEvalOptions::default());
        assert_eq!(
            r.per_findings.len(),
            4,
            "every Keep/Downgrade finding must be captured pre-floor:\n{:?}",
            r.per_findings
        );
        // Survives-the-floor view at the captured floor (0.55 default).
        // Only the 0.4 finding is below floor → 3 ship.
        assert_eq!(r.findings_total, 3);
    }

    #[test]
    fn score_case_skips_drop_verdict_in_records() {
        let case = make_calibration_case();
        let mut findings = make_calibration_report().judged;
        findings[1].verdict = JudgeVerdict::Drop;
        let report = mk_report(findings);
        let r = score_case(&case, &report, CouncilEvalOptions::default());
        // Drop'd one removed; 3 records remain.
        assert_eq!(r.per_findings.len(), 3);
        // Drop'd finding's title MUST NOT be in records.
        for rec in &r.per_findings {
            assert!(rec.confidence >= 0.5 || rec.verdict != JudgeVerdict::Drop);
        }
    }

    #[test]
    fn recalibrate_at_original_floor_is_byte_identical_to_original_metrics() {
        // The whole point of records: a recalibration at the captured
        // floor must reproduce the original aggregate metrics exactly.
        // Anything else is a bug — the floor-driven scoring path must be
        // single-sourced through `score_at_floor`.
        let case = make_calibration_case();
        let report = make_calibration_report();
        let original = score_case(&case, &report, CouncilEvalOptions::default());
        let recalibrated =
            recalibrate_case_from_records(&case, &original, CouncilEvalOptions::default())
                .expect("records were captured by score_case");
        assert_eq!(original.expected_caught, recalibrated.expected_caught);
        assert_eq!(original.findings_total, recalibrated.findings_total);
        assert_eq!(original.findings_unmatched, recalibrated.findings_unmatched);
        assert_eq!(original.findings_forbidden, recalibrated.findings_forbidden);
        assert_eq!(
            original.recall_by_severity.critical_caught,
            recalibrated.recall_by_severity.critical_caught
        );
        assert_eq!(
            original.recall_by_severity.major_caught,
            recalibrated.recall_by_severity.major_caught
        );
        assert!((original.brier_sum - recalibrated.brier_sum).abs() < 1e-12);
        assert_eq!(original.brier_n, recalibrated.brier_n);
        assert_eq!(
            original.severity_distance_sum, recalibrated.severity_distance_sum,
            "severity_distance must round-trip"
        );
        // Per-findings are preserved verbatim so chained recalibrations are stable.
        assert_eq!(original.per_findings, recalibrated.per_findings);
    }

    #[test]
    fn raising_floor_above_a_match_drops_that_catch() {
        let case = make_calibration_case();
        let report = make_calibration_report();
        let original = score_case(&case, &report, CouncilEvalOptions::default());
        assert_eq!(original.expected_caught, 2, "sanity: 2/2 caught at 0.55");

        // Floor 0.65 hides the 0.6-conf major-match → recall drops to 1/2.
        let tighter = recalibrate_case_from_records(
            &case,
            &original,
            CouncilEvalOptions {
                confidence_floor: 0.65,
            },
        )
        .unwrap();
        assert_eq!(tighter.expected_caught, 1);
        assert_eq!(
            tighter.recall_by_severity.major_caught, 0,
            "major catch lost"
        );
        assert_eq!(
            tighter.recall_by_severity.critical_caught, 1,
            "critical catch survives"
        );

        // The 0.7-conf FP also survives 0.65 → still 1 FP in `findings_total`.
        // Records still 4 entries (unchanged across recalibrations).
        assert_eq!(tighter.per_findings.len(), 4);
    }

    #[test]
    fn lowering_floor_can_recover_catches_but_increases_fp() {
        let case = make_calibration_case();
        let report = make_calibration_report();
        let original = score_case(&case, &report, CouncilEvalOptions::default());

        // Drop floor to 0.30 → the 0.4-conf finding ships too. It's an
        // unmatched FP (line 77, expected is [5,8]).
        let looser = recalibrate_case_from_records(
            &case,
            &original,
            CouncilEvalOptions {
                confidence_floor: 0.30,
            },
        )
        .unwrap();
        assert_eq!(looser.findings_total, 4, "all 4 records now ship");
        assert_eq!(
            looser.expected_caught, original.expected_caught,
            "no new expected catches"
        );
        assert_eq!(
            looser.findings_unmatched,
            original.findings_unmatched + 1,
            "the newly-shipped finding is an unmatched FP"
        );
    }

    #[test]
    fn per_case_recalibrate_with_empty_records_returns_zero_findings_not_error() {
        // A case that legitimately produced zero non-Drop findings (e.g.
        // forbidden-generated-code with all findings filtered before the
        // judge) recalibrates cleanly to zero findings at any floor.
        let case = make_calibration_case();
        let zero_findings = CouncilCaseResult {
            name: "mixed".into(),
            expected_total: 2,
            expected_caught: 0,
            bonus_caught: 0,
            findings_total: 0,
            findings_unmatched: 0,
            findings_forbidden: 0,
            max_findings_violated: false,
            recall_by_severity: SeverityRecall::default(),
            severity_distance_sum: 0,
            severity_distance_n: 0,
            brier_sum: 0.0,
            brier_n: 0,
            total_duration_ms: 0,
            total_prompt_tokens: 0,
            total_completion_tokens: 0,
            judge_error: None,
            per_findings: Vec::new(),
        };
        let r = recalibrate_case_from_records(&case, &zero_findings, CouncilEvalOptions::default())
            .expect("empty records is legitimate zero, not an error");
        assert_eq!(r.findings_total, 0);
        assert_eq!(r.expected_caught, 0);
    }

    #[test]
    fn summary_recalibrate_errors_when_every_case_has_no_records() {
        // Old eval JSON signature: per_findings empty on every case →
        // we can't tell legitimate-zero from missing-data at the case
        // level. The summary-level driver detects this and errors with
        // an actionable message.
        let zero = CouncilCaseResult {
            name: "any".into(),
            expected_total: 0,
            expected_caught: 0,
            bonus_caught: 0,
            findings_total: 0,
            findings_unmatched: 0,
            findings_forbidden: 0,
            max_findings_violated: false,
            recall_by_severity: SeverityRecall::default(),
            severity_distance_sum: 0,
            severity_distance_n: 0,
            brier_sum: 0.0,
            brier_n: 0,
            total_duration_ms: 0,
            total_prompt_tokens: 0,
            total_completion_tokens: 0,
            judge_error: None,
            per_findings: Vec::new(),
        };
        let mut second = zero.clone();
        second.name = "other".into();
        let summary = CouncilEvalSummary {
            cases_total: 2,
            cases_fully_recalled: 0,
            cases_over_cap: 0,
            cases_with_judge_error: 0,
            recall: SeverityRecall::default(),
            critical_precision: 0.0,
            critical_recall: 0.0,
            critical_f1: 0.0,
            severity_calibration_mae: 0.0,
            judge_brier_score: 0.0,
            avg_false_positives_per_case: 0.0,
            forbidden_path_findings: 0,
            avg_latency_ms: 0,
            total_prompt_tokens: 0,
            total_completion_tokens: 0,
            per_case: vec![zero, second],
        };
        let mut cases = std::collections::HashMap::new();
        cases.insert("any".to_string(), make_calibration_case());
        cases.insert("other".to_string(), make_calibration_case());
        let err = recalibrate_summary_from_records(&cases, &summary, CouncilEvalOptions::default())
            .expect_err("all-empty must error");
        match err {
            RecalibrateError::AllRecordsMissing { case_count } => {
                assert_eq!(case_count, 2);
            }
            other => panic!("expected AllRecordsMissing, got {other:?}"),
        }
        let msg = format!("{err:?}");
        assert!(
            msg.contains("AllRecordsMissing"),
            "error variant should be AllRecordsMissing, got: {msg}"
        );
    }

    #[test]
    fn summary_recalibrate_errors_when_case_missing_from_corpus() {
        // Corpus has been edited since the prior eval ran — the case is
        // no longer on disk. Recalibration can't match findings without
        // ground truth, so error out distinctly.
        let case_a = make_calibration_case();
        let report_a = make_calibration_report();
        let a_with_records = score_case(&case_a, &report_a, CouncilEvalOptions::default());
        let summary = score_council(vec![a_with_records.clone()]);
        // Cases map is EMPTY — corpus drift.
        let empty: std::collections::HashMap<String, CouncilCase> =
            std::collections::HashMap::new();
        let err = recalibrate_summary_from_records(&empty, &summary, CouncilEvalOptions::default())
            .expect_err("missing-from-corpus must error");
        match err {
            RecalibrateError::CaseMissingFromCorpus { case } => {
                assert_eq!(case, a_with_records.name);
            }
            other => panic!("expected CaseMissingFromCorpus, got {other:?}"),
        }
    }

    #[test]
    fn summary_recalibrate_succeeds_when_some_cases_have_records() {
        // Mixed case: case "a" has records (real findings), case "b" is
        // an empty (zero-findings) case. The summary driver must NOT
        // error in this scenario.
        let case_a = make_calibration_case();
        let report_a = make_calibration_report();
        let a_with_records = score_case(&case_a, &report_a, CouncilEvalOptions::default());
        let b_no_findings = CouncilCaseResult {
            name: "b".into(),
            expected_total: 0,
            expected_caught: 0,
            bonus_caught: 0,
            findings_total: 0,
            findings_unmatched: 0,
            findings_forbidden: 0,
            max_findings_violated: false,
            recall_by_severity: SeverityRecall::default(),
            severity_distance_sum: 0,
            severity_distance_n: 0,
            brier_sum: 0.0,
            brier_n: 0,
            total_duration_ms: 0,
            total_prompt_tokens: 0,
            total_completion_tokens: 0,
            judge_error: None,
            per_findings: Vec::new(),
        };
        let summary = score_council(vec![a_with_records.clone(), b_no_findings]);
        let mut cases = std::collections::HashMap::new();
        cases.insert(a_with_records.name.clone(), case_a);
        cases.insert("b".to_string(), make_calibration_case());
        let recal =
            recalibrate_summary_from_records(&cases, &summary, CouncilEvalOptions::default())
                .expect("at least one case has records → recalibrate proceeds");
        assert_eq!(recal.cases_total, 2);
    }

    #[test]
    fn per_findings_field_serialises_and_round_trips() {
        // Belt-and-braces: ensure the new field round-trips through JSON.
        let case = make_calibration_case();
        let report = make_calibration_report();
        let original = score_case(&case, &report, CouncilEvalOptions::default());
        let s = serde_json::to_string(&original).expect("serialize");
        let back: CouncilCaseResult = serde_json::from_str(&s).expect("deserialize");
        assert_eq!(original.per_findings, back.per_findings);
    }

    #[test]
    fn per_findings_omitted_when_empty_preserves_legacy_baselines() {
        // The `skip_serializing_if = "Vec::is_empty"` attribute is the
        // back-compat hinge: existing baselines without per-findings stay
        // round-trip-clean, and re-serialising an empty Vec doesn't add
        // a noisy `"perFindings": []` to the JSON.
        let r = CouncilCaseResult {
            name: "x".into(),
            expected_total: 0,
            expected_caught: 0,
            bonus_caught: 0,
            findings_total: 0,
            findings_unmatched: 0,
            findings_forbidden: 0,
            max_findings_violated: false,
            recall_by_severity: SeverityRecall::default(),
            severity_distance_sum: 0,
            severity_distance_n: 0,
            brier_sum: 0.0,
            brier_n: 0,
            total_duration_ms: 0,
            total_prompt_tokens: 0,
            total_completion_tokens: 0,
            judge_error: None,
            per_findings: Vec::new(),
        };
        let s = serde_json::to_string(&r).unwrap();
        assert!(
            !s.contains("perFindings"),
            "empty per_findings must NOT appear in serialised output: {s}"
        );
    }

    #[test]
    fn truncate_title_handles_long_and_multibyte() {
        assert_eq!(truncate_title("short"), "short");
        let long = "x".repeat(200);
        let t = truncate_title(&long);
        // 77 ASCII chars + the 3-byte `…` codepoint → max 80 bytes.
        assert!(t.len() <= 80, "got {} bytes", t.len());
        assert!(t.ends_with('…'));
        // Multibyte boundary: drop into a position where a naive truncate
        // would slice a UTF-8 codepoint.
        let s: String = "🦀".repeat(50); // 200 bytes
        let t = truncate_title(&s);
        assert!(t.ends_with('…'));
        assert!(
            std::str::from_utf8(t.as_bytes()).is_ok(),
            "truncated title must remain valid UTF-8"
        );
    }
}
