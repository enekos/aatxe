//! Council orchestrator. Pure with respect to the model — the
//! [`crate::llm::LlmClient`] trait is the only seam to the world.
//!
//! The pipeline runs the four proposer personas, then the synthesiser
//! (deterministic), then the judge. Proposer calls are independent and
//! parallelised with `std::thread::scope` so total latency is roughly the
//! latency of the slowest single call rather than 4× the average. We use
//! the standard library's scoped threads (no tokio, no rayon) to keep the
//! dependency tree as light as the rest of aatxe.

use crate::diff::{
    attach_file_contexts, chunk_for_review_with_related_owned, filter_ignored, parse_unified_diff,
    ChunkPolicy, RelatedFile, DEFAULT_IGNORED_PATTERNS,
};
use crate::llm::LlmClient;
use crate::parse::{parse_findings_json, parse_judge_verdicts};
use crate::persona::Persona;
use crate::prompt::{build_judge_request, build_proposer_request};
use crate::synth::{dedup_and_rank, SynthOptions};
use crate::types::{AgentReview, CouncilReport, Finding, JudgeVerdict, JudgedFinding, Severity};
use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::time::Instant;

#[derive(Debug, Clone)]
pub struct CouncilOptions {
    /// Model identifier passed to the LLM client (e.g. `kimi-k2.6`).
    pub model: String,
    /// `owner/name` of the repo, surfaced in the report header.
    pub repo: String,
    /// PR number, surfaced in the report header.
    pub pr: u64,
    /// Below this confidence the judge's keep/downgrade verdicts are
    /// hidden. Default 0.55 — see `judge_system_prompt` for the rubric we
    /// instruct the model with.
    pub confidence_floor: f64,
    /// Diff chunking policy.
    pub chunk_policy: ChunkPolicy,
    /// Synthesizer dedup policy.
    pub synth: SynthOptions,
    /// Extra path patterns to ignore *in addition to*
    /// [`DEFAULT_IGNORED_PATTERNS`]. The CLI populates this from
    /// `--ignore` flags.
    pub extra_ignored: Vec<String>,
    /// Which personas to run. Defaults to all four.
    pub personas: Vec<Persona>,
    /// Optional guidance string prepended to every proposer + judge
    /// system prompt. Caller-supplied; the council doesn't construct it —
    /// in production it's the rendered learning-corpus block from
    /// [`aatxe-learn::build_guidance`], but the council stays decoupled
    /// from that crate by taking a plain string.
    pub learned_guidance: String,
    /// Optional AST-derived symbol scope, rendered into the proposer
    /// user message between related-file context and the diff. Caller
    /// builds it (in production from `aatxe-ast::render_scope_block`)
    /// and the council passes it through as a plain string so this crate
    /// stays decoupled from any specific parser.
    ///
    /// Empty string means "no AST scope available" — the prompt
    /// short-circuits the section and is byte-identical to the
    /// pre-scope baseline.
    pub ast_scope: String,
}

impl Default for CouncilOptions {
    fn default() -> Self {
        Self {
            model: "kimi-k2.6".to_string(),
            repo: String::new(),
            pr: 0,
            confidence_floor: 0.55,
            chunk_policy: ChunkPolicy::default(),
            synth: SynthOptions::default(),
            extra_ignored: Vec::new(),
            personas: Persona::ALL.to_vec(),
            learned_guidance: String::new(),
            ast_scope: String::new(),
        }
    }
}

/// End-to-end council pass over a unified-diff blob.
///
/// `client` is shared (via `&dyn LlmClient`) across all four proposer
/// threads + the judge call. Implementations must therefore be `Send +
/// Sync`; the trait already requires it.
///
/// Diff-only: proposers see hunks without surrounding-code context. For
/// the project-wide-context variant, see [`run_council_with_files`].
pub fn run_council(
    diff: &str,
    opts: &CouncilOptions,
    client: &dyn LlmClient,
) -> Result<CouncilReport> {
    run_council_with_files(diff, &HashMap::new(), opts, client)
}

