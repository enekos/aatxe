//! Deterministic stub LLM for offline / CI smoke-testing.
//!
//! Enabled by setting `AATXE_COUNCIL_STUB=1`. The council pipeline runs
//! against the same `LlmClient` trait, with this stub answering every
//! proposer + judge call from a fixed playbook. The point is to verify
//! the *plumbing* (workspace build, CLI wiring, sticky-marker rendering,
//! exit-code gate, GH comment upsert) inside CI / `act` without burning
//! real Moonshot quota — the **stub never proves the model integration
//! works**, only that everything around it does.

use aatxe_council::llm::{ChatRequest, ChatResponse, LlmClient, LlmError, Role};

const CANNED_FINDINGS_CORRECTNESS: &str = r#"{
  "findings": [
    {
      "file": "src/auth/login.rs",
      "line": 13,
      "severity": "minor",
      "category": "correctness",
      "title": "unwrap_or(false) silently hides verify errors",
      "rationale": "If bcrypt::verify itself fails (e.g. malformed hash), the user is treated as having entered a wrong password instead of returning an internal error.",
      "suggestion": "Distinguish the bcrypt error from a wrong-password result and propagate it."
    },
    {
      "file": "src/utils.rs",
      "line": 5,
      "severity": "nit",
      "category": "correctness",
      "title": "TODO comment ships unresolved",
      "rationale": "Comment flags an off-by-one for negative numbers but the fix is not in the diff.",
      "suggestion": "Either resolve the TODO or open a tracking issue and reference it."
    }
  ]
}"#;

const CANNED_FINDINGS_SECURITY: &str = r#"{
  "findings": [
    {
      "file": "src/auth/login.rs",
      "line": 15,
      "severity": "critical",
      "category": "security",
      "title": "password logged in plain text",
      "rationale": "The new log line `log::info!(\"user {} logged in with password {}\", user.email, req.password)` writes the user's plaintext password to whatever sink log::info is configured for.",
      "suggestion": "Remove req.password from the log entirely. Logging the email is fine; logging the credential is not."
    },
    {
      "file": "src/fetch/http.rs",
      "line": 11,
      "severity": "major",
      "category": "security",
      "title": "no URL/host allowlist before outbound fetch",
      "rationale": "fetch() now loops 10 times and the new allow_any_host() helper hard-codes `true`. Together they make this code path a useful SSRF primitive.",
      "suggestion": "Validate the parsed URL host against an allowlist before issuing the request. Delete allow_any_host() or wire it to a real check."
    }
  ]
}"#;

const CANNED_FINDINGS_PERFORMANCE: &str = r#"{
  "findings": [
    {
      "file": "src/fetch/http.rs",
      "line": 7,
      "severity": "major",
      "category": "performance",
      "title": "10-iteration loop with serial HTTP requests",
      "rationale": "Each iteration awaits the full request before the next starts. If the goal was retries, this multiplies tail latency 10×; if it was concurrency, futures::future::try_join_all is the right tool.",
      "suggestion": "Clarify intent: collapse to one request, run in parallel via join_all, or attach an explicit retry budget."
    }
  ]
}"#;

const CANNED_FINDINGS_MAINTAINABILITY: &str = r#"{
  "findings": [
    {
      "file": "docs/architecture.md",
      "line": 3,
      "severity": "nit",
      "category": "maintainability",
      "title": "new doc is a TODO placeholder",
      "rationale": "docs/architecture.md is created with body 'TODO: write this.' — ships dead content.",
      "suggestion": "Either delete the file from this PR or fill it in."
    }
  ]
}"#;

// Notes are intentionally verdict-generic, not finding-specific: the
// synthesiser's deterministic sort decides which candidate ends up at
// which index, so a note that talks about a specific finding would land
// on the wrong row if the sort order shifts. Generic notes read correctly
// regardless of permutation.
const CANNED_JUDGE_VERDICTS: &str = r#"{
  "verdicts": [
    {"index": 0, "verdict": "keep",      "confidence": 0.95, "note": "stub-judge: high-severity finding, high confidence"},
    {"index": 1, "verdict": "keep",      "confidence": 0.78, "note": "stub-judge: real signal, confident"},
    {"index": 2, "verdict": "keep",      "confidence": 0.72, "note": "stub-judge: worth surfacing to the author"},
    {"index": 3, "verdict": "keep",      "confidence": 0.62, "note": "stub-judge: softer signal, still actionable"},
    {"index": 4, "verdict": "downgrade", "confidence": 0.55, "note": "stub-judge: overstated severity; nit at most"},
    {"index": 5, "verdict": "drop",      "confidence": 0.92, "note": "stub-judge: speculative / WIP-placeholder; not actionable"}
  ]
}"#;

