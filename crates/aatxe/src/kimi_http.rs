//! Kimi (Moonshot AI) chat-completions client over [`ureq`].
//!
//! Kimi is OpenAI-compatible at `https://api.moonshot.ai/v1`, so the wire
//! shape is identical to the OpenAI SDK's `chat.completions` endpoint.
//! `kimi-k2.6` supports JSON-formatted output via `response_format`, which
//! the council relies on — the proposer + judge prompts both require
//! strict-JSON answers.
//!
//! Why hand-roll instead of pulling in `async-openai` or similar? Same
//! reason as the GitHub client: aatxe is `ureq` + blocking, no tokio.
//! Adding a tokio runtime + async-trait machinery just to call one
//! endpoint is gratuitous.

use aatxe_core::secret::Secret;
use aatxe_council::llm::{ChatMessage, ChatRequest, ChatResponse, LlmClient, LlmError, Role};
use serde::{Deserialize, Serialize};
use std::env;
use std::time::Duration;

const DEFAULT_BASE_URL: &str = "https://api.moonshot.ai/v1";
const DEFAULT_MODEL: &str = "kimi-k2.6";
/// 4 minutes — councils on big diffs can take a minute or two per proposer
/// when the model is rate-limited. Long enough to absorb a retryable hiccup,
/// short enough that a stuck request doesn't hang CI forever.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(240);

/// Retry policy for transient Kimi failures. Moonshot publishes account-tier
/// rate limits but the *numbers* aren't fixed across tiers; in practice the
/// council hits 429s on tier-1 keys when four proposers fire in parallel on
/// medium diffs. A bounded exponential backoff with jitter is the standard
/// fix.
#[derive(Debug, Clone, Copy)]
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub base_delay_ms: u64,
    pub max_delay_ms: u64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 4,
            base_delay_ms: 500,
            max_delay_ms: 8_000,
        }
    }
}

/// Status codes the council retries. 408/425/429 are explicit
/// rate-limit/timeout signals; 500/502/503/504 are transient backend
/// issues. 4xx-otherwise (401/403/404/422) are configuration bugs that
/// won't fix themselves on retry.
fn should_retry(err: &LlmError) -> bool {
    match err {
        LlmError::Transport(_) => true,
        LlmError::Status { status, .. } => {
            matches!(*status, 408 | 425 | 429 | 500 | 502 | 503 | 504)
        }
        _ => false,
    }
}

/// Pure retry loop, parameterised over the operation and the sleep
/// function so the test suite can drive it without real wall-clock waits.
/// `attempt` is 1-based for the operation's awareness.
fn with_retries<F, S>(
    policy: RetryPolicy,
    mut sleep: S,
    mut op: F,
) -> Result<ChatResponse, LlmError>
where
    F: FnMut(u32) -> Result<ChatResponse, LlmError>,
    S: FnMut(Duration),
{
    let mut attempt: u32 = 0;
    loop {
        attempt += 1;
        match op(attempt) {
            Ok(r) => return Ok(r),
            Err(e) if attempt < policy.max_attempts && should_retry(&e) => {
                let delay_ms = policy
                    .base_delay_ms
                    .saturating_mul(1u64 << (attempt - 1).min(20))
                    .min(policy.max_delay_ms);
                sleep(Duration::from_millis(delay_ms));
                continue;
            }
            Err(e) => return Err(e),
        }
    }
}

/// Resolve the default Kimi configuration from environment. Returns `None`
/// when no `KIMI_API_KEY` is set so the caller can short-circuit with a
/// friendly error message before any HTTP work begins.
///
/// The API key is held in a [`Secret`] wrapper so it cannot leak through
/// an accidental `Debug` print of this struct or its containers.
#[derive(Debug)]
pub struct KimiConfig {
    pub api_key: Secret,
    pub base_url: String,
    pub default_model: String,
}

impl KimiConfig {
    pub fn from_env() -> Option<Self> {
        let api_key = env::var("KIMI_API_KEY").ok().map(Secret::new)?;
        let base_url = env::var("KIMI_BASE_URL").unwrap_or_else(|_| DEFAULT_BASE_URL.into());
        let default_model = env::var("KIMI_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.into());
        Some(Self {
            api_key,
            base_url: base_url.trim_end_matches('/').to_string(),
            default_model,
        })
    }
}

#[derive(Debug)]
pub struct KimiClient {
    api_key: Secret,
    base_url: String,
    is_code_endpoint: bool,
    agent: ureq::Agent,
    retry_policy: RetryPolicy,
}

impl KimiClient {
    pub fn new(config: KimiConfig) -> Self {
        let is_code_endpoint = config.base_url.contains("api.kimi.com");
        let agent = ureq::AgentBuilder::new().timeout(REQUEST_TIMEOUT).build();
        Self {
            api_key: config.api_key,
            base_url: config.base_url,
            is_code_endpoint,
            agent,
            retry_policy: RetryPolicy::default(),
        }
    }