/// End-to-end council pass *with* repo-snapshot context. `files_by_path`
/// maps repo-relative POSIX paths to the **post-PR** contents of each
/// file.
///
/// The pipeline classifies every path in the map:
/// * Paths matching a file in the diff → attached as
///   [`crate::ParsedFile::context`] so proposers see the surrounding
///   code of the hunks they're reviewing.
/// * Paths NOT in the diff → packed as chunk-level
///   [`crate::DiffChunk::related`] so proposers can cross-reference
///   helpers, conventions, or existing patterns the diff would-be
///   reviewer should know about. The same related set is shared across
///   every chunk produced from this diff.
///
/// This is the entry point eval cases that ship `files/` fixtures use,
/// and what a future `aatxe council --repo-path <dir>` invocation will
/// use to attach the working tree.
pub fn run_council_with_files(
    diff: &str,
    files_by_path: &HashMap<String, String>,
    opts: &CouncilOptions,
    client: &dyn LlmClient,
) -> Result<CouncilReport> {
    let t_total = Instant::now();

    // 1. Parse + attach context + filter.
    let all_files = parse_unified_diff(diff);
    let files_total = all_files.len() as u32;
    let diff_paths: HashSet<String> = all_files.iter().map(|f| f.path.clone()).collect();
    let all_files = attach_file_contexts(all_files, |path| files_by_path.get(path).cloned());
    let mut ignored = DEFAULT_IGNORED_PATTERNS.to_vec();
    let owned_extra: Vec<String> = opts.extra_ignored.clone();
    for p in &owned_extra {
        ignored.push(p.as_str());
    }
    let (kept, _dropped) = filter_ignored(all_files, &ignored);
    let files_reviewed = kept.len() as u32;
    // Anything in the supplied map that wasn't matched to a diff'd file
    // becomes "related" cross-reference context. Sort by path so chunk
    // packing is deterministic regardless of HashMap iteration order.
    let mut related: Vec<RelatedFile> = files_by_path
        .iter()
        .filter(|(path, _)| !diff_paths.contains(path.as_str()))
        .map(|(path, content)| RelatedFile {
            path: path.clone(),
            content: content.clone(),
        })
        .collect();
    related.sort_by(|a, b| a.path.cmp(&b.path));
    let chunks = chunk_for_review_with_related_owned(kept, &related, opts.chunk_policy);

    // 2. Proposer calls — parallel across personas, sequential across
    //    chunks. A per-persona LLM failure is captured into the agent
    //    review's `error` field and the council keeps going. The only
    //    thing that aborts the pipeline is internal logic going wrong,
    //    which never returns from `run_proposers_parallel`.
    let mut proposer_reviews: Vec<AgentReview> = Vec::new();
    let mut all_findings: Vec<Finding> = Vec::new();

    for chunk in chunks.iter() {
        let reviews = run_proposers_parallel(
            client,
            &opts.personas,
            &opts.model,
            chunk,
            &opts.learned_guidance,
            &opts.ast_scope,
        );
        for r in reviews {
            all_findings.extend(r.findings.iter().cloned());
            proposer_reviews.push(r);
        }
    }

    // 3. Synthesise (pure).
    let synthesized = dedup_and_rank(all_findings, opts.synth);

    // 4. Judge pass — single call over the full deduped candidate list.
    let (judged, judge_error) =
        run_judge(client, &opts.model, &synthesized, &opts.learned_guidance);

    // 5. Accounting.
    let total_prompt_tokens: u32 = proposer_reviews
        .iter()
        .filter_map(|r| r.prompt_tokens)
        .sum();
    let total_completion_tokens: u32 = proposer_reviews
        .iter()
        .filter_map(|r| r.completion_tokens)
        .sum();

    let total_duration_ms = t_total.elapsed().as_millis() as u64;
    Ok(CouncilReport {
        model: opts.model.clone(),
        repo: opts.repo.clone(),
        pr: opts.pr,
        files_total,
        files_reviewed,
        proposer_reviews,
        synthesized,
        judged,
        confidence_floor: opts.confidence_floor,
        total_duration_ms,
        total_prompt_tokens,
        total_completion_tokens,
        judge_error,
    })
}

