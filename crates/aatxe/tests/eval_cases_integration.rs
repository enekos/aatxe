//! Loads real eval-corpus cases from `evals/council/cases/` end-to-end
//! and verifies the council pipeline composes the expected prompt
//! shape — specifically that per-file context and related-file context
//! both reach a proposer call.
//!
//! These tests touch the filesystem (the corpus is committed to the
//! repo) and re-walk the same `filesDir` convention the CLI harness
//! uses. They run in CI alongside the unit suite — they're cheap (a
//! few hundred ms total) because the LLM client is a capturing stub.

use aatxe_council::llm::{ChatRequest, ChatResponse, LlmClient, LlmError};
use aatxe_council::pipeline::{run_council_with_files, CouncilOptions};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

fn repo_root() -> PathBuf {
    // CARGO_MANIFEST_DIR points at crates/aatxe; the repo root is two up.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

fn load_files_dir(dir: &Path) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(cur) = stack.pop() {
        for entry in std::fs::read_dir(&cur).unwrap() {
            let e = entry.unwrap();
            let p = e.path();
            let ft = e.file_type().unwrap();
            if ft.is_symlink() {
                continue;
            }
            if ft.is_dir() {
                stack.push(p);
                continue;
            }
            let rel = p.strip_prefix(dir).unwrap();
            let rel_str = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            out.insert(rel_str, std::fs::read_to_string(&p).unwrap());
        }
    }
    out
}

/// Captures every user-message body seen by `chat`, with the *first*
/// system prompt's persona substring as the key, so tests can assert
/// "the correctness proposer saw X" without depending on call order.
#[derive(Default)]
struct CapturingClient {
    seen: Mutex<Vec<String>>, // user-message bodies
}

impl LlmClient for CapturingClient {
    fn chat(&self, req: ChatRequest) -> Result<ChatResponse, LlmError> {
        for m in &req.messages {
            if m.role == aatxe_council::llm::Role::User {
                self.seen.lock().unwrap().push(m.content.clone());
            }
        }
        Ok(ChatResponse {
            content: "{\"findings\":[]}".to_string(),
            finish_reason: "stop".into(),
            prompt_tokens: Some(1),
            completion_tokens: Some(1),
        })
    }
}

fn run_case(case_slug: &str) -> Vec<String> {
    let root = repo_root();
    let diff_path = root
        .join("evals/council/cases")
        .join(format!("{case_slug}.diff"));
    let case_json_path = root
        .join("evals/council/cases")
        .join(format!("{case_slug}.json"));
    let case_raw = std::fs::read_to_string(&case_json_path).expect("case JSON");
    let case: serde_json::Value = serde_json::from_str(&case_raw).unwrap();
    let files_dir_rel = case["filesDir"].as_str().expect("filesDir present");
    let files_dir = root.join("evals/council/cases").join(files_dir_rel);
    let files = load_files_dir(&files_dir);
    let diff = std::fs::read_to_string(&diff_path).unwrap();

    let client = CapturingClient::default();
    run_council_with_files(
        &diff,
        &files,
        &CouncilOptions {
            model: "stub".into(),
            ..CouncilOptions::default()
        },
        &client,
    )
    .expect("council pipeline");

    client.seen.into_inner().unwrap()
}

#[test]
fn django_case_emits_related_context_for_db_and_billing_helpers() {
    let prompts = run_case("perf-django-export-n-plus-one");
    // Exactly 4 proposer calls (one per persona) + the judge — but the
    // judge's user message is the candidate list, not the diff. So at
    // least 4 proposer prompts must carry the related section.
    let with_related: Vec<&String> = prompts
        .iter()
        .filter(|p| {
            p.contains("Related repository context (not in diff):")
                && p.contains("def prefetch_in_batches")
                && p.contains("class BillingClient")
        })
        .collect();
    assert!(
        with_related.len() >= 4,
        "expected 4 proposer prompts carrying both helpers as related context, got {} \
         (total prompts captured: {})",
        with_related.len(),
        prompts.len()
    );

    // Per-file context (full exports.py) also lands in those prompts.
    for p in &with_related {
        assert!(
            p.contains("File contents (post-PR):") && p.contains("def export_active_users"),
            "proposer must also see the full diff'd file as per-file context"
        );
    }
}

#[test]
fn jwt_case_inlines_full_files_for_every_diff_path() {
    let prompts = run_case("security-jwt-fallback-secret");
    // The diff touches src/auth/jwt.ts and src/routes/auth.ts. All
    // three fixture files (those two plus the read-only env.ts) ship —
    // env.ts shows up under "Related repository context" because it's
    // not in the diff.
    let with_all: Vec<&String> = prompts
        .iter()
        .filter(|p| {
            p.contains("File contents (post-PR):")
                && p.contains("rotateRefreshToken")
                && p.contains("Related repository context (not in diff):")
                && p.contains("JWT_SECRET must be >=32 chars")
        })
        .collect();
    assert!(
        with_all.len() >= 4,
        "every proposer must see both diff'd files in full AND env.ts as related, got {}",
        with_all.len()
    );
}

#[test]
fn idor_case_forbids_findings_on_authz_helper() {
    let prompts = run_case("security-authz-idor-export-route");
    // authz.ts is the related-context file. The prompt builder must
    // include it AND must instruct the model not to file findings
    // against it (the forbidden-path scorer would catch FPs but the
    // guardrail prevents most of them upstream).
    for p in &prompts {
        if p.contains("Related repository context") {
            assert!(
                p.contains("=== src/middleware/authz.ts ==="),
                "authz.ts must appear as related context"
            );
            assert!(
                p.contains("do NOT raise findings against unchanged lines"),
                "prompt must guard against reviewing related files"
            );
        }
    }
}

#[test]
fn rust_counters_case_inlines_metrics_module_as_related() {
    let prompts = run_case("maintainability-rust-reinvents-counters");
    // metrics/mod.rs is the canonical-pattern file — its docstring is
    // the load-bearing piece for the recall path. Confirm it lands.
    let saw_pattern_doc: Vec<&String> = prompts
        .iter()
        .filter(|p| {
            p.contains("=== src/metrics/mod.rs ===")
                && p.contains("Constructing new ad-hoc")
                && p.contains("File contents (post-PR):")
                && p.contains("UploadStats")
        })
        .collect();
    assert!(
        saw_pattern_doc.len() >= 4,
        "every proposer prompt must inline the metrics-module rationale, got {}",
        saw_pattern_doc.len()
    );
}