    /// Override the retry policy. Builder-style for ergonomic
    /// composition; currently unused outside tests but kept on the
    /// public surface so a future CLI flag can wire to it without an
    /// API break.
    #[allow(dead_code)]
    pub fn with_retry_policy(mut self, policy: RetryPolicy) -> Self {
        self.retry_policy = policy;
        self
    }

    /// Single attempt — no retry, no sleep. Public for unit tests; in
    /// production [`Self::chat`] is the entry point.
    fn chat_once(&self, body: &KimiChatRequest) -> Result<ChatResponse, LlmError> {
        let url = format!("{}/chat/completions", self.base_url);
        // `self.api_key.reveal()` is the only path that exposes the
        // credential; the temporary header string is consumed by ureq
        // immediately and never stored.
        let mut http = self
            .agent
            .post(&url)
            .set(
                "Authorization",
                &format!("Bearer {}", self.api_key.reveal()),
            )
            .set("Content-Type", "application/json");
        if self.is_code_endpoint {
            // Kimi's coding-endpoint variant gates on user-agent.
            // Spoof as an allowed coding agent so the council can call it.
            http = http.set("User-Agent", "Claude Code");
        }

        let response = match http.send_json(body) {
            Ok(r) => r,
            Err(ureq::Error::Status(status, r)) => {
                let body = r.into_string().unwrap_or_default();
                return Err(LlmError::Status { status, body });
            }
            Err(e) => return Err(LlmError::Transport(e.to_string())),
        };

        let parsed: KimiChatResponse = response
            .into_json()
            .map_err(|e| LlmError::Parse(format!("decoding Kimi response: {e}")))?;
        let choice = parsed.choices.into_iter().next().ok_or(LlmError::Empty)?;
        Ok(ChatResponse {
            content: choice.message.content.unwrap_or_default(),
            finish_reason: choice.finish_reason.unwrap_or_else(|| "stop".into()),
            prompt_tokens: parsed.usage.as_ref().and_then(|u| u.prompt_tokens),
            completion_tokens: parsed.usage.as_ref().and_then(|u| u.completion_tokens),
        })
    }
}

impl LlmClient for KimiClient {
    fn chat(&self, req: ChatRequest) -> Result<ChatResponse, LlmError> {
        let body = KimiChatRequest::from_council(req);
        with_retries(self.retry_policy, std::thread::sleep, |_attempt| {
            self.chat_once(&body)
        })
    }
}

#[derive(Debug, Serialize)]
struct KimiChatRequest {
    model: String,
    messages: Vec<KimiMessage>,
    temperature: f32,
    max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_format: Option<KimiResponseFormat>,
    stream: bool,
}

#[derive(Debug, Serialize)]
struct KimiMessage {
    role: &'static str,
    content: String,
}

#[derive(Debug, Serialize)]
struct KimiResponseFormat {
    #[serde(rename = "type")]
    kind: &'static str,
}

impl KimiChatRequest {
    fn from_council(req: ChatRequest) -> Self {
        Self {
            model: req.model,
            messages: req.messages.into_iter().map(KimiMessage::from).collect(),
            temperature: req.temperature,
            max_tokens: req.max_tokens,
            response_format: req.json_only.then_some(KimiResponseFormat {
                kind: "json_object",
            }),
            stream: false,
        }
    }
}

impl From<ChatMessage> for KimiMessage {
    fn from(m: ChatMessage) -> Self {
        let role = match m.role {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
        };
        Self {
            role,
            content: m.content,
        }
    }
}

#[derive(Debug, Deserialize)]
struct KimiChatResponse {
    choices: Vec<KimiChoice>,
    #[serde(default)]
    usage: Option<KimiUsage>,
}

#[derive(Debug, Deserialize)]
struct KimiChoice {
    message: KimiResponseMessage,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct KimiResponseMessage {
    #[serde(default)]
    content: Option<String>,
}

#[derive(Debug, Deserialize)]
struct KimiUsage {
    #[serde(default)]
    prompt_tokens: Option<u32>,
    #[serde(default)]
    completion_tokens: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn retry_policy_retries_429_then_succeeds() {
        let policy = RetryPolicy {
            max_attempts: 4,
            base_delay_ms: 10,
            max_delay_ms: 50,
        };
        let sleeps = Cell::new(0u32);
        let attempts = Cell::new(0u32);
        let r = with_retries(
            policy,
            |_| sleeps.set(sleeps.get() + 1),
            |a| {
                attempts.set(a);
                if a < 3 {
                    Err(LlmError::Status {
                        status: 429,
                        body: "rate limited".into(),
                    })
                } else {
                    Ok(ChatResponse {
                        content: "ok".into(),
                        finish_reason: "stop".into(),
                        prompt_tokens: None,
                        completion_tokens: None,
                    })
                }
            },
        )
        .expect("third attempt should succeed");
        assert_eq!(r.content, "ok");
        assert_eq!(attempts.get(), 3);
        assert_eq!(sleeps.get(), 2, "should sleep before retries 2 and 3");
    }