/// Run every persona on this chunk in parallel via `std::thread::scope`.
/// **Never returns Err** — per-persona failures are captured into the
/// returned [`AgentReview`]'s `error` field so the council degrades
/// gracefully when one model call dies on a rate limit.
fn run_proposers_parallel(
    client: &dyn LlmClient,
    personas: &[Persona],
    model: &str,
    chunk: &crate::diff::DiffChunk,
    learned_guidance: &str,
    ast_scope: &str,
) -> Vec<AgentReview> {
    use std::sync::Mutex;
    let results: Mutex<Vec<(usize, AgentReview)>> = Mutex::new(Vec::with_capacity(personas.len()));

    std::thread::scope(|s| {
        for (i, &persona) in personas.iter().enumerate() {
            let results = &results;
            s.spawn(move || {
                let t0 = Instant::now();
                let req =
                    build_proposer_request(model, persona, chunk, learned_guidance, ast_scope);
                let review = match client.chat(req) {
                    Ok(resp) => {
                        let findings = parse_findings_json(&resp.content, persona);
                        AgentReview {
                            agent: persona.label().to_string(),
                            category: persona.category(),
                            findings,
                            duration_ms: Some(t0.elapsed().as_millis() as u64),
                            error: None,
                            prompt_tokens: resp.prompt_tokens,
                            completion_tokens: resp.completion_tokens,
                        }
                    }
                    Err(e) => AgentReview {
                        agent: persona.label().to_string(),
                        category: persona.category(),
                        findings: Vec::new(),
                        duration_ms: Some(t0.elapsed().as_millis() as u64),
                        error: Some(format!("{e}")),
                        prompt_tokens: None,
                        completion_tokens: None,
                    },
                };
                results.lock().expect("poisoned").push((i, review));
            });
        }
    });

    let mut list = results.into_inner().expect("poisoned");
    list.sort_by_key(|(i, _)| *i);
    list.into_iter().map(|(_, r)| r).collect()
}

/// Run the judge over the synthesized candidates. Returns the judged
/// list plus an optional error if the judge call itself failed. On
/// failure every candidate ships at the parser's fallback
/// (`Keep` / confidence 0.5) — better to over-include than to silently
/// drop a deduped finding because the judge timed out.
fn run_judge(
    client: &dyn LlmClient,
    model: &str,
    candidates: &[Finding],
    learned_guidance: &str,
) -> (Vec<JudgedFinding>, Option<String>) {
    if candidates.is_empty() {
        return (Vec::new(), None);
    }
    let req = build_judge_request(model, candidates, learned_guidance);
    let (verdicts, err) = match client.chat(req) {
        Ok(resp) => (parse_judge_verdicts(&resp.content, candidates.len()), None),
        Err(e) => {
            let fallback = vec![(JudgeVerdict::Keep, 0.5, None); candidates.len()];
            (fallback, Some(format!("{e}")))
        }
    };

    let mut out = Vec::with_capacity(candidates.len());
    for (i, cand) in candidates.iter().enumerate() {
        let (verdict, confidence, note) = verdicts[i].clone();
        let final_finding = if verdict == JudgeVerdict::Downgrade {
            let mut f = cand.clone();
            f.severity = downgrade(f.severity);
            f
        } else {
            cand.clone()
        };
        out.push(JudgedFinding {
            finding: final_finding,
            verdict,
            confidence,
            judge_note: note,
        });
    }
    (out, err)
}

