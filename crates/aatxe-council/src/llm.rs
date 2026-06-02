//! `LlmClient` — the seam between the pure pipeline and any chat-completion
//! backend. The CLI binary implements this against Kimi over `ureq`; the
//! test suite and the council benches use a deterministic stub so the
//! whole pipeline is reproducible without network calls.
//!
//! Shape matches the OpenAI chat-completions API on purpose: Kimi is
//! OpenAI-compatible, and the same trait drops in for OpenAI proper if
//! someone ever wants to swap providers.

use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("LLM transport error: {0}")]
    Transport(String),
    #[error("LLM returned HTTP {status}: {body}")]
    Status { status: u16, body: String },
    #[error("LLM response was not parseable: {0}")]
    Parse(String),
    #[error("LLM was asked but produced no choices")]
    Empty,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
}

impl ChatMessage {
    pub fn system(s: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: s.into(),
        }
    }
    pub fn user(s: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: s.into(),
        }
    }
}

/// What the pipeline asks the `LlmClient` to do. The implementation is
/// free to map this to whatever backend it likes — Kimi, OpenAI, a stub.
#[derive(Debug, Clone)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    /// `[0.0, 2.0]`. The pipeline picks 0.2 for proposers (slightly diverse
    /// per persona) and 0.0 for the judge (we want deterministic scoring).
    pub temperature: f32,
    /// `(0, 1]`. Cap on response tokens; backends should clamp to their
    /// model max.
    pub max_tokens: u32,
    /// When true, the backend should request JSON-formatted output via
    /// `response_format` (Kimi: `json_object`). Pure-text backends are
    /// free to ignore this hint.
    pub json_only: bool,
}

#[derive(Debug, Clone)]
pub struct ChatResponse {
    /// The first (typically only) completion content.
    pub content: String,
    /// Backend-reported finish reason, e.g. `"stop"`, `"length"`. Used by
    /// the parser to flag truncated JSON.
    pub finish_reason: String,
    /// Optional usage breakdown for cost accounting; backends fill it
    /// when they can.
    pub prompt_tokens: Option<u32>,
    pub completion_tokens: Option<u32>,
}

pub trait LlmClient: Send + Sync {
    fn chat(&self, req: ChatRequest) -> Result<ChatResponse, LlmError>;
}

/// A deterministic in-memory stub used by the test suite and the benches.
/// Configure by mapping (persona | judge) → canned response text.
#[derive(Debug, Default, Clone)]
pub struct StubClient {
    /// Map from "tag" (any case-insensitive substring matched against the
    /// first ~256 chars of the system prompt) → response content.
    pub responses: Vec<(String, String)>,
    /// Fallback when no tag matches. If `None`, returns
    /// `{"findings": []}` for proposer-shaped calls and
    /// `{"verdicts": []}` for judge-shaped calls.
    pub fallback: Option<String>,
}

impl StubClient {
    pub fn with(mut self, tag: &str, response: &str) -> Self {
        self.responses
            .push((tag.to_lowercase(), response.to_string()));
        self
    }
}

impl LlmClient for StubClient {
    fn chat(&self, req: ChatRequest) -> Result<ChatResponse, LlmError> {
        // Match against the *full* system prompt — persona-distinguishing
        // text often appears past the shared preamble, so a windowed scan
        // would silently route every persona to the fallback.
        let system_snippet: String = req
            .messages
            .iter()
            .find(|m| m.role == Role::System)
            .map(|m| m.content.to_lowercase())
            .unwrap_or_default();

        for (tag, resp) in &self.responses {
            if system_snippet.contains(tag) {
                return Ok(ChatResponse {
                    content: resp.clone(),
                    finish_reason: "stop".into(),
                    prompt_tokens: None,
                    completion_tokens: None,
                });
            }
        }

        let fallback = self.fallback.clone().unwrap_or_else(|| {
            if system_snippet.contains("judge") {
                "{\"verdicts\": []}".to_string()
            } else {
                "{\"findings\": []}".to_string()
            }
        });

        Ok(ChatResponse {
            content: fallback,
            finish_reason: "stop".into(),
            prompt_tokens: None,
            completion_tokens: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stub_routes_by_system_tag() {
        let c = StubClient::default()
            .with(
                "specialty: correctness",
                "{\"findings\": [{\"file\":\"x.rs\",\"severity\":\"major\",\"title\":\"bug\",\"rationale\":\"r\"}]}",
            )
            .with("specialty: security", "{\"findings\": []}");
        let r = c
            .chat(ChatRequest {
                model: "stub".into(),
                messages: vec![ChatMessage::system(
                    "you are the council. Your specialty: CORRECTNESS. blah blah",
                )],
                temperature: 0.0,
                max_tokens: 256,
                json_only: true,
            })
            .unwrap();
        assert!(r.content.contains("\"title\":\"bug\""));
    }

    #[test]
    fn stub_falls_back_distinctly_for_judge_and_proposer() {
        let c = StubClient::default();
        let prop = c
            .chat(ChatRequest {
                model: "stub".into(),
                messages: vec![ChatMessage::system(
                    "you are a proposer. specialty: correctness",
                )],
                temperature: 0.0,
                max_tokens: 100,
                json_only: true,
            })
            .unwrap();
        assert_eq!(prop.content, "{\"findings\": []}");
        let judge = c
            .chat(ChatRequest {
                model: "stub".into(),
                messages: vec![ChatMessage::system(
                    "You are the JUDGE on the aatxe council",
                )],
                temperature: 0.0,
                max_tokens: 100,
                json_only: true,
            })
            .unwrap();
        assert_eq!(judge.content, "{\"verdicts\": []}");
    }
}