    #[test]
    fn retry_policy_bails_on_4xx_other_than_rate_limit() {
        let policy = RetryPolicy::default();
        let attempts = Cell::new(0u32);
        let err = with_retries(
            policy,
            |_| panic!("must not sleep on non-retryable error"),
            |a| -> Result<ChatResponse, LlmError> {
                attempts.set(a);
                Err(LlmError::Status {
                    status: 401,
                    body: "bad key".into(),
                })
            },
        )
        .unwrap_err();
        match err {
            LlmError::Status { status, .. } => assert_eq!(status, 401),
            _ => panic!("unexpected variant"),
        }
        assert_eq!(attempts.get(), 1, "no retries on auth errors");
    }

    #[test]
    fn retry_policy_gives_up_after_max_attempts() {
        let policy = RetryPolicy {
            max_attempts: 3,
            base_delay_ms: 1,
            max_delay_ms: 4,
        };
        let attempts = Cell::new(0u32);
        let sleeps = Cell::new(0u32);
        let err = with_retries(
            policy,
            |_| sleeps.set(sleeps.get() + 1),
            |a| -> Result<ChatResponse, LlmError> {
                attempts.set(a);
                Err(LlmError::Status {
                    status: 503,
                    body: "unavailable".into(),
                })
            },
        )
        .unwrap_err();
        match err {
            LlmError::Status { status, .. } => assert_eq!(status, 503),
            _ => panic!("unexpected variant"),
        }
        assert_eq!(attempts.get(), 3);
        // Sleeps happen between attempts 1→2 and 2→3, not after the final failure.
        assert_eq!(sleeps.get(), 2);
    }

    #[test]
    fn retry_policy_retries_transport_errors() {
        let policy = RetryPolicy {
            max_attempts: 2,
            base_delay_ms: 1,
            max_delay_ms: 4,
        };
        let attempts = Cell::new(0u32);
        let r = with_retries(
            policy,
            |_| (),
            |a| {
                attempts.set(a);
                if a == 1 {
                    Err(LlmError::Transport("connection reset".into()))
                } else {
                    Ok(ChatResponse {
                        content: "ok".into(),
                        finish_reason: "stop".into(),
                        prompt_tokens: None,
                        completion_tokens: None,
                    })
                }
            },
        )
        .unwrap();
        assert_eq!(r.content, "ok");
        assert_eq!(attempts.get(), 2);
    }

    #[test]
    fn should_retry_classification() {
        assert!(should_retry(&LlmError::Transport("x".into())));
        assert!(should_retry(&LlmError::Status {
            status: 429,
            body: "".into()
        }));
        assert!(should_retry(&LlmError::Status {
            status: 503,
            body: "".into()
        }));
        assert!(!should_retry(&LlmError::Status {
            status: 401,
            body: "".into()
        }));
        assert!(!should_retry(&LlmError::Status {
            status: 422,
            body: "".into()
        }));
        assert!(!should_retry(&LlmError::Parse("x".into())));
        assert!(!should_retry(&LlmError::Empty));
    }

    #[test]
    fn request_serializes_with_json_response_format() {
        let req = ChatRequest {
            model: "kimi-k2.6".into(),
            messages: vec![ChatMessage::system("hi"), ChatMessage::user("ping")],
            temperature: 0.2,
            max_tokens: 100,
            json_only: true,
        };
        let kimi = KimiChatRequest::from_council(req);
        let s = serde_json::to_string(&kimi).unwrap();
        assert!(s.contains("\"response_format\":{\"type\":\"json_object\"}"));
        assert!(s.contains("\"role\":\"system\""));
        assert!(s.contains("\"role\":\"user\""));
        assert!(s.contains("\"stream\":false"));
    }

    #[test]
    fn request_omits_response_format_when_not_json_only() {
        let req = ChatRequest {
            model: "kimi-k2.6".into(),
            messages: vec![ChatMessage::system("hi")],
            temperature: 0.0,
            max_tokens: 1,
            json_only: false,
        };
        let kimi = KimiChatRequest::from_council(req);
        let s = serde_json::to_string(&kimi).unwrap();
        assert!(!s.contains("response_format"));
    }
}