fn downgrade(s: Severity) -> Severity {
    match s {
        Severity::Critical => Severity::Major,
        Severity::Major => Severity::Minor,
        Severity::Minor => Severity::Nit,
        Severity::Nit => Severity::Nit,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{ChatRequest, ChatResponse, LlmClient, LlmError, Role, StubClient};
    use std::sync::Mutex;

    const DIFF: &str = "diff --git a/src/x.rs b/src/x.rs
index 0..1 100644
--- a/src/x.rs
+++ b/src/x.rs
@@ -1 +1 @@
-let x = 1;
+let x = unsafe { *std::ptr::null::<i32>() };
diff --git a/Cargo.lock b/Cargo.lock
index 0..1 100644
--- a/Cargo.lock
+++ b/Cargo.lock
@@ -1 +1 @@
-old
+new
";

    #[test]
    fn end_to_end_runs_all_personas_and_judges_them() {
        let proposer_blob = r#"{"findings":[{"file":"src/x.rs","line":1,"severity":"critical","title":"dereferences null pointer","rationale":"undefined behaviour on read"}]}"#;
        let judge_blob =
            r#"{"verdicts":[{"index":0,"verdict":"keep","confidence":0.95,"note":"clear UB"}]}"#;

        let client = StubClient::default()
            .with("specialty: correctness", proposer_blob)
            .with("specialty: security", proposer_blob)
            .with("specialty: performance", "{\"findings\":[]}")
            .with("specialty: maintainability", "{\"findings\":[]}")
            .with("judge on the aatxe", judge_blob);

        let report = run_council(
            DIFF,
            &CouncilOptions {
                model: "stub".into(),
                repo: "x/y".into(),
                pr: 1,
                ..CouncilOptions::default()
            },
            &client,
        )
        .unwrap();

        assert_eq!(report.files_total, 2);
        assert_eq!(
            report.files_reviewed, 1,
            "Cargo.lock should be filtered out"
        );
        assert_eq!(report.proposer_reviews.len(), 4);
        // Correctness + Security both raised the same finding → dedupes to 1.
        assert_eq!(report.synthesized.len(), 1);
        let by = report.synthesized[0].raised_by.as_deref().unwrap();
        assert!(by.contains("correctness"));
        assert!(by.contains("security"));
        assert_eq!(report.judged.len(), 1);
        assert_eq!(report.judged[0].verdict, JudgeVerdict::Keep);
        assert_eq!(report.shippable().len(), 1);
        assert!(report.has_critical());
    }

    #[test]
    fn judge_drop_removes_shippable_finding() {
        let proposer_blob = r#"{"findings":[{"file":"src/x.rs","line":1,"severity":"major","title":"speculative thing","rationale":"r"}]}"#;
        let judge_blob =
            r#"{"verdicts":[{"index":0,"verdict":"drop","confidence":1.0,"note":"hallucinated"}]}"#;

        let client = StubClient::default()
            .with("specialty:", proposer_blob)
            .with("judge on the aatxe", judge_blob);

        let report = run_council(
            DIFF,
            &CouncilOptions {
                model: "stub".into(),
                repo: "x/y".into(),
                pr: 1,
                ..CouncilOptions::default()
            },
            &client,
        )
        .unwrap();

        assert!(!report.judged.is_empty());
        assert!(
            report.shippable().is_empty(),
            "dropped findings should not ship"
        );
        assert!(!report.has_critical());
    }

    #[test]
    fn judge_downgrade_lowers_severity_on_the_shipped_finding() {
        let proposer_blob = r#"{"findings":[{"file":"src/x.rs","line":1,"severity":"critical","title":"foo","rationale":"r"}]}"#;
        let judge_blob = r#"{"verdicts":[{"index":0,"verdict":"downgrade","confidence":0.9}]}"#;

        let client = StubClient::default()
            .with("specialty:", proposer_blob)
            .with("judge on the aatxe", judge_blob);

        let report = run_council(
            DIFF,
            &CouncilOptions {
                model: "stub".into(),
                repo: "x/y".into(),
                pr: 1,
                ..CouncilOptions::default()
            },
            &client,
        )
        .unwrap();

        let shipped = report.shippable();
        assert_eq!(shipped.len(), 1);
        assert_eq!(shipped[0].finding.severity, Severity::Major);
        assert!(!report.has_critical());
    }

    #[test]
    fn low_confidence_findings_are_hidden_from_shippable() {
        let proposer_blob = r#"{"findings":[{"file":"src/x.rs","line":1,"severity":"major","title":"foo","rationale":"r"}]}"#;
        let judge_blob = r#"{"verdicts":[{"index":0,"verdict":"keep","confidence":0.1}]}"#;

        let client = StubClient::default()
            .with("specialty:", proposer_blob)
            .with("judge on the aatxe", judge_blob);

        let report = run_council(
            DIFF,
            &CouncilOptions {
                model: "stub".into(),
                repo: "x/y".into(),
                pr: 1,
                confidence_floor: 0.55,
                ..CouncilOptions::default()
            },
            &client,
        )
        .unwrap();

        assert!(!report.judged.is_empty());
        assert!(report.shippable().is_empty());
    }

    #[test]
    fn empty_synth_short_circuits_judge_call() {
        // All proposers find nothing → no judge call needed.
        let client = StubClient::default()
            .with("specialty:", "{\"findings\":[]}")
            // Intentionally NO judge stub: if pipeline tried to call it,
            // it would fall back to the default {"verdicts": []} which is
            // safe but we don't want to require that path.
            ;
        let report = run_council(
            DIFF,
            &CouncilOptions {
                model: "stub".into(),
                ..CouncilOptions::default()
            },
            &client,
        )
        .unwrap();
        assert!(report.judged.is_empty());
        assert!(report.synthesized.is_empty());
    }

    // ---- Fail-soft tests ------------------------------------------------

    /// LLM client that fails specific personas (matched by system-prompt
    /// substring) and succeeds for everything else with canned content.
    struct FlakyClient {
        fail_substring: String,
        success_blob: String,
    }

    impl crate::llm::LlmClient for FlakyClient {
        fn chat(
            &self,
            req: crate::llm::ChatRequest,
        ) -> Result<crate::llm::ChatResponse, crate::llm::LlmError> {
            let sys = req
                .messages
                .iter()
                .find(|m| m.role == crate::llm::Role::System)
                .map(|m| m.content.to_lowercase())
                .unwrap_or_default();
            if sys.contains(&self.fail_substring.to_lowercase()) {
                return Err(crate::llm::LlmError::Status {
                    status: 429,
                    body: "rate limited".into(),
                });
            }
            Ok(crate::llm::ChatResponse {
                content: self.success_blob.clone(),
                finish_reason: "stop".into(),
                prompt_tokens: Some(100),
                completion_tokens: Some(40),
            })
        }
    }

    #[test]
    fn one_failed_proposer_does_not_abort_the_council() {
        let success_blob = r#"{"findings":[{"file":"src/x.rs","line":1,"severity":"major","title":"foo","rationale":"r"}]}"#;
        let client = FlakyClient {
            fail_substring: "specialty: security".into(),
            success_blob: success_blob.into(),
        };
        // No judge stub is necessary because FlakyClient returns the same
        // payload regardless of role; the judge will receive the success
        // blob and the parser will fall back to keep/0.5 because there's
        // no `verdicts` field.
        let report = run_council(
            DIFF,
            &CouncilOptions {
                model: "stub".into(),
                ..CouncilOptions::default()
            },
            &client,
        )
        .expect("pipeline must succeed despite one proposer failing");

        assert_eq!(report.proposer_reviews.len(), 4);
        let by_agent: std::collections::HashMap<&str, &AgentReview> = report
            .proposer_reviews
            .iter()
            .map(|r| (r.agent.as_str(), r))
            .collect();
        assert!(by_agent["security"].error.is_some());
        assert!(by_agent["security"]
            .error
            .as_deref()
            .unwrap()
            .contains("429"));
        assert!(by_agent["security"].findings.is_empty());
        assert!(by_agent["correctness"].error.is_none());
        assert_eq!(by_agent["correctness"].findings.len(), 1);
        // The 3 surviving proposers also produced findings → at least one
        // gets through dedup + judge.
        assert!(
            !report.synthesized.is_empty(),
            "surviving proposers' findings should still synthesise"
        );
    }

    #[test]
    fn judge_failure_falls_back_to_keep_at_half_confidence() {
        let proposer_blob = r#"{"findings":[{"file":"src/x.rs","line":1,"severity":"major","title":"foo","rationale":"r"}]}"#;
        // Client that answers proposers fine but fails on the judge call.
        struct OnlyJudgeFails(String);
        impl crate::llm::LlmClient for OnlyJudgeFails {
            fn chat(
                &self,
                req: crate::llm::ChatRequest,
            ) -> Result<crate::llm::ChatResponse, crate::llm::LlmError> {
                let sys = req
                    .messages
                    .iter()
                    .find(|m| m.role == crate::llm::Role::System)
                    .map(|m| m.content.to_lowercase())
                    .unwrap_or_default();
                if sys.contains("judge on the aatxe") {
                    return Err(crate::llm::LlmError::Transport(
                        "connection reset by peer".into(),
                    ));
                }
                Ok(crate::llm::ChatResponse {
                    content: self.0.clone(),
                    finish_reason: "stop".into(),
                    prompt_tokens: Some(50),
                    completion_tokens: Some(20),
                })
            }
        }
        let client = OnlyJudgeFails(proposer_blob.to_string());
        let report = run_council(
            DIFF,
            &CouncilOptions {
                model: "stub".into(),
                confidence_floor: 0.4,
                ..CouncilOptions::default()
            },
            &client,
        )
        .expect("pipeline must survive a failed judge call");

        assert!(
            report
                .judge_error
                .as_deref()
                .unwrap_or("")
                .contains("connection reset"),
            "judge_error should be surfaced"
        );
        assert!(
            !report.judged.is_empty(),
            "candidates ship at default keep/0.5"
        );
        for jf in &report.judged {
            assert_eq!(jf.verdict, JudgeVerdict::Keep);
            assert!((jf.confidence - 0.5).abs() < 1e-9);
        }
        // Default fallback confidence 0.5 ≥ floor 0.4 → finding ships.
        assert!(!report.shippable().is_empty());
    }

    #[test]
    fn run_council_with_files_attaches_context_visible_to_proposers() {
        use std::sync::Mutex;
        // Capture every system+user prompt seen by the (single-thread)
        // client so we can assert the file-contents block reached at
        // least one proposer call.
        struct CapturingClient {
            saw_context: Mutex<bool>,
        }
        impl crate::llm::LlmClient for CapturingClient {
            fn chat(
                &self,
                req: crate::llm::ChatRequest,
            ) -> Result<crate::llm::ChatResponse, crate::llm::LlmError> {
                for m in &req.messages {
                    if m.content.contains("File contents (post-PR):")
                        && m.content.contains("fn login() { /* full body */ }")
                    {
                        *self.saw_context.lock().unwrap() = true;
                    }
                }
                Ok(crate::llm::ChatResponse {
                    content: "{\"findings\":[]}".to_string(),
                    finish_reason: "stop".into(),
                    prompt_tokens: Some(10),
                    completion_tokens: Some(2),
                })
            }
        }
        let client = CapturingClient {
            saw_context: Mutex::new(false),
        };
        let mut files = std::collections::HashMap::new();
        files.insert(
            "src/x.rs".to_string(),
            "fn login() { /* full body */ }\n".to_string(),
        );
        let report = run_council_with_files(
            DIFF,
            &files,
            &CouncilOptions {
                model: "stub".into(),
                ..CouncilOptions::default()
            },
            &client,
        )
        .unwrap();
        assert_eq!(report.files_reviewed, 1, "Cargo.lock still filtered");
        assert!(
            *client.saw_context.lock().unwrap(),
            "at least one proposer must see the file contents block"
        );
    }

    #[test]
    fn run_council_with_files_splits_diff_matched_from_related() {
        use std::sync::Mutex;
        struct CapturingClient {
            saw_file_context: Mutex<bool>,
            saw_related: Mutex<bool>,
        }
        impl crate::llm::LlmClient for CapturingClient {
            fn chat(
                &self,
                req: crate::llm::ChatRequest,
            ) -> Result<crate::llm::ChatResponse, crate::llm::LlmError> {
                for m in &req.messages {
                    if m.content.contains("File contents (post-PR):")
                        && m.content.contains("fn login() { /* full file */ }")
                    {
                        *self.saw_file_context.lock().unwrap() = true;
                    }
                    if m.content.contains("Related repository context")
                        && m.content.contains("pub fn shared_helper()")
                    {
                        *self.saw_related.lock().unwrap() = true;
                    }
                }
                Ok(crate::llm::ChatResponse {
                    content: "{\"findings\":[]}".to_string(),
                    finish_reason: "stop".into(),
                    prompt_tokens: Some(10),
                    completion_tokens: Some(2),
                })
            }
        }
        let client = CapturingClient {
            saw_file_context: Mutex::new(false),
            saw_related: Mutex::new(false),
        };
        let mut files = std::collections::HashMap::new();
        // matched to a diff'd path → file context
        files.insert(
            "src/x.rs".to_string(),
            "fn login() { /* full file */ }\n".to_string(),
        );
        // NOT in the diff → related context
        files.insert(
            "src/util.rs".to_string(),
            "pub fn shared_helper() {}\n".to_string(),
        );
        run_council_with_files(
            DIFF,
            &files,
            &CouncilOptions {
                model: "stub".into(),
                ..CouncilOptions::default()
            },
            &client,
        )
        .unwrap();
        assert!(
            *client.saw_file_context.lock().unwrap(),
            "matched file must appear as per-file context"
        );
        assert!(
            *client.saw_related.lock().unwrap(),
            "unmatched file must appear as related context"
        );
    }

    #[test]
    fn token_totals_aggregate_across_proposers() {
        let canned = r#"{"findings":[]}"#;
        struct UsageReporter;
        impl crate::llm::LlmClient for UsageReporter {
            fn chat(
                &self,
                _req: crate::llm::ChatRequest,
            ) -> Result<crate::llm::ChatResponse, crate::llm::LlmError> {
                Ok(crate::llm::ChatResponse {
                    content: "{\"findings\":[]}".to_string(),
                    finish_reason: "stop".into(),
                    prompt_tokens: Some(123),
                    completion_tokens: Some(45),
                })
            }
        }
        let _ = canned; // shadow-only so the const is exercised in dev
        let report = run_council(
            DIFF,
            &CouncilOptions {
                model: "stub".into(),
                ..CouncilOptions::default()
            },
            &UsageReporter,
        )
        .unwrap();
        // 4 proposers × 123/45 → 492/180
        assert_eq!(report.total_prompt_tokens, 492);
        assert_eq!(report.total_completion_tokens, 180);
    }

    /// Captures the user-message of every chat call so a test can assert
    /// what reached a proposer. Returns a canned `{"findings":[]}` so the
    /// council pipeline runs to completion.
    #[derive(Default)]
    struct UserMessageCaptor {
        user_messages: Mutex<Vec<String>>,
    }
    impl LlmClient for UserMessageCaptor {
        fn chat(&self, req: ChatRequest) -> Result<ChatResponse, LlmError> {
            let user = req
                .messages
                .iter()
                .find(|m| m.role == Role::User)
                .map(|m| m.content.clone())
                .unwrap_or_default();
            self.user_messages.lock().unwrap().push(user);
            Ok(ChatResponse {
                content: "{\"findings\":[]}".to_string(),
                finish_reason: "stop".into(),
                prompt_tokens: None,
                completion_tokens: None,
            })
        }
    }

    #[test]
    fn council_options_ast_scope_reaches_every_proposer_user_message() {
        let captor = UserMessageCaptor::default();
        let scope = "src/x.rs:\n  - fn render (L1) `pub fn render()`\n";
        let _ = run_council(
            DIFF,
            &CouncilOptions {
                model: "stub".into(),
                repo: "x/y".into(),
                pr: 1,
                ast_scope: scope.to_string(),
                ..CouncilOptions::default()
            },
            &captor,
        )
        .unwrap();
        let msgs = captor.user_messages.lock().unwrap().clone();
        // 4 proposers (one chunk after Cargo.lock filter) + 0 judge calls
        // (no findings → judge short-circuits).
        assert_eq!(msgs.len(), 4, "one user message per proposer");
        for (i, m) in msgs.iter().enumerate() {
            assert!(
                m.contains("Symbol scope (AST-derived):"),
                "proposer {i} should see scope section"
            );
            assert!(
                m.contains("fn render (L1)"),
                "proposer {i} missing scope body"
            );
        }
    }

    #[test]
    fn empty_ast_scope_omits_section_on_every_proposer() {
        let captor = UserMessageCaptor::default();
        let _ = run_council(
            DIFF,
            &CouncilOptions {
                model: "stub".into(),
                ast_scope: String::new(),
                ..CouncilOptions::default()
            },
            &captor,
        )
        .unwrap();
        for m in captor.user_messages.lock().unwrap().iter() {
            assert!(
                !m.contains("Symbol scope (AST-derived):"),
                "no scope section when ast_scope empty"
            );
        }
    }
}