const FALLBACK_FINDINGS: &str = r#"{"findings": []}"#;
const FALLBACK_VERDICTS: &str = r#"{"verdicts": []}"#;

/// A deterministic LLM that answers from a fixed playbook tailored to the
/// bundled `examples/council-bench/fixtures/sample.diff` fixture.
///
/// The matching is done on the *system* prompt's specialty marker, so the
/// stub works regardless of the user-message contents.
pub struct StubKimi;

impl LlmClient for StubKimi {
    fn chat(&self, req: ChatRequest) -> Result<ChatResponse, LlmError> {
        let sys: String = req
            .messages
            .iter()
            .find(|m| m.role == Role::System)
            .map(|m| m.content.to_lowercase())
            .unwrap_or_default();

        let content = if sys.contains("specialty: correctness") {
            CANNED_FINDINGS_CORRECTNESS
        } else if sys.contains("specialty: security") {
            CANNED_FINDINGS_SECURITY
        } else if sys.contains("specialty: performance") {
            CANNED_FINDINGS_PERFORMANCE
        } else if sys.contains("specialty: maintainability") {
            CANNED_FINDINGS_MAINTAINABILITY
        } else if sys.contains("you are the judge on the aatxe agent council") {
            CANNED_JUDGE_VERDICTS
        } else if sys.contains("judge") {
            FALLBACK_VERDICTS
        } else {
            FALLBACK_FINDINGS
        };

        Ok(ChatResponse {
            content: content.to_string(),
            finish_reason: "stop".into(),
            prompt_tokens: None,
            completion_tokens: None,
        })
    }
}

/// True when the env var is set to anything truthy. Read once per
/// invocation by the CLI.
pub fn stub_enabled() -> bool {
    matches!(
        std::env::var("AATXE_COUNCIL_STUB").as_deref(),
        Ok("1") | Ok("true") | Ok("yes") | Ok("on")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use aatxe_council::llm::{ChatMessage, ChatRequest};

    fn req_with_sys(sys: &str) -> ChatRequest {
        ChatRequest {
            model: "stub".into(),
            messages: vec![ChatMessage::system(sys)],
            temperature: 0.0,
            max_tokens: 100,
            json_only: true,
        }
    }

    #[test]
    fn routes_each_specialty_to_its_canned_findings() {
        let s = StubKimi;
        let correctness = s
            .chat(req_with_sys(
                "preamble... Your specialty: CORRECTNESS. rest",
            ))
            .unwrap();
        assert!(correctness.content.contains("unwrap_or(false)"));

        let security = s
            .chat(req_with_sys("preamble... Your specialty: SECURITY. rest"))
            .unwrap();
        assert!(security.content.contains("plain text"));

        let perf = s
            .chat(req_with_sys(
                "preamble... Your specialty: PERFORMANCE. rest",
            ))
            .unwrap();
        assert!(perf.content.contains("10-iteration"));

        let maint = s
            .chat(req_with_sys(
                "preamble... Your specialty: MAINTAINABILITY. rest",
            ))
            .unwrap();
        assert!(maint.content.contains("TODO placeholder"));
    }

    #[test]
    fn routes_judge_to_canned_verdicts() {
        let s = StubKimi;
        let judge = s
            .chat(req_with_sys(
                "You are the JUDGE on the aatxe agent council. Four specialist reviewers ...",
            ))
            .unwrap();
        assert!(judge.content.contains("\"verdicts\""));
        assert!(judge.content.contains("\"confidence\": 0.95"));
    }

    #[test]
    fn unknown_system_prompt_returns_empty_findings() {
        let s = StubKimi;
        let r = s.chat(req_with_sys("you are something else")).unwrap();
        assert_eq!(r.content, FALLBACK_FINDINGS);
    }

    #[test]
    fn stub_enabled_recognises_truthy_values() {
        for v in ["1", "true", "yes", "on"] {
            std::env::set_var("AATXE_COUNCIL_STUB", v);
            assert!(stub_enabled(), "value `{}` should enable", v);
        }
        std::env::set_var("AATXE_COUNCIL_STUB", "no");
        assert!(!stub_enabled());
        std::env::remove_var("AATXE_COUNCIL_STUB");
        assert!(!stub_enabled());
    }
}
